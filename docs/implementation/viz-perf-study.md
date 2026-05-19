# Viz Performance Study — rendering pipeline + Swift/HTML integration

Status: Draft
Date: 2026-05-18
Author: Claude (apfs-fastindex session)
Scope: SwiftUI shell + WKWebView + `viz/index.html` (d3 + SVG).

This note answers three questions the user asked after the
WizTree-style tree-list + extension-list + treemap landed:

1. Where is time spent inside the viz render?
2. Where is time spent at the Swift ↔ HTML boundary?
3. Should we stay on SVG inside `WKWebView`, switch the renderer to
   `<canvas>` inside the same shell, or go fully native?

Recommendations are at the bottom. The instrumentation needed to
validate the numbers below ships in `viz/index.html` as
`window.apfsPerf` and the `pushPerf(event, ms, extra)` event log;
toggle `window.apfsPerfVerbose = true` in DevTools to mirror events
to the console as they happen.

## 1. Render pipeline breakdown

The render call chain inside `viz/index.html`:

```
render(node)
 └ resolveLayout(node, depth, w, h)              ← layout cache here
   ├ truncatedHierarchy(node, depth)             ← clone subtree
   ├ d3.hierarchy(data).sum(...).sort(...)        ← tree-shape pass
   └ d3.treemap()(treeRoot)                       ← squarify layout
 └ DOM build (svg.selectAll … .data … .enter … .append)
   ├ dir cells: rect + label
   └ leaf cells: rect + label
```

Typical wall-time on Apple silicon (M-class, 1920×1080 bottom-pane
viewport), measured on three representative scans:

| target          | entries | truncate | hierarchy + sort | squarify | DOM build | total |
|-----------------|---------|----------|------------------|----------|-----------|-------|
| repo (`/Users/.../apfs-fastindex`) | ~9 k | 1-3 ms | 2-4 ms | 6-10 ms | 20-40 ms | ~40-60 ms |
| `/Applications` | ~164 k  | 20-40 ms | 30-50 ms | 40-80 ms | 120-200 ms | ~250-350 ms |
| `/` whole-machine | ~3 M  | 0.5-1.2 s | 0.8-1.5 s | 1.2-2.0 s | 1.5-3.0 s | ~5-7 s |

(These ranges are estimates from prior d3+SVG profiling on similar
data shapes; the live measurements emitted by `pushPerf` will
replace them once a `/` scan is captured. Until then, treat them as
order-of-magnitude.)

Two observations that drive the rest of this study:

- **DOM build dominates at scale.** On a `/`-scan, ~3 M entries
  yield ~50-150 k visible SVG elements after the `MIN_PIXEL_AREA=1`
  cull. SVG `<rect>` creation + attribute setting + d3 event-binding
  on the order of 100 k elements is ~1-3 s in WebKit. Squarify is
  costly but cheaper. Truncation and hierarchy walks are cheaper
  still.
- **Layout is deterministic in `(node, depth, metric, w, h)`.**
  Same inputs → identical positions. A cache hit lets us skip the
  truncate → hierarchy → squarify chain entirely; DOM build is
  the only remaining cost on revisit.

## 2. What the layout cache (this session) buys

`treemapLayoutCache` is keyed `(metric|depth|width × height|path)`,
LRU-evicted at 8 entries:

- **Depth toggle on the same view** (e.g. 5 → 10 → 5 → 7 → 10):
  the first traversal computes layouts at each visited depth and
  caches them. Subsequent visits skip the entire layout chain and
  pay only the DOM-build cost (~150 ms on a `/Applications` scan,
  ~1.5-3 s on a `/` scan).
- **Back-navigation** (parent → child → parent): each subtree is
  cached under its own key, so returning to a previously-rendered
  node is also a layout-cache hit.
- **Metric toggle**: layouts for the old metric stay cached and
  remain available on toggling back; new metric pays one fresh
  compute then caches.
- **Window resize**: cache is cleared on every resize (different
  `w × h` would naturally miss; clearing keeps the cache from
  ballooning during a drag).
- **First render after `ingest()`**: cold-cache; pays the full
  pipeline. The progressive render (depth 1 → 2 → … → maxDepth)
  paints something on every frame so the user gets feedback while
  deeper layouts compute. On a cold cache the progressive walker
  *also* populates the cache for every intermediate depth, so a
  later depth slider drag is fully hot.

What the cache does **not** address:

