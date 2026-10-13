//! Cache regressions run ordinary CLI subprocesses with private fixtures
//! and APFS_FASTINDEX_CACHE_DIR overrides. They never use the user's cache.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    tree: PathBuf,
    cache: PathBuf,
    file: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let tree = temp.path().join("tree");
        let cache = temp.path().join("cache");
        fs::create_dir(&tree).unwrap();
        let file = tree.join("file.txt");
        fs::write(&file, b"hello").unwrap();
        Self {
            _temp: temp,
            tree,
            cache,
            file,
        }
    }

    fn scan(&self, options: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_apfs-fastindex-scan"))
            .args([
                "--mode",
                "fallback",
                "--threads",
                "1",
                "--cache",
                "--format",
                "msgpack",
            ])
            .args(options)
            .arg(&self.tree)
            .env("APFS_FASTINDEX_CACHE_DIR", &self.cache)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn cached_file(&self, suffix: &str) -> PathBuf {
        fs::read_dir(&self.cache)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with(suffix)
            })
            .unwrap()
    }
}

fn is_hit(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr).contains("cache: hit")
}

fn envelope(output: &Output) -> serde_json::Value {
    rmp_serde::from_slice(&output.stdout).expect("exactly one valid output envelope")
}

fn file_size(output: &Output) -> u64 {
    let envelope = envelope(output);
    let entries = envelope["parser_output"]["entries"].as_array().unwrap();
    entries
        .iter()
        .find(|entry| entry["path"] == "file.txt")
        .unwrap()["logical_size"]
        .as_u64()
        .unwrap()
}

#[test]
fn cache_hits_then_refreshes_after_resize_and_same_length_corruption() {
    let fixture = Fixture::new();
    let first = fixture.scan(&[]);
    assert!(!is_hit(&first));
    assert_eq!(file_size(&first), 5);
    let second = fixture.scan(&[]);
    assert!(is_hit(&second));
    assert_eq!(second.stdout, first.stdout);
    fs::OpenOptions::new()
        .write(true)
        .open(&fixture.file)
        .unwrap()
        .set_len(1234)
        .unwrap();
    let resized = fixture.scan(&[]);
    assert!(!is_hit(&resized));
    assert_eq!(file_size(&resized), 1234);
    let resized_hit = fixture.scan(&[]);
    assert!(is_hit(&resized_hit));
    assert_eq!(resized_hit.stdout, resized.stdout);

    let blob = fixture.cached_file(".scan.msgpack");
    let bytes = fs::read(&blob).unwrap();
    fs::write(&blob, vec![0xc1; bytes.len()]).unwrap();
    let refreshed = fixture.scan(&[]);
    assert!(!is_hit(&refreshed));
    // Byte equality also rules out corrupt cached bytes preceding the
    // fresh envelope on stdout.
    assert_eq!(refreshed.stdout, resized.stdout);
    let repaired_hit = fixture.scan(&[]);
    assert!(is_hit(&repaired_hit));
    assert_eq!(repaired_hit.stdout, resized.stdout);
}

#[test]
fn a_manifest_with_another_generations_checksum_is_refreshed() {
    let fixture = Fixture::new();
    let first = fixture.scan(&[]);
    let manifest_path = fixture.cached_file(".manifest.json");
    let old: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&fixture.file)
        .unwrap()
        .set_len(6)
        .unwrap();
    let current = fixture.scan(&[]);
    assert_eq!(first.stdout.len(), current.stdout.len());
    let mut mixed: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_ne!(mixed["scan_msgpack_checksum"], old["scan_msgpack_checksum"]);
    // Retain the current identity/signature, but bind it to the older
    // generation's equal-length bytes. Length checks alone cannot catch it.
    mixed["scan_msgpack_checksum"] = old["scan_msgpack_checksum"].clone();
    fs::write(&manifest_path, serde_json::to_vec(&mixed).unwrap()).unwrap();
    let refreshed = fixture.scan(&[]);
    assert!(!is_hit(&refreshed));
    assert_eq!(refreshed.stdout, current.stdout);
    assert!(is_hit(&fixture.scan(&[])));
}

fn directory_contents(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn incompatible_mount_slim_and_summary_options_do_not_reuse_or_write_cache() {
    for option in ["--cross-mounts", "--slim", "--summary"] {
        let fixture = Fixture::new();
        fixture.scan(&[]);
        let cached_before = directory_contents(&fixture.cache);
        let output = fixture.scan(&[option]);
        assert!(!is_hit(&output), "{option}");
        if option == "--summary" {
            assert!(String::from_utf8_lossy(&output.stdout).contains("mode: fallback"));
        } else {
            assert_eq!(envelope(&output)["mode"], "fallback");
        }
        assert_eq!(directory_contents(&fixture.cache), cached_before);
    }
}

#[cfg(unix)]
#[test]
fn partial_scans_are_retried_instead_of_cached() {
    use std::os::unix::fs::DirBuilderExt;

    if unsafe { libc::geteuid() } == 0 {
        eprintln!("partial-scan fixture skipped because root bypasses directory permissions");
        return;
    }

    struct UnreadableDirectory(PathBuf);
    impl Drop for UnreadableDirectory {
        fn drop(&mut self) {
            // The empty directory can be removed through its writable parent.
            // Its permissions never need to be changed.
            fs::remove_dir(&self.0).expect("remove private unreadable directory");
        }
    }

    let fixture = Fixture::new();
    let locked = fixture.tree.join("locked");
    fs::DirBuilder::new().mode(0o000).create(&locked).unwrap();
    let _cleanup = UnreadableDirectory(locked.clone());
    match fs::read_dir(&locked) {
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {}
        Ok(_) => {
            eprintln!(
                "partial-scan fixture skipped because this host bypasses directory permissions"
            );
            return;
        }
        Err(err) => panic!("unexpected fixture read failure: {err}"),
    }
    for _ in 0..2 {
        let output = fixture.scan(&[]);
        assert!(!is_hit(&output));
        assert!(envelope(&output)["parser_output"]["walk_skips"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skip| skip["path"] == "locked" && skip["reason"] == "permission_denied"));
        if fixture.cache.exists() {
            assert!(fs::read_dir(&fixture.cache).unwrap().next().is_none());
        }
    }
}
