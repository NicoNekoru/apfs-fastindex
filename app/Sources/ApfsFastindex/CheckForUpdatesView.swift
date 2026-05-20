import SwiftUI
import Sparkle

/// SwiftUI menu-item wrapper around Sparkle's `checkForUpdates`
/// action.
///
/// Earlier iterations of this view tried to mirror Sparkle's
/// canonical sample by bridging `SPUUpdater.canCheckForUpdates`
/// (KVO-observable) into a `@Published` flag and toggling the
/// button's `.disabled(...)` accordingly. That pattern composes
/// poorly with `CommandGroup`'s rendering on macOS — the menu
/// item could end up registered-but-invisible when the
/// `@ObservedObject` failed to materialise during the command
/// pass. Plain unconditional button is reliable; Sparkle
/// handles re-entrant-check serialisation internally, so we
/// don't lose much by dropping the disabled-binding.
struct CheckForUpdatesView: View {
    let updater: SPUUpdater

    var body: some View {
        Button("Check for Updates…") {
            updater.checkForUpdates()
        }
    }
}