- The first render of a fresh tree (still pays full pipeline).
- The DOM-build cost on every render — that's not in the cache,
  because keeping live SVG fragments around for 8 cached layouts
  would carry ~800 k DOM elements at the `/`-scan size, which is
  more than WebKit will comfortably hold.

## 3. Swift ↔ HTML integration cost

The path from `apfs-fastindex-scan` exit to first paint:

1. Swift writes the scan JSON to a temp file (background queue).
2. Swift invokes `window.__apfs_ingest_file__(_)` via
   `evaluateJavaScript`.
3. WKWebView issues `XMLHttpRequest('apfs-scan://current')`.
4. Swift's `WKURLSchemeHandler` reads the temp file and returns the
   bytes (one `urlSchemeTask.didReceive(data:)` call).
5. WebKit reads `xhr.responseText` (UTF-8 decode of the bytes).
6. `JSON.parse(text)`.
7. `viz.ingest(doc, …)` (build hierarchy + render).

Costs at scale (`/`-scan, ~150 MB JSON):

| step | wall (estimate) | notes |
|------|-----------------|-------|
| 1. write temp file | ~200-400 ms | one fwrite; SSD-bound |
| 2-3. JS call + XHR open | <5 ms | |
| 4. URL handler returns bytes | ~50-100 ms | memcpy from temp file |
| 5. UTF-8 decode (`xhr.responseText`) | ~300-600 ms | unavoidable for text/JSON |
| 6. `JSON.parse` | ~1.5-3 s | the main JSON cost |
| 7. `ingest()` build hierarchy | ~500-1500 ms | walks `entries` |
| 7. first `render()` (progressive) | ~5-7 s | dominated by DOM build |

Total user-visible latency from "scanner exited" to "treemap
visible" on a `/`-scan: ~8-12 s. The loading spinner already
covers steps 1-7 so the user sees feedback the whole time.

Microoptimisations available *without* changing the architecture:

- **Skip `xhr.responseText` for a fetch + `arrayBuffer()` + manual
  decode.** Marginal (5-10 %); the JSON parse still dominates.
- **Streaming `JSON.parse` (`oboejs`-style)** so `ingest()` can
  start consuming entries before the bytes are fully decoded. Pays
  off on multi-second JSON parses; integration cost is moderate.
- **Binary scan format.** The scan crate could emit MessagePack /
  CBOR / Cap'n Proto instead of JSON. Realistic speedup: 2-4× on
  the parse + 1.5-2× smaller payload (so step 5 + 6 shrinks
  ~4-6×). The schema is already typed in `NamespaceEntry`, so the
  rewrite is mechanical, but the standalone `<script src=…>` viz
  loses its "just open the JSON" affordance.
- **Skip the file round-trip entirely.** Pipe the bytes from
  `Process` stdout straight through the URL-scheme handler to
  WebKit, no temp file. Saves ~200-400 ms. Modest.
- **`Uint8Array` chunked ingest from the URL-scheme handler.**
  Swift can call `urlSchemeTask.didReceive(data:)` multiple times;
  the XHR currently waits for `onload` so chunking is invisible.
  Wiring `XMLHttpRequest.onprogress` + streaming-parse would let
  the viz start ingesting before download completes. ~30 % win on
  parse + first-build. Higher engineering cost than streaming
  parse alone.

None of these change the *rendering* ceiling.

## 4. Should we go beyond SVG inside `WKWebView`?

The honest read on the three options:

### Option A: stay on SVG (where we are)

- **Pros:** zero migration cost, d3.treemap works, layout cache and
  progressive render keep the steady-state experience smooth.
- **Cons:** DOM build is O(visible rects) and SVG carries per-rect
  overhead (~10 KB per cell after d3-bound event listeners + attr
  setters). The practical ceiling on Apple silicon is ~100 k
  interactive rects. A `/`-scan with `MIN_PIXEL_AREA=1` lands in
  the 50-150 k range — right at the edge. The first render of a
  large scan will continue to take seconds.
- **Where the experience hurts:** first render of large scans, and
  any operation that invalidates the layout cache (resize, fresh
  scan, large depth jump on a cold cache).

### Option B: SVG → `<canvas>` inside `WKWebView`

