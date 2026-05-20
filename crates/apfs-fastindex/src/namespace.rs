//! Namespace and per-directory aggregate emission from `FsRecordDump.records`.
//!
//! This is the v1 Rust MWP slice. It is gated by:
//!
//! - `EX-18` (Rust body-field dump field-level parity with Python),
//! - `EX-19` (SR-017 per-inode logical-size precedence),
//! - `EX-20` (SR-018 row-enumeration name preservation on CI + CS volumes).
//!
//! All three are validated against the proof-fixture shape. This module
//! consumes the same `FsRecordRow` set EX-18 emits and produces
//! `NamespaceEntry` + `DirectoryAggregate` rows under:
//!
//! - SR-017 logical-size precedence (compressed -> inode `uncompressed_size`
//!   when `INODE_HAS_UNCOMPRESSED_SIZE`, else decmpfs header
//!   `uncompressed_size`; symlinks -> `com.apple.fs.symlink` payload byte
//!   length excluding trailing NUL; ordinary/sparse/clone/hard-link -> the
//!   inode's `j_dstream_t.size`; otherwise zero),
//! - SR-018 row enumeration (stored UTF-8 bytes preserved verbatim; no
//!   normalization or case fold; lookup-by-name explicitly not claimed),
//! - SR-009 unique-inode per-directory aggregate policy (each directory's
//!   total counts every contributing inode exactly once, mirroring
//!   `src/apfs_fastindex/aggregate.py`),
//! - SR-019 + EX-22 + EX-26 allocated-size precedence:
//!     - regular + dstream + no `INO_EXT_TYPE_SPARSE_BYTES` xfield ->
//!       `Some(alloced_size)` (EX-22 baseline).
//!     - regular + dstream + sparse_bytes set ->
//!       `Some(alloced_size - sparse_bytes)` (EX-26 Hypothesis A,
//!       validated 4/4 on the EX-26 fixture; EX-22 had observed this
//!       relation on one fixture).
//!     - regular + `com.apple.decmpfs` xattr -> sum of stream-backed
//!       xattr `alloced_size` (primary_dstream + decmpfs xattr's
//!       `stream_dstream.alloced` + ResourceFork xattr's
//!       `stream_dstream.alloced`, each defaulting to 0; EX-26
//!       Hypothesis F, validated 2/2). Covers both shapes ditto
//!       produces: xattr-stream-stored compressed bytes
//!       (decmpfs.stream_dstream) and resource-fork-stored
//!       (ResourceFork.stream_dstream).
//!     - symlink -> `Some(0)`; directory -> `Some(0)`; else -> `None`.
//!   The directory aggregate's `unique_inode_allocated_total`
//!   collapses to `None` if any contributing inode has
//!   `allocated_size == None`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::extent_ref::{ExtentRefDump, PhysExtRecord};
use crate::fs_record_body::{FileExtentBody, FsRecordKey, FsRecordRow, FsRecordValue};
use crate::fs_records::FsRecordDump;
use crate::{DirectoryAggregate, EntryKind, NamespaceEntry};

/// APFS root-directory virtual OID per Apple's reference.
const APFS_ROOT_DIR_OID: u64 = 2;

/// `dirent.h` `DT_*` values reused by APFS via `j_drec_val_t.flags`.
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

const INODE_HAS_UNCOMPRESSED_SIZE: u64 = 0x0004_0000;

const XATTR_SYMLINK_NAME: &str = "com.apple.fs.symlink";
const XATTR_DECMPFS_NAME: &str = "com.apple.decmpfs";
const XATTR_RFORK_NAME: &str = "com.apple.ResourceFork";

/// Reconstruct `NamespaceEntry` rows and per-directory aggregates from one
/// volume's `FsRecordDump.records`.
///
/// `extent_ref_dump` is the parallel walk of the same volume's
/// extent-reference tree; when present, every regular-file row gets a
/// computed `real_size` (EX-27 clone-deduplicated allocated bytes).
/// When absent (extent-reference tree failed to validate), `real_size`
/// stays `None` on every row and the parser fails closed for the new
/// metric.
///
/// `block_size` is the container's block size; needed to convert
/// `phys_ext.length_blocks` to bytes when joining with
/// `file_extent.length_bytes`.
///
/// Returns `(entries, aggregates)` in stable sorted-by-path order.
pub(crate) fn build_namespace(
    dump: &FsRecordDump,
    extent_ref_dump: Option<&ExtentRefDump>,
    block_size: u32,
) -> (Vec<NamespaceEntry>, Vec<DirectoryAggregate>, Vec<crate::WalkSkip>) {
    let phys_exts = extent_ref_dump.map(|d| d.records.as_slice()).unwrap_or(&[]);
    let index = NamespaceIndex::from_records(&dump.records, phys_exts, block_size);
    let mut entries: Vec<NamespaceEntry> = Vec::new();
    // Round-2 audit #N4: surface depth-cap truncation as
    // `WalkSkip`s so the user sees subtrees the walker refused
    // to descend into instead of silently missing them. One
    // entry per subtree-root that hit the cap.
    let mut depth_truncations: Vec<crate::WalkSkip> = Vec::new();
    index.walk_into(&mut entries, &mut depth_truncations);
    // Paths inside a single volume's namespace are unique, so stable
    // sort is not required; sort_unstable_by is faster on the
    // post-walk ordering pass.
    entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let aggregates = build_aggregates(&entries);
    (entries, aggregates, depth_truncations)
}

struct NamespaceIndex<'a> {
    drec_children: BTreeMap<u64, Vec<DrecChild<'a>>>,
    inode_by_id: BTreeMap<u64, &'a crate::fs_record_body::InodeBody>,
    xattrs_by_id: BTreeMap<u64, BTreeMap<&'a str, &'a crate::fs_record_body::XattrBody>>,
    /// EX-27 clone-dedup state. `dstream_real_total[dstream_id]` is the
    /// dstream's clone-deduplicated allocated bytes (Σ over its
    /// file_extents of `length / phys_ext.refcnt`); empty when the
    /// extent-reference tree wasn't supplied (real_size stays None).
    dstream_real_total: HashMap<u64, u64>,
    /// `dstream_refcnt[d]` is the count of inodes referencing dstream
    /// `d` (from `j_dstream_id_val_t.refcnt` records). When a dstream
    /// is missing from this map, default to 1.
    dstream_refcnt: HashMap<u64, u32>,
    /// True iff the extent-reference dump was supplied. When false,
    /// `real_size` is `None` on every row (parser fails closed).
    extent_refs_available: bool,
}

struct DrecChild<'a> {
    name: &'a str,
    file_id: u64,
    entry_type: u8,
}

