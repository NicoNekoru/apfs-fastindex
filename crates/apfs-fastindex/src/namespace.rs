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
//!   `src/apfs_fastindex/aggregate.py`).

use std::collections::{BTreeMap, BTreeSet};

use crate::fs_record_body::{FsRecordKey, FsRecordRow, FsRecordValue};
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

/// Reconstruct `NamespaceEntry` rows and per-directory aggregates from one
/// volume's `FsRecordDump.records`.
///
/// Returns `(entries, aggregates)` in stable sorted-by-path order.
pub(crate) fn build_namespace(
    dump: &FsRecordDump,
) -> (Vec<NamespaceEntry>, Vec<DirectoryAggregate>) {
    let index = NamespaceIndex::from_records(&dump.records);
    let mut entries: Vec<NamespaceEntry> = Vec::new();
    index.walk_into(&mut entries);
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let aggregates = build_aggregates(&entries);
    (entries, aggregates)
}

struct NamespaceIndex<'a> {
    drec_children: BTreeMap<u64, Vec<DrecChild<'a>>>,
    inode_by_id: BTreeMap<u64, &'a crate::fs_record_body::InodeBody>,
    xattrs_by_id: BTreeMap<u64, BTreeMap<&'a str, &'a crate::fs_record_body::XattrBody>>,
}

struct DrecChild<'a> {
    name: &'a str,
    file_id: u64,
    entry_type: u8,
}

impl<'a> NamespaceIndex<'a> {
    fn from_records(records: &'a [FsRecordRow]) -> Self {
        let mut drec_children: BTreeMap<u64, Vec<DrecChild<'a>>> = BTreeMap::new();
        let mut inode_by_id: BTreeMap<u64, &'a crate::fs_record_body::InodeBody> = BTreeMap::new();
        let mut xattrs_by_id: BTreeMap<
            u64,
            BTreeMap<&'a str, &'a crate::fs_record_body::XattrBody>,
        > = BTreeMap::new();

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
                _ => {}
            }
        }

        // Stable child ordering inside each directory: SR-018 keeps stored
        // UTF-8 bytes verbatim, but sorted-by-name keeps the namespace
        // output deterministic across runs without claiming lookup
        // semantics.
        for children in drec_children.values_mut() {
            children.sort_by(|a, b| a.name.cmp(b.name));
        }

        Self {
            drec_children,
            inode_by_id,
            xattrs_by_id,
        }
    }

    fn walk_into(&self, out: &mut Vec<NamespaceEntry>) {
        // Root `.` is not part of `NamespaceEntry` output (the Python
        // `oracle_diff` and `ProofRawWalkBackend` both omit it). The root
        // still owns the per-directory aggregate row keyed by `.`.
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        visited.insert(APFS_ROOT_DIR_OID);
        self.walk_dir(APFS_ROOT_DIR_OID, ".", out, &mut visited);
    }

    fn walk_dir(
        &self,
        parent_id: u64,
        parent_path: &str,
        out: &mut Vec<NamespaceEntry>,
        visited: &mut BTreeSet<u64>,
    ) {
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
            out.push(NamespaceEntry {
                path: path.clone(),
                entry_kind: entry_kind.clone(),
                file_id: child.file_id,
                logical_size,
                symlink_target,
            });
            if matches!(entry_kind, EntryKind::Dir) && visited.insert(child.file_id) {
                self.walk_dir(child.file_id, &path, out, visited);
            }
        }
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
fn build_aggregates(entries: &[NamespaceEntry]) -> Vec<DirectoryAggregate> {
    let mut directories: BTreeSet<String> = BTreeSet::new();
    directories.insert(".".to_string());
    for entry in entries {
        if matches!(entry.entry_kind, EntryKind::Dir) {
            directories.insert(entry.path.clone());
        }
    }
    let mut contributors: BTreeMap<String, BTreeMap<u64, u64>> = BTreeMap::new();
    for path in &directories {
        contributors.insert(path.clone(), BTreeMap::new());
    }
    for entry in entries {
        if !matches!(entry.entry_kind, EntryKind::File) {
            continue;
        }
        for ancestor in ancestor_directories(&entry.path) {
            if let Some(map) = contributors.get_mut(&ancestor) {
                map.entry(entry.file_id).or_insert(entry.logical_size);
            }
        }
    }
    contributors
        .into_iter()
        .map(|(path, file_sizes)| {
            let unique_inode_logical_total = file_sizes.values().sum();
            let contributing_file_ids = file_sizes.keys().copied().collect();
            DirectoryAggregate {
                path,
                unique_inode_logical_total,
                contributing_file_ids,
            }
        })
        .collect()
}

/// Return every parent directory of `path`, ending with `"."` for the root.
/// `"a/b/c.txt"` -> `["a/b", "a", "."]`.
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
}
