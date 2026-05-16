import SwiftUI
import WebKit

/// `NSViewRepresentable` wrapper around `WKWebView` that hosts the bundled
/// `viz/index.html`.
///
/// Why this exists in its current form:
///
/// - The viz page is loaded from a `file://` URL (the SwiftPM resource
///   bundle). Scan results land in a different temp directory. Bridging
///   the two with `XMLHttpRequest('file://…')` requires private
///   WebKit prefs (`allowUniversalAccessFromFileURLs`), which on
///   macOS 26 (Tahoe) raise an uncatchable `NSException` when set
///   via KVC — boom, launch crash.
/// - Instead we register a `WKURLSchemeHandler` for `apfs-scan://`.
///   The page fetches `apfs-scan://current` and the handler returns the
///   bytes of the latest scan temp file. No private API, no
///   cross-origin headaches, no file:// quirks.
///
/// Contract:
///
/// - Once the page finishes loading, call `onReady(webView)` so the
///   controller can drive `evaluateJavaScript`.
/// - Forward any structured JS-side messages back as `BridgeMessage`s.
/// - Replay any pending progress event once the page is up so a slow
///   viz load doesn't drop the live counters the user already saw on
///   the status bar.
struct VizWebView: NSViewRepresentable {
    let onMessage: (BridgeMessage) -> Void
    let onReady: (WKWebView) -> Void
    let onDeliverScanFileURL: URL?
    let onDeliverProgress: ProgressUpdate?

    func makeCoordinator() -> Coordinator {
        Coordinator(onMessage: onMessage, onReady: onReady)
    }

    func makeNSView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()

        let userContentController = WKUserContentController()
        userContentController.add(context.coordinator, name: "app")
        let shimScript = WKUserScript(
            source: vizBridgeShim,
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true
        )
        userContentController.addUserScript(shimScript)
        config.userContentController = userContentController

        // Register the apfs-scan:// scheme so the viz can fetch scan
        // results without crossing file:// origin restrictions or
        // touching private WebKit prefs.
        config.setURLSchemeHandler(context.coordinator, forURLScheme: "apfs-scan")

        let webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = context.coordinator
        webView.setValue(false, forKey: "drawsBackground")

        if let url = Bundle.module.url(forResource: "index", withExtension: "html", subdirectory: "viz") {
            webView.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
        } else {
            NSLog("VizWebView: bundled viz/index.html not found")
        }
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        // The scan-result URL is *not* propagated through this re-render
        // path. SwiftUI's view update can lag a frame behind the
        // controller's `evaluateJavaScript` call, which would race the
        // page's XHR against a stale (nil) `currentScanFileURL`. The
        // controller writes directly to `coordinator.currentScanFileURL`
        // before evaluating JS instead.

