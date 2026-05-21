import Foundation
import CApfsFastindex

extension Notification.Name {
    /// Posted by the File > Scan as Administrator… menu command.
    /// The active `NativeContentView` observes this and kicks off
    /// `startPrivilegedScan()` with the current `pathInput`.
    /// No payload — the path lives in SwiftUI state, not on the
    /// notification.
    static let scanAsAdministratorRequested = Notification.Name(
        "apfsfastindex.scanAsAdministratorRequested"
    )

    /// Posted by Edit > Find (⌘F) so the active
    /// `NativeContentView` can focus its tree-list search
    /// field. Same notification pattern as
    /// `scanAsAdministratorRequested`; no payload.
    static let findRequested = Notification.Name(
        "apfsfastindex.findRequested"
    )
}

/// "Scan as Administrator…" privileged-subprocess flow.
///
/// The product question this answers: the in-process fallback walker
/// hits `EACCES` on TCC-restricted user-data paths (`~/Library/Mail`,
/// `~/Library/Messages`, Safari/Mail/Calendar databases, etc.). Most
/// of those entries make it into `walk_skips` with reason
/// `permission_denied`, and the user sees "nothing" for those
/// subtrees. EX-28 closed the question of whether `sudo` unlocks raw
/// fast-path mode on live disks (it doesn't — kernel returns
/// `EPERM`), but `sudo` on the fallback walker still bypasses TCC
/// and surfaces those paths.
///
/// Implementation:
///
/// 1. Locate the CLI binary inside the .app bundle
///    (`Bundle.main.url(forAuxiliaryExecutable:)`). `make-release.sh`
///    copies it to `Contents/MacOS/apfs-fastindex-scan` alongside the
///    GUI binary.
/// 2. Pick a temp output path for the msgpack blob.
/// 3. Compose a one-line shell command:
///    `<cli> --format msgpack <user-path> > <temp-out>`.
/// 4. Run that command via `osascript` with `do shell script ... with
///    administrator privileges`. macOS pops the auth dialog; if the
///    user cancels, osascript exits non-zero and we return
///    `.cancelled`.
/// 5. On success, hand the temp file to `Scan.fromPrivilegedMsgpack`,
///    which calls the new Rust FFI `apfs_scan_from_msgpack_file`.
/// 6. Always remove the temp file, even on failure.
///
/// macOS-paths-with-quotes: every component (CLI path, user path,
/// temp output path) is double-quoted in the shell command. Each
/// quote-relevant character is escaped with `\` so a user-supplied
/// directory named `Documents/with"weird/name` doesn't break the
/// command. The `osascript` string itself is also escaped, since
/// osascript runs through its own AppleScript parser.
enum PrivilegedScan {
    /// Result of an attempted privileged scan.
    enum Outcome {
        /// The subprocess succeeded and rehydration produced a Scan.
        case ok(Scan)
        /// The user cancelled the macOS authentication prompt
        /// (or hit Escape, etc.). Quiet UX — no error popup.
        case cancelled
        /// The subprocess ran but reported an error. `message`
        /// is suitable for an error popup; `stderr` carries the
        /// subprocess's full stderr if any.
        case failed(message: String, stderr: String)
    }

