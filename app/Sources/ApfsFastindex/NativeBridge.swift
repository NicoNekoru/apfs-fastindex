import Foundation
import CApfsFastindex
import os.log

/// Unified-logging handle used by the bridge. Tag everything that
/// crosses the Rust FFI with this subsystem so users can tail the
/// app's diagnostic output with:
///
///   log stream --predicate 'subsystem == "com.apfsfastindex.app"'
///
/// macOS's bundled-app stderr redirect makes `NSLog` invisible
/// outside Xcode debug runs; `os.Logger` writes into the unified
/// log where Console.app and the `log(1)` CLI can both find it.
let appLogger = Logger(subsystem: "com.apfsfastindex.app", category: "ffi")

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

    /// Filesystem path the Rust panic hook appends to on every
    /// caught panic. Lives under `~/Library/Logs/` so the user
    /// can find it via Finder → ⌘⇧G. Pointer is process-static;
    /// the panic hook installs itself on the first FFI call.
    static var logPath: String? {
        guard let cstr = apfs_log_path() else { return nil }
        let s = String(cString: cstr)
        return s.isEmpty ? nil : s
    }

    /// Pop the most-recent error message recorded by the Rust
    /// side on the current thread. Reading clears the slot, so a
    /// caller that wants the message should grab it immediately
    /// after a failed FFI call (NULL return / sentinel value).
    /// Covers both recoverable errors (bad UTF-8 path, scan-side
    /// failures) and caught panics in debug builds.
    static func lastError() -> String? {
        guard let cstr = apfs_last_error() else { return nil }
        let s = String(cString: cstr)
        return s.isEmpty ? nil : s
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
        // Two probe shapes:
        //   - lightweight (default) on /usr/bin — fast launch.
        //   - per-phase benchmark on /Applications when
        //     `APFS_BENCH=1` is set. Comparable to the
        //     /Applications numbers in the v4 perf study so we
        //     can confirm native is meaningfully faster than
        //     the WKWebView pipeline before continuing.
        DispatchQueue.global(qos: .background).async {
            let env = ProcessInfo.processInfo.environment
            if env["APFS_BENCH"] == "1" {
                let benchPath = env["APFS_BENCH_PATH"] ?? "/Applications"
                runNativeBench(path: benchPath)
            } else {
                runLightProbe(path: "/usr/bin")
            }
        }
        return true
    }
}

/// Lightweight per-launch probe. Same shape as the phase-2/3
/// probes from earlier commits.
private func runLightProbe(path probePath: String) {
    if let scan = Scan.fallback(path: probePath, threads: 0, crossMounts: false) {
        NSLog(
            "[native probe] %@: %llu entries, %llu logical bytes, allocated=%@",
            probePath,
            scan.entryCount,
            scan.logicalTotal,
            scan.allocatedTotal.map(String.init) ?? "unclaimed"
        )
    } else {
        NSLog("[native probe] \(probePath): FAILED")
    }
}

