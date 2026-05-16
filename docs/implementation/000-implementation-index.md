# Implementation Index

Status: Active
Date: 2026-04-26

This directory contains implementation-facing specs only when the corresponding
research direction is resolved tightly enough to describe reproducible behavior
without turning hypotheses into design commitments.

The broader product roadmap lives in
`../research/plans/general-wiztree-for-any-mac-roadmap.md`. It is intentionally
not an implementation spec; use it to choose the next research or implementation
gate, then promote only resolved slices into this directory.

## Specs

- `narrow-v1-proof-parser-skeleton.md`: current `src/apfs_fastindex`
  proof-backed parser skeleton. This documents the resolved runnable boundary,
  validation command, module contracts, current limitations, and replacement
  path toward native APFS parsing.
- `rust-checkpoint-scanner.md`: native Rust read-only path in
  `crates/apfs-fastindex`. It now covers source gating, descriptor scanning,
  selected NX superblock decoding, checkpoint-map validation, container/volume
  OMAP `(oid, max_xid)` resolution, volume superblock decoding under the v1
  feature allowlist, and a read-only FS-tree record-family dump. It does
  **not** decode FS record bodies, normalize names, emit `NamespaceEntry`
  rows, or compute logical size.
- `../research/experiments/EX-10-rust-checkpoint-scanner/README.md`: research
  log for the native Rust implementation slice with synthetic, proof-fixture,
  and FS-record-dump oracles.
- `../research/experiments/EX-11-checkpoint-map-integrity/README.md`: research
  log proving native checkpoint-map context validation on a generated proof
  fixture and synthetic malformed cases.
- `../research/experiments/EX-12-omap-lookup-contract/README.md`: research
  log proving native `(omap domain, oid, selected_xid)` lower-bound lookup,
  obj-header replay at returned paddrs, SR-006 hard stops, and cross-tool
  oracle pairing with `go-apfs identitydump`. Verdict on the proof fixture:
  `validated_omap_lookup_contract`.
- `../research/experiments/EX-15-block-1031-context-replay/README.md`:
  research log proving the EX-14 `checksum mismatch at block 1031` was a Rust
  FS-tree traversal bug (internal-node values are virtual OIDs, not paddrs)
  and recording the patch + two synthetic regression tests in
  `crates/apfs-fastindex/src/fs_records.rs`.
- `../research/experiments/EX-16-sr-015-xfield-replay/README.md`:
  research log proving the SR-015 single-cursor xfield rule on the
  EX-13 proof fixture (14/14 records pass
  `xf_used_data == sum(round_up(x_size, 8))`).
- `../research/experiments/EX-17-synthetic-fail-closed-bodies/README.md`:
  research log enumerating the 21 per-record SR-016 fail-closed unit
  tests landed in `crates/apfs-fastindex/src/fs_record_body.rs::tests`.
- `../research/experiments/EX-18-rust-body-field-dump/README.md`:
  research log proving the Rust body decoder is field-identical to the
  Python EX-13 + EX-16 parser on the proof fixture
  (53/53 records, 0 divergent fields).
- `../research/experiments/EX-19-sr-017-logical-size-precedence/README.md`:
  research log validating the SR-017 per-inode logical-size precedence
  on a same-run fixture covering ordinary, sparse, clone, hard link,
  symlink, and `ditto --hfsCompression`.
- `../research/experiments/EX-20-sr-018-name-case-fixture/README.md`:
  research log validating SR-018 row enumeration: Rust paths from
  `FsRecordDump.records` match mounted POSIX byte-for-byte on both
  case-insensitive and case-sensitive APFS volumes; volume
  case/normalization flags propagate correctly. Lookup-by-name still
  unclaimed.
- `../research/experiments/EX-22-sr-019-alloced-size-precedence/README.md`:
  research log running SR-019's per-file allocated-size precedence
  against the EX-19 same-run fixture. Verdict
  `partial_validated_sr_019_alloced_size`: ordinary, clone, symlink,
  and `dir` rows match `st_blocks * 512` exactly; the decmpfs row
  fails closed by design; the sparse row diverges by exactly
  `INO_EXT_TYPE_SPARSE_BYTES`. The Rust slice ships with the
  amended precedence (sparse → `None`, decmpfs → `None`,
  regular+dstream+no-SPARSE_BYTES → `Some(alloced_size)`,
  symlink/dir → `Some(0)`) and lists both sparse and decmpfs in
  `not_claimed`. The `alloced_size - sparse_bytes` algebraic
  identity is recorded as a Hypothesis for an EX-22b sparse-corpus
  probe before any sparse rows are promoted to `Some(_)`.
