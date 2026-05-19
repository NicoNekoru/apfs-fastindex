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
    /// Set of node indices whose tree-list rows are expanded.
    /// On a fresh scan we seed with `{0}` so the root's
    /// top-level children are visible without a click.
    @State private var expandedNodes: Set<UInt32> = [0]
    /// Cached flattened tree-list rows. Recomputed when scan /
    /// currentNode / expandedNodes / metric change so the
    /// SwiftUI list view doesn't re-walk the tree on every
    /// view body re-evaluation.
    @State private var treeRows: [TreeListRow] = []

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
            // HSplitView so the user can drag the boundary
            // between the tree-list sidebar and the treemap.
            // Tree-list panel is on the left at ~280 pt
            // default; the treemap fills the rest.
            HSplitView {
                treeListPanel
                    .frame(minWidth: 220, idealWidth: 300, maxWidth: 500)
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
                .frame(minWidth: 320)
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
        .onChange(of: scan?.entryCount) { _ in rebuildTreeRows() }
        .onChange(of: currentNode) { _ in rebuildTreeRows() }
        .onChange(of: metric) { _ in rebuildTreeRows() }
        .onChange(of: expandedNodes) { _ in rebuildTreeRows() }
    }

    // MARK: - Tree-list panel

    /// Soft cap on visible children per directory in the tree-
    /// list — matches the JS canvas-era constant. Beyond this
    /// the row build adds a "… and N more" placeholder; the
    /// user can drill into the directory to see the rest.
    private static let treeListChildrenCap = 400

    private struct TreeListRow: Identifiable {
        // Stable identity: a (nodeIndex, depth) pair survives
        // re-renders without re-issuing implicit identifiers.
        let id: UInt64
        let nodeIndex: UInt32
        let depth: Int
        let hasChildren: Bool
        let isExpanded: Bool
        let isCurrent: Bool
        /// Special "+N more" row that doesn't correspond to a
        /// real node. `nodeIndex == APFS_NODE_INVALID`.
        let isOverflow: Bool
        let overflowCount: Int
    }

    private var treeListPanel: some View {
        VStack(spacing: 0) {
            paneHeader("Folder tree")
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(VizPalette.bg)
            colHeader
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .background(VizPalette.bg)
            Divider().background(VizPalette.border)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(treeRows) { row in
                        treeListRowView(row)
                    }
                }
                .padding(.vertical, 2)
            }
        }
        .background(VizPalette.panel)
    }

    @ViewBuilder
    private func paneHeader(_ title: String) -> some View {
        HStack {
            Text(title.uppercased())
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .tracking(0.4)
            Spacer()
        }
    }

    private var colHeader: some View {
        HStack(spacing: 0) {
            // 22 pt indent column for the disclosure triangle.
            Spacer().frame(width: 22)
            Text("Name")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .tracking(0.4)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text("% / parent")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .frame(width: 80, alignment: .trailing)
            Text("Size")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .frame(width: 70, alignment: .trailing)
                .padding(.leading, 4)
        }
    }

    @ViewBuilder
    private func treeListRowView(_ row: TreeListRow) -> some View {
        if row.isOverflow {
            HStack {
                Spacer().frame(width: CGFloat(14 * (row.depth + 1) + 8))
                Text("… and \(row.overflowCount) more")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(VizPalette.muted)
                Spacer()
            }
            .padding(.vertical, 1)
        } else {
            let scan = self.scan
            HStack(spacing: 0) {
                Spacer().frame(width: CGFloat(14 * row.depth + 4))
                // Disclosure triangle (or invisible placeholder
                // when this row has no children — keeps the
                // name column lined up).
                Button {
                    toggleExpansion(of: row.nodeIndex)
                } label: {
                    Image(systemName: row.isExpanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 9))
                        .foregroundStyle(row.hasChildren ? VizPalette.muted : .clear)
                        .frame(width: 14, height: 14, alignment: .center)
                }
                .buttonStyle(.plain)
                .disabled(!row.hasChildren)

                // Kind icon — quick visual telling files,
                // symlinks, dirs apart.
                let kind = scan?.kind(of: row.nodeIndex) ?? .invalid
                Image(systemName: rowIconName(kind: kind))
                    .font(.system(size: 11))
                    .foregroundStyle(rowIconColor(kind: kind))
                    .frame(width: 14)
                    .padding(.trailing, 4)

                // Name (or "/" for the root). Tap-anywhere area:
                // wrap in a Button so SwiftUI catches the click
                // without us having to hand-roll gesture
                // tracking.
                Button {
                    navigate(to: row.nodeIndex)
                } label: {
                    let name: String = {
                        if row.nodeIndex == 0 { return "/" }
                        return scan?.name(of: row.nodeIndex) ?? "?"
                    }()
                    HStack(spacing: 0) {
                        Text(name)
                            .font(.system(size: 12, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text(percentText(for: row))
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(VizPalette.muted)
                            .frame(width: 80, alignment: .trailing)
                        Text(sizeText(for: row))
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(VizPalette.text)
                            .frame(width: 70, alignment: .trailing)
                            .padding(.leading, 4)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(row.isCurrent
                        ? VizPalette.accent.opacity(0.22)
                        : Color.clear)
        }
    }

    private func rowIconName(kind: Scan.NodeKind) -> String {
        switch kind {
        case .dir: return "folder.fill"
        case .file: return "doc"
        case .symlink: return "arrow.forward.circle"
        default: return "questionmark.circle"
        }
    }

    private func rowIconColor(kind: Scan.NodeKind) -> Color {
        switch kind {
        case .dir: return Color(red: 0xf4 / 255, green: 0xd3 / 255, blue: 0x5e / 255)
        case .symlink: return Color(red: 0x7d / 255, green: 0x8a / 255, blue: 0x99 / 255)
        default: return VizPalette.muted
        }
    }

    private func percentText(for row: TreeListRow) -> String {
        guard let scan, row.nodeIndex != 0 else { return "—" }
        let parent = scan.parent(of: row.nodeIndex) ?? 0
        let parentValue = metricValue(for: parent, scan: scan)
        guard parentValue > 0 else { return "—" }
        let ownValue = metricValue(for: row.nodeIndex, scan: scan)
        let pct = Double(ownValue) / Double(parentValue) * 100.0
        return String(format: "%.1f%%", pct)
    }

    private func sizeText(for row: TreeListRow) -> String {
        guard let scan else { return "" }
        let value = metricValue(for: row.nodeIndex, scan: scan)
        if metric == .allocated && scan.valueAllocated(of: row.nodeIndex) == nil {
            return "unclaimed"
        }
        return ByteCountFormatter.string(fromByteCount: Int64(value), countStyle: .binary)
    }

    private func metricValue(for nodeIndex: UInt32, scan: Scan) -> UInt64 {
        if metric == .allocated {
            return scan.valueAllocated(of: nodeIndex) ?? 0
        }
        return scan.valueLogical(of: nodeIndex)
    }

    private func toggleExpansion(of nodeIndex: UInt32) {
        if expandedNodes.contains(nodeIndex) {
            expandedNodes.remove(nodeIndex)
        } else {
            expandedNodes.insert(nodeIndex)
        }
    }

    private func navigate(to nodeIndex: UInt32) {
        guard let scan else { return }
        currentNode = nodeIndex
        let path = scan.path(of: nodeIndex) ?? ""
        lastClickedPath = path.isEmpty ? "/" : path
        // Expand the path so the highlighted row is visible.
        // We don't auto-expand subtrees — only ancestors of the
        // navigated node.
        var c: UInt32? = scan.parent(of: nodeIndex)
        while let cur = c {
            expandedNodes.insert(cur)
            c = scan.parent(of: cur)
        }
        updateLayout()
    }

    private func rebuildTreeRows() {
        guard let scan else { treeRows = []; return }
        var out: [TreeListRow] = []
        walkForRows(scan: scan, nodeIndex: 0, depth: 0, out: &out)
        treeRows = out
    }

    private func walkForRows(scan: Scan, nodeIndex: UInt32, depth: Int, out: inout [TreeListRow]) {
        let kind = scan.kind(of: nodeIndex)
        let childCount = scan.childCount(of: nodeIndex)
        let isDir = kind == .dir
        let hasChildren = isDir && childCount > 0
        let isExpanded = hasChildren && expandedNodes.contains(nodeIndex)
        let isCurrent = nodeIndex == currentNode
        let id = (UInt64(nodeIndex) << 8) | UInt64(depth & 0xff)
        out.append(TreeListRow(
            id: id, nodeIndex: nodeIndex, depth: depth,
            hasChildren: hasChildren, isExpanded: isExpanded,
            isCurrent: isCurrent,
            isOverflow: false, overflowCount: 0
        ))
        guard isExpanded else { return }
        // Sort children descending by the active metric. Pull
        // the value-per-child once into an array then sort to
        // keep the per-comparison FFI cost down.
        let children = Array(scan.children(of: nodeIndex))
        let scored: [(UInt32, UInt64)] = children.map { ($0, metricValue(for: $0, scan: scan)) }
        let sorted = scored.sorted { $0.1 > $1.1 }
        let visible = sorted.prefix(NativeContentView.treeListChildrenCap)
        for (child, _) in visible {
            walkForRows(scan: scan, nodeIndex: child, depth: depth + 1, out: &out)
        }
        if sorted.count > visible.count {
            let id = (UInt64(nodeIndex) << 8) | UInt64((depth + 1) & 0xff) | 0xff_0000_0000
            out.append(TreeListRow(
                id: id, nodeIndex: Scan.nodeInvalid, depth: depth,
                hasChildren: false, isExpanded: false, isCurrent: false,
                isOverflow: true, overflowCount: sorted.count - visible.count
            ))
        }
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
                    expandedNodes = [0]
                    // If the scan's allocated column is missing,
                    // force the picker back to "Logical" so the
                    // user doesn't see an empty treemap on the
                    // disabled metric.
                    if !result.allocatedAvailable && metric == .allocated {
                        metric = .logical
                    }
                    updateLayout()
                    rebuildTreeRows()
                } else {
                    scan = nil
                    layout = nil
                    treeRows = []
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
