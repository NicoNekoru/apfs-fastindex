import Foundation
import SwiftUI
import WebKit

enum ScanMode: String, CaseIterable, Hashable {
    case auto
    case raw
    case fallback

    var cliValue: String { rawValue }
}

/// Drives the `apfs-fastindex-scan` subprocess and bridges results into
/// the viz WKWebView.
///
/// The earlier `Task.detached` + `nonisolated async` plumbing was
/// fragile: under `@MainActor` isolation, the optional-chained
/// `self?.streamProgress(...)` could end up serialized through the main
/// actor anyway and `FileHandle.read(upToCount:)` would block the run
/// loop — the progress counters didn't tick until the subprocess closed
/// its pipes. We use `FileHandle.readabilityHandler` instead: the
/// callback fires on a private background queue by construction, we
/// process the bytes there, and hop to main via `DispatchQueue.main`
/// for the @Published state mutations.
@MainActor
final class ScanController: ObservableObject {
    @Published var targetPath: String = ""
    @Published var mode: ScanMode = .auto
    @Published var crossMounts: Bool = false
    @Published var isScanning: Bool = false
    /// True from the moment the scanner subprocess exits to the
    /// moment the viz fires `ingest_succeeded` (or `ingest_failed`).
    /// Covers three concrete sub-phases that the user sees as one
    /// "still working" gap: writing the scan JSON to a temp file,
    /// the WKWebView's XHR fetching it, and the viz's JSON.parse +
    /// `ingest()` materialising the treemap. On a `/`-scale scan
    /// (3M entries, ~150 MB JSON) this gap is several seconds long;
    /// without a spinner the UI looks frozen because `isScanning`
    /// has already flipped to false.
    @Published var isIngesting: Bool = false
    /// True from the moment the scanner emits its final
    /// `terminal: true` progress event to the moment the
    /// subprocess actually exits (and `isIngesting` flips on).
    /// Covers the JSON-serialise + stdout-transfer gap on big
    /// scans: the user has stopped seeing the count tick up, the
    /// scanner is still running, but there's nothing to show
    /// progress for. The spinner overlay reads
    /// `isFinalizingScan || isIngesting` so the visual
    /// continuity from "scan progressing" → "viz loading" is
    /// unbroken on `/`-class scans where the gap is multiple
    /// seconds.
    @Published var isFinalizingScan: Bool = false
    @Published var scannedCount: UInt64 = 0
    @Published var skippedCount: UInt64 = 0
    @Published var elapsedMs: UInt64 = 0
    @Published var lastError: String? = nil
    @Published var selectedPath: String = ""
    @Published var correctnessClaim: String = ""
    /// Sum of `entry.logical_size` across the loaded scan's root,
    /// populated once the viz finishes ingest. `0` until then.
    @Published var logicalTotal: UInt64 = 0
    /// Sum of `entry.allocated_size` across the loaded scan's root.
    /// `nil` means SR-019 / EX-22 None-collapse fired (at least one
    /// sparse or decmpfs row in the subtree). When
    /// `allocatedColumnAvailable == false` the scan pre-dates R2-A
    /// and this column should be hidden entirely.
    @Published var allocatedTotal: UInt64? = nil
    @Published var allocatedColumnAvailable: Bool = false
    /// SourceDescriptor.source_kind reported by the most recent scan
    /// the viz successfully ingested. Empty until the first scan
    /// finishes. The shell uses this to decide whether file operations
    /// (Reveal in Finder / Move to Trash / Copy Path) are valid for
    /// the currently-loaded scan: only `mounted_directory` resolves to
    /// on-disk paths the host can act on.
    @Published var lastScanSourceKind: String = ""
    /// Absolute path the scanner was pointed at (parser_output.source.
    /// requested_path). Combined with an entry's relative path to
    /// resolve a real on-disk URL for Reveal in Finder / Move to
    /// Trash. Empty when no scan has loaded yet.
    @Published var lastScanRequestedPath: String = ""
    /// Surfaced when a file operation finishes (or fails). The status
    /// bar shows it for a couple of seconds before going back to the
    /// scan-summary text. Cleared on next scan / next op.
    @Published var lastOperationMessage: String? = nil {
        didSet {
            // Generation token so a fast-fire of multiple ops doesn't
            // have stale timers clear a newer message early.
            operationMessageGeneration &+= 1
            let generation = operationMessageGeneration
            if lastOperationMessage != nil {
                DispatchQueue.main.asyncAfter(deadline: .now() + 3.5) { [weak self] in
                    guard let self else { return }
                    if self.operationMessageGeneration == generation {
                        self.lastOperationMessage = nil
                    }
                }
            }
        }
    }
    private var operationMessageGeneration: UInt64 = 0

