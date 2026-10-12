import AppKit
import ApfsCore
import SwiftUI

// Select the property wrapper, not the SDK 27 State macro. Command Line Tools
// does not ship that macro's SwiftUIMacros plugin. This also supports older SDKs.
private typealias ViewState<Value> = SwiftUI.State<Value>

@MainActor
struct NativeContentView: View {
    @ViewState private var pathInput: String = NSHomeDirectory()
    @ViewState private var scan: Scan?
    @ViewState private var layout: Scan.Layout?
    @ViewState private var scanError: String?
    @ViewState private var scanning: Bool = false
    @ViewState private var adminMode: Bool = false
    @AppStorage(AppPrefs.depthKey) private var depth: Int = 0
    @AppStorage(AppPrefs.threadsKey) private var threads: Int = 0
    @AppStorage(AppPrefs.showFolderTreeKey) private var showFolderTree: Bool = true
    @AppStorage(AppPrefs.showExtensionsKey) private var showExtensions: Bool = true
    @AppStorage(AppPrefs.fdaPromptDismissedKey) private var fdaPromptDismissed: Bool = false
    @ViewState private var metric: Scan.Metric = .logical
    @ViewState private var lastSize: CGSize = .zero
    @StateObject private var browser = BrowserModel()
    @StateObject private var fileActions = FileActions()
    @FocusState private var searchFieldFocused: Bool
    @ViewState private var extSummary: Scan.ExtSummary?

    private var currentNode: UInt32 { browser.currentNode }
    private var scanWorkerCount: UInt32 {
        let count = threads == 0 ? min(AppPrefs.maxWorkerThreads, 4) : threads
        return UInt32(clamping: min(max(count, 1), AppPrefs.maxWorkerThreads))
    }
    private var lastClickedPath: String {
        guard let scan, browser.selectedNode != 0 else { return "" }
        return DisplaySanitizer.sanitiseDisplay(scan.path(of: browser.selectedNode) ?? "")
    }

    @ViewState private var walkSkips: [Scan.WalkSkip] = []

    @ViewState private var scanPhaseLabel: String = "Scanning"
    @ViewState private var scanProgressScanned: UInt64 = 0
    @ViewState private var scanProgressSkipped: UInt64 = 0
    @ViewState private var scanProgressBytes: UInt64 = 0
    @ViewState private var scanProgressElapsedMs: UInt64 = 0
    @ViewState private var scanProgressBytesTotal: UInt64 = 0

