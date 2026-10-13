//! POSIX-traversal fallback walker.
//!
//! Implements the spec's fall-closed boundary: when raw mode is rejected
//! (encryption, sealed volumes, unsupported source class), we fall back to
//! a POSIX traversal that emits the same `NamespaceEntry` and
//! `DirectoryAggregate` shape. This module is the Rust port of
//! `src/apfs_fastindex/fallback_traversal.py` and is gated by EX-21 shape
//! parity.
//!
//! macOS uses `getattrlistbulk`, with descriptor-relative POSIX traversal
//! on a bulk failure. Other Unix hosts use `read_dir` and `lstat`.
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
//! - `directory_changed` (a queued path no longer names the observed
//!   directory, or a changing directory iterator returned a name twice)
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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::fallback_bulk::{BulkEntry, BulkReader};
use crate::{
    DirectoryAggregate, EntryKind, NamespaceEntry, ParserOutput, ScanState, SourceDescriptor,
    WalkSkip,
};

/// macOS-injected top-level directories that the raw walker drops and the
/// fallback walker must drop too so the shape contract holds.
const SKIP_TOP_LEVEL_NAMES: &[&str] = &[".fseventsd", ".Spotlight-V100", ".Trashes"];

/// Set of absolute paths that the walker should refuse to descend
/// into, regardless of inode-dedup state. Built once per scan from
/// `/usr/share/firmlinks` so that `/System/Volumes/Data/<source>`
/// paths are explicitly skipped on `/`-class scans — that prevents
/// the data-volume duplicate of every firmlink target from
/// appearing in the namespace and (more importantly) prevents the
/// parallel walker from racing it ahead of the firmlink path and
/// making the deeper `/System/Volumes/Data/...` path the canonical
/// one. The set is empty when scanning anywhere else.
type FirmlinkOverlaySet = HashSet<PathBuf>;

/// Read `/usr/share/firmlinks` and return the set of underlying
/// paths the walker should refuse to descend into. Returns an empty
/// set on hosts without the file (older macOS, non-macOS) or on any
/// I/O error — the inode-dedup path is the safety net.
///
/// File format (since macOS Catalina): one firmlink per line as
/// `<source-path>\t<destination-rel>`. The underlying path on the
/// Data volume is `/System/Volumes/Data` joined with the destination.
fn read_firmlink_overlays(scan_root: &Path) -> FirmlinkOverlaySet {
    // Only meaningful when the scan root is `/`; narrower scans
    // either don't reach `/System/Volumes/Data` or have the user
    // intentionally aimed at it.
    if scan_root != Path::new("/") {
        return FirmlinkOverlaySet::new();
    }
    let path = Path::new("/usr/share/firmlinks");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return FirmlinkOverlaySet::new();
    };
    firmlink_overlays_from_contents(&contents)
}

fn firmlink_overlays_from_contents(contents: &str) -> FirmlinkOverlaySet {
    let mut out = FirmlinkOverlaySet::new();
    for line in contents.lines() {
        let Some((source, destination)) = line.split_once('\t') else {
            continue;
        };
        let destination = Path::new(destination.trim());
        if source.trim().is_empty()
            || destination.as_os_str().is_empty()
            || !destination
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            continue;
        }
        let underlying = Path::new("/System/Volumes/Data").join(destination);
        out.insert(underlying);
    }
    out
}

/// Bound untrusted CLI/FFI worker counts before allocating or spawning.
/// Existing normal settings are unchanged, and zero still selects serial.
const MAX_FALLBACK_THREADS: usize = 64;

fn normalize_fallback_threads(requested: usize) -> usize {
    requested.clamp(1, MAX_FALLBACK_THREADS)
}

/// Tracks `(dev_id, file_id)` pairs for every directory the walker has
/// committed to enumerate. Used to detect firmlink-presented duplicates
/// (macOS firmlinks expose the Data volume's `/Users`, `/Applications`,
/// `/Library`, etc. inside `/` as well as at `/System/Volumes/Data/...`;
/// both paths share the same inode) and any other case where two paths
/// resolve to the same physical directory (bind mounts, volume overlays,
/// future overlay mechanisms).
///
/// `check_and_mark` returns `true` only on the first observation of an
/// inode; subsequent observations get `false` and the walker treats the
/// directory as a duplicate (records a `walk_skip` and skips both emit
/// and recurse).
///
/// Internal mutex is shared across parallel workers; the per-call cost
/// is a single `HashSet::insert` under a lock, which at ~200k
/// directories on a `/`-scan totals well under 50 ms of overhead — far
/// less than even one missed `lstat`.
/// Number of mutex-shards on `VisitedDirs`. With T=4 workers and
/// 16 shards we expect 4× under-utilisation per shard — i.e. the
/// odds of two workers colliding on the same shard at the same
/// moment are ~1/16. EX-25 measured the mutex as the second-
/// largest single-point contention behind the work queue; this
/// drops the wait-on-mutex term by ~16× on /-class scans.
///
/// Power-of-two so the shard pick is a bit-mask instead of a mod.
const VISITED_SHARDS: usize = 16;
const VISITED_SHARD_MASK: usize = VISITED_SHARDS - 1;

struct VisitedDirs {
    /// Sharded `(dev, ino)` dedup set. Each visit hashes into one
    /// of `VISITED_SHARDS` independent buckets; under the EX-25
    /// walk profile (~200k dirs on /, evenly inode-distributed)
    /// each shard holds ~12.5k entries — small enough that
    /// rehashing on the FxHashSet is amortised cheaply. Shards
    /// use fxhash (already a dep) since the keys are kernel
    /// inode pairs, non-adversarial.
    shards: [Mutex<rustc_hash::FxHashSet<(u64, u64)>>; VISITED_SHARDS],
}

impl VisitedDirs {
    fn new() -> Self {
        // `Default::default()` for the array isn't const, and
        // `[Mutex::new(...); 16]` requires the inner be `Copy`,
        // which Mutex isn't. Use `from_fn` to construct each
        // shard individually.
        Self {
            shards: std::array::from_fn(|_| Mutex::new(rustc_hash::FxHashSet::default())),
        }
    }

    /// Pick a shard for `(dev, ino)`. Mixes the two u64s into a
    /// single hash via fxhash-style multiply-shift; APFS inodes
    /// for a single volume share a `dev`, so the `ino` term has
    /// to carry the mixing. Bitwise-masked to `VISITED_SHARDS`.
    #[inline]
    fn shard_idx(dev: u64, ino: u64) -> usize {
        // Cheap two-step mix — multiply ino by a 64-bit odd
        // constant (the fxhash multiplier) and XOR dev's high
        // bits. Good distribution for non-adversarial inode pairs.
        let h = ino.wrapping_mul(0x517c_c1b7_2722_0a95).wrapping_add(dev);
        (h as usize) & VISITED_SHARD_MASK
    }

    fn mark_root(&self, dev: u64, ino: u64) {
        let s = Self::shard_idx(dev, ino);
        self.shards[s].lock().unwrap().insert((dev, ino));
    }

    fn check_and_mark(&self, dev: u64, ino: u64) -> bool {
        let s = Self::shard_idx(dev, ino);
        self.shards[s].lock().unwrap().insert((dev, ino))
    }
}

