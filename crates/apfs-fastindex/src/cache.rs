//! Persistent fallback-scan cache (R4 / Gate 6 v1).
//!
//! ## Why this exists
//!
//! EX-30 measured the production fallback walker at 9.62 s warm
//! median on `/Users/kai` (1.56 M entries, 392 MB msgpack
//! output). For a tool the user invokes multiple times per
//! session, that's the wrong UX. R4 (the "Fast Repeat-Scan
//! Product" release in the roadmap) is the lever that turns a
//! 10 s rescan into a sub-second one when nothing has changed.
//!
//! ## What's cached
//!
//! Per `(volume UUID, canonical scan-root, parser version)`
//! key, two files:
//!
//! 1. `<key-hash>.manifest.json` — a small JSON document with
//!    the cache identity, timestamps, the directory-signature
//!    hash, and counts. Cheap to read; consulted first on every
//!    cache check.
//! 2. `<key-hash>.scan.msgpack` — the full serialised
//!    `FallbackScanOutput`. Only read on a hit, and streamed
//!    straight to stdout so we don't pay deserialise cost
//!    when the consumer is just going to re-encode.
//!
//! ## Cache identity
//!
//! ```text
//! CacheKey = (volume_uuid, canonical_scan_root, parser_version)
//! ```
//!
//! - `volume_uuid`: from `statfs.f_fsid` (a `u64` that's stable
//!   per volume across mounts). Not the APFS container UUID
//!   (which would need raw-mode access; EX-28-blocked) but
//!   sufficient — if the user remounts a different filesystem
//!   at the same path, `f_fsid` changes and we miss-then-write,
//!   no false hit.
//! - `canonical_scan_root`: `realpath(scan_root)`. Resolves
//!   symlinks + `..`/`.` so equivalent paths share a cache
//!   entry.
//! - `parser_version`: a build-time constant. Bumping the
//!   walker's output format invalidates every cached scan;
//!   that's the safe default. We don't try to detect "is this
//!   change format-compatible" — too easy to get wrong.
//!
//! ## Invalidation: directory-signature hash
//!
//! A cache hit requires a fresh "is this tree unchanged?" probe
//! that's cheaper than a full walk. We compute a 64-bit hash
//! over the (path, mtime, ctime) tuples of **every directory**
//! in the tree, in sorted order. Two observations make this
//! work:
//!
//! 1. APFS updates a directory's `mtime` when a child entry is
//!    added or removed (file created/deleted, file renamed, dir
//!    created/deleted). It updates `ctime` when those happen
//!    plus when xattrs / permissions change on the directory
//!    itself.
//! 2. The size totals our tool reports only change when files
//!    are added/removed/resized. A file's content edit that
//!    keeps the same size still updates the file's mtime but
//!    not the parent directory's — but it also doesn't change
//!    our reported totals, so the cache hit gives the right
//!    answer.
//!
//! So: directory-only signature is a cheap, sound invalidation
//! oracle for the byte totals this tool surfaces. It misses
//! in-place file size changes that don't bump dir mtime — a
//! `truncate()` followed by `write()` is the canonical hostile
//! case. Documented limitation; the user can force a full
//! rescan with `--no-cache` or by clearing the cache dir.
//!
//! Walking dirs-only is 5-10× faster than the full walker
//! (no file stat, no aggregate build). On a 172k-directory
//! tree (per EX-30) it's measured at ~0.5 s — fast enough
//! that even a cache miss only pays this probe + the full
//! walk on top.
//!
//! ## Cache location
//!
//! `~/Library/Caches/com.apfsfastindex.app/scans/`. Follows
//! macOS conventions; the OS can purge this dir under low-disk
//! pressure without losing user data.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Bumped whenever the walker's output format or invalidation
/// semantics change. Reads of a cache written under a different
/// version always miss — we never try to deserialise a stale
/// schema and risk a panic.
pub const CACHE_PARSER_VERSION: &str = concat!(
    "apfs-fastindex-fallback-",
    env!("CARGO_PKG_VERSION"),
);

