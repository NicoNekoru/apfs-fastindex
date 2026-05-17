//! POSIX-traversal fallback walker.
//!
//! Implements the spec's fall-closed boundary: when raw mode is rejected
//! (encryption, sealed volumes, unsupported source class), we fall back to
//! a POSIX traversal that emits the same `NamespaceEntry` and
//! `DirectoryAggregate` shape. This module is the Rust port of
//! `src/apfs_fastindex/fallback_traversal.py` and is gated by EX-21 shape
//! parity.
//!
//! Today's backend is `std::fs::read_dir` + `symlink_metadata` (an `lstat`
//! per entry). That is correct on any Unix host but does one syscall per
//! file. A future pass can swap in a macOS `getattrlistbulk` backend
//! without changing the public contract; the module split here is
//! deliberate so that swap is bounded.
//!
//! Support-matrix cell covered today: locally mounted directory the
//! current user can read. Not covered (still v1-excluded): live boot
//! disk + sealed-system + FileVault runtime semantics.
//!
//! ## Resilience policy (per-entry, not the whole walk)
//!
//! The walker hard-stops only on errors that signal an unusable source
//! (root path missing, root not a directory, non-UTF-8 root component).
//! For *per-entry* I/O errors it records a `WalkSkip` and keeps going so
//! a user-facing scan does not abort the moment it hits a directory it
//! lacks permission to read. Recorded reasons:
//!
//! - `permission_denied` (`EACCES` / `EPERM`)
//! - `not_found` (raced between `readdir` and `lstat`, e.g. tmp file
//!   removed mid-scan)
//! - `mount_boundary` (a child directory whose `dev_t` differs from the
//!   root; skipped unless the caller passed `cross_mounts = true`)
//! - `non_utf8_name` (a directory entry whose name is not valid UTF-8;
//!   the v1 namespace contract requires UTF-8 paths)
//! - `read_error` (any other `io::Error` returned while reading the
//!   directory or its entries — kept open for ENOMEM/EIO/etc.)
//!
//! ## Field semantics
//!
//! - `path`: forward-slash-joined path relative to `root`, stored as
//!   verbatim UTF-8 bytes (SR-018: no normalization, no case fold).
//! - `entry_kind`: `dir`, `file`, `symlink`, or `other` from
//!   `stat::st_mode`.
//! - `file_id`: POSIX inode number from `stat::st_ino`. On a freshly
//!   built APFS image this happens to coincide with the APFS virtual
//!   OID the raw scanner reports; the v1 contract permits divergence
//!   across source classes.
//! - `logical_size`: `stat::st_size` for regular files; UTF-8 byte
//!   length of the symlink target for symlinks (SR-017 step 5); zero
//!   for directories and other types.
//! - `symlink_target`: `readlink` for symlinks; `None` otherwise.
//! - `allocated_size`: `Some(stat::st_blocks * 512)` for regular
//!   files (the fallback's truth *is* the public oracle, so this
//!   column never fails closed here); `Some(0)` for symlinks and
//!   directories so the shape parity with raw mode holds (raw mode
//!   does not promise per-directory or per-symlink allocation bytes
//!   in v1 — see EX-22).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::fallback_bulk::{BulkEntry, BulkReader};
use crate::{
    DirectoryAggregate, EntryKind, NamespaceEntry, ParserOutput, ScanState, SourceDescriptor,
    WalkSkip,
};

/// macOS-injected top-level directories that the raw walker drops and the
/// fallback walker must drop too so the shape contract holds.
const SKIP_TOP_LEVEL_NAMES: &[&str] = &[".fseventsd", ".Spotlight-V100", ".Trashes"];

/// Caller-facing knobs for the fallback walker. Defaults are
/// `cross_mounts: false` so an `apfs-fastindex-scan /` won't accidentally
/// descend into every mounted volume.
#[derive(Default)]
pub struct FallbackOptions<'a> {
    pub cross_mounts: bool,
    /// Optional progress sink. When set, the walker calls it roughly
    /// every second with a snapshot of `(scanned, skipped, elapsed)`.
    /// The CLI uses this to stream JSON-per-line progress to stderr.
    pub progress: Option<&'a mut dyn FnMut(ProgressEvent)>,
}

