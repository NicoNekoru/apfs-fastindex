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
    @Published var scannedCount: UInt64 = 0
    @Published var skippedCount: UInt64 = 0
    @Published var elapsedMs: UInt64 = 0
    @Published var lastError: String? = nil
    @Published var selectedPath: String = ""
    @Published var correctnessClaim: String = ""

    /// Latest scan-result URL written to disk so the viz can load it via
    /// XHR. We retain it for the lifetime of the scan + page render; the
    /// next scan or app exit cleans it up.
    private(set) var pendingScanFileURL: URL? = nil
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

    func bindWebView(_ webView: WKWebView) {
        self.webView = webView
        // The Coordinator we registered in `makeNSView` is both the
        // navigation delegate and the URL-scheme handler for
        // `apfs-scan://`. Grab the reference so we can push the latest
        // scan-result file URL onto it directly without going through
        // the SwiftUI re-render path.
        self.vizCoordinator = webView.navigationDelegate as? VizWebView.Coordinator
        if let url = pendingScanFileURL {
            evaluateLoadScanFromFile(url)
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
        pendingProgress = nil
        // Discard any previous temp scan file before starting a new one.
        cleanupLastTempScan()
        stdoutBox.clear()
        stderrBuffer.removeAll(keepingCapacity: false)
        isScanning = true
        scanCancelled = false

        var args: [String] = ["--slim", "--progress"]
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
            }
        }
    }

    private func finishScan(process: Process, stdout: Data) {
        isScanning = false
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

        // Pull the small `correctness_claim` field off the raw bytes
        // before we ship the rest of the blob to disk. Doing this on
        // main is fine: it reads only the first ~4 KB.
        if let claim = Self.extractCorrectnessClaim(stdout) {
            correctnessClaim = claim
        }

        // The scan JSON can be hundreds of MB. We write it to a temp
        // file on a background queue and hand the path to the viz; the
        // viz fetches via XHR (parsed in the WebKit content process)
        // instead of eating a giant string interpolation + IPC on the
        // main thread.
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("apfs-scan-\(UUID().uuidString).json")
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            do {
                try stdout.write(to: url, options: .atomic)
                DispatchQueue.main.async {
                    self?.pendingScanFileURL = url
                    self?.lastTempScanURL = url
                    self?.evaluateLoadScanFromFile(url)
                }
            } catch {
                DispatchQueue.main.async {
                    self?.lastError = "failed to write scan to disk: \(error.localizedDescription)"
                }
            }
        }
    }

    private func cleanupLastTempScan() {
        if let url = lastTempScanURL {
            try? FileManager.default.removeItem(at: url)
            lastTempScanURL = nil
            pendingScanFileURL = nil
        }
    }

    // MARK: - WKWebView calls

    private func evaluateLoadScanFromFile(_ url: URL) {
        guard let webView else { return }
        // **Order matters.** The Coordinator (URL-scheme handler) must
        // know about this scan file *before* the JS XHR fires; if we
        // relied on SwiftUI's `updateNSView` to propagate the URL,
        // `evaluateJavaScript` would beat the re-render and the XHR
        // would 404. Set it directly on the coordinator first.
        vizCoordinator?.currentScanFileURL = url

        // The path argument is purely for diagnostics — the JS shim
        // always fetches `apfs-scan://current` regardless of what we
        // pass. Escape backslashes and single-quotes defensively.
        let escaped = url.path
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "'", with: "\\'")
        let js = "if (window.__apfs_ingest_file__) __apfs_ingest_file__('\(escaped)');"
        webView.evaluateJavaScript(js) { _, err in
            if let err = err {
                NSLog("evaluateLoadScanFromFile JS error: \(err.localizedDescription)")
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

    // MARK: - Bridge inbound (Phase 2 will populate this)

    func handleBridgeMessage(_ message: BridgeMessage) {
        switch message {
        case .selected(let path, _, _):
            self.selectedPath = path
        case .contextMenu, .revealInFinder, .moveToTrash:
            // Reserved for Phase 2.
            break
        case .consoleError(let message):
            // Surface viz-side failures in the host log. Without this
            // the page can silently swallow exceptions and we have no
            // signal that ingest broke.
            NSLog("[viz] console.error: %@", message)
        case .ingestStarted:
            NSLog("[viz] ingest started")
        case .ingestSucceeded(let root, let total):
            NSLog("[viz] ingest ok: root=%@ entries=%llu", root, total)
        case .ingestFailed(let message):
            NSLog("[viz] ingest failed: %@", message)
            self.lastError = "viz failed to load scan: \(message)"
        }
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
