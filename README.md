# APFS-FastIndex

A WizTree-class disk visualiser for macOS / APFS, with the
correctness discipline that file-system tools usually skip. Two
surfaces: a native SwiftUI app whose treemap is laid out by Rust
and drawn by Core Graphics, and a headless CLI that emits the
same row shape to stdout.

The work is documented in
[`docs/manual/apfs-fastindex-manual.pdf`](docs/manual/apfs-fastindex-manual.pdf)
("Reading APFS"). Chapter 12 has the measurement tables;
chapter 13 has the architecture; the appendix carries the
experiment register the manual cites.

## Motivations

WizTree's speed on Windows comes from one structural fact: NTFS
keeps a Master File Table, a single flat structure carrying one
record per file on the volume. A whole-disk index is a
sequential scan of the table — no directory traversal, no
recursive subtree work.

APFS does not have an MFT-equivalent. Metadata is spread across
three copy-on-write B-trees (the object map, the file-system
tree, and the extent tree), keyed by sparse 64-bit object IDs
and pinned to a transaction identifier (XID) at the container
level. Every "where is this file?" question is a B-tree walk; a
whole-disk index is millions of walks.

The macOS POSIX APIs (`readdir`, `lstat`, `getattrlistbulk`)
are the highest-level abstraction over this, but they are not
built for full-disk scans. `getattrlistbulk` in particular has
a number of kernel-side cost cliffs the project has measured
and worked around. The native shell goes one layer further: a
Rust parser of the on-disk APFS format itself, with the
fallback walker as the dependable second oracle when the raw
path is not in the allowlist.

The result is roughly two-times the speed of WizTree-equivalent
naïve directory walks on the same host, with a fail-closed
correctness contract instead of a best-effort one.

## Try it

The native app is the primary surface.

```sh
# 1. Build the Rust crate + Swift executable + .app bundle.
sh build-native.sh
cd app && ./make-app.sh

# 2. Launch.
open ApfsFastindex.app
```

`build-native.sh` produces `libapfs_fastindex.a` and the
cbindgen-generated C header and stages both for SwiftPM;
`make-app.sh` runs `swift build -c release`, copies the
executable + resource bundle into `ApfsFastindex.app`, and
writes the `Info.plist`. The Rust crate links statically into
the app binary — no subprocess, no WebKit, no JSON file on
disk.

