import Foundation
import Security
import Darwin

/// Function-pointer signature for `AuthorizationExecuteWithPrivileges`
/// looked up via `dlsym` so we avoid the Swift deprecation warning
/// at the call site. The API itself is still loaded into every
/// macOS process via the Security framework — the deprecation is
/// purely an SDK-level annotation, not an unlinker. `dlsym` with
/// `RTLD_DEFAULT` returns the live address; the cast turns it
/// into a callable Swift function.
private typealias AuthorizationExecuteWithPrivilegesType = @convention(c) (
    AuthorizationRef,
    UnsafePointer<CChar>,
    AuthorizationFlags,
    UnsafePointer<UnsafeMutablePointer<CChar>?>,
    UnsafeMutablePointer<UnsafeMutablePointer<FILE>?>?
) -> OSStatus

/// Long-lived privileged scan helper.
///
/// The pattern is "spawn root once, reuse forever" so subsequent
/// scans don't re-prompt for auth. The previous attempt used
/// `osascript ... do shell script ... with administrator
/// privileges` but that path could not pipe stdin to the inner
/// command — `do shell script` connects the child's stdin to
/// `/dev/null`, so the helper would send its handshake and then
/// EOF on stdin before any scan command could be delivered.
///
/// This version uses Apple's `AuthorizationServices`:
///
/// 1. `AuthorizationCreate` opens a session.
/// 2. `AuthorizationCopyRights` with `kAuthorizationRightExecute`
///    pops the auth dialog (the only prompt for the whole app
///    lifetime).
/// 3. `AuthorizationExecuteWithPrivileges` spawns the bundled
///    CLI with `--server`, returning a bidirectional FILE* whose
///    socketpair half is dup'd onto the child's stdin and stdout.
/// 4. Wrap the FILE* in a `FileHandle` and reuse it for every
///    scan request.
///
/// `AuthorizationExecuteWithPrivileges` is marked deprecated since
/// 10.7. The compiler warns; the API still works on every shipping
/// macOS and is the practical path for an unsigned helper. Apps
/// that ship under Developer ID can graduate to SMAppService.
///
/// Protocol matches `apfs-fastindex-scan --server`:
///
/// ```text
/// pipe-out (child stdout):  ready\t1\n                  (once, after auth)
/// pipe-in  (child stdin):   scan\t<path>\t<out>\t<prog>\n
/// pipe-out (child stdout):  ok\t<exit_code>\n
/// pipe-in  (child stdin):   quit\n
/// ```
final class AdminSession {
    static let shared = AdminSession()

    /// Result of a privileged scan request.
    enum Outcome {
        case ok(Scan)
        case cancelled
        case failed(message: String, stderr: String)
    }