/// Caller-facing knobs for the fallback walker. Defaults are
/// `cross_mounts: false` so an `apfs-fastindex-scan /` won't accidentally
/// descend into every mounted volume, and `threads: 1` so library
/// callers preserve historical single-threaded semantics until they opt
/// into parallelism.
#[derive(Default)]
pub struct FallbackOptions<'a> {
    pub cross_mounts: bool,
    /// Optional progress sink. When set, the walker calls it roughly
    /// every 250 ms with a snapshot of `(scanned, skipped, elapsed)`.
    /// The CLI uses this to stream JSON-per-line progress to stderr.
    ///
    /// Fires from both the serial and parallel walkers. The `+ Send`
    /// bound is what lets the parallel walker hand the callback to a
    /// dedicated progress thread inside `std::thread::scope`; that
    /// thread samples per-worker atomic counters and calls the
    /// callback on the same cadence as the serial path. The bound is
    /// satisfied by any closure that doesn't capture a non-Send
    /// resource (the CLI's `let mut progress_writer = |event| { ...
    /// stderr ... };` qualifies, and most reasonable embedders do too).
    pub progress: Option<&'a mut (dyn FnMut(ProgressEvent) + Send)>,
    /// Number of worker threads for the parallel walker. `0` and `1`
    /// both select the serial implementation (preserves the original
    /// single-threaded behaviour and the FnMut progress contract).
    /// EX-25 measured a 2.47× speedup at T=4 on Apple silicon; the
    /// CLI default is `min(hw.physicalcpu, 4)`. Higher values
    /// regress (T=8 cost 4× T=1 sys-CPU for 1.94× throughput; T=14
    /// cost 9.3× T=1 sys-CPU for 1.38× throughput — see EX-25 for
    /// the contention shape).
    pub threads: usize,
    /// Skip the per-directory aggregate build. The aggregates
    /// pass walks every file's ancestor chain, accumulating
    /// `(file_id → (logical, allocated))` per directory; on a
    /// /-class scan that's ~90 ms (~17% of total scan wall).
    /// FFI callers (the SwiftUI app) reconstruct subtree totals
    /// from the indexed tree's `value_logical`/`value_allocated`
    /// fields, so the legacy aggregate Vec is dead weight for
    /// them. CLI emits aggregates in the JSON output, so the
    /// CLI keeps the default (`false`); FFI flips this to
    /// `true`.
    pub skip_aggregates: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ProgressEvent {
    pub scanned: u64,
    pub skipped: u64,
    /// Sum of `entry.logical_size` for every entry scanned so far —
    /// lets a UI render a determinate-style progress bar with the
    /// volume's used-bytes as the denominator.
    pub bytes: u64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackScanOutput {
    pub parser_output: ParserOutput,
    pub correctness_claim: String,
    pub not_claimed: Vec<String>,
}

/// Device identity belongs to traversal and aggregate construction, not the
/// public wire schema. Moving this wrapper into NamespaceEntry does not clone
/// names, targets, or entries, and needs no parallel path-to-device map.
struct WalkEntry {
    entry: NamespaceEntry,
    dev_id: u64,
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
    options: FallbackOptions<'_>,
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
    let root_ino = root_meta.ino();
    let cross_mounts = options.cross_mounts;
    let threads = normalize_fallback_threads(options.threads);

    // Seed the directory-inode dedup tracker with the root so any path
    // that loops back to it (rare but possible via symlink misuse or
    // weird overlay mounts) is caught.
    let visited_dirs = Arc::new(VisitedDirs::new());
    visited_dirs.mark_root(root_dev, root_ino);

    // Build the firmlink-overlay skip set. Only non-empty when the
    // scan root is `/`; on narrower scans `/usr/share/firmlinks` is
    // either not on the walk path or the user has intentionally
    // aimed inside `/System/Volumes/Data`.
    let firmlink_overlays = Arc::new(read_firmlink_overlays(&resolved));

    let scan_start = Instant::now();
    let (mut entries, mut walk_skips) = if threads <= 1 {
        scan_serial(
            &resolved,
            (root_dev, root_ino),
            cross_mounts,
            options.progress,
            &visited_dirs,
            &firmlink_overlays,
            scan_start,
        )
    } else {
        scan_parallel(
            &resolved,
            (root_dev, root_ino),
            cross_mounts,
            threads,
            options.progress,
            &visited_dirs,
            &firmlink_overlays,
            scan_start,
        )
    };
    let walk_done = Instant::now();

    // Paths are unique inside a walk (no two entries can share a full
    // path) so stability is not required. `sort_unstable_by` is ~20%
    // faster on the 5M-entry case and produces an equivalent order.
    // Parallel mode produces entries in arbitrary thread-interleaved
    // order, so the post-walk sort is doing real work for it; serial
    // mode produces approximately-DFS order and the sort completes
    // quickly via the existing already-mostly-sorted optimisation.
    entries.sort_unstable_by(|a, b| a.entry.path.cmp(&b.entry.path));
    walk_skips.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let sort_done = Instant::now();
    let aggregates = if options.skip_aggregates {
        Vec::new()
    } else {
        build_aggregates(&entries)
    };
    let aggregates_done = Instant::now();
    let entries: Vec<NamespaceEntry> = entries.into_iter().map(|row| row.entry).collect();

    // Optional per-phase timing dump for the perf_probe ablation
    // runs. Off by default; gated on `APFS_PHASE_TIMINGS=1` so
    // production callers don't pay even the env-var lookup cost.
    if std::env::var("APFS_PHASE_TIMINGS").as_deref() == Ok("1") {
        eprintln!(
            "[phase] walk={:.1}ms sort={:.1}ms aggregates={:.1}ms entries={} skips={}",
            walk_done.duration_since(scan_start).as_secs_f64() * 1000.0,
            sort_done.duration_since(walk_done).as_secs_f64() * 1000.0,
            aggregates_done.duration_since(sort_done).as_secs_f64() * 1000.0,
            entries.len(),
            walk_skips.len(),
        );
    }

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

    let mount_clause = if cross_mounts {
        "mount boundaries are crossed (--cross-mounts)"
    } else {
        "mount boundaries are not crossed (default)"
    };
    let thread_clause = if threads <= 1 {
        "single-threaded walk".to_string()
    } else {
        format!("up to {}-thread parallel walk (EX-25 validated)", threads)
    };
    let emitted_rows = if options.skip_aggregates {
        "NamespaceEntry rows"
    } else {
        "NamespaceEntry + DirectoryAggregate rows"
    };
    let claim = format!(
        "Rust path emits one mounted directory's {emitted_rows} via \
         POSIX traversal ({thread_clause}); logical size is st_size for files and symlink target \
         length for symlinks; allocated size is st_blocks*512 for files and zero for \
         symlinks/directories (EX-22 oracle); per-entry permission/access errors are skipped and \
         recorded in walk_skips; {mount_clause}"
    );

    let not_claimed = vec![
        "raw APFS-specific size sources (dstream / decmpfs precedence)".to_string(),
        "live mounted raw-scan correctness".to_string(),
        "atomic snapshot of a changing mounted namespace".to_string(),
        "exclusive / shared / snapshot-retained byte accounting".to_string(),
        "incremental cache reuse".to_string(),
        "encryption decryption or keybag handling".to_string(),
        "snapshot, sealed-volume, or volume-group merged semantics".to_string(),
        "APFS lookup-by-name (hash + normalization + case fold)".to_string(),
        "boot-root or Finder-visible merged namespace".to_string(),
        "subtrees recorded in walk_skips (the walker reports them but does not read \
             through them)"
            .to_string(),
    ];
    // Parallel-mode progress is now wired through the in-scope
    // progress thread; the previous not_claimed entry no longer
    // applies and the `threads` field above already disambiguates
    // the regime.

    Ok(FallbackScanOutput {
        parser_output,
        correctness_claim: claim,
        not_claimed,
    })
}

/// Single-threaded depth-first walk. Preserves the original walker
/// shape (and therefore the `&mut dyn FnMut(ProgressEvent)` progress
/// callback contract — the callback fires from this thread only).
fn scan_serial(
    resolved: &Path,
    root_identity: (u64, u64),
    cross_mounts: bool,
    progress: Option<&mut (dyn FnMut(ProgressEvent) + Send)>,
    visited_dirs: &VisitedDirs,
    firmlink_overlays: &FirmlinkOverlaySet,
    scan_start: Instant,
) -> (Vec<WalkEntry>, Vec<WalkSkip>) {
    let mut progress = progress;
    let mut entries: Vec<WalkEntry> = Vec::new();
    let mut walk_skips: Vec<WalkSkip> = Vec::new();
    let mut stack: Vec<WalkFrame> = vec![WalkFrame {
        absolute: resolved.to_path_buf(),
        relative: PathBuf::new(),
        expected_identity: root_identity,
    }];
    let mut bulk_reader = BulkReader::new();
    let mut bulk_children: Vec<BulkEntry> = Vec::new();
    let mut new_frames: Vec<WalkFrame> = Vec::new();

    let progress_interval = Duration::from_millis(250);
    let mut next_progress_tick = scan_start + progress_interval;
    // Running sum of `entry.logical_size`. Updated at each
    // progress tick from `entries[bytes_cursor..]` so the
    // per-entry path stays untouched.
    let mut bytes_total: u64 = 0;
    let mut bytes_cursor: usize = 0;

    while let Some(frame) = stack.pop() {
        if let Some(cb) = progress.as_mut() {
            let now = Instant::now();
            if now >= next_progress_tick {
                for e in &entries[bytes_cursor..] {
                    bytes_total = bytes_total.saturating_add(e.entry.logical_size);
                }
                bytes_cursor = entries.len();
                cb(ProgressEvent {
                    scanned: entries.len() as u64,
                    skipped: walk_skips.len() as u64,
                    bytes: bytes_total,
                    elapsed: now.duration_since(scan_start),
                    terminal: false,
                });
                next_progress_tick = now + progress_interval;
            }
        }
        new_frames.clear();
        walk_one_directory(
            &frame,
            root_identity.0,
            cross_mounts,
            &mut bulk_reader,
            &mut bulk_children,
            visited_dirs,
            firmlink_overlays,
            &mut entries,
            &mut walk_skips,
            &mut new_frames,
        );
        // Append the new frames in reverse so the next pop is the
        // first child — preserves the original DFS visit order.
        for child in new_frames.drain(..).rev() {
            stack.push(child);
        }
    }

    if let Some(cb) = progress.as_mut() {
        for e in &entries[bytes_cursor..] {
            bytes_total = bytes_total.saturating_add(e.entry.logical_size);
        }
        cb(ProgressEvent {
            scanned: entries.len() as u64,
            skipped: walk_skips.len() as u64,
            bytes: bytes_total,
            elapsed: scan_start.elapsed(),
            terminal: true,
        });
    }

    (entries, walk_skips)
}

/// Worker-pool walk. Each of `threads` workers owns its own
/// `BulkReader` and pulls directories off a shared work queue. Per-
/// worker `entries` + `walk_skips` Vecs are merged on the main
/// thread after all workers join.
///
/// EX-25 validated 2.47× speedup at T=4 on a 14-logical-CPU Apple
/// silicon host; beyond T=4 the APFS container lock fires
/// (sys-CPU grows super-linearly with T). The CLI clamps T to
/// `min(hw.physicalcpu, 4)` by default.
///
/// Progress callbacks are invoked from a dedicated `progress`
/// thread spawned inside `std::thread::scope` so the borrowed
/// callback reference stays alive for the whole walk. The thread
/// samples per-walker `Arc<AtomicU64>` counters every 250 ms and
/// fires the callback on the same cadence as the serial path; a
/// terminal event fires once on exit.
#[allow(clippy::too_many_arguments)]
fn scan_parallel(
    resolved: &Path,
    root_identity: (u64, u64),
    cross_mounts: bool,
    threads: usize,
    progress: Option<&mut (dyn FnMut(ProgressEvent) + Send)>,
    visited_dirs: &Arc<VisitedDirs>,
    firmlink_overlays: &Arc<FirmlinkOverlaySet>,
    scan_start: Instant,
) -> (Vec<WalkEntry>, Vec<WalkSkip>) {
    let queue = Arc::new(WorkQueue::new(vec![WalkFrame {
        absolute: resolved.to_path_buf(),
        relative: PathBuf::new(),
        expected_identity: root_identity,
    }]));
    // Atomic counters drive the progress thread. Workers bump
    // them as they accumulate per-thread entries / skips; the
    // progress thread polls and forwards to the callback. The
    // counters mirror the per-thread Vec lengths at scan
    // completion, modulo the post-walk sort + merge (which
    // doesn't change the totals).
    let scanned_count = Arc::new(AtomicU64::new(0));
    let skipped_count = Arc::new(AtomicU64::new(0));
    let bytes_count = Arc::new(AtomicU64::new(0));
    let progress_done = Arc::new(AtomicBool::new(false));
    let progress = Mutex::new(progress);

    std::thread::scope(|scope| -> (Vec<WalkEntry>, Vec<WalkSkip>) {
        // This guard must live inside the scope body. During unwinding it
        // stops the progress loop before scope automatically joins threads.
        let _finish_progress = FinishProgressOnDrop(&progress_done);
        // Optional progress thread. We move the &mut callback
        // (Send-bounded) into the spawned closure so it lives for
        // the whole scope. The callback fires roughly every 250 ms,
        // matching the serial walker's cadence so consumers
        // (SwiftUI status bar, terminal stderr) see the same shape.
        let progress_handle = if progress.lock().unwrap().is_some() {
            let scanned = Arc::clone(&scanned_count);
            let skipped = Arc::clone(&skipped_count);
            let bytes = Arc::clone(&bytes_count);
            let done = Arc::clone(&progress_done);
            let callback = &progress;
            std::thread::Builder::new()
                .spawn_scoped(scope, move || {
                    let interval = Duration::from_millis(250);
                    // Poll cadence is short so the done-flag latency is
                    // bounded; the callback itself only fires on the
                    // 250 ms tick.
                    let poll = Duration::from_millis(50);
                    let mut next_tick = scan_start + interval;
                    loop {
                        if done.load(Ordering::Acquire) {
                            break;
                        }
                        let now = Instant::now();
                        if now >= next_tick {
                            callback.lock().unwrap().as_mut().unwrap()(ProgressEvent {
                                scanned: scanned.load(Ordering::Relaxed),
                                skipped: skipped.load(Ordering::Relaxed),
                                bytes: bytes.load(Ordering::Relaxed),
                                elapsed: now.duration_since(scan_start),
                                terminal: false,
                            });
                            next_tick = now + interval;
                        }
                        std::thread::sleep(poll);
                    }
                })
                .ok()
        } else {
            None
        };

        // Spawn N worker threads. Each owns its own BulkReader +
        // per-worker entries / skips Vecs, and bumps the shared
        // atomic counters as it processes a frame.
        let mut handles: Vec<std::thread::ScopedJoinHandle<'_, (Vec<WalkEntry>, Vec<WalkSkip>)>> =
            Vec::with_capacity(threads);
        for index in 0..threads {
            let q = Arc::clone(&queue);
            let scanned = Arc::clone(&scanned_count);
            let skipped = Arc::clone(&skipped_count);
            let bytes = Arc::clone(&bytes_count);
            let visited = Arc::clone(visited_dirs);
            let overlays = Arc::clone(firmlink_overlays);
            match spawn_worker(scope, index, move || {
                scan_worker(
                    &q,
                    root_identity.0,
                    cross_mounts,
                    &visited,
                    &overlays,
                    &scanned,
                    &skipped,
                    &bytes,
                )
            }) {
                Ok(handle) => handles.push(handle),
                Err(_) => break, // Started workers can drain the entire queue.
            }
        }

        // Join all worker threads first.
        let mut entries: Vec<WalkEntry> = Vec::new();
        let mut walk_skips: Vec<WalkSkip> = Vec::new();
        if handles.is_empty() {
            // No worker could start. The caller can perform the same work
            // without needing another thread or discarding the scan.
            (entries, walk_skips) = scan_worker(
                &queue,
                root_identity.0,
                cross_mounts,
                visited_dirs,
                firmlink_overlays,
                &scanned_count,
                &skipped_count,
                &bytes_count,
            );
        }
        for h in handles {
            let (e, s) = match h.join() {
                Ok(result) => result,
                Err(payload) => {
                    queue.cancel();
                    std::panic::resume_unwind(payload);
                }
            };
            entries.extend(e);
            walk_skips.extend(s);
        }
        // Signal the progress thread to fire its terminal event
        // and exit; then join it so its callback finishes before
        // `progress` (the &mut reference) is returned to the
        // caller.
        progress_done.store(true, Ordering::Release);
        if let Some(h) = progress_handle {
            if let Err(payload) = h.join() {
                std::panic::resume_unwind(payload);
            }
        }
        // This still fires if the optional progress thread could not start.
        if let Some(callback) = progress.lock().unwrap().as_mut() {
            callback(ProgressEvent {
                scanned: scanned_count.load(Ordering::Relaxed),
                skipped: skipped_count.load(Ordering::Relaxed),
                bytes: bytes_count.load(Ordering::Relaxed),
                elapsed: scan_start.elapsed(),
                terminal: true,
            });
        }
        (entries, walk_skips)
    })
}

struct FinishProgressOnDrop<'a>(&'a AtomicBool);