Inside the app: pick a folder, watch the determinate progress
island (phase label, scanned / skipped counts, bytes against
the volume's used size, m:ss stopwatch), then read the
treemap. Hover for a tooltip; right-click for a context menu
(Open, Reveal in Finder, Copy Path, Move to Trash); use the
breadcrumb to focus on a subtree; toggle between Logical and
Allocated size with the metric picker. The left panel is the
tree-list of largest immediate children; the right panel is
the per-extension aggregate. `Cmd-,` opens the settings scene
for depth and worker-thread preferences.

## The CLI

For headless use, scripting, or any host without a built `.app`:

```sh
cargo build --release --bin apfs-fastindex-scan

# Fallback path (mounted directory). --slim drops fields the
# standalone viz does not consume; useful for big trees.
./target/release/apfs-fastindex-scan --slim /Applications > scan.json

# Raw path (detached .dmg or caller-pinned /dev/disk*).
./target/release/apfs-fastindex-scan /path/to/source.dmg > scan.json

# One-line correctness claim + the not_claimed register.
./target/release/apfs-fastindex-scan --summary /Applications

# Drop the JSON onto the standalone viz.
open viz/index.html
```

The CLI honours `--threads N` (default `min(hw.physicalcpu, 4)`
clamped to `[1, 4]` — the ceiling is tuned to APFS's
container-lock contention regime, documented in chapter 12 of
the manual), `--cross-mounts`, `--progress` (stderr event
stream at 250 ms cadence), `--format msgpack` /
`--format msgpack-stream`, and `--summary`.

## What landed

- **Native renderer.** SwiftUI shell, Core Graphics treemap,
  Rust-laid-out cells with a 64×64 spatial-hash hit-grid for
  constant-time mouse-move resolution. Manual chapter 13.
- **R2-A allocated size.** Every `NamespaceEntry` carries
  `allocated_size: Option<u64>` and every `DirectoryAggregate`
  carries `unique_inode_allocated_total: Option<u64>` under the
  SR-019 precedence rule. Fail-closed for sparse and decmpfs
  files (no public oracle). Manual chapter 8.
- **Parallel walker.** Per-worker `BulkReader`, shared work
  queue, sharded `VisitedDirs` mutex (16-way), firmlink-overlay
  dedup that cuts whole-machine `/` scans from 5.25 M to 3.06 M
  entries by refusing the `/System/Volumes/Data/*` duplicates.
  Manual chapter 11.
- **Structural-density pass.** Eight rounds of post-FFI
  optimisation against `Box<str>` for write-once fields,
  fxhash on non-adversarial keys, lazy path computation, and
  the CLI-only aggregates Vec. Net −32% scan wall, −50%
  tree-build wall, −28% peak RSS against the FFI workload.
  Manual chapter 12 § Structural-Density Pass.
- **Determinate progress through the FFI.**
  `apfs_scan_directory_with_progress` carries
  `(phase, scanned, skipped, bytes, terminal)` events at the
  same 250 ms cadence as the CLI's `--progress` stream; the
  SwiftUI shell renders them against `volume.used`.

## Measurement snapshot

Numbers below are from chapter 12 of the manual, on Apple
silicon, release builds. The manual's tables carry the full
shape (target, backend, mode, cache state).

- `/Applications` (163 k entries, warm cache), end-to-end:
  T=1 single-threaded post-micro-opt 816 ms (200 k entries / s);
  T=4 parallel default 523 ms (313 k entries / s, +56%).
- `/Users/kai/Projects` (320 k entries, warm, FFI path,
  post-structural-density-pass): ~410 ms scan + ~34 ms tree
  build (~780 k entries / s steady-state, peak RSS ~190 MiB).
- Whole-machine `/` scan (cold cache, fallback bulk path,
  pre-dedup): 5.26 M entries in 108.7 s (~48 k entries / s).
  Post-firmlink-dedup the entry count is 3.06 M; the time
  scales with disk I/O, not with the structural pass.

The walker is resilient: per-entry permission errors and other
I/O failures are recorded under `parser_output.walk_skips`
with a reason and the walk keeps going. Mount-boundary
skipping is the default; pass `--cross-mounts` to descend into
mounted volumes.

## Project map

- [`docs/manual/`](docs/manual/) — "Reading APFS", the long-form
  manual. The PDF is the canonical reference; the chapter `.tex`
  files live alongside it.
- [`spec.md`](spec.md) — binding v1 contract for the row shape
  and the fail-closed gates.
- [`crates/apfs-fastindex/`](crates/apfs-fastindex/) — the Rust
  scanner. Raw and fallback backends, the indexed tree, the
  squarified-treemap layout, the per-extension aggregator, and
  the cbindgen-generated C ABI under `src/ffi.rs`.
- [`app/`](app/) — the native macOS app: SwiftUI shell, AppKit
  `TreemapView` driven by the Rust cell array, settings,
  context menu, breadcrumb, tree-list / ext-list side panels.
- [`src/apfs_fastindex/`](src/apfs_fastindex/) — Python
  proof-of-concept, fallback walker, oracle diff, benchmark
  harness, and the `rust_mwp_smoke` cross-tool check.
- [`viz/`](viz/) — standalone HTML/canvas treemap for the
  drop-a-JSON-into-the-browser workflow. Independent of the
  native build.
- [`docs/research/`](docs/research/) — `RL-*` rolling
  synthesis, `SR-*` source reviews, `EX-*` controlled probes
  (the manual's appendix indexes these).
- [`docs/implementation/`](docs/implementation/) — implementation
  notes, performance studies, the measurement baseline.

## Development

```sh
# Build + test the Rust crate.
cargo test -p apfs-fastindex

# Quick perf probe — wall time + peak RSS per phase. Honours
# APFS_PHASE_TIMINGS=1 for per-phase walk / sort / aggregates
# breakdown, and APFS_BULK_BUF_KIB to ablate the bulk-syscall
# buffer size.
cargo run --release -p apfs-fastindex --example perf_probe -- /Users/me/Projects 4

# Struct sizes for the on-disk and in-memory shapes.
cargo run --release -p apfs-fastindex --example sizes_probe
```

The cbindgen header regenerates on every change to
`crates/apfs-fastindex/src/ffi.rs`. After Rust changes,
re-run `sh build-native.sh && cd app && ./make-app.sh` to pick
them up in the app bundle.