impl<'a> NamespaceIndex<'a> {
    fn from_records(
        records: &'a [FsRecordRow],
        phys_exts: &[PhysExtRecord],
        block_size: u32,
    ) -> Self {
        let mut drec_children: BTreeMap<u64, Vec<DrecChild<'a>>> = BTreeMap::new();
        let mut inode_by_id: BTreeMap<u64, &'a crate::fs_record_body::InodeBody> = BTreeMap::new();
        let mut xattrs_by_id: BTreeMap<
            u64,
            BTreeMap<&'a str, &'a crate::fs_record_body::XattrBody>,
        > = BTreeMap::new();
        let mut file_extents_by_dstream: BTreeMap<u64, Vec<(u64, &FileExtentBody)>> =
            BTreeMap::new();
        let mut dstream_refcnt: HashMap<u64, u32> = HashMap::new();

        for record in records {
            match (&record.key, &record.value) {
                (FsRecordKey::Named { name, .. }, FsRecordValue::DirRec(drec))
                    if record.family == "dir_rec" =>
                {
                    drec_children
                        .entry(record.object_id)
                        .or_default()
                        .push(DrecChild {
                            name: name.as_str(),
                            file_id: drec.file_id,
                            entry_type: drec.entry_type,
                        });
                }
                (_, FsRecordValue::Inode(inode)) => {
                    inode_by_id.insert(record.object_id, inode);
                }
                (FsRecordKey::Named { name, .. }, FsRecordValue::Xattr(xattr)) => {
                    xattrs_by_id
                        .entry(record.object_id)
                        .or_default()
                        .insert(name.as_str(), xattr);
                }
                (FsRecordKey::FileExtent { logical_addr }, FsRecordValue::FileExtent(fe)) => {
                    // EX-27: file_extent records are keyed by dstream_id
                    // (clones share a dstream), not inode obj_id.
                    file_extents_by_dstream
                        .entry(record.object_id)
                        .or_default()
                        .push((*logical_addr, fe));
                }
                (_, FsRecordValue::DstreamId { refcnt: Some(rc) }) => {
                    dstream_refcnt.insert(record.object_id, *rc);
                }
                _ => {}
            }
        }

        // Stable child ordering inside each directory: SR-018 keeps stored
        // UTF-8 bytes verbatim, but sorted-by-name keeps the namespace
        // output deterministic across runs without claiming lookup
        // semantics.
        for children in drec_children.values_mut() {
            // Stored DREC keys are unique on a well-formed APFS volume;
            // stability is not required for the per-directory child
            // ordering.
            children.sort_unstable_by(|a, b| a.name.cmp(b.name));
        }

        let extent_refs_available = !phys_exts.is_empty();
        let dstream_real_total = if extent_refs_available {
            compute_dstream_real_totals(&file_extents_by_dstream, phys_exts, block_size)
        } else {
            HashMap::new()
        };

        Self {
            drec_children,
            inode_by_id,
            xattrs_by_id,
            dstream_real_total,
            dstream_refcnt,
            extent_refs_available,
        }
    }

    /// EX-27 per-inode `real_size`.
    ///
    /// - Symlinks and directories: `Some(0)`.
    /// - Regular files with a known dstream: dedup share of the dstream,
    ///   with the integer-division residue allocated to the source inode
    ///   (the one whose `inode.private_id` equals its own `object_id`) so
    ///   per-directory totals sum to `Σ phys_ext bytes` exactly.
    /// - Anything else (or `extent_refs_available == false`): `None`.
    fn real_size(&self, file_id: u64, entry_type: u8) -> Option<u64> {
        if !self.extent_refs_available {
            return None;
        }
        match entry_type {
            DT_LNK | DT_DIR => Some(0),
            DT_REG => {
                let inode = self.inode_by_id.get(&file_id).copied()?;
                let dstream_id = inode.private_id;
                let dstream_total = *self.dstream_real_total.get(&dstream_id).unwrap_or(&0);
                let refcnt = self
                    .dstream_refcnt
                    .get(&dstream_id)
                    .copied()
                    .filter(|r| *r > 0)
                    .unwrap_or(1) as u64;
                let share = dstream_total / refcnt;
                let residue = dstream_total - share * refcnt;
                // The "source" inode (the one whose obj_id equals the
                // dstream_id) carries the residue so per-directory totals
                // close without integer-division loss. Every other inode
                // sharing the dstream gets the flat share.
                if file_id == dstream_id {
                    Some(share + residue)
                } else {
                    Some(share)
                }
            }
            _ => None,
        }
    }

    fn walk_into(&self, out: &mut Vec<NamespaceEntry>, truncated: &mut Vec<crate::WalkSkip>) {
        // Root `.` is not part of `NamespaceEntry` output (the Python
        // `oracle_diff` and `ProofRawWalkBackend` both omit it). The root
        // still owns the per-directory aggregate row keyed by `.`.
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        visited.insert(APFS_ROOT_DIR_OID);
        self.walk_dir(APFS_ROOT_DIR_OID, ".", out, &mut visited, 0, truncated);
    }

    fn walk_dir(
        &self,
        parent_id: u64,
        parent_path: &str,
        out: &mut Vec<NamespaceEntry>,
        visited: &mut BTreeSet<u64>,
        depth: usize,
        truncated: &mut Vec<crate::WalkSkip>,
    ) {
        // Stack-safety cap (audit #3). The `visited` set above
        // catches DREC cycles by file_id; the depth bound catches
        // pathologically deep but non-cyclic chains (a hostile
        // image could supply 100k nested directories with unique
        // file_ids). Refuse to recurse past `MAX_TREE_DEPTH` and
        // record the truncation so the user sees subtrees we
        // refused to descend into (audit #N4 — the previous
        // silent-truncation behaviour let a crafted image make
        // real content invisible).
        if depth >= crate::MAX_TREE_DEPTH {
            truncated.push(crate::WalkSkip {
                path: parent_path.to_string(),
                reason: format!("depth_cap_reached({})", crate::MAX_TREE_DEPTH),
            });
            return;
        }
        let Some(children) = self.drec_children.get(&parent_id) else {
            return;
        };
        for child in children {
            // Skip the kernel-injected `.fseventsd` directory the way every
            // existing oracle path does; it has no place in product output.
            if child.name == ".fseventsd" {
                continue;
            }
            let path = if parent_path == "." {
                child.name.to_string()
            } else {
                format!("{parent_path}/{}", child.name)
            };
            let entry_kind = entry_kind_from_drec(child.entry_type);
            let (logical_size, symlink_target) =
                self.logical_size_and_target(child.file_id, child.entry_type);
            let allocated_size = self.allocated_size(child.file_id, child.entry_type);
            let is_dir = matches!(entry_kind, EntryKind::Dir);
            if is_dir {
                if visited.insert(child.file_id) {
                    // Recurse before the entry move so we can hand the
                    // child path in by reference; the parent path Vec push
                    // still consumes its own owned String afterwards.
                    self.walk_dir(child.file_id, &path, out, visited, depth + 1, truncated);
                } else {
                    // DREC cycle detected — `visited` already
                    // contains this file_id, so an ancestor in
                    // the current walk already touched it. Pre-
                    // r3 fix this was silently dropped; now we
                    // emit a WalkSkip so the UI shows the
                    // truncation (audit r3 #F3).
                    truncated.push(crate::WalkSkip {
                        path: path.clone(),
                        reason: format!("drec_cycle(file_id={})", child.file_id),
                    });
                }
            }
            let real_size = self.real_size(child.file_id, child.entry_type);
            out.push(NamespaceEntry {
                path: path.into_boxed_str(),
                entry_kind,
                file_id: child.file_id,
                logical_size,
                symlink_target: symlink_target.map(String::into_boxed_str),
                allocated_size,
                real_size,
            });
        }
    }

