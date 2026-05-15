# apfs-fastindex native shell (Phase 1)

A SwiftUI macOS app that wraps the existing scanner + viz so we can
attach the native features the web demo can't reach (file selection,
Reveal in Finder, Move to Trash, context menus). This is Phase 1 of a
larger plan; see `docs/implementation/000-implementation-index.md` for
the trajectory.

## What's in this build

- Toolbar with: target path field, Browse… button, mode picker
  (auto / raw / fallback), cross-mounts toggle, Scan / Cancel.
- `WKWebView` loading the bundled `viz/index.html` (the same depth-N
  treemap the standalone HTML demo uses).
- `ScanController` runs `apfs-fastindex-scan --slim --progress` as a
  subprocess, streams stderr progress to the toolbar / status bar, and
  pipes the stdout JSON into the viz via the `__apfs_ingest__` shim.
- Bottom status bar: mode pill, live scanning state, skipped count
  (orange pill, clickable in the next phase), currently-selected path.

## What's not in this build yet

- Right-click context menu (Reveal in Finder, Move to Trash, Copy
  Path) — Phase 2.
- Top-N sidebar + path search — Phase 3.
- Bundled scanner binary — Phase 4. For now the app shells out to the
  release binary the user built with `cargo build --release` from the
  repo root.
- Code signing / notarization — Phase 4.

## Run it

```sh
# 1. Build the Rust scanner (the app shells out to it).
cargo build --release --bin apfs-fastindex-scan

# 2. Build a proper .app bundle and launch it.
cd app
./make-app.sh
open ApfsFastindex.app
```

`make-app.sh` runs `swift build -c release`, copies the executable and
the SwiftPM resource bundle into `ApfsFastindex.app/Contents/`, and
writes a minimal `Info.plist`. The first launch takes ~30 s while
SwiftPM compiles; subsequent rebuilds are near-instant.

### `swift run` for development iteration

`swift run` also works (`./.build/debug/ApfsFastindex` launches the
window) but SwiftPM-built binaries aren't `.app` bundles, so macOS
launches them as CLI tools by default. The app forces
`.regular` activation policy in `init` + `applicationWillFinishLaunching`,
which is usually enough — if the window doesn't focus, click the dock
icon or run `osascript -e 'tell app "apfs-fastindex" to activate'`. For
anything beyond fast iteration, prefer `make-app.sh`.

To scan, type a path in the toolbar (or click Browse…) and hit Scan
(Cmd-Return). Toolbar progress + status bar update live; the treemap
renders once the scan finishes.

## Layout

```
app/
  Package.swift                          # SwiftPM, macOS 13+
  Sources/ApfsFastindex/
    ApfsFastindexApp.swift               # @main; SwiftUI App scene
    ContentView.swift                    # toolbar + viz + status bar
    ScanController.swift                 # subprocess driver + state
    VizWebView.swift                     # NSViewRepresentable wrapper
    BridgeProtocol.swift                 # typed JS→Swift messages
    Resources/
      viz/                               # bundled copy of repo viz/
        index.html
        vendor/d3.v7.min.js
```

The bundled `viz/` mirrors `/viz/` at the repo root. They must stay in
sync; a build-time `cp -R` will replace the manual mirroring in Phase
4. For now, if you edit one, mirror the change to the other (the same
contract — `parser_output.entries` etc. — is on both sides).

## JS ↔ Swift bridge

Swift → JS (via `WKWebView.evaluateJavaScript`):

- `window.__apfs_ingest__(doc)` — hand a scan document to the viz.
  Equivalent to dragging a JSON file in.
- `window.__apfs_progress__(update)` — push a streaming progress event.
  The viz holds this in `window.__apfs_latest_progress__`; a later viz
  polish pass renders it as a progress bar inside the page.

JS → Swift (via `window.webkit.messageHandlers.app.postMessage`):

- `{ "type": "selected", "path": "...", "kind": "...", "size": ... }`
- `{ "type": "context_menu", "path": "...", "x": ..., "y": ... }` —
  Phase 2 will show an `NSMenu` here.
- `{ "type": "reveal_in_finder", "path": "..." }` — Phase 2.
- `{ "type": "move_to_trash", "paths": [...] }` — Phase 2.

The Swift side parses these into a typed `BridgeMessage` enum
(`BridgeProtocol.swift`) before handing them to the controller.
