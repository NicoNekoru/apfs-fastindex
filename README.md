# APFS-FastIndex

Attempting to create as fast of a WizTree alternative as possible for MacOS/APFS disk format.

## Motivations

The blazingly-fast speed of WizTree's drive indexing relies on the convenience of NTFS metadata, i.e. that NTFS keeps a Master File Table (MFT). The MFT is a single, flat structure in which each file on the drive is stored as a record in the table. As a result, we can sequentially scan this table directly, and don't need to traverse the drive or get stuck recursively searching subdirectories.

Apple's disk format, APFS, does not have this. Instead of indexing metadata with a flat table, the APFS superblock uses B-trees as object maps, thereby spreading metadata across three different structures: the object map (OMAP), FS tree, and extent tree. Everything in this record is copy-on-write and transactional, relying on sparse object IDs rather than a convenient linear record.

As a result, it is easiest to interface with the APFS drive via filesystem APIs. However, `readdir`, `fstatat`, and even `getattrlistbulk` are not exactly purpose built/ideal for a full drive index. Outside of these abstractions, documentation is extremely poor, and there are no lower level APIs.

This is a project to reverse engineer APFS structures directly in order to get the most optimal APFS indexing as possible.

## Try the v0 demo

R1 — Narrow Rust MWP — is shippable. R2-A landed on top of it: every
`NamespaceEntry` now carries an `allocated_size: Option<u64>` column
under SR-019 + EX-22 precedence, and every `DirectoryAggregate`
carries `unique_inode_allocated_total: Option<u64>` (None whenever a
sparse or decmpfs row in the subtree triggers the fail-closed
contract). Two modes, one CLI, one browser-side treemap with a
**Logical / Allocated metric toggle**:

```sh
# Build the release scanner.
cargo build --release --bin apfs-fastindex-scan

# (a) Scan a directory via the POSIX fallback path. For big trees
#     (~100k+ entries) pass --slim so the JSON fits in-browser:
./target/release/apfs-fastindex-scan --slim /Applications > scan.json

# (b) Or scan a detached APFS .dmg via the raw decoder:
./target/release/apfs-fastindex-scan /path/to/source.dmg > scan.json

# Open the treemap and drop scan.json onto the page.
open viz/index.html
```

The treemap header has a Logical / Allocated toggle. Logical sizes
by `st_size` (always available; SR-017). Allocated sizes by
`st_blocks * 512` for files; symlinks and directories are zero;
sparse and decmpfs rows render muted because their `allocated_size`
is intentionally not claimed (SR-019 / EX-22). The status bar shows
both totals; "allocated: unclaimed" surfaces when the None-collapse
fires anywhere in the loaded subtree.

`apfs-fastindex-scan --summary <path>` prints a one-line correctness
claim and the `not_claimed` register so you can read the semantic mode
at a glance. The register names sparse and decmpfs allocated-size
fail-closed cases explicitly.

Quick measurement reference: see
[`docs/implementation/measurement-baseline.md`](docs/implementation/measurement-baseline.md).
On a recent macOS host the fallback path scans a whole `/` tree
(~5.25M entries) in ~130 s with no sudo. Hot-cache `/Applications`
sustains ~130 000 entries / s.

The walker is resilient: per-entry permission errors and other I/O
failures are recorded under `parser_output.walk_skips` and the walk
keeps going. Mount-boundary skipping is the default; pass
`--cross-mounts` to descend into mounted volumes.

## Project map

- `spec.md`: binding v1 contract.
- `crates/apfs-fastindex/`: native Rust scanner (raw + fallback).
- `src/apfs_fastindex/`: Python proof-of-concept, fallback walker,
  oracle diff, benchmark harness, and the
  `rust_mwp_smoke` cross-tool check.
- `viz/`: drop-in HTML treemap (the web demo).
- `app/`: native macOS shell (SwiftUI + WKWebView wrapper around the
  viz; Phase 1 of the native trajectory). Build with `cd app && swift
  run`.
- `docs/research/`: `RL-*` rolling synthesis, `SR-*` source reviews,
  `EX-*` controlled probes.
- `docs/implementation/`: implementation-facing specs, including the
  measurement baseline.