    var body: some View {
        VStack(spacing: 0) {
            toolbar
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(VizPalette.panel)
            Divider().background(VizPalette.border)
            if scan != nil && showFolderTree {
                searchField
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(VizPalette.panel)
                Divider().background(VizPalette.border)
            }
            if scan != nil && currentNode != 0 {
                breadcrumbBar
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(VizPalette.panel)
                Divider().background(VizPalette.border)
            }
            VSplitView {
                if showFolderTree || showExtensions {
                    sidePanels
                        .frame(maxWidth: .infinity, minHeight: 120, idealHeight: 220)
                }
                GeometryReader { proxy in
                    ZStack {
                        VizPalette.bg
                        TreemapViewRepresentable(
                            scan: scan,
                            layout: layout,
                            metric: metric,
                            fileActions: fileActions,
                            selectedNode: browser.selectedNode,
                            highlightedNodes: browser.highlightedNodes,
                            onClick: { nodeIndex in
                                handleClick(nodeIndex: nodeIndex)
                            }
                        )
                        if scan == nil && !scanning {
                            initialStatsCard
                        }
                        if let scan, !scanning, scan.itemCount(of: currentNode) == 0 {
                            Text("This folder contains no files.")
                                .foregroundStyle(VizPalette.muted)
                                .allowsHitTesting(false)
                        } else if scan != nil, !scanning, layout?.count == 0 {
                            Text("No items have a size to display for this metric.")
                                .foregroundStyle(VizPalette.muted)
                                .allowsHitTesting(false)
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
            if fileActions.needsRescan {
                HStack {
                    Text("Files changed. Rescan to update sizes and remove stale entries.")
                    Spacer()
                    Button("Rescan") {
                        if let scan { pathInput = scan.sourceRequestedPath }
                        startScan()
                    }
                    .disabled(scanning)
                }
                .font(AppFont.ui(11))
                .padding(8)
                .background(VizPalette.panel)
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
        .navigationTitle(adminMode ? "apfs-fastindex — Administrator" : "apfs-fastindex")
        .focusedSceneValue(\.browserWindowActions, BrowserWindowActions(
            find: scan == nil ? nil : focusSearch,
            scanAsAdministrator: scanning || pathInput.trimmingCharacters(in: .whitespaces).isEmpty
                ? nil : startPrivilegedScan
        ))
        .onChange(of: currentNode) { _ in
            updateLayout()
            rebuildExtSummary()
        }
        .onChange(of: metric) { value in
            browser.setMetric(value)
            updateLayout()
            rebuildExtSummary()
        }
        .onChange(of: depth) { _ in updateLayout() }
    }

    @ViewBuilder private var sidePanels: some View {
        if showFolderTree && showExtensions {
            HSplitView {
                treeListPanel.frame(minWidth: 300, idealWidth: 420, maxWidth: .infinity)
                extListPanel.frame(minWidth: 300, idealWidth: 380, maxWidth: .infinity)
            }
        } else if showFolderTree {
            treeListPanel.frame(maxWidth: .infinity)
        } else if showExtensions {
            extListPanel.frame(maxWidth: .infinity)
        }
    }

    private var treeListPanel: some View {
        FolderTreePanel(scan: scan, browser: browser, actions: fileActions)
    }

    private var extListPanel: some View {
        ExtensionPanel(summary: extSummary, browser: browser) { category in
            showFolderTree = true
            browser.toggleExtension(category)
        }
    }

    private var searchField: some View {
        BrowserSearchField(browser: browser, focus: $searchFieldFocused)
    }

    private func focusSearch() {
        guard scan != nil else { return }
        showFolderTree = true
        DispatchQueue.main.async { searchFieldFocused = true }
    }

    private func rebuildExtSummary() {
        extSummary = scan?.extSummary(rootedAt: currentNode, metric: metric)
    }

    private func installScan(_ result: Scan) {
        scan = result
        if !result.allocatedAvailable { metric = .logical }
        browser.install(result, metric: metric)
        fileActions.install(result)
        walkSkips = result.walkSkips()
        updateLayout()
        rebuildExtSummary()
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
            .accessibilityLabel("Choose folder to scan")
            .disabled(scanning)
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
                .disabled(scanning)

            Picker("", selection: $metric) {
                Text("Logical").tag(Scan.Metric.logical)
                Text("Allocated").tag(Scan.Metric.allocated)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 180)
            .disabled(scan?.allocatedAvailable == false)
            .accessibilityLabel("Size metric")

            Spacer(minLength: 12)

            Button {
                startScan()
            } label: {
                if adminMode {
                    Label("Scan", systemImage: "lock.fill")
                        .frame(minWidth: 72)
                } else {
                    Label("Scan", systemImage: "play.fill")
                        .frame(minWidth: 72)
                }
            }
            .buttonStyle(.borderedProminent)
            .tint(adminMode ? VizPalette.warning : VizPalette.accent)
            .keyboardShortcut(.return, modifiers: .command)
            .disabled(scanning || pathInput.trimmingCharacters(in: .whitespaces).isEmpty)
            .help(adminMode
                ? "Sticky admin mode is on — every Scan runs with administrator privileges."
                : "Scan as the current user.")
        }
    }

    // MARK: - Breadcrumb (visible only when navigated below root)
    private var breadcrumbBar: some View {
        HStack(spacing: 8) {
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(breadcrumbChain, id: \.index) { node in
                            if node.index != 0 { Text("›").foregroundStyle(VizPalette.muted) }
                            Button(node.label) { browser.select(node.index) }
                                .buttonStyle(.plain)
                                .foregroundStyle(node.index == currentNode ? VizPalette.text : VizPalette.accent)
                                .disabled(node.index == currentNode)
                                .id(node.index)
                        }
                    }
                    .font(AppFont.ui(12))
                    .fixedSize()
                }
                .onChange(of: currentNode) { node in proxy.scrollTo(node, anchor: .trailing) }
            }
            Button {
                guard let scan, let parent = scan.parent(of: currentNode) else { return }
                browser.select(parent)
            } label: {
                Label("Up", systemImage: "chevron.up").labelStyle(.iconOnly)
            }
            .buttonStyle(.borderless)
            .help("Up to parent directory (⌘↑)")
            .keyboardShortcut(.upArrow, modifiers: .command)
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
        while let c = cursor {
            let label: String
            if c == 0 {
                let root = scan.sourceRequestedPath
                label = DisplaySanitizer.sanitiseDisplay(root.isEmpty ? "/" : root)
            } else {
                label = DisplaySanitizer.sanitiseDisplay(scan.name(of: c) ?? "?")
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
                // Admin-mode chip (EX-28 follow-up): bound to
                // sticky `adminMode`, not `scan.isAdmin`, so it
                // flips the moment auth completes (before the
                // first scan returns). Once sticky-admin is
                // engaged for the session, every Scan-button
                // press routes through AdminSession's long-lived
                // privileged helper and the chip stays on.
                if adminMode {
                    statusPill("Admin", tint: VizPalette.warning)
                        .help("Scan ran with administrator privileges; "
                          + "TCC-restricted user-data paths are included.")
                }
                // Snapshot-path chip (EX-29 follow-up, implicit
                // opt-in case): user pointed the scanner at a
                // path on a snapshot filesystem
                // (statfs.f_flags & MNT_SNAPSHOT). The data is
                // frozen at the snapshot's transaction — surface
                // it so they aren't confused when totals don't
                // match the live volume.
                if scan.isSnapshotPath {
                    statusPill("Snapshot", tint: VizPalette.accent)
                        .help("Scan target is on a snapshot filesystem. "
                              + "Totals are frozen at the snapshot's "
                              + "transaction; they don't reflect live volume state.")
                }
                Text(totalsText(for: scan))
                    .font(AppFont.ui(12)).monospacedDigit()
                    .foregroundStyle(VizPalette.muted)
                // Walk-skip banner (audit r3 #F1). Shown when
                // the walker recorded any skip — permission
                // denied, mount boundary, depth-cap truncation,
                // DREC cycle. Tooltip lists the first few
                // entries so the user can investigate without
                // a separate UI surface.
                if !walkSkips.isEmpty {
                    statusPill("\(walkSkips.count) elided", tint: VizPalette.warning)
                        .help(walkSkipTooltip)
                }
                // Sparse-files hint. Surfaces when scanned
                // content's logical bytes are at least 2×
                // allocated — the OrbStack-on-laptop case
                // where a single qcow2 reports an 8 TB
                // capacity on a 500 GB drive. Clicking flips
                // the metric picker to Allocated so the
                // treemap shows real disk usage; the cell
                // stops dominating its parent. Hidden when
                // already on Allocated (the user has already
                // resolved it).
                if metric == .logical, scan.sparseFilesSuspected {
                    Button {
                        metric = .allocated
                        updateLayout()
                    } label: {
                        statusPill("Sparse → Allocated", tint: VizPalette.warning)
                    }
                    .buttonStyle(.plain)
                    .help(
                        "Logical bytes (\(formattedBytes(scan.logicalTotal))) are much larger "
                        + "than allocated (\(formattedBytes(scan.allocatedTotal ?? 0))). "
                        + "Sparse files like OrbStack containers and qcow2 disk images "
                        + "report nominal capacity, not actual disk usage. Click to switch "
                        + "the treemap to Allocated for a more honest view."
                    )
                }
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

    /// Multi-line tooltip text for the walk-skip pill. Sums
    /// reasons by category + lists the first few paths so the
    /// user has enough to investigate without a popover.
    /// Sanitised so a crafted skip-path can't spoof the tooltip
    /// (audit #App-2 applies here too).
    private var walkSkipTooltip: String {
        var counts: [String: Int] = [:]
        for s in walkSkips {
            counts[s.reason, default: 0] += 1
        }
        let summary = counts
            .sorted { $0.key < $1.key }
            .map { "\($1) × \($0)" }
            .joined(separator: ", ")
        let sample = walkSkips.prefix(5)
            .map { "  \(DisplaySanitizer.sanitiseDisplay($0.path)) — \($0.reason)" }
            .joined(separator: "\n")
        let more = walkSkips.count > 5 ? "\n  … and \(walkSkips.count - 5) more" : ""
        return "Walker elided \(walkSkips.count) subtree(s): \(summary)\n\(sample)\(more)"
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
        let logical = ByteCountFormatter.string(fromByteCount: Int64(clamping: scan.logicalTotal),
                                                countStyle: .binary)
        let allocatedPart: String
        if let alloc = scan.allocatedTotal {
            allocatedPart = "; allocated \(ByteCountFormatter.string(fromByteCount: Int64(clamping: alloc), countStyle: .binary))"
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
        // Determinate fraction when we have a denominator.
        //
        // Fall back to indeterminate when the running tally
        // overshoots the predicted total by more than ~10 %.
        // A pegged-at-100 % bar is misleading — the user
        // can't tell scanning from stalled. The overshoot
        // case is sparse files (OrbStack qcow2 reporting an
        // 8 TB capacity on a 500 GB drive is the canonical
        // hit) or hard-linked clones double-counted at the
        // logical-bytes layer. Switching to indeterminate
        // honestly signals "still working; predicted total
        // was wrong".
        let fraction: Double? = {
            guard scanProgressBytesTotal > 0 else { return nil }
            let f = Double(scanProgressBytes) / Double(scanProgressBytesTotal)
            if f > 1.10 {
                return nil
            }
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

            // Bytes-scanned ground truth (C): the left-hand
            // number is the running sum of `entry.logical_size`
            // that the walker reports — the same number the
            // treemap shows as "logical" after the scan
            // completes. Shown identically in both bar modes
            // so the user has a stable anchor regardless of
            // whether the bar is determinate.
            //
            // When the denominator is known (volume-root scan,
            // or post-terminal-snap on a subpath scan) we
            // append " / Y · NN%" for context. Otherwise just
            // the running scanned bytes.
            if let fraction {
                Text(
                    "\(formattedBytes(scanProgressBytes)) / \(formattedBytes(scanProgressBytesTotal)) "
                    + "· \(Int(fraction * 100))%"
                )
                .font(AppFont.ui(11)).monospacedDigit()
                .foregroundStyle(VizPalette.text)
            } else {
                Text("\(formattedBytes(scanProgressBytes)) scanned")
                    .font(AppFont.ui(11)).monospacedDigit()
                    .foregroundStyle(VizPalette.text)
            }

            // Items / skipped row. The "Items" cell shows the
            // running entry count — the same number the final
            // treemap reports as `entryCount`. `Skipped` is
            // rendered with value "0" when none have skipped
            // yet, so the row height is stable across the whole
            // scan — the user doesn't get a layout-shift the
            // first time a permission-denied directory appears.
            HStack(spacing: 0) {
                Spacer(minLength: 0)
                fixedMetricCell(label: "Items",
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

    /// Pre-scan volume capacity. File counts are only known after scanning.
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
        ByteCountFormatter.string(fromByteCount: Int64(clamping: n), countStyle: .binary)
    }

    private struct VolumeStats {
        let volumeName: String
        let total: UInt64
        let used: UInt64
        let free: UInt64
    }

    private func volumeStats(for path: String) -> VolumeStats? {
        let p = path.isEmpty ? NSHomeDirectory() : path
        guard let attrs = try? FileManager.default.attributesOfFileSystem(forPath: p) else {
            return nil
        }
        let total = (attrs[.systemSize] as? NSNumber)?.uint64Value ?? 0
        let free = (attrs[.systemFreeSize] as? NSNumber)?.uint64Value ?? 0
        let used = total > free ? total - free : 0
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
            total: total, used: used, free: free
        )
    }

    /// Returns `true` iff `path` is the mount point of its
    /// containing volume — i.e. scanning `path` covers exactly
    /// the bytes `statfs` reports as "used" for that volume.
    /// Drives the progress-bar denominator choice in
    /// `startScan`: volume-root scans get an accurate
    /// determinate bar from byte zero; subpath scans start
    /// indeterminate and snap on terminal.
    ///
    /// Comparison is on `standardizedFileURL` (resolves `.` /
    /// `..` and trailing slashes) so `"/"`, `"/."`, and
    /// `"/Volumes/MyDisk/"` all normalise to their canonical
    /// volume URL before the `==`. Symlink resolution is
    /// deliberately *not* applied — a symlink whose target
    /// happens to be a mount point shouldn't be treated as
    /// the volume root for accounting purposes; the user
    /// asked us to scan the symlink, not the resolved path.
    private func isVolumeRoot(path: String) -> Bool {
        guard !path.isEmpty else { return false }
        let url = URL(fileURLWithPath: path).standardizedFileURL
        // Note: `URLResourceValues`'s property is `.volume`,
        // accessed via the `volumeURLKey` lookup. The Swift
        // shape is consistent with `volumeLocalizedNameKey` →
        // `values.volumeLocalizedName` above.
        guard let values = try? url.resourceValues(forKeys: [.volumeURLKey]),
              let volumeURL = values.volume
        else {
            return false
        }
        return url == volumeURL.standardizedFileURL
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

    private func beginScan(path: String, phase: String) {
        scanError = nil
        scanning = true
        scanPhaseLabel = phase
        scanProgressScanned = 0
        scanProgressSkipped = 0
        scanProgressBytes = 0
        scanProgressElapsedMs = 0
        // A subfolder has no known total until the scanner finishes.
        scanProgressBytesTotal = isVolumeRoot(path: path) ? volumeStats(for: path)?.used ?? 0 : 0
    }

    private func receiveProgress(_ snapshot: Scan.ProgressSnapshot) {
        if scanPhaseLabel == "Authorizing" { scanPhaseLabel = "Scanning (administrator)" }
        scanProgressScanned = snapshot.scanned
        scanProgressSkipped = snapshot.skipped
        scanProgressBytes = snapshot.bytes
        scanProgressElapsedMs = snapshot.elapsedMs
        if snapshot.terminal {
            scanPhaseLabel = "Indexing"
            if scanProgressBytesTotal == 0 { scanProgressBytesTotal = snapshot.bytes }
        }
    }

    private func startPrivilegedScan() {
        let path = pathInput.trimmingCharacters(in: .whitespaces)
        guard !path.isEmpty else { return }
        guard !scanning else { return }
        beginScan(path: path, phase: "Authorizing")
        let workerCount = scanWorkerCount
        DispatchQueue.global(qos: .userInitiated).async {
            let outcome = PrivilegedScan.run(
                path: path,
                threads: workerCount,
                onSessionReady: {
                    // Auth just completed (helper sent its
                    // ready handshake). Engage sticky admin
                    // mode NOW so the title bar and chip flip
                    // immediately, not after the first scan
                    // finishes. The session is reusable for
                    // every subsequent scan; the auth dialog
                    // pops once per app lifetime.
                    DispatchQueue.main.async {
                        adminMode = true
                    }
                },
                onProgress: { snapshot in
                    DispatchQueue.main.async { receiveProgress(snapshot) }
                }
            )
            DispatchQueue.main.async {
                switch outcome {
                case .ok(let result):
                    scanPhaseLabel = "Rendering"
                    installScan(result)
                    scanProgressScanned = result.entryCount
                    scanProgressBytes = result.logicalTotal
                    if scanProgressBytesTotal == 0 {
                        scanProgressBytesTotal = result.logicalTotal
                    }
                    // adminMode was already engaged via
                    // onSessionReady above; idempotent here.
                    adminMode = true
                case .cancelled:
                    break
                case .failed(let message, _):
                    scanError = message
                }
                scanning = false
            }
        }
    }

    private func startScan() {
        guard !scanning else { return }
        // Two paths route through the privileged flow instead of
        // the in-process fallback walker.
        //
        // Sticky admin mode: once the user has elevated for the
        // session, every Scan-button click stays on the
        // privileged path so subsequent scans don't lose
        // admin-only paths.
        if adminMode {
            startPrivilegedScan()
            return
        }
        let path = pathInput.trimmingCharacters(in: .whitespaces)
        guard !path.isEmpty else { return }

        // Full Disk Access gate (unified TCC handling). When
        // FDA is on, every TCC-protected folder is implicitly
        // readable and the scan runs without any prompts. When
        // it's off, we offer a one-time upsell pointing the
        // user at System Settings. The "dismissed" preference
        // lets the user opt out permanently (mid-scan prompts
        // then fire for any folder they haven't authorised
        // individually).
        if !TCCAccess.hasFullDiskAccess && !fdaPromptDismissed {
            switch TCCAccess.showFullDiskAccessExplainer() {
            case .openedSettings:
                // User went to System Settings. The app must
                // be restarted after they add it to the FDA
                // allowlist (kernel caches TCC state per
                // process at launch). Bail out of this scan
                // attempt — they'll come back and click Scan
                // again post-relaunch.
                return
            case .dontAskAgain:
                fdaPromptDismissed = true
                // Fall through to the scan with current
                // (possibly partial) TCC state.
            case .continueWithout:
                // Fall through to the scan; mid-scan prompts
                // may fire for protected dirs they haven't
                // already authorised.
                break
            }
        }

        beginScan(path: path, phase: "Scanning")
        let workerCount = scanWorkerCount
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Scan.fallbackWithProgress(
                path: path,
                threads: workerCount,
                crossMounts: false,
                onProgress: { snapshot in
                    DispatchQueue.main.async { receiveProgress(snapshot) }
                }
            )
            let failure = result == nil ? NativeBridge.lastError() : nil
            DispatchQueue.main.async {
                if let result {
                    // Flip phase to "Rendering" *before* the
                    // synchronous layout pass — for /-scale roots
                    // squarify is fast enough that this is mostly
                    // for symmetry, but it surfaces the post-
                    // index gap rather than letting the spinner
                    // disappear silently.
                    scanPhaseLabel = "Rendering"
                    installScan(result)
                } else {
                    // Pull the Rust-side last-error if there is
                    // one — this is what makes panics and
                    // recoverable scan failures user-visible
                    // instead of surfacing the generic "scan
                    // failed" toast for every cause.
                    if let detail = failure {
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
            maxDepth: UInt32(clamping: depth),
            metric: metric,
            width: Float(lastSize.width),
            height: Float(lastSize.height)
        )
    }

    private func handleClick(nodeIndex: UInt32) {
        browser.select(nodeIndex)
    }

    private func browseForFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = false
        panel.prompt = "Choose"
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
