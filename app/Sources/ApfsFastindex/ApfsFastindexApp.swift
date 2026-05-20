import SwiftUI
import AppKit

@main
struct ApfsFastindexApp: App {
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
    }

    var body: some Scene {
        WindowGroup("apfs-fastindex") {
            NativeContentView()
                .frame(minWidth: 900, minHeight: 600)
                .font(AppFont.ui(12))
        }
        .windowResizability(.contentSize)
        .commands {
            // File menu: "Scan as Administrator…" (⌘⇧A). Posts a
            // notification picked up by the active
            // `NativeContentView`, which spawns the bundled CLI
            // under osascript-with-admin-privileges. Disabled if
            // the build pipeline didn't ship the CLI helper
            // (Bundle.main.url(forAuxiliaryExecutable:) returns
            // nil) — the user can still use regular Scan in that
            // case. EX-28 follow-up: macOS pops the auth dialog;
            // the privileged scan uses the fallback walker (not
            // raw mode, which EX-28 closed as kernel-blocked).
            CommandGroup(after: .newItem) {
                Button("Scan as Administrator…") {
                    NotificationCenter.default.post(
                        name: .scanAsAdministratorRequested,
                        object: nil
                    )
                }
                .keyboardShortcut("A", modifiers: [.command, .shift])
                .disabled(PrivilegedScan.bundledCliURL == nil)
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
}
