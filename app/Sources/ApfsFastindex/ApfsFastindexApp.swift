import SwiftUI
import AppKit
import Sparkle

@main
struct ApfsFastindexApp: App {
    /// Sparkle 2 updater controller. Owns the SPUUpdater + the
    /// user-driver that mediates UI prompts. Initialised with
    /// `startingUpdater: true` so the daily background check
    /// fires automatically; configured via Info.plist keys the
    /// build pipeline (`make-release.sh`) injects:
    ///
    /// - `SUFeedURL`                appcast.xml URL on main
    /// - `SUPublicEDKey`            EdDSA verification public key
    /// - `SUEnableAutomaticChecks`  true (daily background poll)
    /// - `SUAutomaticallyUpdate`    false (always prompt before
    ///                              installing — clearer UX for
    ///                              an early-stage app)
    /// - `SUScheduledCheckInterval` 86400 seconds (one day)
    private let updaterController: SPUStandardUpdaterController
    /// SwiftPM-built binaries launch as a CLI tool by default, so even
    /// though the SwiftUI scene wires up, the window is never raised to
    /// the foreground and no dock icon appears. We flip the activation
    /// policy to `.regular` at the earliest possible moment (App.init
    /// runs before the run loop starts) and also re-issue it in the
    /// delegate as belt-and-suspenders. The proper Phase-4 fix is a
    /// real `.app` bundle (see `make-release.sh` in the repo root,
    /// which assembles `app/ApfsFastindex.app`); these calls become
    /// no-ops there.
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    init() {
        NSApplication.shared.setActivationPolicy(.regular)
        // Native-bridge sanity check. Logs the linked crate
        // version to NSLog; if `apfs_hello` doesn't return 42
        // the FFI is misconfigured (wrong linker order, wrong
        // static-lib search path, name-mangling drift).
        NativeBridge.validate()
        updaterController = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
    }

    var body: some Scene {
        WindowGroup("apfs-fastindex") {
            NativeContentView()
                .frame(minWidth: 900, minHeight: 600)
                .font(AppFont.ui(12))
        }
        .windowResizability(.contentSize)
        .commands {
            // Application menu: "Check for Updates…" — sits next
            // to "About apfs-fastindex". `CommandGroup(after:
            // .appInfo)` is the canonical placement for Sparkle's
            // standard updater button per Sparkle 2's SwiftUI
            // integration docs. The view's `disabled` state
            // mirrors `SPUUpdater.canCheckForUpdates` (false
            // while another check or download is in flight).
            CommandGroup(after: .appInfo) {
                CheckForUpdatesView(updater: updaterController.updater)
            }

            // File menu: "Scan as Administrator…" (⌘⇧A). Posts a
            // notification picked up by the active
            // `NativeContentView`, which (per
            // `PrivilegedScan.run`) either spawns the bundled CLI
            // under osascript-with-admin-privileges or — when the
            // process is already running as root — calls the
            // in-process fallback walker directly.
            //
            // The menu is always enabled: silently disabling it
            // when the CLI helper is missing was confusing in
            // dev-mode swift-run launches. `PrivilegedScan.run`
            // surfaces a clear error popup in the status bar if
            // the helper cannot be located, which is friendlier
            // than a mysteriously grey menu item.
            //
            // EX-28 follow-up: the privileged scan uses the
            // fallback walker (not raw mode, which EX-28 closed
            // as kernel-blocked under SIP).
            CommandGroup(after: .newItem) {
                Button("Scan as Administrator…") {
                    NotificationCenter.default.post(
                        name: .scanAsAdministratorRequested,
                        object: nil
                    )
                }
                .keyboardShortcut("A", modifiers: [.command, .shift])
            }
            // Help-menu entry → opens the Rust panic-hook log
            // file in the user's default editor. The file lives
            // under `~/Library/Logs/apfs-fastindex.log`; this
            // shortcut means users don't have to know that.
            CommandGroup(after: .help) {
                Button("View Log…") {
                    if let path = NativeBridge.logPath {
                        NSWorkspace.shared.open(URL(fileURLWithPath: path))
                    }
                }
                .disabled(NativeBridge.logPath == nil)
            }
        }

        // ⌘, gets a real SwiftUI Settings scene. The scene
        // discovery hook ("Preferences…" / "Settings…" in the
        // app menu) is wired automatically by SwiftUI when this
        // is present.
        Settings {
            SettingsView()
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
    }

    // We intentionally do *not* iterate `NSApp.windows` and call
    // `makeKeyAndOrderFront` in `applicationDidFinishLaunching`. On
    // macOS 26 (Tahoe) that runs before SwiftUI has fully attached its
    // window to the hierarchy and can raise an AppKit exception during
    // the first constraint pass. The `WindowGroup` brings its window
    // up by itself.

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Tear down the privileged helper (if any) so it doesn't
        // hang around as a zombie after the GUI dies. Best-effort
        // — `AdminSession.shutdown()` is idempotent on the
        // already-not-running state.
        AdminSession.shared.shutdown()
    }
}
