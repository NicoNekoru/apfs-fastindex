import Foundation

/// Long-lived privileged scan helper.
///
/// `osascript ... with administrator privileges` is the only
/// unprivileged-app affordance for spawning a root subprocess on
/// stock macOS without code signing / SMAppService. Every osascript
/// invocation pops its own auth dialog — there is no cache across
/// `osascript` runs. To make subsequent scans painless we spawn
/// **one** osascript that runs the bundled CLI in `--server` mode
/// and reuse its stdin/stdout for every subsequent scan request.
/// The auth dialog pops once at session start; from then on,
/// scans are direct stdin writes to a process that is already root.
///
/// Protocol matches `apfs-fastindex-scan --server` (see
/// `crates/apfs-fastindex/src/main.rs::run_server_mode`):
///
/// ```text
/// stdout: ready\t1\n                      (once, after auth)
/// stdin:  scan\t<path>\t<out>\t<prog>\n   (per scan request)
/// stdout: ok\t<exit_code>\n               (per scan)
/// stdin:  quit\n                          (graceful shutdown)
/// ```
///
/// `AdminSession.shared` is the singleton entry point. Lifecycle:
///
/// - `requestScan(path:onSessionReady:onProgress:)` spawns the
///   helper if not already running, waits for the `ready`
///   handshake (calls `onSessionReady`), writes the `scan` command,
///   polls the progress file, awaits the `ok` reply, rehydrates
///   the msgpack via `Scan.fromPrivilegedMsgpack`, returns the
///   outcome.
/// - `shutdown()` writes `quit`, waits for the helper to exit
///   (best-effort, capped at 1 s). Called from the app's
///   `applicationWillTerminate` hook.
final class AdminSession {
    static let shared = AdminSession()

    /// Result of a privileged scan request.
    enum Outcome {
        case ok(Scan)
        case cancelled
        case failed(message: String, stderr: String)
    }

    private let stateLock = NSLock()
    private var process: Process?
    private var stdinHandle: FileHandle?
    private var stdoutHandle: FileHandle?
    private var stderrHandle: FileHandle?
    /// Serialises scan requests on the helper's stdin. Two
    /// concurrent requests would interleave commands on the wire.
    private let requestQueue = DispatchQueue(label: "apfsfastindex.adminsession.requests")

    /// True iff a privileged helper is currently running and
    /// available to take requests. Reading is lock-free for the
    /// UI binding; writing happens inside `stateLock`.
    @MainActor private(set) var active: Bool = false

    private init() {}