    /// Apply the EX-26-amended SR-019 precedence per inode.
    fn allocated_size(&self, file_id: u64, entry_type: u8) -> Option<u64> {
        let inode = self.inode_by_id.get(&file_id).copied();
        let xattrs = self.xattrs_by_id.get(&file_id);
        compute_allocated_size(entry_type, inode, xattrs)
    }

    fn logical_size_and_target(&self, file_id: u64, entry_type: u8) -> (u64, Option<String>) {
        let inode = self.inode_by_id.get(&file_id).copied();
        let xattrs = self.xattrs_by_id.get(&file_id);

        if entry_type == DT_LNK {
            if let Some(symlink_xattr) = xattrs.and_then(|m| m.get(XATTR_SYMLINK_NAME)) {
                if symlink_xattr.embedded {
                    if let Some(text) = &symlink_xattr.payload_utf8 {
                        let trimmed = text.trim_end_matches('\u{0}');
                        return (trimmed.len() as u64, Some(trimmed.to_string()));
                    }
                }
            }
            return (0, None);
        }

        if entry_type != DT_REG {
            // Directories and other types have no logical-size meaning in v1.
            return (0, None);
        }

        let Some(inode) = inode else {
            return (0, None);
        };

        // SR-017 step 4: compressed regular files use inode
        // `uncompressed_size` if `INODE_HAS_UNCOMPRESSED_SIZE` is set,
        // else fall back to the `com.apple.decmpfs` header
        // `uncompressed_size`.
        let has_decmpfs = xattrs
            .map(|m| m.contains_key(XATTR_DECMPFS_NAME))
            .unwrap_or(false);
        if has_decmpfs {
            if inode.has_uncompressed_size
                || (inode.internal_flags & INODE_HAS_UNCOMPRESSED_SIZE) != 0
            {
                return (inode.uncompressed_size, None);
            }
            if let Some(decmpfs) = xattrs.and_then(|m| m.get(XATTR_DECMPFS_NAME)) {
                if let Some(size) = decmpfs_uncompressed_size(&decmpfs.payload_hex) {
                    return (size, None);
                }
            }
            // SR-017 step 4 fail-closed branch: compressed file without a
            // valid uncompressed-size source. Emit zero and let the caller
            // observe via not_claimed.
            return (0, None);
        }

        // Steps 1-3: ordinary, sparse, cloned, hard-linked files all use
        // dstream `size`. SR-017 explicitly notes that
        // `INO_EXT_TYPE_SPARSE_BYTES` is an allocation hint, not the
        // logical size.
        if let Some(dstream) = &inode.dstream {
            return (dstream.size, None);
        }
        (0, None)
    }
}

