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
        // Phase-2 smoke test: scan a small known directory on a
        // background queue so the launch path isn't blocked.
        // Logs the entry count + totals; if the FFI is wired
        // wrong this will null-out or NSLog a "FAILED" line.
        // Pull when the controller actually uses `Scan` for
        // real scans (phase 5).
        DispatchQueue.global(qos: .background).async {
            let probePath = "/usr/bin"
            if let scan = Scan.fallback(path: probePath, threads: 0, crossMounts: false) {
                NSLog(
                    "[native phase2 probe] %@: %llu entries, %llu logical bytes, allocated=%@",
                    probePath,
                    scan.entryCount,
                    scan.logicalTotal,
                    scan.allocatedTotal.map(String.init) ?? "unclaimed"
                )
            } else {
                NSLog("[native phase2 probe] \(probePath): FAILED")
            }
        }
        return true
    }
}

/// Owning Swift wrapper around an opaque `ApfsScan *` handle.
/// Construct via `Scan.fallback(path:threads:crossMounts:)`,
/// which calls into Rust to perform the scan. The Rust handle
/// is dropped in `deinit` via `apfs_scan_free`.
///
/// Phase 2 exposes the totals + provenance fields the existing
/// SwiftUI status bar reads. Phase 3 grows render-cells +
/// hit-test FFI on top of the same handle.
final class Scan {
    /// Sentinel matching `APFS_ALLOCATED_TOTAL_UNCLAIMED` on the
    /// Rust side. Stays in sync with `u64::MAX`; if the Rust
    /// constant changes, the Swift mapping changes here too.
    static let allocatedTotalUnclaimed: UInt64 = UInt64.max

    /// Performs a fallback (POSIX-traversal) scan of `path`.
    /// `threads` of 0 picks the default. Returns `nil` if the
    /// Rust side rejected the path (bad UTF-8, missing,
    /// permission denied at the root).
    static func fallback(path: String, threads: UInt32 = 0, crossMounts: Bool = false) -> Scan? {
        let handle = path.withCString { cPath in
            apfs_scan_directory(cPath, threads, crossMounts)
        }
        guard let handle else { return nil }
        return Scan(handle: handle)
    }

    private let handle: OpaquePointer

    private init(handle: OpaquePointer) {
        self.handle = handle
    }

    deinit {
        apfs_scan_free(handle)
    }

    /// Number of `NamespaceEntry` rows in the scan.
    var entryCount: UInt64 {
        apfs_scan_entry_count(handle)
    }

    /// Sum of `entry.logical_size` across the whole scan.
    var logicalTotal: UInt64 {
        apfs_scan_logical_total(handle)
    }

    /// Sum of `entry.allocated_size` across the whole scan, or
    /// `nil` when any row was SR-019 None-collapsed. The Rust
    /// side encodes `None` as `u64::MAX`; we map that back to
    /// Swift's nil here.
    var allocatedTotal: UInt64? {
        let raw = apfs_scan_allocated_total(handle)
        return raw == Scan.allocatedTotalUnclaimed ? nil : raw
    }

    /// `true` iff at least one entry has a concrete
    /// `allocated_size`. Gates the "Allocated" metric chip.
    var allocatedAvailable: Bool {
        apfs_scan_allocated_available(handle)
    }

    /// The scan's `correctness_claim` paragraph.
    var correctnessClaim: String {
        guard let ptr = apfs_scan_correctness_claim(handle) else { return "" }
        return String(cString: ptr)
    }

    /// `SourceDescriptor.source_kind` — e.g.
    /// `"mounted_directory"`, `"dmg_image"`, `"raw_device"`.
    var sourceKind: String {
        guard let ptr = apfs_scan_source_kind(handle) else { return "" }
        return String(cString: ptr)
    }

    /// `SourceDescriptor.requested_path` — the absolute path the
    /// caller asked us to scan.
    var sourceRequestedPath: String {
        guard let ptr = apfs_scan_source_requested_path(handle) else { return "" }
        return String(cString: ptr)
    }
}