impl Drop for FinishProgressOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct CancelWorkOnPanic<'a>(&'a WorkQueue);

impl Drop for CancelWorkOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // Cancellation recovers poisoned state rather than panicking
            // again from a destructor while the original panic unwinds.
            self.0.cancel();
        }
    }
}

#[cfg(test)]
thread_local! {
    static WORKER_SPAWN_FAILURE_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static WORKER_PANIC_AFTER_POP: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

fn spawn_worker<'scope, 'env, F>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    _index: usize,
    worker: F,
) -> io::Result<std::thread::ScopedJoinHandle<'scope, (Vec<WalkEntry>, Vec<WalkSkip>)>>
where
    F: FnOnce() -> (Vec<WalkEntry>, Vec<WalkSkip>) + Send + 'scope,
{
    #[cfg(test)]
    if WORKER_SPAWN_FAILURE_AFTER.with(|limit| limit.get().is_some_and(|n| _index >= n)) {
        return Err(io::Error::other("injected worker spawn failure"));
    }
    std::thread::Builder::new().spawn_scoped(scope, worker)
}

#[allow(clippy::too_many_arguments)]
fn scan_worker(
    queue: &WorkQueue,
    root_dev: u64,
    cross_mounts: bool,
    visited: &VisitedDirs,
    overlays: &FirmlinkOverlaySet,
    scanned: &AtomicU64,
    skipped: &AtomicU64,
    bytes: &AtomicU64,
) -> (Vec<WalkEntry>, Vec<WalkSkip>) {
    let _cancel_on_panic = CancelWorkOnPanic(queue);
    let mut bulk_reader = BulkReader::new();
    let mut bulk_children = Vec::new();
    let mut entries = Vec::new();
    let mut skips = Vec::new();
    let mut new_frames = Vec::new();
    while let Some(frame) = queue.pop() {
        #[cfg(test)]
        queue.inject_worker_panic();
        let prev_entries = entries.len();
        let prev_skips = skips.len();
        new_frames.clear();
        walk_one_directory(
            &frame,
            root_dev,
            cross_mounts,
            &mut bulk_reader,
            &mut bulk_children,
            visited,
            overlays,
            &mut entries,
            &mut skips,
            &mut new_frames,
        );
        let added_entries = (entries.len() - prev_entries) as u64;
        let added_skips = (skips.len() - prev_skips) as u64;
        if added_entries > 0 {
            scanned.fetch_add(added_entries, Ordering::Relaxed);
        }
        if added_skips > 0 {
            skipped.fetch_add(added_skips, Ordering::Relaxed);
        }
        let added_bytes = entries[prev_entries..].iter().fold(0u64, |total, row| {
            total.saturating_add(row.entry.logical_size)
        });
        if added_bytes > 0 {
            // Rust 1.89 compatibility. Newer toolchains call this try_update.
            #[allow(deprecated)]
            let _ = bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                Some(total.saturating_add(added_bytes))
            });
        }
        queue.push_many(std::mem::take(&mut new_frames));
        queue.complete();
    }
    (entries, skips)
}

/// Shared depth-first work queue + outstanding-work counter for the parallel
/// walker. `pop` returns `None` only when the queue is empty AND every
/// previously-popped frame has been marked `complete()`, or when a panic
/// cancels the scan. Cancellation cannot produce a successful partial scan.
struct WorkQueue {
    state: Mutex<WorkQueueState>,
    cond: Condvar,
    #[cfg(test)]
    panic_after_pop: Option<bool>,
}

