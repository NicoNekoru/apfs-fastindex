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
    /// real `.app` bundle (see `make-app.sh`); these calls become
    /// no-ops there.
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var controller = ScanController()

    init() {
        NSApplication.shared.setActivationPolicy(.regular)
        // Phase-1 native-bridge sanity check. Logs the linked
        // crate version to NSLog; if `apfs_hello` doesn't return
        // 42 the FFI is misconfigured (wrong linker order, wrong
        // dylib search path, name-mangling drift). Returning
        // here would stay quiet so the UI still launches —
        // future phases will surface the failure in a dialog.
        NativeBridge.validate()
    }

    /// `APFS_NATIVE=1` swaps in the native (CoreGraphics +
    /// Rust) renderer for this launch — opt-in while phases 5 +
    /// 6 are still in flight. Default keeps the WKWebView path
    /// so the user's everyday flow is unaffected.
    private static var useNative: Bool {
        ProcessInfo.processInfo.environment["APFS_NATIVE"] == "1"
    }

    var body: some Scene {
        WindowGroup("apfs-fastindex") {
            if Self.useNative {
                NativeContentView()
                    .frame(minWidth: 900, minHeight: 600)
            } else {
                ContentView()
                    .environmentObject(controller)
                    .frame(minWidth: 900, minHeight: 600)
            }
        }
        .windowResizability(.contentSize)
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
