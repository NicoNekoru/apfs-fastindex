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
                    // Sniff the first byte to pick the right
                    // Content-Type. JSON output starts with `{`
                    // (0x7b); MessagePack output starts with a
                    // fixmap (0x80-0x8f) or map16/map32
                    // (0xde/0xdf). Anything else is fail-closed
                    // — we'd rather the page show a parse error
                    // than mis-label the payload.
                    let firstByte = data.first ?? 0
                    let isJson = firstByte == 0x7b // '{'
                    let mime = isJson
                        ? "application/json"
                        : "application/x-msgpack"
                    let contentType = isJson
                        ? "application/json; charset=utf-8"
                        : "application/x-msgpack"
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
                            "Content-Type": contentType,
                            "Content-Length": "\(data.count)",
                            "Access-Control-Allow-Origin": "*",
                            "Cache-Control": "no-store"
                        ]
                    ) ?? URLResponse(
                        url: requestURL,
                        mimeType: mime,
                        expectedContentLength: data.count,
                        textEncodingName: isJson ? "utf-8" : nil
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
  // file, with a Content-Type that distinguishes json / msgpack.
  // We pull the body as an ArrayBuffer (avoids WebKit's
  // UTF-8 → JS-string intermediate that `xhr.responseText` would
  // create), hand it to `window.ingestRawBytes` which dispatches to
  // the right decoder, then read the populated `window.rootNode`
  // for the ingest_succeeded payload.
  window.__apfs_ingest_file__ = function(_pathHint) {
    postToSwift({ type: 'ingest_started' });
    try {
      const xhr = new XMLHttpRequest();
      xhr.open('GET', 'apfs-scan://current', true);
      xhr.responseType = 'arraybuffer';
      xhr.onload = function() {
        const ok = xhr.status === 0 || (xhr.status >= 200 && xhr.status < 300);
        if (!ok) {
          const msg = 'scan fetch http ' + xhr.status;
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          return;
        }
        const buffer = xhr.response;
        if (!buffer || buffer.byteLength === 0) {
          const msg = 'scan fetch returned empty body';
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          return;
        }
        const contentType = xhr.getResponseHeader('Content-Type') || '';
        if (typeof window.ingestRawBytes !== 'function') {
          const msg = 'viz ingestRawBytes() function missing';
          console.error(msg);
          postToSwift({ type: 'ingest_failed', message: msg });
          return;
        }
        // `ingestRawBytes` returns a Promise that resolves after
        // the canvas has actually been painted (not just after the
        // bytes were decoded). Awaiting it before posting
        // `ingest_succeeded` keeps the loading spinner up until
        // the user can see the treemap — without this hop, a
        // multi-second slow-path render would happen between
        // spinner-clear and first-paint.
        const ingestPromise = window.ingestRawBytes(buffer, contentType, 'native://current-scan');
        const onIngestDone = function(ok) {
          if (!ok) {
            postToSwift({ type: 'ingest_failed', message: 'ingestRawBytes returned false; see console_error' });
            return;
          }
          // `window.rootNode.itemCount` is the descendant count
          // (matches `entries.length` because `buildHierarchy`
          // walked every row); `window.scanSource` carries the
          // SourceDescriptor the native shell uses to enable /
          // disable file ops; logical / allocated totals come
          // from the rootNode value-* fields. Pulling them off
          // `window` means we never have to keep the parsed
          // entries array alive past the ingest call.
          const totalEntries = (window.rootNode && window.rootNode.itemCount) || 0;
          const rootPath = '';
          const sourceKind = (window.scanSource && window.scanSource.source_kind) || '';
          const sourceRequestedPath = (window.scanSource && window.scanSource.requested_path) || '';
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
            totalEntries: totalEntries,
            logicalTotal: logicalTotal,
            allocatedTotal: allocatedTotal,
            allocatedAvailable: allocatedAvailable,
            sourceKind: sourceKind,
            sourceRequestedPath: sourceRequestedPath
          });
        };
        if (ingestPromise && typeof ingestPromise.then === 'function') {
          ingestPromise.then(onIngestDone, function(err) {
            console.error('ingestRawBytes rejected: ' + (err && err.message ? err.message : err));
            postToSwift({ type: 'ingest_failed', message: 'ingestRawBytes rejected; see console_error' });
          });
        } else {
          onIngestDone(!!ingestPromise);
        }
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