- `../research/experiments/EX-21-fallback-path-skeleton/README.md`:
  research log proving that the POSIX-traversal fallback emits the same
  `NamespaceEntry`/`DirectoryAggregate` shape as Rust raw mode on the
  proof fixture. Implementations live in
  `src/apfs_fastindex/fallback_traversal.py` (Python) and
  `crates/apfs-fastindex/src/fallback.rs` (Rust, used by the
  `apfs-fastindex-scan` CLI when the source is a directory).
- `../research/experiments/EX-23-snapshot-shape-parity/README.md`:
  research log for R2-B's best-effort snapshot shape-parity
  probe (never sudo per SR-020). First-run verdict
  `blocked_no_snapshots_at_all`: the only snapshot on the host
  is the sealed-system OS-update one, which SR-020 excludes.
  The R2-B Rust integration (`--snapshot <mountpoint>` flag on
  `apfs-fastindex-scan`) is **not** landed yet; the lane stays
  in `not_claimed` until a privileged rerun produces
  `validated_snapshot_shape_parity`. SR-020 documents the
  entitlement gate (mutating snapshot calls need root + the
  DTS-issued private entitlement `com.apple.developer.vfs.snapshot`).
- `measurement-baseline.md`: first reproducible measurement
  (entries/sec, wall time, CPU breakdown) for raw and fallback modes
  on three reference targets. Standing baseline for any future
  performance claim. Reproducer:
  `PYTHONPATH=src python3 -m apfs_fastindex.bench [...]`.

## Open Native Slice (Gate A)

With `EX-10`–`EX-18` complete, the Rust crate now decodes
`FsRecordRow` fields for `DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`,
`SIBLING_MAP`, and dstream/sibling_id/inode_name xfields, under SR-015
cursor + SR-016 fail-closed gates, and is field-level identical to the
Python parser on the proof fixture.

All four MWP gates have landed:

1. EX-18: Rust body-field dump field-level parity with Python EX-13/EX-16
   (53/53 records, 0 divergent fields).
2. EX-19: SR-017 logical-size precedence (5/5 inodes match `st_size`
   across ordinary, sparse, clone, hard link, symlink, compressed).
3. EX-20: SR-018 row enumeration (paths match POSIX byte-for-byte on
   both CI and CS volumes).
4. Rust MWP promote: `NamespaceEntry` + `DirectoryAggregate` emission
   with the SR-009 unique-inode aggregate policy. The smoke test at
   `src/apfs_fastindex/rust_mwp_smoke.py` rebuilds the proof fixture,
   runs the Rust scanner, deserializes `parser_output` into the Python
   `ParserOutput` model, and asserts `compare_parser_output_to_oracle`
   matches plus that `build_directory_aggregates` rebuilt from Rust's
   entries equals Rust's aggregates. First-run verdict: matched.
   The Rust CLI `--summary` mode prints the one-line correctness_claim
   plus the `not_claimed` register.

R1 (Narrow Rust MWP) is complete. **R2-A (allocated_size column)
also landed.** The Rust scanner now emits
`allocated_size: Option<u64>` on `NamespaceEntry` and
`unique_inode_allocated_total: Option<u64>` on `DirectoryAggregate`
under the EX-22-amended SR-019 precedence
(regular+dstream+no-sparse → `Some(alloced_size)`; sparse / decmpfs
→ `None`; symlink/dir → `Some(0)`; `--summary` lists both
fail-closed cases in `not_claimed`). The fallback path emits
`Some(st_blocks * 512)` for files (the EX-22 oracle directly) via
`ATTR_FILE_ALLOCSIZE` in the bulk backend and
`Metadata::blocks() * 512` in the std-read_dir fallback. Test count
delta: 55 → 68 (three aggregate-rule tests for the None-collapse
policy + extended fallback / bulk assertions). The
`rust_mwp_smoke` smoke now also checks the SR-019 column against
the proof fixture's known sparse-file row and aggregate
None-collapse pattern. EX-21 also lands the fallback-path
skeleton in both Python (`src/apfs_fastindex/fallback_traversal.py`)
and Rust (`crates/apfs-fastindex/src/fallback.rs`); both emit the same
`NamespaceEntry` + `DirectoryAggregate` shape via POSIX traversal when
raw mode is rejected. Rust raw vs Rust fallback matches on the proof
fixture (same 7 entries, same 3 aggregates) so the demo CLI can
auto-dispatch: `apfs-fastindex-scan <path>` picks raw mode for
`.dmg` / `/dev/...` sources and fallback mode for directories
(override via `--mode raw|fallback|auto`).

