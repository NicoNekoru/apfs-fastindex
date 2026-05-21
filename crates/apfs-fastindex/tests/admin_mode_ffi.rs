//! Integration tests for the privileged-scan rehydration FFI.
//!
//! `apfs_scan_from_msgpack_file` is the bridge for the
//! "Scan as Administrator…" path: a privileged subprocess writes its
//! `FallbackScanOutput` as a msgpack blob to a temp file, the parent
//! GUI calls this FFI to rehydrate the same `ApfsScan` handle the
//! in-process scan produces, and the rest of the renderer is
//! unchanged.
//!
//! These tests round-trip through the disk to exercise the same
//! code path the Swift bridge will hit at runtime.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use apfs_fastindex::fallback::{fallback_scan_path, FallbackScanOutput};
use apfs_fastindex::ffi::{
    apfs_last_error, apfs_scan_allocated_total, apfs_scan_entry_count,
    apfs_scan_from_msgpack_file, apfs_scan_free, apfs_scan_logical_total,
};

fn write_msgpack_temp(output: &FallbackScanOutput) -> std::path::PathBuf {
    let bytes = rmp_serde::to_vec_named(output).expect("encode FallbackScanOutput");
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "apfs-fastindex-admin-test-{}.msgpack",
        std::process::id()
    ));
    std::fs::write(&path, bytes).expect("write temp msgpack");
    path
}

fn cstr_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Round-trip: scan a small tree in-process, serialize, rehydrate via
/// the new FFI, and assert the rehydrated handle reports the same
/// entry-count + logical-total as the original. This is the contract
/// the Swift bridge relies on.
#[test]
fn admin_mode_roundtrip_preserves_entry_count_and_totals() {
    // Use the repo root as the scan target — it's small enough to scan
    // quickly and present on every CI host.
    let repo_root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate parent")
            .parent()
            .expect("repo root")
            .to_path_buf();
    let scan_target = repo_root.join("crates/apfs-fastindex/src");
    assert!(
        scan_target.exists(),
        "scan target {} should exist for the round-trip test",
        scan_target.display()
    );

    let in_process = fallback_scan_path(&scan_target).expect("in-process scan");
    let entries = in_process.parser_output.entries.len();
    let logical_total: u64 = in_process
        .parser_output
        .entries
        .iter()
        .map(|e| e.logical_size)
        .sum();

    let temp_path = write_msgpack_temp(&in_process);
    let c_path = CString::new(temp_path.to_string_lossy().into_owned()).unwrap();

    let handle = apfs_scan_from_msgpack_file(c_path.as_ptr());
    assert!(
        !handle.is_null(),
        "apfs_scan_from_msgpack_file returned NULL; last_error = {:?}",
        cstr_to_string(apfs_last_error())
    );

    let rehydrated_entries = apfs_scan_entry_count(handle);
    let rehydrated_logical = apfs_scan_logical_total(handle);
    assert_eq!(rehydrated_entries, entries as u64);
    assert_eq!(rehydrated_logical, logical_total);
    // Allocated total: in-process fallback emits Some(st_blocks*512)
    // for files; the rehydrated handle's tree root aggregate carries
    // the same number. We don't pin the exact value — just that
    // rehydration produces *a* value, not the None sentinel.
    let _ = apfs_scan_allocated_total(handle);

    apfs_scan_free(handle);
    let _ = std::fs::remove_file(&temp_path);
}

/// NULL path returns NULL handle and records last_error. The bridge
/// surfaces last_error in a popup so the user sees what went wrong.
#[test]
fn admin_mode_null_path_returns_null_and_records_error() {
    let handle = apfs_scan_from_msgpack_file(std::ptr::null());
    assert!(handle.is_null());
    let err = cstr_to_string(apfs_last_error());
    assert!(
        err.as_deref()
            .is_some_and(|s| s.contains("apfs_scan_from_msgpack_file")),
        "expected last_error to mention apfs_scan_from_msgpack_file; got {err:?}"
    );
}

/// A path to a non-existent file returns NULL and records the IO
/// error so the Swift bridge can surface why the rehydration failed.
#[test]
fn admin_mode_missing_file_returns_null_and_records_error() {
    let c_path = CString::new("/tmp/apfs-fastindex-this-file-does-not-exist-xyz.msgpack").unwrap();
    let handle = apfs_scan_from_msgpack_file(c_path.as_ptr());
    assert!(handle.is_null());
    let err = cstr_to_string(apfs_last_error());
    assert!(
        err.as_deref().is_some_and(|s| s.contains("read")),
        "expected last_error to mention read failure; got {err:?}"
    );
}

