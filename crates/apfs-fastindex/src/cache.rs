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
//! Per `(mount device identity, canonical scan-root, parser version)`
//! key, two files:
//!
//! 1. `<key-hash>.manifest.json` — a small JSON document with
//!    the cache identity, timestamps, the directory-signature
//!    hash, and counts. Cheap to read; consulted first on every
//!    cache check.
//! 2. `<key-hash>.scan.msgpack` — the full serialised
//!    `FallbackScanOutput`. Its length and checksum must match
//!    the manifest before any cached bytes are emitted.
//!
//! ## Cache identity
//!
//! ```text
//! CacheKey = (volume_id, canonical_scan_root, parser_version)
//! ```
//!
//! - `volume_id`: the current mount's device identity from
//!   `MetadataExt::dev()`. This is not a persistent volume UUID
//!   and has no stability guarantee across remounts.
//! - `canonical_scan_root`: `realpath(scan_root)`. Resolves
//!   symlinks + `..`/`.` so equivalent paths share a cache
//!   entry.
//! - `parser_version`: a build-time constant. Bumping the
//!   walker's output format invalidates every cached scan;
//!   that's the safe default. We don't try to detect "is this
//!   change format-compatible" — too easy to get wrong.
//!
//! ## Invalidation
//!
//! Validation hashes every entry's path, identity, timestamps, sizes and
//! permissions. Directory timestamps alone miss ordinary file resizes,
//! allocation changes and file permission changes. Any metadata or directory
//! read failure prevents reuse. The CLI also declines to cache partial scans
//! or serve options that this default-mount-boundary probe cannot validate.
//!
//! This remains an opt-in, expensive metadata walk. It is not an atomic
//! snapshot and does not provide a fast incremental index. A journal-backed
//! cache needs separate invalidation and dropped-event handling.
//!
//! ## Cache location
//!
//! `~/Library/Caches/com.apfsfastindex.app/scans/`. Follows
//! macOS conventions; the OS can purge this dir under low-disk
//! pressure without losing user data.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
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
    "-metadata-v2",
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
    /// path via `canonicalize()` and reads `MetadataExt::dev()`
    /// for the current mount's device identity.
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
    /// Hash of all entry metadata in the scanned tree. The historical
    /// field name is retained for manifest compatibility.
    pub directory_signature: u64,
    /// Sanity-check counts; lets the consumer print useful
    /// status without reading the full msgpack.
    pub entry_count: usize,
    pub aggregate_count: usize,
    /// Size of the corresponding `.scan.msgpack` blob.
    pub scan_msgpack_bytes: u64,
    /// Noncryptographic checksum of the exact blob bytes. Missing on
    /// older manifests, which cannot be reused. This detects accidental
    /// corruption and mixed generations, not malicious cache forgery.
    #[serde(default)]
    pub scan_msgpack_checksum: Option<u64>,
}

impl CacheManifest {
    /// Verify the bytes the caller will consume, rather than trusting a
    /// path that another writer may have replaced since lookup.
    pub fn matches_scan_msgpack(&self, bytes: &[u8]) -> bool {
        self.scan_msgpack_bytes == bytes.len() as u64
            && self.scan_msgpack_checksum == Some(scan_msgpack_checksum(bytes))
    }
}

fn scan_msgpack_checksum(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
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
    if manifest.directory_signature != current_signature || manifest.scan_msgpack_checksum.is_none()
    {
        return Ok(None);
    }
    let Ok(scan_bytes) = fs::read(&scan_path) else {
        return Ok(None);
    };
    if !manifest.matches_scan_msgpack(&scan_bytes) {
        return Ok(None);
    }
    Ok(Some((manifest, scan_path)))
}

/// Cache write. Each file is staged through its own unique tempfile
/// descriptor and atomically replaced. The pair is not a filesystem
/// transaction. Readers reject mixed or interrupted generations by
/// checking the manifest's length and checksum against the blob.
pub fn cache_save(
    key: &CacheKey,
    directory_signature: u64,
    entry_count: usize,
    aggregate_count: usize,
    scan_msgpack: &[u8],
) -> Result<(), CacheError> {
    let pending = prepare_cache_save(
        key,
        directory_signature,
        entry_count,
        aggregate_count,
        scan_msgpack,
    )?;
    replace_cache_file(pending.scan_tmp, &pending.scan_path)?;
    replace_cache_file(pending.manifest_tmp, &pending.manifest_path)?;
    Ok(())
}