fn entry_kind_from_drec(entry_type: u8) -> EntryKind {
    match entry_type {
        DT_DIR => EntryKind::Dir,
        DT_REG => EntryKind::File,
        DT_LNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

/// EX-27 per-dstream dedup totals.
///
/// For each `file_extent` record (keyed by dstream_id), split its
/// physical byte range against the `phys_ext` records that overlap.
/// Sub-extents covered by a `phys_ext` record divide by that record's
/// `refcnt`; sub-extents not covered by any `phys_ext` (gaps in the
/// extent-reference tree, which APFS only populates for refcnt > 1
/// extents in some macOS versions) get implicit `refcnt = 1`.
///
/// Returns `dstream_id -> Σ over its file_extents of dedup_bytes`.
fn compute_dstream_real_totals(
    file_extents_by_dstream: &BTreeMap<u64, Vec<(u64, &FileExtentBody)>>,
    phys_exts: &[PhysExtRecord],
    block_size: u32,
) -> HashMap<u64, u64> {
    // Sort phys_ext records by paddr_first for binary-search overlap
    // detection. The tree we read from is already sorted by key, but the
    // EX-27 walker emits in walk order, so make no assumption.
    let mut sorted_phys: Vec<&PhysExtRecord> = phys_exts.iter().collect();
    sorted_phys.sort_by_key(|p| p.paddr_first);

    let block_size_u64 = block_size as u64;
    let mut totals: HashMap<u64, u64> = HashMap::new();
    for (dstream_id, extents) in file_extents_by_dstream {
        let mut dedup_total = 0u64;
        for (_logical_addr, fe) in extents {
            if fe.length_bytes == 0 || fe.phys_block_num == 0 {
                continue;
            }
            dedup_total =
                dedup_total.saturating_add(file_extent_dedup_bytes(fe, &sorted_phys, block_size_u64));
        }
        totals.insert(*dstream_id, dedup_total);
    }
    totals
}

/// Split one file_extent against the phys_ext records it overlaps and
/// sum `length / refcnt` per sub-extent.
fn file_extent_dedup_bytes(
    fe: &FileExtentBody,
    sorted_phys: &[&PhysExtRecord],
    block_size: u64,
) -> u64 {
    // Convert the file_extent's byte range to a block range. APFS file
    // extents are block-aligned on every macOS-produced volume; we round
    // up any sub-block residue to keep the math conservative.
    let fe_paddr_start = fe.phys_block_num;
    let length_blocks = if fe.length_bytes.is_multiple_of(block_size) {
        fe.length_bytes / block_size
    } else {
        fe.length_bytes / block_size + 1
    };
    let fe_paddr_end = fe_paddr_start.saturating_add(length_blocks); // exclusive

    // Binary-search the first phys_ext whose extent_end > fe_paddr_start.
    // sorted_phys is sorted by paddr_first ascending.
    let start_index = sorted_phys
        .partition_point(|p| p.paddr_first + p.length_blocks <= fe_paddr_start);

    let mut cursor = fe_paddr_start;
    let mut total = 0u64;
    for p in &sorted_phys[start_index..] {
        if p.paddr_first >= fe_paddr_end {
            break;
        }
        let p_start = p.paddr_first;
        let p_end = p_start + p.length_blocks;
        if p_start > cursor {
            // Gap between cursor and this phys_ext: implicit refcnt = 1.
            let gap_blocks = p_start - cursor;
            total = total.saturating_add(gap_blocks * block_size);
            cursor = p_start;
        }
        let overlap_start = cursor.max(p_start);
        let overlap_end = fe_paddr_end.min(p_end);
        if overlap_end > overlap_start {
            let overlap_blocks = overlap_end - overlap_start;
            let refcnt = if p.refcnt > 0 { p.refcnt as u64 } else { 1 };
            total = total.saturating_add((overlap_blocks * block_size) / refcnt);
            cursor = overlap_end;
        }
    }
    if cursor < fe_paddr_end {
        // Trailing range with no phys_ext coverage: implicit refcnt = 1.
        let tail_blocks = fe_paddr_end - cursor;
        total = total.saturating_add(tail_blocks * block_size);
    }
    total
}

/// EX-26-amended SR-019 allocated-size rule.
///
/// Free function so the tests can drive it without spinning up a real
/// `FsRecordDump`. Returns `None` only for cases the rule explicitly
/// does not emit (no inode for a regular file, or `entry_type` outside
/// `{DT_REG, DT_LNK, DT_DIR}`).
fn compute_allocated_size(
    entry_type: u8,
    inode: Option<&crate::fs_record_body::InodeBody>,
    xattrs: Option<&BTreeMap<&str, &crate::fs_record_body::XattrBody>>,
) -> Option<u64> {
    match entry_type {
        DT_LNK | DT_DIR => Some(0),
        DT_REG => {
            let inode = inode?;
            let has_decmpfs = xattrs
                .map(|m| m.contains_key(XATTR_DECMPFS_NAME))
                .unwrap_or(false);
            if has_decmpfs {
                // EX-26 Hypothesis F: decmpfs allocated bytes are the sum of
                // stream-backed xattr `alloced_size` plus the primary
                // dstream (which is typically absent for decmpfs files).
                // Both `com.apple.decmpfs` and `com.apple.ResourceFork`
                // can be the carrier; an embedded (non-stream) xattr
                // contributes 0 because the compressed bytes live inline
                // in the xattr payload.
                let primary = inode
                    .dstream
                    .as_ref()
                    .map(|d| d.alloced_size)
                    .unwrap_or(0);
                let xattr_share = |name: &str| -> u64 {
                    xattrs
                        .and_then(|m| m.get(name))
                        .and_then(|x| x.stream_dstream.as_ref())
                        .map(|d| d.alloced_size)
                        .unwrap_or(0)
                };
                let decmpfs_share = xattr_share(XATTR_DECMPFS_NAME);
                let rfork_share = xattr_share(XATTR_RFORK_NAME);
                return Some(primary + decmpfs_share + rfork_share);
            }
            if let Some(sparse_bytes) = inode.sparse_bytes {
                // EX-26 Hypothesis A: sparse allocated = `alloced_size -
                // sparse_bytes`. EX-22 saw this relation on one fixture;
                // EX-26 validated it across four sparse shapes (small
                // HEAD/TAIL hole, ~10 MiB, ~50 MiB, chunked-stride). The
                // dstream is always present when `sparse_bytes` is set;
                // we fail closed otherwise rather than guess.
                return inode
                    .dstream
                    .as_ref()
                    .map(|d| d.alloced_size.saturating_sub(sparse_bytes));
            }
            inode.dstream.as_ref().map(|d| d.alloced_size)
        }
        _ => None,
    }
}

/// `com.apple.decmpfs` header is `magic (4) + compression_type (4) +
/// uncompressed_size (8)` little-endian. The Rust crate does not interpret
/// the magic; if the payload is at least 16 bytes we read the field.
fn decmpfs_uncompressed_size(payload_hex: &str) -> Option<u64> {
    let bytes = hex_to_vec(payload_hex)?;
    if bytes.len() < 16 {
        return None;
    }
    Some(u64::from_le_bytes(
        bytes[8..16].try_into().expect("u64 uncompressed_size"),
    ))
}

fn hex_to_vec(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_nibble(bytes[index])?;
        let low = hex_nibble(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Per-directory unique-inode aggregate, mirroring
/// `src/apfs_fastindex/aggregate.py`. Each directory's total counts every
/// file inode in its subtree exactly once, regardless of how many
/// hard-link paths refer to the same inode (SR-009).
///
/// `unique_inode_allocated_total` collapses to `None` if any
/// contributing inode has `allocated_size == None` (SR-019 / EX-22
/// fail-closed cases). A partial total cannot be authoritative.
///
/// Implementation uses `HashMap` keyed by `&str` slices borrowed
/// from the entries' own `path` Strings; the ancestor walk allocates
/// nothing per file. Only the final sorted emission converts to
/// owned `String`s.
fn build_aggregates(entries: &[NamespaceEntry]) -> Vec<DirectoryAggregate> {
    // Per-directory contributor map: dir_path -> {file_id -> (logical,
    // allocated, real)}. Each inode contributes once per ancestor
    // directory regardless of hard-link count (SR-009).
    let mut contributors: HashMap<&str, HashMap<u64, (u64, Option<u64>, Option<u64>)>> =
        HashMap::new();
    contributors.insert(".", HashMap::new());
    for entry in entries {
        if matches!(entry.entry_kind, EntryKind::Dir) {
            contributors.entry(&*entry.path).or_default();
        }
    }
    for entry in entries {
        if !matches!(entry.entry_kind, EntryKind::File) {
            continue;
        }
        let mut current: &str = &*entry.path;
        loop {
            match current.rfind('/') {
                Some(idx) => {
                    let parent = &current[..idx];
                    let key = if parent.is_empty() { "." } else { parent };
                    if let Some(map) = contributors.get_mut(key) {
                        map.entry(entry.file_id).or_insert((
                            entry.logical_size,
                            entry.allocated_size,
                            entry.real_size,
                        ));
                    }
                    if parent.is_empty() {
                        break;
                    }
                    current = parent;
                }
                None => {
                    if let Some(map) = contributors.get_mut(".") {
                        map.entry(entry.file_id).or_insert((
                            entry.logical_size,
                            entry.allocated_size,
                            entry.real_size,
                        ));
                    }
                    break;
                }
            }
        }
    }

    let mut paths: Vec<&str> = contributors.keys().copied().collect();
    paths.sort_unstable();
    let mut out: Vec<DirectoryAggregate> = Vec::with_capacity(paths.len());
    for path in paths {
        let file_sizes = contributors
            .remove(path)
            .expect("path was just keys()d from the map");
        let unique_inode_logical_total: u64 =
            file_sizes.values().map(|(logical, _, _)| *logical).sum();
        let unique_inode_allocated_total: Option<u64> = file_sizes
            .values()
            .try_fold(0u64, |acc, (_, allocated, _)| allocated.map(|a| acc + a));
        let unique_inode_real_total: Option<u64> = file_sizes
            .values()
            .try_fold(0u64, |acc, (_, _, real)| real.map(|r| acc + r));
        let mut contributing_file_ids: Vec<u64> = file_sizes.keys().copied().collect();
        contributing_file_ids.sort_unstable();
        out.push(DirectoryAggregate {
            path: path.to_string(),
            unique_inode_logical_total,
            contributing_file_ids,
            unique_inode_allocated_total,
            unique_inode_real_total,
        });
    }
    out
}

/// (legacy ancestor_directories was removed in r2c-fallback-perf;
/// the new build_aggregates walks ancestors as &str slices inline.)
#[cfg(test)]
fn ancestor_directories(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = path;
    loop {
        if let Some(idx) = current.rfind('/') {
            let parent = &current[..idx];
            if parent.is_empty() {
                out.push(".".to_string());
                break;
            }
            out.push(parent.to_string());
            current = parent;
        } else {
            out.push(".".to_string());
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_directories_root_only() {
        assert_eq!(ancestor_directories("foo.txt"), vec!["."]);
    }

    #[test]
    fn ancestor_directories_nested() {
        assert_eq!(
            ancestor_directories("a/b/c.txt"),
            vec!["a/b".to_string(), "a".to_string(), ".".to_string()],
        );
    }

    #[test]
    fn decmpfs_size_reads_le_u64_at_offset_8() {
        // magic (4) + type (4) + size=0x55 little-endian = 8 bytes
        let payload = "00000000000000005500000000000000";
        assert_eq!(decmpfs_uncompressed_size(payload), Some(0x55));
    }

    #[test]
    fn decmpfs_size_rejects_short_payload() {
        let payload = "00000000";
        assert_eq!(decmpfs_uncompressed_size(payload), None);
    }

    /// Build a minimal `InodeBody` carrying just the fields the
    /// EX-26 rule reads. All other fields take sensible defaults; the
    /// helper exists so the tests below stay focused on the dstream
    /// and `sparse_bytes` axes.
    fn make_inode(
        dstream: Option<crate::fs_record_body::DstreamFields>,
        sparse_bytes: Option<u64>,
    ) -> crate::fs_record_body::InodeBody {
        crate::fs_record_body::InodeBody {
            parent_id: 0,
            private_id: 0,
            internal_flags: 0,
            nchildren_or_nlink: 1,
            bsd_flags: 0,
            owner: 0,
            group: 0,
            mode: 0o100_644,
            uncompressed_size: 0,
            has_uncompressed_size: false,
            xfields: vec![],
            xfield_used_data: 0,
            xfield_padded_total: 0,
            xfield_unused_trailing_bytes: 0,
            dstream,
            sparse_bytes,
            inode_name: None,
        }
    }

    fn make_dstream(alloced_size: u64) -> crate::fs_record_body::DstreamFields {
        crate::fs_record_body::DstreamFields {
            size: 0,
            alloced_size,
            default_crypto_id: 0,
            total_bytes_written: 0,
            total_bytes_read: 0,
        }
    }

    fn make_xattr_stream(alloced_size: u64) -> crate::fs_record_body::XattrBody {
        crate::fs_record_body::XattrBody {
            flags: 0x0001, // XATTR_DATA_STREAM
            xdata_len: 48,
            embedded: false,
            stream: true,
            payload_hex: String::new(),
            payload_utf8: None,
            stream_xattr_obj_id: Some(0),
            stream_dstream: Some(make_dstream(alloced_size)),
        }
    }

    fn make_xattr_embedded() -> crate::fs_record_body::XattrBody {
        crate::fs_record_body::XattrBody {
            flags: 0x0002, // XATTR_DATA_EMBEDDED
            xdata_len: 16,
            embedded: true,
            stream: false,
            // `fpmc` magic + compression_type=8 (lzvn fork-stored) +
            // uncompressed_size=0x400; the field values are not read
            // by the rule but realistic content keeps the fixture
            // close to what `ditto --hfsCompression` produces.
            payload_hex: "66706d63080000000004000000000000".to_string(),
            payload_utf8: None,
            stream_xattr_obj_id: None,
            stream_dstream: None,
        }
    }

    /// EX-26 Hypothesis A: sparse regular file allocated bytes are
    /// `alloced_size - sparse_bytes`. Was fail-closed under SR-019 v1.
    #[test]
    fn ex26_sparse_subtracts_sparse_bytes_from_alloced() {
        let inode = make_inode(Some(make_dstream(1_056_768)), Some(1_032_192));
        let picked = compute_allocated_size(DT_REG, Some(&inode), None);
        assert_eq!(picked, Some(24_576));
    }

    /// EX-26 sparse with `alloced_size < sparse_bytes` (pathological,
    /// not seen in practice): saturate at 0 rather than panic on
    /// underflow.
    #[test]
    fn ex26_sparse_underflow_saturates_at_zero() {
        let inode = make_inode(Some(make_dstream(4096)), Some(8192));
        let picked = compute_allocated_size(DT_REG, Some(&inode), None);
        assert_eq!(picked, Some(0));
    }

    /// EX-22 baseline preserved: regular + dstream + no sparse_bytes
    /// emits the raw `alloced_size`.
    #[test]
    fn ex26_regular_emits_dstream_alloced_size() {
        let inode = make_inode(Some(make_dstream(4096)), None);
        let picked = compute_allocated_size(DT_REG, Some(&inode), None);
        assert_eq!(picked, Some(4096));
    }

    /// EX-26 Hypothesis F (xattr-stream-stored compressed bytes):
    /// `com.apple.decmpfs` is stream-backed; allocated = its
    /// `stream_dstream.alloced_size`. EX-26 fixture `compressed.txt`.
    #[test]
    fn ex26_decmpfs_xattr_stream_stored() {
        let inode = make_inode(None, None);
        let decmpfs = make_xattr_stream(4096);
        let mut map: BTreeMap<&str, &crate::fs_record_body::XattrBody> = BTreeMap::new();
        map.insert(XATTR_DECMPFS_NAME, &decmpfs);
        let picked = compute_allocated_size(DT_REG, Some(&inode), Some(&map));
        assert_eq!(picked, Some(4096));
    }

    /// EX-26 Hypothesis F (fork-stored compressed bytes):
    /// `com.apple.decmpfs` is embedded (carries the `fpmc` header
    /// inline) and `com.apple.ResourceFork` is stream-backed; the
    /// resource fork's `stream_dstream.alloced_size` is the answer.
    /// EX-26 fixture `compressed-big.bin`.
    #[test]
    fn ex26_decmpfs_resource_fork_stored() {
        let inode = make_inode(None, None);
        let decmpfs = make_xattr_embedded();
        let rfork = make_xattr_stream(4096);
        let mut map: BTreeMap<&str, &crate::fs_record_body::XattrBody> = BTreeMap::new();
        map.insert(XATTR_DECMPFS_NAME, &decmpfs);
        map.insert(XATTR_RFORK_NAME, &rfork);
        let picked = compute_allocated_size(DT_REG, Some(&inode), Some(&map));
        assert_eq!(picked, Some(4096));
    }

    /// EX-26 Hypothesis F with both xattrs stream-backed (defensive:
    /// observed in `compressed.txt` where ditto kept the empty
    /// resource-fork stream alongside the decmpfs stream). The two
    /// allocated sizes add.
    #[test]
    fn ex26_decmpfs_both_streams_sum() {
        let inode = make_inode(None, None);
        let decmpfs = make_xattr_stream(4096);
        let rfork = make_xattr_stream(8192);
        let mut map: BTreeMap<&str, &crate::fs_record_body::XattrBody> = BTreeMap::new();
        map.insert(XATTR_DECMPFS_NAME, &decmpfs);
        map.insert(XATTR_RFORK_NAME, &rfork);
        let picked = compute_allocated_size(DT_REG, Some(&inode), Some(&map));
        assert_eq!(picked, Some(12_288));
    }

    /// EX-26 Hypothesis F with both xattrs embedded: compressed
    /// bytes live entirely inline; the file has no extents. Picks 0.
    #[test]
    fn ex26_decmpfs_all_embedded_picks_zero() {
        let inode = make_inode(None, None);
        let decmpfs = make_xattr_embedded();
        let mut map: BTreeMap<&str, &crate::fs_record_body::XattrBody> = BTreeMap::new();
        map.insert(XATTR_DECMPFS_NAME, &decmpfs);
        let picked = compute_allocated_size(DT_REG, Some(&inode), Some(&map));
        assert_eq!(picked, Some(0));
    }

    /// Symlinks and directories emit `Some(0)` unconditionally.
    #[test]
    fn ex26_symlink_and_dir_emit_zero() {
        let inode = make_inode(None, None);
        assert_eq!(
            compute_allocated_size(DT_LNK, Some(&inode), None),
            Some(0)
        );
        assert_eq!(
            compute_allocated_size(DT_DIR, Some(&inode), None),
            Some(0)
        );
    }

    /// A regular file with no inode record at all (parse anomaly):
    /// fail closed.
    #[test]
    fn ex26_regular_without_inode_returns_none() {
        let picked = compute_allocated_size(DT_REG, None, None);
        assert_eq!(picked, None);
    }

    // ---- EX-27 clone-dedup tests --------------------------------- //

    fn make_file_extent(
        length_bytes: u64,
        phys_block_num: u64,
    ) -> crate::fs_record_body::FileExtentBody {
        crate::fs_record_body::FileExtentBody {
            length_bytes,
            flags: 0,
            phys_block_num,
            crypto_id: 0,
        }
    }

    fn make_phys_ext(
        paddr_first: u64,
        length_blocks: u64,
        refcnt: i32,
    ) -> crate::extent_ref::PhysExtRecord {
        crate::extent_ref::PhysExtRecord {
            paddr_first,
            length_blocks,
            kind: 1, // APFS_KIND_NEW
            owning_obj_id: 0,
            refcnt,
        }
    }

    /// EX-27 family-A: 5 inodes share dstream 23, which has one
    /// 64 KiB file_extent. The phys_ext refcnt for that extent is 1
    /// (only one dstream references it; the sharing is at the
    /// dstream_id layer). Per inode share = 64 KiB / 5 = 13,107; the
    /// source inode (file_id == dstream_id == 23) carries the
    /// 1-byte rounding residue.
    #[test]
    fn ex27_family_a_5_clones_share_one_extent() {
        let mut map: BTreeMap<u64, Vec<(u64, &crate::fs_record_body::FileExtentBody)>> =
            BTreeMap::new();
        let fe = make_file_extent(64 * 1024, 848);
        map.insert(23, vec![(0, &fe)]);
        let phys_exts = vec![make_phys_ext(848, 16, 1)];
        let totals = compute_dstream_real_totals(&map, &phys_exts, 4096);
        assert_eq!(totals.get(&23).copied(), Some(65_536));

        // Replay the per-inode share math directly to confirm the
        // residue allocation.
        let dstream_total = 65_536u64;
        let refcnt = 5u64;
        let share = dstream_total / refcnt;
        let residue = dstream_total - share * refcnt;
        assert_eq!(share, 13_107);
        assert_eq!(residue, 1);
        // Source inode (file_id == dstream_id == 23): share + residue.
        assert_eq!(share + residue, 13_108);
    }

    /// EX-27 family-B: 3 inodes share dstream 29, one 1 MiB extent.
    /// 1 MiB / 3 = 349,525 per inode, residue 1 byte to source.
    #[test]
    fn ex27_family_b_3_clones_share_one_extent() {
        let mut map: BTreeMap<u64, Vec<(u64, &crate::fs_record_body::FileExtentBody)>> =
            BTreeMap::new();
        let fe = make_file_extent(1024 * 1024, 1000);
        map.insert(29, vec![(0, &fe)]);
        let phys_exts = vec![make_phys_ext(1000, 256, 1)];
        let totals = compute_dstream_real_totals(&map, &phys_exts, 4096);
        assert_eq!(totals.get(&29).copied(), Some(1_048_576));
    }

    /// EX-27 family-C: partial-share rewrite. src (dstream 33) has
    /// one 1 MiB file_extent at paddr 1176. The physical layout is
    /// split into three phys_ext records:
    ///   - paddr 1176, length 96 blocks (384 KiB), refcnt 2 — shared with clone
    ///   - paddr 1272, length 64 blocks (256 KiB), refcnt 1 — src-exclusive
    ///   - paddr 1336, length 96 blocks (384 KiB), refcnt 2 — shared with clone
    /// dedup_bytes = 384 KiB/2 + 256 KiB + 384 KiB/2 = 192+256+192 = 640 KiB.
    #[test]
    fn ex27_family_c_partial_share_after_rewrite() {
        let mut map: BTreeMap<u64, Vec<(u64, &crate::fs_record_body::FileExtentBody)>> =
            BTreeMap::new();
        let fe = make_file_extent(1024 * 1024, 1176);
        map.insert(33, vec![(0, &fe)]);
        let phys_exts = vec![
            make_phys_ext(1176, 96, 2),
            make_phys_ext(1272, 64, 1),
            make_phys_ext(1336, 96, 2),
        ];
        let totals = compute_dstream_real_totals(&map, &phys_exts, 4096);
        // 96*4096/2 + 64*4096 + 96*4096/2 = 196608 + 262144 + 196608 = 655_360
        assert_eq!(totals.get(&33).copied(), Some(655_360));
    }

    /// EX-27 family-C clone side: dstream 35 has three file_extents
    /// (one covering the rewritten middle exclusively, two pointing
    /// at the shared paddrs). The dedup math should match the src
    /// side at 640 KiB.
    #[test]
    fn ex27_family_c_clone_with_three_file_extents() {
        let mut map: BTreeMap<u64, Vec<(u64, &crate::fs_record_body::FileExtentBody)>> =
            BTreeMap::new();
        // Three file_extents in the clone:
        //   - logical [0, 384 KiB), paddr 1176, length 384 KiB (shares src's first chunk).
        //   - logical [384 KiB, 640 KiB), paddr ?, length 256 KiB (new, refcnt 1).
        //   - logical [640 KiB, 1024 KiB), paddr 1336, length 384 KiB (shares src's last chunk).
        let fe_shared_head = make_file_extent(384 * 1024, 1176);
        let fe_new_middle = make_file_extent(256 * 1024, 2000);
        let fe_shared_tail = make_file_extent(384 * 1024, 1336);
        map.insert(
            35,
            vec![
                (0, &fe_shared_head),
                (384 * 1024, &fe_new_middle),
                (640 * 1024, &fe_shared_tail),
            ],
        );
        let phys_exts = vec![
            make_phys_ext(1176, 96, 2),
            make_phys_ext(1272, 64, 1),    // src-exclusive (not referenced by clone)
            make_phys_ext(1336, 96, 2),
            make_phys_ext(2000, 64, 1),    // clone's new middle (refcnt 1)
        ];
        let totals = compute_dstream_real_totals(&map, &phys_exts, 4096);
        // 384 KiB/2 + 256 KiB + 384 KiB/2 = 655_360
        assert_eq!(totals.get(&35).copied(), Some(655_360));
    }

    /// Extents not covered by any phys_ext record default to
    /// refcnt = 1 (the on-disk convention: APFS may omit phys_ext
    /// records for refcnt=1 extents on some macOS versions).
    #[test]
    fn ex27_uncovered_extent_implicit_refcnt_one() {
        let mut map: BTreeMap<u64, Vec<(u64, &crate::fs_record_body::FileExtentBody)>> =
            BTreeMap::new();
        let fe = make_file_extent(8192, 500);
        map.insert(7, vec![(0, &fe)]);
        // Empty phys_exts means "extent-ref tree available but
        // doesn't track this paddr" — interpreted as refcnt=1.
        let phys_exts: Vec<crate::extent_ref::PhysExtRecord> = vec![make_phys_ext(9999, 1, 1)];
        let totals = compute_dstream_real_totals(&map, &phys_exts, 4096);
        assert_eq!(totals.get(&7).copied(), Some(8192));
    }

    /// Σ over per-inode real_size across all clones in a sharing
    /// group equals the dstream's dedup total — the invariant that
    /// makes the metric sum cleanly in a treemap. The source inode
    /// (file_id == private_id) carries the residue.
    #[test]
    fn ex27_per_inode_shares_close_with_residue() {
        // dstream id 23, refcnt 5, dedup total 65,536. Inodes 23
        // (source) + 24, 25, 26, 27 (clones).
        let dstream_total = 65_536u64;
        let refcnt = 5u64;
        let share = dstream_total / refcnt;
        let residue = dstream_total - share * refcnt;
        // Source share:
        let source_share = share + residue;
        // Clone shares:
        let clone_share = share;
        let total = source_share + clone_share * 4;
        assert_eq!(total, dstream_total);
    }

    /// SR-019 / EX-22 aggregate None-collapse: a directory whose
    /// inodes include even one `None` allocated_size must report
    /// `unique_inode_allocated_total = None`, never a partial sum.
    #[test]
    fn aggregate_collapses_when_any_inode_has_none_allocated() {
        let entries = vec![
            NamespaceEntry {
                path: "ordinary.txt".into(),
                entry_kind: EntryKind::File,
                file_id: 10,
                logical_size: 100,
                symlink_target: None,
                allocated_size: Some(4096),
                real_size: Some(4096),
            },
            NamespaceEntry {
                path: "sparse.bin".into(),
                entry_kind: EntryKind::File,
                file_id: 11,
                logical_size: 200,
                symlink_target: None,
                allocated_size: None,
                real_size: None,
            },
        ];
        let aggregates = build_aggregates(&entries);
        let root = aggregates
            .iter()
            .find(|a| a.path == ".")
            .expect("root aggregate exists");
        assert_eq!(root.unique_inode_logical_total, 300);
        assert_eq!(root.unique_inode_allocated_total, None);
        assert_eq!(root.unique_inode_real_total, None);
        assert_eq!(root.contributing_file_ids, vec![10, 11]);
    }

    /// All inodes Some -> aggregate sums them.
    #[test]
    fn aggregate_sums_allocated_when_all_inodes_present() {
        let entries = vec![
            NamespaceEntry {
                path: "a.txt".into(),
                entry_kind: EntryKind::File,
                file_id: 20,
                logical_size: 100,
                symlink_target: None,
                allocated_size: Some(4096),
                real_size: Some(4096),
            },
            NamespaceEntry {
                path: "b.txt".into(),
                entry_kind: EntryKind::File,
                file_id: 21,
                logical_size: 200,
                symlink_target: None,
                allocated_size: Some(8192),
                real_size: Some(8192),
            },
        ];
        let aggregates = build_aggregates(&entries);
        let root = aggregates
            .iter()
            .find(|a| a.path == ".")
            .expect("root aggregate exists");
        assert_eq!(root.unique_inode_allocated_total, Some(12288));
        assert_eq!(root.unique_inode_real_total, Some(12288));
    }

    /// A hard-link sibling that shares an inode contributes its
    /// allocated bytes exactly once, even if both rows carry the
    /// same `file_id` and `allocated_size`.
    #[test]
    fn aggregate_counts_hard_linked_inode_once_for_allocation() {
        let entries = vec![
            NamespaceEntry {
                path: "ordinary.txt".into(),
                entry_kind: EntryKind::File,
                file_id: 30,
                logical_size: 29,
                symlink_target: None,
                allocated_size: Some(4096),
                real_size: Some(4096),
            },
            NamespaceEntry {
                path: "hard.txt".into(),
                entry_kind: EntryKind::File,
                file_id: 30,
                logical_size: 29,
                symlink_target: None,
                allocated_size: Some(4096),
                real_size: Some(4096),
            },
        ];
        let aggregates = build_aggregates(&entries);
        let root = aggregates
            .iter()
            .find(|a| a.path == ".")
            .expect("root aggregate exists");
        assert_eq!(root.unique_inode_logical_total, 29);
        assert_eq!(root.unique_inode_allocated_total, Some(4096));
        assert_eq!(root.contributing_file_ids, vec![30]);
    }

    /// Build a stand-alone `NamespaceIndex` over a hand-rolled
    /// drec-children map. Lets the depth / cycle tests below
    /// drive `walk_into` without spinning a real
    /// `FsRecordDump`.
    fn make_index_with_chain(names: &[&'static str]) -> NamespaceIndex<'static> {
        // Build a single-child chain: root → n0 → n1 → n2 → ...
        // Each `child.file_id` = index + APFS_ROOT_DIR_OID + 1,
        // ensuring no collisions with the root oid.
        let mut drec: BTreeMap<u64, Vec<DrecChild<'static>>> = BTreeMap::new();
        for (i, name) in names.iter().enumerate() {
            // Parent of level i is the previous level's
            // file_id. Level 0 has the synthetic root as
            // parent.
            let parent_id = if i == 0 {
                APFS_ROOT_DIR_OID
            } else {
                APFS_ROOT_DIR_OID + i as u64
            };
            let child_file_id = APFS_ROOT_DIR_OID + (i as u64) + 1;
            drec.entry(parent_id).or_default().push(DrecChild {
                name,
                file_id: child_file_id,
                entry_type: DT_DIR,
            });
        }
        NamespaceIndex {
            drec_children: drec,
            inode_by_id: BTreeMap::new(),
            xattrs_by_id: BTreeMap::new(),
            dstream_real_total: HashMap::new(),
            dstream_refcnt: HashMap::new(),
            extent_refs_available: false,
        }
    }

    /// Audit r3 #F2: a 130-deep directory chain must hit the
    /// `MAX_TREE_DEPTH = 128` cap and surface as a `WalkSkip`
    /// row with `reason: "depth_cap_reached(128)"`. Pre-fix
    /// this truncated silently.
    #[test]
    fn walk_dir_depth_cap_emits_walk_skip() {
        // 130 nested dirs `d000` … `d129`. The cap fires at
        // depth=128, so the subtree starting at d128 is skipped.
        // Use leaked &'static str so the names live for the
        // whole test (NamespaceIndex's drec_children borrows
        // them by &'a str).
        let names: Vec<&'static str> = (0..130)
            .map(|i| -> &'static str { Box::leak(format!("d{:03}", i).into_boxed_str()) })
            .collect();
        let index = make_index_with_chain(&names);

        let mut entries: Vec<NamespaceEntry> = Vec::new();
        let mut truncated: Vec<crate::WalkSkip> = Vec::new();
        index.walk_into(&mut entries, &mut truncated);

        assert_eq!(
            truncated.len(),
            1,
            "exactly one WalkSkip expected at the depth cap; got {:?}",
            truncated
        );
        let skip = &truncated[0];
        assert_eq!(skip.reason, "depth_cap_reached(128)");
        assert!(
            skip.path.contains("d127") || skip.path.contains("d128"),
            "skip path should mention the cap-hitting dir; got: {}",
            skip.path
        );
        // We should have emitted at least MAX_TREE_DEPTH entries
        // before truncating (every nested dir up to the cap).
        assert!(
            entries.len() >= crate::MAX_TREE_DEPTH,
            "expected at least {} entries before truncation; got {}",
            crate::MAX_TREE_DEPTH,
            entries.len()
        );
    }

    /// Audit r3 #F3: a DREC cycle (a child whose file_id
    /// references an ancestor already in `visited`) must
    /// surface as a `WalkSkip` with
    /// `reason: "drec_cycle(file_id=X)"`. Pre-fix this skipped
    /// the recursion silently.
    #[test]
    fn walk_dir_drec_cycle_emits_walk_skip() {
        // Build root → A → B, where B's drec entry points
        // back at A's file_id. Walking A inserts file_id=3 into
        // `visited`; when we reach B's child (also file_id=3),
        // visited.insert returns false → emit a WalkSkip.
        const A_ID: u64 = APFS_ROOT_DIR_OID + 1; // 3
        const B_ID: u64 = APFS_ROOT_DIR_OID + 2; // 4

        let mut drec: BTreeMap<u64, Vec<DrecChild<'static>>> = BTreeMap::new();
        // root → A
        drec.entry(APFS_ROOT_DIR_OID).or_default().push(DrecChild {
            name: "A",
            file_id: A_ID,
            entry_type: DT_DIR,
        });
        // A → B
        drec.entry(A_ID).or_default().push(DrecChild {
            name: "B",
            file_id: B_ID,
            entry_type: DT_DIR,
        });
        // B → cycle back to A
        drec.entry(B_ID).or_default().push(DrecChild {
            name: "loop",
            file_id: A_ID,
            entry_type: DT_DIR,
        });

        let index = NamespaceIndex {
            drec_children: drec,
            inode_by_id: BTreeMap::new(),
            xattrs_by_id: BTreeMap::new(),
            dstream_real_total: HashMap::new(),
            dstream_refcnt: HashMap::new(),
            extent_refs_available: false,
        };
        let mut entries: Vec<NamespaceEntry> = Vec::new();
        let mut truncated: Vec<crate::WalkSkip> = Vec::new();
        index.walk_into(&mut entries, &mut truncated);

        assert_eq!(
            truncated.len(),
            1,
            "exactly one cycle WalkSkip expected; got {:?}",
            truncated
        );
        let skip = &truncated[0];
        assert!(
            skip.reason.contains("drec_cycle"),
            "reason should mention cycle; got: {}",
            skip.reason
        );
        assert!(
            skip.reason.contains(&format!("file_id={}", A_ID)),
            "reason should mention the cyclic file_id; got: {}",
            skip.reason
        );
    }
}