struct WorkQueueState {
    queue: Vec<WalkFrame>,
    /// Queued and currently-processing frames. A producer stays counted
    /// until it has published all children and calls complete.
    outstanding: u64,
    cancelled: bool,
    #[cfg(test)]
    waiters: usize,
}

impl WorkQueueState {
    fn cancel(&mut self) {
        self.cancelled = true;
        self.queue.clear();
        self.outstanding = 0;
    }
}

impl WorkQueue {
    fn new(initial: Vec<WalkFrame>) -> Self {
        let initial_len = initial.len() as u64;
        Self {
            state: Mutex::new(WorkQueueState {
                queue: initial,
                outstanding: initial_len,
                cancelled: false,
                #[cfg(test)]
                waiters: 0,
            }),
            cond: Condvar::new(),
            #[cfg(test)]
            panic_after_pop: WORKER_PANIC_AFTER_POP.with(|mode| mode.get()),
        }
    }

    fn push_many(&self, items: Vec<WalkFrame>) {
        if items.is_empty() {
            return;
        }
        let mut state = self.recover_lock(self.state.lock());
        if state.cancelled {
            return;
        }
        assert!(
            state.outstanding > 0,
            "cannot publish work after completion"
        );
        state.outstanding = state
            .outstanding
            .checked_add(items.len() as u64)
            .expect("work count overflow");
        state.queue.extend(items);
        drop(state);
        self.cond.notify_all();
    }

    fn pop(&self) -> Option<WalkFrame> {
        let mut state = self.recover_lock(self.state.lock());
        loop {
            if state.cancelled {
                return None;
            }
            if let Some(frame) = state.queue.pop() {
                return Some(frame);
            }
            if state.outstanding == 0 {
                return None;
            }
            // Predicate checking and wait use the same lock as publication
            // and completion, so notifications cannot fall between them.
            #[cfg(test)]
            {
                state.waiters += 1;
                self.cond.notify_all();
            }
            state = self.recover_lock(self.cond.wait(state));
            #[cfg(test)]
            {
                state.waiters -= 1;
            }
        }
    }

    fn complete(&self) {
        let mut state = self.recover_lock(self.state.lock());
        if state.cancelled {
            return;
        }
        state.outstanding = state
            .outstanding
            .checked_sub(1)
            .expect("completed a frame more than once");
        if state.outstanding == 0 {
            debug_assert!(state.queue.is_empty());
            drop(state);
            self.cond.notify_all();
        }
    }

    fn cancel(&self) {
        let mut state = self.recover_lock(self.state.lock());
        state.cancel();
        drop(state);
        self.cond.notify_all();
    }

    fn recover_lock<'a>(
        &self,
        result: std::sync::LockResult<std::sync::MutexGuard<'a, WorkQueueState>>,
    ) -> std::sync::MutexGuard<'a, WorkQueueState> {
        match result {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.cancel();
                self.cond.notify_all();
                state
            }
        }
    }

    #[cfg(test)]
    fn inject_worker_panic(&self) {
        let Some(poison_queue) = self.panic_after_pop else {
            return;
        };
        let mut state = self.recover_lock(self.state.lock());
        // Hold the first frame until another worker really is waiting.
        // The production build has no waiter counter or injection branch.
        while state.waiters == 0 {
            state = self.recover_lock(self.cond.wait(state));
        }
        if !poison_queue {
            drop(state);
        }
        std::panic::panic_any("original injected worker panic");
    }
}

/// Process one directory: enumerate its children via the bulk-or-fallback
/// `sorted_children` path, push `NamespaceEntry` rows for each child into
/// `out_entries`, record `WalkSkip` rows in `out_skips`, and push new
/// `WalkFrame`s for sub-directories into `out_new_frames`. The caller
/// decides where the new frames go (a local stack for serial, the shared
/// work queue for parallel) — that's the only difference between the two
/// schedulers.
///
/// Same EX-22 / SR-019 emission contract as the original inline loop.
/// All `String`/`PathBuf` allocations live on the same heap regardless
/// of thread; the per-worker `bulk_reader`/`bulk_children` scratch is
/// reused across calls to amortise the 64 KiB buffer.
#[allow(clippy::too_many_arguments)]
fn walk_one_directory(
    frame: &WalkFrame,
    root_dev: u64,
    cross_mounts: bool,
    bulk_reader: &mut BulkReader,
    bulk_children: &mut Vec<BulkEntry>,
    visited_dirs: &VisitedDirs,
    firmlink_overlays: &FirmlinkOverlaySet,
    out_entries: &mut Vec<WalkEntry>,
    out_skips: &mut Vec<WalkSkip>,
    out_new_frames: &mut Vec<WalkFrame>,
) {
    let (directory, children) = match sorted_children(frame, bulk_reader, bulk_children, out_skips)
    {
        Ok(result) => result,
        Err(skip) => {
            out_skips.push(skip);
            return;
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
                out_skips.push(WalkSkip {
                    path: absolute.to_string_lossy().into_owned(),
                    reason: "non_utf8_name".to_string(),
                });
                continue;
            }
        };

        // Bulk metadata for mountpoints and firmlinks describes the
        // underlying directory, not the mounted/firmlink target. Refresh
        // before making any mount or visited-inode decision.
        let meta = match child_metadata(&directory, &child.name, child.bulk.as_ref()) {
            Ok(meta) => meta,
            Err(err) => {
                out_skips.push(WalkSkip {
                    path: relative_str,
                    reason: io_skip_reason(&err),
                });
                continue;
            }
        };
        let BulkMeta {
            kind,
            file_id,
            logical_size: file_logical_size,
            allocated_bytes: file_allocated_bytes,
            dev_id,
            ..
        } = meta;

        let (logical_size, symlink_target) = match kind {
            EntryKind::Symlink => match directory.read_link(&child.name) {
                Ok(target) => {
                    // `Box<str>` (16B) instead of `String` (24B) —
                    // symlink_target is set once and never grown,
                    // so the spare capacity slot of `String` is
                    // dead weight in `NamespaceEntry`.
                    (target.len() as u64, Some(target))
                }
                Err(err) => {
                    out_skips.push(WalkSkip {
                        path: relative_str.clone(),
                        reason: io_skip_reason(&err),
                    });
                    (0, None)
                }
            },
            EntryKind::File => (file_logical_size, None),
            _ => (0, None),
        };

        let allocated_size = match kind {
            EntryKind::File => Some(file_allocated_bytes),
            EntryKind::Symlink | EntryKind::Dir => Some(0),
            EntryKind::Other => None,
        };

        let is_dir = matches!(kind, EntryKind::Dir);
        let cross_mount_skip = is_dir && !cross_mounts && dev_id != root_dev;

        // Firmlink-overlay skip: when the walker would otherwise
        // descend into a path that's listed in /usr/share/firmlinks as
        // a Data-volume source (e.g. /System/Volumes/Data/Users), drop
        // both emit and recurse. The same content is reachable via
        // the firmlink-canonical path (/Users) which is preferred
        // because it's what users see in Finder. The overlay set is
        // pre-built per scan, only non-empty for root=/ scans, and
        // empty otherwise — so narrower scans are unaffected.
        if is_dir && !cross_mount_skip && firmlink_overlays.contains(&absolute) {
            out_skips.push(WalkSkip {
                path: relative_str,
                reason: "firmlink_overlay_source".to_string(),
            });
            continue;
        }

        // Directory-inode dedup: when the walker would otherwise
        // descend, check whether we've already enumerated this exact
        // (dev_id, file_id) via another path. Catches anything the
        // firmlink-overlay skip misses (bind mounts, other overlay
        // mechanisms, future macOS changes that introduce new
        // namespace-merging) — belt-and-braces safety net.
        //
        // On dedup, drop both emit and recurse and record a
        // `duplicate_dir_inode` walk_skip so the user can see *where*
        // the elision happened.
        if is_dir && !cross_mount_skip && !visited_dirs.check_and_mark(dev_id, file_id) {
            out_skips.push(WalkSkip {
                path: relative_str,
                reason: "duplicate_dir_inode".to_string(),
            });
            continue;
        }

        if cross_mount_skip {
            out_skips.push(WalkSkip {
                path: relative_str.clone(),
                reason: "mount_boundary".to_string(),
            });
        }
        out_entries.push(WalkEntry {
            dev_id,
            entry: NamespaceEntry {
                // `into_boxed_str()` is a free conversion (no realloc)
                // when the String has zero spare capacity — and our
                // `relative_str` came from `s.to_string()` on a fresh
                // `&str`, so its capacity equals its length.
                path: relative_str.into_boxed_str(),
                entry_kind: kind,
                file_id,
                logical_size,
                symlink_target,
                allocated_size,
                // EX-27 honesty: the fallback walker has no refcount info
                // from POSIX (`stat` does not surface clone-share data on
                // macOS). We report `real_size == allocated_size` so the
                // shape parity with raw mode holds and the metric is
                // observably non-deduped on this backend.
                real_size: allocated_size,
            },
        });
        if is_dir && !cross_mount_skip {
            out_new_frames.push(WalkFrame {
                absolute,
                relative,
                expected_identity: (dev_id, file_id),
            });
        }
    }
}

