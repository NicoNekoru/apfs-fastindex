import ApfsCore
import Foundation

/// An immutable index with the same borrowed-child lifetime as the Rust bridge.
private final class FixtureTree: BrowserTree {
    struct Node {
        var name: String
        var kind: BrowserNodeKind = .file
        var parent: UInt32? = 0
        var logical: UInt64 = 1
        var allocated: UInt64? = 1
    }
    let nodes: [Node]
    private var buffers: [UInt32: UnsafeMutableBufferPointer<UInt32>] = [:]
    private let searchGate: DispatchSemaphore?
    private let searchStarted: DispatchSemaphore?

    init(_ nodes: [Node], searchGate: DispatchSemaphore? = nil, searchStarted: DispatchSemaphore? = nil) {
        self.nodes = nodes
        self.searchGate = searchGate
        self.searchStarted = searchStarted
        var children: [UInt32: [UInt32]] = [:]
        for (index, node) in nodes.enumerated() {
            if let parent = node.parent { children[parent, default: []].append(UInt32(index)) }
        }
        for (parent, indices) in children {
            let buffer = UnsafeMutableBufferPointer<UInt32>.allocate(capacity: indices.count)
            _ = buffer.initialize(from: indices)
            buffers[parent] = buffer
        }
    }

    deinit {
        for buffer in buffers.values {
            buffer.baseAddress?.deinitialize(count: buffer.count)
            buffer.deallocate()
        }
    }

    func kind(of node: UInt32) -> BrowserNodeKind {
        Int(node) < nodes.count ? nodes[Int(node)].kind : .invalid
    }
    func name(of node: UInt32) -> String? { nodes[Int(node)].name }
    func parent(of node: UInt32) -> UInt32? { nodes[Int(node)].parent }
    func children(of node: UInt32) -> UnsafeBufferPointer<UInt32> {
        buffers[node].map { UnsafeBufferPointer($0) } ?? UnsafeBufferPointer(start: nil, count: 0)
    }
    func valueLogical(of node: UInt32) -> UInt64 { nodes[Int(node)].logical }
    func valueAllocated(of node: UInt32) -> UInt64? { nodes[Int(node)].allocated }
    func searchMatches(query: String) -> [UInt32] {
        searchStarted?.signal()
        searchGate?.wait()
        return nodes.indices.filter { nodes[$0].name.localizedCaseInsensitiveContains(query) }.map(
            UInt32.init)
    }
}

@main @MainActor
enum BrowserTests {
    static var assertions = 0
    static func expect(_ condition: @autoclosure () -> Bool, _ message: String, line: UInt = #line) {
        assertions += 1
        guard condition() else {
            print("FAIL at line \(line). \(message)")
            exit(1)
        }
    }

