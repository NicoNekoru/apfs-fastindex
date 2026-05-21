//! Integration tests for the FFI diagnostic surface landed in
//! commit 1eacf54 (security: panic-hook log + `apfs_last_error`).
//!
//! Lives in `tests/` rather than `#[cfg(test)] mod tests` so each
//! test runs in its own process — the panic hook installs once
//! via `Once`, so process isolation is the only way to exercise
//! "fresh install + first call" semantics. The thread-local
//! `LAST_ERROR` likewise can drift between tests when they share
//! a process; the per-binary harness keeps them clean.

use std::ffi::CStr;
use std::os::raw::c_char;

use apfs_fastindex::ffi::{apfs_last_error, apfs_log_path, apfs_scan_directory_with_progress};

fn cstr_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `apfs_log_path` resolves to a non-empty path after the panic
/// hook installs (which happens lazily on the first FFI call).
/// The path itself lives under `~/Library/Logs/` on macOS; we
/// don't pin the exact value because $HOME varies by test host.
#[test]
fn apfs_log_path_resolves_after_install() {
    let path = cstr_to_string(apfs_log_path()).expect("log path should be non-null");
    assert!(
        path.ends_with("apfs-fastindex.log"),
        "log path should end with the canonical filename, got: {path}"
    );
}

/// A NULL `path` argument to `apfs_scan_directory_with_progress`
/// returns a NULL handle *and* populates the thread-local error.
/// Pre-fix, the Err branch returned NULL silently and Swift saw
/// the generic "scan failed" toast for every cause.
#[test]
fn null_path_populates_last_error() {
    // Prime: any leftover error from previous tests in this
    // binary should be drained first. (Each integration test
    // binary is its own process, so this is paranoia, but it's
    // free.)
    let _ = cstr_to_string(apfs_last_error());

    let scan = apfs_scan_directory_with_progress(
        std::ptr::null(),
        0,
        false,
        None,
        std::ptr::null_mut(),
    );
    assert!(scan.is_null(), "NULL path must fail-closed");

    let err = cstr_to_string(apfs_last_error())
        .expect("last_error should be set after NULL-path scan");
    assert!(
        err.contains("NULL") || err.contains("null"),
        "error message should mention the cause; got: {err}"
    );
}

/// `apfs_last_error` clears the slot when read. The second call
/// without an intervening failure returns NULL.
#[test]
fn last_error_clears_after_read() {
    // Trigger an error to populate the slot.
    let _ = apfs_scan_directory_with_progress(
        std::ptr::null(),
        0,
        false,
        None,
        std::ptr::null_mut(),
    );
    // First read drains it.
    let first = cstr_to_string(apfs_last_error());
    assert!(first.is_some(), "first read should return the message");

    // Second read on the same thread without another error
    // should return NULL.
    let second = apfs_last_error();
    assert!(
        second.is_null(),
        "last_error should clear after read; got non-null"
    );
}

/// In debug builds, `ffi_guard` catches panics via `catch_unwind`.
/// The panic hook records the payload in `LAST_ERROR` before the
/// unwind reaches us, so `apfs_last_error` surfaces the message.
///
/// In release builds, `panic = "abort"` makes `catch_unwind` a
/// no-op and the process dies — so this test is debug-only. We
/// can still exercise the path by triggering a `catch_unwind` in
/// the test itself, which the harness allows independently.
#[test]
fn synthetic_panic_in_catch_unwind_records_last_error() {
    // Prime the panic hook install. `apfs_last_error` deliberately
    // skips `ffi_guard` (no recursion into panic capture), so it
    // doesn't install the hook by itself. The other tests in this
    // binary do install it via their FFI calls, but `cargo test`
    // runs tests in parallel with non-deterministic order — this
    // test might run before any of them. `apfs_log_path` runs
    // through `ffi_guard` and installs the hook idempotently via
    // the `Once` inside `diag`. Cheap; the call itself just
    // returns a `*const c_char` we don't need.
    let _ = apfs_log_path();
    let _ = cstr_to_string(apfs_last_error()); // drain
    let _ = std::panic::catch_unwind(|| {
        std::panic::panic_any("synthetic panic for diag_ffi");
    });
    let err = cstr_to_string(apfs_last_error());

    // Release builds with `panic = "abort"` never reach this
    // assertion — the process aborts during `panic_any`. Debug
    // builds (the cargo test default) unwind, the hook fires,
    // and we end up here with `last_error` populated.
    #[cfg(debug_assertions)]
    {
        let msg = err.expect("last_error should carry the panic payload");
        assert!(
            msg.contains("synthetic panic for diag_ffi"),
            "panic message should round-trip; got: {msg}"
        );
    }
    #[cfg(not(debug_assertions))]
    {
        // If we got here in release mode, the panic was caught
        // (unlikely with panic=abort but harmless if so).
        let _ = err;
    }
}
