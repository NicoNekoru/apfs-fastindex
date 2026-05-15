import Foundation

/// Typed messages the viz JS can send to Swift. Decoded from the JSON
/// payload that `window.webkit.messageHandlers.app.postMessage` sends.
enum BridgeMessage {
    case selected(path: String, kind: String, size: UInt64)
    case contextMenu(path: String, kind: String, viewportX: Double, viewportY: Double)
    case revealInFinder(path: String)
    case moveToTrash(paths: [String])

    init?(payload: Any) {
        guard let dict = payload as? [String: Any],
              let type = dict["type"] as? String else {
            return nil
        }
        switch type {
        case "selected":
            let path = (dict["path"] as? String) ?? ""
            let kind = (dict["kind"] as? String) ?? "file"
            let size = (dict["size"] as? NSNumber)?.uint64Value ?? 0
            self = .selected(path: path, kind: kind, size: size)
        case "context_menu":
            let path = (dict["path"] as? String) ?? ""
            let kind = (dict["kind"] as? String) ?? "file"
            let x = (dict["x"] as? NSNumber)?.doubleValue ?? 0
            let y = (dict["y"] as? NSNumber)?.doubleValue ?? 0
            self = .contextMenu(path: path, kind: kind, viewportX: x, viewportY: y)
        case "reveal_in_finder":
            self = .revealInFinder(path: (dict["path"] as? String) ?? "")
        case "move_to_trash":
            self = .moveToTrash(paths: (dict["paths"] as? [String]) ?? [])
        default:
            return nil
        }
    }
}
