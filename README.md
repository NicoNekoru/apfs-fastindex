# APFS-FastIndex
A WizTree-like disk visualiser for macOS / APFS. 


https://github.com/user-attachments/assets/3ca0c987-0ea7-4c57-8902-1c3a0008f5f7


APFS indexing backend in Rust, rendered in a Native SwiftUI app frontend using Rust-generated graphics drawn by Core Graphics. Rust backend can also be used as a standalone headless CLI.

## Motivations

The blazingly-fast speed of WizTree's drive indexing relies on the convenience of NTFS metadata, i.e. that NTFS keeps a Master File Table (MFT). The MFT is a single, flat structure in which each file on the drive is stored as a record in the table. As a result, we can sequentially scan this table directly, and don't need to traverse the drive or get stuck recursively searching subdirectories.

APFS does not have an MFT-equivalent. Metadata is spread across three copy-on-write B-trees (the object map, the file-system tree, and the extent tree), keyed by sparse 64-bit object IDs and pinned to a transaction identifier (XID) at the container level. Every "where is this file?" question is a B-tree walk; a whole-disk index is millions of walks. Chapter 1 of the manual develops the consequences.

The macOS POSIX APIs (`readdir`, `lstat`, `getattrlistbulk`) are the highest-level abstraction over the on-disk structure, but they are not built for full-disk scans. The native shell goes one layer further: a Rust parser of the on-disk APFS format itself, with the POSIX fallback as the dependable second oracle when the raw path is not in the allowlist. Chapters 3–7 take the raw path apart; chapter 11 documents the fallback and the support matrix.

The technical work is documented in [`docs/manual/apfs-fastindex-manual.pdf`](docs/manual/apfs-fastindex-manual.pdf) ("Reading APFS"). 

## Reading the manual

The manual is organised as a textbook on APFS, with this project as the worked example. Six parts:

| Part | Chapters | What you learn |
| --- | --- | --- |
| The Problem | 1–2 | Why APFS resists MFT-style indexing; the discipline of evidence and oracles. |
| The Container and the Object Map | 3–4 | How to find the authoritative container superblock and how virtual object IDs resolve to physical addresses. |
| The File-System Tree | 5–7 | How directory records, inodes, and extended fields are stored; the fail-closed cases. |
| Names and Sizes | 8–9 | The size precedence rule (logical, allocated, the failure modes); name preservation across case-insensitive volumes. |
| Boundaries | 10–11 | Identity and incremental caching; the support matrix; the POSIX-traversal fallback. |
| Engineering | 12–13 | Performance measurement, the native renderer, the FFI boundary. |

Two appendices follow: a glossary of every term the manual uses, and an experiment register that lists each controlled probe the manual cites.

## Try it

```sh
# 1. Build the Rust crate + Swift executable + .app bundle.
./make-release.sh

# 2. Launch.
open app/ApfsFastindex.app
```

`make-release.sh` builds the Rust crate, stages the static lib + cbindgen header into the SwiftPM `CApfsFastindex` shim, runs `swift build -c release`, and assembles the `app/ApfsFastindex.app` bundle. The Rust crate links statically into the app binary — no subprocess, no WebKit, no JSON file on disk. See [`app/README.md`](app/README.md) for `PROFILE=debug` and `--no-bundle` options, and chapter 13 of the manual for the architecture.