    /// The CLI's filesystem path. Tries a series of lookups so
    /// the menu works in:
    ///
    /// 1. A production `.app` bundle (from `make-release.sh`) —
    ///    the CLI lives at `Contents/MacOS/apfs-fastindex-scan`.
    /// 2. A dev SwiftPM run (`swift run` from `app/`) — the
    ///    binary is at `.build/<triple>/debug/ApfsFastindex` and
    ///    there is no bundle; we walk up to the repo root and
    ///    use `target/<profile>/apfs-fastindex-scan`.
    /// 3. Anything on the user's `PATH` (last-resort —
    ///    `cargo install`-style setups).
    ///
    /// Every candidate is `fileExists`-verified so we never hand
    /// a phantom path to osascript.
    static var bundledCliURL: URL? {
        let fm = FileManager.default

        // 1. Bundle's auxiliary-executable lookup (production
        //    `.app`). Foundation looks in Contents/MacOS/.
        if let url = Bundle.main.url(forAuxiliaryExecutable: "apfs-fastindex-scan"),
           fm.fileExists(atPath: url.path) {
            return url
        }

        // 1a. Explicit Contents/MacOS/ fallback in case the
        //     auxiliary lookup misses despite the file existing.
        let bundleSibling = Bundle.main.bundleURL
            .appendingPathComponent("Contents/MacOS/apfs-fastindex-scan")
        if fm.fileExists(atPath: bundleSibling.path) {
            return bundleSibling
        }

        // 2. Dev-mode walk-up-to-Cargo.toml fallback. Audit M1:
        //    the walk runs `target/<profile>/apfs-fastindex-scan`
        //    as the privileged helper. A dev who launches the
        //    app from a directory whose ancestors contain an
        //    attacker-controlled Cargo.toml ships the
        //    attacker's `target/release/apfs-fastindex-scan` to
        //    root on the next "Scan as Administrator…". Gated
        //    behind DEBUG so release builds never enter this
        //    code path; release builds rely on the bundle
        //    siblings (steps 1, 1a) being correctly populated
        //    by `make-release.sh`.
        #if DEBUG
        if let exec = Bundle.main.executableURL {
            var dir: URL? = exec.deletingLastPathComponent()
            for _ in 0..<8 {
                guard let here = dir else { break }
                let cargo = here.appendingPathComponent("Cargo.toml")
                if fm.fileExists(atPath: cargo.path) {
                    for profile in ["release", "debug"] {
                        let candidate = here
                            .appendingPathComponent("target")
                            .appendingPathComponent(profile)
                            .appendingPathComponent("apfs-fastindex-scan")
                        if fm.fileExists(atPath: candidate.path) {
                            return candidate
                        }
                    }
                    break
                }
                let parent = here.deletingLastPathComponent()
                if parent.path == here.path { break }
                dir = parent
            }
        }
        #endif

        // 3. Anything on PATH. Audit M1: same threat model as
        //    step 2 — running root code from a user-controlled
        //    PATH entry. Gated behind DEBUG so release builds
        //    never resolve the helper via `which`.
        #if DEBUG
        let which = Process()
        which.executableURL = URL(fileURLWithPath: "/usr/bin/which")
        which.arguments = ["apfs-fastindex-scan"]
        let pipe = Pipe()
        which.standardOutput = pipe
        which.standardError = Pipe()
        if (try? which.run()) != nil {
            which.waitUntilExit()
            if which.terminationStatus == 0 {
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                if let s = String(data: data, encoding: .utf8) {
                    let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
                    if !trimmed.isEmpty && fm.fileExists(atPath: trimmed) {
                        return URL(fileURLWithPath: trimmed)
                    }
                }
            }
        }
        #endif

        return nil
    }

    /// True iff the GUI process is already running with EUID 0.
    /// In that case the in-process fallback walker already sees
    /// every TCC-restricted path (the kernel checks EUID, not
    /// whether the call came from a freshly-spawned subprocess),
    /// so the osascript escalation prompt is unnecessary.
    /// Typical paths to this state: `sudo open …` rarely works
    /// for sandboxed GUI apps but `sudo
    /// /path/to/App.app/Contents/MacOS/ApfsFastindex` does, and a
    /// future SMAppService helper would inherit it too.
    static var alreadyRoot: Bool {
        geteuid() == 0
    }

    /// Synchronous; intended to be called from a background queue.
    ///
    /// Thin delegate to `AdminSession.shared.requestScan`. The
    /// session manages the long-lived osascript-under-admin
    /// helper so successive scans don't re-prompt for auth.
    ///
    /// `onSessionReady` fires the moment auth completes (after
    /// the helper sends its `ready\t1` handshake) — wire this to
    /// flip the UI's admin-mode flag immediately so the title
    /// bar updates before the scan finishes.
    ///
    /// `onProgress` follows the same contract as
    /// `Scan.fallbackWithProgress.onProgress` — invoked from a
    /// background thread, marshal to main if you touch SwiftUI.
    static func run(
        path: String,
        onSessionReady: (() -> Void)? = nil,
        onProgress: ((Scan.ProgressSnapshot) -> Void)? = nil
    ) -> Outcome {
        let outcome = AdminSession.shared.requestScan(
            path: path,
            onSessionReady: onSessionReady,
            onProgress: onProgress
        )
        switch outcome {
        case .ok(let scan):
            return .ok(scan)
        case .cancelled:
            return .cancelled
        case .failed(let message, let stderr):
            return .failed(message: message, stderr: stderr)
        }
    }

    // ---- progress polling -------------------------------------- //

