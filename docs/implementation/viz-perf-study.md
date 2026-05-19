# Viz End-to-End Performance Study

Status: Active
Date: 2026-05-18
Author: Claude (apfs-fastindex session)
Scope: full path from `apfs-fastindex-scan` exit to user-visible
       rectangle, including ingest, render, and the depth/metric/
       navigation controls.

This note replaces the v1 perf study now that the user's two
specific complaints have surfaced:

1. **"It does still rerender when going up."** The v1 layout
   cache skipped the layout pipeline on a hit but still rebuilt
   the SVG DOM. On a `/`-scan, DOM build is most of the cost.
2. **"We store the full disk data in memory twice, once in Rust,
   another time in JS."** The Rust scanner serialises to JSON, JS
   re-materialises the same data in WebKit's heap.

Both point at the same architectural fact: the Rust ↔ WebKit
bridge is a JSON round-trip, and the WebKit-side render uses SVG.
The fastest path forward picks one of these to attack first; this
note lays out the options and recommends a build order.

## 1. Where time and memory go today

Steady-state path on a `/`-class scan (~3 M entries, ~150 MB JSON):

| stage | wall (est.) | dominant cost | freeable? |
|-------|-------------|---------------|-----------|
| Rust scan (`apfs-fastindex-scan`) | ~120 s cold / ~20 s warm | syscalls, sort | not via viz |
| Rust → JSON serialise | ~2-3 s | `serde_json::to_writer` | yes (binary format) |
| Swift writes temp file | ~200-400 ms | one `fwrite` | yes (skip the file) |
| WKWebView XHR fetch + UTF-8 decode | ~400-600 ms | bytes → string | partly (binary) |
| `JSON.parse(text)` | ~1.5-3 s | parse | yes (binary or streaming) |
| `viz.ingest()` build hierarchy | ~500-1500 ms | walk entries → tree | partly (streaming) |
| `render()` first paint at depth N | ~2-7 s (depth-dependent) | DOM build | yes (canvas) |
| `render()` re-paint on depth change | ~150 ms - 3 s | DOM build | yes (canvas + cache) |

Memory peaks during ingest:

