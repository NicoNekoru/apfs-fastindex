import AppKit
import Foundation

@main @MainActor
enum FileActionsTests {
    static var assertions = 0

    static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
        assertions += 1
        guard condition() else {
            fputs("FAIL: \(message)\n", stderr)
            exit(1)
        }
    }

    static func main() throws {
        let manager = FileManager.default
        let fixture = manager.temporaryDirectory.appendingPathComponent("apfs-file-actions-\(UUID().uuidString)")
        try manager.createDirectory(at: fixture, withIntermediateDirectories: false)
        defer { try? manager.removeItem(at: fixture) }
        try Data("fixture".utf8).write(to: fixture.appendingPathComponent("file.txt"))
        try manager.createDirectory(at: fixture.appendingPathComponent("folder"), withIntermediateDirectories: false)
        try manager.createSymbolicLink(at: fixture.appendingPathComponent("system-link"), withDestinationURL: URL(fileURLWithPath: "/usr/bin"))
        try manager.createSymbolicLink(at: fixture.appendingPathComponent("snapshot-root-link"), withDestinationURL: URL(fileURLWithPath: "/"))
        try manager.createSymbolicLink(at: fixture.appendingPathComponent("broken-link"), withDestinationURL: fixture.appendingPathComponent("missing"))
        let locked = fixture.appendingPathComponent("locked")
        try manager.createDirectory(at: locked, withIntermediateDirectories: false)
        try Data("fixture".utf8).write(to: locked.appendingPathComponent("child.txt"))

        guard let scan = Scan.fallback(path: fixture.path) else { fatalError("Fixture scan failed") }
        let actions = FileActions()
        actions.install(scan)
        for snapshotRoot in [false, true] {
            // A full-drive scan starts on the System snapshot but includes
            // writable Data entries. Reproduce that provenance without
            // enumerating the entire drive or moving any items to Trash.
            scan.isSnapshotPath = snapshotRoot
            for name in ["file.txt", "folder", "system-link", "snapshot-root-link", "broken-link"] {
                guard let node = scan.nodeIndex(forPath: name),
                    let target = actions.target(node: node, scan: scan)
                else { fatalError("Missing fixture target \(name)") }
                expect(target.canTrash, "Writable entry \(name) must remain trashable with snapshot-root flag \(snapshotRoot)")
                let menu = actions.menu(for: target)
                expect(!menu.autoenablesItems, "Explicit eligibility must survive AppKit menu validation")
                expect(menu.items.last?.isEnabled == true, "Native Trash item must be enabled for \(name)")
            }
            guard let root = actions.target(node: 0, scan: scan) else { fatalError("Missing root target") }
            expect(!root.canTrash, "The scan root must remain protected")
            expect(actions.menu(for: root).items.last?.isEnabled == false, "The native menu must protect the scan root")
            expect(root.trashDisabledReason != nil, "A protected root must explain why Trash is disabled")
            expect(actions.menu(for: root).items.last?.toolTip == root.trashDisabledReason, "Native disabled items expose their reason")
        }
        expect(actions.target(node: UInt32.max, scan: scan) == nil, "Invalid nodes must not produce file actions")

        if geteuid() != 0 {
            try manager.setAttributes([.posixPermissions: 0o500], ofItemAtPath: locked.path)
            defer { try? manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: locked.path) }
            guard let node = scan.nodeIndex(forPath: "locked/child.txt"),
                let target = actions.target(node: node, scan: scan)
            else { fatalError("Missing permission fixture target") }
            expect(!target.canTrash, "A non-writable enclosing folder must prevent Trash")
            expect(target.trashDisabledReason != nil, "Unavailable Trash actions must explain the restriction")
            expect(actions.menu(for: target).items.last?.isEnabled == false, "Native menu retains the permission restriction")
        }

        if SnapshotDetect.isOnSnapshot("/usr/bin") {
            guard let system = Scan.fallback(path: "/usr/bin"),
                let child = system.children(of: 0).first
            else { fatalError("System fixture scan failed") }
            system.isSnapshotPath = false
            guard let target = actions.target(node: child, scan: system) else { fatalError("Missing System target") }
            expect(!target.canTrash, "Snapshot contents must remain protected even without a snapshot-root badge")
            expect(actions.menu(for: target).items.last?.isEnabled == false, "Native Trash item must stay disabled on a snapshot")
            expect(target.trashDisabledReason != nil, "Snapshot items must explain their protection")
        } else {
            print("Snapshot fixture not present on this host")
        }
        print("apfs-file-action-tests: all \(assertions) assertions passed; no Trash operations performed")
    }
}
