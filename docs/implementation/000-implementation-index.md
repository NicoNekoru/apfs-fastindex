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

## Open Native Slice (Gate A)

With `EX-10`–`EX-18` complete, the Rust crate now decodes
`FsRecordRow` fields for `DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`,
`SIBLING_MAP`, and dstream/sibling_id/inode_name xfields, under SR-015
cursor + SR-016 fail-closed gates, and is field-level identical to the
Python parser on the proof fixture.

Remaining work before the Rust MWP can emit `NamespaceEntry`:

1. EX-19: SR-017 logical-size precedence fixture (ordinary, sparse, clone,
   hard link, symlink, compressed) — pins the rule for non-zero
   `logical_size`.
2. EX-20: SR-018 name/case fixture — APFS hash, normalization,
   case-folding, collision; row enumeration may emit stored UTF-8 names
   verbatim before this lands, but lookup-by-name semantics must wait.
3. Rust MWP: wire `NamespaceEntry` and `DirectoryAggregate` emission with
   the SR-009 unique-inode aggregate policy, gated on EX-19 + EX-20
   passing.

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
