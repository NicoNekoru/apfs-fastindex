import SwiftUI
import AppKit

struct ContentView: View {
    @EnvironmentObject var controller: ScanController

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            VizWebView(
                onMessage: controller.handleBridgeMessage,
                onReady: controller.bindWebView,
                onDeliverScanFileURL: controller.pendingScanFileURL,
                onDeliverProgress: controller.pendingProgress
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            statusBar
        }
        .background(Color(NSColor.windowBackgroundColor))
    }

    // Layout strategy:
    //
    // - The path field has a `maxWidth` cap so it can't grow indefinitely
    //   (which is what was previously pushing the Scan button off the
    //   right edge of the window).
    // - The right-side action cluster (mode picker, options menu, Scan
    //   button) is separated from the path-field cluster by a Spacer.
    //   The Spacer claims whatever's left, anchoring the Scan button to
    //   the right side of the toolbar with consistent trailing padding.
    // - `Button(.borderedProminent)` style on Scan makes it a tinted
    //   "primary" button so it reads as the main action at a glance.
    private var toolbar: some View {
        HStack(spacing: 8) {
            TextField("Path or .dmg to scan", text: $controller.targetPath)
                .textFieldStyle(.roundedBorder)
                .frame(minWidth: 180, idealWidth: 360, maxWidth: 520)

            Button {
                browseForTarget()
            } label: {
                Image(systemName: "folder")
            }
            .help("Browse… (⌘O)")
            .keyboardShortcut("o", modifiers: .command)

            Picker("", selection: $controller.mode) {
                Text("Auto").tag(ScanMode.auto)
                Text("Raw").tag(ScanMode.raw)
                Text("Fallback").tag(ScanMode.fallback)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 200)
            .help("Scanner mode")

            Menu {
                Toggle("Cross mounts", isOn: $controller.crossMounts)
            } label: {
                Image(systemName: "slider.horizontal.3")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .help("Scan options")

            Spacer(minLength: 12)

            if controller.isScanning {
                ProgressView()
                    .controlSize(.small)
                    .progressViewStyle(.circular)
                Button {
                    controller.cancelScan()
                } label: {
                    Text("Cancel")
                }
                .keyboardShortcut(".", modifiers: .command)
            } else {
                Button {
                    controller.startScan()
                } label: {
                    Text("Scan")
                        .fontWeight(.semibold)
                        // Give the button a comfortable hit target so it
                        // never collapses into a few-pixel sliver near
                        // the right edge.
                        .frame(minWidth: 56)
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(controller.targetPath.isEmpty)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var statusBar: some View {
        HStack(spacing: 10) {
            statusPill(controller.modeLabel, tint: .blue)
            Text(controller.statusText)
                .font(.system(size: 12).monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
            if !controller.totalsText.isEmpty {
                // Logical / allocated totals from the viz's
                // ingest_succeeded message (SR-019 / EX-22). "allocated:
                // unclaimed" means at least one sparse or decmpfs row
                // collapsed the aggregate per the fail-closed contract.
                Text(controller.totalsText)
                    .font(.system(size: 12).monospaced())
                    .foregroundStyle(.secondary)
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
                statusPill("\(controller.skippedCount) skipped", tint: .orange)
            }
            if !controller.selectedPath.isEmpty {
                Text(controller.selectedPath)
                    .font(.system(size: 12).monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 280)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
        .background(Color(NSColor.controlBackgroundColor))
    }

    @ViewBuilder
    private func statusPill(_ text: String, tint: Color) -> some View {
        Text(text)
            .font(.system(size: 11))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .overlay(
                RoundedRectangle(cornerRadius: 4)
                    .stroke(tint.opacity(0.5), lineWidth: 1)
            )
            .foregroundStyle(tint)
    }

    private func browseForTarget() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = []
        panel.message = "Pick a directory to scan, or an APFS .dmg image."
        if panel.runModal() == .OK, let url = panel.url {
            controller.targetPath = url.path
        }
    }
}
