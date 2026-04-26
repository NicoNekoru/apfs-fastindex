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

## Not Yet Specified

The following topics intentionally remain in research docs until experiments or
benchmarks close their proof gates:

- FS record body decoding (`j_drec_*`, `j_inode_*`, `j_dstream_*`,
  `j_xattr_*`, `j_sibling_*`) in Rust
- name normalization (UTF-8, NFD, case folding) and path reconstruction in
  Rust
- logical-size extraction from `j_dstream_t` and compressed `XATTR` in Rust
- live mounted raw scanning
- subtree reuse and persistent incremental cache
- physical, shared, exclusive, compression, and snapshot-retained accounting
- Finder-visible merged boot-root semantics
- production performance optimizations