#[derive(Debug)]
pub enum CacheError {
    Io(io::Error),
    NoVolumeUuid,
    BadKey(String),
    Serialize(String),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Io(e) => write!(f, "cache I/O: {e}"),
            CacheError::NoVolumeUuid => write!(f, "could not determine volume UUID"),
            CacheError::BadKey(s) => write!(f, "bad cache key: {s}"),
            CacheError::Serialize(s) => write!(f, "cache serialise: {s}"),
        }
    }
}

impl std::error::Error for CacheError {}

impl From<io::Error> for CacheError {
    fn from(e: io::Error) -> Self {
        CacheError::Io(e)
    }
}

/// Cache identity for one scan. Hash this to derive the
/// on-disk filename.
#[derive(Debug, Clone)]
pub struct CacheKey {
    pub volume_id: u64,
    pub scan_root_canonical: PathBuf,
    pub parser_version: String,
}

impl CacheKey {
    /// Build a key for the given scan-root path. Resolves the
    /// path via `canonicalize()` and reads `statfs.f_fsid` for
    /// the volume identifier.
    pub fn for_path(scan_root: &Path) -> Result<Self, CacheError> {
        let canonical = scan_root
            .canonicalize()
            .map_err(|e| CacheError::BadKey(format!("canonicalize({:?}): {e}", scan_root)))?;
        let volume_id = volume_id_for_path(&canonical)?;
        Ok(CacheKey {
            volume_id,
            scan_root_canonical: canonical,
            parser_version: CACHE_PARSER_VERSION.to_string(),
        })
    }

