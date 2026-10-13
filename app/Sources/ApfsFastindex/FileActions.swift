import ApfsCore
import AppKit
import SwiftUI

/// Both renderers use the same path checks, actions and confirmation flow.
final class FileActions: NSObject, ObservableObject {
    struct Target {
        let scan: Scan
        let relative: String
        let path: String
        let trashDisabledReason: String?
        var canTrash: Bool { trashDisabledReason == nil }
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
            scan: scan, relative: relative, path: path,
            trashDisabledReason: trashDisabledReason(
                path: path, isScanRoot: node == 0 || relative.isEmpty))
    }

    private func trashDisabledReason(path: String, isScanRoot: Bool) -> String? {
        if isScanRoot { return "The scan root cannot be moved to the Trash." }
        // Removing an entry modifies its containing directory. Inspect that
        // filesystem, not the scan root or a symlink's destination. A scan
        // of / includes writable Data entries beneath the System snapshot.
        let parent = URL(fileURLWithPath: path).deletingLastPathComponent().path
        if SnapshotDetect.isOnSnapshot(parent) {
            return "Items on a snapshot filesystem cannot be moved to the Trash."
        }
        let type = try? FileManager.default.attributesOfItem(atPath: path)[.type] as? FileAttributeType
        if type == .typeDirectory,
            (try? URL(fileURLWithPath: path).resourceValues(forKeys: [.isVolumeKey]).isVolume) == true {
            return "A mounted volume cannot be moved to the Trash."
        }
        guard FileManager.default.isWritableFile(atPath: parent) else {
            return "The enclosing folder is read-only or cannot be modified by this app."
        }
        return nil
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
        guard canTrashNow(target) else { return }
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
            guard self?.canTrashNow(target) == true else { return }
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

    private func canTrashNow(_ target: Target) -> Bool {
        guard target.canTrash, itemExists(target) else { return false }
        if let reason = trashDisabledReason(path: target.path, isScanRoot: target.relative.isEmpty) {
            showError(reason)
            return false
        }
        return true
    }

    private func itemExists(_ target: Target) -> Bool {
        guard PathContainment.resolveContained(
            scanRoot: target.scan.sourceRequestedPath, relative: target.relative) == target.path
        else {
            showError("This path has changed. Rescan the folder before acting on it.")
            return false
        }
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
        func add(_ title: String, _ action: Selector, disabledReason: String? = nil) {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
            item.target = self
            item.representedObject = target
            item.isEnabled = disabledReason == nil
            item.toolTip = disabledReason
            menu.addItem(item)
        }
        add("Reveal in Finder", #selector(revealItem(_:)))
        add("Copy Path", #selector(copyItem(_:)))
        menu.addItem(.separator())
        add("Move to Trash…", #selector(trashItem(_:)), disabledReason: target.trashDisabledReason)
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
            .help(target.trashDisabledReason ?? "Move this item to the Trash in Finder.")
    }
}
