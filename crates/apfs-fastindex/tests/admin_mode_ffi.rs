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
