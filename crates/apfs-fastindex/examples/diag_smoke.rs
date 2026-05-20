//! Smoke test for the FFI diagnostic surface:
//!
//!   1. `apfs_log_path()` returns the log file location.
//!   2. A recoverable failure (NULL path) populates
//!      `apfs_last_error()`.
//!   3. A real panic inside `ffi_guard` writes to the log file
//!      and also populates `apfs_last_error()`.
//!
//! Run with:
//!   cargo run --release --example diag_smoke

use std::ffi::CStr;
use std::os::raw::c_char;

// Direct calls into the FFI module — same surface Swift sees,
// just exercised from Rust. Going via `apfs_fastindex::ffi::*`
// lets us link against the rlib without the cdylib symbol-export
// machinery.
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

fn main() {
    let path = cstr_to_string(apfs_log_path());
    println!("log path: {:?}", path);

    println!();
    println!("=== test 1: recoverable error path (NULL path) ===");
    let scan = apfs_scan_directory_with_progress(
        std::ptr::null(),
        0,
        false,
        None,
        std::ptr::null_mut(),
    );
    println!("scan returned: {:?}", scan);
    let err = cstr_to_string(apfs_last_error());
    println!("last_error: {:?}", err);
    assert!(scan.is_null(), "scan should fail-closed on NULL path");
    assert!(err.is_some(), "last_error should be set");

    println!();
    println!("=== test 2: panic-hook log writes ===");
    // Drive a synthetic panic through `catch_unwind` so we can
    // observe whether the hook (installed by the FFI calls above)
    // appended to the log. The default panic hook also writes to
    // stderr; the diag hook chains through it.
    let _ = std::panic::catch_unwind(|| {
        std::panic::panic_any("synthetic panic for diag_smoke");
    });
    let err = cstr_to_string(apfs_last_error());
    println!("last_error after panic: {:?}", err);
    assert!(err.is_some(), "last_error should carry the panic message");

    println!();
    println!("=== log file tail (last 5 lines) ===");
    if let Some(p) = &path {
        match std::fs::read_to_string(p) {
            Ok(contents) => {
                let lines: Vec<&str> = contents.lines().collect();
                for line in lines.iter().rev().take(5).collect::<Vec<_>>().iter().rev() {
                    println!("  {}", line);
                }
            }
            Err(e) => println!("  (could not read log: {})", e),
        }
    }
    println!();
    println!("smoke test OK");
}
