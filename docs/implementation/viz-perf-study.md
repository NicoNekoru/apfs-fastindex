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

### Lever B — binary scan format (negative result on parse speed)

The v1 study predicted MessagePack would be ~3-6× faster on
ingest than JSON. The implementation landed (rmp-serde on the
Rust side, hand-rolled msgpack decoder + content-type sniffing
on the JS side) and the measurements **falsified the
prediction**. Recording the negative finding here so future
work doesn't re-tread it.

Measured on `/Applications` (~164 k entries, `--slim`):

| encoding | wire size | client-side decode (Node V8) |
|----------|-----------|-------------------------------|
| JSON     | 27.2 MB   | ~30 ms                        |
| msgpack  | 25.3 MB   | ~90 ms                        |

Wire savings are modest (~7%) because path strings dominate
the payload and neither encoding compresses them. The bigger
surprise is decode speed: V8's `JSON.parse` is a heavily
optimised native C++ path with adaptive shape inference and
SIMD UTF-8 scanning; a pure-JS msgpack decoder, even keeping
the hot loop tight (no closures, cached TextDecoder, direct
DataView reads), is interpreted bytecode and lands ~3× slower
on this shape of payload. WebKit's JSC has comparable
JSON.parse optimisations; the same gap is expected there.

The plumbing remains end-to-end so a future WASM-backed msgpack
library (or a streaming JSON parser, see Lever C) can pick the
faster encoding at the URL-scheme-handler boundary without
touching either side of the bridge:

- `apfs-fastindex-scan --format json|msgpack` (default json).
- `WKURLSchemeHandler` sets `Content-Type` based on the byte
  signature of the served scan file.
- The viz shim now XHRs an `ArrayBuffer` regardless of encoding
  (skips WebKit's UTF-16 intermediate for `xhr.responseText`)
  and routes to `JSON.parse` or `decodeMsgpack` via
  `window.ingestRawBytes()`.

The arraybuffer-XHR change is a small standalone win — it
trims the peak ingest memory by ~150 MB on a `/`-scan
(no intermediate UTF-16 string) — and keeps the door open for
streaming.

**Take-away**: don't pursue msgpack-as-default. The parse-speed
ceiling is set by `JSON.parse`'s native implementation; beating
it requires native code (WASM, Rust → Swift FFI). The wire-size
~7% delta isn't enough to justify a slower JS path on its own.

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

The honest read **with the canvas migration landed and the
msgpack hypothesis falsified**: the remaining JS-side ingest
cost (~30 ms / 10 MB for `JSON.parse`, scaling linearly) is
hard to attack without leaving JavaScript. Canvas already
took render off the critical path. Going native buys roughly:

- another 2-3× on render (vs. canvas)
- the JSON round trip goes away (Rust hands a `Vec` directly to
  Swift)
- the literal "twice in memory" complaint is gone (Rust process
  hands the `Vec` to Swift via FFI or memory map; one buffer)
- ~150 ms saved on each `/`-class ingest (no JSON parse)

The engineering bill is 1-2 weeks; the user-visible payoff is
the ~150 ms ingest win plus the architectural cleanup.

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

1. ✅ **Land canvas rendering (Lever A)**. Done. ~10× on
   render at the cost of ~50 ms per redraw instead of sub-ms
   visibility toggles. Both directions (depth-up, depth-down,
   navigation back) collapse to canvas-draw time.
2. ✅ **Land the binary scan format (Lever B)**. Done as
   end-to-end plumbing; default flipped back to JSON after
   measurements falsified the parse-speed hypothesis (see § 3
   Lever B). msgpack ships as an opt-in encoding via
   `--format msgpack`.
3. **Measure on a real `/`-scan**. The numbers above are
   estimates; live `window.apfsPerf` capture is the next step
   before deciding whether anything else is worth doing.
4. **If end-to-end on `/`-scan is still painful**, go native.
   The remaining JS-side cost is `JSON.parse`-floor; leaving JS
   is the only way down from there.

Optional follow-ups that are cheaper than going native:

- **Drop the temp file.** Pipe the scanner's stdout through the
  URL-scheme handler directly. Saves the disk write/read round
  trip (~200-600 ms) and one filesystem allocation.
- **Streaming ingest** (Lever C). Build the tree incrementally
  as entries stream in. Only worth doing if the "blank screen
  between scan-exit and first treemap" is the next reported
  pain.

Cumulative effect on `/`-scan time-to-first-paint after canvas
landed: ~12 s → ~6-7 s (estimate; real measurement needed).
Steady-state render after a depth change: ~3 s → ~50-100 ms.

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
- `render_full` — full-rebuild render (cold cache or new context).
  Carries `flattenMs`, `gridMs`, `drawMs`, `cellCount`.
- `draw` — canvas draw pass (filter-only redraw). `ms` field
  is -1 because the cost is bounded by `drawn` cell count;
  read `drawn` and `total` instead.
- `redraw_depth_change` — depth toggle on the same view. Both
  directions land here; cost = one `drawCells()` pass.
- `ingest_decode` — bytes-to-doc decode wall time. Carries
  `bytes`, `contentType`, and `encoding` (`json` or `msgpack`).

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
