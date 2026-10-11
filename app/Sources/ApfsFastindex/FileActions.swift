import ApfsCore
import AppKit
import SwiftUI

/// Both renderers use the same path checks, actions and confirmation flow.
final class FileActions: NSObject, ObservableObject {
    struct Target {
        let scan: Scan
        let path: String
        let canTrash: Bool
        var url: URL { URL(fileURLWithPath: path) }
    }

    @Published private(set) var needsRescan = false
    private weak var scan: Scan?

    func install(_ scan: Scan) {
        self.scan = scan
        needsRescan = false
    }

    func target(node: UInt32, scan: Scan) -> Target? {
        guard node < scan.nodeCount,
            let relative = scan.path(of: node),
            let path = PathContainment.resolveContained(
                scanRoot: scan.sourceRequestedPath, relative: relative)
        else { return nil }
        return Target(
            scan: scan, path: path, canTrash: node != 0 && !relative.isEmpty && !scan.isSnapshotPath)
    }

    func reveal(_ target: Target) {
        guard itemExists(target) else { return }
        NSWorkspace.shared.activateFileViewerSelecting([target.url])
    }

    func copyPath(_ target: Target) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(target.path, forType: .string)
    }

    func moveToTrash(_ target: Target) {
        guard target.canTrash, itemExists(target) else { return }
        let alert = NSAlert()
        alert.messageText = "Move to Trash?"
        alert.informativeText =
            DisplaySanitizer.sanitiseDisplay(target.path)
            + "\n\nYou can restore this item from the Trash in Finder."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: "Move to Trash")
        let completion: (NSApplication.ModalResponse) -> Void = { [weak self] response in
            guard response == .alertSecondButtonReturn else { return }
            NSWorkspace.shared.recycle([target.url]) { _, error in
                DispatchQueue.main.async {
                    if let error {
                        self?.showError(error.localizedDescription)
                    } else if self?.scan === target.scan {
                        self?.needsRescan = true
                    }
                }
            }
        }
        if let window = NSApp.keyWindow {
            alert.beginSheetModal(for: window, completionHandler: completion)
        } else {
            completion(alert.runModal())
        }
    }

    private func itemExists(_ target: Target) -> Bool {
        // attributesOfItem also recognises dangling symlinks, unlike fileExists.
        guard (try? FileManager.default.attributesOfItem(atPath: target.path)) != nil else {
            showError("This item is no longer available. Rescan the folder to update the index.")
            return false
        }
        return true
    }

    private func showError(_ message: String) {
        let alert = NSAlert()
        alert.messageText = "File action failed"
        alert.informativeText = DisplaySanitizer.sanitiseDisplay(message)
        if let window = NSApp.keyWindow { alert.beginSheetModal(for: window) } else { alert.runModal() }
    }

    func menu(for target: Target) -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        func add(_ title: String, _ action: Selector, enabled: Bool = true) {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
            item.target = self
            item.representedObject = target
            item.isEnabled = enabled
            menu.addItem(item)
        }
        add("Reveal in Finder", #selector(revealItem(_:)))
        add("Copy Path", #selector(copyItem(_:)))
        menu.addItem(.separator())
        add("Move to Trash…", #selector(trashItem(_:)), enabled: target.canTrash)
        return menu
    }

    @objc private func revealItem(_ item: NSMenuItem) {
        if let target = item.representedObject as? Target { reveal(target) }
    }
    @objc private func copyItem(_ item: NSMenuItem) {
        if let target = item.representedObject as? Target { copyPath(target) }
    }
    @objc private func trashItem(_ item: NSMenuItem) {
        if let target = item.representedObject as? Target { moveToTrash(target) }
    }
}

struct FileActionMenu: View {
    let actions: FileActions
    let target: FileActions.Target

    var body: some View {
        // Opening scanned executables is intentionally left to Finder.
        Button("Reveal in Finder") { actions.reveal(target) }
        Button("Copy Path") { actions.copyPath(target) }
        Divider()
        Button("Move to Trash…", role: .destructive) { actions.moveToTrash(target) }
            .disabled(!target.canTrash)
    }
}