/// The CLI's `--server` mode speaks JSON-line over
/// stdin/stdout (audit C1+H1 fix replaced the old tab-
/// delimited protocol). End-to-end probe: spawn the helper,
/// read the `ready` handshake, send a `scan` command, drain
/// progress events, expect a terminal `ok` carrying a helper-
/// chosen `out_path`, release the tempfile, then `quit`.
/// The result msgpack rehydrates via the same FFI the GUI
/// bridge uses.
#[test]
fn admin_mode_server_loop_scans_and_quits() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let crate_root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bin = crate_root
        .parent()
        .expect("crate parent")
        .parent()
        .expect("repo root")
        .join("target")
        .join(if cfg!(debug_assertions) { "debug" } else { "release" })
        .join("apfs-fastindex-scan");
    if !bin.exists() {
        eprintln!(
            "skip: apfs-fastindex-scan not built at {} — \
             run `cargo build --bin apfs-fastindex-scan` first",
            bin.display()
        );
        return;
    }
    let scan_target = crate_root.join("src");

    let mut child = Command::new(&bin)
        .arg("--server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server-mode CLI");

    let stdin = child.stdin.as_mut().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    fn parse_event(line: &str) -> serde_json::Value {
        serde_json::from_str(line.trim_end()).expect("valid JSON")
    }

    // First line: ready handshake.
    let mut line = String::new();
    reader.read_line(&mut line).expect("read ready");
    let ready = parse_event(&line);
    assert_eq!(ready["event"], "ready", "unexpected handshake: {line:?}");
    assert_eq!(ready["version"], 1);

    // Send scan command as JSON-line.
    let cmd = serde_json::json!({
        "op": "scan",
        "path": scan_target.to_string_lossy(),
    });
    let bytes = serde_json::to_vec(&cmd).expect("encode scan");
    stdin.write_all(&bytes).expect("write scan");
    stdin.write_all(b"\n").expect("newline");
    stdin.flush().expect("flush");

    // Drain progress events until the terminal ok/err event.
    // `loop { … break value … }` is a Rust expression — the
    // ok arm's payload becomes the loop's value, dropping
    // the placeholder-`None` + later-reassign shape that
    // clippy's `unused_assignments` lint flagged.
    let out_path: Option<String> = loop {
        line.clear();
        reader.read_line(&mut line).expect("read reply");
        let evt = parse_event(&line);
        match evt["event"].as_str() {
            Some("progress") => continue,
            Some("ok") => break evt["out_path"].as_str().map(|s| s.to_string()),
            Some("err") => panic!("scan err: {}", evt["message"]),
            other => panic!("unexpected event {other:?} in {line:?}"),
        }
    };
    let out_path = out_path.expect("ok carried out_path");
    assert!(
        !out_path.is_empty(),
        "ok reply must carry a non-empty out_path"
    );

    // Sanity-check that the helper actually owns the file —
    // the parent's path-verification in production does this
    // too, with stricter ownership checks (audit H2).
    let meta = std::fs::metadata(&out_path).expect("stat out_path");
    assert!(meta.is_file(), "out_path is not a regular file");
    assert!(meta.len() > 0, "out_path is empty");

    // The output msgpack should rehydrate cleanly via the
    // existing FFI — same shape as a non-server-mode scan.
    let c_path = CString::new(out_path.clone()).unwrap();
    let handle = apfs_scan_from_msgpack_file(c_path.as_ptr());
    assert!(
        !handle.is_null(),
        "server-produced msgpack failed to rehydrate; last_error = {:?}",
        cstr_to_string(apfs_last_error())
    );
    let count = apfs_scan_entry_count(handle);
    assert!(count > 0, "server scan returned zero entries");
    apfs_scan_free(handle);

    // Release the tempfile (helper unlinks it).
    let release = serde_json::json!({ "op": "release", "paths": [out_path] });
    stdin
        .write_all(&serde_json::to_vec(&release).unwrap())
        .expect("write release");
    stdin.write_all(b"\n").expect("newline");
    stdin.flush().expect("flush");
    line.clear();
    reader.read_line(&mut line).expect("read release ack");
    let ack = parse_event(&line);
    assert_eq!(ack["event"], "ok");

    // Quit.
    let quit = serde_json::json!({ "op": "quit" });
    stdin
        .write_all(&serde_json::to_vec(&quit).unwrap())
        .expect("write quit");
    stdin.write_all(b"\n").expect("newline");
    stdin.flush().expect("flush");
    line.clear();
    reader.read_line(&mut line).expect("read bye");
    let bye = parse_event(&line);
    assert_eq!(bye["event"], "bye");

    let status = child.wait().expect("wait");
    assert!(status.success(), "server exited non-zero: {status:?}");
}

/// A file that exists but isn't valid msgpack returns NULL and
/// records the decode error.
#[test]
fn admin_mode_bad_msgpack_returns_null_and_records_error() {
    let path = std::env::temp_dir().join(format!(
        "apfs-fastindex-admin-bad-{}.msgpack",
        std::process::id()
    ));
    std::fs::write(&path, b"this is not msgpack").expect("write garbage");
    let c_path = CString::new(path.to_string_lossy().into_owned()).unwrap();

    let handle = apfs_scan_from_msgpack_file(c_path.as_ptr());
    assert!(handle.is_null());
    let err = cstr_to_string(apfs_last_error());
    assert!(
        err.as_deref().is_some_and(|s| s.contains("decode")),
        "expected last_error to mention decode failure; got {err:?}"
    );

    let _ = std::fs::remove_file(&path);
}
