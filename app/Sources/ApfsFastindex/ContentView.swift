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
                onDeliverScanJSON: controller.pendingScanJSON,
                onDeliverProgress: controller.pendingProgress
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            statusBar
        }
        .background(Color(NSColor.windowBackgroundColor))
    }

    private var toolbar: some View {
        HStack(spacing: 12) {
            // Target path field + browse
            TextField("Path or .dmg to scan", text: $controller.targetPath)
                .textFieldStyle(.roundedBorder)
                .frame(minWidth: 320)
            Button("Browse…") { browseForTarget() }
                .keyboardShortcut("o", modifiers: .command)

            // Mode picker
            Picker("Mode", selection: $controller.mode) {
                Text("Auto").tag(ScanMode.auto)
                Text("Raw").tag(ScanMode.raw)
                Text("Fallback").tag(ScanMode.fallback)
            }
            .pickerStyle(.segmented)
            .fixedSize()

            Toggle("Cross mounts", isOn: $controller.crossMounts)

            Spacer()

            if controller.isScanning {
                Button("Cancel") { controller.cancelScan() }
                ProgressView()
                    .controlSize(.small)
                    .progressViewStyle(.circular)
            } else {
                Button("Scan") { controller.startScan() }
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(controller.targetPath.isEmpty)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    private var statusBar: some View {
        HStack(spacing: 14) {
            statusPill(controller.modeLabel, tint: .blue)
            Text(controller.statusText)
                .font(.system(size: 12).monospaced())
                .foregroundStyle(.secondary)
            Spacer()
            if controller.skippedCount > 0 {
                statusPill("\(controller.skippedCount) skipped", tint: .orange)
            }
            if !controller.selectedPath.isEmpty {
                Text(controller.selectedPath)
                    .font(.system(size: 12).monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 360)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
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
