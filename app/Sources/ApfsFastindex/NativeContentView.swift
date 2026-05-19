import SwiftUI
import AppKit

/// Phase-5b SwiftUI shell over the native renderer. Path field
/// + scan trigger live up top; the centre is the treemap; a
/// breadcrumb sits above it for navigation; the status bar
/// reports totals and the source descriptor.
///
/// Opt-in via `APFS_NATIVE=1`. Phase 5c lands the tree-list /
/// ext-list side panels; phase 6 drops the WKWebView path.
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
                .background(VizPalette.panel)
            Divider().background(VizPalette.border)
            breadcrumbBar
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(VizPalette.panel)
            Divider().background(VizPalette.border)
            GeometryReader { proxy in
                ZStack {
                    VizPalette.bg
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
            Divider().background(VizPalette.border)
            statusBar
                .padding(.horizontal, 12)
                .padding(.vertical, 5)
                .background(VizPalette.panel)
        }
        .background(VizPalette.bg)
        .preferredColorScheme(.dark)
        .foregroundStyle(VizPalette.text)
    }

    // MARK: - Toolbar

    private var toolbar: some View {
        HStack(spacing: 8) {
            Button {
                browseForFolder()
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

            TextField("Path to scan", text: $pathInput)
                .textFieldStyle(.plain)
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(VizPalette.bg)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(VizPalette.border, lineWidth: 1)
                )
                .onSubmit { startScan() }

            Picker("", selection: $metric) {
                Text("Logical").tag(Scan.Metric.logical)
                Text("Allocated").tag(Scan.Metric.allocated)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 180)
            .disabled(scan?.allocatedAvailable == false)
            .onChange(of: metric) { _ in updateLayout() }

            HStack(spacing: 4) {
                Text("Depth")
                    .font(.system(size: 11))
                    .foregroundStyle(VizPalette.muted)
                Stepper(value: $depth, in: 0...20) {
                    Text(depth == 0 ? "auto" : String(depth))
                        .font(.system(size: 12, design: .monospaced))
                        .frame(minWidth: 32)
                }
                .labelsHidden()
                .onChange(of: depth) { _ in updateLayout() }
            }
            .padding(.horizontal, 8)

            Spacer(minLength: 12)

            if scanning {
                ProgressView()
                    .controlSize(.small)
                    .progressViewStyle(.circular)
                    .tint(VizPalette.accent)
            }

            Button {
                startScan()
            } label: {
                Label("Scan", systemImage: "play.fill")
                    .frame(minWidth: 72)
            }
            .buttonStyle(.borderedProminent)
            .tint(VizPalette.accent)
            .keyboardShortcut(.return, modifiers: .command)
            .disabled(scanning || pathInput.trimmingCharacters(in: .whitespaces).isEmpty)
        }
    }

    // MARK: - Breadcrumb

    /// Path chain from root → currentNode, clickable in either
    /// direction. Reconstructed via `Scan.parent(of:)` walks; the
    /// chain is bounded by tree depth so this is cheap.
    private var breadcrumbBar: some View {
        HStack(spacing: 0) {
            if scan == nil {
                Text("(no scan loaded)")
                    .font(.system(size: 12))
                    .foregroundStyle(VizPalette.muted)
            } else {
                let chain = breadcrumbChain
                ForEach(0..<chain.count, id: \.self) { i in
                    let node = chain[i]
                    Button {
                        guard node.index != currentNode else { return }
                        currentNode = node.index
                        updateLayout()
                    } label: {
                        Text(node.label)
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(
                                node.index == currentNode
                                    ? VizPalette.text
                                    : VizPalette.accent
                            )
                    }
                    .buttonStyle(.plain)
                    .disabled(node.index == currentNode)
                    if i < chain.count - 1 {
                        Text("›")
                            .foregroundStyle(VizPalette.muted)
                            .padding(.horizontal, 6)
                    }
                }
                Spacer(minLength: 8)
                if currentNode != 0 {
                    Button {
                        if let parent = scan?.parent(of: currentNode) {
                            currentNode = parent
                            updateLayout()
                        }
                    } label: {
                        Label("Up", systemImage: "chevron.up")
                            .labelStyle(.iconOnly)
                            .padding(.horizontal, 4)
                    }
                    .buttonStyle(.borderless)
                    .help("Up to parent directory (⌘↑)")
                    .keyboardShortcut(.upArrow, modifiers: .command)
                }
            }
        }
        .frame(height: 22)
    }

    private struct BreadcrumbNode {
        let index: UInt32
        let label: String
    }

    private var breadcrumbChain: [BreadcrumbNode] {
        guard let scan else { return [] }
        var chain: [BreadcrumbNode] = []
        var cursor: UInt32? = currentNode
        // Walk parents up to root.
        while let c = cursor {
            let label: String
            if c == 0 {
                // Synthetic root — show "/" plus the scan's
                // requested path so the user has context.
                let root = scan.sourceRequestedPath
                label = root.isEmpty ? "/" : root
            } else {
                label = scan.name(of: c) ?? "?"
            }
            chain.append(BreadcrumbNode(index: c, label: label))
            cursor = scan.parent(of: c)
        }
        return chain.reversed()
    }

    // MARK: - Status bar

    private var statusBar: some View {
        HStack(spacing: 10) {
            if let err = scanError {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                Text("error: \(err)")
                    .font(.system(size: 11))
                    .foregroundStyle(.red)
            } else if let scan {
                statusPill(scan.sourceKind.isEmpty ? "fallback" : scan.sourceKind,
                           tint: VizPalette.accent)
                Text(totalsText(for: scan))
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(VizPalette.muted)
                if !lastClickedPath.isEmpty {
                    Spacer()
                    Text(lastClickedPath)
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(VizPalette.text)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: 480, alignment: .trailing)
                        .help(lastClickedPath)
                } else {
                    Spacer()
                }
            } else {
                Text("no scan loaded")
                    .font(.system(size: 11))
                    .foregroundStyle(VizPalette.muted)
                Spacer()
            }
        }
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

    private func totalsText(for scan: Scan) -> String {
        let logical = ByteCountFormatter.string(fromByteCount: Int64(scan.logicalTotal),
                                                countStyle: .binary)
        let allocatedPart: String
        if let alloc = scan.allocatedTotal {
            allocatedPart = "; allocated \(ByteCountFormatter.string(fromByteCount: Int64(alloc), countStyle: .binary))"
        } else if scan.allocatedAvailable {
            allocatedPart = "; allocated unclaimed"
        } else {
            allocatedPart = ""
        }
        return "\(scan.entryCount.formatted()) entries · logical \(logical)\(allocatedPart)"
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
                    lastClickedPath = ""
                    // If the scan's allocated column is missing,
                    // force the picker back to "Logical" so the
                    // user doesn't see an empty treemap on the
                    // disabled metric.
                    if !result.allocatedAvailable && metric == .allocated {
                        metric = .logical
                    }
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
        // Drill into directories; clicking a file just selects it.
        if scan.childCount(of: nodeIndex) > 0 {
            currentNode = nodeIndex
            updateLayout()
        }
    }

    private func browseForFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = false
        panel.prompt = "Scan"
        panel.message = "Pick a directory to scan."
        let trimmed = pathInput.trimmingCharacters(in: .whitespaces)
        if !trimmed.isEmpty,
           FileManager.default.fileExists(atPath: trimmed) {
            panel.directoryURL = URL(fileURLWithPath: trimmed)
        } else {
            panel.directoryURL = URL(fileURLWithPath: NSHomeDirectory())
        }
        let window = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp.windows.first
        let completion: (NSApplication.ModalResponse) -> Void = { response in
            if response == .OK, let url = panel.url {
                pathInput = url.path
            }
        }
        if let window {
            panel.beginSheetModal(for: window, completionHandler: completion)
        } else {
            panel.begin(completionHandler: completion)
        }
    }
}
