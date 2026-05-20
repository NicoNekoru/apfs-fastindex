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

    /// The bundled CLI's filesystem path. Returns `nil` if the
    /// build pipeline didn't ship it — in which case "Scan as
    /// Administrator…" should be disabled or hidden in the UI.
    static var bundledCliURL: URL? {
        Bundle.main.url(forAuxiliaryExecutable: "apfs-fastindex-scan")
    }

    /// Synchronous; intended to be called from a background queue.
    /// Pops the macOS auth dialog (modal); the calling thread blocks
    /// until the subprocess exits.
    static func run(path: String) -> Outcome {
        guard let cliURL = bundledCliURL else {
            return .failed(
                message: "apfs-fastindex-scan helper is missing from the app bundle. "
                    + "Rebuild with make-release.sh.",
                stderr: ""
            )
        }
        let cliPath = cliURL.path

        let tempDir = NSTemporaryDirectory()
        let tempName = "apfs-fastindex-admin-scan-\(ProcessInfo.processInfo.processIdentifier)-\(UInt64(Date().timeIntervalSince1970 * 1000)).msgpack"
        let tempOut = (tempDir as NSString).appendingPathComponent(tempName)
        defer {
            // Best-effort cleanup. Leave the file in place if
            // removal fails (no point surfacing IO errors here);
            // the OS's temp-cleaner will catch it on next reboot.
            try? FileManager.default.removeItem(atPath: tempOut)
        }

        // Build the shell command. Every path is single-quoted
        // with embedded single-quotes escaped via the standard
        // sh `'\''` pattern. That handles spaces, $, backticks,
        // doubles quotes, and the like uniformly.
        let shellCommand =
            shellQuote(cliPath)
            + " --format msgpack "
            + shellQuote(path)
            + " > "
            + shellQuote(tempOut)

        // Now wrap the shell command for AppleScript. AppleScript
        // string literals use double-quotes; inside, " and \ are
        // the only metacharacters we need to escape.
        let appleScriptCommand =
            "do shell script "
            + appleScriptQuote(shellCommand)
            + " with administrator privileges"

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        process.arguments = ["-e", appleScriptCommand]
        let stderrPipe = Pipe()
        process.standardError = stderrPipe
        // Discard osascript's stdout — it's just the (empty)
        // result of the do-shell-script call when we redirect the
        // CLI's stdout to a temp file.
        process.standardOutput = Pipe()

        do {
            try process.run()
        } catch {
            return .failed(
                message: "Could not start osascript: \(error.localizedDescription)",
                stderr: ""
            )
        }
        process.waitUntilExit()
        let stderrData = stderrPipe.fileHandleForReading.readDataToEndOfFile()
        let stderrString = String(data: stderrData, encoding: .utf8) ?? ""

        if process.terminationStatus != 0 {
            // AppleScript returns -128 when the user cancels the
            // authentication prompt. osascript surfaces that as a
            // non-zero exit with a recognisable stderr message.
            if stderrString.contains("User canceled") || stderrString.contains("(-128)") {
                return .cancelled
            }
            let trimmed = stderrString.trimmingCharacters(in: .whitespacesAndNewlines)
            let message = trimmed.isEmpty
                ? "Privileged scan exited with status \(process.terminationStatus)."
                : "Privileged scan failed: \(trimmed)"
            return .failed(message: message, stderr: stderrString)
        }

        // The CLI exited 0 — temp file should be a valid msgpack
        // blob. Hand it to Scan.fromPrivilegedMsgpack.
        guard let scan = Scan.fromPrivilegedMsgpack(path: tempOut) else {
            let cause = lastFfiError()
            let message = "Privileged scan finished but the result file "
                + "couldn't be loaded: \(cause)."
            return .failed(message: message, stderr: stderrString)
        }
        return .ok(scan)
    }

    // ---- helpers ------------------------------------------------- //

    /// Wrap a string in single quotes for /bin/sh. Embedded
    /// single-quotes are escaped via `'\''` (standard sh idiom).
    /// Everything else inside single quotes is literal — no $, no
    /// backtick, no \ interpolation.
    private static func shellQuote(_ s: String) -> String {
        let escaped = s.replacingOccurrences(of: "'", with: "'\\''")
        return "'\(escaped)'"
    }

    /// Wrap a string as an AppleScript double-quoted literal.
    /// Inside double-quotes, only `"` and `\` need escaping.
    private static func appleScriptQuote(_ s: String) -> String {
        let escaped = s
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        return "\"\(escaped)\""
    }

    /// Pull the most recent FFI error message off the thread-local
    /// slot the Rust diag module populates. Returns "unknown" if
    /// no error was recorded.
    private static func lastFfiError() -> String {
        let ptr = apfs_last_error()
        guard let ptr else { return "unknown" }
        let cstr = String(cString: ptr)
        return cstr.isEmpty ? "unknown" : cstr
    }
}