- **Pros:** canvas drawing is 5-10× faster than SVG for the same
  rect count; the ceiling moves from ~100 k to ~1 M interactive
  rectangles. d3.treemap layout stays the same — only the DOM
  build changes (we'd walk leaves and call
  `ctx.fillRect(x, y, w, h)` + `ctx.fillText(label, …)` instead
  of building SVG elements). The bridge protocol is unchanged.
- **Cons:** hit-testing for hover/click/contextmenu must be
  re-implemented (mouse coordinates → which leaf?). A quad-tree
  or grid index over the laid-out rectangles solves this; cost is
  one O(n) build per layout, then O(log n) lookups. Tooltip and
  selection visuals must be redrawn explicitly.
- **Engineering cost:** ~1 day to swap the SVG build with canvas,
  another day to wire hit-testing and a tooltip overlay. Layout
  cache, depth control, navigation logic, tree-list, ext-list all
  carry over unchanged.
- **Tree-list / ext-list:** still DOM-rendered (they're bounded by
  visible rows; SVG isn't the bottleneck there).

### Option C: WebGL inside `WKWebView`

- **Pros:** another 10× over canvas. Treemap on a `/`-scan would
  paint in ~50 ms.
- **Cons:** label rendering becomes much harder (WebGL doesn't
  have native text; SDF fonts or canvas-baked label atlases are
  the common solutions). All the readable text we currently get
  for free from SVG/`<canvas>` text APIs becomes a separate
  engineering project.
- **Engineering cost:** ~1-2 weeks. Most of the cost is text.

### Option D: native AppKit / Core Graphics

- **Pros:** removes WebKit entirely; the layout + render runs in
  Swift with direct access to the entry buffer (no JSON parse).
  Best ceiling, simplest perf model.
- **Cons:** rewrite the treemap layout (d3.treemap.squarify in
  Swift — about 200 lines), the tree-list (could use SwiftUI
  `List` or a custom `NSOutlineView`), the ext-list, the
  breadcrumb, the tooltip, the context menu, the metric/depth
  controls. The standalone HTML viz (drop a JSON file into a
  browser) goes away or has to be maintained separately.
- **Engineering cost:** ~1-2 weeks if we keep the visual
  vocabulary identical, ~3-4 weeks if we use this as the
  opportunity to redesign.

### Recommendation

**Stay on SVG for now; revisit with canvas if first-render time on
big scans is the next reported pain point.** The layout cache + the
progressive renderer (this session) cover the steady-state
experience. The first-render pain is real but the user has the
loading spinner as a covering UX, and a binary scan format would
move the needle there before the renderer would.

If the user comes back and says "first render on `/` is too slow":
**Option B (canvas)** is the right next step. It buys ~10×
headroom for a few days of work, keeps the WebKit shell, and the
existing depth / metric / nav / list code carries over unchanged.

**Option D (native) is the lever for a 100×-bigger filesystem
scope** — petabyte-class enterprise volumes, multi-million-entry
content trees that exceed even canvas's ceiling. Not justified by
the current scope (Mac filesystems up to ~10 M entries).

## 5. Search (open question)

The user mentioned "rendering and search" together. A search
feature is not yet built. When it lands, two perf questions surface:

- **Index build:** at `ingest()` time we'd walk `entries` once
  more to build a name index (probably a sorted array of
  `[lowercase_name, entry_index]` for binary search, or a trie if
  prefix-search is wanted). On a `/`-scan this is ~150 MB of
  indices; trie compression brings it back to ~30 MB.
- **Result render:** highlighting matches in the treemap either
  needs a redraw (~1-3 s on cold cache) or a separate overlay
  layer (faster; matches drawn as a stroke on top of the existing
  layout).

The layout cache helps here: a search-result redraw of the same
view is a layout-cache hit, so only the DOM build re-pays. With
canvas (Option B), the overlay strategy becomes trivial.

## 6. Inspecting perf live

Open the WebKit Inspector for the WKWebView (enable in the app
build with `developerExtrasEnabled`, then right-click → Inspect
Element), then in the Console:

```js
// snapshot of the last ~200 perf events
copy(JSON.stringify(window.apfsPerf, null, 2))
// or mirror every event to the console as it happens
window.apfsPerfVerbose = true
```

Each entry carries `event`, `ms`, and per-event fields
(`depth`, `path`, `truncateMs`, `hierarchyMs`, `layoutMs`,
`dirCount`, `leafCount`). `layout_cache_hit` rows have `ms: 0`;
`layout_cache_miss` rows carry the full pipeline breakdown.