struct WalkFrame {
    absolute: PathBuf,
    relative: PathBuf,
    expected_identity: (u64, u64),
}

struct ChildEntry {
    name: String,
    /// Metadata populated by the bulk-attribute path. `None` means the
    /// caller must `symlink_metadata` this child itself.
    bulk: Option<BulkMeta>,
}

#[derive(Clone, Copy)]
struct BulkMeta {
    kind: EntryKind,
    file_id: u64,
    logical_size: u64,
    allocated_bytes: u64,
    dev_id: u64,
    directory_needs_refresh: bool,
}

/// Only active workers retain descriptors. Queued frames carry identity
/// numbers, so a wide tree cannot exhaust FDs through queued parent handles.
struct PinnedDirectory {
    #[cfg(target_os = "macos")]
    file: fs::File,
    #[cfg(not(target_os = "macos"))]
    path: PathBuf,
}

impl PinnedDirectory {
    fn open(frame: &WalkFrame) -> Result<Self, WalkSkip> {
        let fail = |err: io::Error| WalkSkip {
            path: skip_path_for(&frame.relative),
            reason: io_skip_reason(&err),
        };
        #[cfg(target_os = "macos")]
        let (directory, meta) = {
            use std::os::unix::fs::OpenOptionsExt;
            // The root was canonicalized once. Descendant paths must not
            // follow a symlink introduced in any component during the walk.
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW_ANY | libc::O_CLOEXEC)
                .open(&frame.absolute)
                .map_err(fail)?;
            let meta = file.metadata().map_err(fail)?;
            (Self { file }, meta)
        };
        #[cfg(not(target_os = "macos"))]
        let (directory, meta) = {
            let meta = fs::symlink_metadata(&frame.absolute).map_err(fail)?;
            (
                Self {
                    path: frame.absolute.clone(),
                },
                meta,
            )
        };
        if !meta.is_dir() || (meta.dev(), meta.ino()) != frame.expected_identity {
            return Err(WalkSkip {
                path: skip_path_for(&frame.relative),
                reason: "directory_changed".to_string(),
            });
        }
        Ok(directory)
    }

    fn metadata(&self, name: &str) -> io::Result<BulkMeta> {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(name)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            // fstatat follows mounted/firmlink targets but never a symlink
            // entry. The parent descriptor pins the enumerated directory.
            let rc = unsafe {
                libc::fstatat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            let kind = entry_kind_from_mode(stat.st_mode as u32);
            let regular = matches!(kind, EntryKind::File);
            let logical_size = if regular {
                u64::try_from(stat.st_size)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative file size"))?
            } else {
                0
            };
            let allocated_bytes = if regular {
                u64::try_from(stat.st_blocks)
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "negative block count")
                    })?
                    .saturating_mul(512)
            } else {
                0
            };
            Ok(BulkMeta {
                kind,
                file_id: stat.st_ino,
                logical_size,
                allocated_bytes,
                dev_id: stat.st_dev as u64,
                directory_needs_refresh: false,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let meta = fs::symlink_metadata(self.path.join(name))?;
            let kind = entry_kind_from_meta(&meta);
            let regular = matches!(kind, EntryKind::File);
            Ok(BulkMeta {
                kind,
                file_id: meta.ino(),
                logical_size: if regular { meta.size() } else { 0 },
                allocated_bytes: if regular {
                    meta.blocks().saturating_mul(512)
                } else {
                    0
                },
                dev_id: meta.dev(),
                directory_needs_refresh: false,
            })
        }
    }

    fn read_link(&self, name: &str) -> io::Result<Box<str>> {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(name)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
            let mut bytes = vec![0u8; 256];
            loop {
                let count = unsafe {
                    libc::readlinkat(
                        self.file.as_raw_fd(),
                        name.as_ptr(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                };
                if count < 0 {
                    return Err(io::Error::last_os_error());
                }
                if (count as usize) < bytes.len() {
                    return Ok(String::from_utf8_lossy(&bytes[..count as usize])
                        .into_owned()
                        .into_boxed_str());
                }
                bytes.resize(
                    bytes
                        .len()
                        .checked_mul(2)
                        .ok_or_else(|| io::Error::other("symlink target length overflow"))?,
                    0,
                );
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(fs::read_link(self.path.join(name))?
                .to_string_lossy()
                .into_owned()
                .into_boxed_str())
        }
    }
}

fn child_metadata(
    directory: &PinnedDirectory,
    name: &str,
    bulk: Option<&BulkMeta>,
) -> io::Result<BulkMeta> {
    match bulk {
        Some(meta) if !matches!(meta.kind, EntryKind::Dir) || !meta.directory_needs_refresh => {
            Ok(*meta)
        }
        _ => directory.metadata(name),
    }
}

#[cfg(test)]
thread_local! {
    static FORCE_BULK_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn sorted_children(
    frame: &WalkFrame,
    bulk_reader: &mut BulkReader,
    bulk_children: &mut Vec<BulkEntry>,
    out_skips: &mut Vec<WalkSkip>,
) -> Result<(PinnedDirectory, Vec<ChildEntry>), WalkSkip> {
    let directory = PinnedDirectory::open(frame)?;
    // Try the macOS `getattrlistbulk` backend first. If it succeeds we
    // get name + kind + ino + size + dev_id per entry in one syscall
    // batch — no per-child `lstat` needed. On any failure we fall
    // through to `read_dir + symlink_metadata` so behavior is
    // preserved on non-macOS or when the kernel rejects bulk reads
    // for this directory.
    //
    // `bulk_children` is borrowed scratch from the caller; the
    // `BulkReader` reuses its 64 KiB kernel-fill buffer across calls.
    #[cfg(target_os = "macos")]
    let bulk_result = bulk_reader.read_directory_fd(&directory.file, bulk_children);
    #[cfg(not(target_os = "macos"))]
    let bulk_result = bulk_reader.read_directory(&frame.absolute, bulk_children);
    #[cfg(test)]
    let bulk_result = if FORCE_BULK_FAILURE.with(|enabled| enabled.get()) {
        // Simulate a later-batch failure after the descriptor has advanced
        // and the output contains a prefix that must not be committed.
        if let Some(mut ghost) = bulk_children.first().cloned() {
            ghost.name = "injected-ghost".to_string();
            bulk_children.push(ghost);
        }
        Err(io::Error::other("injected bulk failure"))
    } else {
        bulk_result
    };
    if bulk_result.is_ok() {
        let mut out: Vec<ChildEntry> = Vec::with_capacity(bulk_children.len());
        // `drain(..)` empties the scratch Vec but retains its capacity
        // for the next call; that's the load-bearing reuse here.
        out.extend(bulk_children.drain(..).map(child_from_bulk));
        sort_and_deduplicate_children(&mut out, &frame.relative, out_skips);
        return Ok((directory, out));
    }
    // Bulk read failed for this directory; the scratch Vec may hold
    // partial output from the kernel. Clear it before falling through
    // so the next directory starts clean.
    bulk_children.clear();
    // Fall through to the std read_dir path on bulk failure.

    #[cfg(target_os = "macos")]
    let read = StdDirReader::open(&directory.file);
    #[cfg(not(target_os = "macos"))]
    let read = fs::read_dir(&frame.absolute)
        .map(|entries| entries.map(|entry| entry.map(|e| e.file_name())));
    let read = read.map_err(|err| WalkSkip {
        path: skip_path_for(&frame.relative),
        reason: io_skip_reason(&err),
    })?;
    let mut out = collect_std_children(read, &frame.relative, out_skips);
    sort_and_deduplicate_children(&mut out, &frame.relative, out_skips);
    Ok((directory, out))
}

fn child_from_bulk(entry: BulkEntry) -> ChildEntry {
    let bulk = BulkMeta {
        kind: entry.kind,
        file_id: entry.file_id,
        logical_size: entry.logical_size,
        allocated_bytes: entry.allocated_bytes,
        dev_id: entry.dev_id,
        directory_needs_refresh: entry.directory_needs_refresh,
    };
    ChildEntry {
        name: entry.name,
        bulk: Some(bulk),
    }
}

fn sort_and_deduplicate_children(
    children: &mut Vec<ChildEntry>,
    relative: &Path,
    skips: &mut Vec<WalkSkip>,
) {
    // Stable sorting keeps the first observed metadata for a repeated name.
    // A live iterator can repeat names during renames; public paths must
    // still be unique. Only the exceptional duplicate path is copied here.
    children.sort_by(|a, b| a.name.cmp(&b.name));
    children.dedup_by(|later, earlier| {
        if later.name != earlier.name {
            return false;
        }
        skips.push(WalkSkip {
            path: relative.join(&later.name).to_string_lossy().into_owned(),
            reason: "directory_changed".to_string(),
        });
        true
    });
}

fn collect_std_children(
    entries: impl Iterator<Item = io::Result<std::ffi::OsString>>,
    relative: &Path,
    skips: &mut Vec<WalkSkip>,
) -> Vec<ChildEntry> {
    let mut out = Vec::new();
    for entry in entries {
        let raw_name = match entry {
            Ok(name) => name,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                skips.push(WalkSkip {
                    path: skip_path_for(relative),
                    reason: io_skip_reason(&err),
                });
                break; // Keep the valid prefix, but do not retry a failing stream indefinitely.
            }
        };
        let bytes = raw_name.as_encoded_bytes();
        if bytes.is_empty()
            || bytes.contains(&0)
            || bytes.contains(&b'/')
            || bytes == b"."
            || bytes == b".."
        {
            skips.push(WalkSkip {
                path: skip_path_for(relative),
                reason: io_skip_reason(&io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid directory component",
                )),
            });
            continue;
        }
        let Some(name) = raw_name.to_str() else {
            skips.push(WalkSkip {
                path: relative.join(&raw_name).to_string_lossy().into_owned(),
                reason: "non_utf8_name".to_string(),
            });
            continue;
        };
        out.push(ChildEntry {
            name: name.to_string(),
            bulk: None,
        });
    }
    out
}

