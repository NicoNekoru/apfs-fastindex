import Combine
import Foundation

public enum BrowserMetric: UInt32 {
    case logical = 0
    case allocated = 1
}
public enum BrowserNodeKind: UInt32 {
    case dir = 0
    case file = 1
    case symlink = 2
    case other = 3
    case invalid = 0xff
}

/// Read-only access to an index. Implementations must permit concurrent reads.
public protocol BrowserTree: AnyObject {
    func kind(of node: UInt32) -> BrowserNodeKind
    func name(of node: UInt32) -> String?
    func parent(of node: UInt32) -> UInt32?
    /// The buffer stays valid for the lifetime of this tree.
    func children(of node: UInt32) -> UnsafeBufferPointer<UInt32>
    func valueLogical(of node: UInt32) -> UInt64
    func valueAllocated(of node: UInt32) -> UInt64?
    func searchMatches(query: String) -> [UInt32]
}

/// Matches Rust's ext_summary::extension_of, including its ASCII-only folding.
public enum FileCategory {
    public static func key(name: String, kind: BrowserNodeKind) -> String {
        switch kind {
        case .dir: return "(dir)"
        case .symlink: return "(symlink)"
        case .other, .invalid: return "(other)"
        case .file:
            let bytes = Array(name.utf8)
            guard let dot = bytes.lastIndex(of: 46), dot > 0, dot < bytes.count - 1 else {
                return "(no ext)"
            }
            return String(
                decoding: bytes[dot...].map { (65...90).contains($0) ? $0 + 32 : $0 }, as: UTF8.self)
        }
    }
}

/// Navigation, pagination and filtering shared by the tree and treemap.
/// The scan owns the index. This model only retains it while browsing or searching.
@MainActor
public final class BrowserModel: ObservableObject {
    public struct Row: Identifiable {
        public enum ID: Hashable {
            case node(UInt32)
            case more(UInt32)
        }
        public let id: ID
        public let node: UInt32
        public let depth: Int
        public let hasChildren: Bool
        public let expanded: Bool
        public let remaining: Int
    }

    @Published public private(set) var currentNode: UInt32 = 0
    @Published public private(set) var selectedNode: UInt32 = 0
    @Published public private(set) var rows: [Row] = []
    @Published public var query = "" { didSet { if query != oldValue { scheduleFilter() } } }
    @Published public private(set) var extensionFilter: String?
    @Published public private(set) var filtering = false
    @Published public private(set) var matchCount = 0
    @Published public private(set) var visibleMatchCount = 0
    /// All category matches, not just the paginated rows, for treemap highlighting.
    @Published public private(set) var highlightedNodes: Set<UInt32>?
    public private(set) var metric: BrowserMetric = .logical

    private var tree: (any BrowserTree)?
    private var expanded: Set<UInt32> = [0]
    private var limits: [UInt32: Int] = [:]
    private var sortedChildren: [UInt32: [UInt32]] = [:]
    private var matches: [UInt32] = []
    private var categoryCache: (root: UInt32, key: String, nodes: Set<UInt32>)?
    private var matchLimit = pageSize
    private var filterTask: Task<Void, Never>?
    private var generation = 0
    private var changingNavigation = false
    public static let pageSize = 400

    public init() {}

    deinit { filterTask?.cancel() }

    public var isFiltered: Bool {
        extensionFilter != nil || query.trimmingCharacters(in: .whitespacesAndNewlines).count >= 2
    }

    public func install(_ tree: any BrowserTree, metric: BrowserMetric) {
        changingNavigation = true
        self.tree = tree
        self.metric = metric
        currentNode = 0
        selectedNode = 0
        expanded = [0]
        limits = [:]
        sortedChildren = [:]
        categoryCache = nil
        resetFilters()
        changingNavigation = false
        rebuildRows()
    }

    public func setMetric(_ metric: BrowserMetric) {
        guard self.metric != metric else { return }
        self.metric = metric
        sortedChildren = [:]
        if isFiltered { scheduleFilter() } else { rebuildRows() }
    }

    public func select(_ node: UInt32, revealInParent: Bool = false) {
        guard let tree, tree.kind(of: node) != .invalid else { return }
        let wasFiltered = isFiltered
        changingNavigation = true
        resetFilters()
        selectedNode = node
        if tree.kind(of: node) == .dir {
            currentNode = node
            expanded.insert(node)
        } else if revealInParent || wasFiltered {
            currentNode = tree.parent(of: node) ?? 0
        }
        var cursor = tree.parent(of: node)
        while let ancestor = cursor {
            expanded.insert(ancestor)
            cursor = tree.parent(of: ancestor)
        }
        changingNavigation = false
        rebuildRows()
    }

    public func toggleExpansion(_ node: UInt32) {
        if expanded.contains(node) { expanded.remove(node) } else { expanded.insert(node) }
        rebuildRows()
    }

    public func showMore(in node: UInt32) {
        if isFiltered {
            matchLimit += Self.pageSize
        } else {
            limits[node, default: Self.pageSize] += Self.pageSize
        }
        rebuildRows()
    }

    public func toggleExtension(_ category: String) {
        extensionFilter = extensionFilter == category ? nil : category
        scheduleFilter()
    }

    public func clearFilters() {
        changingNavigation = true
        resetFilters()
        changingNavigation = false
        rebuildRows()
    }

    private func resetFilters() {
        cancelFilter()
        query = ""
        extensionFilter = nil
        highlightedNodes = nil
        matches = []
        matchCount = 0
        visibleMatchCount = 0
    }

    private func cancelFilter() {
        generation += 1
        filterTask?.cancel()
        filterTask = nil
        filtering = false
    }