| location | size on `/`-scan | persistent? |
|----------|------------------|-------------|
| Rust scanner `Vec<NamespaceEntry>` | ~600 MB | no — process exits |
| Temp file on disk | ~150 MB | until next scan or app quit |
| WebKit `xhr.responseText` (UTF-8 string) | ~150 MB | dropped after JSON.parse |
| WebKit `JSON.parse` result (entries[]) | ~600-900 MB | dropped after ingest() (GC'd) |
| WebKit tree (`rootNode` + descendants) | ~600 MB | held until next scan |

Peak JS heap during ingest: ~1.4 GB. Steady state after ingest:
~600 MB.

The Rust process exits before ingest starts, so the "twice in
memory simultaneously" claim is only literally true during
serialise + decode + parse. **The redundancy that *is* persistent
is the WebKit tree mirroring data that already existed in Rust** —
the same bytes shaped twice, once in `Vec<NamespaceEntry>` and
once as JS objects with parent / child references.

## 2. What the v2 (this session) changes already buy

`treemapLayoutCache` (v1) + visibility-toggle on depth-down (v2):

- **Depth-down on the same (node, metric, dims)**: no layout, no
  DOM rebuild. One `style.display` flip per `.cell` element,
  measured in single-digit milliseconds even at 50 k rects.
  `pushPerf("visibility_toggle", …)` records the hit.
- **Depth-up on the same view, within the rendered max**: same
  visibility-toggle fast path. The directory cells at the new
  boundary keep their `dir-bg` + paddingTop strip and just
  un-hide their previously-hidden children.
- **Depth-up beyond the rendered max**: pays the full pipeline
  for the first visit to the new depth, then caches both the
  layout and the SVG state for future toggles. Subsequent
  depth-down → depth-up cycles within that max are free.
- **Back-navigation**: the layout cache hits, but the SVG context
  has changed (different `node.path`) so a DOM rebuild is still
  required. This is the cost the user feels next.

Uniform directory rendering is a prerequisite for the
visibility-toggle path to look right: directories at the depth
boundary now render as containers (with `dir-bg` fill + paddingTop
strip), regardless of whether d3 sees them as truncated leaves.
Hiding their children produces the same visual as a fresh render
at that depth would have.

## 3. The next-biggest wins

The remaining ceiling is **DOM build** (~2-3 s per render on a
`/`-scan) and **JSON parse** (~1.5-3 s per scan ingest). Three
independent levers, ordered by impact-per-effort:

### Lever A — `<canvas>` rendering (highest impact, moderate cost)

Replace the SVG output of `renderAtDepth()` with `<canvas>` draws.
Layout (d3.treemap) is unchanged; only the DOM build changes.
Expected effect on a `/`-scan:

- DOM build ~2-3 s → canvas draw ~50-100 ms (20-50×).
- Memory: removes ~600 MB of WebKit DOM nodes; canvas backing
  store is ~8 MB regardless of rect count.
- Back-navigation feels instant (cache hit on layout, fast paint
  on canvas).
- Hit-testing: add an interval-tree or a flat sorted index over
  the laid-out leaves. The depth-first treemap layout already
  groups siblings; a quad-tree index built once per render is
  O(n log n) build + O(log n) lookup. Negligible per render
  given the leaf count.
- Tooltips / context menus need to redraw an overlay or use a
  thin SVG layer for hover state; both are <5 ms.

Engineering cost: ~1-2 days. The depth control, navigation
plumbing, breadcrumb, tree-list, ext-list, bridge protocol, and
layout cache all carry over unchanged. No native code, no new
build steps, no API surface changes.

This is the recommended next step.

### Lever B — binary scan format (parse-time + memory)

Switch the scan output from JSON to MessagePack or CBOR. The
`NamespaceEntry` schema is already typed in
`crates/apfs-fastindex/src/lib.rs`; serialisation via `rmp-serde`
or `ciborium` is one trait swap. The viz side parses with
`@msgpack/msgpack` or `cbor-x` (~100 KB libraries).

Expected effect on a `/`-scan:

- Payload size ~150 MB → ~40-60 MB (2.5-4×).
- Parse + ingest ~3-4.5 s → ~700 ms - 1.5 s (3-6×).
- Memory: peak ~1.4 GB → ~600 MB (no UTF-8 decode buffer, no
  intermediate string).
- The standalone HTML viz (drop a JSON file into a browser)
  stops working unless we also keep a JSON mode behind a flag.

Engineering cost: ~1 day Rust + ~½ day JS. The bigger cost is
the standalone-viz UX — we'd either ship `apfs-fastindex-scan`
with `--format=msgpack|json` (default json for compat) or accept
that the in-browser file-drop demo handles a smaller scan size.

### Lever C — streaming ingest

WKURLSchemeHandler can call `didReceive(data:)` multiple times.
Wire the XHR `onprogress` event so the viz starts consuming
chunks before the whole payload lands. Combined with a streaming
JSON or MessagePack parser, the user sees the first depth-1
treemap before the scanner's full output has even been read into
WebKit.

Expected effect: ~30-40 % win on time-to-first-paint for cold
ingest; doesn't change steady-state render perf.

Engineering cost: ~1-2 days. The streaming-parser library is the
fiddly part. Lower ROI than A or B unless the user complains
specifically about the "blank screen between scan-exit and first
treemap" window.

## 4. The "go beyond WebView" question, revisited

The user explicitly asked again: should we get out of WKWebView?

The honest read **with the v2 changes landed**: still not yet.
The remaining cost we can attack inside WKWebView is ~5-10× on
both render and ingest with options A + B. That covers the
performance gap on `/`-class scans on Apple silicon. Going fully
native buys another 2-3× on render and removes the JSON round
trip entirely, but the engineering bill is 1-2 weeks vs. 2-3
days for A + B, and the user-facing benefit overlaps heavily.

**The case for going native** is honestly narrower than "perf":

- One canonical data shape. No JSON, no JS tree, no marshalling.
  The Rust scanner can hand the entry buffer directly to a Swift
  `treemap` module via a C ABI; the renderer reads it in place.
- The "double storage" complaint disappears completely (one Rust
  process exits, hands its `Vec` to Swift via a memory map or a
  thin FFI; Swift renders from the same bytes).
- No standalone HTML viz to maintain.
- Faster iteration on macOS-specific UI affordances (force-press,
  quick-look, drag-and-drop into Finder).

**The case against** is just as honest:

- Two-week engineering bill before anyone sees a faster pixel.
- Loses the dev-loop speed of editing `viz/index.html` and
  hitting reload.
- Loses cross-platform path entirely (the HTML viz could
  conceptually run on Linux + a different scanner; native
  AppKit can't).
- d3.treemap is mature and battle-tested; reimplementing
  squarify in Swift is straightforward but yet-another thing to
  keep correct.

Recommendation order:

1. Land canvas rendering (Lever A). **~1-2 days; biggest single
   win.**
2. Land the binary scan format (Lever B). **~1.5 days; biggest
   ingest win.**
3. Measure. If end-to-end on `/`-scan is still painful, then go
   native. Otherwise, stop here — we've moved the perf ceiling
   ~10× without rewriting the renderer.

## 5. Action: what to do in the *next* session

In priority order, with concrete entry points:

1. **Canvas migration of `renderAtDepth()`**. Replace `dirSel` /
   `leafSel` SVG-building with a `<canvas>` `2d` context that
   draws rects + labels. Build a flat array of laid-out cells
   (already produced by d3) and a quad-tree for hit-testing.
   Replace `mousemove` / `click` / `contextmenu` on SVG cells
   with single listeners on the canvas that look up via the
   quad-tree. The layout cache, depth picker, navigation,
   breadcrumb, tree-list, and ext-list stay as-is.
2. **Binary scan format**. Add a `--format msgpack` flag to
   `apfs-fastindex-scan`. Default to `json` so the standalone
   HTML viz keeps working. Swift defaults to `--format msgpack`
   when invoking the scanner; the URL-scheme handler reports the
   `Content-Type`, and the viz dispatches to `@msgpack/msgpack`
   instead of `JSON.parse`.
3. **Drop the temp file**. Pipe the scanner's stdout through the
   URL-scheme handler directly. Saves the disk write/read round
   trip (~200-600 ms) and one filesystem allocation.

Each lands independently. Cumulative effect on `/`-scan time-to-first-paint:
~12 s → ~2-3 s. Steady-state render-on-depth-up: ~3 s → ~80 ms.

## 6. Live perf inspection

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
- `dom_build` — wall-time of the SVG build phase for the most
  recent render. Carries `dirCount`, `leafCount`.
- `visibility_toggle` — the depth-down fast path. Carries
  `fromDepth`, `toDepth`. Should read sub-millisecond.

A useful summary query in the console:

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
