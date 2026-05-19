import SwiftUI
import AppKit

/// Palette matched to the bundled `viz/index.html` so the SwiftUI
/// chrome and the WKWebView treemap read as one surface instead of a
/// dark page sandwiched between a light toolbar and a light status bar.
/// The user reported "some system, some dark" theming on the previous
/// build; pinning every chrome color here is the fix.
enum VizPalette {
    static let bg      = Color(red: 0x0f/255.0, green: 0x11/255.0, blue: 0x15/255.0)
    static let panel   = Color(red: 0x1a/255.0, green: 0x1d/255.0, blue: 0x24/255.0)
    static let border  = Color(red: 0x2a/255.0, green: 0x2e/255.0, blue: 0x38/255.0)
    static let text    = Color(red: 0xe4/255.0, green: 0xe7/255.0, blue: 0xee/255.0)
    static let muted   = Color(red: 0x8b/255.0, green: 0x93/255.0, blue: 0xa5/255.0)
    static let accent  = Color(red: 0x4f/255.0, green: 0x8c/255.0, blue: 0xff/255.0)
    static let warning = Color(red: 0xfb/255.0, green: 0xbf/255.0, blue: 0x24/255.0)
}

struct ContentView: View {
    @EnvironmentObject var controller: ScanController

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider().background(VizPalette.border)
            ZStack {
                VizWebView(
                    onMessage: controller.handleBridgeMessage,
                    onReady: controller.bindWebView,
                    onDeliverScanFileURL: controller.pendingScanFileURL,
                    onDeliverProgress: controller.pendingProgress
                )
                if controller.isFinalizingScan || controller.isIngesting {
                    ingestSpinnerOverlay
                        .transition(.opacity)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // Animate the overlay's appear/disappear so the spinner
            // doesn't snap in and out — at the boundaries the user
            // already sees a state change in the toolbar, and the
            // fade keeps the eye on the right region. Both finalize
            // (post-`terminal:true`, pre-subprocess-exit) and ingest
            // (subprocess-exit through canvas first-paint) drive the
            // overlay; the spinner stays up unbroken across the
            // hand-off.
            .animation(.easeOut(duration: 0.18), value: controller.isFinalizingScan || controller.isIngesting)
            Divider().background(VizPalette.border)
            statusBar
        }
        .background(VizPalette.bg)
        .preferredColorScheme(.dark)
        .foregroundStyle(VizPalette.text)
    }