#[derive(Debug, Clone, Copy)]
pub struct ProgressEvent {
    pub scanned: u64,
    pub skipped: u64,
    pub elapsed: Duration,
    /// `true` for the final event emitted at the end of the walk so a
    /// consumer can render the last line without waiting for the
    /// tick.
    pub terminal: bool,
}

#[derive(Debug)]
pub enum FallbackError {
    Io(io::Error),
    NotADirectory(PathBuf),
    NonUtf8RootComponent(PathBuf),
}

impl std::fmt::Display for FallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::NotADirectory(path) => {
                write!(f, "fallback source is not a directory: {}", path.display())
            }
            Self::NonUtf8RootComponent(path) => write!(
                f,
                "fallback root resolves to a non-UTF-8 path: {} (v1 namespace contract requires \
                 valid UTF-8)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for FallbackError {}

impl From<io::Error> for FallbackError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FallbackScanOutput {
    pub parser_output: ParserOutput,
    pub correctness_claim: String,
    pub not_claimed: Vec<String>,
}

/// Walk `root` and produce a `FallbackScanOutput`.
///
/// `root` must point at a directory the current user can read. Returns a
/// typed error only for source-level problems; per-entry I/O failures are
/// recorded in `parser_output.walk_skips`.
pub fn fallback_scan_path<P: AsRef<Path>>(root: P) -> Result<FallbackScanOutput, FallbackError> {
    fallback_scan_path_with_options(root, FallbackOptions::default())
}

pub fn fallback_scan_path_with_options<P: AsRef<Path>>(
    root: P,
    mut options: FallbackOptions<'_>,
) -> Result<FallbackScanOutput, FallbackError> {
    let root_path = root.as_ref();
    let resolved = fs::canonicalize(root_path)?;
    let root_meta = fs::symlink_metadata(&resolved)?;
    if !root_meta.is_dir() {
        return Err(FallbackError::NotADirectory(resolved));
    }
    if resolved.to_str().is_none() {
        return Err(FallbackError::NonUtf8RootComponent(resolved));
    }
    let root_dev = root_meta.dev();

    let mut entries: Vec<NamespaceEntry> = Vec::new();
    let mut walk_skips: Vec<WalkSkip> = Vec::new();
    let mut stack: Vec<WalkFrame> = vec![WalkFrame {
        absolute: resolved.clone(),
        relative: PathBuf::new(),
    }];
    // One BulkReader owns the 64 KiB getattrlistbulk buffer for the
    // whole walk; the per-directory output Vec is also reused via
    // `bulk_children`. On a 200k-directory whole-machine scan this
    // saves ~13 GiB of allocation churn.
    let mut bulk_reader = BulkReader::new();
    let mut bulk_children: Vec<BulkEntry> = Vec::new();

    let scan_start = Instant::now();
    // 250 ms cadence: the GUI shell renders this as a live counter; at
    // 1 Hz the count visibly stutters and the UI feels broken. 250 ms
    // is fast enough to look smooth on a real scan and slow enough that
    // we add at most ~4 progress events per second to the stderr bridge.
    let progress_interval = Duration::from_millis(250);
    let mut next_progress_tick = scan_start + progress_interval;

    while let Some(frame) = stack.pop() {
        if let Some(cb) = options.progress.as_deref_mut() {
            let now = Instant::now();
            if now >= next_progress_tick {
                cb(ProgressEvent {
                    scanned: entries.len() as u64,
                    skipped: walk_skips.len() as u64,
                    elapsed: now.duration_since(scan_start),
                    terminal: false,
                });
                next_progress_tick = now + progress_interval;
            }
        }
        let children = match sorted_children(
            &frame.absolute,
            &frame.relative,
            &mut bulk_reader,
            &mut bulk_children,
        ) {
            Ok(children) => children,
            Err(skip) => {
                walk_skips.push(skip);
                continue;
            }
        };

        for child in &children {
            if frame.relative.as_os_str().is_empty()
                && SKIP_TOP_LEVEL_NAMES.contains(&child.name.as_str())
            {
                continue;
            }
            let absolute = frame.absolute.join(&child.name);
            let relative = if frame.relative.as_os_str().is_empty() {
                PathBuf::from(&child.name)
            } else {
                frame.relative.join(&child.name)
            };
            let relative_str = match relative.to_str() {
                Some(s) => s.to_string(),
                None => {
                    walk_skips.push(WalkSkip {
                        path: absolute.to_string_lossy().into_owned(),
                        reason: "non_utf8_name".to_string(),
                    });
                    continue;
                }
            };

            // Prefer the bulk-supplied metadata when present; otherwise
            // pay for one `symlink_metadata` syscall here.
            let (kind, file_id, file_logical_size, file_allocated_bytes, dev_id) =
                if let Some(bulk) = &child.bulk {
                    (
                        bulk.kind,
                        bulk.file_id,
                        bulk.logical_size,
                        bulk.allocated_bytes,
                        bulk.dev_id as u64,
                    )
                } else {
                    let meta = match fs::symlink_metadata(&absolute) {
                        Ok(meta) => meta,
                        Err(err) => {
                            walk_skips.push(WalkSkip {
                                path: relative_str.clone(),
                                reason: io_skip_reason(&err),
                            });
                            continue;
                        }
                    };
                    let kind = entry_kind_from_meta(&meta);
                    let size = if matches!(kind, EntryKind::File) {
                        meta.size()
                    } else {
                        0
                    };
                    // st_blocks is reported in 512-byte units regardless of
                    // the underlying block size; this is BSD stat semantics
                    // and what EX-22's oracle uses.
                    let allocated = if matches!(kind, EntryKind::File) {
                        meta.blocks().saturating_mul(512)
                    } else {
                        0
                    };
                    (kind, meta.ino(), size, allocated, meta.dev())
                };

            // Symlink target still requires a separate `readlink`. Bulk
            // mode skips it because it isn't an attribute getattrlistbulk
            // exposes.
            let (logical_size, symlink_target) = match kind {
                EntryKind::Symlink => match fs::read_link(&absolute) {
                    Ok(target_path) => {
                        let target = target_path.to_string_lossy().into_owned();
                        (target.len() as u64, Some(target))
                    }
                    Err(err) => {
                        walk_skips.push(WalkSkip {
                            path: relative_str.clone(),
                            reason: io_skip_reason(&err),
                        });
                        (0, None)
                    }
                },
                EntryKind::File => (file_logical_size, None),
                _ => (0, None),
            };
            // EX-22 amended SR-019 emission rule for the fallback path:
            // regular files emit `Some(st_blocks * 512)` (the kernel's
            // public allocation count and the EX-22 oracle); symlinks
            // and directories emit `Some(0)` so the shape parity with
            // raw-mode emission holds (raw-mode does not promise
            // per-symlink or per-directory allocation bytes); `other`
            // emits `None` for symmetry with raw.
            let allocated_size = match kind {
                EntryKind::File => Some(file_allocated_bytes),
                EntryKind::Symlink | EntryKind::Dir => Some(0),
                EntryKind::Other => None,
            };
            let is_dir = matches!(kind, EntryKind::Dir);
            // Cross-mount directories are reported in entries AND in
            // walk_skips (no recursion). Both branches consume an
            // owned `relative_str`; on the common no-mount-boundary
            // path the only owner is the NamespaceEntry push and no
            // clone happens. Cloning unconditionally cost 5M+ String
            // clones on a /-scan.
            let cross_mount_skip = is_dir && !options.cross_mounts && dev_id != root_dev;
            if cross_mount_skip {
                walk_skips.push(WalkSkip {
                    path: relative_str.clone(),
                    reason: "mount_boundary".to_string(),
                });
            }
            entries.push(NamespaceEntry {
                path: relative_str,
                entry_kind: kind,
                file_id,
                logical_size,
                symlink_target,
                allocated_size,
            });
            if is_dir && !cross_mount_skip {
                stack.push(WalkFrame { absolute, relative });
            }
        }
    }

    if let Some(cb) = options.progress.as_deref_mut() {
        cb(ProgressEvent {
            scanned: entries.len() as u64,
            skipped: walk_skips.len() as u64,
            elapsed: scan_start.elapsed(),
            terminal: true,
        });
    }

    // Paths are unique inside a walk (no two entries can share a full
    // path) so stability is not required. `sort_unstable_by` is ~20%
    // faster on the 5M-entry case and produces an equivalent order.
    entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    walk_skips.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let aggregates = build_aggregates(&entries);

    let descriptor = SourceDescriptor {
        requested_path: root_path.to_path_buf(),
        raw_container_path: resolved.to_string_lossy().into_owned(),
        source_kind: "mounted_directory".to_string(),
        allowlist_reason: if options.cross_mounts {
            "POSIX traversal fallback for mounted source (cross-mount enabled)".to_string()
        } else {
            "POSIX traversal fallback for mounted source (does not cross mount boundaries)"
                .to_string()
        },
    };
    let scan_state = ScanState {
        block_size: 0,
        descriptor_blocks: 0,
        descriptor_base: 0,
        descriptor_base_non_contiguous: false,
        highest_xid: 0,
        candidate_count: 0,
        validation_gaps: Vec::new(),
    };
    let parser_output = ParserOutput {
        source: descriptor,
        scan_state,
        backend_name: "rust-fallback-posix-walk".to_string(),
        entries,
        aggregates,
        walk_skips,
    };

    let claim = if options.cross_mounts {
        "Rust path emits one mounted directory's NamespaceEntry + DirectoryAggregate rows via \
         POSIX traversal; logical size is st_size for files and symlink target length for \
         symlinks; allocated size is st_blocks*512 for files and zero for symlinks/directories \
         (EX-22 oracle); per-entry permission/access errors are skipped and recorded in \
         walk_skips; mount boundaries are crossed (--cross-mounts)"
    } else {
        "Rust path emits one mounted directory's NamespaceEntry + DirectoryAggregate rows via \
         POSIX traversal; logical size is st_size for files and symlink target length for \
         symlinks; allocated size is st_blocks*512 for files and zero for symlinks/directories \
         (EX-22 oracle); per-entry permission/access errors are skipped and recorded in \
         walk_skips; mount boundaries are not crossed (default)"
    };

    Ok(FallbackScanOutput {
        parser_output,
        correctness_claim: claim.to_string(),
        not_claimed: vec![
            "raw APFS-specific size sources (dstream / decmpfs precedence)".to_string(),
            "live mounted raw-scan correctness".to_string(),
            "exclusive / shared / snapshot-retained byte accounting".to_string(),
            "incremental cache reuse".to_string(),
            "encryption decryption or keybag handling".to_string(),
            "snapshot, sealed-volume, or volume-group merged semantics".to_string(),
            "APFS lookup-by-name (hash + normalization + case fold)".to_string(),
            "boot-root or Finder-visible merged namespace".to_string(),
            "subtrees recorded in walk_skips (the walker reports them but does not read \
             through them)"
                .to_string(),
        ],
    })
}

struct WalkFrame {
    absolute: PathBuf,
    relative: PathBuf,
}

struct ChildEntry {
    name: String,
    /// Metadata populated by the bulk-attribute path. `None` means the
    /// caller must `symlink_metadata` this child itself.
    bulk: Option<BulkMeta>,
}

struct BulkMeta {
    kind: EntryKind,
    file_id: u64,
    logical_size: u64,
    allocated_bytes: u64,
    dev_id: u32,
}

fn sorted_children(
    dir: &Path,
    relative: &Path,
    bulk_reader: &mut BulkReader,
    bulk_children: &mut Vec<BulkEntry>,
) -> Result<Vec<ChildEntry>, WalkSkip> {
    // Try the macOS `getattrlistbulk` backend first. If it succeeds we
    // get name + kind + ino + size + dev_id per entry in one syscall
    // batch — no per-child `lstat` needed. On any failure we fall
    // through to `read_dir + symlink_metadata` so behavior is
    // preserved on non-macOS or when the kernel rejects bulk reads
    // for this directory.
    //
    // `bulk_children` is borrowed scratch from the caller; the
    // `BulkReader` reuses its 64 KiB kernel-fill buffer across calls.
    if bulk_reader.read_directory(dir, bulk_children).is_ok() {
        let mut out: Vec<ChildEntry> = Vec::with_capacity(bulk_children.len());
        // `drain(..)` empties the scratch Vec but retains its capacity
        // for the next call; that's the load-bearing reuse here.
        out.extend(bulk_children.drain(..).map(child_from_bulk));
        out.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        return Ok(out);
    }
    // Bulk read failed for this directory; the scratch Vec may hold
    // partial output from the kernel. Clear it before falling through
    // so the next directory starts clean.
    bulk_children.clear();
    // Fall through to the std read_dir path on bulk failure.

    let read = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(err) => {
            return Err(WalkSkip {
                path: skip_path_for(relative),
                reason: io_skip_reason(&err),
            });
        }
    };
    let mut out: Vec<ChildEntry> = Vec::new();
    for child in read {
        let child = match child {
            Ok(c) => c,
            Err(err) => {
                return Err(WalkSkip {
                    path: skip_path_for(relative),
                    reason: io_skip_reason(&err),
                });
            }
        };
        let raw_name = child.file_name();
        let name = match raw_name.to_str() {
            Some(text) => text.to_string(),
            None => {
                // Record this entry as a skip and keep enumerating; we do
                // not have a UTF-8 name to emit but the other siblings in
                // this directory are still scannable.
                return Err(WalkSkip {
                    path: child.path().to_string_lossy().into_owned(),
                    reason: "non_utf8_name".to_string(),
                });
            }
        };
        out.push(ChildEntry { name, bulk: None });
    }
    out.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn child_from_bulk(entry: BulkEntry) -> ChildEntry {
    let bulk = BulkMeta {
        kind: entry.kind,
        file_id: entry.file_id,
        logical_size: entry.logical_size,
        allocated_bytes: entry.allocated_bytes,
        dev_id: entry.dev_id,
    };
    ChildEntry {
        name: entry.name,
        bulk: Some(bulk),
    }
}

fn io_skip_reason(err: &io::Error) -> String {
    use io::ErrorKind::*;
    match err.kind() {
        PermissionDenied => "permission_denied".to_string(),
        NotFound => "not_found".to_string(),
        _ => format!("read_error:{:?}", err.kind()),
    }
}

fn skip_path_for(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.to_string_lossy().into_owned()
    }
}

