import AppKit
import Foundation

/// Full Disk Access (FDA) detection + redirect helpers.
///
/// macOS gates dozens of folders through the TCC subsystem:
/// `~/Desktop`, `~/Documents`, `~/Library/Mail`, every
/// `~/Library/Containers/*` (one per app), every
/// `~/Library/CloudStorage/*` (one per cloud provider), the
/// system Time Machine snapshot tree, and more. The walker
/// hits each one as it descends; the first touch triggers a
/// modal permission prompt.
///
/// The unified answer is Full Disk Access. A single one-time
/// grant covers every TCC-gated location for this app
/// forever. Without FDA the alternative is whack-a-mole
/// per-dir prompts mid-scan.
///
/// **There is no API to programmatically grant FDA.** Only
/// the user can — via System Settings → Privacy & Security →
/// Full Disk Access — and the app must restart afterwards
/// for the kernel's per-process TCC cache to refresh. The
/// best we can do is detect the current state and open the
/// settings pane on the user's behalf.
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

    /// Open System Settings to the Full Disk Access pane via
    /// the documented `x-apple.systempreferences:` URL scheme.
    /// Drops the user one click away from adding the app to
    /// the FDA allowlist; we can't drag the app entry in for
    /// them (System Settings won't accept programmatic
    /// authorisation), but this minimises friction.
    static func openFullDiskAccessSettings() {
        // The query string after the bundle identifier is
        // case-sensitive in current macOS releases.
        guard let url = URL(string:
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        ) else { return }
        NSWorkspace.shared.open(url)
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

    /// Show the modal explainer. Returns synchronously — the
    /// `NSAlert.runModal()` call blocks the main thread, which
    /// is exactly what we want here (no UI state changes
    /// mid-prompt). Caller is responsible for honouring the
    /// outcome.
    static func showFullDiskAccessExplainer() -> ExplainerOutcome {
        let alert = NSAlert()
        alert.messageText = "Grant Full Disk Access for smooth scans?"
        alert.informativeText = """
        Without Full Disk Access, macOS will ask permission \
        for individual folders (Documents, Downloads, every \
        app's container, every cloud provider's folder, …) \
        as the scan walks the tree. You'll see one prompt per \
        protected folder.

        Granting Full Disk Access once covers all of them \
        forever. The app needs to be restarted after granting \
        for the permission to take effect.
        """
        alert.alertStyle = .informational
        // Button order matters — leftmost is default; macOS
        // assigns return-value indices in declaration order.
        alert.addButton(withTitle: "Open System Settings…")
        alert.addButton(withTitle: "Continue Without")
        alert.addButton(withTitle: "Don't Ask Again")

        switch alert.runModal() {
        case .alertFirstButtonReturn:
            return .openedSettings
        case .alertThirdButtonReturn:
            return .dontAskAgain
        default:
            return .continueWithout
        }
    }
}