#[cfg(target_os = "macos")]
struct StdDirReader {
    stream: *mut libc::DIR,
    finished: bool,
}

#[cfg(target_os = "macos")]
impl StdDirReader {
    fn open(pinned: &fs::File) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        // getattrlistbulk and readdir must not share an open-file
        // description. dup/try_clone would share that state too. Opening
        // "." relative to the pinned directory gives an independent offset.
        let fd = unsafe {
            libc::openat(
                pinned.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let err = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(err);
        }
        Ok(Self {
            stream,
            finished: false,
        })
    }
}

#[cfg(target_os = "macos")]
impl Iterator for StdDirReader {
    type Item = io::Result<std::ffi::OsString>;

    fn next(&mut self) -> Option<Self::Item> {
        use std::os::unix::ffi::OsStringExt;
        if self.finished {
            return None;
        }
        loop {
            // readdir returns NULL both at EOF and on error. Clear the
            // thread-local errno immediately before the call to distinguish them.
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(self.stream) };
            if entry.is_null() {
                let errno = unsafe { *libc::__error() };
                if errno == libc::EINTR {
                    continue;
                }
                self.finished = true;
                return if errno == 0 {
                    None
                } else {
                    Some(Err(io::Error::from_raw_os_error(errno)))
                };
            }
            // Copy the name before calling readdir again. Each reader belongs
            // to one worker, and the returned record remains valid until then.
            let record = unsafe { &*entry };
            let length = usize::from(record.d_namlen);
            if length == 0 || length >= record.d_name.len() || record.d_name[length] != 0 {
                self.finished = true;
                return Some(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid readdir name length",
                )));
            }
            let name =
                unsafe { std::slice::from_raw_parts(record.d_name.as_ptr().cast::<u8>(), length) };
            if name == b"." || name == b".." {
                continue;
            }
            return Some(Ok(std::ffi::OsString::from_vec(name.to_vec())));
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for StdDirReader {
    fn drop(&mut self) {
        // fdopendir transferred ownership of its descriptor to this DIR.
        unsafe {
            libc::closedir(self.stream);
        }
    }
}

