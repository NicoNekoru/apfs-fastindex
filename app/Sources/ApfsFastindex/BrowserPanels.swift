import ApfsCore
import SwiftUI

@MainActor
struct BrowserSearchField: View {
    @ObservedObject var browser: BrowserModel
    let focus: FocusState<Bool>.Binding

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(VizPalette.muted)
            TextField(
                browser.extensionFilter == nil ? "Search all names (⌘F)" : "Search names in this file type",
                text: $browser.query
            )
            .textFieldStyle(.plain)
            .focused(focus)
            .accessibilityLabel("Search file names")
            if let category = browser.extensionFilter {
                Button {
                    browser.toggleExtension(category)
                } label: {
                    Label(DisplaySanitizer.sanitiseDisplay(category), systemImage: "xmark.circle.fill")
                }
                .buttonStyle(.borderless)
                .help("Clear file-type filter")
            }
            if browser.filtering {
                Text("Searching…").foregroundStyle(VizPalette.muted)
            } else if browser.isFiltered {
                Text("\(browser.visibleMatchCount) of \(browser.matchCount) matches")
                    .foregroundStyle(VizPalette.muted)
            } else if !browser.query.isEmpty {
                Text("Type 2+ characters").foregroundStyle(VizPalette.muted)
            }
            if !browser.query.isEmpty || browser.extensionFilter != nil {
                Button {
                    browser.clearFilters()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.plain)
                .help("Clear filters (Esc)")
                .accessibilityLabel("Clear filters")
                .keyboardShortcut(.cancelAction)
            }
        }
        .font(AppFont.ui(12))
        .padding(8)
        .background(VizPalette.bg)
        .overlay(
            RoundedRectangle(cornerRadius: 5).stroke(
                focus.wrappedValue ? VizPalette.accent : VizPalette.border))
    }
}

@MainActor
struct FolderTreePanel: View {
    let scan: Scan?
    @ObservedObject var browser: BrowserModel
    let actions: FileActions

    var body: some View {
        VStack(spacing: 0) {
            panelHeading(browser.isFiltered ? "Matching items" : "Folder tree")
            HStack {
                Text("Name").frame(maxWidth: .infinity, alignment: .leading)
                Text("% / parent").frame(width: 80, alignment: .trailing)
                Text("Size").frame(width: 70, alignment: .trailing)
            }
            .font(AppFont.ui(10))
            .foregroundStyle(VizPalette.muted)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            Divider()
            if let scan {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ForEach(browser.rows) { row in
                                treeRow(row, scan: scan).id(row.id)
                            }
                            if browser.rows.isEmpty {
                                panelMessage(
                                    browser.filtering
                                        ? "Searching…"
                                        : "No matching items. Try another name or clear the filters.")
                            }
                        }
                        .padding(.vertical, 3)
                    }
                    .onChange(of: browser.selectedNode) { node in
                        proxy.scrollTo(BrowserModel.Row.ID.node(node), anchor: .center)
                    }
                    .onChange(of: browser.isFiltered) { filtered in
                        if !filtered {
                            proxy.scrollTo(BrowserModel.Row.ID.node(browser.selectedNode), anchor: .center)
                        }
                    }
                }
            } else {
                panelMessage("Scan a folder to browse its contents.")
            }
        }
        .background(VizPalette.panel)
    }

    @ViewBuilder private func treeRow(_ row: BrowserModel.Row, scan: Scan) -> some View {
        if row.remaining > 0 {
            Button("Show more (\(row.remaining.formatted()) remaining)") { browser.showMore(in: row.node) }
                .buttonStyle(.plain)
                .font(AppFont.ui(11))
                .foregroundStyle(VizPalette.accent)
                .padding(.leading, CGFloat(row.depth * 14 + 22))
                .padding(.vertical, 5)
        } else {
            HStack(spacing: 4) {
                Color.clear.frame(width: CGFloat(row.depth * 14), height: 1)
                Button {
                    browser.toggleExpansion(row.node)
                } label: {
                    Image(systemName: row.expanded ? "chevron.down" : "chevron.right")
                        .font(AppFont.ui(9))
                        .frame(width: 14, height: 18)
                        .foregroundStyle(row.hasChildren ? VizPalette.muted : .clear)
                }
                .buttonStyle(.plain)
                .disabled(!row.hasChildren)
                .accessibilityLabel(row.expanded ? "Collapse folder" : "Expand folder")
                .accessibilityHidden(!row.hasChildren)
                Button {
                    browser.select(row.node, revealInParent: true)
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: icon(scan.kind(of: row.node)))
                            .foregroundStyle(
                                scan.kind(of: row.node) == .dir ? VizPalette.warning : VizPalette.muted
                            )
                            .frame(width: 14)
                        Text(displayName(row.node, scan: scan))
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text(percent(row.node, scan: scan))
                            .foregroundStyle(VizPalette.muted)
                            .frame(width: 80, alignment: .trailing)
                        Text(size(row.node, scan: scan))
                            .frame(width: 70, alignment: .trailing)
                    }
                    .font(AppFont.ui(11)).monospacedDigit()
                    .padding(.vertical, 3)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 6)
            .background(row.node == browser.selectedNode ? VizPalette.accent.opacity(0.22) : .clear)
            .contentShape(Rectangle())
            .help(DisplaySanitizer.sanitiseDisplay(scan.path(of: row.node) ?? scan.sourceRequestedPath))
            .contextMenu {
                if let target = actions.target(node: row.node, scan: scan) {
                    FileActionMenu(actions: actions, target: target)
                }
            }
        }
    }

    private func displayName(_ node: UInt32, scan: Scan) -> String {
        let raw =
            node == 0
            ? scan.sourceRequestedPath
            : (browser.isFiltered ? scan.path(of: node) : scan.name(of: node)) ?? "?"
        return DisplaySanitizer.sanitiseDisplay(raw)
    }

    private func value(_ node: UInt32, scan: Scan) -> UInt64? {
        browser.metric == .allocated ? scan.valueAllocated(of: node) : scan.valueLogical(of: node)
    }

    private func size(_ node: UInt32, scan: Scan) -> String {
        guard let bytes = value(node, scan: scan) else { return "unclaimed" }
        return formatBytes(bytes)
    }

    private func percent(_ node: UInt32, scan: Scan) -> String {
        guard let parent = scan.parent(of: node), let total = value(parent, scan: scan), total > 0,
            let bytes = value(node, scan: scan)
        else { return "—" }
        return String(format: "%.1f%%", Double(bytes) / Double(total) * 100)
    }

    private func icon(_ kind: Scan.NodeKind) -> String {
        switch kind {
        case .dir: return "folder.fill"
        case .symlink: return "arrow.forward.circle"
        default: return "doc"
        }
    }
}