/// Per-phase benchmark probe. Times scan / tree / layout / hit
/// against the same target the v4 perf study used (/Applications
/// by default) so the native numbers can be compared 1:1 to the
/// WKWebView pipeline's published costs.
private func runNativeBench(path: String) {
    NSLog("[native bench] target=%@", path)

    // Scan + tree build are folded into Scan.fallback (the FFI
    // runs the fallback walker, then builds the tree before
    // returning the handle), so we time them together as
    // "scan+tree" and document the breakdown.
    let tScan0 = Date()
    guard let scan = Scan.fallback(path: path, threads: 0, crossMounts: false) else {
        NSLog("[native bench] scan FAILED for %@", path)
        return
    }
    let scanMs = Date().timeIntervalSince(tScan0) * 1000.0
    let entries = scan.entryCount
    let nodes = scan.nodeCount
    let logical = scan.logicalTotal
    let allocated = scan.allocatedTotal.map(String.init) ?? "unclaimed"
    NSLog(
        "[native bench] scan+tree: %.1f ms · %llu entries · %u nodes · logical=%llu allocated=%@",
        scanMs, entries, nodes, logical, allocated
    )

    // Layout cold (depth=0, viewport 1200×800 to match the
    // typical app window). Three runs so we see warm-cache vs
    // cold-cache variance (today: no internal cache — every
    // call is a fresh layout).
    let viewportW: Float = 1200
    let viewportH: Float = 800
    var layoutMsRuns: [Double] = []
    var lastLayout: Scan.Layout?
    for i in 0..<3 {
        let t0 = Date()
        let layout = scan.layout(
            rootedAt: 0,
            maxDepth: 0,
            metric: .logical,
            width: viewportW,
            height: viewportH
        )
        let ms = Date().timeIntervalSince(t0) * 1000.0
        layoutMsRuns.append(ms)
        if i == 2 {
            lastLayout = layout
        }
    }
    let layoutFmt = layoutMsRuns
        .map { String(format: "%.1f", $0) }
        .joined(separator: ", ")
    let cellCount = lastLayout?.count ?? 0
    NSLog(
        "[native bench] layout (depth=0, %.0fx%.0f) [3 runs]: %@ ms · %d cells",
        Double(viewportW), Double(viewportH), layoutFmt, cellCount
    )

    // Hit-test 10 000 random points to amortise the (1 µs)
    // per-call cost above timer noise.
    if let layout = lastLayout, layout.count > 0 {
        let n = 10_000
        let t0 = Date()
        var hits = 0
        for i in 0..<n {
            // Cheap LCG-style spread, not crypto-random; we
            // just want the points to land at varied locations.
            let x = Float((i * 1103515245 + 12345) & 0x7fff) / 32767.0 * viewportW
            let y = Float((i * 2147483647 + 17) & 0x7fff) / 32767.0 * viewportH
            if layout.hitTest(x: x, y: y) != nil {
                hits += 1
            }
        }
        let totalMs = Date().timeIntervalSince(t0) * 1000.0
        let perHitUs = (totalMs / Double(n)) * 1000.0
        NSLog(
            "[native bench] hit-test [%d random points]: %.1f ms total · %.2f µs/query · %d hits",
            n, totalMs, perHitUs, hits
        )
    }

    // Layout at a deeper rooted path so the squarify cost
    // scaling is visible. Pick the largest top-level child by
    // logical size — that's the dir the user is most likely to
    // drill into first.
    if scan.childCount(of: 0) > 0 {
        var heaviestIdx: UInt32 = 1
        var heaviestVal: UInt64 = 0
        // Walk root's direct children to find the biggest by
        // value_logical. Bounded by root's child count which
        // is small (~150 on /Applications).
        for i in 0..<scan.childCount(of: 0) {
            let childIdx = UInt32(1 + Int(i)) // not strictly correct — need a real children-accessor FFI in phase 5b
            if childIdx >= scan.nodeCount { break }
            let v = scan.valueLogical(of: childIdx)
            if v > heaviestVal {
                heaviestVal = v
                heaviestIdx = childIdx
            }
        }
        let t0 = Date()
        let sub = scan.layout(
            rootedAt: heaviestIdx,
            maxDepth: 0,
            metric: .logical,
            width: viewportW,
            height: viewportH
        )
        let ms = Date().timeIntervalSince(t0) * 1000.0
        let path = scan.path(of: heaviestIdx) ?? "(no path)"
        NSLog(
            "[native bench] sub-layout rooted at idx %u (%@, %llu logical): %.1f ms · %d cells",
            heaviestIdx, path, heaviestVal, ms, sub?.count ?? 0
        )
    }

    NSLog("[native bench] done — compare to v4 perf study (WKWebView pipeline):")
    NSLog("[native bench]   /Applications first paint (WKWebView+canvas): ~800ms-1.5s")
    NSLog("[native bench]   /Applications first paint (WKWebView+SVG):    ~2-3s")
    NSLog("[native bench]   /             first paint (WKWebView):         ~5-8s")
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

    /// `true` when this Scan was produced under the
    /// "Scan as Administrator…" flow (privileged subprocess, or
    /// — when the GUI is already running as root — the in-process
    /// fallback walker). Drives the status-bar admin chip and the
    /// window title suffix. The Rust crate's
    /// `apfs_scan_from_msgpack_file` cannot tell whether the
    /// msgpack came from a privileged scan; we set this field at
    /// the Swift-construction site that knows.
    ///
    /// `var` rather than `let` so callers can flip the badge after
    /// construction when the in-process fallback path happens to
    /// inherit root privileges (e.g. the `alreadyRoot` short-
    /// circuit in `PrivilegedScan.run`). The flag is advisory —
    /// it doesn't change the Rust-side handle, only the UI.
    var isAdmin: Bool
    /// `true` when this scan targeted a path on a snapshot
    /// filesystem (statfs.f_flags & MNT_SNAPSHOT). Set at
    /// construction time by inspecting the path the user typed
    /// or via the same MNT_SNAPSHOT check inside
    /// `PrivilegedScan`. Drives a separate "Snapshot" status
    /// chip — orthogonal to `isAdmin`, since walking a
    /// pre-mounted snapshot doesn't require root, but the data
    /// is still snapshot-frozen and worth labelling.
    var isSnapshotPath: Bool

    /// Performs a fallback (POSIX-traversal) scan of `path`.
    /// `threads` of 0 picks the default. Returns `nil` if the
    /// Rust side rejected the path (bad UTF-8, missing,
    /// permission denied at the root).
    static func fallback(path: String, threads: UInt32 = 0, crossMounts: Bool = false) -> Scan? {
        let handle = path.withCString { cPath in
            apfs_scan_directory(cPath, threads, crossMounts)
        }
        guard let handle else { return nil }
        return Scan(handle: handle, isAdmin: false, isSnapshotPath: SnapshotDetect.isOnSnapshot(path))
    }

    /// Rehydrate a Scan from a msgpack file written by a
    /// privileged subprocess (the "Scan as Administrator…" flow).
    /// The file is a `FallbackScanOutput` serialised with
    /// `rmp_serde::to_vec_named`; the Rust FFI decodes it and
    /// builds the same handle the in-process scan produces.
    /// Returns `nil` on read / decode failure; the caller can
    /// read `apfs_last_error` for the cause.
    static func fromPrivilegedMsgpack(path: String, sourcePath: String? = nil) -> Scan? {
        let handle = path.withCString { cPath in
            apfs_scan_from_msgpack_file(cPath)
        }
        guard let handle else { return nil }
        // The `path` argument here is the msgpack temp-file path;
        // `sourcePath` is the user-supplied scan target. Detect
        // snapshot-path on the source, not the temp file.
        let detect = sourcePath.map { SnapshotDetect.isOnSnapshot($0) } ?? false
        return Scan(handle: handle, isAdmin: true, isSnapshotPath: detect)
    }

    /// Run the in-process fallback walker and mark the result as
    /// admin-mode. Used when the GUI app is already running as
    /// root (e.g. launched via `sudo` or by a SMAppService helper
    /// in a future build) — the osascript escalation prompt is
    /// redundant in that case, and the in-process walker already
    /// sees every TCC-restricted path because the process EUID
    /// is 0. The result carries `isAdmin = true` so the
    /// status-bar chip and window title suffix still appear.
    static func fallbackAsAdministrator(
        path: String,
        threads: UInt32 = 0,
        crossMounts: Bool = false
    ) -> Scan? {
        let handle = path.withCString { cPath in
            apfs_scan_directory(cPath, threads, crossMounts)
        }
        guard let handle else { return nil }
        return Scan(handle: handle, isAdmin: true, isSnapshotPath: SnapshotDetect.isOnSnapshot(path))
    }

    /// One snapshot from the running scanner — `scanned` and
    /// `skipped` are running entry counts, `elapsedMs` is wall
    /// time since the scan began, `terminal` is `true` on the
    /// final event when the scan is done (`scanned + skipped`
    /// matches the final entry count).
    struct ProgressSnapshot {
        let scanned: UInt64
        let skipped: UInt64
        /// Cumulative logical bytes seen so far — used as the
        /// numerator of the determinate progress bar.
        let bytes: UInt64
        let elapsedMs: UInt64
        let terminal: Bool
    }

    /// Like `fallback`, but invokes `onProgress` from a background
    /// thread on each scanner tick (≈ every 250 ms during the
    /// scan plus one terminal event). The callback is *not*
    /// marshalled to the main queue automatically — UI consumers
    /// must dispatch back themselves.
    ///
    /// Implemented with the `_with_progress` FFI: a Swift box
    /// holding the closure is `passRetained` into Rust as the
    /// userdata pointer, and a trampoline `extern "C" fn` casts
    /// it back to invoke `onProgress`. The box is released after
    /// the scan returns (success or failure) — the Rust side
    /// guarantees no callbacks fire past return, so this is safe.
    static func fallbackWithProgress(
        path: String,
        threads: UInt32 = 0,
        crossMounts: Bool = false,
        onProgress: @escaping (ProgressSnapshot) -> Void
    ) -> Scan? {
        final class Box {
            let cb: (ProgressSnapshot) -> Void
            init(_ cb: @escaping (ProgressSnapshot) -> Void) { self.cb = cb }
        }
        let box = Box(onProgress)
        let userdata = Unmanaged.passRetained(box).toOpaque()

        // Trampoline must be a non-capturing `@convention(c)`
        // function pointer; we recover the box via the
        // `userdata` arg and forward to `box.cb`.
        let trampoline: @convention(c) (UInt64, UInt64, UInt64, UInt64, Bool, UnsafeMutableRawPointer?) -> Void = {
            scanned, skipped, bytes, elapsedMs, terminal, ud in
            guard let ud else { return }
            let b = Unmanaged<Box>.fromOpaque(ud).takeUnretainedValue()
            b.cb(ProgressSnapshot(
                scanned: scanned,
                skipped: skipped,
                bytes: bytes,
                elapsedMs: elapsedMs,
                terminal: terminal
            ))
        }

        let handle = path.withCString { cPath in
            apfs_scan_directory_with_progress(
                cPath, threads, crossMounts, trampoline, userdata
            )
        }
        // Always release the retained box once the scan has
        // returned (no further callbacks can fire after this
        // point — the Rust side joins its progress thread before
        // returning).
        Unmanaged<Box>.fromOpaque(userdata).release()

        guard let handle else { return nil }
        return Scan(handle: handle, isAdmin: false, isSnapshotPath: SnapshotDetect.isOnSnapshot(path))
    }

    private let handle: OpaquePointer

    private init(handle: OpaquePointer, isAdmin: Bool, isSnapshotPath: Bool = false) {
        self.handle = handle
        self.isAdmin = isAdmin
        self.isSnapshotPath = isSnapshotPath
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

    // MARK: - Phase 3 tree queries

    /// Sentinel matching `APFS_NODE_INVALID` (`u32::MAX`) on the
    /// Rust side — "no such node".
    static let nodeInvalid: UInt32 = UInt32.max

    /// Total node count in the indexed tree (root + every
    /// synthesised directory + every leaf). Larger than
    /// `entryCount` because directories at intermediate path
    /// components also become nodes even if no entry directly
    /// names them.
    var nodeCount: UInt32 {
        apfs_scan_node_count(handle)
    }

    /// Find a node by its absolute logical path. Empty string
    /// and `"/"` both map to the root. Returns `nil` for missing
    /// paths.
    func nodeIndex(forPath path: String) -> UInt32? {
        let idx = path.withCString { apfs_scan_node_index_for_path(handle, $0) }
        return idx == Scan.nodeInvalid ? nil : idx
    }

    /// Immediate-child count for a node.
    func childCount(of nodeIndex: UInt32) -> UInt32 {
        apfs_scan_node_child_count(handle, nodeIndex)
    }

    /// Subtree `value_logical` for the node. Pre-computed during
    /// scan-time tree finalize.
    func valueLogical(of nodeIndex: UInt32) -> UInt64 {
        apfs_scan_node_value_logical(handle, nodeIndex)
    }

    /// Subtree `value_allocated` for the node, or `nil` when
    /// SR-019 None-collapse fired in this subtree.
    func valueAllocated(of nodeIndex: UInt32) -> UInt64? {
        let raw = apfs_scan_node_value_allocated(handle, nodeIndex)
        return raw == Scan.allocatedTotalUnclaimed ? nil : raw
    }

    /// File / symlink / other count beneath this node.
    /// `itemCount(of: root)` is the total non-directory entries
    /// in the scan.
    func itemCount(of nodeIndex: UInt32) -> UInt64 {
        apfs_scan_node_item_count(handle, nodeIndex)
    }

    /// Absolute logical path of the node, e.g.
    /// `"Library/Application Support"`. Empty string for the
    /// root. Returns nil for invalid indices.
    func path(of nodeIndex: UInt32) -> String? {
        let ref = apfs_scan_node_path(handle, nodeIndex)
        return Scan.stringFrom(ref)
    }

    /// Last path component (display name).
    func name(of nodeIndex: UInt32) -> String? {
        let ref = apfs_scan_node_name(handle, nodeIndex)
        return Scan.stringFrom(ref)
    }

    /// Parent node index, or nil for the root / invalid index.
    /// Used by the breadcrumb (walk root-ward from `currentNode`).
    func parent(of nodeIndex: UInt32) -> UInt32? {
        let p = apfs_scan_node_parent(handle, nodeIndex)
        return p == Scan.nodeInvalid ? nil : p
    }

    /// Entry kind for the node.
    enum NodeKind: UInt32 {
        case dir = 0
        case file = 1
        case symlink = 2
        case other = 3
        case invalid = 0xff
    }
    func kind(of nodeIndex: UInt32) -> NodeKind {
        NodeKind(rawValue: apfs_scan_node_kind(handle, nodeIndex)) ?? .invalid
    }

    /// Immediate-children indices for the node. Returns an empty
    /// buffer for invalid indices or leaves. Lifetime is tied to
    /// this `Scan` — Swift must not retain the buffer past it.
    func children(of nodeIndex: UInt32) -> UnsafeBufferPointer<UInt32> {
        let slice = apfs_scan_node_children(handle, nodeIndex)
        return UnsafeBufferPointer(start: slice.items, count: Int(slice.count))
    }

    /// Helper: turn a borrowed Rust `(bytes, len)` pair into a
    /// Swift `String`. Copies because `Data(bytesNoCopy:)` would
    /// alias Rust-owned memory beyond Swift's tracking and ARC
    /// can't reason about that.
    private static func stringFrom(_ ref: ApfsPathRef) -> String? {
        guard let bytes = ref.bytes, ref.len > 0 else {
            // Zero-length and root path both decode to "".
            return ref.bytes != nil ? "" : nil
        }
        let data = Data(bytes: bytes, count: Int(ref.len))
        return String(data: data, encoding: .utf8)
    }

    // MARK: - Phase 3b render

    /// Treemap metric the renderer should size cells by.
    /// `0 = logical`, `1 = allocated` (matches the Rust enum
    /// discriminants in `apfs_render_cells`).
    enum Metric: UInt32 {
        case logical = 0
        case allocated = 1
    }

    /// Owning wrapper around an `ApfsLayout` — the Rust handle
    /// that holds both the laid-out cells and the spatial-hash
    /// hit-test grid. `deinit` calls `apfs_layout_free`. Swift
    /// keeps a `Layout` alive for as long as the NSView needs
    /// it (one per (node, depth, metric, dims) request).
    final class Layout {
        fileprivate let handle: OpaquePointer
        let count: Int

        /// Read access to the laid-out cells as
        /// `UnsafeBufferPointer<ApfsCell>` — `forEach`, `for…in`,
        /// random indexing all work. Pointer + count come
        /// straight from the Rust slice with no copy; valid for
        /// the lifetime of this `Layout`.
        var cells: UnsafeBufferPointer<ApfsCell> {
            let slice = apfs_layout_cells(handle)
            return UnsafeBufferPointer(start: slice.cells, count: Int(slice.count))
        }

        fileprivate init(handle: OpaquePointer, count: Int) {
            self.handle = handle
            self.count = count
        }

        deinit {
            apfs_layout_free(handle)
        }

        /// Sentinel matching `APFS_CELL_INVALID` (`u32::MAX`).
        static let cellInvalid: UInt32 = UInt32.max

        /// Hit-test the layout at the given CSS-pixel point.
        /// Returns the index of the deepest cell containing
        /// `(x, y)`, or `nil` if no cell does. Sub-millisecond
        /// on a /-scan thanks to the Rust-side 64×64 spatial
        /// hash; safe to call every mousemove.
        func hitTest(x: Float, y: Float) -> UInt32? {
            let idx = apfs_layout_hit_test(handle, x, y)
            return idx == Layout.cellInvalid ? nil : idx
        }
    }

    /// Lay out the subtree rooted at `nodeIndex` into a viewport
    /// of CSS-pixel `width × height`. `maxDepth = 0` is the
    /// unlimited sentinel (matches the depth-picker behaviour).
    /// Returns nil when the subtree has nothing to draw (no
    /// children with positive value).
    func layout(
        rootedAt nodeIndex: UInt32,
        maxDepth: UInt32,
        metric: Metric,
        width: Float,
        height: Float
    ) -> Layout? {
        guard let handle = apfs_layout_new(
            self.handle, nodeIndex, maxDepth, metric.rawValue, width, height
        ) else { return nil }
        let count = Int(apfs_layout_cell_count(handle))
        return Layout(handle: handle, count: count)
    }

    // MARK: - Phase 5d ext-list summary

    /// Owning wrapper around an `ApfsExtSummaryHandle`. Built
    /// once per (node, metric) tuple; the Swift panel calls
    /// `row(at:)` for each visible row. `deinit` calls
    /// `apfs_scan_ext_summary_free`.
    final class ExtSummary {
        fileprivate let handle: OpaquePointer
        let count: Int
        let totalValue: UInt64
        let anyUnclaimed: Bool

        /// One materialised row. We copy out into Swift-owned
        /// types so the SwiftUI view doesn't have to hold the
        /// `ApfsPathRef` lifetime invariant; the ext strings
        /// are tiny (< 16 chars typical) so the copy cost is
        /// negligible.
        struct Row: Identifiable {
            let id: Int
            let ext: String
            let valueLogical: UInt64
            let valueAllocated: UInt64?
            let fileCount: UInt32
        }

        fileprivate init(handle: OpaquePointer) {
            self.handle = handle
            self.count = Int(apfs_scan_ext_summary_count(handle))
            self.totalValue = apfs_scan_ext_summary_total(handle)
            self.anyUnclaimed = apfs_scan_ext_summary_any_unclaimed(handle)
        }

        deinit {
            apfs_scan_ext_summary_free(handle)
        }

        /// Materialise row `n`. Index out of range returns a
        /// blank-extension row with zero values.
        func row(at n: Int) -> Row {
            let r = apfs_scan_ext_summary_row(handle, UInt32(n))
            let ext: String = {
                guard let bytes = r.ext.bytes, r.ext.len > 0 else { return "" }
                let data = Data(bytes: bytes, count: Int(r.ext.len))
                return String(data: data, encoding: .utf8) ?? ""
            }()
            let allocated: UInt64? =
                r.value_allocated == Scan.allocatedTotalUnclaimed ? nil : r.value_allocated
            return Row(
                id: n,
                ext: ext,
                valueLogical: r.value_logical,
                valueAllocated: allocated,
                fileCount: r.file_count
            )
        }

        /// Materialise every row at once. Cheap enough for the
        /// typical case (~50 extensions per directory tree); the
        /// SwiftUI `ForEach` consumes the array directly.
        func allRows() -> [Row] {
            (0..<count).map { row(at: $0) }
        }
    }

    /// Build the ext-list summary for the subtree rooted at
    /// `nodeIndex`. Returns nil on invalid args.
    func extSummary(rootedAt nodeIndex: UInt32, metric: Metric) -> ExtSummary? {
        guard let h = apfs_scan_ext_summary_new(handle, nodeIndex, metric.rawValue) else {
            return nil
        }
        return ExtSummary(handle: h)
    }

    // MARK: - Walk skips (audit r3 #F1)

    /// One walker skip event — a subtree the parser couldn't or
    /// wouldn't descend into. Reasons include
    /// `permission_denied`, `mount_boundary`, `non_utf8_name`,
    /// `depth_cap_reached(N)`, and `drec_cycle(file_id=X)`.
    struct WalkSkip: Identifiable {
        let id: Int
        let path: String
        let reason: String
    }

    /// All walk-skip rows from this scan. Empty when the scan
    /// completed cleanly. Materialised once into `[WalkSkip]`
    /// rather than borrowed pointers — the typical scan has at
    /// most a few hundred skip rows, and SwiftUI consumes
    /// `Identifiable` collections more naturally than buffer
    /// pointers.
    func walkSkips() -> [WalkSkip] {
        let count = Int(apfs_scan_walk_skip_count(handle))
        guard count > 0 else { return [] }
        var out: [WalkSkip] = []
        out.reserveCapacity(count)
        for i in 0..<count {
            let row = apfs_scan_walk_skip_row(handle, UInt32(i))
            let path = Self.stringFrom(row.path) ?? ""
            let reason = Self.stringFrom(row.reason) ?? ""
            out.append(WalkSkip(id: i, path: path, reason: reason))
        }
        return out
    }
}