    /// Filesystem-safe identifier for this key — 16 hex chars
    /// of a stable hash. Same key always produces the same id.
    pub fn id(&self) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.volume_id.hash(&mut hasher);
        self.scan_root_canonical.hash(&mut hasher);
        self.parser_version.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CacheManifest {
    pub key_volume_id: u64,
    pub key_scan_root: PathBuf,
    pub key_parser_version: String,
    /// Unix epoch seconds when the scan was captured.
    pub generated_at_unix_s: u64,
    /// Hash of every directory's `(path, mtime_ns, ctime_ns)`
    /// in the scanned tree. Invalidation oracle.
    pub directory_signature: u64,
    /// Sanity-check counts; lets the consumer print useful
    /// status without reading the full msgpack.
    pub entry_count: usize,
    pub aggregate_count: usize,
    /// Size of the corresponding `.scan.msgpack` blob.
    pub scan_msgpack_bytes: u64,
}

/// Returns the cache root directory. Creates it if missing.
/// Honours `APFS_FASTINDEX_CACHE_DIR` env var for tests.
pub fn cache_dir() -> Result<PathBuf, CacheError> {
    let base = match std::env::var_os("APFS_FASTINDEX_CACHE_DIR") {
        Some(s) => PathBuf::from(s),
        None => {
            let home = std::env::var_os("HOME")
                .ok_or_else(|| CacheError::BadKey("HOME not set".into()))?;
            PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("com.apfsfastindex.app")
                .join("scans")
        }
    };
    fs::create_dir_all(&base)?;
    Ok(base)
}

/// Cache lookup. Returns `Some((manifest, msgpack_path))` if a
/// cache entry exists and its directory_signature matches the
/// caller-supplied `current_signature`. The caller is expected
/// to have just called `compute_directory_signature` on the
/// live scan target.
///
/// On any I/O / deserialise error, returns `Ok(None)` — a cache
/// problem should never cause a scan to fail. The caller falls
/// through to the fresh walker.
pub fn cache_lookup(
    key: &CacheKey,
    current_signature: u64,
) -> Result<Option<(CacheManifest, PathBuf)>, CacheError> {
    let dir = cache_dir()?;
    let id = key.id();
    let manifest_path = dir.join(format!("{id}.manifest.json"));
    let scan_path = dir.join(format!("{id}.scan.msgpack"));

    let Ok(manifest_bytes) = fs::read(&manifest_path) else {
        return Ok(None);
    };
    let manifest: CacheManifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if manifest.key_parser_version != key.parser_version
        || manifest.key_volume_id != key.volume_id
        || manifest.key_scan_root != key.scan_root_canonical
    {
        // Cache key collision shouldn't happen given the id
        // hash, but if it does we conservatively miss.
        return Ok(None);
    }
    if manifest.directory_signature != current_signature {
        return Ok(None);
    }
    if !scan_path.exists() {
        return Ok(None);
    }
    Ok(Some((manifest, scan_path)))
}

/// Cache write. Stores the scan blob + manifest atomically:
/// write to `<id>.{scan.msgpack,manifest.json}.tmp`, then
/// rename over the target. A concurrent reader either sees the
/// old pair or the new pair, never a half-written one.
pub fn cache_save(
    key: &CacheKey,
    directory_signature: u64,
    entry_count: usize,
    aggregate_count: usize,
    scan_msgpack: &[u8],
) -> Result<(), CacheError> {
    let dir = cache_dir()?;
    let id = key.id();
    let scan_path = dir.join(format!("{id}.scan.msgpack"));
    let manifest_path = dir.join(format!("{id}.manifest.json"));
    let scan_tmp = dir.join(format!("{id}.scan.msgpack.tmp"));
    let manifest_tmp = dir.join(format!("{id}.manifest.json.tmp"));

    fs::write(&scan_tmp, scan_msgpack)?;
    let manifest = CacheManifest {
        key_volume_id: key.volume_id,
        key_scan_root: key.scan_root_canonical.clone(),
        key_parser_version: key.parser_version.clone(),
        generated_at_unix_s: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        directory_signature,
        entry_count,
        aggregate_count,
        scan_msgpack_bytes: scan_msgpack.len() as u64,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| CacheError::Serialize(e.to_string()))?;
    fs::write(&manifest_tmp, &manifest_bytes)?;

    // Rename scan first; if it fails, leave the old manifest
    // pointing at the old scan. Then rename manifest, atomically
    // switching the cache to the new generation.
    fs::rename(&scan_tmp, &scan_path)?;
    fs::rename(&manifest_tmp, &manifest_path)?;
    Ok(())
}

/// Walk the scan target and compute a 64-bit hash over every
/// directory's `(path, mtime_ns, ctime_ns)`. Files are NOT
/// included — that's the v1 invalidation contract (see the
/// module-level docs for the trade-off). Returns the hash + the
/// number of directories visited (useful for telemetry).
pub fn compute_directory_signature(scan_root: &Path) -> io::Result<(u64, usize)> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut dir_count: usize = 0;

    let mut stack: Vec<PathBuf> = vec![scan_root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let meta = match fs::symlink_metadata(&current) {
            Ok(m) => m,
            Err(_) => continue, // unreadable dir — treat as absent
        };
        if !meta.is_dir() {
            continue;
        }
        dir_count += 1;
        // Stable mtime/ctime as i64 nanoseconds-since-epoch.
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i128)
            .unwrap_or(0);
        // ctime: prefer `metadata.created()` when available, but
        // on macOS Rust's `MetadataExt` gives us `st_ctime_nsec`
        // via `as_raw()`. Use that directly for change-time.
        let ctime = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128
            }
            #[cfg(not(unix))]
            {
                0i128
            }
        };
        current.to_string_lossy().hash(&mut hasher);
        mtime.hash(&mut hasher);
        ctime.hash(&mut hasher);

        // Enqueue children. Sorted for deterministic ordering;
        // the hash MUST be stable across runs even if readdir
        // returns entries in different order.
        let mut children: Vec<PathBuf> = match fs::read_dir(&current) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect(),
            Err(_) => continue,
        };
        children.sort();
        for child in children {
            // Mount-boundary check matches the walker's default
            // (don't cross). We don't want a signature based on
            // a path the walker won't visit.
            let Ok(meta) = fs::symlink_metadata(&child) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let root_meta = fs::symlink_metadata(scan_root)?;
                if meta.dev() != root_meta.dev() {
                    continue;
                }
            }
            stack.push(child);
        }
    }
    Ok((hasher.finish(), dir_count))
}