    /// Run a scan under admin privileges. Synchronous on the
    /// calling thread; intended to be called from a background
    /// queue.
    ///
    /// `onSessionReady` fires on the calling queue the moment
    /// auth completes (after the `ready` handshake). Use it to
    /// flip the UI's `adminMode` flag immediately, before the
    /// first scan has finished — that's how the title bar
    /// updates right after the password prompt.
    ///
    /// `onProgress` is invoked from the progress poller's
    /// dispatch queue (not the main queue) for every JSON event
    /// the helper writes to its progress log. Marshal back to
    /// the main queue if you touch SwiftUI state.
    func requestScan(
        path: String,
        onSessionReady: (() -> Void)? = nil,
        onProgress: ((Scan.ProgressSnapshot) -> Void)? = nil
    ) -> Outcome {
        // Already-root short-circuit: the GUI process inherits
        // root privileges (e.g. via `sudo /path/to/binary` or a
        // future SMAppService helper), so the in-process walker
        // already sees every TCC-restricted path. No subprocess,
        // no auth prompt.
        if PrivilegedScan.alreadyRoot {
            onSessionReady?()
            DispatchQueue.main.async { self.active = true }
            let scan: Scan?
            if let onProgress {
                scan = Scan.fallbackWithProgress(path: path, onProgress: onProgress)
            } else {
                scan = Scan.fallbackAsAdministrator(path: path)
            }
            guard let scan else {
                let cause = PrivilegedScan.lastFfiError()
                return .failed(
                    message: "Administrator scan failed: \(cause).",
                    stderr: ""
                )
            }
            scan.isAdmin = true
            return .ok(scan)
        }

        // Ensure helper is up. If this is the first request,
        // ensure() spawns osascript, blocks on the `ready`
        // handshake, then returns. Subsequent requests reuse the
        // running helper.
        let ready: Bool
        switch ensure() {
        case .alreadyReady:
            // Helper was already running; auth was prompted on
            // an earlier request. `onSessionReady` still fires
            // because callers may want to update UI on every
            // entry (idempotent flips are cheap).
            ready = true
        case .freshlySpawned:
            ready = true
        case .userCancelled:
            return .cancelled
        case .failedToStart(let message):
            return .failed(message: message, stderr: "")
        }
        if ready {
            onSessionReady?()
            DispatchQueue.main.async { self.active = true }
        }

        // Each request gets its own pair of temp files. The
        // server protocol lets the parent name them, which means
        // we never see a stale msgpack from a previous request.
        let runId = "\(ProcessInfo.processInfo.processIdentifier)-\(UInt64(Date().timeIntervalSince1970 * 1000))"
        let tempDir = NSTemporaryDirectory()
        let outPath = (tempDir as NSString)
            .appendingPathComponent("apfs-fastindex-admin-out-\(runId).msgpack")
        let progressPath = (tempDir as NSString)
            .appendingPathComponent("apfs-fastindex-admin-progress-\(runId).log")
        defer {
            try? FileManager.default.removeItem(atPath: outPath)
            try? FileManager.default.removeItem(atPath: progressPath)
        }
        // Ensure the progress file exists so the poller's first
        // open() call doesn't no-op. The helper truncates on its
        // own write so a pre-existing empty file is fine.
        FileManager.default.createFile(atPath: progressPath, contents: nil)

        let pollStop = DispatchSemaphore(value: 0)
        let pollGroup = DispatchGroup()
        if let onProgress {
            pollGroup.enter()
            DispatchQueue.global(qos: .userInitiated).async {
                defer { pollGroup.leave() }
                PrivilegedScan.pollProgress(
                    file: progressPath,
                    stop: pollStop,
                    onProgress: onProgress
                )
            }
        }

        // Serialise the stdin write + reply read against any
        // concurrent caller. There is only one helper, one
        // stdin, one stdout — concurrent requests must take
        // turns. Each `requestScan` call gets one full round-trip
        // before the next can proceed.
        let result = requestQueue.sync(execute: { () -> Outcome in
            guard let stdin = stdinHandle, let stdout = stdoutHandle else {
                return .failed(
                    message: "Administrator session not active.",
                    stderr: ""
                )
            }
            let command = "scan\t\(path)\t\(outPath)\t\(progressPath)\n"
            guard let bytes = command.data(using: .utf8) else {
                return .failed(message: "Path is not valid UTF-8.", stderr: "")
            }
            do {
                try stdin.write(contentsOf: bytes)
            } catch {
                self.invalidate()
                return .failed(
                    message: "Could not send command to helper: "
                        + "\(error.localizedDescription).",
                    stderr: ""
                )
            }

            // Read one line of response. The protocol guarantees
            // exactly one line per scan; read until the first
            // newline.
            let line = AdminSession.readLine(from: stdout)
            return AdminSession.consumeOk(
                line: line,
                outPath: outPath
            )
        })

        pollStop.signal()
        pollGroup.wait()
        return result
    }

    /// Signal the helper to exit and clean up state. Best-effort;
    /// safe to call on app shutdown.
    func shutdown() {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard let stdin = stdinHandle, let process = process else {
            return
        }
        let quit = "quit\n".data(using: .utf8)!
        try? stdin.write(contentsOf: quit)
        try? stdin.close()
        // Cap waitUntilExit at ~1 s so a hung helper doesn't
        // freeze the app's terminate path. Termination on
        // timeout is fine — the helper is just a subprocess.
        let deadline = Date().addingTimeInterval(1.0)
        while process.isRunning && Date() < deadline {
            usleep(20_000)
        }
        if process.isRunning {
            process.terminate()
        }
        invalidate()
    }

    // ---- internal -------------------------------------------------- //

    private enum EnsureResult {
        case alreadyReady
        case freshlySpawned
        case userCancelled
        case failedToStart(String)
    }