Inside the app: pick a folder, watch the determinate progress island (phase label, scanned / skipped counts, bytes against the volume's used size, m:ss stopwatch), then read the treemap.  Hover for a tooltip; right-click for a context menu (Open, Reveal in Finder, Copy Path, Move to Trash); use the breadcrumb to focus on a subtree; toggle between Logical and Allocated size with the metric picker. The left panel is the tree-list of largest immediate children; the right panel is the per-extension aggregate. `Cmd-,` opens the settings scene for depth and worker-thread preferences.

## The CLI

For headless use, scripting, or any host without a built `.app`:

```sh
cargo build --release --bin apfs-fastindex-scan

# Fallback path (mounted directory). --slim drops fields the
# standalone viz does not consume; useful for big trees.
./target/release/apfs-fastindex-scan --slim /Applications > scan.json

# Raw path (detached .dmg or caller-pinned /dev/disk*).
./target/release/apfs-fastindex-scan /path/to/source.dmg > scan.json

# One-line correctness claim + the not-claimed register.
./target/release/apfs-fastindex-scan --summary /Applications

# Drop the JSON onto the standalone viz.
open viz/index.html
```

The CLI honours `--threads N` (default `min(hw.physicalcpu, 4)` clamped to `[1, 4]` — the ceiling is tuned to APFS's container-lock contention regime, documented in chapter 12), `--cross-mounts`, `--progress` (stderr event stream at 250 ms cadence), `--format msgpack` / `--format msgpack-stream`, and `--summary`.

## Capabilities

- **Native renderer** (chapter 13). SwiftUI shell, Core Graphics treemap, Rust-laid-out cells with a 64×64 spatial-hash hit-grid for constant-time mouse-move resolution. The Rust crate is linked into the app process; the C ABI is generated by cbindgen.
- **Two size metrics** (chapter 8). Logical (`st_size`) and allocated (`st_blocks * 512`). Allocated is supported for ordinary, clone, hard-link, sparse, decmpfs-compressed (both xattr-stream-stored and resource-fork-stored), symlink, and directory cases — every shape ditto and the macOS write-path produce on a modern APFS volume. EX-26 closed the last two fail-closed branches (sparse: `alloced_size - sparse_bytes`; decmpfs: sum of stream-backed xattr dstreams' allocated bytes).
- **Parallel walker** (chapters 11–12). Per-worker `BulkReader`, shared work queue, sharded `VisitedDirs` mutex (16-way), firmlink-overlay dedup that cuts whole-machine `/` scans from 5.25 M to 3.06 M entries by refusing the `/System/Volumes/Data/*` duplicates.
- **Tuned in-memory layout** (chapter 12). The `TreeNode` is 112 bytes; the `NamespaceEntry` is 72 bytes; per-directory children live in a single contiguous arena; path strings are lazy-computed and cached. Allocator pressure is roughly half what a naïve representation would produce on a `/`-scale scan.
- **Determinate progress through the FFI** (chapter 11).  `apfs_scan_directory_with_progress` carries `(phase, scanned, skipped, bytes, terminal)` events at the same 250 ms cadence as the CLI's `--progress` stream; the SwiftUI shell renders them against the volume's used bytes.

## Measurement snapshot

Numbers below are from chapter 12 of the manual, on Apple silicon, release builds. The manual's tables carry the full shape (target, backend, mode, cache state).

- `/Applications` (163 k entries, warm cache): single-threaded 816 ms (200 k entries / s); four-thread parallel default 523 ms (313 k entries / s, +56 %).
- `/Users/kai/Projects` (320 k entries, warm, in-process FFI path): ~410 ms scan + ~34 ms tree build, ~780 k entries / s steady-state, peak RSS ~190 MiB.
- Whole-machine `/` scan (cold cache, fallback bulk path, pre-dedup): 5.26 M entries in 108.7 s (~48 k entries / s).  Post-firmlink-dedup the entry count is 3.06 M; the time scales with disk I/O.

The walker is resilient: per-entry permission errors and other I/O failures are recorded under `parser_output.walk_skips` with a reason and the walk keeps going. Mount-boundary skipping is the default; pass `--cross-mounts` to descend into mounted volumes.

## Project map

- [`docs/manual/`](docs/manual/) — "Reading APFS", the long-form manual. The PDF is the canonical reference; the chapter `.tex` files live alongside it.
- [`spec.md`](spec.md) — binding contract for the row shape and the fail-closed gates the parser enforces.
- [`crates/apfs-fastindex/`](crates/apfs-fastindex/) — the Rust scanner. Raw and fallback backends, the indexed tree, the squarified-treemap layout, the per-extension aggregator, and the cbindgen-generated C ABI under `src/ffi.rs`.
- [`app/`](app/) — the native macOS app: SwiftUI shell, AppKit `TreemapView` driven by the Rust cell array, settings, context menu, breadcrumb, tree-list / ext-list side panels.
- [`src/apfs_fastindex/`](src/apfs_fastindex/) — Python proof-of-concept, fallback walker, oracle diff, benchmark harness, and the cross-tool smoke check.
- [`viz/`](viz/) — standalone HTML/canvas treemap for the drop-a-JSON-into-the-browser workflow. Independent of the native build.
- [`docs/research/`](docs/research/) — rolling synthesis, source reviews, and the controlled probes the manual's experiment register indexes.
- [`docs/implementation/`](docs/implementation/) — implementation notes, performance studies, and the measurement baseline.

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

The cbindgen header regenerates on every change to `crates/apfs-fastindex/src/ffi.rs`. After Rust changes, re-run `./make-release.sh` to pick them up in the app bundle.

## Future Development
Ultimately, I started working on this project because I missed WizTree, and wanted the efficient disk index/visualization experience for Mac. In its current form, the app is already quite usable for me, and without further impulse my development will stop here. However, I am open to feature requests, and there are still some things that I would like to spend at least a little bit of time on, of which I list a few here. For my other projects, check out [my website](https://kai.ichwan.rocks/).

- Automatic updates
- Better coverage (?)
- A live/watch mode (watch cache visualization during development)
- Filtering/searching