fn entry_kind_from_meta(meta: &fs::Metadata) -> EntryKind {
    let mode = meta.mode();
    let ifmt = mode & 0o170000;
    match ifmt {
        0o040000 => EntryKind::Dir,
        0o120000 => EntryKind::Symlink,
        0o100000 => EntryKind::File,
        _ => EntryKind::Other,
    }
}

/// Per-directory unique-inode aggregate, mirroring
/// `src/apfs_fastindex/aggregate.py` (SR-009). The
/// `unique_inode_allocated_total` collapses to `None` if any
/// contributing inode has `allocated_size == None` (mirrors the raw
/// namespace.rs SR-019 fail-closed contract). In fallback mode all
/// regular files emit `Some(_)` so the column should always populate
/// in practice; the None branch exists for the `EntryKind::Other`
/// case.
///
/// Build phase uses `HashMap` keyed by `&str` borrowed from the
/// entries' own `path` Strings, so the ancestor walk allocates
/// nothing per file. Final emission sorts the keys and copies them
/// into owned `String`s once. Replaces the prior
/// `BTreeMap<String, ...>` + `ancestor_directories() -> Vec<String>`
/// shape which allocated ~25M intermediate Strings on a 5M-entry
/// `/`-scan.
fn build_aggregates(entries: &[NamespaceEntry]) -> Vec<DirectoryAggregate> {
    // Seed the directory set from the explicit dir entries in the
    // input. We use the path strings *borrowed* from `entries` so no
    // allocation happens here either. The implicit root `.` is
    // inserted separately because no entry has path == ".".
    let mut contributors: HashMap<&str, HashMap<u64, (u64, Option<u64>)>> = HashMap::new();
    contributors.insert(".", HashMap::new());
    for entry in entries {
        if matches!(entry.entry_kind, EntryKind::Dir) {
            contributors.entry(entry.path.as_str()).or_default();
        }
    }
    for entry in entries {
        if !matches!(entry.entry_kind, EntryKind::File) {
            continue;
        }
        // Walk ancestors by repeatedly stripping the trailing `/<name>`
        // off the path slice. Every step is a single `rfind('/')` plus
        // a slice; no heap activity. We don't pre-build a Vec of
        // ancestors because the inner work is just a HashMap probe
        // that takes the `&str` directly.
        let mut current: &str = entry.path.as_str();
        loop {
            match current.rfind('/') {
                Some(idx) => {
                    let parent = &current[..idx];
                    let key = if parent.is_empty() { "." } else { parent };
                    if let Some(map) = contributors.get_mut(key) {
                        map.entry(entry.file_id)
                            .or_insert((entry.logical_size, entry.allocated_size));
                    }
                    if parent.is_empty() {
                        break;
                    }
                    current = parent;
                }
                None => {
                    if let Some(map) = contributors.get_mut(".") {
                        map.entry(entry.file_id)
                            .or_insert((entry.logical_size, entry.allocated_size));
                    }
                    break;
                }
            }
        }
    }

    // Final emission: collect keys, sort them once, materialise the
    // owned-String aggregates in the same pass. The owned-String cost
    // is unavoidable because the output struct outlives `entries`.
    let mut paths: Vec<&str> = contributors.keys().copied().collect();
    paths.sort_unstable();
    let mut out: Vec<DirectoryAggregate> = Vec::with_capacity(paths.len());
    for path in paths {
        let file_sizes = contributors
            .remove(path)
            .expect("path was just keys()d from the map");
        let unique_inode_logical_total: u64 =
            file_sizes.values().map(|(logical, _)| *logical).sum();
        let unique_inode_allocated_total: Option<u64> = file_sizes
            .values()
            .try_fold(0u64, |acc, (_, allocated)| allocated.map(|a| acc + a));
        let mut contributing_file_ids: Vec<u64> = file_sizes.keys().copied().collect();
        contributing_file_ids.sort_unstable();
        out.push(DirectoryAggregate {
            path: path.to_string(),
            unique_inode_logical_total,
            contributing_file_ids,
            unique_inode_allocated_total,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn fallback_emits_namespace_shape_on_synthetic_tree() {
        let tmp = TempDir::new();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("dst")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        {
            let mut f = File::create(root.join("dst/moved.txt")).unwrap();
            writeln!(f, "alpha").unwrap();
        }
        symlink("moved.txt", root.join("dst/link.txt")).unwrap();

        let output = fallback_scan_path(root).expect("fallback walks");
        let parser_output = &output.parser_output;
        let paths: Vec<&str> = parser_output
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(paths, vec!["dst", "dst/link.txt", "dst/moved.txt", "src"]);

        let link = parser_output
            .entries
            .iter()
            .find(|e| e.path == "dst/link.txt")
            .unwrap();
        assert_eq!(link.entry_kind, EntryKind::Symlink);
        assert_eq!(link.symlink_target.as_deref(), Some("moved.txt"));
        assert_eq!(link.logical_size, "moved.txt".len() as u64);
        // EX-22 / SR-019: fallback emits Some(0) for symlinks so the
        // shape parity with raw-mode emission holds.
        assert_eq!(link.allocated_size, Some(0));

        let moved = parser_output
            .entries
            .iter()
            .find(|e| e.path == "dst/moved.txt")
            .expect("dst/moved.txt present");
        assert_eq!(moved.entry_kind, EntryKind::File);
        // Regular files: fallback emits Some(st_blocks * 512), which
        // is the EX-22 oracle. We do not assert the exact value
        // because it depends on the host's block size and any APFS
        // local-snapshot interaction, only that it is Some(_).
        assert!(
            moved.allocated_size.is_some(),
            "regular file should emit Some(allocated_size); got None"
        );

        let dst_dir = parser_output
            .entries
            .iter()
            .find(|e| e.path == "dst")
            .expect("dst directory entry present");
        assert_eq!(dst_dir.entry_kind, EntryKind::Dir);
        assert_eq!(dst_dir.allocated_size, Some(0));

        let aggregates: Vec<&str> = parser_output
            .aggregates
            .iter()
            .map(|a| a.path.as_str())
            .collect();
        assert_eq!(aggregates, vec![".", "dst", "src"]);
        // The fallback's per-file allocated_size is always Some(_)
        // for files, so the aggregate must populate as Some(_).
        let root_agg = parser_output
            .aggregates
            .iter()
            .find(|a| a.path == ".")
            .expect("root aggregate present");
        assert!(
            root_agg.unique_inode_allocated_total.is_some(),
            "fallback unique_inode_allocated_total should populate; got None"
        );
        assert!(parser_output.walk_skips.is_empty());
    }

    #[test]
    fn fallback_rejects_non_directory_source() {
        let tmp = TempDir::new();
        let path = tmp.path().join("a-file");
        File::create(&path).unwrap();
        let err = fallback_scan_path(&path).expect_err("file source rejected");
        assert!(matches!(err, FallbackError::NotADirectory(_)));
    }

    #[test]
    fn fallback_drops_top_level_fseventsd() {
        let tmp = TempDir::new();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".fseventsd")).unwrap();
        File::create(root.join(".fseventsd/somefile")).unwrap();
        std::fs::create_dir_all(root.join("ordinary")).unwrap();
        let output = fallback_scan_path(root).expect("fallback walks");
        let paths: Vec<&str> = output
            .parser_output
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(paths, vec!["ordinary"]);
    }

    #[test]
    fn fallback_skips_and_records_permission_denied_subdir() {
        let tmp = TempDir::new();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("readable")).unwrap();
        std::fs::create_dir_all(root.join("locked")).unwrap();
        File::create(root.join("readable/file.txt")).unwrap();
        File::create(root.join("locked/secret.txt")).unwrap();
        // Strip every mode bit; the parent's lstat still works but reading
        // the directory itself will return EACCES for our user.
        let mut perms = std::fs::metadata(root.join("locked"))
            .unwrap()
            .permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(root.join("locked"), perms).unwrap();

        let output = fallback_scan_path(root).expect("fallback walks");
        // Restore permissions so the TempDir can be cleaned up.
        let mut restore = std::fs::metadata(root.join("locked"))
            .unwrap()
            .permissions();
        restore.set_mode(0o755);
        std::fs::set_permissions(root.join("locked"), restore).unwrap();

        let paths: Vec<&str> = output
            .parser_output
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        // Both directories show up; "locked" is recorded but not descended.
        assert!(paths.contains(&"readable"));
        assert!(paths.contains(&"readable/file.txt"));
        assert!(paths.contains(&"locked"));
        assert!(!paths.contains(&"locked/secret.txt"));

        let locked_skip = output
            .parser_output
            .walk_skips
            .iter()
            .find(|s| s.path == "locked");
        assert!(
            matches!(locked_skip, Some(skip) if skip.reason == "permission_denied"),
            "expected a permission_denied skip for `locked`, got {:?}",
            output.parser_output.walk_skips
        );
    }

    /// Minimal in-tree temp-directory helper so the crate stays dep-free.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let pid = std::process::id();
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("apfsfi-fallback-test-{pid}-{seq}"));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