struct PendingCacheSave {
    scan_tmp: tempfile::NamedTempFile,
    manifest_tmp: tempfile::NamedTempFile,
    scan_path: PathBuf,
    manifest_path: PathBuf,
}

fn prepare_cache_save(
    key: &CacheKey,
    directory_signature: u64,
    entry_count: usize,
    aggregate_count: usize,
    scan_msgpack: &[u8],
) -> Result<PendingCacheSave, CacheError> {
    let dir = cache_dir()?;
    let id = key.id();
    let scan_path = dir.join(format!("{id}.scan.msgpack"));
    let manifest_path = dir.join(format!("{id}.manifest.json"));
    let mut scan_tmp = tempfile::Builder::new()
        .prefix(&format!("{id}.scan-"))
        .suffix(".tmp")
        .tempfile_in(&dir)?;
    scan_tmp.write_all(scan_msgpack)?;
    scan_tmp.flush()?;
    scan_tmp.as_file().sync_all()?;
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
        scan_msgpack_checksum: Some(scan_msgpack_checksum(scan_msgpack)),
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| CacheError::Serialize(e.to_string()))?;
    let mut manifest_tmp = tempfile::Builder::new()
        .prefix(&format!("{id}.manifest-"))
        .suffix(".tmp")
        .tempfile_in(&dir)?;
    manifest_tmp.write_all(&manifest_bytes)?;
    manifest_tmp.flush()?;
    manifest_tmp.as_file().sync_all()?;
    Ok(PendingCacheSave {
        scan_tmp,
        manifest_tmp,
        scan_path,
        manifest_path,
    })
}

fn replace_cache_file(temp: tempfile::NamedTempFile, target: &Path) -> Result<(), CacheError> {
    temp.persist(target)
        .map(|_| ())
        .map_err(|err| CacheError::Io(err.error))
}

/// Hash all entry metadata without following symlink entries or crossing
/// mount boundaries. The historical name is retained for callers.
/// Returns the hash and directory count. Any read error fails validation.
pub fn compute_directory_signature(scan_root: &Path) -> io::Result<(u64, usize)> {
    use std::os::unix::fs::MetadataExt;

    let root = fs::canonicalize(scan_root)?;
    let root_meta = fs::symlink_metadata(&root)?;
    if !root_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache root is not a directory",
        ));
    }
    let root_dev = root_meta.dev();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut dir_count: usize = 0;
    // The wire output preserves the caller's requested root spelling.
    scan_root.hash(&mut hasher);
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![(root, root_meta)];
    while let Some((current, meta)) = stack.pop() {
        current.hash(&mut hasher);
        meta.dev().hash(&mut hasher);
        meta.ino().hash(&mut hasher);
        meta.mode().hash(&mut hasher);
        meta.uid().hash(&mut hasher);
        meta.gid().hash(&mut hasher);
        meta.nlink().hash(&mut hasher);
        meta.size().hash(&mut hasher);
        meta.blocks().hash(&mut hasher);
        meta.mtime().hash(&mut hasher);
        meta.mtime_nsec().hash(&mut hasher);
        meta.ctime().hash(&mut hasher);
        meta.ctime_nsec().hash(&mut hasher);
        if !meta.is_dir() || meta.dev() != root_dev || !visited.insert((meta.dev(), meta.ino())) {
            continue;
        }
        dir_count += 1;
        let mut children = fs::read_dir(&current)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            let meta = fs::symlink_metadata(&child)?;
            stack.push((child, meta));
        }
    }
    Ok((hasher.finish(), dir_count))
}