    /// Loading spinner shown over the WKWebView in the gap between
    /// scanner-process-exit and viz `ingest_succeeded`. On a /-scan
    /// (~3M entries, ~150 MB JSON) that gap is several seconds:
    /// writing the temp file (background queue), the WebKit XHR
    /// fetching it, JSON.parse on the WebKit main thread, and the
    /// viz's `ingest()` materialising the treemap. Without this
    /// overlay the UI looks frozen because `isScanning` is already
    /// false. The overlay covers the whole viz area, dims the
    /// underlying empty/old-treemap state, and shows a centered
    /// spinner + "Loading visualization…" label so the user knows
    /// the app is still working.
    private var ingestSpinnerOverlay: some View {
        ZStack {
            // Slight darkening over the viz so a previously-loaded
            // treemap (after a rescan) reads as "stale" while the
            // new one is being built.
            VizPalette.bg.opacity(0.72)
                .ignoresSafeArea(edges: .bottom)
            VStack(spacing: 12) {
                ProgressView()
                    .controlSize(.large)
                    .progressViewStyle(.circular)
                    .tint(VizPalette.accent)
                Text("Loading visualization…")
                    .font(.system(size: 13))
                    .foregroundStyle(VizPalette.muted)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 18)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .fill(VizPalette.panel)
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(VizPalette.border, lineWidth: 1)
                    )
            )
        }
        // Swallow clicks so the user can't accidentally interact with
        // the underlying webview (right-click context menu, link clicks)
        // while the ingest is mid-flight.
        .contentShape(Rectangle())
        .onTapGesture { /* swallow */ }
    }

    // MARK: - Toolbar (state-aware)

    /// The toolbar has three distinct visual modes so the user can
    /// always tell what's actionable. Previously a single layout was
    /// shown unconditionally, which meant the prominent "Path or .dmg
    /// to scan" prompt and the big Scan button kept asking the user
    /// to start a scan even after one had finished — exactly the
    /// "Half the UI continues to ask you to scan" complaint.
    @ViewBuilder
    private var toolbar: some View {
        Group {
            if controller.isScanning {
                scanningToolbar
            } else if controller.hasLoadedScan {
                loadedToolbar
            } else {
                idleToolbar
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(VizPalette.panel)
    }

    /// Idle: no scan yet. Prominent target picker + Scan.
    private var idleToolbar: some View {
        HStack(spacing: 8) {
            // The folder icon is the affordance the user reaches for
            // when they want to pick a directory — keeping the browse
            // action on a separate `ellipsis.circle` on the far side
            // of the text field was the original UX miss. The icon
            // itself is now the button: full hit target, plain
            // styling so it doesn't shout, ⌘O still bound for
            // keyboard. The text field stays editable for paste-in
            // or typed paths, but no longer has a competing browse
            // affordance next to it.
            Button {
                browseForTarget()
            } label: {
                Image(systemName: "folder")
                    .font(.system(size: 14))
                    .foregroundStyle(VizPalette.muted)
                    .padding(.horizontal, 4)
                    .padding(.vertical, 4)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Browse… (⌘O)")
            .keyboardShortcut("o", modifiers: .command)

            TextField("Pick a folder or detached .dmg", text: $controller.targetPath)
                .textFieldStyle(.plain)
                .foregroundStyle(VizPalette.text)
                .font(.system(size: 13))
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(VizPalette.bg)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(VizPalette.border, lineWidth: 1)
                )
                .frame(minWidth: 220, idealWidth: 420)

            modePicker

            optionsMenu

            Spacer(minLength: 12)

            Button {
                controller.startScan()
            } label: {
                Label("Scan", systemImage: "play.fill")
                    .labelStyle(.titleAndIcon)
                    .frame(minWidth: 72)
            }
            .buttonStyle(.borderedProminent)
            .tint(VizPalette.accent)
            .keyboardShortcut(.return, modifiers: .command)
            .disabled(controller.targetPath.isEmpty)
        }
    }

    /// Scanning: the path field is no longer interactive. Show the
    /// live count + elapsed time prominently with a Cancel button.
    private var scanningToolbar: some View {
        HStack(spacing: 12) {
            ProgressView()
                .controlSize(.small)
                .progressViewStyle(.circular)
                .tint(VizPalette.accent)

            VStack(alignment: .leading, spacing: 1) {
                Text("Scanning")
                    .font(.system(size: 11))
                    .foregroundStyle(VizPalette.muted)
                Text(controller.targetPath.isEmpty ? "—" : controller.targetPath)
                    .font(.system(size: 12).monospaced())
                    .foregroundStyle(VizPalette.text)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .frame(maxWidth: 380, alignment: .leading)

            Spacer(minLength: 12)

            Text(controller.liveCountersText)
                .font(.system(size: 12).monospaced())
                .foregroundStyle(VizPalette.text)
                .lineLimit(1)

            Button(role: .cancel) {
                controller.cancelScan()
            } label: {
                Text("Cancel").frame(minWidth: 64)
            }
            .keyboardShortcut(".", modifiers: .command)
        }
    }

    /// Post-scan: the path field is gone. We show a compact summary
    /// of what was scanned plus two buttons: Rescan (re-runs the
    /// same target/mode) and New… (resets to idle so the user can
    /// pick something else). The status bar carries the totals.
    private var loadedToolbar: some View {
        HStack(spacing: 10) {
            Image(systemName: "checkmark.seal.fill")
                .foregroundStyle(VizPalette.accent)
            VStack(alignment: .leading, spacing: 1) {
                Text("Scanned")
                    .font(.system(size: 11))
                    .foregroundStyle(VizPalette.muted)
                Text(controller.targetPath.isEmpty ? "—" : controller.targetPath)
                    .font(.system(size: 12).monospaced())
                    .foregroundStyle(VizPalette.text)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(controller.targetPath)
            }
            .frame(maxWidth: 360, alignment: .leading)

            Divider().frame(height: 22).background(VizPalette.border)

            Text(controller.loadedSummaryText)
                .font(.system(size: 12).monospaced())
                .foregroundStyle(VizPalette.muted)
                .lineLimit(1)
                .truncationMode(.tail)

            Spacer(minLength: 12)

            Button {
                controller.startScan()
            } label: {
                Label("Rescan", systemImage: "arrow.clockwise")
                    .labelStyle(.titleAndIcon)
                    .frame(minWidth: 84)
            }
            .buttonStyle(.borderedProminent)
            .tint(VizPalette.accent)
            .keyboardShortcut("r", modifiers: .command)
            .help("Rescan the same target (⌘R)")

            Button {
                controller.clearLoadedScan()
            } label: {
                Label("New…", systemImage: "plus")
                    .labelStyle(.titleAndIcon)
                    .frame(minWidth: 70)
            }
            .buttonStyle(.bordered)
            .help("Pick a different target")
        }
    }

    private var modePicker: some View {
        Picker("", selection: $controller.mode) {
            Text("Auto").tag(ScanMode.auto)
            Text("Raw").tag(ScanMode.raw)
            Text("Fallback").tag(ScanMode.fallback)
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .frame(width: 200)
        .help("Scanner mode")
    }

    private var optionsMenu: some View {
        Menu {
            Toggle("Cross mounts", isOn: $controller.crossMounts)
        } label: {
            Image(systemName: "slider.horizontal.3")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Scan options")
    }

    // MARK: - Status bar

    private var statusBar: some View {
        HStack(spacing: 10) {
            statusPill(controller.modeLabel, tint: VizPalette.accent)
            Text(controller.statusBarPrimaryText)
                .font(.system(size: 12).monospaced())
                .foregroundStyle(VizPalette.muted)
                .lineLimit(1)
                .truncationMode(.tail)
            if !controller.totalsText.isEmpty {
                Text(controller.totalsText)
                    .font(.system(size: 12).monospaced())
                    .foregroundStyle(VizPalette.muted)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .help(
                        controller.allocatedColumnAvailable
                            ? "Logical = sum of st_size; allocated = sum of st_blocks*512 (SR-019). \"unclaimed\" means at least one sparse or decmpfs row collapsed the aggregate per the SR-019 / EX-22 fail-closed contract."
                            : "Logical = sum of st_size. This scan pre-dates R2-A so the allocated_size column is not available."
                    )
            }
            Spacer(minLength: 8)
            if controller.skippedCount > 0 {
                statusPill("\(controller.skippedCount) skipped", tint: VizPalette.warning)
            }
            if !controller.selectedPath.isEmpty {
                Text(controller.selectedPath)
                    .font(.system(size: 12).monospaced())
                    .foregroundStyle(VizPalette.text)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    // The previous build capped this at 280 pt, which
                    // truncated long absolute paths so aggressively
                    // (e.g. `/Users/.../somefile.txt` -> 30-char fragment)
                    // that the user couldn't tell what they had
                    // clicked. Lifting the cap to 480 pt and keeping
                    // truncation in the middle keeps both ends
                    // visible on real-world paths.
                    .frame(maxWidth: 480, alignment: .trailing)
                    .help(controller.selectedPath)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
        .background(VizPalette.panel)
    }

    @ViewBuilder
    private func statusPill(_ text: String, tint: Color) -> some View {
        Text(text)
            .font(.system(size: 11))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .overlay(
                RoundedRectangle(cornerRadius: 4)
                    .stroke(tint.opacity(0.55), lineWidth: 1)
            )
            .foregroundStyle(tint)
    }

    private func browseForTarget() {
        // The previous build called `panel.runModal()`, which is
        // synchronous and spins the main thread's run loop in modal
        // mode. NSOpenPanel does a noticeable amount of work the
        // first time it shows (NSDocumentController hookup, sidebar
        // enumeration, NSURLBookmarkResolution for the recent-items
        // list); under runModal() all of that runs *before* the
        // panel paints, so the click on the folder button stalls
        // visibly. Sheets bind to the window's run loop instead and
        // present immediately, deferring the slow work to after the
        // panel is on screen.
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        // An empty `allowedContentTypes` means "no filter", which is
        // what we want (a folder OR a .dmg). Setting it would force
        // NSOpenPanel into the "filtered" rendering path, which
        // adds a layout pass per directory.
        panel.allowedContentTypes = []
        panel.canCreateDirectories = false
        panel.treatsFilePackagesAsDirectories = false
        panel.prompt = "Scan"
        panel.message = "Pick a directory to scan, or an APFS .dmg image."
        // Pre-seed the panel's starting directory so it doesn't have
        // to roll its own default (which on a cold cache walks
        // ~/Library/Recent and friends). If we have a previously
        // typed target use that, otherwise fall back to $HOME.
        let trimmed = controller.targetPath.trimmingCharacters(in: .whitespaces)
        if !trimmed.isEmpty {
            let url = URL(fileURLWithPath: trimmed)
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) {
                panel.directoryURL = isDir.boolValue ? url : url.deletingLastPathComponent()
            } else {
                panel.directoryURL = URL(fileURLWithPath: NSHomeDirectory())
            }
        } else {
            panel.directoryURL = URL(fileURLWithPath: NSHomeDirectory())
        }

        let window = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp.windows.first
        let completion: (NSApplication.ModalResponse) -> Void = { response in
            if response == .OK, let url = panel.url {
                controller.targetPath = url.path
            }
        }
        if let window {
            panel.beginSheetModal(for: window, completionHandler: completion)
        } else {
            // Fallback for the rare case where no window is up yet:
            // `begin` is still async, just unattached to any window.
            panel.begin(completionHandler: completion)
        }
    }
}
