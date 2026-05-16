# apfs-fastindex treemap (v0)

A self-contained HTML page that renders a treemap from any
`apfs-fastindex-scan` JSON output. No network, no upload — the page
parses the file you drop in and draws it locally.

> **Status: temporary demo surface.** This HTML page exists so the
> scanner has a reviewable visualization while we iterate on the
> emission contract and the scan resilience. The product target is a
> **native macOS app** that owns the scan trigger, progress reporting,
> and rendering directly (no JSON shuttle through the filesystem, no
> browser memory ceiling). The viz HTML will be retired once that
> shell lands; treat it as a scaffold, not a UI commitment.

## Quick start

```sh
# 1. Build the release binary if you haven't already.
cargo build --release --bin apfs-fastindex-scan

# 2. Scan something. Either a directory (fallback mode) or a detached
#    APFS .dmg (raw mode). Use --slim for big trees — it drops fields the
#    viz does not consume and gets you a 3-4× smaller JSON:
./target/release/apfs-fastindex-scan --slim /Applications > scan.json
./target/release/apfs-fastindex-scan --slim /Users > scan.json
./target/release/apfs-fastindex-scan /path/to/source.dmg > scan.json

# 3. Open viz/index.html in your browser. Drag scan.json onto the page,
#    or click "Open JSON…".
open viz/index.html
```

### Why --slim

On a 1.3M-entry `/Users` scan, the JSON output is ~546 MB pretty,
~345 MB compact, and ~185 MB with `--slim`. Browser `JSON.parse`
struggles north of ~250 MB, so `--slim` is the right default whenever
the source has more than ~100k entries. The dropped fields (`file_id`,
`aggregates`, null `symlink_target`, `scan_state`) are recomputed or
ignored by the viz.

## What the visual shows

- Every file in the scan is its own rectangle, all rendered at once
  (depth-N layout — the WizTree-style "see the whole disk at a glance"
  view).
- Files are colored by extension. Known categories (text/code, images,
  AV, documents, archives, app/system) have stable palette entries;
  unknown extensions hash to a deterministic HSL color so the same
  extension always looks the same and unfamiliar clusters stay
  distinguishable from each other.
- Directories are rendered as labeled containers around their contents:
  a thin border + a name/size label band at the top. Clicking the
  directory band zooms into that subtree. The breadcrumb at the top
  navigates back; **Reset zoom** returns to the root.
- Hover any rectangle for full path, kind, and **both size metrics**
  (logical and allocated, when the scan carries the SR-019 column).
  Symlinks also show their target.
- The header has a **Logical / Allocated metric toggle** that rebalances
  the treemap between the two columns. Allocated is sized by
  `st_blocks * 512` for files (the EX-22 oracle), zero for symlinks and
  directories, and **muted** for sparse / decmpfs rows whose
  `allocated_size` is intentionally not claimed by SR-019. The
  Allocated toggle stays disabled on pre-R2-A scans that don't carry
  the column.
- The status bar shows both totals at the root; "allocated:
  unclaimed" surfaces when any sparse or decmpfs row in the loaded
  subtree triggers the SR-019 None-collapse contract.
- The header shows the scanner's `correctness_claim` so you can tell at
  a glance which semantic mode produced the data (raw APFS vs POSIX
  fallback).

### Performance notes

The viz uses SVG. To keep the page interactive on 100k+ entry scans the
renderer skips any node whose computed pixel area is below ~4 px², so
slivers smaller than your screen can resolve don't bog down the
browser. Zooming into a directory re-lays out only that subtree, which
brings back the detail for entries that were too small at the parent
zoom level. Truly massive scans (the multi-million-entry end) will
eventually need a Canvas or Metal renderer in the native app; the SVG
version remains the demo surface.

## Known limits (v0)

- Logical and allocated bytes only. The Allocated metric is
  per-stream `j_dstream_t.alloced_size` (raw mode) or
  `st_blocks * 512` (fallback mode), neither of which is
  clone-aware: two clones of the same content each count their
  shared extents in full, so the sum over a clone family
  over-counts on disk. Exclusive / shared / snapshot-retained
  bytes remain out of v1 — see `spec.md` and SR-019.
- Encrypted, live-boot, snapshot-assisted, and boot-root merged scans
  are not supported by the scanner itself — see the project root
  `spec.md` for the support matrix.
- D3.js is now vendored under `viz/vendor/d3.v7.min.js`; the demo runs
  with no network access.
- No persistence: reopening the page re-asks for the JSON.
- Visual depth is currently one level at a time (click to drill in,
  breadcrumb to navigate back); no in-place dir-vs-file disambiguation
  beyond hover/click. That polish is on the next-chunk list.

## Skipped paths

When the scanner can't enter a subtree it records a `walk_skip` instead
of aborting. The viz shows a `N skipped` pill in the lower-right
corner; click it to see the list with `(reason, path)` pairs. Common
reasons:

- `permission_denied` — current user can't `read_dir` or `lstat` the
  subdirectory.
- `not_found` — file existed during `read_dir` but disappeared before
  `lstat` (mid-scan race).
- `mount_boundary` — child directory is on a different device than
  the scan root. Re-run with `apfs-fastindex-scan --cross-mounts …`
  to descend into mounted volumes.
- `non_utf8_name` — entry name is not valid UTF-8 (v1 namespace
  contract requires UTF-8).
- `read_error:…` — any other `io::Error` (ENOMEM, EIO, …) reported
  per directory.
