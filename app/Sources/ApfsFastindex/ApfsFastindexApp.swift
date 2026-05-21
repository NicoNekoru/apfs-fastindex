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
    /// Held for the lifetime of the app so Sparkle's weak
    /// delegate reference stays valid. Provides the appcast URL
    /// in both production and dev (where Info.plist's SUFeedURL
    /// isn't present).
    private let updaterDelegate = UpdaterDelegate()
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
            updaterDelegate: updaterDelegate,
            userDriverDelegate: nil
        )
        // Apply the user's saved check-interval preference on
        // launch. Re-applied on every UserDefaults change via
        // the AppDelegate's notification observer so Settings
        // edits take effect without a relaunch.
        ApfsFastindexApp.applyUpdateInterval(
            to: updaterController.updater,
            hours: UserDefaults.standard.object(forKey: AppPrefs.updateCheckIntervalHoursKey)
                .flatMap { ($0 as? Int) } ?? 24
        )
        // Publish for the AppDelegate's UserDefaults observer
        // so Settings-stepper changes get re-applied without a
        // relaunch.
        AppDelegate.updater = updaterController.updater
    }

    /// Last value the apply path actually wrote to Sparkle.
    /// Setting Sparkle's properties triggers internal
    /// UserDefaults writes that fire didChangeNotification —
    /// which our AppDelegate observer responds to by calling
    /// this method again. Without a de-dup the result is
    /// unbounded recursion through Sparkle's notification
    /// pipeline (observed as a stack-overflow crash with
    /// thousands of identical frames).
    ///
    /// `nil` until the first apply so the initial App.init call
    /// always runs. Accessed only from the main queue (App.init
    /// + the .main-queued UserDefaults observer).
    private static var lastAppliedHours: Int?

    /// Configure Sparkle's automatic-check cadence from the
    /// user's `apfs.updateCheckIntervalHours` preference.
    ///
    /// - `0` → "check on every launch": fire one
    ///   `checkForUpdatesInBackground()` now and don't schedule
    ///   any further background checks. We set the interval to
    ///   a very large value (10 years) instead of disabling
    ///   `automaticallyChecksForUpdates`, because the latter
    ///   also gates first-launch behaviour Sparkle needs.
    /// - `N > 0` → schedule a check every N hours.
    static func applyUpdateInterval(to updater: SPUUpdater, hours: Int) {
        // Reentrancy break: the property writes below trigger
        // Sparkle's internal UserDefaults writes, which fire
        // didChangeNotification, which calls us again. If the
        // requested interval is unchanged, this round-trip is
        // a no-op — return before re-applying so we never feed
        // the notification loop.
        if lastAppliedHours == hours {
            return
        }
        lastAppliedHours = hours
        updater.automaticallyChecksForUpdates = true
        if hours <= 0 {
            // Effectively never; we manually drive the
            // every-launch check below.
            updater.updateCheckInterval = 60 * 60 * 24 * 365 * 10
            updater.checkForUpdatesInBackground()
        } else {
            updater.updateCheckInterval = TimeInterval(hours * 3600)
        }
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
                CheckForUpdatesView(controller: updaterController)
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
            // Edit menu: replace the standard system "Find"
            // command (CommandGroup.textEditing.find) with our
            // own ⌘F that focuses the tree-list search field
            // — Apple's default Find is geared at NSTextView
            // and does nothing useful in this app. Posting via
            // NotificationCenter matches the
            // scanAsAdministratorRequested pattern: the active
            // `NativeContentView` listens, the menu command
            // stays neutral on whether a view exists yet.
            CommandGroup(replacing: .textEditing) {
                Button("Find") {
                    NotificationCenter.default.post(
                        name: .findRequested,
                        object: nil
                    )
                }
                .keyboardShortcut("f", modifiers: .command)
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
    /// Set by `ApfsFastindexApp.init()` so the delegate's
    /// UserDefaults observer can re-apply the interval to the
    /// live updater when the Settings stepper changes mid-
    /// session.
    static weak var updater: SPUUpdater?

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        // Re-apply Sparkle's check interval whenever the user
        // changes the Settings stepper. UserDefaults fires this
        // notification for any defaults write, so we filter on
        // our specific key inside the observer.
        NotificationCenter.default.addObserver(
            forName: UserDefaults.didChangeNotification,
            object: UserDefaults.standard,
            queue: .main
        ) { _ in
            guard let updater = AppDelegate.updater else { return }
            let hours = UserDefaults.standard
                .object(forKey: AppPrefs.updateCheckIntervalHoursKey)
                .flatMap { $0 as? Int } ?? 24
            ApfsFastindexApp.applyUpdateInterval(to: updater, hours: hours)
        }
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
