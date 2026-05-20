import SwiftUI
import Sparkle

/// SwiftUI menu-item wrapper around Sparkle's `checkForUpdates`
/// action. Disabled while another check or download is already
/// in flight so a user mashing the menu doesn't queue multiple
/// concurrent checks. The `canCheckForUpdates` publisher on
/// `SPUUpdater` flips the `@Published` bound to the button's
/// `disabled` modifier.
///
/// Lives in its own file so the `App` body in
/// `ApfsFastindexApp.swift` stays focused on scene/command
/// wiring and doesn't have to carry the view-model boilerplate.
struct CheckForUpdatesView: View {
    /// View model that bridges Sparkle's KVO-observable
    /// `canCheckForUpdates` into SwiftUI's reactive layer.
    private final class ViewModel: ObservableObject {
        @Published var canCheckForUpdates: Bool = false

        init(updater: SPUUpdater) {
            // Sparkle's docs use this exact pattern for the
            // SwiftUI menu-item wiring.
            updater.publisher(for: \.canCheckForUpdates)
                .assign(to: &$canCheckForUpdates)
        }
    }

    private let updater: SPUUpdater
    @ObservedObject private var viewModel: ViewModel

    init(updater: SPUUpdater) {
        self.updater = updater
        self.viewModel = ViewModel(updater: updater)
    }

    var body: some View {
        Button("Check for Updates…", action: updater.checkForUpdates)
            .disabled(!viewModel.canCheckForUpdates)
    }
}