@MainActor
struct ExtensionPanel: View {
    let summary: Scan.ExtSummary?
    @ObservedObject var browser: BrowserModel
    let onSelect: (String) -> Void

    var body: some View {
        VStack(spacing: 0) {
            panelHeading(
                "By extension",
                detail: summary.map {
                    "\($0.count) types · \(formatBytes($0.totalValue))"
                        + (browser.metric == .allocated && $0.anyUnclaimed ? " · some unclaimed" : "")
                })
            HStack(spacing: 4) {
                Text("Extension").frame(maxWidth: .infinity, alignment: .leading)
                Text("% / view").frame(width: 70, alignment: .trailing)
                Text("Size").frame(width: 70, alignment: .trailing)
                Text("Files").frame(width: 55, alignment: .trailing)
            }
            .font(AppFont.ui(10))
            .foregroundStyle(VizPalette.muted)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            Divider()
            if let summary, summary.count > 0 {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(summary.allRows(), id: \.ext) { row in
                            extensionRow(row, total: summary.totalValue)
                        }
                    }
                    .padding(.vertical, 3)
                }
            } else {
                panelMessage(
                    summary == nil ? "Scan a folder to see file types." : "This folder contains no files.")
            }
        }
        .background(VizPalette.panel)
    }

    private func extensionRow(_ row: Scan.ExtSummary.Row, total: UInt64) -> some View {
        let value = browser.metric == .allocated ? row.valueAllocated : row.valueLogical
        let selected = browser.extensionFilter == row.ext
        return Button {
            onSelect(row.ext)
        } label: {
            HStack(spacing: 4) {
                Rectangle().fill(VizPalette.color(rgb: value == nil ? 0x3b3f4a : row.fillRGB)).frame(
                    width: 10, height: 10)
                Text(DisplaySanitizer.sanitiseDisplay(row.ext))
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text(
                    value.flatMap {
                        total > 0 ? String(format: "%.1f%%", Double($0) / Double(total) * 100) : nil
                    } ?? "—"
                )
                .foregroundStyle(VizPalette.muted)
                .frame(width: 70, alignment: .trailing)
                Text(value.map(formatBytes) ?? "unclaimed").frame(width: 70, alignment: .trailing)
                Text(row.fileCount.formatted())
                    .foregroundStyle(VizPalette.muted)
                    .frame(width: 55, alignment: .trailing)
            }
            .font(AppFont.ui(11)).monospacedDigit()
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(selected ? VizPalette.accent.opacity(0.22) : .clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(
            selected
                ? "Clear this file-type filter"
                : "Show these files in the current folder and highlight them in the treemap"
        )
        .accessibilityValue(selected ? "Selected" : "")
    }
}

private func panelHeading(_ title: String, detail: String? = nil) -> some View {
    HStack {
        Text(title.uppercased()).font(AppFont.ui(10, weight: .semibold))
        if let detail { Text(detail).font(AppFont.ui(10)).lineLimit(1) }
        Spacer(minLength: 0)
    }
    .foregroundStyle(VizPalette.muted)
    .frame(height: 14)
    .padding(.horizontal, 10)
    .padding(.vertical, 6)
    .background(VizPalette.bg)
}

private func panelMessage(_ text: String) -> some View {
    Text(text)
        .font(AppFont.ui(12))
        .foregroundStyle(VizPalette.muted)
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(20)
}

private func formatBytes(_ value: UInt64) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .binary)
}