    /// Latest scan-result URL written to disk so the viz can load it via
    /// XHR. Retained for compatibility with the SwiftUI re-render
    /// path; the post-scan flow no longer writes a temp file — it
    /// hands the bytes directly to `vizCoordinator.currentScanData`
    /// and stashes the same buffer in `pendingScanData` for the
    /// replay-on-bind case.
    private(set) var pendingScanFileURL: URL? = nil
    /// Latest scan-result bytes, kept in memory until the next scan
    /// or reset replaces them. Used by `bindWebView` to replay the
    /// scan into a WKWebView that hadn't finished its first
    /// navigation when the controller fired `__apfs_ingest_file__`.
    private(set) var pendingScanData: Data? = nil
    private(set) var pendingProgress: ProgressUpdate? = nil

    private weak var webView: WKWebView?
    /// Direct handle to the WebView's coordinator (also the
    /// `WKURLSchemeHandler` for `apfs-scan://`). Set in `bindWebView`
    /// so we can stash the latest scan-result file URL on the handler
    /// *before* the JS XHR fires — going through SwiftUI's
    /// `updateNSView` propagation has a one-frame lag that loses the
    /// race with `evaluateJavaScript`.
    private weak var vizCoordinator: VizWebView.Coordinator?
    private var scanProcess: Process?
    private var scanCancelled: Bool = false

    /// Thread-safe accumulator for stdout bytes. Lives outside the
    /// @MainActor class so the `readabilityHandler` and the
    /// `terminationHandler` can append + snapshot it without crossing
    /// the actor boundary on every chunk.
    private let stdoutBox = ScanBufferBox()
    private var stderrBuffer = Data()
    private var lastTempScanURL: URL? = nil

    var modeLabel: String {
        switch mode {
        case .auto:     return "auto"
        case .raw:      return "raw"
        case .fallback: return "fallback"
        }
    }

    var statusText: String {
        if isScanning {
            let elapsedSec = Double(elapsedMs) / 1000.0
            return String(format: "scanning… %llu entries, %.1fs elapsed", scannedCount, elapsedSec)
        }
        if let err = lastError {
            return "error: \(err)"
        }
        if scannedCount > 0 {
            return String(format: "%llu entries, %.2fs", scannedCount, Double(elapsedMs) / 1000.0)
        }
        return "ready"
    }

    /// True iff the viz has a usable scan loaded. The toolbar collapses
    /// from the "pick a target" prompt to a compact summary whenever
    /// this is true, so the user isn't asked to start a scan they
    /// already finished. While `isIngesting` is true we deliberately
    /// return false so the toolbar stays on the scanning-style chrome
    /// (no "Rescan / New…" buttons) until the treemap actually shows.
    var hasLoadedScan: Bool {
        !isScanning && !isIngesting && lastError == nil && scannedCount > 0
    }

    /// Drop the loaded scan and reset the viz, leaving `targetPath`
    /// alone so the user can edit it for the next scan. Called from
    /// the "New…" button in the post-scan toolbar.
    func clearLoadedScan() {
        scannedCount = 0
        skippedCount = 0
        elapsedMs = 0
        correctnessClaim = ""
        logicalTotal = 0
        allocatedTotal = nil
        allocatedColumnAvailable = false
        lastScanSourceKind = ""
        lastScanRequestedPath = ""
        selectedPath = ""
        lastError = nil
        lastOperationMessage = nil
        isIngesting = false
        cleanupLastTempScan()
        evaluateClearScan()
    }

