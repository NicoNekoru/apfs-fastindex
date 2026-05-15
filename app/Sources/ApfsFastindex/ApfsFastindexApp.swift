import SwiftUI

@main
struct ApfsFastindexApp: App {
    @StateObject private var controller = ScanController()

    var body: some Scene {
        WindowGroup("apfs-fastindex") {
            ContentView()
                .environmentObject(controller)
                .frame(minWidth: 900, minHeight: 600)
        }
        .windowResizability(.contentSize)
    }
}
