# Implementation Index

Status: Active
Date: 2026-04-26

This directory contains implementation-facing specs only when the corresponding
research direction is resolved tightly enough to describe reproducible behavior
without turning hypotheses into design commitments.

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

## Open Native Slice (Gate A)

With `EX-10`, `EX-11`, and `EX-12` complete, the next native slice is
**FS-record body decoding** under the validated checkpoint/OMAP/root
context. Specifically, in this order:

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
