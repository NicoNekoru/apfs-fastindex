import SwiftUI
import Sparkle

/// SwiftUI menu-item wrapper around Sparkle's `checkForUpdates`
/// action.
///
/// Earlier iterations tried to mirror Sparkle's canonical
/// sample with an `@ObservedObject` view-model bridging
/// `canCheckForUpdates` into a `.disabled` binding. That
/// pattern composed poorly with `CommandGroup`'s rendering on
/// macOS — the menu item could end up registered-but-invisible
/// when the `@ObservedObject` hadn't materialised by the time
/// the command-pass ran. Plain unconditional button is
/// reliable.
///
/// Routed through `SPUStandardUpdaterController.checkForUpdates(_:)`
/// rather than `SPUUpdater.checkForUpdates()` — the controller-
/// level action is the documented menu-item entrypoint and is
/// the one that reliably surfaces the standard user-driver UI
/// (the "up to date" / "update available" dialog) for
/// user-initiated checks. The updater-level method runs the
/// same logic internally but in some configurations runs
/// silently without the result dialog.
struct CheckForUpdatesView: View {
    let controller: SPUStandardUpdaterController

    var body: some View {
        Button("Check for Updates…") {
            controller.checkForUpdates(nil)
        }
    }
}