fn io_skip_reason(err: &io::Error) -> String {
    // EPERM is not consistently classified as PermissionDenied on every
    // platform/Rust version, but it is an access-policy skip for this API.
    if matches!(err.raw_os_error(), Some(1 | 13)) {
        return "permission_denied".to_string();
    }
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

#[cfg(not(target_os = "macos"))]
fn entry_kind_from_meta(meta: &fs::Metadata) -> EntryKind {
    entry_kind_from_mode(meta.mode())
}

fn entry_kind_from_mode(mode: u32) -> EntryKind {
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
fn build_aggregates(entries: &[WalkEntry]) -> Vec<DirectoryAggregate> {
    use rustc_hash::FxBuildHasher;
    // Both maps swap to fxhash. The outer is probed once per
    // ancestor per file (~30M probes on a /-scale scan); the
    // inner is probed once per (file, ancestor) pair. SipHash is
    // ~3× slower than fxhash on short-string and u64 keys, and
    // the keys here are filesystem paths + inode numbers, both
    // non-adversarial.
    type OuterMap<'a> = HashMap<&'a str, InnerMap, FxBuildHasher>;
    type InnerMap = HashMap<(u64, u64), (u64, Option<u64>), FxBuildHasher>;

    let mut contributors: OuterMap = OuterMap::default();
    contributors.insert(".", InnerMap::default());
    for row in entries {
        let entry = &row.entry;
        if matches!(entry.entry_kind, EntryKind::Dir) {
            contributors.entry(&*entry.path).or_default();
        }
    }
    for row in entries {
        let entry = &row.entry;
        if !matches!(entry.entry_kind, EntryKind::File) {
            continue;
        }
        // Walk ancestors by repeatedly stripping the trailing `/<name>`
        // off the path slice. Every step is a single `rfind('/')` plus
        // a slice; no heap activity. We don't pre-build a Vec of
        // ancestors because the inner work is just a HashMap probe
        // that takes the `&str` directly.
        let mut current: &str = &entry.path;
        loop {
            match current.rfind('/') {
                Some(idx) => {
                    let parent = &current[..idx];
                    let key = if parent.is_empty() { "." } else { parent };
                    if let Some(map) = contributors.get_mut(key) {
                        map.entry((row.dev_id, entry.file_id))
                            .or_insert((entry.logical_size, entry.allocated_size));
                    }
                    if parent.is_empty() {
                        break;
                    }
                    current = parent;
                }
                None => {
                    if let Some(map) = contributors.get_mut(".") {
                        map.entry((row.dev_id, entry.file_id))
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
        let unique_inode_logical_total: u64 = file_sizes
            .values()
            .fold(0u64, |acc, (logical, _)| acc.saturating_add(*logical));
        let unique_inode_allocated_total: Option<u64> =
            file_sizes.values().try_fold(0u64, |acc, (_, allocated)| {
                allocated.map(|a| acc.saturating_add(a))
            });
        // This internal, serde-skipped field carries bare POSIX IDs for
        // historical callers. IDs can repeat across distinct devices.
        let mut contributing_file_ids: Vec<u64> = file_sizes.keys().map(|(_, ino)| *ino).collect();
        contributing_file_ids.sort_unstable();
        // EX-27: the fallback walker emits `real_size == allocated_size`
        // per entry (no refcount info from POSIX), so the per-directory
        // real total mirrors the allocated total. Same None-collapse
        // contract.
        let unique_inode_real_total: Option<u64> = unique_inode_allocated_total;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn frame(path: &Path) -> WalkFrame {
        let absolute = fs::canonicalize(path).unwrap();
        let meta = fs::symlink_metadata(&absolute).unwrap();
        WalkFrame {
            absolute,
            relative: PathBuf::new(),
            expected_identity: (meta.dev(), meta.ino()),
        }
    }

    fn sample_file(
        path: &str,
        file_id: u64,
        logical_size: u64,
        allocated_size: Option<u64>,
    ) -> WalkEntry {
        WalkEntry {
            dev_id: 1,
            entry: NamespaceEntry {
                path: path.into(),
                entry_kind: EntryKind::File,
                file_id,
                logical_size,
                symlink_target: None,
                allocated_size,
                real_size: allocated_size,
            },
        }
    }

    #[test]
    fn aggregate_totals_saturate_and_keep_none_collapse() {
        let entries = vec![
            sample_file("a", 1, i64::MAX as u64, Some(i64::MAX as u64)),
            sample_file("b", 2, i64::MAX as u64, Some(i64::MAX as u64)),
            sample_file("c", 3, 1024, Some(1024)),
        ];
        let total = &build_aggregates(&entries)[0];
        assert_eq!(total.unique_inode_logical_total, u64::MAX);
        assert_eq!(total.unique_inode_allocated_total, Some(u64::MAX));
        assert_eq!(total.unique_inode_real_total, Some(u64::MAX));
        let mut unknown = entries;
        unknown.push(sample_file("d", 4, 0, None));
        assert_eq!(
            build_aggregates(&unknown)[0].unique_inode_allocated_total,
            None
        );
    }

    #[test]
    fn aggregates_do_not_collapse_same_inode_on_different_devices() {
        let first = sample_file("a", 7, 100, Some(10));
        let mut second = sample_file("b", 7, 200, Some(20));
        second.dev_id = 2;
        let aggregates = build_aggregates(&[first, second]);
        assert_eq!(aggregates[0].unique_inode_logical_total, 300);
        assert_eq!(aggregates[0].unique_inode_allocated_total, Some(30));
        assert_eq!(aggregates[0].contributing_file_ids, [7, 7]);
    }

    #[test]
    fn same_device_hardlinks_keep_first_observed_sizes() {
        let aggregates = build_aggregates(&[
            sample_file("a", 7, 100, Some(10)),
            sample_file("b", 7, 200, Some(20)),
        ]);
        assert_eq!(aggregates[0].unique_inode_logical_total, 100);
        assert_eq!(aggregates[0].unique_inode_allocated_total, Some(10));
        assert_eq!(aggregates[0].contributing_file_ids, [7]);
    }

    #[test]
    fn walk_entry_conversion_moves_strings_without_cloning() {
        let rows: Vec<_> = (0..1000)
            .map(|i| sample_file(&format!("file-{i}"), i, i, Some(i)))
            .collect();
        let original_vec = rows.as_ptr().cast::<u8>();
        let original_paths: Vec<_> = rows.iter().map(|row| row.entry.path.as_ptr()).collect();
        let entries: Vec<NamespaceEntry> = rows.into_iter().map(|row| row.entry).collect();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.as_ptr())
                .collect::<Vec<_>>(),
            original_paths
        );
        eprintln!(
            "walk wrapper {} bytes, namespace {} bytes, Vec allocation reused {}",
            std::mem::size_of::<WalkEntry>(),
            std::mem::size_of::<NamespaceEntry>(),
            original_vec == entries.as_ptr().cast::<u8>()
        );
    }

    #[test]
    fn firmlink_overlay_uses_destination_and_rejects_escaping_paths() {
        let overlays = firmlink_overlays_from_contents(
            "visible\trelocated/target\nother\t../escape\nthird\t/absolute\ninvalid\n\tempty-source\nempty-destination\t\n");
        assert_eq!(
            overlays,
            HashSet::from([PathBuf::from("/System/Volumes/Data/relocated/target")])
        );
        assert!(read_firmlink_overlays(Path::new("/not-root")).is_empty());
    }

    #[test]
    fn std_invalid_name_does_not_discard_readable_siblings() {
        use std::os::unix::ffi::OsStringExt;
        let input = vec![
            Ok("before".into()),
            Ok(std::ffi::OsString::from_vec(b"bad-\xff".to_vec())),
            Ok("after".into()),
        ];
        let mut skips = Vec::new();
        let children = collect_std_children(input.into_iter(), Path::new("parent"), &mut skips);
        assert_eq!(
            children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["before", "after"]
        );
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].reason, "non_utf8_name");
        assert!(skips[0].path.starts_with("parent/"));
    }

    #[test]
    fn std_midstream_error_keeps_valid_prefix_and_records_actual_errno() {
        let input = vec![
            Ok("before".into()),
            Err(io::Error::from_raw_os_error(5)),
            Ok("after".into()),
        ];
        let mut skips = Vec::new();
        let children = collect_std_children(input.into_iter(), Path::new("parent"), &mut skips);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "before");
        assert_eq!(skips[0].path, "parent");
        assert_eq!(
            skips[0].reason,
            io_skip_reason(&io::Error::from_raw_os_error(5))
        );
        assert_eq!(
            io_skip_reason(&io::Error::from_raw_os_error(1)),
            "permission_denied"
        );
        assert_eq!(
            io_skip_reason(&io::Error::from_raw_os_error(13)),
            "permission_denied"
        );
        assert_eq!(
            io_skip_reason(&io::Error::from_raw_os_error(2)),
            "not_found"
        );
    }

    #[test]
    fn std_interrupted_iteration_retries_without_losing_siblings() {
        let input = vec![
            Ok("before".into()),
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "injected interruption",
            )),
            Ok("after".into()),
        ];
        let mut skips = Vec::new();
        let children = collect_std_children(input.into_iter(), Path::new("parent"), &mut skips);
        assert_eq!(
            children
                .iter()
                .map(|child| child.name.as_str())
                .collect::<Vec<_>>(),
            ["before", "after"]
        );
        assert!(skips.is_empty());
    }

    #[test]
    fn repeated_directory_names_keep_first_metadata_and_unique_paths() {
        let child = |name: &str, file_id, logical_size| ChildEntry {
            name: name.to_string(),
            bulk: Some(BulkMeta {
                kind: EntryKind::File,
                file_id,
                logical_size,
                allocated_bytes: logical_size,
                dev_id: 1,
                directory_needs_refresh: false,
            }),
        };
        let mut children = vec![
            child("z", 9, 9),
            child("a", 1, 100),
            child("a", 2, 200),
            child("b", 3, 300),
        ];
        let mut skips = Vec::new();
        sort_and_deduplicate_children(&mut children, Path::new("parent"), &mut skips);
        assert_eq!(
            children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "z"]
        );
        let first = children[0].bulk.as_ref().unwrap();
        assert_eq!((first.file_id, first.logical_size), (1, 100));
        assert_eq!(
            skips,
            [WalkSkip {
                path: "parent/a".to_string(),
                reason: "directory_changed".to_string()
            }]
        );
    }

    #[test]
    fn std_names_cannot_escape_the_pinned_parent() {
        let mut skips = Vec::new();
        let entries = ["", ".", "..", "../escape", "nul\0name", "valid"]
            .into_iter()
            .map(|name| Ok(name.into()));
        let children = collect_std_children(entries, Path::new("parent"), &mut skips);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "valid");
        assert_eq!(skips.len(), 5);
        assert!(skips
            .iter()
            .all(|skip| skip.path == "parent" && skip.reason.starts_with("read_error:")));
    }

    #[test]
    fn worker_counts_are_bounded_without_changing_normal_settings() {
        assert_eq!(normalize_fallback_threads(0), 1);
        for count in 1..=14 {
            assert_eq!(normalize_fallback_threads(count), count);
        }
        assert_eq!(normalize_fallback_threads(usize::MAX), MAX_FALLBACK_THREADS);
        assert_eq!(
            normalize_fallback_threads(MAX_FALLBACK_THREADS + 1),
            MAX_FALLBACK_THREADS
        );
    }

    #[test]
    fn worker_spawn_failures_retain_scan_results_and_terminal_progress() {
        let tmp = TempDir::new();
        fs::create_dir(tmp.path().join("dir")).unwrap();
        fs::write(tmp.path().join("dir/file"), b"payload").unwrap();
        let baseline = fallback_scan_path(tmp.path()).unwrap();
        struct ResetSpawnLimit;
        impl Drop for ResetSpawnLimit {
            fn drop(&mut self) {
                WORKER_SPAWN_FAILURE_AFTER.with(|limit| limit.set(None));
            }
        }
        let _reset = ResetSpawnLimit;
        for (threads, limit) in [(4, 0), (4, 1), (4, 2), (usize::MAX, 0)] {
            WORKER_SPAWN_FAILURE_AFTER.with(|value| value.set(Some(limit)));
            let mut events = Vec::new();
            let mut callback = |event| events.push(event);
            let output = fallback_scan_path_with_options(
                tmp.path(),
                FallbackOptions {
                    threads,
                    progress: Some(&mut callback),
                    ..FallbackOptions::default()
                },
            )
            .unwrap();
            assert_eq!(output.parser_output.entries, baseline.parser_output.entries);
            assert_eq!(
                output.parser_output.aggregates,
                baseline.parser_output.aggregates
            );
            assert_eq!(
                output.parser_output.walk_skips,
                baseline.parser_output.walk_skips
            );
            assert_eq!(events.iter().filter(|e| e.terminal).count(), 1);
            let terminal = events.last().unwrap();
            assert_eq!(
                terminal.scanned,
                baseline.parser_output.entries.len() as u64
            );
            assert_eq!(terminal.bytes, 7);
        }
    }

    #[test]
    fn worker_panic_wakes_waiters_stops_progress_and_preserves_the_original_panic() {
        for poison_queue in [false, true] {
            let tmp = TempDir::new();
            let path = tmp.path().to_path_buf();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            // An unscoped driver and a bounded receive make a regression
            // fail rather than hanging the entire unit-test process.
            let driver = std::thread::spawn(move || {
                WORKER_PANIC_AFTER_POP.with(|mode| mode.set(Some(poison_queue)));
                let mut events = Vec::new();
                let mut callback = |event| events.push(event);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    fallback_scan_path_with_options(
                        &path,
                        FallbackOptions {
                            threads: 2,
                            progress: Some(&mut callback),
                            ..FallbackOptions::default()
                        },
                    )
                }));
                WORKER_PANIC_AFTER_POP.with(|mode| mode.set(None));
                result_tx.send((result, events)).unwrap();
            });
            let (result, events) = result_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("worker panic left scoped threads blocked");
            let panic = result.expect_err("a worker panic cannot return partial success");
            assert_eq!(
                panic.downcast_ref::<&str>(),
                Some(&"original injected worker panic")
            );
            assert!(events.iter().all(|event| !event.terminal));
            driver.join().unwrap();
        }
    }

    #[test]
    fn work_queue_rechecks_publication_and_completion_under_one_lock() {
        let tmp = TempDir::new();
        let queue = Arc::new(WorkQueue::new(vec![frame(tmp.path())]));
        let producer = queue.pop().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let q = Arc::clone(&queue);
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                let child = q.pop().unwrap();
                q.complete();
                assert!(q.pop().is_none());
                result_tx.send(child.relative).unwrap();
            });
            started_rx.recv().unwrap();
            queue.push_many(vec![WalkFrame {
                relative: PathBuf::from("child"),
                ..producer
            }]);
            queue.complete();
            assert_eq!(
                result_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                PathBuf::from("child")
            );
        });
        assert!(queue.pop().is_none());
    }

    #[test]
    #[should_panic(expected = "completed a frame more than once")]
    fn work_queue_does_not_hide_completion_underflow() {
        WorkQueue::new(Vec::new()).complete();
    }

    #[test]
    fn queued_directory_replacement_is_not_enumerated() {
        let tmp = TempDir::new();
        let original = tmp.path().join("directory");
        fs::create_dir(&original).unwrap();
        let queued = frame(&original);
        fs::rename(&original, tmp.path().join("retired")).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("replacement-secret"), b"new").unwrap();
        let skip = PinnedDirectory::open(&queued).err().unwrap();
        assert_eq!(skip.reason, "directory_changed");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn queued_final_and_ancestor_symlink_replacements_are_not_followed() {
        let tmp = TempDir::new();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(outside.join("leaf")).unwrap();
        fs::write(outside.join("leaf/secret"), b"outside").unwrap();
        let original = tmp.path().join("directory");
        fs::create_dir_all(original.join("leaf")).unwrap();
        let final_frame = frame(&original);
        let ancestor_frame = frame(&original.join("leaf"));
        fs::rename(&original, tmp.path().join("retired")).unwrap();
        symlink(&outside, &original).unwrap();
        for queued in [final_frame, ancestor_frame] {
            assert!(PinnedDirectory::open(&queued).is_err());
            let mut entries = Vec::new();
            let mut skips = Vec::new();
            let mut frames = Vec::new();
            walk_one_directory(
                &queued,
                queued.expected_identity.0,
                true,
                &mut BulkReader::new(),
                &mut Vec::new(),
                &VisitedDirs::new(),
                &FirmlinkOverlaySet::new(),
                &mut entries,
                &mut skips,
                &mut frames,
            );
            assert!(entries.is_empty());
            assert!(frames.is_empty());
            assert_eq!(skips.len(), 1);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn metadata_refresh_uses_target_identity_and_never_follows_a_symlink_entry() {
        let tmp = TempDir::new();
        fs::create_dir(tmp.path().join("target")).unwrap();
        symlink("target", tmp.path().join("link")).unwrap();
        let directory = PinnedDirectory::open(&frame(tmp.path())).unwrap();
        let fake_underlying = BulkMeta {
            kind: EntryKind::Dir,
            file_id: 999,
            dev_id: 111,
            logical_size: 0,
            allocated_bytes: 0,
            directory_needs_refresh: true,
        };
        let refreshed = child_metadata(&directory, "target", Some(&fake_underlying)).unwrap();
        let actual = fs::symlink_metadata(tmp.path().join("target")).unwrap();
        assert_eq!(
            (refreshed.dev_id, refreshed.file_id),
            (actual.dev(), actual.ino())
        );
        assert_eq!(refreshed.kind, EntryKind::Dir);
        assert_eq!(
            child_metadata(&directory, "link", Some(&fake_underlying))
                .unwrap()
                .kind,
            EntryKind::Symlink
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pinned_metadata_and_readlink_survive_parent_path_replacement() {
        let tmp = TempDir::new();
        let original = tmp.path().join("original");
        let replacement = tmp.path().join("replacement");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(original.join("file"), b"old").unwrap();
        symlink("old-target", original.join("link")).unwrap();
        fs::write(replacement.join("file"), b"replacement").unwrap();
        symlink("new-target", replacement.join("link")).unwrap();
        let directory = PinnedDirectory::open(&frame(&original)).unwrap();
        fs::rename(&original, tmp.path().join("retired")).unwrap();
        symlink(&replacement, &original).unwrap();
        assert_eq!(directory.metadata("file").unwrap().logical_size, 3);
        assert_eq!(directory.read_link("link").unwrap().as_ref(), "old-target");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bulk_failure_discards_partial_output_and_restarts_std_at_zero() {
        let tmp = TempDir::new();
        fs::write(tmp.path().join("a"), b"a").unwrap();
        fs::write(tmp.path().join("b"), b"b").unwrap();
        struct ResetBulkFailure;
        impl Drop for ResetBulkFailure {
            fn drop(&mut self) {
                FORCE_BULK_FAILURE.with(|enabled| enabled.set(false));
            }
        }
        let _reset = ResetBulkFailure;
        FORCE_BULK_FAILURE.with(|enabled| enabled.set(true));
        let mut scratch = Vec::new();
        let mut skips = Vec::new();
        let (_, children) = sorted_children(
            &frame(tmp.path()),
            &mut BulkReader::new(),
            &mut scratch,
            &mut skips,
        )
        .unwrap();
        assert!(scratch.is_empty());
        assert!(skips.is_empty());
        assert_eq!(
            children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(children.iter().all(|child| child.bulk.is_none()));
    }

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
            .map(|e| e.path.as_ref())
            .collect();
        assert_eq!(paths, vec!["dst", "dst/link.txt", "dst/moved.txt", "src"]);

        let link = parser_output
            .entries
            .iter()
            .find(|e| &*e.path == "dst/link.txt")
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
            .find(|e| &*e.path == "dst/moved.txt")
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
            .find(|e| &*e.path == "dst")
            .expect("dst directory entry present");
        assert_eq!(dst_dir.entry_kind, EntryKind::Dir);
        assert_eq!(dst_dir.allocated_size, Some(0));

        let aggregates: Vec<&str> = parser_output
            .aggregates
            .iter()
            .map(|a| a.path.as_ref())
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

    /// EX-25: the parallel walker must produce byte-for-byte identical
    /// `NamespaceEntry` + `DirectoryAggregate` rows as the serial
    /// walker on any tree the serial walker can handle, regardless of
    /// thread count. The post-walk sort + the per-inode aggregate
    /// policy guarantee deterministic ordering even though the workers
    /// process directories in non-deterministic interleaving.
    #[test]
    fn parallel_walker_matches_serial_shape() {
        let tmp = TempDir::new();
        let root = tmp.path();
        // A small tree with multiple subdirectories so the work queue
        // actually has parallel-distributable work. 3 directories ×
        // 4 files each + 1 nested deeper = 13 entries, enough to
        // verify the merge.
        for top in ["alpha", "beta", "gamma"] {
            std::fs::create_dir_all(root.join(top)).unwrap();
            for name in ["one.txt", "two.bin", "three.md", "four.dat"] {
                let mut f = File::create(root.join(top).join(name)).unwrap();
                f.write_all(b"x").unwrap();
            }
        }
        std::fs::create_dir_all(root.join("alpha/nested")).unwrap();
        File::create(root.join("alpha/nested/deep.log")).unwrap();

        let serial = fallback_scan_path(root).expect("serial walks");
        let parallel = fallback_scan_path_with_options(
            root,
            FallbackOptions {
                cross_mounts: false,
                progress: None,
                threads: 4,
                skip_aggregates: false,
            },
        )
        .expect("parallel walks");

        let serial_paths: Vec<&str> = serial
            .parser_output
            .entries
            .iter()
            .map(|e| e.path.as_ref())
            .collect();
        let parallel_paths: Vec<&str> = parallel
            .parser_output
            .entries
            .iter()
            .map(|e| e.path.as_ref())
            .collect();
        assert_eq!(
            serial_paths, parallel_paths,
            "serial vs parallel produced different entry path sets"
        );
        assert_eq!(
            serial.parser_output.entries, parallel.parser_output.entries,
            "serial vs parallel produced different entries"
        );
        assert_eq!(
            serial.parser_output.aggregates, parallel.parser_output.aggregates,
            "serial vs parallel produced different aggregates"
        );
        assert_eq!(
            serial.parser_output.walk_skips, parallel.parser_output.walk_skips,
            "serial vs parallel produced different walk_skips"
        );
        // The correctness_claim should mention the thread count in
        // the parallel case so consumers (the SwiftUI app, the viz)
        // can tell which mode produced the scan.
        assert!(
            parallel.correctness_claim.contains("4-thread"),
            "parallel correctness_claim should mention 4-thread; got: {}",
            parallel.correctness_claim
        );
    }

    /// VisitedDirs is the load-bearing primitive for the firmlink /
    /// bind-mount dedup. The first observation of an inode wins and
    /// returns true; every subsequent observation returns false so the
    /// caller can record a `duplicate_dir_inode` walk_skip.
    #[test]
    fn visited_dirs_first_wins_subsequent_lose() {
        let v = VisitedDirs::new();
        assert!(v.check_and_mark(7, 100), "first insert should succeed");
        assert!(!v.check_and_mark(7, 100), "second insert should fail");
        assert!(v.check_and_mark(7, 101), "different inode should succeed");
        assert!(v.check_and_mark(8, 100), "different dev should succeed");
    }

    /// Empirical firmlink-dedup confidence on a synthetic tree. We
    /// can't create real firmlinks in a unit test (it's a kernel
    /// feature, not a userspace API), but a *bind*-equivalent setup
    /// via two scans of the same content lets us verify that the
    /// dedup primitive itself behaves correctly across scans.
    /// The realistic firmlink behaviour is exercised by the EX-26b
    /// /-scan smoke described in the commit message; this unit
    /// test only verifies that the absence of duplicates on a
    /// clean tree produces zero `duplicate_dir_inode` skips.
    #[test]
    fn fallback_emits_no_duplicate_dir_inode_skips_on_clean_tree() {
        let tmp = TempDir::new();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("alpha/nested")).unwrap();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        File::create(root.join("alpha/nested/x.txt")).unwrap();
        File::create(root.join("beta/y.txt")).unwrap();

        let output = fallback_scan_path(root).expect("scan ok");
        let dup_skips: Vec<&WalkSkip> = output
            .parser_output
            .walk_skips
            .iter()
            .filter(|s| s.reason == "duplicate_dir_inode")
            .collect();
        assert!(
            dup_skips.is_empty(),
            "clean tree produced unexpected duplicate_dir_inode skips: {:?}",
            dup_skips,
        );
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
            .map(|e| e.path.as_ref())
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
            .map(|e| e.path.as_ref())
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

    /// Atomic fixture creation avoids reusing a pre-existing directory or
    /// following a predictable test-path symlink.
    struct TempDir {
        directory: tempfile::TempDir,
    }

    impl TempDir {
        fn new() -> Self {
            Self {
                directory: tempfile::tempdir().expect("create temp dir"),
            }
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }
    }
}
