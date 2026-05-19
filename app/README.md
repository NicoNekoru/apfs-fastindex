# apfs-fastindex native shell

The macOS app. SwiftUI shell, AppKit `TreemapView`, Rust scanner
linked into the process as a static library. No WebKit, no
JavaScript, no JSON file on disk, no subprocess — every call
from Swift into the indexer crosses a cbindgen-generated C ABI.
Chapter 13 of the manual documents the architecture; this README
is the build and code-tour reference for working inside `app/`.

## What's in the build

- **Toolbar.** Target-path field, folder-picker, mode picker
  (Auto / Raw / Fallback), cross-mounts toggle, Scan / Cancel,
  metric picker (Logical / Allocated), depth stepper (in
  settings).
- **Breadcrumb header.** Click any segment to focus that subtree;
  back-arrow undoes one step.
- **Tree-list panel** (left). Largest immediate children of the
  current subtree, sorted by the active metric; click a row to
  drill in. Resizable.
- **Ext-list panel** (right). Per-extension aggregate (entry
  count + total size) computed by a Rust aggregator that walks
  the indexed tree once. Resizable.
- **Treemap.** Squarified layout (Bruls / Huijzen / van Wijk
  2000, in the d3-hierarchy variant), Core Graphics fillRect per
  cell. A 64×64 spatial hash sits over the cell array for
  constant-time hit-tests on mouse-move.
- **Determinate progress island** during a scan: phase label
  (Scanning → Indexing → Rendering), scanned / skipped counts,
  cumulative logical bytes against `volume.used`, m:ss
  stopwatch. Driven by the FFI's
  `apfs_scan_directory_with_progress` callback at 250 ms cadence.
- **Right-click context menu.** Real `NSMenu` → `NSWorkspace`:
  Open, Reveal in Finder, Copy Path, confirm-then-Trash. Inherits
  the OS's permission and recovery semantics.
- **Hover tooltip.** Path + size for the deepest cell under the
  cursor.
- **Initial-view stats card.** Volume name + total / used / free
  when no scan is loaded.
- **Settings scene** (Cmd-,). Depth stepper and worker-thread
  preferences as `@AppStorage` values, persisted across launches.

The depth-limited progressive treemap defers the deepest level
of cells off the first render so the initial paint lands tens of
milliseconds sooner on a whole-machine scan; the deferred cells
fill in on a follow-up tick.

## Build & run

```sh
# From the repo root: build the Rust crate, stage the static
# library + cbindgen-generated header for SwiftPM.
sh build-native.sh

# From app/: build the .app bundle and launch.
cd app
./make-app.sh
open ApfsFastindex.app
```

`build-native.sh` runs `cargo build --release -p apfs-fastindex`
and copies `libapfs_fastindex.a` + `apfs_fastindex.h` into
`Sources/CApfsFastindex/`, where the SwiftPM `systemLibrary`
shim picks them up. `make-app.sh` runs `swift build -c release`,
copies the executable and the SwiftPM resource bundle into
`ApfsFastindex.app/Contents/`, and writes a minimal `Info.plist`.
First launch takes ~30 s while SwiftPM compiles; subsequent
rebuilds are near-instant.

### `swift run` for iteration

`swift run` works (`./.build/debug/ApfsFastindex` launches the
window) but SwiftPM-built binaries aren't `.app` bundles, so
macOS treats them as CLI tools by default. The app forces
`.regular` activation policy in `init` +
`applicationWillFinishLaunching`, which is usually enough — if
the window doesn't focus, click the dock icon or run
`osascript -e 'tell app "apfs-fastindex" to activate'`. For
anything beyond fast iteration, prefer `make-app.sh`.

After Rust changes, re-run `sh build-native.sh` before `swift
build` / `make-app.sh` so the SwiftPM target sees the fresh
static lib and header.

## Layout

```
app/
  Package.swift                          # SwiftPM, macOS 13+
  build-native.sh (in repo root)         # Rust build + stage
  make-app.sh                            # SwiftPM build + bundle
  Sources/
    CApfsFastindex/                      # SwiftPM systemLibrary shim
      module.modulemap                   # link "apfs_fastindex"
      apfs_fastindex.h                   # cbindgen-generated
      libapfs_fastindex.a                # staged by build-native.sh
    ApfsFastindex/
      ApfsFastindexApp.swift             # @main, App scene, Settings
      NativeContentView.swift            # SwiftUI shell + state
      TreemapView.swift                  # NSView, Core Graphics draw
      NativeBridge.swift                 # Swift wrapper over C ABI
      SettingsView.swift                 # ⌘, scene (@AppStorage)
      VizPalette.swift                   # extension → colour family
```

## The FFI boundary

`NativeBridge.swift` is the only file that calls into the C ABI.
Three opaque-handle lifetimes flow across it:

- **`ApfsScan`** — returned by `apfs_scan_directory(_with_progress)`.
  Owns the `FallbackScanOutput` (entries, walk-skips,
  `correctness_claim`, source descriptor), the indexed tree, and
  a lazy path-string cache. Freed by `apfs_scan_free`.
- **`ApfsLayout`** — returned by `apfs_layout_new(scan, subtree,
  depth, metric, dims)`. Owns the laid-out `ApfsCell` array (one
  `#[repr(C)]` record per visible rectangle: x0, y0, x1, y1,
  depth, node_index, flags, fill_rgb) plus the 64×64 hit-grid.
  Freed by `apfs_layout_free`.
- **`ApfsExtSummary`** — returned by `apfs_scan_ext_summary_new`.
  Per-extension aggregate (entry count + total size) computed
  Rust-side and read by the right-hand panel.

Bulk data crosses the boundary as `(*const Item, usize)` pairs
that Swift consumes via `UnsafeBufferPointer<Item>` — no copy,
no per-element call into Rust. The header is regenerated by
cbindgen on every change to `src/ffi.rs` so the C declarations
cannot drift from the Rust `#[no_mangle] extern "C"` surface.

Progress events flow through
`apfs_scan_directory_with_progress`'s callback at the same 250 ms
cadence the CLI's `--progress` stream uses, on a sampling thread
inside the parallel walker. The Swift side hands in a closure
that updates an `@Published ProgressSnapshot` on the main actor;
the determinate progress island follows it.

## The TreemapView render path

`TreemapView` is an `NSView` subclass (`isFlipped = true`) with a
four-phase `draw(_:)`:

1. Fill the background.
2. Draw directory backgrounds (one fillRect per directory cell,
   with a 1 pt `#4a5260` stroke so boundaries read at `/` scale).
3. Draw leaves, grouped by `fill_rgb` so `setFillColor` is called
   once per colour family instead of once per cell.
4. Hover overlay on the currently-tracked cell.

Hit-tests for `mouseMoved` / `mouseDown` / `menu(for:)` go
through `layout.hitTest(point:)`, which the 64×64 spatial hash
turns into a few-cell comparison even on `/`-scan layouts. The
tracking-area is registered on `viewDidMoveToWindow`.

`right-click → menu(for:)` builds an `NSMenu` whose actions
postMessage equivalents are direct `NSWorkspace` calls — no JS
bridge, no marshalled message types. Cmd-Z and Move-to-Trash
inherit the OS's recovery semantics.

## Not in this build

- Snapshot-assisted scanning (R2-B,
  `apfs-fastindex-scan --snapshot <mountpoint>`) is wired on the
  Rust side but the privileged-rerun oracle has not landed (the
  EX-23 probe is `blocked_no_snapshots_at_all` on the test
  host). Until that lifts, snapshot inputs tag the output as
  `not_claimed` rather than shape-matching to a live walk.
- Code signing / notarisation.
