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

    /// One snapshot from the running scanner — `scanned` and
    /// `skipped` are running entry counts, `elapsedMs` is wall
    /// time since the scan began, `terminal` is `true` on the
    /// final event when the scan is done (`scanned + skipped`
    /// matches the final entry count).
    struct ProgressSnapshot {
        let scanned: UInt64
        let skipped: UInt64
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
        let trampoline: @convention(c) (UInt64, UInt64, UInt64, Bool, UnsafeMutableRawPointer?) -> Void = {
            scanned, skipped, elapsedMs, terminal, ud in
            guard let ud else { return }
            let b = Unmanaged<Box>.fromOpaque(ud).takeUnretainedValue()
            b.cb(ProgressSnapshot(
                scanned: scanned,
                skipped: skipped,
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
}
