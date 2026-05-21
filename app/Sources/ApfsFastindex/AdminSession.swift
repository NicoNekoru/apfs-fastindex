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

        // Defence in depth: refuse control bytes in the path.
        // JSON serialisation handles them fine, but the helper
        // historically split on TAB and a user-influenced path
        // with embedded TAB shifted the parser (audit H1). The
        // protocol is JSON-line now so this is belt-and-suspenders;
        // we keep it because any path the user typed should be
        // a "normal" filesystem path with no control bytes.
        if path.contains(where: { $0 == "\0" || $0 == "\n" || $0 == "\r" || $0 == "\t" }) {
            return .failed(
                message: "Path contains a control character; refusing to send to helper.",
                stderr: ""
            )
        }

        // Serialise the stdin write + reply read against any
        // concurrent caller. There is only one helper, one
        // stdin, one stdout — concurrent requests must take
        // turns. Each `requestScan` call gets one full round-trip
        // (including all interleaved progress events) before
        // the next can proceed.
        let result = requestQueue.sync(execute: { () -> Outcome in
            guard let pipe = pipeHandle else {
                return .failed(
                    message: "Administrator session not active.",
                    stderr: ""
                )
            }

            // JSON-line protocol (audit C1+H1 fix). Helper
            // picks its own tempfile path via mkstemp and
            // reports it back in the `ok` reply; we never
            // tell the helper where to write.
            let scanCmd: [String: Any] = ["op": "scan", "path": path]
            guard let scanBytes = try? JSONSerialization.data(
                withJSONObject: scanCmd, options: []
            ) else {
                return .failed(message: "Could not encode scan command.", stderr: "")
            }
            do {
                try pipe.write(contentsOf: scanBytes)
                try pipe.write(contentsOf: Data([0x0a]))
            } catch {
                self.invalidate()
                return .failed(
                    message: "Could not send command to helper: "
                        + "\(error.localizedDescription).",
                    stderr: ""
                )
            }

            // Read replies until we hit a terminal event
            // (`ok` or `err`). Progress events flow inline on
            // the same stdout stream — no separate progress
            // file (audit C1: the old progress file lived at a
            // parent-supplied path and was the same LPE primitive
            // as the output file).
            var terminalOutPath: String? = nil
            var terminalErr: String? = nil
            readLoop: while true {
                let line = AdminSession.readLine(from: pipe)
                if line.isEmpty {
                    return .failed(
                        message: "Administrator helper closed the connection unexpectedly.",
                        stderr: ""
                    )
                }
                guard let lineData = line.data(using: .utf8),
                      let json = try? JSONSerialization.jsonObject(
                        with: lineData, options: []
                      ) as? [String: Any],
                      let event = json["event"] as? String
                else {
                    return .failed(
                        message: "Helper reply was not a JSON object: \(line.prefix(200))",
                        stderr: ""
                    )
                }
                switch event {
                case "progress":
                    if let onProgress = onProgress {
                        onProgress(Scan.ProgressSnapshot(
                            scanned: AdminSession.uint(json["scanned"]),
                            skipped: AdminSession.uint(json["skipped"]),
                            bytes: AdminSession.uint(json["bytes"]),
                            elapsedMs: AdminSession.uint(json["elapsed_ms"]),
                            terminal: (json["terminal"] as? Bool) ?? false
                        ))
                    }
                case "ok":
                    terminalOutPath = json["out_path"] as? String
                    break readLoop
                case "err":
                    terminalErr = (json["message"] as? String) ?? "unknown helper error"
                    break readLoop
                default:
                    // Forward-compat: ignore unknown event
                    // shapes so a future helper version can
                    // add events (e.g. "warning") without
                    // breaking this parent.
                    break
                }
            }

            if let err = terminalErr {
                return .failed(message: err, stderr: "")
            }
            guard let outPath = terminalOutPath, !outPath.isEmpty else {
                return .failed(
                    message: "Helper finished without an output path.",
                    stderr: ""
                )
            }

            // Audit H2: stat-verify the path is a root-owned
            // regular file (mode <= 0600) before reading. The
            // helper created it via mkstemp under sticky /tmp,
            // so a user-attacker can neither pre-place a
            // symlink at that exact random name nor swap the
            // file (sticky + root-owned blocks both). The
            // explicit check is defence-in-depth.
            if !AdminSession.verifyHelperOutputPath(outPath) {
                // Ask the helper to clean up before we bail;
                // best-effort.
                _ = AdminSession.sendRelease(pipe: pipe, paths: [outPath])
                return .failed(
                    message: "Helper output file failed ownership check; refusing to read.",
                    stderr: ""
                )
            }

            guard let scan = Scan.fromPrivilegedMsgpack(
                path: outPath, sourcePath: path
            ) else {
                let cause = PrivilegedScan.lastFfiError()
                _ = AdminSession.sendRelease(pipe: pipe, paths: [outPath])
                return .failed(
                    message: "Privileged scan finished but the result file "
                        + "couldn't be loaded: \(cause).",
                    stderr: ""
                )
            }

            // Tell the helper to unlink the tempfile. We can't
            // unlink it ourselves (sticky /tmp + root-owned).
            // Best-effort: if the helper hung up between scan-
            // and-release, the file leaks until the next quit.
            _ = AdminSession.sendRelease(pipe: pipe, paths: [outPath])
            return .ok(scan)
        })

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
        // JSON-line protocol: `{"op":"quit"}`. The helper
        // unlinks its remaining tempfiles, writes a `bye` reply,
        // and exits. We don't read the reply — a hung helper
        // shouldn't block app termination, and fclose below
        // tears down the socket so the helper EOFs on its
        // next read.
        if let quit = try? JSONSerialization.data(
            withJSONObject: ["op": "quit"], options: []
        ) {
            try? pipe.write(contentsOf: quit)
            try? pipe.write(contentsOf: Data([0x0a]))
        }
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

        // 5. Wait for the helper's ready handshake. The
        //    helper writes a JSON-line `{"event":"ready",
        //    "version":1}` on startup (audit C1+H1 fix
        //    changed the protocol). EOF here means the child
        //    crashed before the handshake.
        let firstLine = AdminSession.readLine(from: handle)
        if firstLine.isEmpty {
            invalidateLocked()
            return .failedToStart(
                "Privileged helper exited before the ready handshake."
            )
        }
        let parsed: [String: Any]? = firstLine.data(using: .utf8).flatMap {
            try? JSONSerialization.jsonObject(with: $0, options: []) as? [String: Any]
        }
        guard let json = parsed, (json["event"] as? String) == "ready" else {
            invalidateLocked()
            return .failedToStart(
                "Privileged helper sent unexpected handshake: \(firstLine.prefix(200))"
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
    ///
    /// **Cap at 64 KiB** (audit M5). A buggy/hostile helper
    /// emitting megabytes without a newline would otherwise
    /// OOM the parent. JSON-line replies are < 1 KiB in
    /// normal operation; 64 KiB is room enough for an
    /// extremely verbose `err.message` while still bounded.
    fileprivate static let readLineCapBytes = 64 * 1024

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
            if buf.count > AdminSession.readLineCapBytes {
                // Hostile / buggy helper. Drop the buffer
                // (don't return arbitrary helper bytes) and
                // return empty so the caller treats this as
                // a closed connection.
                return ""
            }
        }
        if let nlIndex = buf.firstIndex(of: 0x0A) {
            let line = buf.subdata(in: 0..<nlIndex)
            return String(data: line, encoding: .utf8) ?? ""
        }
        return String(data: buf, encoding: .utf8) ?? ""
    }

    /// Coerce a JSON value (Any?) to a UInt64. JSONSerialization
    /// returns NSNumber for integers; we widen via int64Value
    /// when possible, fall back to 0 on type mismatch.
    fileprivate static func uint(_ value: Any?) -> UInt64 {
        if let n = value as? NSNumber {
            // Negative values clamp to 0; the helper only
            // emits unsigned counts.
            return n.int64Value >= 0 ? UInt64(n.int64Value) : 0
        }
        if let s = value as? String, let parsed = UInt64(s) {
            return parsed
        }
        return 0
    }

    /// Audit H2 defence-in-depth: verify the helper's output
    /// path is a regular file owned by root with no other-
    /// write bits set, BEFORE we hand the path to the FFI for
    /// reading. The mkstemp-created file should always pass;
    /// any failure means either the helper misbehaved or an
    /// attacker swapped the file (very unlikely under sticky
    /// /tmp + root ownership, but cheap to verify).
    fileprivate static func verifyHelperOutputPath(_ path: String) -> Bool {
        var st = stat()
        // lstat — refuse symlinks outright. The helper used
        // mkstemp, which never creates a symlink, so this
        // also catches "user swapped the file for a symlink
        // between create and read" though that swap shouldn't
        // be possible (sticky /tmp).
        let rc = path.withCString { cPath in lstat(cPath, &st) }
        guard rc == 0 else { return false }
        // Regular file (not symlink, not dir, not device).
        guard (st.st_mode & S_IFMT) == S_IFREG else { return false }
        // Root-owned. The helper runs as uid 0 under
        // AuthorizationExecuteWithPrivileges; any mkstemp it
        // does should land with uid == 0.
        guard st.st_uid == 0 else { return false }
        // No world or group write. mkstemp creates mode 0600
        // (owner rw), so anything looser is suspicious.
        let badBits = mode_t(S_IWGRP | S_IWOTH)
        guard (st.st_mode & badBits) == 0 else { return false }
        return true
    }

    /// Best-effort: ask the helper to unlink one or more
    /// tempfiles it owns. Failures are silent — the helper
    /// also unlinks everything on `quit`, so a missed
    /// `release` just delays the cleanup to app exit.
    @discardableResult
    fileprivate static func sendRelease(pipe: FileHandle, paths: [String]) -> Bool {
        guard let cmd = try? JSONSerialization.data(
            withJSONObject: ["op": "release", "paths": paths], options: []
        ) else { return false }
        do {
            try pipe.write(contentsOf: cmd)
            try pipe.write(contentsOf: Data([0x0a]))
            // Drain the helper's ack so the next request
            // doesn't read a stale reply.
            _ = readLine(from: pipe)
            return true
        } catch {
            return false
        }
    }
}
