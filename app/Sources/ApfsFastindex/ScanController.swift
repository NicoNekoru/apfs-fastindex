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
/// the viz WKWebView. Phase 1 scope: launch the scanner, stream stderr
/// progress, hand the stdout JSON to the viz. Selection / context-menu
/// plumbing is reserved for Phase 2.
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

    /// Cached values pushed to the viz once the WebView reports it is
    /// ready. We may receive a scan before the viz finishes loading, so
    /// the bridge replays the latest cached scan + progress when it
    /// connects.
    private(set) var pendingScanJSON: String? = nil
    private(set) var pendingProgress: ProgressUpdate? = nil

    private weak var webView: WKWebView?
    private var scanProcess: Process?
    private var scanCancelled: Bool = false

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
        if let cached = pendingScanJSON {
            evaluateLoadScan(json: cached)
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
        pendingScanJSON = nil
        pendingProgress = nil
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
        scanProcess = process

        // Stream stderr progress on a background task; assemble stdout
        // on another. Both terminate when the pipes close.
        Task.detached { [weak self] in
            await self?.streamProgress(stderrPipe.fileHandleForReading)
        }
        Task.detached { [weak self] in
            await self?.collectStdout(stdoutPipe.fileHandleForReading, process: process)
        }

        do {
            try process.run()
        } catch {
            isScanning = false
            scanProcess = nil
            lastError = "failed to launch scanner: \(error.localizedDescription)"
        }
    }

    func cancelScan() {
        scanCancelled = true
        scanProcess?.terminate()
    }

    // MARK: - Subprocess plumbing

    private func streamProgress(_ handle: FileHandle) async {
        // Read progress lines one at a time; each is a JSON object terminated
        // by `\n`. We hand the parsed counters to the UI on the main actor.
        var buffer = Data()
        while true {
            let chunk: Data
            do {
                chunk = try handle.read(upToCount: 4096) ?? Data()
            } catch {
                break
            }
            if chunk.isEmpty { break }
            buffer.append(chunk)
            while let newline = buffer.firstIndex(of: 0x0A) {
                let line = buffer.subdata(in: 0..<newline)
                buffer.removeSubrange(0..<(newline + 1))
                if let update = decodeProgress(line) {
                    await MainActor.run {
                        self.scannedCount = update.scanned
                        self.skippedCount = update.skipped
                        self.elapsedMs = update.elapsedMs
                        self.pendingProgress = update
                        self.evaluateSetProgress(update)
                    }
                }
            }
        }
    }

    private func collectStdout(_ handle: FileHandle, process: Process) async {
        // The Rust binary emits one big JSON document. We accumulate it
        // and ship to the viz once the process exits.
        var stdout = Data()
        while true {
            let chunk: Data
            do {
                chunk = try handle.read(upToCount: 65_536) ?? Data()
            } catch { break }
            if chunk.isEmpty { break }
            stdout.append(chunk)
        }
        process.waitUntilExit()
        let cancelled = await MainActor.run { self.scanCancelled }
        let exitCode = process.terminationStatus

        let result = String(data: stdout, encoding: .utf8) ?? ""
        await MainActor.run {
            self.isScanning = false
            self.scanProcess = nil
            if cancelled {
                self.lastError = "scan cancelled"
                return
            }
            if exitCode != 0 {
                self.lastError = "scanner exited with status \(exitCode)"
                return
            }
            if let claim = self.extractCorrectnessClaim(result) {
                self.correctnessClaim = claim
            }
            self.pendingScanJSON = result
            self.evaluateLoadScan(json: result)
        }
    }

    private func extractCorrectnessClaim(_ json: String) -> String? {
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return nil
        }
        return obj["correctness_claim"] as? String
    }

    // MARK: - WKWebView calls

    private func evaluateLoadScan(json: String) {
        guard let webView else { return }
        // Pass through `JSON.parse` on the JS side so a 100+ MB payload
        // does not have to be re-stringified by us. The web side reads
        // the top-level doc the same way it does in the drag-drop flow.
        let escaped = jsStringLiteral(json)
        let js = "if (window.__apfs_ingest__) { __apfs_ingest__(\(escaped)); }"
        webView.evaluateJavaScript(js) { _, err in
            if let err = err {
                NSLog("loadScan JS error: \(err.localizedDescription)")
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

    private func jsStringLiteral(_ s: String) -> String {
        // Minimal JS-safe escape: wrap in JSON.stringify on the JS side by
        // using a JSON-quoted string. We re-encode as a JSON string so
        // backslashes, quotes, and control chars are preserved.
        if let data = try? JSONSerialization.data(withJSONObject: [s], options: []),
           let str = String(data: data, encoding: .utf8),
           str.count >= 2 {
            // Strip surrounding `[` / `]`
            return String(str.dropFirst().dropLast())
        }
        return "\"\""
    }

    // MARK: - Bridge inbound (Phase 2 will populate this)

    func handleBridgeMessage(_ message: BridgeMessage) {
        switch message {
        case .selected(let path, _, _):
            self.selectedPath = path
        case .contextMenu, .revealInFinder, .moveToTrash:
            // Reserved for Phase 2.
            break
        }
    }

    // MARK: - Binary discovery

    private func locateScannerBinary() -> URL? {
        // Search order:
        // 1. Bundled inside the .app at Contents/Resources/apfs-fastindex-scan.
        // 2. Sibling release build under the repo: target/release/apfs-fastindex-scan.
        // 3. PATH (`which apfs-fastindex-scan`).
        if let bundled = Bundle.main.url(forResource: "apfs-fastindex-scan", withExtension: nil) {
            return bundled
        }
        let fileManager = FileManager.default
        let candidates: [String] = [
            // From `swift run` cwd (typically app/), look up to find target/release.
            FileManager.default.currentDirectoryPath + "/../target/release/apfs-fastindex-scan",
            FileManager.default.currentDirectoryPath + "/target/release/apfs-fastindex-scan",
            // Hard-coded developer fallback for repos checked out at the standard path.
            "/Users/kai/Projects/apfs-fastindex/target/release/apfs-fastindex-scan",
        ]
        for path in candidates {
            let url = URL(fileURLWithPath: path).standardized
            if fileManager.isExecutableFile(atPath: url.path) {
                return url
            }
        }
        // Final fallback: PATH lookup.
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
