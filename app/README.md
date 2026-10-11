# apfs-fastindex native shell

The macOS app. SwiftUI shell, AppKit `TreemapView`, Rust scanner
linked into the process as a static library. No WebKit, no
JavaScript, no JSON file on disk, no subprocess — every call
from Swift into the indexer crosses a cbindgen-generated C ABI.
Chapter 13 of the manual documents the architecture; this README
is the build and code-tour reference for working inside `app/`.

## What's in the build

- **Toolbar.** Target-path field, folder picker, Scan, and
  Logical / Allocated metric picker. Depth and worker count live
  in Settings. Scan as Administrator is in the File menu.
- **Breadcrumb header.** Click any segment to focus that subtree;
  back-arrow undoes one step.
- **Folder tree** (left). Expand folders or click to focus their
  subtree. Files select and reveal their parent in the treemap.
  Rows have the same context menu as treemap cells. Show more
  loads another 400 children or search results.
- **Ext-list panel** (right). Per-extension aggregate (entry
  count + total size) computed by Rust. Click a row to filter
  files within the current subtree and highlight matching cells.
  Click again or press Escape to clear. Chip colours come from
  the same Rust palette as the treemap.
- **Search.** Cmd-F reveals the folder tree and focuses the name
  search. Name-only searches cover the scan. With an extension
  selected, names narrow that category within the current subtree.
  Results sort by the active size metric and support pagination.
- **Treemap.** Squarified layout (Bruls / Huijzen / van Wijk
  2000, in the d3-hierarchy variant), Core Graphics fillRect per
  cell. A 64×64 spatial hash sits over the cell array for
  constant-time hit-tests on mouse-move.
- **Determinate progress island** during a scan: phase label
  (Scanning → Indexing → Rendering), scanned / skipped counts,
  cumulative logical bytes against `volume.used`, m:ss
  stopwatch. Driven by the FFI's
  `apfs_scan_directory_with_progress` callback at 250 ms cadence.
- **Context menus.** Reveal in Finder, Copy Path and confirmed
  Move to Trash share path checks and action handlers. The scan
  root and snapshot contents cannot be trashed. After a successful
  Trash operation, a rescan notice prevents stale sizes from
  appearing current. Restore items through Finder's Trash.
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

Use Rust 1.88 or newer and the macOS Command Line Tools. Distribution
builds need a Rust standard library compatible with macOS 13. If the
linker reports that Rust objects require a newer macOS version, use
a compatible rustup toolchain before publishing.

```sh
# From the repo root: one script does the whole pipeline —
# Rust crate, SwiftPM systemLibrary staging, Swift build, .app
# bundle assembly.
./make-release.sh
open app/ApfsFastindex.app
```

`make-release.sh` runs `cargo build --release -p apfs-fastindex`
(producing `libapfs_fastindex.a` + the cbindgen header), copies
both into `app/Sources/CApfsFastindex/` where the SwiftPM
`systemLibrary` shim picks them up, runs `swift build -c release`
inside `app/`, then assembles `app/ApfsFastindex.app` with a
minimal `Info.plist`. First launch takes ~30 s while SwiftPM
compiles; subsequent rebuilds are near-instant. Use
`PROFILE=debug ./make-release.sh` for a debug build, or
`./make-release.sh --no-bundle` to stop after `swift build`
without assembling the `.app`.

### `swift run` for iteration

`swift run` works (`./.build/debug/ApfsFastindex` launches the
window) but SwiftPM-built binaries aren't `.app` bundles, so
macOS treats them as CLI tools by default. The app forces
`.regular` activation policy in `init` +
`applicationWillFinishLaunching`, which is usually enough — if
the window doesn't focus, click the dock icon or run
`osascript -e 'tell app "apfs-fastindex" to activate'`. For
anything beyond fast iteration, prefer `make-release.sh`.

After Rust changes, re-run `./make-release.sh` so the SwiftPM
target sees the fresh static lib and header.

## Layout

```
app/
  Package.swift                          # SwiftPM, macOS 13+
  Sources/
    CApfsFastindex/                      # SwiftPM systemLibrary shim
      module.modulemap                   # link "apfs_fastindex"
      apfs_fastindex.h                   # cbindgen-generated
      libapfs_fastindex.a                # staged by make-release.sh
    ApfsFastindex/
      ApfsFastindexApp.swift             # @main, App scene, Settings
      NativeContentView.swift            # SwiftUI shell + state
      BrowserPanels.swift                # tree, extension list and search views
      FileActions.swift                  # shared context-menu actions and checks
      TreemapView.swift                  # NSView, Core Graphics draw
      NativeBridge.swift                 # Swift wrapper over C ABI
      SettingsView.swift                 # ⌘, scene (@AppStorage)
      VizPalette.swift                   # typography and app colours
    ApfsCore/
      BrowserModel.swift                 # navigation, pagination and cancellable filters

make-release.sh (in repo root)           # end-to-end build script
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

`menu(for:)` delegates to `FileActions`, also used by SwiftUI's
folder-tree context menu. Restore trashed items in Finder. The
app does not register an Undo action for file operations.

## Interaction tests

```sh
swift run --package-path app apfs-browser-tests
swift run --package-path app apfs-ffi-tests
cargo test -p apfs-fastindex
```

The browser tests exercise navigation, both kinds of pagination,
filter cancellation, metric changes and scan replacement without
AppKit. The FFI suite checks extension classification and colour
parity against a real scan. See `Tests/README.md` for UI checks.

## Not in this build

- Snapshot-assisted scanning (R2-B,
  `apfs-fastindex-scan --snapshot <mountpoint>`) is wired on the
  Rust side but the privileged-rerun oracle has not landed (the
  EX-23 probe is `blocked_no_snapshots_at_all` on the test
  host). Until that lifts, snapshot inputs tag the output as
  `not_claimed` rather than shape-matching to a live walk.
- Code signing / notarisation.