        guard context.coordinator.viewReady else { return }
        if let progress = onDeliverProgress,
           progress.elapsedMs != context.coordinator.lastProgressElapsedMs {
            context.coordinator.lastProgressElapsedMs = progress.elapsedMs
            deliverProgress(webView: webView, progress: progress)
        }
    }

    private func deliverProgress(webView: WKWebView, progress: ProgressUpdate) {
        let payload = """
        {"scanned":\(progress.scanned),"skipped":\(progress.skipped),"elapsedMs":\(progress.elapsedMs),"terminal":\(progress.terminal ? "true" : "false")}
        """
        let js = "if (window.__apfs_progress__) { __apfs_progress__(\(payload)); }"
        webView.evaluateJavaScript(js, completionHandler: nil)
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler, WKURLSchemeHandler {
        let onMessage: (BridgeMessage) -> Void
        let onReady: (WKWebView) -> Void
        var viewReady: Bool = false
        var lastProgressElapsedMs: UInt64 = .max

        /// Latest scan-result file the controller wrote. Read on the
        /// main thread when the WKURLSchemeHandler fires for
        /// `apfs-scan://current`.
        var currentScanFileURL: URL?

        init(onMessage: @escaping (BridgeMessage) -> Void,
             onReady: @escaping (WKWebView) -> Void) {
            self.onMessage = onMessage
            self.onReady = onReady
        }

        // MARK: WKNavigationDelegate

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            viewReady = true
            onReady(webView)
        }

        // MARK: WKScriptMessageHandler

        func userContentController(_ userContentController: WKUserContentController,
                                   didReceive message: WKScriptMessage) {
            guard message.name == "app" else { return }
            if let parsed = BridgeMessage(payload: message.body) {
                onMessage(parsed)
            }
        }

        // MARK: WKURLSchemeHandler

        func webView(_ webView: WKWebView, start urlSchemeTask: WKURLSchemeTask) {
            guard let requestURL = urlSchemeTask.request.url else {
                urlSchemeTask.didFailWithError(NSError(domain: "ApfsScanScheme", code: 400))
                return
            }
            // Only one resource is recognized today: `apfs-scan://current`.
            // (We don't gate on host because some macOS WebKit builds
            // canonicalize the URL differently — accepting any path
            // makes the handler robust to that.)
            guard let scanURL = currentScanFileURL else {
                urlSchemeTask.didFailWithError(NSError(
                    domain: "ApfsScanScheme", code: 404,
                    userInfo: [NSLocalizedDescriptionKey: "no scan available yet"]
                ))
                return
            }
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    let data = try Data(contentsOf: scanURL, options: .mappedIfSafe)
                    // **CORS matters here.** The viz is loaded from a
                    // `file://` URL (the SwiftPM resource bundle) and
                    // is XHR'ing to `apfs-scan://`. Different schemes
                    // count as different origins; without an
                    // `Access-Control-Allow-Origin` header WebKit
                    // silently rejects the response and the JS
                    // `onload` never fires. Return a real
                    // `HTTPURLResponse` with `*` so the bytes reach
                    // `xhr.response`.
                    let response = HTTPURLResponse(
                        url: requestURL,
                        statusCode: 200,
                        httpVersion: "HTTP/1.1",
                        headerFields: [
                            "Content-Type": "application/json; charset=utf-8",
                            "Content-Length": "\(data.count)",
                            "Access-Control-Allow-Origin": "*",
                            "Cache-Control": "no-store"
                        ]
                    ) ?? URLResponse(
                        url: requestURL,
                        mimeType: "application/json",
                        expectedContentLength: data.count,
                        textEncodingName: "utf-8"
                    )
                    DispatchQueue.main.async {
                        urlSchemeTask.didReceive(response)
                        urlSchemeTask.didReceive(data)
                        urlSchemeTask.didFinish()
                    }
                } catch {
                    DispatchQueue.main.async {
                        urlSchemeTask.didFailWithError(error)
                    }
                }
            }
        }

        func webView(_ webView: WKWebView, stop urlSchemeTask: WKURLSchemeTask) {
            // Nothing to cancel: our I/O is best-effort and the
            // continuation simply no-ops if the task already finished.
        }
    }
}

