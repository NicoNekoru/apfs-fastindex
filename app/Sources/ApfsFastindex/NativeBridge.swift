import Foundation
import CApfsFastindex

/// Swift wrapper around the apfs-fastindex Rust crate's C ABI.
///
/// Every call from Swift into Rust crosses this struct. As the
/// native rewrite advances it grows methods for `scan(path:)`,
/// `cells(node:depth:metric:dims:)`, `hitTest(point:)`, and so
/// on. For phase 1 it only knows how to read the FFI
/// sanity-check constants — the bridge being importable and the
/// link going through at all is what we validate here.
///
/// Owning lifetime: any pointer Rust hands back to Swift is
/// owned by Rust unless the function's docs explicitly say
/// otherwise. Swift wrappers (the future `Scan` class etc.)
/// hold on to opaque handles and call `_free` in `deinit`.
enum NativeBridge {
    /// Trivia value Rust hands back. Returns 42; mismatched
    /// values mean the link or name mangling is wrong (e.g.
    /// the symbol `_apfs_hello` didn't resolve to our static
    /// lib at link time). Used by `validate()` below.
    static var hello: Int32 {
        apfs_hello()
    }

    /// The Rust crate's `CARGO_PKG_VERSION` string. The
    /// pointer is to static storage; Swift must not free it.
    static var version: String {
        guard let cstr = apfs_version() else { return "(unknown)" }
        return String(cString: cstr)
    }

    /// Verifies the FFI bridge is loaded + reachable. Called at
    /// app launch; logs the version and a one-line FAIL trace
    /// if the sanity check trips. Returns true on success so
    /// the caller can bail (or surface a diagnostic UI) if the
    /// Rust crate didn't link.
    @discardableResult
    static func validate() -> Bool {
        let h = hello
        let v = version
        if h != 42 {
            NSLog("NativeBridge.validate: apfs_hello returned \(h), expected 42 — FFI is broken")
            return false
        }
        NSLog("NativeBridge.validate: apfs-fastindex v\(v) linked")
        return true
    }
}