    /// Cached decimal formatter for entry counts. The previous build
    /// allocated a `NumberFormatter` on every read of
    /// `liveCountersText` and `loadedSummaryText`, both of which
    /// SwiftUI re-evaluates on every @Published change — i.e. four
    /// times a second while a scan is running. NumberFormatter
    /// initialisation walks the CFLocale tables and is the most
    /// expensive accessor on this hot path. Caching it is a clean
    /// 4× reduction in allocations per second for the toolbar
    /// re-renders.
    private static let entryCountFormatter: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .decimal
        return f
    }()

    /// Compact "1,234,567 entries · 2.3s" line for the scanning
    /// toolbar. Locale-aware grouping makes huge counts readable
    /// (an `/` scan tops out around 5M entries).
    var liveCountersText: String {
        let entries = Self.entryCountFormatter.string(from: NSNumber(value: scannedCount))
            ?? "\(scannedCount)"
        let elapsed = Double(elapsedMs) / 1000.0
        let elapsedStr: String
        if elapsed < 10 {
            elapsedStr = String(format: "%.1fs", elapsed)
        } else {
            elapsedStr = String(format: "%.0fs", elapsed)
        }
        return "\(entries) entries · \(elapsedStr)"
    }

    /// Post-scan summary for the loaded-toolbar chip: entries count +
    /// elapsed time. The size totals live in the status bar.
    var loadedSummaryText: String {
        let entries = Self.entryCountFormatter.string(from: NSNumber(value: scannedCount))
            ?? "\(scannedCount)"
        let elapsed = Double(elapsedMs) / 1000.0
        return String(format: "%@ entries · %.1fs", entries, elapsed)
    }

    /// What the left side of the status bar shows. Transient
    /// last-operation messages (Reveal in Finder, Move to Trash, …)
    /// take priority over the steady-state summary so the user gets
    /// immediate feedback when they invoke a file op; otherwise we
    /// fall through to `statusText`.
    var statusBarPrimaryText: String {
        if let op = lastOperationMessage { return op }
        return statusText
    }

    /// Human-readable size-totals string for the status bar, matching the
    /// viz's `formatBytes` / `formatAllocated` semantics. Empty when no
    /// scan has finished ingesting yet (i.e., logicalTotal is still 0
    /// AND the column hasn't reported in).
    var totalsText: String {
        if logicalTotal == 0 && !allocatedColumnAvailable {
            return ""
        }
        let logical = "logical: \(Self.formatBytes(logicalTotal))"
        guard allocatedColumnAvailable else { return logical }
        let allocated: String
        if let bytes = allocatedTotal {
            allocated = "allocated: \(Self.formatBytes(bytes))"
        } else {
            // SR-019 / EX-22 None-collapse: at least one sparse or
            // decmpfs row in the subtree means the aggregate is
            // deliberately not claimed. Surface that verbatim rather
            // than a misleading zero.
            allocated = "allocated: unclaimed"
        }
        return "\(logical) · \(allocated)"
    }

    /// Mirrors the JS `formatBytes()` in `viz/index.html` so the native
    /// status bar and the in-page tooltip agree on units.
    static func formatBytes(_ bytes: UInt64) -> String {
        if bytes == 0 { return "0 B" }
        let units = ["B", "KB", "MB", "GB", "TB", "PB"]
        let value = Double(bytes)
        let exponent = min(units.count - 1, Int(log10(value) / 3))
        let scaled = value / pow(1000.0, Double(exponent))
        let format: String
        if scaled >= 100 || exponent == 0 {
            format = "%.0f %@"
        } else if scaled >= 10 {
            format = "%.1f %@"
        } else {
            format = "%.2f %@"
        }
        return String(format: format, scaled, units[exponent])
    }

    func bindWebView(_ webView: WKWebView) {
        self.webView = webView
        // The Coordinator we registered in `makeNSView` is both the
        // navigation delegate and the URL-scheme handler for
        // `apfs-scan://`. Grab the reference so we can push the latest
        // scan-result file URL onto it directly without going through
        // the SwiftUI re-render path.
        self.vizCoordinator = webView.navigationDelegate as? VizWebView.Coordinator
        // Replay any scan that landed before the webview was ready
        // for `evaluateJavaScript`. Prefer the in-memory bytes (the
        // default since Lever C); fall back to the file URL for
        // any legacy code path.
        if let data = pendingScanData {
            vizCoordinator?.currentScanData = data
            evaluateLoadScan()
        } else if let url = pendingScanFileURL {
            vizCoordinator?.currentScanFileURL = url
            evaluateLoadScan()
        }
        if let progress = pendingProgress {
            evaluateSetProgress(progress)
        }
    }

    // MARK: - Scan lifecycle

    func startScan() {
        guard !isScanning, !targetPath.isEmpty else { return }
        guard let binary = locateScannerBinary() else {
            lastError = "could not locate apfs-fastindex-scan; build the release binary with `cargo build --release` from the repo root."
            return
        }
        scannedCount = 0
        skippedCount = 0
        elapsedMs = 0
        lastError = nil
        correctnessClaim = ""
        logicalTotal = 0
        allocatedTotal = nil
        allocatedColumnAvailable = false
        pendingProgress = nil
        // Discard any previous temp scan file before starting a new one.
        cleanupLastTempScan()
        stdoutBox.clear()
        stderrBuffer.removeAll(keepingCapacity: false)
        isScanning = true
        // A rescan from the loaded-scan state could otherwise leave
        // isIngesting true if the previous ingest hasn't reported back
        // yet. Reset here so the spinner only ever reflects this scan.
        isIngesting = false
        isFinalizingScan = false
        scanCancelled = false

        // Native shell defaults to `--format msgpack-stream`. The
        // viz dispatches on the URL-scheme handler's Content-Type
        // and decodes the byte stream one record at a time,
        // building the tree + re-painting the canvas before the
        // full scan output has been consumed — first paint can
        // land after ~5 k entries. Standalone HTML usage keeps
        // working with `--format json` for the drop-a-file
        // affordance.
        var args: [String] = ["--slim", "--progress", "--format", "msgpack-stream"]
        if mode != .auto {
            args.append(contentsOf: ["--mode", mode.cliValue])
        }
        if crossMounts && mode != .raw {
            args.append("--cross-mounts")
        }
        args.append(targetPath)

        let process = Process()
        process.executableURL = binary
        process.arguments = args

        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        // Wire the readability handlers BEFORE launching the process so
        // we don't miss the first burst of progress lines or output bytes.
        let stdoutReader = stdoutPipe.fileHandleForReading
        let stderrReader = stderrPipe.fileHandleForReading

        let bufferBox = stdoutBox
        // stdout: just buffer the bytes — the JSON is one big blob that
        // we ship to disk at EOF, no parsing here.
        stdoutReader.readabilityHandler = { handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                return
            }
            bufferBox.append(data)
        }

        // stderr: progress JSON, one object per line. Hop to main for the
        // @Published state update because that's where SwiftUI observes
        // them; the JSON parse itself is tiny (<200 bytes per line).
        stderrReader.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if data.isEmpty {
                handle.readabilityHandler = nil
                return
            }
            DispatchQueue.main.async { [weak self] in
                self?.appendStderr(data)
            }
        }

        process.terminationHandler = { [weak self] proc in
            // terminationHandler fires on a background queue. Drain any
            // remaining bytes from the pipes (Apple recommends a
            // best-effort `availableData` after termination), commit
            // them through the thread-safe box, then hop to main for
            // the final state update.
            let stdoutTail = stdoutReader.availableData
            let stderrTail = stderrReader.availableData
            if !stdoutTail.isEmpty { bufferBox.append(stdoutTail) }
            let collected = bufferBox.snapshot()
            DispatchQueue.main.async { [weak self] in
                if !stderrTail.isEmpty { self?.appendStderr(stderrTail) }
                self?.finishScan(process: proc, stdout: collected)
            }
        }

        scanProcess = process
        do {
            try process.run()
        } catch {
            stdoutReader.readabilityHandler = nil
            stderrReader.readabilityHandler = nil
            isScanning = false
            scanProcess = nil
            lastError = "failed to launch scanner: \(error.localizedDescription)"
        }
    }

    func cancelScan() {
        scanCancelled = true
        scanProcess?.terminate()
    }

    // MARK: - Stream handlers (main actor)

    private func appendStderr(_ data: Data) {
        stderrBuffer.append(data)
        // Pop one line at a time. Data uses Int indices that aren't
        // necessarily 0-based after slicing, so we always work from
        // `startIndex` and use `removeSubrange` to slide the buffer.
        while let newline = stderrBuffer.firstIndex(of: 0x0A) {
            let lineRange = stderrBuffer.startIndex..<newline
            let lineData = Data(stderrBuffer[lineRange])
            let dropRange = stderrBuffer.startIndex...newline
            stderrBuffer.removeSubrange(dropRange)
            if let update = decodeProgress(lineData) {
                scannedCount = update.scanned
                skippedCount = update.skipped
                elapsedMs = update.elapsedMs
                pendingProgress = update
                evaluateSetProgress(update)
                // The scanner emits `terminal: true` once at the
                // very end — right before it starts serialising
                // the scan output and writing it to stdout. From
                // here on the count won't tick anymore but the
                // subprocess still has seconds of work to do. Flip
                // the finalizing flag so the loading spinner can
                // come up immediately rather than waiting for the
                // subprocess to exit.
                if update.terminal {
                    isFinalizingScan = true
                }
            }
        }
    }

    private func finishScan(process: Process, stdout: Data) {
        isScanning = false
        // Hand off from the finalizing flag to the ingesting flag.
        // Both feed into the same spinner overlay condition, so
        // the user never sees a flicker between them.
        isFinalizingScan = false
        scanProcess = nil

        if scanCancelled {
            lastError = "scan cancelled"
            return
        }
        let exitCode = process.terminationStatus
        if exitCode != 0 {
            lastError = "scanner exited with status \(exitCode)"
            return
        }

        // Flip into the ingest phase: scanner has exited successfully
        // but the viz hasn't materialised the treemap yet. The spinner
        // overlay reads this flag; it clears in handleBridgeMessage on
        // either ingest_succeeded or ingest_failed.
        isIngesting = true

        // Pull the small `correctness_claim` field off the raw bytes
        // for the status line. Reads only the first ~4 KB.
        if let claim = Self.extractCorrectnessClaim(stdout) {
            correctnessClaim = claim
        }

        // Hand the scan bytes straight to the WKURLSchemeHandler —
        // no temp file roundtrip. The previous flow wrote ~150 MB
        // to `/tmp/apfs-scan-…json` on a background queue, then
        // the URL-scheme handler read it back via
        // `Data(contentsOf:, .mappedIfSafe)`. Skipping the disk
        // hop saves ~200 ms-1.5 s on `/`-class scans and one
        // FileManager allocation per scan. The bytes are
        // referenced from the coordinator; clearing it on the
        // next reset / scan-start lets ARC drop them.
        vizCoordinator?.currentScanData = stdout
        pendingScanData = stdout
        evaluateLoadScan()
    }

    private func cleanupLastTempScan() {
        // No temp file to remove since Lever C — the scan bytes
        // live in memory on the coordinator + the pendingScanData
        // mirror. Clear both so ARC can drop the ~150 MB the
        // previous scan held.
        if let url = lastTempScanURL {
            try? FileManager.default.removeItem(at: url)
            lastTempScanURL = nil
        }
        pendingScanFileURL = nil
        pendingScanData = nil
        vizCoordinator?.currentScanData = nil
    }

    // MARK: - WKWebView calls

    private func evaluateLoadScan() {
        guard let webView else { return }
        // The coordinator's `currentScanData` (or `currentScanFileURL`
        // fallback) must already be set by the caller. The JS shim
        // fetches `apfs-scan://current` regardless of what path arg
        // we pass — keep the call payload empty.
        let js = "if (window.__apfs_ingest_file__) __apfs_ingest_file__('');"
        webView.evaluateJavaScript(js) { _, err in
            if let err = err {
                NSLog("evaluateLoadScan JS error: \(err.localizedDescription)")
            }
        }
    }

    private func evaluateSetProgress(_ update: ProgressUpdate) {
        guard let webView else { return }
        let payload = """
        {"scanned":\(update.scanned),"skipped":\(update.skipped),"elapsedMs":\(update.elapsedMs),"terminal":\(update.terminal ? "true" : "false")}
        """
        let js = "if (window.__apfs_progress__) { __apfs_progress__(\(payload)); }"
        webView.evaluateJavaScript(js, completionHandler: nil)
    }

    /// Tell the bundled viz to drop its loaded scan and return to the
    /// empty state (the "Press Scan in the toolbar to begin." chrome
    /// the apfs-native-shell style injects). Used by `New…` to keep
    /// the WKWebView in sync with the controller's reset.
    private func evaluateClearScan() {
        guard let webView else { return }
        vizCoordinator?.currentScanFileURL = nil
        vizCoordinator?.currentScanData = nil
        let js = """
        (function() {
          try {
            const main = document.getElementById('main');
            const dropZone = document.getElementById('drop-zone');
            const statusBar = document.getElementById('status-bar');
            const breadcrumb = document.getElementById('breadcrumb');
            if (main) main.hidden = true;
            if (dropZone) dropZone.hidden = false;
            if (statusBar) statusBar.hidden = true;
            if (breadcrumb) breadcrumb.style.display = 'none';
            window.rootNode = null;
            window.allocatedAvailable = false;
            window.scanSource = null;
          } catch (e) { /* viz already gone */ }
        })();
        """
        webView.evaluateJavaScript(js, completionHandler: nil)
    }

    /// `static` so the call site doesn't need actor isolation. Scans
    /// only the first 4 KB of the buffer for `"correctness_claim"`,
    /// since the scanner emits that key near the top of the document.
    nonisolated static func extractCorrectnessClaim(_ data: Data) -> String? {
        let prefixData = data.prefix(4096)
        guard let prefix = String(data: prefixData, encoding: .utf8) else { return nil }
        guard let keyRange = prefix.range(of: "\"correctness_claim\"") else { return nil }
        let after = prefix[keyRange.upperBound...]
        guard let colon = after.firstIndex(of: ":") else { return nil }
        let rest = after[after.index(after: colon)...]
        guard let openQuote = rest.firstIndex(of: "\"") else { return nil }
        var cursor = rest.index(after: openQuote)
        var out = ""
        var escaped = false
        while cursor < rest.endIndex {
            let ch = rest[cursor]
            if escaped {
                out.append(ch)
                escaped = false
            } else if ch == "\\" {
                escaped = true
            } else if ch == "\"" {
                return out
            } else {
                out.append(ch)
            }
            cursor = rest.index(after: cursor)
        }
        return nil
    }

    // MARK: - Bridge inbound

    func handleBridgeMessage(_ message: BridgeMessage) {
        switch message {
        case .selected(let path, _, _):
            self.selectedPath = path
        case .contextMenu(let path, let kind, let x, let y):
            showContextMenu(forRelativePath: path, kind: kind, viewportX: x, viewportY: y)
        case .revealInFinder(let path):
            revealInFinder(relativePath: path)
        case .moveToTrash(let paths):
            // The viz currently only emits single-path trash requests
            // (one row at a time); the protocol carries a list for a
            // future multi-select pass.
            for path in paths {
                moveToTrash(relativePath: path)
            }
        case .consoleError(let message):
            // Surface viz-side failures in the host log. Without this
            // the page can silently swallow exceptions and we have no
            // signal that ingest broke.
            NSLog("[viz] console.error: %@", message)
        case .ingestStarted:
            NSLog("[viz] ingest started")
            // ingestStarted fires *during* the viz's JSON.parse +
            // ingest() pass; keep isIngesting true through this
            // sub-phase so the spinner stays visible. It will clear
            // in the success or failure branch below.
            self.logicalTotal = 0
            self.allocatedTotal = nil
            self.allocatedColumnAvailable = false
        case .ingestSucceeded(
            let root,
            let total,
            let logical,
            let allocated,
            let allocatedAvailable,
            let sourceKind,
            let sourceRequestedPath
        ):
            NSLog(
                "[viz] ingest ok: root=%@ entries=%llu logical=%llu allocated=%@ source=%@",
                root,
                total,
                logical,
                allocated.map { String($0) } ?? "unclaimed",
                sourceKind
            )
            self.logicalTotal = logical
            self.allocatedTotal = allocated
            self.allocatedColumnAvailable = allocatedAvailable
            self.lastScanSourceKind = sourceKind
            self.lastScanRequestedPath = sourceRequestedPath
            self.lastOperationMessage = nil
            // Treemap is on-screen now; drop the loading spinner and
            // flip into the loaded-scan toolbar.
            self.isIngesting = false
        case .ingestFailed(let message):
            NSLog("[viz] ingest failed: %@", message)
            self.lastError = "viz failed to load scan: \(message)"
            // Ingest failed: drop the spinner so the error message
            // is what the user sees (instead of a perpetual
            // spinner over an empty viz).
            self.isIngesting = false
        }
    }

    // MARK: - File operations

    /// True iff the currently-loaded scan resolves to on-disk paths
    /// the host can act on. `mounted_directory` is the fallback walker
    /// against a live directory; `dmg_image` and `raw_device` are
    /// scanners against detached images and have no live filesystem
    /// behind them.
    var fileOperationsAvailable: Bool {
        guard !lastScanRequestedPath.isEmpty else { return false }
        return lastScanSourceKind == "mounted_directory"
    }

    /// Resolve an entry's stored relative path against the scan root.
    /// Returns nil if the scan is not a `mounted_directory` (raw-mode
    /// entries cannot reach a real file), the entry's path is empty
    /// (the root row), or the resulting URL doesn't fall inside the
    /// scan root (defence against `..` traversal in malformed input).
    func resolveAbsoluteURL(forRelativePath relative: String) -> URL? {
        guard fileOperationsAvailable else { return nil }
        guard !relative.isEmpty else { return nil }
        let root = URL(fileURLWithPath: lastScanRequestedPath)
            .standardizedFileURL
            .resolvingSymlinksInPath()
        let candidate = root.appendingPathComponent(relative)
            .standardizedFileURL
        let rootComponents = root.pathComponents
        let candidateComponents = candidate.pathComponents
        guard candidateComponents.count >= rootComponents.count else { return nil }
        for (index, component) in rootComponents.enumerated() {
            if candidateComponents[index] != component { return nil }
        }
        return candidate
    }

    func revealInFinder(relativePath: String) {
        guard let url = resolveAbsoluteURL(forRelativePath: relativePath) else {
            lastOperationMessage = "cannot reveal: scan source is not a live directory"
            return
        }
        // `activateFileViewerSelecting:` brings Finder forward AND
        // selects the row, even for symlinks; it doesn't follow them.
        NSWorkspace.shared.activateFileViewerSelecting([url])
        lastOperationMessage = "revealed \(url.lastPathComponent) in Finder"
    }

    func openInFinder(relativePath: String) {
        guard let url = resolveAbsoluteURL(forRelativePath: relativePath) else {
            lastOperationMessage = "cannot open: scan source is not a live directory"
            return
        }
        // `open` invokes the default-app handler; equivalent to
        // double-clicking the file in Finder.
        NSWorkspace.shared.open(url)
    }

    func copyPathToPasteboard(relativePath: String) {
        guard let url = resolveAbsoluteURL(forRelativePath: relativePath) else {
            // Fall back to copying the relative path so the user
            // still gets *something* useful even when the scan was a
            // detached image.
            let pasteboard = NSPasteboard.general
            pasteboard.clearContents()
            pasteboard.setString(relativePath, forType: .string)
            lastOperationMessage = "copied relative path"
            return
        }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(url.path, forType: .string)
        lastOperationMessage = "copied \(url.lastPathComponent) path"
    }

    func moveToTrash(relativePath: String) {
        guard let url = resolveAbsoluteURL(forRelativePath: relativePath) else {
            lastOperationMessage = "cannot trash: scan source is not a live directory"
            return
        }
        let alert = NSAlert()
        alert.messageText = "Move “\(url.lastPathComponent)” to the Trash?"
        alert.informativeText = url.path
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Move to Trash")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else {
            return
        }
        do {
            var resultingURL: NSURL?
            try FileManager.default.trashItem(at: url, resultingItemURL: &resultingURL)
            lastOperationMessage = "moved \(url.lastPathComponent) to Trash"
        } catch {
            lastOperationMessage = "trash failed: \(error.localizedDescription)"
        }
    }

    /// Build and present an `NSMenu` at the given viewport coordinates.
    /// Coordinates come from the viz as `event.clientX/Y` (top-left of
    /// the WKWebView's bounds in points); we hand them off to AppKit
    /// after the calling NSEvent's window is resolved.
    func showContextMenu(
        forRelativePath path: String,
        kind: String,
        viewportX: Double,
        viewportY: Double
    ) {
        let menu = NSMenu()
        menu.autoenablesItems = false
        let reachable = fileOperationsAvailable && !path.isEmpty

        let header = NSMenuItem()
        header.title = path.isEmpty ? "/" : path
        header.isEnabled = false
        menu.addItem(header)
        menu.addItem(.separator())

        let open = NSMenuItem(
            title: "Open",
            action: #selector(ContextMenuTarget.open(_:)),
            keyEquivalent: ""
        )
        open.isEnabled = reachable
        menu.addItem(open)

        let reveal = NSMenuItem(
            title: "Reveal in Finder",
            action: #selector(ContextMenuTarget.reveal(_:)),
            keyEquivalent: ""
        )
        reveal.isEnabled = reachable
        menu.addItem(reveal)

        let copy = NSMenuItem(
            title: reachable ? "Copy Path" : "Copy Path (relative)",
            action: #selector(ContextMenuTarget.copyPath(_:)),
            keyEquivalent: ""
        )
        copy.isEnabled = !path.isEmpty
        menu.addItem(copy)

        menu.addItem(.separator())
        let trash = NSMenuItem(
            title: "Move to Trash…",
            action: #selector(ContextMenuTarget.trash(_:)),
            keyEquivalent: ""
        )
        trash.isEnabled = reachable
        menu.addItem(trash)

        let target = ContextMenuTarget(controller: self, relativePath: path, kind: kind)
        for item in menu.items where item.action != nil {
            item.target = target
        }
        // Retain the target for the lifetime of the menu.
        objc_setAssociatedObject(menu, &ContextMenuTarget.associationKey, target, .OBJC_ASSOCIATION_RETAIN)

        // Anchor at the cursor's screen location rather than at the
        // (x, y) the viz reported: NSEvent.mouseLocation gives global
        // screen coords and doesn't depend on whether the WebView's
        // origin moved between the JS event firing and Swift handling
        // it.
        let mouseScreenLocation = NSEvent.mouseLocation
        guard let window = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp.windows.first else {
            return
        }
        let pointInWindow = window.convertPoint(fromScreen: mouseScreenLocation)
        guard let contentView = window.contentView else { return }
        let pointInView = contentView.convert(pointInWindow, from: nil)
        let dummyEvent = NSEvent.mouseEvent(
            with: .rightMouseDown,
            location: pointInWindow,
            modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber,
            context: nil,
            eventNumber: 0,
            clickCount: 1,
            pressure: 0
        )
        if let event = dummyEvent {
            NSMenu.popUpContextMenu(menu, with: event, for: contentView)
        } else {
            // Fallback: anchor in the view directly. Slightly less
            // precise but always works.
            menu.popUp(positioning: nil, at: pointInView, in: contentView)
        }
        // Silence the unused-variable warning when the dummyEvent
        // path is taken.
        _ = viewportX
        _ = viewportY
    }

    // MARK: - Binary discovery

    private func locateScannerBinary() -> URL? {
        if let bundled = Bundle.main.url(forResource: "apfs-fastindex-scan", withExtension: nil) {
            return bundled
        }
        let fileManager = FileManager.default
        let candidates: [String] = [
            FileManager.default.currentDirectoryPath + "/../target/release/apfs-fastindex-scan",
            FileManager.default.currentDirectoryPath + "/target/release/apfs-fastindex-scan",
            "/Users/kai/Projects/apfs-fastindex/target/release/apfs-fastindex-scan",
        ]
        for path in candidates {
            let url = URL(fileURLWithPath: path).standardized
            if fileManager.isExecutableFile(atPath: url.path) {
                return url
            }
        }
        if let envPath = ProcessInfo.processInfo.environment["PATH"] {
            for component in envPath.split(separator: ":") {
                let candidate = URL(fileURLWithPath: String(component))
                    .appendingPathComponent("apfs-fastindex-scan")
                if fileManager.isExecutableFile(atPath: candidate.path) {
                    return candidate
                }
            }
        }
        return nil
    }
}