/// Read the path's current mount device identity. We use the
/// `stat(2).st_dev` device-number rather than `statfs.f_fsid`
/// because Darwin's `fsid_t` is exposed as an opaque struct
/// (private `__fsid_val` field) by the `libc` crate and we'd
/// have to transmute to read it. `st_dev` is exposed via
/// `MetadataExt::dev() -> u64`, without a persistent UUID or
/// cross-remount stability guarantee.
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
        previous_cache_dir: Option<std::ffi::OsString>,
    }

    impl Drop for CacheTestGuard<'_> {
        fn drop(&mut self) {
            // Restore before releasing the module's environment lock.
            match &self.previous_cache_dir {
                Some(previous) => unsafe {
                    std::env::set_var("APFS_FASTINDEX_CACHE_DIR", previous)
                },
                None => unsafe { std::env::remove_var("APFS_FASTINDEX_CACHE_DIR") },
            }
        }
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
        let previous_cache_dir = std::env::var_os("APFS_FASTINDEX_CACHE_DIR");
        // SAFETY: this section is serialised by `cache_lock()`;
        // no other thread inside this module can race the
        // env-var write.
        unsafe {
            std::env::set_var("APFS_FASTINDEX_CACHE_DIR", td.path());
        }
        CacheTestGuard {
            _lock: lock,
            td,
            previous_cache_dir,
        }
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
    fn file_resize_invalidates_without_changing_directory_timestamps() {
        use std::os::unix::fs::MetadataExt;
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).unwrap();
        let parent = root.join("sub_a");
        let before = fs::metadata(&parent).unwrap();
        let (sig_before, _) = compute_directory_signature(&root).unwrap();
        File::options()
            .write(true)
            .open(parent.join("x.txt"))
            .unwrap()
            .set_len(1234)
            .unwrap();
        let after = fs::metadata(&parent).unwrap();
        assert_eq!(
            (
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            )
        );
        let (sig_after, _) = compute_directory_signature(&root).unwrap();
        assert_ne!(sig_before, sig_after);
    }

    #[test]
    fn file_permissions_invalidate_and_root_aliases_are_not_empty_signatures() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).unwrap();
        let alias = g.path().join("alias");
        symlink(&root, &alias).unwrap();
        let (before, dirs) = compute_directory_signature(&alias).unwrap();
        assert_eq!(dirs, 3);
        fs::set_permissions(root.join("sub_a/x.txt"), fs::Permissions::from_mode(0o600)).unwrap();
        let (after, _) = compute_directory_signature(&alias).unwrap();
        assert_ne!(before, after);
        assert_ne!(after, compute_directory_signature(&root).unwrap().0);
        assert!(compute_directory_signature(&root.join("missing")).is_err());
        assert!(compute_directory_signature(&root.join("sub_a/x.txt")).is_err());
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

    #[test]
    fn corruption_or_missing_blob_checksum_cannot_be_reused() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).unwrap();
        let key = CacheKey::for_path(&root).unwrap();
        let (sig, _) = compute_directory_signature(&root).unwrap();
        let scan_path = cache_dir()
            .unwrap()
            .join(format!("{}.scan.msgpack", key.id()));
        let manifest_path = cache_dir()
            .unwrap()
            .join(format!("{}.manifest.json", key.id()));
        for corrupted in [&b"evil"[..], &b"truncated-or-extended"[..], &b""[..]] {
            cache_save(&key, sig, 1, 1, b"good").unwrap();
            fs::write(&scan_path, corrupted).unwrap();
            assert!(cache_lookup(&key, sig).unwrap().is_none());
        }
        cache_save(&key, sig, 1, 1, b"good").unwrap();
        let mut old_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        old_manifest
            .as_object_mut()
            .unwrap()
            .remove("scan_msgpack_checksum");
        fs::write(&manifest_path, serde_json::to_vec(&old_manifest).unwrap()).unwrap();
        assert!(cache_lookup(&key, sig).unwrap().is_none());
    }

    #[test]
    fn mixed_or_interrupted_generations_are_misses_and_can_be_repaired() {
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).unwrap();
        let key = CacheKey::for_path(&root).unwrap();
        let (sig, _) = compute_directory_signature(&root).unwrap();
        cache_save(&key, sig, 1, 1, b"aaaa").unwrap();
        let pending = prepare_cache_save(&key, sig, 2, 2, b"bbbb").unwrap();
        // Simulate interruption after the blob rename, before manifest
        // publication. Equal lengths require the checksum to detect it.
        replace_cache_file(pending.scan_tmp, &pending.scan_path).unwrap();
        assert!(cache_lookup(&key, sig).unwrap().is_none());
        replace_cache_file(pending.manifest_tmp, &pending.manifest_path).unwrap();
        let (manifest_b, _) = cache_lookup(&key, sig).unwrap().unwrap();
        let writer_a = prepare_cache_save(&key, sig, 1, 1, b"aaaa").unwrap();
        let writer_b = prepare_cache_save(&key, sig, 2, 2, b"bbbb").unwrap();
        assert_ne!(writer_a.scan_tmp.path(), writer_b.scan_tmp.path());
        assert_ne!(writer_a.manifest_tmp.path(), writer_b.manifest_tmp.path());
        replace_cache_file(writer_a.scan_tmp, &writer_a.scan_path).unwrap();
        replace_cache_file(writer_b.scan_tmp, &writer_b.scan_path).unwrap();
        replace_cache_file(writer_b.manifest_tmp, &writer_b.manifest_path).unwrap();
        replace_cache_file(writer_a.manifest_tmp, &writer_a.manifest_path).unwrap();
        assert!(cache_lookup(&key, sig).unwrap().is_none());
        // A reader holding the older manifest also rejects replaced
        // bytes. The CLI repeats this check on the bytes it will emit.
        assert!(!manifest_b.matches_scan_msgpack(b"aaaa"));
        cache_save(&key, sig, 3, 3, b"cccc").unwrap();
        let (manifest, path) = cache_lookup(&key, sig).unwrap().unwrap();
        assert_eq!(manifest.entry_count, 3);
        assert!(manifest.matches_scan_msgpack(&fs::read(path).unwrap()));
    }

    #[test]
    fn concurrent_saves_use_independent_staging_and_never_validate_mixed_bytes() {
        use std::sync::{Arc, Barrier};
        let g = with_temp_cache_dir();
        let root = g.path().join("tree");
        make_tree(&root).unwrap();
        let key = CacheKey::for_path(&root).unwrap();
        let (sig, _) = compute_directory_signature(&root).unwrap();
        let barrier = Arc::new(Barrier::new(4));
        let paths = std::thread::scope(|scope| {
            let handles: Vec<_> = (1u8..=4)
                .map(|generation| {
                    let barrier = Arc::clone(&barrier);
                    let key = &key;
                    scope.spawn(move || {
                        let pending = prepare_cache_save(
                            key,
                            sig,
                            generation as usize,
                            1,
                            &[generation; 512],
                        )
                        .unwrap();
                        let paths = (
                            pending.scan_tmp.path().to_owned(),
                            pending.manifest_tmp.path().to_owned(),
                        );
                        barrier.wait();
                        replace_cache_file(pending.scan_tmp, &pending.scan_path).unwrap();
                        // All blobs are published before any manifests,
                        // forcing real publication overlap deterministically.
                        barrier.wait();
                        replace_cache_file(pending.manifest_tmp, &pending.manifest_path).unwrap();
                        paths
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        let unique: std::collections::HashSet<_> =
            paths.into_iter().flat_map(|(a, b)| [a, b]).collect();
        assert_eq!(unique.len(), 8);
        if let Some((manifest, path)) = cache_lookup(&key, sig).unwrap() {
            assert_eq!(
                fs::read(&path).unwrap(),
                vec![manifest.entry_count as u8; 512]
            );
        }
        // Whether the concurrent final pair hit or conservatively missed,
        // an ordinary subsequent save restores a coherent generation.
        cache_save(&key, sig, 9, 1, b"repaired").unwrap();
        let (manifest, path) = cache_lookup(&key, sig).unwrap().unwrap();
        assert_eq!(manifest.entry_count, 9);
        assert_eq!(fs::read(path).unwrap(), b"repaired");
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