    private let stateLock = NSLock()
    private var authRef: AuthorizationRef?
    /// Bidirectional FILE* from AuthorizationExecuteWithPrivileges,
    /// wrapped via `fileno()` + `FileHandle(fileDescriptor:)`.
    /// Reads come from the child's stdout, writes go to the
    /// child's stdin.
    private var pipeHandle: FileHandle?
    private var pipeFilePtr: UnsafeMutablePointer<FILE>?
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
            guard let pipe = pipeHandle else {
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
                try pipe.write(contentsOf: bytes)
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
            let line = AdminSession.readLine(from: pipe)
            return AdminSession.consumeOk(
                line: line,
                outPath: outPath,
                sourcePath: path
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
        guard let pipe = pipeHandle else {
            return
        }
        // Send `quit\n`; the helper writes `ok\t0\n` and exits.
        // We don't read the reply — by the time shutdown is
        // called we don't care about the response value, and a
        // hung helper shouldn't block app termination. fclose
        // below flushes our side and tears down the socket; the
        // helper EOFs on read and exits within milliseconds.
        let quit = "quit\n".data(using: .utf8)!
        try? pipe.write(contentsOf: quit)
        invalidateLocked()
    }

    // ---- internal -------------------------------------------------- //

    private enum EnsureResult {
        case alreadyReady
        case freshlySpawned
        case userCancelled
        case failedToStart(String)
    }

    /// Ensure the privileged helper is up and the bidirectional
    /// pipe is open. Single-flighted via `stateLock`.
    ///
    /// On the first call: opens an Authorization session, prompts
    /// the user once via `AuthorizationCopyRights`, then runs the
    /// CLI as root via `AuthorizationExecuteWithPrivileges`. The
    /// returned FILE* is the helper's bidirectional pipe — reads
    /// from it pull the helper's stdout, writes push to its stdin.
    private func ensure() -> EnsureResult {
        stateLock.lock()
        defer { stateLock.unlock() }
        if pipeHandle != nil {
            return .alreadyReady
        }
        invalidateLocked()

        guard let cliURL = PrivilegedScan.bundledCliURL else {
            return .failedToStart(
                "apfs-fastindex-scan helper is missing from the app bundle. "
                    + "Rebuild with make-release.sh."
            )
        }
        let cliPath = cliURL.path

        // 1. Open the authorization session.
        var authRef: AuthorizationRef?
        let createStatus = AuthorizationCreate(nil, nil, [], &authRef)
        guard createStatus == errAuthorizationSuccess, let auth = authRef else {
            return .failedToStart(
                "AuthorizationCreate failed (status \(createStatus))."
            )
        }

        // 2. Request the execute-with-privileges right. This pops
        //    the auth dialog. The result captures whether the
        //    user authenticated, cancelled, or otherwise denied.
        let rightName = (kAuthorizationRightExecute as NSString).utf8String!
        var item = AuthorizationItem(
            name: rightName,
            valueLength: 0,
            value: nil,
            flags: 0
        )
        let copyStatus: OSStatus = withUnsafeMutablePointer(to: &item) { itemPtr in
            var rights = AuthorizationRights(count: 1, items: itemPtr)
            let flags: AuthorizationFlags = [
                .interactionAllowed,
                .preAuthorize,
                .extendRights,
            ]
            return AuthorizationCopyRights(auth, &rights, nil, flags, nil)
        }
        if copyStatus == errAuthorizationCanceled {
            AuthorizationFree(auth, [])
            return .userCancelled
        }
        guard copyStatus == errAuthorizationSuccess else {
            AuthorizationFree(auth, [])
            return .failedToStart(
                "AuthorizationCopyRights failed (status \(copyStatus))."
            )
        }

        // 3. Spawn the CLI with --server as root. The
        //    communicationsPipe is dup'd onto the child's stdin
        //    and stdout — bidirectional. The arg array is C
        //    NULL-terminated; argv[0] is implicit (the path).
        let argv: [UnsafeMutablePointer<CChar>?] = [
            strdup("--server"),
            nil,
        ]
        defer {
            for arg in argv where arg != nil {
                free(arg)
            }
        }
        // Look up AuthorizationExecuteWithPrivileges dynamically
        // to suppress the macOS-10.7-deprecation warning that
        // would otherwise fire at the static call site. The
        // modern replacement (SMAppService) requires Developer
        // ID signing which the open-source build does not have;
        // AuthorizationExecuteWithPrivileges is still loaded
        // into every macOS process via the Security framework.
        let rtldDefault = UnsafeMutableRawPointer(bitPattern: -2)
        guard let symbol = dlsym(rtldDefault, "AuthorizationExecuteWithPrivileges") else {
            AuthorizationFree(auth, [])
            return .failedToStart(
                "AuthorizationExecuteWithPrivileges is not available on this macOS. "
                    + "The 'Scan as Administrator…' path requires it."
            )
        }
        let executeWithPrivileges = unsafeBitCast(
            symbol,
            to: AuthorizationExecuteWithPrivilegesType.self
        )
        var pipePtr: UnsafeMutablePointer<FILE>?
        let execStatus = argv.withUnsafeBufferPointer { buf -> OSStatus in
            cliPath.withCString { cPath -> OSStatus in
                executeWithPrivileges(auth, cPath, [], buf.baseAddress!, &pipePtr)
            }
        }
        guard execStatus == errAuthorizationSuccess, let pipe = pipePtr else {
            AuthorizationFree(auth, [])
            return .failedToStart(
                "AuthorizationExecuteWithPrivileges failed (status \(execStatus))."
            )
        }

        // 4. Wrap the FILE* in a FileHandle for the existing
        //    read/write code paths. closeOnDealloc=false because
        //    we own the FILE* and need to fclose it ourselves to
        //    flush + release the underlying socket.
        let fd = fileno(pipe)
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: false)

        self.authRef = auth
        self.pipeFilePtr = pipe
        self.pipeHandle = handle

        // 5. Wait for the helper's `ready\t1` handshake. The
        //    helper writes this and flushes immediately on
        //    startup. If we see EOF instead (empty line), the
        //    child crashed before the handshake.
        let firstLine = AdminSession.readLine(from: handle)
        if firstLine.isEmpty {
            invalidateLocked()
            return .failedToStart(
                "Privileged helper exited before the ready handshake."
            )
        }
        if !firstLine.hasPrefix("ready") {
            invalidateLocked()
            return .failedToStart(
                "Privileged helper sent unexpected handshake: \(firstLine)"
            )
        }

        return .freshlySpawned
    }

    /// Forget the current helper process and close handles. Lock
    /// must be held by caller.
    private func invalidate() {
        invalidateLocked()
    }

    private func invalidateLocked() {
        if let pipe = pipeFilePtr {
            fclose(pipe)
        }
        if let auth = authRef {
            // .destroyRights tears down the cached auth so the
            // next ensure() prompts again. Pass [] if you want
            // the session to remain valid for the OS auth
            // cache's natural ~5 min lifetime. For our purposes
            // we want explicit re-prompt on session loss, so
            // destroy.
            AuthorizationFree(auth, [.destroyRights])
        }
        authRef = nil
        pipeFilePtr = nil
        pipeHandle = nil
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
    private static func consumeOk(line: String, outPath: String, sourcePath: String) -> Outcome {
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
        guard let scan = Scan.fromPrivilegedMsgpack(path: outPath, sourcePath: sourcePath) else {
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
