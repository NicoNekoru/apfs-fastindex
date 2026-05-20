//! Diagnostic surfacing for the FFI boundary.
//!
//! The C ABI between the Rust crate and the SwiftUI app
//! (`crates/apfs-fastindex/src/ffi.rs`) is the only place where
//! "scan failed" can mean anything from "bad UTF-8 path" to "the
//! parser hit a malformed B-tree and aborted." Without a way to
//! attach a message to the failure, the user sees a generic
//! `"scan failed for /path"` toast and no path forward.
//!
//! Two complementary mechanisms live here:
//!
//! 1. **Panic-hook log.** A Rust `set_hook` callback installed
//!    once per process appends every panic — payload, file:line,
//!    timestamp — to `~/Library/Logs/apfs-fastindex.log`. The
//!    hook fires before the abort in release builds
//!    (`panic = "abort"`) and before the unwind reaches
//!    `catch_unwind` in debug builds, so the message lands on
//!    disk in both cases. macOS's bundled-app stderr redirect
//!    means the default Rust panic message normally goes
//!    nowhere; this file replaces that with something the user
//!    can find via Finder → ⌘⇧G → `~/Library/Logs`.
//!
//! 2. **Thread-local `last_error`.** Captures panic payloads (so
//!    debug-build `catch_unwind` returns can surface the
//!    message) and recoverable-error strings (set explicitly by
//!    the FFI entry points before they return a NULL/sentinel).
//!    Exposed to Swift via `apfs_last_error() -> *const c_char`.
//!
//! Both are best-effort: a panic inside the panic hook would
//! recurse infinitely, so I/O errors here are swallowed; a
//! thread-local set in worker code that never re-enters the
//! main-thread FFI would be invisible to Swift. The combination
//! is enough to turn "silent crash" into "useful diagnostic" for
//! the common cases the audit flagged.

use std::cell::RefCell;
use std::ffi::CString;
use std::io::Write;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Mutex, Once};

/// Resolved log-path holder. Filled by `install_panic_hook` the
/// first time it runs and read by `apfs_log_path` (FFI).
/// `Mutex<Option<CString>>` so the C pointer's lifetime is the
/// lifetime of the process — Swift can read it any time without
/// worrying about thread-locality.
static LOG_PATH: Mutex<Option<CString>> = Mutex::new(None);

thread_local! {
    /// Last error message recorded on this thread. Cleared on
    /// read via `apfs_last_error()`. Filled by:
    ///   - the panic hook (every panic)
    ///   - `set_last_error(...)` from the FFI's recoverable-error
    ///     branches (bad path, scan returned Err, etc.)
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Backing storage for the C-string handed to the FFI caller.
    /// Owned by the thread-local so we don't leak a CString per
    /// call; the contract is "the pointer is valid until the
    /// next `apfs_last_error()` call on this thread."
    static LAST_ERROR_BUF: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Idempotently install the panic-to-log hook. Safe to call from
/// every FFI entry point (uses `Once`); the second-and-later
/// callers do nothing.
pub fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let path = resolve_log_path();
        if let Ok(c) = CString::new(path.to_string_lossy().into_owned()) {
            if let Ok(mut slot) = LOG_PATH.lock() {
                *slot = Some(c);
            }
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Best-effort: ignore I/O errors so we never recurse
            // into another panic from inside the hook.
            let _ = append_panic_to_log(info);
            // Chain to whatever hook was in place (default: stderr).
            prev(info);
        }));
    });
}

fn resolve_log_path() -> PathBuf {
    // ~/Library/Logs/apfs-fastindex.log is the canonical macOS
    // user-visible log location. Falls back to CWD if $HOME is
    // unset (rare on macOS but defensive).
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join("Library/Logs");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("apfs-fastindex.log")
}

fn append_panic_to_log(info: &std::panic::PanicHookInfo<'_>) -> std::io::Result<()> {
    let path_owned = LOG_PATH.lock().ok().and_then(|slot| {
        slot.as_ref()
            .map(|c| PathBuf::from(c.to_string_lossy().into_owned()))
    });
    let Some(path) = path_owned else {
        return Ok(());
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| format!("{}.{:09}s", d.as_secs(), d.subsec_nanos()))
        .unwrap_or_else(|| "<no-clock>".to_string());
    let location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_else(|| "<unknown>".to_string());
    let payload = extract_panic_payload(info);
    writeln!(f, "[{}] PANIC at {}: {}", now, location, payload)?;
    // Mirror the payload into the thread-local so `catch_unwind`
    // callers (debug builds) can surface it via `apfs_last_error`.
    LAST_ERROR.with(|cell| *cell.borrow_mut() = Some(payload));
    Ok(())
}

fn extract_panic_payload(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Record a recoverable error string for the current thread. The
/// FFI entry points call this before returning a NULL/sentinel
/// so Swift can surface the cause via `apfs_last_error()`.
pub fn set_last_error(msg: impl Into<String>) {
    LAST_ERROR.with(|cell| *cell.borrow_mut() = Some(msg.into()));
}

/// FFI accessor — see `crates/apfs-fastindex/src/ffi.rs`. Returns
/// a `*const c_char` valid until the next call on the same thread,
/// or NULL if no error has been recorded since the last read.
pub fn last_error_cstr() -> *const c_char {
    LAST_ERROR.with(|err_cell| {
        let msg = err_cell.borrow_mut().take();
        match msg {
            Some(s) => {
                let cstring = CString::new(s).unwrap_or_else(|_| {
                    CString::new("<error message contained null byte>").unwrap()
                });
                LAST_ERROR_BUF.with(|buf| {
                    let mut slot = buf.borrow_mut();
                    *slot = Some(cstring);
                    slot.as_ref()
                        .map(|c| c.as_ptr())
                        .unwrap_or(ptr::null())
                })
            }
            None => ptr::null(),
        }
    })
}

/// FFI accessor for the log-file path. Returns NULL if the hook
/// hasn't been installed yet (shouldn't happen post-FFI-call
/// since every entry point installs it via `Once`).
pub fn log_path_cstr() -> *const c_char {
    LOG_PATH
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|c| c.as_ptr()))
        .unwrap_or(ptr::null())
}
