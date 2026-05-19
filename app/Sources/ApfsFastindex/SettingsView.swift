import SwiftUI

/// Persisted user preferences. Settings panel reads/writes these
/// via `@AppStorage`; the main `NativeContentView` reads them
/// (also via `@AppStorage`) so changes propagate without an
/// explicit publish.
enum AppPrefs {
    /// Treemap recursion depth. `0` is the auto-fit sentinel
    /// (recurse until cells fall below 1 px).
    static let depthKey = "apfs.depth"
    /// Worker thread count. `0` is the auto sentinel (CLI
    /// default = `min(hw.physicalcpu, 4)`). The fallback walker
    /// treats `0` and `1` as the serial implementation; anything
    /// ≥ 2 uses the parallel walker.
    static let threadsKey = "apfs.threads"
}

struct SettingsView: View {
    @AppStorage(AppPrefs.depthKey) private var depth: Int = 0
    @AppStorage(AppPrefs.threadsKey) private var threads: Int = 0

    var body: some View {
        Form {
            Section {
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Treemap depth")
                            .font(AppFont.ui(13, weight: .semibold))
                        Text("How many levels to render before collapsing. 0 (auto) recurses until cells fall below 1 px.")
                            .font(AppFont.ui(11))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 12)
                    Text(depth == 0 ? "auto" : String(depth))
                        .font(AppFont.ui(13))
                        .monospacedDigit()
                        .frame(minWidth: 44, alignment: .trailing)
                    Stepper("", value: $depth, in: 0...20)
                        .labelsHidden()
                }
            } header: {
                Text("Visualization")
                    .font(AppFont.ui(11, weight: .semibold))
            }

            Section {
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Worker threads")
                            .font(AppFont.ui(13, weight: .semibold))
                        Text("Parallel-walker thread count. 0 (auto) picks min(hw.physicalcpu, 4). Higher counts hit diminishing returns past 4 on Apple silicon.")
                            .font(AppFont.ui(11))
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 12)
                    Text(threads == 0 ? "auto" : String(threads))
                        .font(AppFont.ui(13))
                        .monospacedDigit()
                        .frame(minWidth: 44, alignment: .trailing)
                    Stepper("", value: $threads, in: 0...32)
                        .labelsHidden()
                }
            } header: {
                Text("Scan")
                    .font(AppFont.ui(11, weight: .semibold))
            }
        }
        .formStyle(.grouped)
        .frame(width: 460, height: 280)
        .font(AppFont.ui(12))
    }
}