    private func scheduleFilter() {
        guard !changingNavigation else { return }
        cancelFilter()
        matchLimit = Self.pageSize
        matches = []
        matchCount = 0
        visibleMatchCount = 0
        let category = extensionFilter
        let root = currentNode
        let cachedCategory = categoryCache.flatMap { $0.root == root && $0.key == category ? $0.nodes : nil }
        highlightedNodes = cachedCategory
        guard isFiltered, let tree else {
            rebuildRows()
            return
        }
        rows = []
        filtering = true
        let request = generation
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let metric = metric
        filterTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 200_000_000) } catch { return }
            let work = Task.detached(priority: .userInitiated) {
                Self.findMatches(
                    tree: tree, query: query, category: category, root: root, metric: metric,
                    cachedCategory: cachedCategory)
            }
            let result = await withTaskCancellationHandler {
                await work.value
            } onCancel: {
                work.cancel()
            }
            guard !Task.isCancelled, let self, self.generation == request else { return }
            self.matches = result.matches
            self.matchCount = result.matches.count
            self.highlightedNodes = category == nil ? nil : result.highlighted
            if let category { self.categoryCache = (root, category, result.highlighted) }
            self.filtering = false
            self.rebuildRows()
        }
    }

    nonisolated private static func findMatches(
        tree: any BrowserTree, query: String, category: String?, root: UInt32, metric: BrowserMetric,
        cachedCategory: Set<UInt32>?
    ) -> (matches: [UInt32], highlighted: Set<UInt32>) {
        let nameMatches = query.count >= 2 ? Set(tree.searchMatches(query: query)) : nil
        var candidates: [UInt32] = []
        var highlighted: Set<UInt32> = []
        if let cachedCategory {
            highlighted = cachedCategory
            candidates = cachedCategory.filter { nameMatches?.contains($0) != false }
        } else if let category {
            var stack = [root]
            while let node = stack.popLast() {
                if Task.isCancelled { return ([], []) }
                let kind = tree.kind(of: node)
                if kind == .dir {
                    stack.append(contentsOf: tree.children(of: node))
                } else if FileCategory.key(name: tree.name(of: node) ?? "", kind: kind) == category {
                    highlighted.insert(node)
                    if nameMatches?.contains(node) != false { candidates.append(node) }
                }
            }
        } else {
            candidates = Array(nameMatches ?? [])
        }
        var scored: [(UInt32, UInt64?)] = []
        scored.reserveCapacity(candidates.count)
        for node in candidates {
            if Task.isCancelled { return ([], []) }
            scored.append(
                (
                    node,
                    metric == .allocated ? tree.valueAllocated(of: node) : tree.valueLogical(of: node)
                ))
        }
        scored.sort(by: largerFirst)
        return (scored.map(\.0), highlighted)
    }

    nonisolated private static func largerFirst(
        _ lhs: (UInt32, UInt64?), _ rhs: (UInt32, UInt64?)
    ) -> Bool {
        switch (lhs.1, rhs.1) {
        case let (a?, b?): return a == b ? lhs.0 < rhs.0 : a > b
        case (_?, nil): return true
        case (nil, _?): return false
        case (nil, nil): return lhs.0 < rhs.0
        }
    }

    private func rebuildRows() {
        guard let tree else {
            rows = []
            return
        }
        if isFiltered {
            let visible = matches.prefix(matchLimit)
            visibleMatchCount = visible.count
            rows = visible.map {
                Row(id: .node($0), node: $0, depth: 0, hasChildren: false, expanded: false, remaining: 0)
            }
            if matches.count > visible.count {
                rows.append(
                    Row(
                        id: .more(0), node: 0, depth: 0, hasChildren: false, expanded: false,
                        remaining: matches.count - visible.count))
            }
            return
        }
        // Keep a selected result visible even when it falls beyond a folder's page.
        var selectedPath: Set<UInt32> = [selectedNode]
        var ancestor = tree.parent(of: selectedNode)
        while let node = ancestor {
            selectedPath.insert(node)
            ancestor = tree.parent(of: node)
        }
        var output: [Row] = []
        var stack: [(node: UInt32, depth: Int, remaining: Int)] = [(0, 0, 0)]
        while let item = stack.popLast() {
            let node = item.node
            if item.remaining > 0 {
                output.append(
                    Row(
                        id: .more(node), node: node, depth: item.depth, hasChildren: false, expanded: false,
                        remaining: item.remaining))
                continue
            }
            let children = tree.children(of: node)
            let isExpanded = !children.isEmpty && expanded.contains(node)
            output.append(
                Row(
                    id: .node(node), node: node, depth: item.depth, hasChildren: !children.isEmpty,
                    expanded: isExpanded, remaining: 0))
            guard isExpanded else { continue }
            let sorted: [UInt32]
            if let cached = sortedChildren[node] {
                sorted = cached
            } else {
                let scored = children.map {
                    ($0, metric == .allocated ? tree.valueAllocated(of: $0) : tree.valueLogical(of: $0))
                }
                sorted = scored.sorted(by: Self.largerFirst).map(\.0)
                sortedChildren[node] = sorted
            }
            let limit = limits[node] ?? Self.pageSize
            var visible = Array(sorted.prefix(limit))
            visible.append(contentsOf: sorted.dropFirst(limit).filter { selectedPath.contains($0) })
            let remaining = sorted.count - visible.count
            if remaining > 0 { stack.append((node, item.depth + 1, remaining)) }
            for child in visible.reversed() { stack.append((child, item.depth + 1, 0)) }
        }
        rows = output
    }
}