struct ProgressUpdate {
    let scanned: UInt64
    let skipped: UInt64
    let elapsedMs: UInt64
    let terminal: Bool
}

private func decodeProgress(_ line: Data) -> ProgressUpdate? {
    guard let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else {
        return nil
    }
    let scanned = (obj["scanned"] as? NSNumber)?.uint64Value ?? 0
    let skipped = (obj["skipped"] as? NSNumber)?.uint64Value ?? 0
    let elapsed = (obj["elapsed_ms"] as? NSNumber)?.uint64Value ?? 0
    let terminal = (obj["terminal"] as? Bool) ?? false
    return ProgressUpdate(scanned: scanned, skipped: skipped, elapsedMs: elapsed, terminal: terminal)
}

/// Thread-safe `Data` accumulator. Sendable so the
/// `FileHandle.readabilityHandler` and `Process.terminationHandler`
/// can pass it across queue boundaries without dragging actor
/// isolation along. Internal locking is a plain `NSLock` because
/// append + snapshot are both fast and we don't need queue-based
/// scheduling guarantees.
final class ScanBufferBox: @unchecked Sendable {
    private var data = Data()
    private let lock = NSLock()

    func append(_ chunk: Data) {
        lock.lock()
        data.append(chunk)
        lock.unlock()
    }

