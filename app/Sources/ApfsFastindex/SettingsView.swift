import Darwin
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

    /// Per-panel visibility for the side column. Both default
    /// to `true` (full layout). The View menu toggles either
    /// individually or both at once via "Treemap Only". When
    /// both are off the top split collapses and the treemap
    /// gets the full window.
    static let showFolderTreeKey = "apfs.showFolderTree"
    static let showExtensionsKey = "apfs.showExtensions"

    /// Whether the user has dismissed the "Grant Full Disk
    /// Access?" explainer. Sticky after they pick
    /// "Don't Ask Again" — at that point the app falls back
    /// to mid-scan TCC prompts for any folders the user
    /// hasn't already authorised. Resettable by toggling
    /// the equivalent option in Settings.
    static let fdaPromptDismissedKey = "apfs.fdaPromptDismissed"

    /// Hours between automatic auto-update checks. `0` means
    /// "check on every app launch and never schedule a
    /// background check"; positive values configure
    /// `SPUUpdater.updateCheckInterval` to that many hours.
    /// Default is `24` (daily) to match the Info.plist baked-in
    /// default. Range clamped to `[0, 168]` (1 week) in the UI
    /// so a user can't accidentally type `99999` and trip the
    /// auto-updater's max-interval clamp.
    static let updateCheckIntervalHoursKey = "apfs.updateCheckIntervalHours"

    /// Upper bound on the worker-threads stepper. Pulled from
    /// `sysctl hw.physicalcpu` — the count of physical CPU
    /// cores, *not* the SMT-doubled logical count. EX-25
    /// measured sub-linear scaling past the physical-core count
    /// on Apple silicon (T=8 cost 4× T=1 sys-CPU for 1.94×
    /// throughput, T=14 cost 9.3× for 1.38×), so the stepper
    /// shouldn't offer values that we know regress performance.
    /// Falls back to `activeProcessorCount` if sysctl ever
    /// fails — that's the logical count on macOS, a safe upper
    /// bound but not the recommended ceiling.
    /// Cached lazily; the value doesn't change over the process
    /// lifetime.
    static let maxWorkerThreads: Int = {
        var count: Int32 = 0
        var size = MemoryLayout<Int32>.size
        let rc = sysctlbyname("hw.physicalcpu", &count, &size, nil, 0)
        if rc == 0 && count > 0 {
            return Int(count)
        }
        return ProcessInfo.processInfo.activeProcessorCount
    }()
}

struct SettingsView: View {
    @AppStorage(AppPrefs.updateCheckIntervalHoursKey) private var updateIntervalHours: Int = 24
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
                        Text("Parallel-walker thread count. 0 (auto) picks min(hw.physicalcpu, 4). Capped at \(AppPrefs.maxWorkerThreads).")
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
                    Stepper("", value: $threads, in: 0...AppPrefs.maxWorkerThreads)
                        .labelsHidden()
                }
                .onAppear {
                    // A user who saved a thread count above the
                    // cap (older build that used `0...32`, or
                    // moved their prefs from a higher-core
                    // machine) would see the stepper unable to
                    // step *up* from the stale value. Clamp on
                    // appear so the displayed number always
                    // matches what the stepper can produce.
                    if threads > AppPrefs.maxWorkerThreads {
                        threads = AppPrefs.maxWorkerThreads
                    }
                }
            } header: {
                Text("Scan")
                    .font(AppFont.ui(11, weight: .semibold))
            }

            Section {
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Check for updates every")
                            .font(AppFont.ui(13, weight: .semibold))
                        Text("How often the app polls the appcast.xml on main for a newer build. 0 = check once on every launch (and never schedule a background check). Range 0–168 hours (1 week).")
                            .font(AppFont.ui(11))
                            .foregroundStyle(.secondary)
                            .lineLimit(4)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 12)
                    Text(updateIntervalHours == 0
                        ? "on launch"
                        : "\(updateIntervalHours) h")
                        .font(AppFont.ui(13))
                        .monospacedDigit()
                        .frame(minWidth: 80, alignment: .trailing)
                    Stepper("", value: $updateIntervalHours, in: 0...168)
                        .labelsHidden()
                }
                .onAppear {
                    // Clamp stale prefs from older builds that may
                    // have stored a value outside the 0–168 range.
                    if updateIntervalHours < 0 {
                        updateIntervalHours = 0
                    } else if updateIntervalHours > 168 {
                        updateIntervalHours = 168
                    }
                }
            } header: {
                Text("Updates")
                    .font(AppFont.ui(11, weight: .semibold))
            }
        }
        .formStyle(.grouped)
        .frame(width: 600, height: 380)
        .font(AppFont.ui(12))
    }
}
