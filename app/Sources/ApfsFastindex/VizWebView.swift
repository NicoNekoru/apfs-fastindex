import SwiftUI
import WebKit

/// `NSViewRepresentable` wrapper around `WKWebView` that hosts the bundled
/// `viz/index.html`. The Phase 1 contract is intentionally tiny:
///
/// - Once the view loads, call `onReady(webView)` so the controller can
///   start pushing data via `evaluateJavaScript`.
/// - Forward any structured JS-side messages back as `BridgeMessage`s.
/// - Replay the latest pending scan / progress once the page is up so a
///   slow viz load does not lose data the user already requested.
struct VizWebView: NSViewRepresentable {
    let onMessage: (BridgeMessage) -> Void
    let onReady: (WKWebView) -> Void
    let onDeliverScanJSON: String?
    let onDeliverProgress: ProgressUpdate?

    func makeCoordinator() -> Coordinator {
        Coordinator(onMessage: onMessage, onReady: onReady)
    }

    func makeNSView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        let userContentController = WKUserContentController()
        userContentController.add(context.coordinator, name: "app")
        // Inject a tiny shim so Swift can deliver scan JSON / progress
        // even before the viz finishes wiring up its drag-drop handlers.
        let shimScript = WKUserScript(
            source: vizBridgeShim,
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true
        )
        userContentController.addUserScript(shimScript)
        config.userContentController = userContentController
        // Allow `file://` resources to read sibling resources (the
        // bundled vendor/ subdirectory). WKWebView blocks this by
        // default on macOS.
        config.preferences.setValue(true, forKey: "allowFileAccessFromFileURLs")
        let webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = context.coordinator
        webView.setValue(false, forKey: "drawsBackground")

        if let url = Bundle.module.url(forResource: "index", withExtension: "html", subdirectory: "viz") {
            // Allow the WebView to read sibling resources (the vendored
            // d3.v7.min.js lives in viz/vendor/). The grant scope is the
            // viz/ subdirectory only.
            webView.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
        } else {
            NSLog("VizWebView: bundled viz/index.html not found")
        }
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        // SwiftUI re-invokes this when @Published state in the parent
        // changes. We use it to replay any pending data the controller
        // accumulated before the viz announced readiness.
        if context.coordinator.viewReady {
            if let json = onDeliverScanJSON, json != context.coordinator.lastDeliveredScan {
                context.coordinator.lastDeliveredScan = json
                deliverScan(webView: webView, json: json)
            }
            if let progress = onDeliverProgress,
               progress.elapsedMs != context.coordinator.lastProgressElapsedMs {
                context.coordinator.lastProgressElapsedMs = progress.elapsedMs
                deliverProgress(webView: webView, progress: progress)
            }
        }
    }

    private func deliverScan(webView: WKWebView, json: String) {
        let literal = ScanController.encodeAsJSStringLiteral(json)
        let js = "if (window.__apfs_ingest__) { __apfs_ingest__(\(literal)); }"
        webView.evaluateJavaScript(js, completionHandler: nil)
    }

    private func deliverProgress(webView: WKWebView, progress: ProgressUpdate) {
        let payload = """
        {"scanned":\(progress.scanned),"skipped":\(progress.skipped),"elapsedMs":\(progress.elapsedMs),"terminal":\(progress.terminal ? "true" : "false")}
        """
        let js = "if (window.__apfs_progress__) { __apfs_progress__(\(payload)); }"
        webView.evaluateJavaScript(js, completionHandler: nil)
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        let onMessage: (BridgeMessage) -> Void
        let onReady: (WKWebView) -> Void
        var viewReady: Bool = false
        var lastDeliveredScan: String? = nil
        var lastProgressElapsedMs: UInt64 = .max

        init(onMessage: @escaping (BridgeMessage) -> Void,
             onReady: @escaping (WKWebView) -> Void) {
            self.onMessage = onMessage
            self.onReady = onReady
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            viewReady = true
            onReady(webView)
        }

        func userContentController(_ userContentController: WKUserContentController,
                                   didReceive message: WKScriptMessage) {
            guard message.name == "app" else { return }
            if let parsed = BridgeMessage(payload: message.body) {
                onMessage(parsed)
            }
        }
    }
}

/// JavaScript injected at document-start that exposes:
///
/// - `window.__apfs_ingest__(doc)` — Swift hands a parsed scan document
///   (or JSON string) to the viz; identical effect to the user dragging
///   a JSON file in.
/// - `window.__apfs_progress__(update)` — Swift posts a live progress
///   update. The viz currently ignores it (a status pill is still on
///   the next-polish list); the function is wired so a later viz patch
///   can render it without touching Swift again.
/// - `window.__apfs_post__(message)` — convenience wrapper around
///   `window.webkit.messageHandlers.app.postMessage`.
private let vizBridgeShim: String = """
(() => {
  if (window.__apfs_shim_installed__) return;
  window.__apfs_shim_installed__ = true;
  window.__apfs_post__ = function(message) {
    try {
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.app) {
        window.webkit.messageHandlers.app.postMessage(message);
      }
    } catch (err) {
      console.error('__apfs_post__ failed', err);
    }
  };
  window.__apfs_ingest__ = function(doc) {
    try {
      const parsed = (typeof doc === 'string') ? JSON.parse(doc) : doc;
      if (typeof window.ingest === 'function') {
        window.ingest(parsed, 'native://current-scan');
      } else {
        // The viz may not have wired `ingest` to window scope; fall back
        // by saving the doc for the page to pick up.
        window.__apfs_pending_scan__ = parsed;
      }
    } catch (err) {
      console.error('__apfs_ingest__ failed', err);
    }
  };
  window.__apfs_progress__ = function(update) {
    window.__apfs_latest_progress__ = update;
    if (typeof window.onApfsProgress === 'function') {
      try { window.onApfsProgress(update); } catch (err) { console.error(err); }
    }
  };
})();
"""

extension ScanController {
    /// Re-encode a Swift string as a JS string literal that survives
    /// `evaluateJavaScript`. Goes through `JSONSerialization` so all the
    /// usual escapes are handled.
    static func encodeAsJSStringLiteral(_ s: String) -> String {
        if let data = try? JSONSerialization.data(withJSONObject: [s], options: []),
           let str = String(data: data, encoding: .utf8),
           str.count >= 2 {
            return String(str.dropFirst().dropLast())
        }
        return "\"\""
    }
}