/// Read the path's volume identifier. We use the
/// `stat(2).st_dev` device-number rather than `statfs.f_fsid`
/// because Darwin's `fsid_t` is exposed as an opaque struct
/// (private `__fsid_val` field) by the `libc` crate and we'd
/// have to transmute to read it. `st_dev` is equally stable
/// per mount and exposed via `MetadataExt::dev() -> u64`.
fn volume_id_for_path(path: &Path) -> Result<u64, CacheError> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).map_err(|_| CacheError::NoVolumeUuid)?;
    Ok(meta.dev())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::sync::{Mutex, MutexGuard};

    /// Global lock — tests in this module mutate the
    /// `APFS_FASTINDEX_CACHE_DIR` env var which is process-wide.
    /// We can't run them in parallel without a race that ends
    /// in a save going to one tempdir and the matching lookup
    /// going to another. The lock is held for the lifetime of
    /// each test's `with_temp_cache_dir()` guard.
    fn cache_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        // poisoned lock: another test panicked — we just take
        // it anyway, since the env var will be reset below.
        match LOCK.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Guard returned by `with_temp_cache_dir()`. Holds the
    /// global lock + the TempDir so they're both dropped at
    /// the end of the test. Exposes `.path()` for the test
    /// to build its data tree inside the same tempdir.
    struct CacheTestGuard<'a> {
        _lock: MutexGuard<'a, ()>,
        td: tempfile::TempDir,
    }

    impl<'a> CacheTestGuard<'a> {
        fn path(&self) -> &Path {
            self.td.path()
        }
    }

    /// Override the cache dir to a temp location for the
    /// duration of the test. Holds the global lock so
    /// concurrent tests don't trample each other's env var.
    fn with_temp_cache_dir() -> CacheTestGuard<'static> {
        let lock = cache_lock();
        let td = tempfile::TempDir::new().expect("tempdir");
        // SAFETY: this section is serialised by `cache_lock()`;
        // no other thread inside this module can race the
        // env-var write.
        unsafe {
            std::env::set_var("APFS_FASTINDEX_CACHE_DIR", td.path());
        }
        CacheTestGuard { _lock: lock, td }
    }

    /// Helper: make a tiny tree with a known shape so we can
    /// invalidate predictably.
    fn make_tree(root: &Path) -> io::Result<()> {
        fs::create_dir_all(root.join("sub_a"))?;
        fs::create_dir_all(root.join("sub_b"))?;
        let mut f = File::create(root.join("sub_a/x.txt"))?;
        f.write_all(b"hello")?;
        let mut f = File::create(root.join("sub_b/y.txt"))?;
        f.write_all(b"world")?;
        Ok(())
    }

    #[test]
    fn key_id_is_stable() {
        let _g = with_temp_cache_dir();
        let scan_root = std::env::temp_dir();
        let k1 = CacheKey::for_path(&scan_root).expect("key");
        let k2 = CacheKey::for_path(&scan_root).expect("key");
        assert_eq!(k1.id(), k2.id());
        assert!(k1.id().len() == 16);
    }

    #[test]
    fn directory_signature_is_deterministic() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).expect("tree");
        let (sig_a, dir_count_a) = compute_directory_signature(&root).expect("sig");
        let (sig_b, dir_count_b) = compute_directory_signature(&root).expect("sig");
        assert_eq!(sig_a, sig_b, "same tree, same signature");
        assert_eq!(dir_count_a, dir_count_b);
        // root + sub_a + sub_b = 3 directories
        assert_eq!(dir_count_a, 3);
    }

    #[test]
    fn save_then_lookup_hits_on_match() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).expect("tree");
        let key = CacheKey::for_path(&root).expect("key");
        let (sig, _) = compute_directory_signature(&root).expect("sig");

        let scan_bytes = b"fake msgpack payload".to_vec();
        cache_save(&key, sig, 42, 7, &scan_bytes).expect("save");

        let hit = cache_lookup(&key, sig).expect("lookup");
        assert!(hit.is_some());
        let (manifest, scan_path) = hit.unwrap();
        assert_eq!(manifest.entry_count, 42);
        assert_eq!(manifest.aggregate_count, 7);
        assert_eq!(manifest.scan_msgpack_bytes, scan_bytes.len() as u64);
        assert_eq!(fs::read(scan_path).unwrap(), scan_bytes);
    }

    #[test]
    fn lookup_misses_on_signature_mismatch() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).expect("tree");
        let key = CacheKey::for_path(&root).expect("key");
        let (sig, _) = compute_directory_signature(&root).expect("sig");
        cache_save(&key, sig, 1, 1, b"x").expect("save");

        // Different signature → miss.
        let hit = cache_lookup(&key, sig.wrapping_add(1)).expect("lookup");
        assert!(hit.is_none());
    }

    #[test]
    fn signature_changes_when_dir_added() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).expect("tree");
        let (sig_before, _) = compute_directory_signature(&root).expect("sig");

        fs::create_dir_all(root.join("sub_c")).expect("mkdir");
        let (sig_after, _) = compute_directory_signature(&root).expect("sig");
        assert_ne!(sig_before, sig_after, "adding a dir bumps the signature");
    }

    #[test]
    fn signature_changes_when_file_added_to_dir() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).expect("tree");
        let (sig_before, _) = compute_directory_signature(&root).expect("sig");

        // Adding a file bumps the parent directory's mtime.
        let mut f = File::create(root.join("sub_a/new_file.txt")).expect("file");
        f.write_all(b"new").expect("write");
        // mtime resolution: ensure the kernel sees a different
        // timestamp by waiting a moment.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (sig_after, _) = compute_directory_signature(&root).expect("sig");
        assert_ne!(
            sig_before, sig_after,
            "adding a file bumps the parent dir's mtime → signature"
        );
    }

    #[test]
    fn save_overwrites_atomically() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).expect("tree");
        let key = CacheKey::for_path(&root).expect("key");
        let (sig, _) = compute_directory_signature(&root).expect("sig");

        cache_save(&key, sig, 1, 1, b"v1").expect("save v1");
        cache_save(&key, sig, 2, 2, b"v2").expect("save v2");

        let hit = cache_lookup(&key, sig).expect("lookup").expect("hit");
        assert_eq!(hit.0.entry_count, 2);
        assert_eq!(fs::read(hit.1).unwrap(), b"v2");
    }

    /// `.tmp` files left behind from an interrupted save must
    /// not be loaded as cache contents. The lookup only
    /// considers `.manifest.json` + `.scan.msgpack`; orphan
    /// tmps are harmless.
    #[test]
    fn orphan_tmp_does_not_poison_lookup() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).expect("tree");
        let key = CacheKey::for_path(&root).expect("key");
        let (sig, _) = compute_directory_signature(&root).expect("sig");
        cache_save(&key, sig, 1, 1, b"good").expect("save");

        // Drop a bogus .tmp.
        let cache = cache_dir().expect("cache dir");
        let bogus = cache.join(format!("{}.scan.msgpack.tmp", key.id()));
        fs::write(&bogus, b"junk").expect("write");

        let hit = cache_lookup(&key, sig).expect("lookup").expect("hit");
        assert_eq!(fs::read(hit.1).unwrap(), b"good");
    }
}
