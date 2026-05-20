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
    /// Treemap depth + worker count live in `Settings` (⌘,). The
    /// `@AppStorage` binding here keeps them reactive: editing
    /// the value in the settings panel re-fires the depth
    /// `onChange` below and re-lays the visible cells without a
    /// rescan.
    @AppStorage(AppPrefs.depthKey) private var depth: Int = 0
    @AppStorage(AppPrefs.threadsKey) private var threads: Int = 0
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
    /// Per-(node, metric) ext-list summary for the right-hand
    /// side panel. Computed in Rust via `Scan.extSummary` and
    /// rebuilt whenever `currentNode` or `metric` changes; on a
    /// typical /Applications-class scan that's well under
    /// 10 ms so we don't bother caching across navigations.
    @State private var extSummary: Scan.ExtSummary?

    // Live scan progress. While `scanning` is true the centered
    // overlay shows `scanPhaseLabel` + the counters below. Phase
    // labels flow:
    //   "Scanning"   — receiving per-tick progress events
    //   "Indexing"   — terminal event fired, tree-build in Rust
    //                  still running (`apfs_scan_directory`
    //                  returns once the tree is built)
    //   "Rendering"  — scan returned; squarify/cells running on
    //                  the main thread
    @State private var scanPhaseLabel: String = "Scanning"
    @State private var scanProgressScanned: UInt64 = 0
    @State private var scanProgressSkipped: UInt64 = 0
    @State private var scanProgressBytes: UInt64 = 0
    @State private var scanProgressElapsedMs: UInt64 = 0
    /// Volume's used-bytes captured at scan start — the
    /// denominator for the determinate progress fraction
    /// (`scanProgressBytes / scanProgressBytesTotal`). When this
    /// is 0 (no volume info) the bar falls back to indeterminate.
    @State private var scanProgressBytesTotal: UInt64 = 0

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
            // Nested splits matching the WizTree layout:
            //   - VSplitView between (tree-list + ext-list) top
            //     half and the treemap bottom half.
            //   - The top half is itself an HSplitView so the
            //     user can drag the boundary between the two
            //     side panels.
            VSplitView {
                HSplitView {
                    treeListPanel
                        .frame(minWidth: 220, idealWidth: 340, maxWidth: .infinity)
                    extListPanel
                        .frame(minWidth: 200, idealWidth: 280, maxWidth: .infinity)
                }
                .frame(maxWidth: .infinity, minHeight: 120, idealHeight: 220)
                GeometryReader { proxy in
                    ZStack {
                        VizPalette.bg
                        TreemapViewRepresentable(
                            scan: scan,
                            layout: layout,
                            metric: metric,
                            onClick: { nodeIndex in
                                handleClick(nodeIndex: nodeIndex)
                            }
                        )
                        if scan == nil && !scanning {
                            initialStatsCard
                        }
                        if scanning {
                            progressOverlay
                        }
                    }
                    .onAppear { resize(to: proxy.size) }
                    .onChange(of: proxy.size) { newSize in resize(to: newSize) }
                }
                .frame(minHeight: 200)
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
        .onChange(of: scan?.entryCount) { _ in
            rebuildTreeRows()
            rebuildExtSummary()
        }
        .onChange(of: currentNode) { _ in
            rebuildTreeRows()
            rebuildExtSummary()
        }
        .onChange(of: metric) { _ in
            rebuildTreeRows()
            rebuildExtSummary()
        }
        .onChange(of: expandedNodes) { _ in rebuildTreeRows() }
        // Settings panel writes both of these; depth re-fires the
        // layout pass without a rescan, threads picks up on the
        // *next* scan.
        .onChange(of: depth) { _ in updateLayout() }
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
            // ScrollViewReader so we can scroll-to-row when
            // `currentNode` changes (treemap → tree-list sync).
            // Each row gets `.id(row.id)` for the lookup.
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(treeRows) { row in
                            treeListRowView(row)
                                .id(row.id)
                        }
                    }
                    .padding(.vertical, 2)
                }
                .onChange(of: currentNode) { newNode in
                    // Find the visible row for the new
                    // currentNode and bring it into view.
                    // The treemap-click path also auto-
                    // expands ancestors in `navigate(to:)` so
                    // the row should always be visible (modulo
                    // the overflow cap).
                    if let id = treeRows.first(where: {
                        !$0.isOverflow && $0.nodeIndex == newNode
                    })?.id {
                        withAnimation(.easeOut(duration: 0.18)) {
                            proxy.scrollTo(id, anchor: .center)
                        }
                    }
                }
            }
        }
        .background(VizPalette.panel)
    }

    @ViewBuilder
    private func paneHeader(_ title: String) -> some View {
        HStack {
            Text(title.uppercased())
                .font(AppFont.ui(10, weight: .semibold))
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
                .font(AppFont.ui(9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .tracking(0.4)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text("% / parent")
                .font(AppFont.ui(9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .frame(width: 80, alignment: .trailing)
            Text("Size")
                .font(AppFont.ui(9, weight: .semibold))
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
                    .font(AppFont.ui(11)).monospacedDigit()
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
                        .font(AppFont.ui(9))
                        .foregroundStyle(row.hasChildren ? VizPalette.muted : .clear)
                        .frame(width: 14, height: 14, alignment: .center)
                }
                .buttonStyle(.plain)
                .disabled(!row.hasChildren)

                // Kind icon — quick visual telling files,
                // symlinks, dirs apart.
                let kind = scan?.kind(of: row.nodeIndex) ?? .invalid
                Image(systemName: rowIconName(kind: kind))
                    .font(AppFont.ui(11))
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
                            .font(AppFont.ui(12)).monospacedDigit()
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text(percentText(for: row))
                            .font(AppFont.ui(10)).monospacedDigit()
                            .foregroundStyle(VizPalette.muted)
                            .frame(width: 80, alignment: .trailing)
                        Text(sizeText(for: row))
                            .font(AppFont.ui(10)).monospacedDigit()
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

    // MARK: - Ext-list panel

    private var extListPanel: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Text("BY EXTENSION")
                    .font(AppFont.ui(10, weight: .semibold))
                    .foregroundStyle(VizPalette.muted)
                    .tracking(0.4)
                if let summary = extSummary, summary.count > 0 {
                    Text(extSubtitle(summary: summary))
                        .font(AppFont.ui(11))
                        .foregroundStyle(VizPalette.muted)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(VizPalette.bg)
            HStack(spacing: 0) {
                Spacer().frame(width: 18)
                Text("Extension")
                    .font(AppFont.ui(9, weight: .semibold))
                    .foregroundStyle(VizPalette.muted)
                    .tracking(0.4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text("% / view")
                    .font(AppFont.ui(9, weight: .semibold))
                    .foregroundStyle(VizPalette.muted)
                    .frame(width: 70, alignment: .trailing)
                Text("Size")
                    .font(AppFont.ui(9, weight: .semibold))
                    .foregroundStyle(VizPalette.muted)
                    .frame(width: 70, alignment: .trailing)
                    .padding(.leading, 4)
                Text("Files")
                    .font(AppFont.ui(9, weight: .semibold))
                    .foregroundStyle(VizPalette.muted)
                    .frame(width: 50, alignment: .trailing)
                    .padding(.leading, 4)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(VizPalette.bg)
            Divider().background(VizPalette.border)
            if let summary = extSummary {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(summary.allRows()) { row in
                            extListRowView(row, total: summary.totalValue)
                        }
                    }
                    .padding(.vertical, 2)
                }
            } else {
                Spacer()
                Text("(scan a folder to see breakdown)")
                    .font(AppFont.ui(11))
                    .foregroundStyle(VizPalette.muted)
                    .frame(maxWidth: .infinity)
                    .padding()
                Spacer()
            }
        }
        .background(VizPalette.panel)
    }

    private func extSubtitle(summary: Scan.ExtSummary) -> String {
        let total = ByteCountFormatter.string(
            fromByteCount: Int64(summary.totalValue),
            countStyle: .binary
        )
        if summary.anyUnclaimed {
            return "\(summary.count) ext · \(total) · some unclaimed"
        }
        return "\(summary.count) ext · \(total)"
    }

    @ViewBuilder
    private func extListRowView(_ row: Scan.ExtSummary.Row, total: UInt64) -> some View {
        HStack(spacing: 0) {
            // Colour chip matching the JS canvas leaf palette so
            // the panel chip → treemap rect colour-binds for
            // the user.
            Rectangle()
                .fill(extChipColor(ext: row.ext))
                .frame(width: 10, height: 10)
                .padding(.leading, 6)
                .padding(.trailing, 6)
            Text(row.ext)
                .font(AppFont.ui(11)).monospacedDigit()
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text(extPercentText(row: row, total: total))
                .font(AppFont.ui(10)).monospacedDigit()
                .foregroundStyle(VizPalette.muted)
                .frame(width: 70, alignment: .trailing)
            Text(extSizeText(row: row))
                .font(AppFont.ui(10)).monospacedDigit()
                .foregroundStyle(VizPalette.text)
                .frame(width: 70, alignment: .trailing)
                .padding(.leading, 4)
            Text(row.fileCount.formatted())
                .font(AppFont.ui(10)).monospacedDigit()
                .foregroundStyle(VizPalette.muted)
                .frame(width: 50, alignment: .trailing)
                .padding(.leading, 4)
        }
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
    }

    private func extPercentText(row: Scan.ExtSummary.Row, total: UInt64) -> String {
        guard total > 0 else { return "—" }
        let v: UInt64 = metric == .allocated
            ? (row.valueAllocated ?? 0)
            : row.valueLogical
        let pct = Double(v) / Double(total) * 100.0
        return String(format: "%.1f%%", pct)
    }

    private func extSizeText(row: Scan.ExtSummary.Row) -> String {
        if metric == .allocated {
            guard let alloc = row.valueAllocated else { return "unclaimed" }
            return ByteCountFormatter.string(fromByteCount: Int64(alloc), countStyle: .binary)
        }
        return ByteCountFormatter.string(fromByteCount: Int64(row.valueLogical),
                                         countStyle: .binary)
    }

    /// Subset of the JS `EXT_COLORS` palette so the chip beside
    /// each row reads the same colour the leaf rects render
    /// in the treemap. Unknown extensions hash to grey for now
    /// (a future commit can FNV-1a → HSL like the canvas-era
    /// `hashColor` for full colour parity).
    private func extChipColor(ext: String) -> Color {
        let key = ext.hasPrefix(".") ? String(ext.dropFirst()) : ext
        switch key.lowercased() {
        case "txt", "md": return Color(red: 0xa0/255, green: 0xc4/255, blue: 0xff/255)
        case "rs": return Color(red: 0xff/255, green: 0xc0/255, blue: 0x9f/255)
        case "py": return Color(red: 0xff/255, green: 0xd6/255, blue: 0xa5/255)
        case "js", "ts", "tsx", "jsx": return Color(red: 0xff/255, green: 0xe0/255, blue: 0x66/255)
        case "json": return Color(red: 0xf4/255, green: 0xd3/255, blue: 0x5e/255)
        case "html": return Color(red: 0xff/255, green: 0x8f/255, blue: 0xab/255)
        case "css": return Color(red: 0xca/255, green: 0xff/255, blue: 0xbf/255)
        case "c", "cpp", "h", "hpp": return Color(red: 0xbd/255, green: 0xb2/255, blue: 0xff/255)
        case "swift": return Color(red: 0xfd/255, green: 0xb5/255, blue: 0xa5/255)
        case "go": return Color(red: 0x9b/255, green: 0xf6/255, blue: 0xff/255)
        case "rb": return Color(red: 0xff/255, green: 0xb3/255, blue: 0xc1/255)
        case "png", "jpg", "jpeg", "gif", "webp", "heic", "svg", "icns":
            return Color(red: 0x8e/255, green: 0xca/255, blue: 0xe6/255)
        case "mp4", "mov", "mp3", "wav", "m4a", "flac":
            return Color(red: 0xb3/255, green: 0x88/255, blue: 0xeb/255)
        case "pdf", "doc", "docx", "pages":
            return Color(red: 0xef/255, green: 0x47/255, blue: 0x6f/255)
        case "zip", "tar", "gz", "bz2", "dmg", "iso":
            return Color(red: 0xad/255, green: 0xb5/255, blue: 0xbd/255)
        case "app", "framework", "dylib", "so":
            return Color(red: 0xff/255, green: 0xaf/255, blue: 0xcc/255)
        default:
            return VizPalette.muted
        }
    }

    private func rebuildExtSummary() {
        guard let scan else { extSummary = nil; return }
        extSummary = scan.extSummary(rootedAt: currentNode, metric: metric)
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
                    .font(AppFont.ui(14))
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

            Spacer(minLength: 12)

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
                    .font(AppFont.ui(12))
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
                            .font(AppFont.ui(12)).monospacedDigit()
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
                    .font(AppFont.ui(11))
                    .foregroundStyle(.red)
            } else if let scan {
                statusPill(scan.sourceKind.isEmpty ? "fallback" : scan.sourceKind,
                           tint: VizPalette.accent)
                Text(totalsText(for: scan))
                    .font(AppFont.ui(12)).monospacedDigit()
                    .foregroundStyle(VizPalette.muted)
                if !lastClickedPath.isEmpty {
                    Spacer()
                    Text(lastClickedPath)
                        .font(AppFont.ui(12)).monospacedDigit()
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
                    .font(AppFont.ui(11))
                    .foregroundStyle(VizPalette.muted)
                Spacer()
            }
        }
    }

    @ViewBuilder
    private func statusPill(_ text: String, tint: Color) -> some View {
        Text(text)
            .font(AppFont.ui(11))
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

    // MARK: - Progress overlay

    /// Centered card shown over the treemap while a scan is in
    /// flight. Reads `scanPhaseLabel`, `scanProgressScanned`,
    /// `scanProgressSkipped`, `scanProgressElapsedMs` — all
    /// updated on the main queue from the FFI progress callback
    /// (see `startScan()`).
    /// Fixed island width — the card is centered in the
    /// available rect (the parent ZStack's default alignment is
    /// `.center`) and never reflows as counters scale. The
    /// per-row layout below uses fixed-width columns for the
    /// same reason — digits growing from 1 to 1,000,000 no
    /// longer push the island around.
    private static let progressIslandWidth: CGFloat = 380

    private var progressOverlay: some View {
        // Determinate fraction when we have a denominator;
        // capped at 1.0 because subdir scans can occasionally
        // overshoot used-bytes (sparse files, hard-linked clones
        // counted twice — both rare but possible).
        let fraction: Double? = {
            guard scanProgressBytesTotal > 0 else { return nil }
            let f = Double(scanProgressBytes) / Double(scanProgressBytesTotal)
            return min(max(f, 0), 1)
        }()

        return VStack(spacing: 12) {
            Text(scanPhaseLabel)
                .font(AppFont.ui(14, weight: .semibold))
                .foregroundStyle(VizPalette.text)
            Text(formattedElapsed(ms: scanProgressElapsedMs))
                .font(AppFont.ui(12)).monospacedDigit()
                .foregroundStyle(VizPalette.muted)

            // Determinate / indeterminate bar — same height
            // either way so the island doesn't reflow.
            if let fraction {
                ProgressView(value: fraction)
                    .progressViewStyle(.linear)
                    .tint(VizPalette.accent)
            } else {
                ProgressView()
                    .progressViewStyle(.linear)
                    .tint(VizPalette.accent)
            }

            // Single line of "X / Y bytes (NN%)" — explicit
            // numbers so the user can see real progress beyond
            // the bar. Hidden when we have no denominator.
            if let fraction {
                Text(
                    "\(formattedBytes(scanProgressBytes)) / \(formattedBytes(scanProgressBytesTotal)) "
                    + "· \(Int(fraction * 100))%"
                )
                .font(AppFont.ui(11)).monospacedDigit()
                .foregroundStyle(VizPalette.text)
            } else {
                Text(formattedBytes(scanProgressBytes))
                    .font(AppFont.ui(11)).monospacedDigit()
                    .foregroundStyle(VizPalette.text)
            }

            // Items / skipped row. `Skipped` is rendered with
            // value "0" when none have skipped yet, so the row
            // height is stable across the whole scan — the user
            // doesn't get a layout-shift the first time a
            // permission-denied directory appears.
            HStack(spacing: 0) {
                Spacer(minLength: 0)
                fixedMetricCell(label: "Scanned",
                                value: scanProgressScanned.formatted(),
                                width: 130)
                fixedMetricCell(label: "Skipped",
                                value: scanProgressSkipped.formatted(),
                                width: 130)
                Spacer(minLength: 0)
            }

            // Truncated path — fixed width matches the card so
            // the long-path case doesn't widen anything.
            Text(pathInput.isEmpty ? " " : pathInput)
                .font(AppFont.ui(10))
                .foregroundStyle(VizPalette.muted)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .center)
        }
        .padding(.horizontal, 22)
        .padding(.vertical, 16)
        .frame(width: NativeContentView.progressIslandWidth)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(VizPalette.panel)
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(VizPalette.border, lineWidth: 1)
                )
                .shadow(color: .black.opacity(0.35), radius: 14, x: 0, y: 6)
        )
    }

    /// Stable-width variant of `metricCell` — pins the column to
    /// `width` so a 1-digit count and a 7-digit count occupy the
    /// same footprint. Used inside the centered progress island
    /// where any width shift is visible as a layout jitter.
    @ViewBuilder
    private func fixedMetricCell(label: String, value: String, width: CGFloat) -> some View {
        VStack(spacing: 1) {
            Text(label.uppercased())
                .font(AppFont.ui(9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .tracking(0.4)
            Text(value)
                .font(AppFont.ui(13)).monospacedDigit()
                .foregroundStyle(VizPalette.text)
        }
        .frame(width: width)
    }

    // MARK: - Initial-view stats card

    /// Pre-scan welcome card. Shows the volume's total / used /
    /// free capacity (and a placeholder item count if the
    /// filesystem reports `f_files`, which APFS doesn't but HFS+
    /// / FAT-mounted images would). Helps the user calibrate
    /// "how big a scan am I about to run".
    private var initialStatsCard: some View {
        let stats = volumeStats(for: pathInput)
        return VStack(alignment: .leading, spacing: 14) {
            Text("Ready to scan")
                .font(AppFont.ui(15, weight: .semibold))
                .foregroundStyle(VizPalette.text)
            if let stats {
                HStack(spacing: 24) {
                    statColumn(label: "Volume",
                               value: stats.volumeName.isEmpty ? "—" : stats.volumeName)
                    statColumn(label: "Total",
                               value: formattedBytes(stats.total))
                    statColumn(label: "Used",
                               value: formattedBytes(stats.used))
                    statColumn(label: "Free",
                               value: formattedBytes(stats.free))
                    statColumn(label: "Items",
                               value: stats.files.map { $0.formatted() } ?? "—")
                }
                if stats.total > 0 {
                    GeometryReader { geom in
                        ZStack(alignment: .leading) {
                            RoundedRectangle(cornerRadius: 3)
                                .fill(VizPalette.border)
                            RoundedRectangle(cornerRadius: 3)
                                .fill(VizPalette.accent.opacity(0.85))
                                .frame(width: geom.size.width * CGFloat(Double(stats.used) / Double(stats.total)))
                        }
                    }
                    .frame(height: 6)
                }
            } else {
                Text("(no volume info available for this path)")
                    .font(AppFont.ui(11))
                    .foregroundStyle(VizPalette.muted)
            }
            Text("Press ⌘↩ or click Scan to index this directory.")
                .font(AppFont.ui(11))
                .foregroundStyle(VizPalette.muted)
        }
        .padding(.horizontal, 22)
        .padding(.vertical, 18)
        .frame(maxWidth: 640)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(VizPalette.panel)
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(VizPalette.border, lineWidth: 1)
                )
                .shadow(color: .black.opacity(0.35), radius: 14, x: 0, y: 6)
        )
    }

    @ViewBuilder
    private func statColumn(label: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased())
                .font(AppFont.ui(9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .tracking(0.4)
            Text(value)
                .font(AppFont.ui(13)).monospacedDigit()
                .foregroundStyle(VizPalette.text)
        }
    }

    private func formattedBytes(_ n: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .binary)
    }

    /// Result of `volumeStats(for:)` — `total/used/free` are byte
    /// counts; `files` is the filesystem's reported node count
    /// (often 0 on APFS — present here for HFS+ / FAT volumes).
    private struct VolumeStats {
        let volumeName: String
        let total: UInt64
        let used: UInt64
        let free: UInt64
        let files: UInt64?
    }

    private func volumeStats(for path: String) -> VolumeStats? {
        let p = path.isEmpty ? NSHomeDirectory() : path
        guard let attrs = try? FileManager.default.attributesOfFileSystem(forPath: p) else {
            return nil
        }
        let total = (attrs[.systemSize] as? NSNumber)?.uint64Value ?? 0
        let free = (attrs[.systemFreeSize] as? NSNumber)?.uint64Value ?? 0
        let used = total > free ? total - free : 0
        // `.systemNodes` is `statvfs::f_files`. APFS reports 0
        // (variable inode count); treat 0 as "unknown" upstream.
        let filesRaw = (attrs[.systemNodes] as? NSNumber)?.uint64Value ?? 0
        let files: UInt64? = filesRaw > 0 ? filesRaw : nil

        // Resolve the human-readable volume name via the URL
        // resource keys (the FileManager attributes don't expose
        // it directly).
        let url = URL(fileURLWithPath: p)
        var volumeName = ""
        if let values = try? url.resourceValues(forKeys: [.volumeLocalizedNameKey]),
           let name = values.volumeLocalizedName {
            volumeName = name
        }
        return VolumeStats(
            volumeName: volumeName,
            total: total, used: used, free: free, files: files
        )
    }

    @ViewBuilder
    private func metricCell(label: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label.uppercased())
                .font(AppFont.ui(9, weight: .semibold))
                .foregroundStyle(VizPalette.muted)
                .tracking(0.4)
            Text(value)
                .font(AppFont.ui(13)).monospacedDigit()
                .foregroundStyle(VizPalette.text)
        }
    }

    /// Stopwatch-style `H:MM:SS` (omitting the hour part when
    /// elapsed is < 1 h, which is the common case).
    private func formattedElapsed(ms: UInt64) -> String {
        let totalSec = Int(ms / 1000)
        let h = totalSec / 3600
        let m = (totalSec % 3600) / 60
        let s = totalSec % 60
        if h > 0 {
            return String(format: "%d:%02d:%02d", h, m, s)
        }
        return String(format: "%d:%02d", m, s)
    }

    // MARK: - Scan + layout flow

    private func startScan() {
        let path = pathInput.trimmingCharacters(in: .whitespaces)
        guard !path.isEmpty else { return }
        scanError = nil
        scanning = true
        scanPhaseLabel = "Scanning"
        scanProgressScanned = 0
        scanProgressSkipped = 0
        scanProgressBytes = 0
        scanProgressElapsedMs = 0
        // Snapshot the volume's used bytes as the progress
        // denominator. Whole-volume scans land near 100%; subdir
        // scans terminate before the bar fills (the bar
        // disappears when `scanning` flips false, so an
        // under-100% finish is invisible).
        scanProgressBytesTotal = volumeStats(for: path)?.used ?? 0
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Scan.fallbackWithProgress(
                path: path,
                threads: UInt32(threads),
                crossMounts: false,
                onProgress: { snapshot in
                    // Marshal off the Rust progress thread onto
                    // the main queue before touching SwiftUI
                    // state. The terminal event flips the phase
                    // to "Indexing" (tree-build runs after the
                    // walker terminates inside the same FFI call).
                    DispatchQueue.main.async {
                        scanProgressScanned = snapshot.scanned
                        scanProgressSkipped = snapshot.skipped
                        scanProgressBytes = snapshot.bytes
                        scanProgressElapsedMs = snapshot.elapsedMs
                        if snapshot.terminal {
                            scanPhaseLabel = "Indexing"
                        }
                    }
                }
            )
            DispatchQueue.main.async {
                if let result {
                    // Flip phase to "Rendering" *before* the
                    // synchronous layout pass — for /-scale roots
                    // squarify is fast enough that this is mostly
                    // for symmetry, but it surfaces the post-
                    // index gap rather than letting the spinner
                    // disappear silently.
                    scanPhaseLabel = "Rendering"
                    scan = result
                    currentNode = 0
                    lastClickedPath = ""
                    expandedNodes = [0]
                    if !result.allocatedAvailable && metric == .allocated {
                        metric = .logical
                    }
                    updateLayout()
                    rebuildTreeRows()
                    rebuildExtSummary()
                } else {
                    scan = nil
                    layout = nil
                    treeRows = []
                    extSummary = nil
                    // Pull the Rust-side last-error if there is
                    // one — this is what makes panics and
                    // recoverable scan failures user-visible
                    // instead of surfacing the generic "scan
                    // failed" toast for every cause.
                    if let detail = NativeBridge.lastError() {
                        scanError = "\(path): \(detail)"
                    } else {
                        scanError = "scan failed for \(path)"
                    }
                }
                scanning = false
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
            maxDepth: UInt32(depth),
            metric: metric,
            width: Float(lastSize.width),
            height: Float(lastSize.height)
        )
    }

    private func handleClick(nodeIndex: UInt32) {
        guard let scan else { return }
        if scan.childCount(of: nodeIndex) > 0 {
            // Treemap → drill in. Reuse `navigate(to:)` so the
            // tree-list sync (expand ancestors, scroll-to-row,
            // highlight) is the same path the side-panel click
            // takes.
            navigate(to: nodeIndex)
        } else {
            // Leaf (file / symlink): surface the path but don't
            // drill — the cell isn't a navigable container.
            let path = scan.path(of: nodeIndex) ?? ""
            lastClickedPath = path.isEmpty ? "/" : path
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