/// JavaScript injected at document-start that exposes:
///
/// - `window.__apfs_ingest_file__(_path)` — Swift signals that a new
///   scan is available; the page fetches `apfs-scan://current` and
///   calls the viz's `ingest()`. Parsing happens in the WebKit content
///   process so the Swift main thread isn't blocked by a giant
///   `JSON.parse`.
/// - `window.__apfs_progress__(update)` — Swift posts a live progress
///   event. Stored on `window.__apfs_latest_progress__` for any viz
///   polish pass that wants to render a progress bar inside the page.
/// - `window.__apfs_post__(message)` — convenience wrapper around
///   `window.webkit.messageHandlers.app.postMessage`.
///
/// The shim also tags `<html>` with the `apfs-native-shell` class so
/// the viz CSS can hide the standalone-only drag-and-drop UI.
private let vizBridgeShim: String = """
(() => {
  if (window.__apfs_shim_installed__) return;
  window.__apfs_shim_installed__ = true;

  function tagNativeShell() {
    try {
      document.documentElement.classList.add('apfs-native-shell');
      // The standalone viz hard-codes a "Drop an
      // apfs-fastindex-scan JSON file to begin." prompt. In the
      // native shell the user has no idea what a "scan JSON" is
      // and never needs to drop a file. Replace it with something
      // useful before the user sees it.
      const claim = document.getElementById('claim');
      if (claim) claim.textContent = 'Pick a folder, click Scan.';
      // The viz already routes its own contextmenu events on the
      // treemap rectangles to `emitContextMenu`. Anywhere else in
      // the page (headers, breadcrumb, empty regions), suppress the
      // default WebKit menu so the user never sees "Reload" /
      // "Inspect Element" / etc. inside a shipped app.
      document.addEventListener('contextmenu', function (ev) {
        ev.preventDefault();
      }, { capture: true });
    } catch (e) {}
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', tagNativeShell);
  } else {
    tagNativeShell();
  }

  function postToSwift(message) {
    try {
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.app) {
        window.webkit.messageHandlers.app.postMessage(message);
      }
    } catch (err) {
      // Swallow — there's nowhere safe to log this and the
      // surrounding console.error would re-enter this function.
    }
  }
  window.__apfs_post__ = postToSwift;

  // Mirror console.error to Swift so silent JS failures show up in
  // the Xcode console / `swift run` stderr instead of evaporating
  // inside WebKit.
  const __origConsoleError = console.error.bind(console);
  console.error = function() {
    __origConsoleError.apply(console, arguments);
    try {
      const parts = [];
      for (let i = 0; i < arguments.length; i++) {
        const a = arguments[i];
        if (a instanceof Error) {
          parts.push(a.stack || a.message);
        } else if (typeof a === 'object') {
          try { parts.push(JSON.stringify(a)); }
          catch (_) { parts.push(String(a)); }
        } else {
          parts.push(String(a));
        }
      }
      postToSwift({ type: 'console_error', message: parts.join(' ') });
    } catch (e) {}
  };
  window.addEventListener('error', function(ev) {
    const msg = (ev && ev.error && (ev.error.stack || ev.error.message)) || (ev && ev.message) || 'unknown error';
    postToSwift({ type: 'console_error', message: 'window.error ' + msg });
  });
  window.addEventListener('unhandledrejection', function(ev) {
    const r = ev && ev.reason;
    const msg = (r && (r.stack || r.message)) || String(r);
    postToSwift({ type: 'console_error', message: 'unhandledrejection ' + msg });
  });

  // Swift signals "a new scan result is available"; the page fetches
  // it via the apfs-scan:// custom scheme. The Swift-side
  // `WKURLSchemeHandler` serves the bytes from the latest scan temp
  // file.
  window.__apfs_ingest_file__ = function(_pathHint) {
    postToSwift({ type: 'ingest_started' });
    try {
      const xhr = new XMLHttpRequest();
      xhr.open('GET', 'apfs-scan://current', true);
      // Pull the bytes as text first so a malformed-JSON failure
      // surfaces with a real message instead of `responseType:
      // 'json'` silently delivering `null`.
      xhr.onload = function() {
        const ok = xhr.status === 0 || (xhr.status >= 200 && xhr.status < 300);
        if (!ok) {
          const msg = 'scan fetch http ' + xhr.status;
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          return;
        }
        const text = xhr.responseText || '';
        if (!text.length) {
          const msg = 'scan fetch returned empty body';
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          return;
        }
        let doc;
        try {
          doc = JSON.parse(text);
        } catch (parseErr) {
          const msg = 'scan parse failed: ' + (parseErr && parseErr.message ? parseErr.message : parseErr);
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          return;
        }
        if (typeof window.ingest !== 'function') {
          const msg = 'viz ingest() function missing';
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          window.__apfs_pending_scan__ = doc;
          return;
        }
        try {
          window.ingest(doc, 'native://current-scan');
        } catch (ingestErr) {
          const msg = 'viz ingest threw: ' + (ingestErr && ingestErr.stack ? ingestErr.stack : ingestErr);
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          return;
        }
        const parserOutput = (doc && (doc.parser_output || doc)) || {};
        const entries = parserOutput.entries || [];
        const rootPath = (entries[0] && entries[0].path) || '';
        const source = parserOutput.source || {};
        // SourceDescriptor.source_kind tells the host which class of
        // scan produced these entries: 'mounted_directory' (fallback,
        // on-disk paths reachable from the shell), 'dmg_image'
        // (detached image; paths NOT reachable; file ops are
        // disabled), 'raw_device' (likewise). The shell uses this to
        // grey out Reveal in Finder / Move to Trash for unreachable
        // scans.
        const sourceKind = source.source_kind || '';
        const sourceRequestedPath = source.requested_path || '';
        // `window.ingest(doc)` already populated rootNode with both
        // metrics; reuse those totals so Swift doesn't have to
        // re-sum the tree. `allocatedTotal === null` is the SR-019 /
        // EX-22 unclaimed marker and is preserved as JSON null.
        let logicalTotal = 0;
        let allocatedTotal = null;
        let allocatedAvailable = false;
        try {
          if (window.rootNode) {
            logicalTotal = window.rootNode.valueLogical || 0;
            allocatedTotal = window.rootNode.valueAllocated;
          }
          allocatedAvailable = !!window.allocatedAvailable;
        } catch (_ignored) { /* the viz still rendered fine */ }
        postToSwift({
          type: 'ingest_succeeded',
          rootPath: rootPath,
          totalEntries: entries.length,
          logicalTotal: logicalTotal,
          allocatedTotal: allocatedTotal,
          allocatedAvailable: allocatedAvailable,
          sourceKind: sourceKind,
          sourceRequestedPath: sourceRequestedPath
        });
      };
      xhr.onerror = function() {
        const msg = 'scan fetch transport error';
        console.error(msg);
        postToSwift({ type: 'ingest_failed', message: msg });
      };
      xhr.send();
    } catch (err) {
      const msg = '__apfs_ingest_file__ threw: ' + (err && err.stack ? err.stack : err);
      console.error(msg);
      postToSwift({ type: 'ingest_failed', message: msg });
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
