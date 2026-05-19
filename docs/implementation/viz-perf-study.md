# Viz End-to-End Performance Study

Status: Active (v4)
Date: 2026-05-18
Author: Claude (apfs-fastindex session)
Scope: full path from `apfs-fastindex-scan` exit to user-visible
       rectangle.

## 0. What's already landed

In session order, oldest first:

- **Tree-list + ext-list panels** above the treemap (WizTree-style
  three-pane layout).
- **Depth control + truncated hierarchies** so a /-scan doesn't
  try to lay out 3 M leaves at once.
- **Layout cache** keyed `(node, depth, metric, dims)` — back-
  navigation and depth-toggle revisits hit cache.
- **Visibility-toggle fast path** + **uniform dir rendering** so
  depth-down within previously-rendered max is a `style.display`
  flip per cell (SVG era) / a `currentDepthFilter` redraw
  (canvas era).
- **Canvas migration** replacing the SVG `<g><rect><text>` build
  with `ctx.fillRect` + `ctx.fillText` on a `<canvas>`. Hit-test
  via a 64×64 spatial hash.
- **Binary scan format** (rmp-serde + inline JS msgpack decoder)
  plumbed end-to-end. **Falsified**: V8/JSC `JSON.parse` is
  ~3× faster than a pure-JS msgpack decoder; flipped Swift
  default back to JSON.
- **Bytes-aware ingest** — XHR is `responseType='arraybuffer'`,
  TextDecoder + `JSON.parse` or `decodeMsgpack` via
  `ingestRawBytes`. No `xhr.responseText` UTF-16 intermediate.
- **Label truncation** at flatten time so canvas text doesn't
  overflow cells.
- **Promise-chained ingest → render → ingest_succeeded** so the
  loading spinner stays up until the canvas is actually painted.
- **Early spinner via `terminal: true` progress event** —
  spinner appears when the scan finishes scanning, not when the
  subprocess finishes serialising + writing stdout.
- **`drawCells` batched by colour** — one `ctx.fillStyle` per
  unique leaf colour instead of one per leaf (~50 k state
  changes → ~10). Strokes dropped (0.5 px alpha-stroke was
  sub-pixel and 3-5× slower than fillRect).
- **Hit grid built lazily** on first hover after render, off the
  first-paint critical path.
- **No rAF yield on ingest path** — cold first-paint is
  synchronous; depth/nav still defer one frame for visual
  continuity.
- **`buildHierarchy` hot loop rewritten** — no `split("/").filter`
  array allocation per entry, cumulative path only built on
  node creation.
- **`finalize` folded with metric application** — one O(n) walk
  not two.
- **Lever C: temp file gone** — `vizCoordinator.currentScanData`
  is set directly from `stdout`; URL-scheme handler reads the
  in-memory `Data`. No fwrite, no `Data(contentsOf:)`.

JS-side cold-path optimisation is essentially exhausted. The
next paragraph is the honest version of "what's left".

## 1. What's left, ranked

Numbers are estimates on a `/`-class scan (~3 M entries) on the
host the previous bench runs were on. They scale roughly linearly
with entry count.

| lever | effort | total-time gain | UX gain | architectural cost |
|-------|--------|-----------------|---------|--------------------|
| Web Worker + OffscreenCanvas (Lever E) | 2-3 days | ~0 ms total | "scan loads to fully responsive UI" (main thread stays idle) | medium |
| Streaming ingest (true Lever C) | 3-5 days | -500 ms to -1.5 s on time-to-first-paint | first paint at depth=1 well before full data ingested | high (custom JSON / msgpack streaming parser, chunked URL-scheme handler) |
| Custom squarify (skip d3.treemap) | 2-3 days | -100-300 ms layout | smaller bundle | low |
| WASM build + layout (Lever F) | 1-2 weeks | -1-2 s (buildHierarchy at native speed) | none | high (wasm-bindgen, msgpack in WASM, JS↔WASM ABI) |
| Native rendering (Lever D) | 1-2 weeks | dominant — see § 2 | dominant | very high (parallel rendering stack) |

A few observations:

- The Web Worker offload is the **only** "still in JS land" change
  that meaningfully fixes the *perceived* freezing on big scans:
  the main thread does almost nothing while the worker decodes /
  builds / lays out / draws to OffscreenCanvas. Total wall time
  is the same; what changes is that the user can scroll the
  tree-list, click into a path, or change the depth picker while
  the worker is still busy with the previous render. That's a
  big subjective win even though the timer reads the same number.

