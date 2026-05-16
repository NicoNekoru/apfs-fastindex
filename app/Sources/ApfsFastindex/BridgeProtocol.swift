import Foundation

/// Typed messages the viz JS can send to Swift. Decoded from the JSON
/// payload that `window.webkit.messageHandlers.app.postMessage` sends.
enum BridgeMessage {
    case selected(path: String, kind: String, size: UInt64)
    case contextMenu(path: String, kind: String, viewportX: Double, viewportY: Double)
    case revealInFinder(path: String)
    case moveToTrash(paths: [String])
    case consoleError(message: String)
    case ingestStarted
    case ingestSucceeded(
        rootPath: String,
        totalEntries: UInt64,
        logicalTotal: UInt64,
        allocatedTotal: UInt64?,
        allocatedAvailable: Bool
    )
    case ingestFailed(message: String)

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
        case "console_error":
            self = .consoleError(message: (dict["message"] as? String) ?? "")
        case "ingest_started":
            self = .ingestStarted
        case "ingest_succeeded":
            let root = (dict["rootPath"] as? String) ?? ""
            let total = (dict["totalEntries"] as? NSNumber)?.uint64Value ?? 0
            let logicalTotal = (dict["logicalTotal"] as? NSNumber)?.uint64Value ?? 0
            // `allocatedTotal` is JSON null when the SR-019 / EX-22
            // None-collapse fired anywhere in the subtree; preserve
            // that as Swift `nil` so the status bar can render
            // "unclaimed" rather than a misleading zero.
            let allocatedTotal: UInt64?
            if dict["allocatedTotal"] is NSNull {
                allocatedTotal = nil
            } else if let num = dict["allocatedTotal"] as? NSNumber {
                allocatedTotal = num.uint64Value
            } else {
                allocatedTotal = nil
            }
            let allocatedAvailable = (dict["allocatedAvailable"] as? Bool) ?? false
            self = .ingestSucceeded(
                rootPath: root,
                totalEntries: total,
                logicalTotal: logicalTotal,
                allocatedTotal: allocatedTotal,
                allocatedAvailable: allocatedAvailable
            )
        case "ingest_failed":
            self = .ingestFailed(message: (dict["message"] as? String) ?? "")
        default:
            return nil
        }
    }
}