    /// Poll `file` every 250ms, parsing newline-delimited JSON
    /// events written by `apfs-fastindex-scan --progress`. Each
    /// event has the shape
    /// `{"scanned":N,"skipped":N,"bytes":N,"elapsed_ms":N,"terminal":true|false}`.
    /// Returns when `stop` is signalled. `internal` so the
    /// long-lived `AdminSession` helper can reuse it.
    static func pollProgress(
        file: String,
        stop: DispatchSemaphore,
        onProgress: @escaping (Scan.ProgressSnapshot) -> Void
    ) {
        var offset: UInt64 = 0
        var pendingLine = Data()
        while stop.wait(timeout: .now() + .milliseconds(250)) == .timedOut {
            drainProgressFile(
                file: file,
                offset: &offset,
                pendingLine: &pendingLine,
                onProgress: onProgress
            )
        }
        // Final drain after the subprocess exits — captures the
        // terminal event the CLI wrote just before exit.
        drainProgressFile(
            file: file,
            offset: &offset,
            pendingLine: &pendingLine,
            onProgress: onProgress
        )
    }

    private static func drainProgressFile(
        file: String,
        offset: inout UInt64,
        pendingLine: inout Data,
        onProgress: (Scan.ProgressSnapshot) -> Void
    ) {
        guard let handle = FileHandle(forReadingAtPath: file) else {
            return
        }
        defer { try? handle.close() }
        do {
            try handle.seek(toOffset: offset)
        } catch {
            return
        }
        let data: Data
        if #available(macOS 10.15.4, *) {
            data = (try? handle.readToEnd()) ?? Data()
        } else {
            data = handle.readDataToEndOfFile()
        }
        if data.isEmpty {
            return
        }
        offset += UInt64(data.count)
        pendingLine.append(data)
        // Split on '\n'. Last fragment without a trailing newline
        // stays in pendingLine until the next drain.
        while let newlineIndex = pendingLine.firstIndex(of: 0x0A) {
            let lineData = pendingLine.subdata(in: 0..<newlineIndex)
            pendingLine.removeSubrange(0...newlineIndex)
            guard let snapshot = parseProgressLine(lineData) else { continue }
            onProgress(snapshot)
        }
    }

    private static func parseProgressLine(_ data: Data) -> Scan.ProgressSnapshot? {
        guard !data.isEmpty else { return nil }
        guard let json = try? JSONSerialization.jsonObject(with: data, options: []) as? [String: Any] else {
            return nil
        }
        // The CLI's --progress shape is documented in
        // `crates/apfs-fastindex/src/main.rs`:
        //   {"scanned":N,"skipped":N,"bytes":N,"elapsed_ms":N,"terminal":<bool>}
        let scanned = (json["scanned"] as? NSNumber)?.uint64Value ?? 0
        let skipped = (json["skipped"] as? NSNumber)?.uint64Value ?? 0
        let bytes = (json["bytes"] as? NSNumber)?.uint64Value ?? 0
        let elapsedMs = (json["elapsed_ms"] as? NSNumber)?.uint64Value ?? 0
        let terminal = (json["terminal"] as? Bool) ?? false
        return Scan.ProgressSnapshot(
            scanned: scanned,
            skipped: skipped,
            bytes: bytes,
            elapsedMs: elapsedMs,
            terminal: terminal
        )
    }

    // ---- helpers ------------------------------------------------- //

    /// Wrap a string in single quotes for /bin/sh. Embedded
    /// single-quotes are escaped via `'\''` (standard sh idiom).
    /// Everything else inside single quotes is literal — no $, no
    /// backtick, no \ interpolation. `internal` so the long-lived
    /// `AdminSession` helper can reuse it.
    static func shellQuote(_ s: String) -> String {
        let escaped = s.replacingOccurrences(of: "'", with: "'\\''")
        return "'\(escaped)'"
    }

    /// Wrap a string as an AppleScript double-quoted literal.
    /// Inside double-quotes, only `"` and `\` need escaping.
    static func appleScriptQuote(_ s: String) -> String {
        let escaped = s
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        return "\"\(escaped)\""
    }

    /// Pull the most recent FFI error message off the thread-local
    /// slot the Rust diag module populates. Returns "unknown" if
    /// no error was recorded.
    static func lastFfiError() -> String {
        let ptr = apfs_last_error()
        guard let ptr else { return "unknown" }
        let cstr = String(cString: ptr)
        return cstr.isEmpty ? "unknown" : cstr
    }
}