    /// Ensure the helper process is up and has emitted the
    /// `ready\t1` handshake. Single-flighted via `stateLock`.
    private func ensure() -> EnsureResult {
        stateLock.lock()
        defer { stateLock.unlock() }
        if let p = process, p.isRunning, stdinHandle != nil, stdoutHandle != nil {
            return .alreadyReady
        }
        // Clear any stale handles from a previous helper that
        // died or was shut down.
        process = nil
        stdinHandle = nil
        stdoutHandle = nil
        stderrHandle = nil
        DispatchQueue.main.async { self.active = false }

        guard let cliURL = PrivilegedScan.bundledCliURL else {
            return .failedToStart(
                "apfs-fastindex-scan helper is missing from the app bundle. "
                    + "Rebuild with make-release.sh."
            )
        }
        let cliPath = cliURL.path

        // The shell command osascript executes under
        // administrator privileges. `exec` replaces the inner sh
        // with the CLI so we don't carry an idle parent process.
        // Single-quoting handles spaces in the CLI path.
        let shellCommand = "exec " + PrivilegedScan.shellQuote(cliPath) + " --server"
        let appleScriptCommand =
            "do shell script "
            + PrivilegedScan.appleScriptQuote(shellCommand)
            + " with administrator privileges"

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        process.arguments = ["-e", appleScriptCommand]
        let stdinPipe = Pipe()
        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardInput = stdinPipe
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe
        do {
            try process.run()
        } catch {
            return .failedToStart(
                "Could not start osascript: \(error.localizedDescription)"
            )
        }

        // Block on the `ready\t1` handshake. The osascript shows
        // its auth dialog synchronously; if the user cancels,
        // osascript exits non-zero and stdout closes — readLine
        // returns an empty string.
        let firstLine = AdminSession.readLine(from: stdoutPipe.fileHandleForReading)
        if firstLine.isEmpty {
            // Helper failed to start. Was it a user-cancel?
            process.waitUntilExit()
            let stderrData = stderrPipe.fileHandleForReading.readDataToEndOfFile()
            let stderrString = String(data: stderrData, encoding: .utf8) ?? ""
            if stderrString.contains("User canceled") || stderrString.contains("(-128)") {
                return .userCancelled
            }
            let trimmed = stderrString.trimmingCharacters(in: .whitespacesAndNewlines)
            let message = trimmed.isEmpty
                ? "Privileged helper exited before the ready handshake."
                : "Privileged helper failed: \(trimmed)"
            return .failedToStart(message)
        }
        if !firstLine.hasPrefix("ready") {
            process.terminate()
            return .failedToStart(
                "Privileged helper sent unexpected handshake: \(firstLine)"
            )
        }

        self.process = process
        self.stdinHandle = stdinPipe.fileHandleForWriting
        self.stdoutHandle = stdoutPipe.fileHandleForReading
        self.stderrHandle = stderrPipe.fileHandleForReading
        return .freshlySpawned
    }

    /// Forget the current helper process and close handles. Lock
    /// must be held by caller.
    private func invalidate() {
        try? stdinHandle?.close()
        try? stdoutHandle?.close()
        try? stderrHandle?.close()
        process = nil
        stdinHandle = nil
        stdoutHandle = nil
        stderrHandle = nil
        DispatchQueue.main.async { self.active = false }
    }

    /// Read bytes from `handle` until `\n` or EOF. Returns the
    /// line without the trailing newline. Empty string on EOF
    /// before any bytes — a signal that the helper died.
    fileprivate static func readLine(from handle: FileHandle) -> String {
        var buf = Data()
        while true {
            let next = handle.availableData
            if next.isEmpty {
                break
            }
            buf.append(next)
            if buf.contains(0x0A) {
                break
            }
        }
        if let nlIndex = buf.firstIndex(of: 0x0A) {
            let line = buf.subdata(in: 0..<nlIndex)
            return String(data: line, encoding: .utf8) ?? ""
        }
        return String(data: buf, encoding: .utf8) ?? ""
    }

    /// Parse the helper's per-scan reply line and turn it into
    /// an `Outcome`. The protocol's only success shape is
    /// `ok\t<exit>`; anything else is treated as a helper error.
    private static func consumeOk(line: String, outPath: String) -> Outcome {
        if line.isEmpty {
            return .failed(
                message: "Administrator helper closed the connection unexpectedly.",
                stderr: ""
            )
        }
        let parts = line.split(separator: "\t", maxSplits: 1, omittingEmptySubsequences: false)
        guard parts.count == 2, parts[0] == "ok" else {
            return .failed(
                message: "Administrator helper returned an error: \(line)",
                stderr: ""
            )
        }
        if let exit = Int(parts[1]), exit != 0 {
            return .failed(
                message: "Administrator scan exited with status \(exit).",
                stderr: ""
            )
        }
        guard let scan = Scan.fromPrivilegedMsgpack(path: outPath) else {
            let cause = PrivilegedScan.lastFfiError()
            return .failed(
                message: "Privileged scan finished but the result file "
                    + "couldn't be loaded: \(cause).",
                stderr: ""
            )
        }
        return .ok(scan)
    }
}
