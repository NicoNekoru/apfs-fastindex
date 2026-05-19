import SwiftUI
import AppKit

/// Phase-5a minimal SwiftUI shell that drives the native
/// `TreemapView` end to end. The existing `ContentView` (with
/// the WKWebView treemap) is still the app's default; this view
/// is opt-in via the `APFS_NATIVE=1` env var. Phase 5b grows
/// breadcrumb / tree-list / ext-list / status bar; phase 6 drops
/// the WKWebView path entirely and makes this the only renderer.
struct NativeContentView: View {
    @State private var pathInput: String = NSHomeDirectory()
    @State private var scan: Scan?
    @State private var layout: Scan.Layout?
    @State private var scanError: String?
    @State private var scanning: Bool = false
    @State private var depth: UInt32 = 0
    @State private var metric: Scan.Metric = .logical
    @State private var lastSize: CGSize = .zero
    @State private var currentNode: UInt32 = 0
    @State private var lastClickedPath: String = ""

    var body: some View {
        VStack(spacing: 0) {
            toolbar
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Color(red: 0x1a / 255, green: 0x1d / 255, blue: 0x24 / 255))
            Divider()
            GeometryReader { proxy in
                ZStack {
                    Color(red: 0x0f / 255, green: 0x11 / 255, blue: 0x15 / 255)
                    TreemapViewRepresentable(
                        layout: layout,
                        onClick: { nodeIndex in
                            handleClick(nodeIndex: nodeIndex)
                        }
                    )
                }
                .onAppear { resize(to: proxy.size) }
                .onChange(of: proxy.size) { newSize in resize(to: newSize) }
            }
            Divider()
            statusBar
                .padding(.horizontal, 12)
                .padding(.vertical, 5)
                .background(Color(red: 0x1a / 255, green: 0x1d / 255, blue: 0x24 / 255))
        }
        .background(Color(red: 0x0f / 255, green: 0x11 / 255, blue: 0x15 / 255))
        .preferredColorScheme(.dark)
        .foregroundStyle(Color(red: 0xe4 / 255, green: 0xe7 / 255, blue: 0xee / 255))
    }

    // MARK: - Toolbar

    private var toolbar: some View {
        HStack(spacing: 8) {
            Image(systemName: "folder")
                .foregroundStyle(.secondary)
            TextField("Path to scan", text: $pathInput)
                .textFieldStyle(.plain)
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(Color(red: 0x0f / 255, green: 0x11 / 255, blue: 0x15 / 255))
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(Color.gray.opacity(0.3), lineWidth: 1)
                )
                .onSubmit { startScan() }

            Picker("", selection: $metric) {
                Text("Logical").tag(Scan.Metric.logical)
                Text("Allocated").tag(Scan.Metric.allocated)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 180)
            .onChange(of: metric) { _ in updateLayout() }

            Stepper("Depth: \(depth == 0 ? "auto" : String(depth))", value: $depth, in: 0...20)
                .onChange(of: depth) { _ in updateLayout() }
                .frame(maxWidth: 180)

            Spacer(minLength: 12)

            if scanning {
                ProgressView()
                    .controlSize(.small)
                    .progressViewStyle(.circular)
            }

            Button {
                startScan()
            } label: {
                Label("Scan", systemImage: "play.fill")
                    .frame(minWidth: 72)
            }
            .buttonStyle(.borderedProminent)
            .keyboardShortcut(.return, modifiers: .command)
            .disabled(scanning)
        }
    }

    // MARK: - Status bar

    private var statusBar: some View {
        HStack(spacing: 12) {
            if let err = scanError {
                Text("error: \(err)")
                    .font(.system(size: 11))
                    .foregroundStyle(.red)
            } else if let scan {
                Text(scan.correctnessClaim.prefix(80))
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Text("•")
                Text("\(scan.entryCount.formatted()) entries")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                if !lastClickedPath.isEmpty {
                    Text("•")
                    Text(lastClickedPath)
                        .font(.system(size: 11, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: 400)
                }
            } else {
                Text("no scan loaded")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
    }

    // MARK: - Scan + layout flow

    private func startScan() {
        let path = pathInput.trimmingCharacters(in: .whitespaces)
        guard !path.isEmpty else { return }
        scanError = nil
        scanning = true
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Scan.fallback(path: path, threads: 0, crossMounts: false)
            DispatchQueue.main.async {
                scanning = false
                if let result {
                    scan = result
                    currentNode = 0
                    updateLayout()
                } else {
                    scan = nil
                    layout = nil
                    scanError = "scan failed for \(path)"
                }
            }
        }
    }

    private func resize(to size: CGSize) {
        // Skip no-op resizes to avoid re-laying out on every
        // SwiftUI re-render.
        if abs(size.width - lastSize.width) < 0.5
            && abs(size.height - lastSize.height) < 0.5 {
            return
        }
        lastSize = size
        updateLayout()
    }

    private func updateLayout() {
        guard let scan, lastSize.width > 0, lastSize.height > 0 else {
            layout = nil
            return
        }
        layout = scan.layout(
            rootedAt: currentNode,
            maxDepth: depth,
            metric: metric,
            width: Float(lastSize.width),
            height: Float(lastSize.height)
        )
    }

    private func handleClick(nodeIndex: UInt32) {
        guard let scan else { return }
        let path = scan.path(of: nodeIndex) ?? ""
        lastClickedPath = path.isEmpty ? "/" : path
        // Phase 5a: clicking a dir navigates into it.
        // Re-layout rooted at the new node.
        if scan.childCount(of: nodeIndex) > 0 {
            currentNode = nodeIndex
            updateLayout()
        }
    }
}
