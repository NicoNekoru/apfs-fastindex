import AppKit
import Foundation

/// Best-effort Full Disk Access detection and Settings navigation.
/// Permission must be granted by the user, never by the app.
enum TCCAccess {
    /// Probe whether the app currently has Full Disk Access.
    ///
    /// Strategy: attempt to enumerate a known FDA-gated path
    /// (`~/Library/Safari` on every supported macOS) and
    /// check whether the call succeeds. Without FDA the
    /// kernel returns `EPERM`; `FileManager` surfaces that
    /// as an Error. With FDA the call returns a (possibly
    /// empty) array.
    ///
    /// Reading rather than the per-Sandbox `SecScopedAccess`
    /// machinery because we're not sandboxed — the app ships
    /// with the hardened runtime + selective TCC, not the
    /// App-Store sandbox.
    ///
    /// Caveats:
    /// - The probe itself does NOT trigger a permission
    ///   prompt — `~/Library/Safari` is implicitly readable
    ///   when FDA is on (no per-dir prompt was ever needed
    ///   for FDA-eligible paths), and without FDA the kernel
    ///   silently returns EPERM. So this is safe to call on
    ///   every launch.
    /// - On rare configurations Safari may not be installed.
    ///   Falls back to `~/Library/Application Support/com.apple.TCC`,
    ///   which is FDA-gated but always present.
    static var hasFullDiskAccess: Bool {
        let home = NSHomeDirectory()
        // Primary probe: ~/Library/Safari. Exists on every
        // user account with at least one Safari launch (i.e.
        // virtually all of them).
        let primary = (home as NSString)
            .appendingPathComponent("Library/Safari")
        if FileManager.default.fileExists(atPath: primary),
           (try? FileManager.default.contentsOfDirectory(atPath: primary)) != nil
        {
            return true
        }
        // Fallback: the TCC database directory. System-owned,
        // present on every macOS install, FDA-gated.
        let fallback = "/Library/Application Support/com.apple.TCC"
        return (try? FileManager.default.contentsOfDirectory(atPath: fallback)) != nil
    }

    /// Open System Settings with a best-effort Full Disk Access
    /// deep link. If LaunchServices rejects the URL, launch the
    /// app directly. The explainer also provides manual navigation
    /// instructions in case Settings does not select the pane.
    static func openFullDiskAccessSettings() {
        let config = NSWorkspace.OpenConfiguration()
        config.activates = true
        config.addsToRecentItems = false

        let deepLink = URL(string:
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        )!

        NSWorkspace.shared.open(deepLink, configuration: config) { _, error in
            guard error != nil else { return }
            let appPath = URL(fileURLWithPath: "/System/Applications/System Settings.app")
            NSWorkspace.shared.openApplication(
                at: appPath,
                configuration: config,
                completionHandler: nil
            )
        }
    }

    /// Decision returned from `showFullDiskAccessExplainer`.
    enum ExplainerOutcome {
        /// User clicked "Open System Settings…". The scan
        /// must NOT proceed — the user needs to grant FDA
        /// and relaunch the app. The caller should bail.
        case openedSettings
        /// User clicked "Continue Without". Scan proceeds;
        /// mid-scan prompts may still fire for individual
        /// folders.
        case continueWithout
        /// User clicked "Don't Ask Again". Same scan behaviour
        /// as `continueWithout`, plus the caller persists the
        /// choice so the explainer won't appear next time.
        case dontAskAgain
    }

    /// Show the modal explainer and open Settings if requested.
    /// The caller uses the outcome to cancel or continue the scan
    /// and to persist the "Don't Ask Again" preference.
    static func showFullDiskAccessExplainer() -> ExplainerOutcome {
        let alert = NSAlert()
        alert.messageText = "Allow access to protected folders?"
        alert.informativeText = """
        Full Disk Access lets ApfsFastindex scan protected folders. \
        Without it, macOS may ask for permission or skip some folders.

        In System Settings, go to Privacy & Security → Full Disk Access. \
        Add ApfsFastindex with the '+' button and enable access. \
        Then quit and relaunch ApfsFastindex before scanning.

        You can change this reminder in the app's Settings.
        """
        alert.alertStyle = .informational
        // NSAlert return values follow declaration order, not visual position.
        alert.addButton(withTitle: "Open System Settings…")
        alert.addButton(withTitle: "Continue Without")
        alert.addButton(withTitle: "Don't Ask Again")

        switch alert.runModal() {
        case .alertFirstButtonReturn:
            openFullDiskAccessSettings()
            return .openedSettings
        case .alertThirdButtonReturn:
            return .dontAskAgain
        default:
            return .continueWithout
        }
    }
}