- Streaming ingest (the original Lever C) is moderate effort with
  a real total-time win, because it parallelises parse +
  buildHierarchy + scanner output. The Rust scanner already
  writes incrementally to stdout; the gating problem is that
  WKURLSchemeHandler-via-XHR-arraybuffer waits for the whole
  payload. Switching to `fetch()` + `ReadableStream` and writing
  a streaming parser would let `ingest` start consuming entries
  as the bytes arrive.

- Custom squarify is a small win in isolation. It would only be
  worth doing alongside Lever E (since it'd run in the worker)
  or Lever F (since it'd be ported to Rust anyway).

- WASM gets ~3-5× over JS for the build-hierarchy hot loop. But
  the engineering cost is comparable to going fully native (you're
  authoring a wasm-bindgen module, designing the JS↔WASM call
  contract, and the renderer still has to copy out to JS to draw)
  — and the resulting architecture is *more* complicated than
  native, not less.

**Read on these:** Lever E is a real-but-bounded win. Lever D
(native) is the next-larger swing, and it's cleaner.

## 2. Native (Lever D) feasibility

The bare-metal path replaces the WKWebView treemap view with a
native renderer. The rest of the SwiftUI app stays. Concretely:

### 2.1 Architecture sketch

```
┌──────────────────────────────────────────────────┐
│ SwiftUI shell (toolbar, breadcrumb, splitters,    │
│   tree-list, ext-list, status bar)                │
│                                                   │
│   ┌────────────────────────────────────────────┐ │
│   │ NSViewRepresentable wrapping a custom NSView │ │
│   │ that draws the treemap (CoreGraphics or       │ │
│   │ Metal — see § 2.4)                           │ │
│   └────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────┘
            │
            │ FFI / shared C ABI
            ▼
┌──────────────────────────────────────────────────┐
│ Rust apfs-fastindex (in-process via dylib, not    │
│   a subprocess as today)                          │
│                                                   │
│  - scans                                          │
│  - holds the `Vec<NamespaceEntry>`                │
│  - exposes a flat tree API to Swift               │
│  - hands a typed buffer (paths, sizes, parent     │
│    indices) over the FFI boundary                 │
└──────────────────────────────────────────────────┘
```

### 2.2 What each piece costs

| piece | est. effort | risk |
|-------|------------|------|
| Rust crate as cdylib + C ABI | 1-2 days | low — `crates/apfs-fastindex` already exposes typed structs |
| Swift wrapper (`@_silgen_name` declarations, `withUnsafePointer` plumbing) | 1-2 days | low |
| Port d3 squarify to Swift (~200 LOC) | 1-2 days | low — algorithm is well-documented, no APFS-specific twists |
| NSView subclass + Core Graphics drawing | 2-3 days | low — fill rects, draw text |
| Hit-test (existing 64×64 grid logic ports trivially) | half a day | low |
| Tree-list / ext-list / breadcrumb rewired to read from Rust buffer | 2-3 days | medium — SwiftUI `List` is fine; binding to the Rust buffer needs care |
| Tooltip + context-menu integration (already SwiftUI / AppKit) | half a day | low |
| Drop bridge protocol, WKURLSchemeHandler, JS msgpack decoder | half a day | low |
| Standalone HTML viz: keep, archive, or delete? | one decision | low |

Total: ~10-14 working days of focused effort for a feature-equivalent native renderer.

### 2.3 What native buys

Hard performance numbers we'd hit:

- **buildHierarchy**: Rust runs at ~10-20× the speed of the JS
  rewrite that just landed. ~2-3 s → ~150-300 ms on `/`-scan.
- **Squarify**: Swift / Rust at ~5-10× JS. ~200-1000 ms → ~30-100 ms.
- **Draw**: Core Graphics `CGContextFillRect` at ~10× canvas
  `fillRect`. ~200-1000 ms → ~20-100 ms. Metal is another 10× past
  that if needed.
- **Memory**: one Rust `Vec` shared with Swift via FFI. No JSON
  buffer, no JS tree, no msgpack decode arena. ~1.4 GB peak →
  ~600 MB peak (basically just the entry data once).
- **No IPC**: Rust runs as a dylib loaded into the app process,
  not a subprocess. Saves the ~150 MB scan-bytes round trip on
  every scan.

Steady-state cold-path time-to-first-paint, projected from the
component numbers above:

| target | today (canvas+microopts) | native (projected) |
|--------|--------------------------|---------------------|
| repo (`~9 k entries`) | ~150 ms | ~30 ms |
| `/Applications` (~164 k) | ~800 ms-1.5 s | ~150-300 ms |
| `/` (~3 M) | ~5-8 s | ~600 ms-1.5 s |

Depth change / navigation on cached layout drops from ~50-300 ms
(canvas draw) to ~10-30 ms (CoreGraphics fillRect).

### 2.4 Core Graphics vs Metal

Core Graphics is the right starting point. Per-frame draw cost on
even a `/`-scan with ~150 k visible rects is ~20-50 ms in CG —
already in the "feels instant" zone. Metal would buy another 10×
but at materially higher complexity (shaders, vertex buffers,
text atlas baking), and the perf budget doesn't need it.

### 2.5 What native *doesn't* buy

- **Scan time itself.** Rust scanner already runs ~as fast as
  syscalls let it (~120 s on /-scan cold, ~20 s warm). Going
  native doesn't help here.
- **JSON parse / msgpack decode.** Goes away entirely because
  we're not crossing a serialisation boundary. ~30 ms-300 ms saved.
- **Standalone HTML viz.** The drop-a-file-into-a-browser
  workflow keeps working — `viz/index.html` is a separate
  artifact. We just stop *bundling* it into the native app.

### 2.6 Risks

- **Two renderers to keep correct.** If we keep the HTML viz for
  standalone use, the native renderer has to match its visual
  vocabulary. d3.treemap.squarify is well-specified; a Swift
  port produces identical positions for the same value array.
  Cross-renderer tests are cheap (snapshot the cell positions).
- **Rust → Swift FFI churn.** Adding fields to `NamespaceEntry`
  now needs ABI bumps on both sides. Manageable with a generated
  header (`cbindgen`).
- **Dev loop.** Editing the SwiftUI shell + rebuilding the dylib
  is slower than editing `viz/index.html` and hitting reload.
  Real cost — keep a debug-mode "load JSON / msgpack file"
  affordance for fast iteration on the rendering code without
  re-running scans.

## 3. Recommendation

Two reasonable paths, pick one:

**Path A — Lever E (Worker + OffscreenCanvas) now, native later.**
~2-3 days of work. Total time stays the same but the main thread
becomes responsive during ingest / cold render — the user can
interact while the worker churns. Buys time for the native port.

**Path B — Native (Lever D) now.** ~2 weeks of work. Cuts
cold-path time-to-first-paint by 5-10× and removes the architecture
costs (JSON round trip, JS tree, msgpack plumbing) the perf study
keeps tripping over.

My honest read: we've squeezed the JS path. Lever E buys
*perceived* perf, not real perf; the user's "still really quite
long" complaint is about wall time on big scans, which only the
native path materially shortens. **Path B is the right next move
if the scope is "the app stays useful on `/`-scale Macs."**

The standalone HTML viz can stay as-is (no work needed) — it's
already feature-complete for "open this scan JSON in a browser."

## 4. Live perf inspection (unchanged)

Open the WKWebView Inspector (right-click → Inspect Element after
enabling `developerExtrasEnabled` in the WKWebView config), then
in the Console:

```js
// Snapshot of the last ~200 perf events (most recent first):
copy(JSON.stringify(window.apfsPerf.slice().reverse(), null, 2))

// Mirror every event to the console as it happens:
window.apfsPerfVerbose = true
```

Events emitted today:

- `layout_cache_hit` / `layout_cache_miss` — layout-cache result
  per `(node, depth, metric, dims)`. Misses carry
  `truncateMs`, `hierarchyMs`, `layoutMs`.
- `render_full` — full-rebuild render (cold cache or new context).
  Carries `flattenMs`, `drawMs`, `cellCount`.
- `draw` — canvas draw pass. `ms` is `-1` because the cost is
  bounded by drawn-cell count; read `drawnDirs`, `drawnLeaves`,
  `colorGroups` instead.
- `redraw_depth_change` — depth toggle on the same view.
- `ingest_decode` — bytes-to-doc decode wall time. Carries
  `bytes`, `contentType`, `encoding`.

Summary recipe:

```js
const grouped = window.apfsPerf.reduce((acc, e) => {
  (acc[e.event] ||= []).push(e.ms);
  return acc;
}, {});
console.table(Object.fromEntries(
  Object.entries(grouped).map(([k, vs]) => [k, {
    count: vs.length,
    p50: vs.slice().sort((a,b)=>a-b)[Math.floor(vs.length/2)],
    max: Math.max(...vs),
  }])
));
```