    func snapshot() -> Data {
        lock.lock()
        defer { lock.unlock() }
        return data
    }

    func clear() {
        lock.lock()
        data.removeAll(keepingCapacity: false)
        lock.unlock()
    }
}

/// `NSMenu` requires its action targets to be `NSObject` instances
/// reachable via `@objc` selectors. `ScanController` is an
/// `ObservableObject` (not an `NSObject`), so we route the four
/// menu actions through this tiny adapter. The adapter is associated
/// with the `NSMenu` (`objc_setAssociatedObject`) so it lives until
/// the menu dismisses, then is released along with it.
final class ContextMenuTarget: NSObject {
    /// Stable address used as the associated-object key.
    static var associationKey: UInt8 = 0

    weak var controller: ScanController?
    let relativePath: String
    let kind: String

    init(controller: ScanController, relativePath: String, kind: String) {
        self.controller = controller
        self.relativePath = relativePath
        self.kind = kind
    }

    @MainActor @objc func open(_ sender: Any?) {
        controller?.openInFinder(relativePath: relativePath)
    }
    @MainActor @objc func reveal(_ sender: Any?) {
        controller?.revealInFinder(relativePath: relativePath)
    }
    @MainActor @objc func copyPath(_ sender: Any?) {
        controller?.copyPathToPasteboard(relativePath: relativePath)
    }
    @MainActor @objc func trash(_ sender: Any?) {
        controller?.moveToTrash(relativePath: relativePath)
    }
}