The fallback walker is resilient: per-entry `EACCES` / `EPERM` /
`ENOENT` are recorded in `parser_output.walk_skips` with a reason and
the walk continues. Whole-machine `/` scans run end-to-end under a
normal user (~5.25M entries, ~130 s, ~830 skipped paths on the
reference host). Mount-boundary skipping is the default (cross-device
children are recorded but not descended); `--cross-mounts` opts in to
the older behavior. The fallback backend defaults to macOS `getattrlistbulk`
(`crates/apfs-fastindex/src/fallback_bulk.rs`) and falls back to
`std::fs::read_dir` + `symlink_metadata` whenever the bulk syscall is
unavailable or errors. The bulk path cuts whole-machine `/`-scan wall
time by ~16% and system CPU by ~38% (see
`measurement-baseline.md`). The `--progress` CLI flag streams one
JSON line per second to stderr describing scan progress for any
wrapper (native app, terminal).

Gate 2+ (live volumes, encryption, boot-root, incremental cache)
requires fresh oracles and is out of scope until separately approved.

**Product trajectory note:** `viz/index.html` is the temporary demo
surface. The shipping product is a native macOS app that owns the
scan trigger, progress feedback, and rendering itself. The web viz
exists to make the JSON contract reviewable and to give us a
treemap to look at while the scanner contract is still moving;
expect it to be retired once a native shell lands. The JSON shape
the scanner emits is the durable interface.

**Native shell — Phase 1 landed.** `app/` contains a SwiftPM macOS
target (SwiftUI + WKWebView). It owns:

- the target picker, mode/cross-mount toggles, and Scan / Cancel
  controls (toolbar);
- the `apfs-fastindex-scan` subprocess (`ScanController` streams the
  stderr progress JSON into a live status bar and hands the stdout
  scan JSON to the viz);
- a typed JS↔Swift bridge (`BridgeProtocol.swift`) that Phase 2 will
  use to surface a right-click context menu (Reveal in Finder, Move
  to Trash, Copy Path); Phase 3 the top-N sidebar + path search;
  Phase 4 the bundled scanner binary + signing/notarization story.

The bundled viz inside `app/Sources/ApfsFastindex/Resources/viz/`
mirrors the canonical `viz/` directory at the repo root; both must
stay in sync until Phase 4 makes the copy a build-time step.

The earlier body-decoding slice has now landed; the steps below are
preserved for traceability.

1. `j_drec_*_key_t` and `j_drec_val_t` (`DIR_REC`) - reconstruct the
   directory-entry table with name bytes, normalization, and child file
   IDs, but do not yet emit `NamespaceEntry` rows.
2. `j_inode_val_t` flags, link count, parent ID, and embedded fields -
   pick the file/dir/symlink kind and produce the inputs required by
   `SR-009`'s logical-size precedence rule.
3. `j_dstream_id_val_t` plus `j_dstream_t` plus the symlink-target
   `XATTR` - read logical size and symlink targets for `RL-06` and
   `RL-07`.
4. `j_sibling_link_val_t` and `j_sibling_map_val_t` - hard-link
   unification for the visualizer-facing `unique-inode directory
   aggregate`.
5. Wire the resolved namespace into the existing
   `ParserOutput`/`NamespaceEntry`/`DirectoryAggregate`/oracle-diff
   contract so the Rust path can be diffed against the Python proof and
   `go-apfs identitydump` oracles.

Each step lands as its own `EX-*` with a paired raw-image plus oracle in
the same execution (the EX-12 pattern), and existing Python proof
regressions must keep passing throughout.

## Not Yet Specified

The following topics intentionally remain in research docs until experiments or
benchmarks close their proof gates:

- name normalization (UTF-8, NFD, case folding) and path reconstruction in
  Rust beyond raw `j_drec_*` decoding
- logical-size extraction from compressed `XATTR` in Rust (uncompressed
  `j_dstream_t` falls under the open slice above)
- live mounted raw scanning
- subtree reuse and persistent incremental cache
- physical, shared, exclusive, compression, and snapshot-retained accounting
- Finder-visible merged boot-root semantics
- production performance optimizations
- visualizer integration beyond the oracle-backed `NamespaceEntry`/
  `DirectoryAggregate` contract