    static func settle(_ model: BrowserModel) async {
        for _ in 0..<500 {
            if !model.filtering { return }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        expect(false, "Filter did not finish")
    }

    static func main() async {
        let tree = FixtureTree([
            .init(name: "root", kind: .dir, parent: nil),
            .init(name: "photos", kind: .dir),
            .init(name: "Photo.JPG", parent: 1, logical: 100, allocated: 1),
            .init(name: "small.jpg", parent: 1, logical: 50, allocated: 100),
            .init(name: "outside.jpg", logical: 200),
            .init(name: "photo.jpg.backup", parent: 1),
            .init(name: ".hidden", parent: 1),
            .init(name: "link.jpg", kind: .symlink, parent: 1),
            .init(name: "empty", kind: .dir),
            .init(name: "unknown.jpg", parent: 1, allocated: nil),
        ])
        let model = BrowserModel()
        model.install(tree, metric: .logical)
        expect(model.currentNode == 0 && model.selectedNode == 0, "Fresh scan starts at root")
        model.toggleExpansion(1)
        expect(model.rows.contains { $0.node == 2 }, "Disclosure expands the folder's children")
        expect(model.currentNode == 0 && model.selectedNode == 0, "Disclosure does not navigate or select")
        model.toggleExpansion(1)
        expect(!model.rows.contains { $0.node == 2 }, "Disclosure collapses the folder's children")
        model.toggleExpansion(2)
        expect(model.currentNode == 0 && model.selectedNode == 0, "Leaf disclosure is inert")
        model.select(1)
        expect(model.currentNode == 1 && model.rows.contains { $0.node == 2 }, "Drilling expands the folder")
        model.select(2)
        expect(
            model.currentNode == 1 && model.selectedNode == 2, "Files select without becoming treemap roots")
        model.select(0)
        model.select(2)
        expect(model.currentNode == 0, "Treemap selection preserves its current scope")
        model.select(2, revealInParent: true)
        expect(model.currentNode == 1, "Tree selection reveals a file in its parent")

        model.toggleExtension(".jpg")
        await settle(model)
        expect(
            model.rows.map(\.node) == [2, 3, 9],
            "Extension matching is exact, case-insensitive and subtree-scoped")
        expect(model.highlightedNodes == [2, 3, 9], "Category highlights include every matching leaf")
        model.query = "small"
        expect(model.highlightedNodes == [2, 3, 9], "Category highlighting does not flicker during name filtering")
        await settle(model)
        expect(model.rows.map(\.node) == [3], "Name and extension filters intersect")
        expect(model.highlightedNodes == [2, 3, 9], "Name search does not change category highlighting")
        model.query = ""
        model.setMetric(.allocated)
        await settle(model)
        expect(
            model.rows.map(\.node) == [3, 2, 9], "Changing metric reorders matches, unclaimed values last")
        model.select(2)
        expect(
            !model.isFiltered && model.query.isEmpty && model.highlightedNodes == nil,
            "Choosing a result clears filters")
        expect(
            model.currentNode == 1 && model.rows.contains { $0.node == 2 }, "Selected result remains visible")

        model.toggleExtension("(no ext)")
        await settle(model)
        expect(model.rows.map(\.node) == [6], "No-extension category works")
        model.toggleExtension("(symlink)")
        await settle(model)
        expect(model.rows.map(\.node) == [7], "Symlink category does not use its filename extension")
        model.toggleExtension("(symlink)")
        expect(!model.isFiltered, "Clicking the selected category toggles it off")
        model.query = "outside"
        await settle(model)
        expect(model.rows.map(\.node) == [4], "Name-only search covers the whole scan")
        model.query = "missing"
        await settle(model)
        expect(model.rows.isEmpty && model.matchCount == 0, "No matches has a real empty state")
        model.query = "x"
        expect(!model.isFiltered && !model.rows.isEmpty, "Short queries restore the hierarchy")
        model.query = "Photo"
        model.query = "small"
        await settle(model)
        expect(model.rows.map(\.node) == [3], "Only the newest debounced search publishes")
        model.query = "outside"
        model.install(tree, metric: .logical)
        try? await Task.sleep(nanoseconds: 250_000_000)
        expect(
            !model.isFiltered && !model.filtering && model.currentNode == 0,
            "Same-sized rescan clears pending searches")
        model.select(8)
        expect(model.currentNode == 8, "Empty directories remain navigable")

        let gate = DispatchSemaphore(value: 0)
        let started = DispatchSemaphore(value: 0)
        let blockedTree = FixtureTree(tree.nodes, searchGate: gate, searchStarted: started)
        model.install(blockedTree, metric: .logical)
        model.query = "Photo"
        let didStart = await withCheckedContinuation { continuation in
            DispatchQueue.global().async {
                continuation.resume(returning: started.wait(timeout: .now() + 3) == .success)
            }
        }
        expect(didStart, "Background search started")
        model.install(tree, metric: .logical)
        gate.signal()
        try? await Task.sleep(nanoseconds: 100_000_000)
        expect(
            model.query.isEmpty && !model.isFiltered && model.currentNode == 0,
            "An old running search cannot publish into a replacement scan")

        let many = FixtureTree(
            [.init(name: "root", kind: .dir, parent: nil)]
                + (1...850).map {
                    .init(name: "file\($0).txt", logical: UInt64(851 - $0))
                })
        model.install(many, metric: .logical)
        expect(
            model.rows.count == 402 && model.rows.last?.remaining == 450, "Folders initially paginate at 400")
        model.showMore(in: 0)
        expect(model.rows.count == 802 && model.rows.last?.remaining == 50, "Show more exposes the next page")
        model.showMore(in: 0)
        expect(model.rows.count == 851 && model.rows.last?.remaining == 0, "All children are reachable")
        model.install(many, metric: .logical)
        model.select(850, revealInParent: true)
        expect(model.rows.contains { $0.id == .node(850) }, "A selected item beyond the cap is visible")
        expect(Set(model.rows.map(\.id)).count == model.rows.count, "Row identities are unique")
        model.toggleExtension(".txt")
        await settle(model)
        expect(model.matchCount == 850 && model.visibleMatchCount == 400, "Filtered results paginate")
        expect(model.highlightedNodes?.count == 850, "Highlights are not limited to the first page")
        model.showMore(in: 0)
        expect(model.visibleMatchCount == 800, "Filtered show more works")
        model.clearFilters()
        expect(model.highlightedNodes == nil && !model.filtering, "Clear resets highlighting and progress")

        let unknownSizes = FixtureTree([
            .init(name: "root", kind: .dir, parent: nil),
            .init(name: "unknown.txt", allocated: nil),
            .init(name: "empty.txt", allocated: 0),
            .init(name: "known.txt", allocated: 1),
        ])
        model.install(unknownSizes, metric: .allocated)
        expect(model.rows.map(\.node) == [0, 3, 2, 1], "Unknown allocation sorts after known zero in folders")
        model.toggleExtension(".txt")
        await settle(model)
        expect(model.rows.map(\.node) == [3, 2, 1], "Unknown allocation sorts after known zero in filtered results")

        for relative in ["/Documents/file", "file\u{0}other"] {
            expect(PathContainment.resolveContained(scanRoot: "/Users/me", relative: relative) == nil,
                "Absolute and NUL-containing entry paths are rejected")
        }
        expect(PathContainment.resolveContained(scanRoot: "/Users/me\u{0}other", relative: "file") == nil,
            "NUL-containing scan roots are rejected")
        do {
            let manager = FileManager.default
            let fixture = manager.temporaryDirectory.appendingPathComponent("apfs-containment-\(UUID().uuidString)")
            try manager.createDirectory(at: fixture, withIntermediateDirectories: false)
            defer { try? manager.removeItem(at: fixture) }
            let root = fixture.appendingPathComponent("root")
            let outside = fixture.appendingPathComponent("outside")
            try manager.createDirectory(at: root, withIntermediateDirectories: false)
            try manager.createDirectory(at: outside, withIntermediateDirectories: false)
            try manager.createSymbolicLink(at: root.appendingPathComponent("escape"), withDestinationURL: outside)
            try manager.createSymbolicLink(at: root.appendingPathComponent("dangling"), withDestinationURL: outside.appendingPathComponent("missing"))
            try manager.createSymbolicLink(at: fixture.appendingPathComponent("alias"), withDestinationURL: root)
            expect(PathContainment.resolveContained(scanRoot: root.path, relative: "escape/file") == nil,
                "An ancestor symlink cannot redirect an action outside the root")
            expect(PathContainment.resolveContained(scanRoot: root.path, relative: "dangling") != nil,
                "The final dangling symlink itself remains actionable")
            expect(PathContainment.resolveContained(scanRoot: fixture.appendingPathComponent("alias").path, relative: "file") != nil,
                "A symlink scan root still accepts its own descendants")
            expect(!PathContainment.volumeIsCaseInsensitive(at: fixture.appendingPathComponent("missing")),
                "Unknown volumes fail closed on case mismatches")
        } catch {
            expect(false, "Containment fixture failed. \(error)")
        }

        let deep = FixtureTree(
            (0..<600).map {
                .init(name: "folder\($0)", kind: .dir, parent: $0 == 0 ? nil : UInt32($0 - 1))
            })
        model.install(deep, metric: .logical)
        model.select(599)
        expect(model.rows.count == 600, "Deep navigation builds rows without recursion")
        expect(Set(model.rows.map(\.id)).count == 600, "Deep rows keep unique identities")

        for name in ["README", ".gitignore", "file."] {
            expect(
                FileCategory.key(name: name, kind: .file) == "(no ext)", "No-extension semantics for \(name)")
        }
        expect(
            FileCategory.key(name: ".config.JSON", kind: .file) == ".json", "Hidden files may have extensions"
        )
        expect(
            FileCategory.key(name: "file.ÄBC", kind: .file) == ".Äbc", "Only ASCII case folds, matching Rust")
        expect(FileCategory.key(name: "archive.tar.gz", kind: .file) == ".gz", "Last extension wins")
        print("apfs-browser-tests: all \(assertions) assertions passed")
    }
}
