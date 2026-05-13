# Narrow V1 Proof Parser Skeleton

Status: Implemented proof skeleton
Date: 2026-04-26
Scope: Current `src/apfs_fastindex` proof-backed parser boundary

## Bottom line

The current implementation is a correctness-first proof skeleton for the narrow
v1 contract. It is not yet the native high-performance APFS parser.

What is resolved and reproducible:

- accepted input: detached APFS `.dmg` image or caller-supplied raw APFS
  container device
- scan state: one pinned checkpoint summary from the raw container
- namespace/logical-size output: collected through the existing `EX-03`
  `go-apfs` raw walker backend
- aggregate policy: unique-inode logical totals per directory aggregate root
- validation: regression fixture compares parser output to mounted oracle

What is not resolved:

- wiring the native Rust checkpoint/OMAP/root path into `ParserSkeleton`
- native FS-record body decoding
- native namespace/logical-size row emission
- performance model
- incremental reuse
- live mounted raw scanning
- physical/shared/snapshot accounting
- merged boot-root semantics

Native Rust status as of 2026-04-26:

- `crates/apfs-fastindex` implements the native read-only path through source
  gating, block-zero locator parsing, checkpoint candidate selection,
  checkpoint-map validation, container/volume OMAP resolution, volume
  superblock decoding, FS-tree root validation, and FS-record family dumping.
- The Rust scanner is tracked by `EX-10`, `EX-11`, and `EX-12`, with current
  status summarized in `docs/implementation/rust-checkpoint-scanner.md`.
- It is not yet wired into `ParserSkeleton` and still does not implement
  FS-record body decoding, namespace entry emission, or logical-size output.

This document is intentionally specific to the proof skeleton. Do not use it as
the final native parser spec.

## Evidence

The skeleton is justified by:

- `docs/research/contracts/narrow-v1-parser-contract.md`
- `docs/research/experiments/EX-03-pinned-state-raw-vs-oracle/README.md`
- `docs/research/experiments/EX-04-expanded-pinned-corpus/README.md`
- `docs/research/sources/SR-003-fs-record-taxonomy.md`
- `docs/research/sources/SR-004-runtime-read-path-and-encryption.md`
- `docs/research/sources/SR-005-checkpoint-validation-details.md`
- `docs/research/experiments/EX-06-identity-tracking/README.md`
- `docs/research/experiments/EX-10-rust-checkpoint-scanner/README.md`
- `docs/implementation/rust-checkpoint-scanner.md`

Current regression command:

```sh
PYTHONPATH=src python3 -m apfs_fastindex.poc_regression
```

Observed result on 2026-04-26:

- exit code: `0`
- fixture paths: `7`
- aggregate count: `3`
- oracle diff: matched
- pinned highest checkpoint `xid`: `14`

## Module contract

### `source_gate.py`

Responsibility:

- accept only current allowlist sources
- normalize `/dev/disk*` to `/dev/rdisk*`
- attach `.dmg` inputs with `hdiutil attach -nomount`
- locate the APFS container entity by APFS container content hint
- detach attached images in a `finally` path

Current allowlist:

- `.dmg` image exposing one simple APFS container
- caller-supplied raw device path under `/dev/`

Current hard stops:

- nonexistent path
- non-`.dmg` non-device path
- `.dmg` without a simple APFS container
- unsupported raw device path shape

Implementation detail:

- Keep source gating before any parser work. Do not partially parse unsupported
  sources and then decide to fail.

### `scan_state.py`

Responsibility:

- read block zero only as a locator
- extract block size
- extract checkpoint descriptor base and count
- reject non-contiguous descriptor layouts
- scan descriptor blocks for `NXSB`
- record highest observed checkpoint XID and candidate count

Current limitation:

- It checks `NXSB` magic and XID but does not yet perform full object checksum
  validation or checkpoint-map integrity validation. Native parser work must add
  those before broadening support.

Implementation detail:

- Keep `descriptor_base_non_contiguous` as an explicit field even when rejected.
  This makes fallback reporting and future support expansion reproducible.

### `proof_backend.py`

Responsibility:

- keep the skeleton runnable while native resolver and FS-record modules are
  still stubs
- invoke the `EX-03` `go-apfs` raw walker
- normalize raw walker entries into `NamespaceEntry`

Current limitation:

- The backend does not consume `ScanState` to force historical XID lookup. It
  relies on detached images so the latest state is stable. This is acceptable
  only inside the current proof allowlist.

Implementation detail:

- Treat this backend as a replaceable adapter. No production parser module
  should depend on `go run` shell-out latency, stdout JSON parsing, or `go-apfs`
  internals as a permanent interface.

### `parser.py`

Responsibility:

- combine source gate, scan-state pinning, entry collection, and aggregate
  construction
- return a `ParserOutput` with source, scan state, backend name, entries, and
  aggregates

Implementation detail:

- Keep orchestration thin. Root discovery and FS record walking belong in
  `resolver.py` and `fs_records.py`, not in `parser.py`.

### `aggregate.py`

Responsibility:

- compute canonical v1 directory totals as unique-inode logical size per
  aggregate root

Current policy:

- only `file` entries contribute bytes
- hard-linked files are counted once per aggregate root
- symlinks do not contribute file bytes to directory totals
- sibling totals may be non-additive when a file identity appears in multiple
  subtrees

Implementation detail:

- Use `setdefault(file_id, logical_size)` so duplicate hard-link paths do not
  double-count within one aggregate root.

### `oracle_diff.py`

Responsibility:

- compare parser output to the mounted oracle for the current proof fixture
- normalize expected and actual records to path, kind, file identity, logical
  size, and symlink target

Implementation detail:

- Keep the oracle feature-specific. This oracle is valid for single-volume
  namespace and logical size; it is not valid for physical/shared accounting or
  boot-root semantics.

## Data shapes

### `SourceDescriptor`

Fields:

- `requested_path`
- `raw_container_path`
- `source_kind`
- `allowlist_reason`

### `ScanState`

Fields:

- `block_size`
- `descriptor_blocks`
- `descriptor_base`
- `descriptor_base_non_contiguous`
- `highest_xid`
- `candidate_count`

### `NamespaceEntry`

Fields:

- `path`
- `entry_kind`
- `file_id`
- `logical_size`
- `symlink_target`

### `DirectoryAggregate`

Fields:

- `path`
- `unique_inode_logical_total`
- `contributing_file_ids`

## Current execution path

```text
ParserSkeleton.parse(source)
-> open_validated_source(source)
-> pin_scan_state(raw_container_path)
-> ProofRawWalkBackend.collect_entries(raw_container_path, scan_state)
-> build_directory_aggregates(entries)
-> ParserOutput
```

Regression path:

```text
build_proof_fixture()
-> create APFS image
-> mutate mounted image
-> capture mounted oracle
-> detach image
-> ParserSkeleton.parse(image_path)
-> compare_parser_output_to_oracle(output, oracle_path)
```

## Performance notes

No production performance claim is resolved.

Known costs in the proof skeleton:

- `hdiutil attach -nomount` dominates source setup for image inputs
- `ProofRawWalkBackend` shells out to `go run`, which includes Go build/startup
  overhead unless cached
- JSON serialization/deserialization occurs across the backend boundary
- directory aggregate construction is in-memory and path-parent based

Microoptimization guidance that is safe now:

- Do not optimize the shell-out backend; replace it with native resolver and
  FS-record modules.
- Keep parser output as flat tuples of small immutable records until profiling
  proves a richer object graph is useful.
- Keep aggregate construction separate from raw traversal so future native
  parsing can stream or batch entries without changing product semantics.
- Keep source-gate failures early to avoid wasted raw reads on unsupported
  inputs.
- Keep hard-link de-duplication by integer `file_id` in aggregate construction;
  this is cheap and semantically required.

Microoptimization guidance that is not yet justified:

- persistent node cache
- subtree skipping
- parallel raw reads
- read batching strategy
- OMAP lookup memoization across runs
- physical extent prefetching

Those require `EX-07` and `RL-12` evidence before becoming implementation
requirements. The first `EX-07` run was positive for exact node-identity reuse
in a detached lab corpus, but persistent caching still requires native parser
summaries and `RL-12` profiling before it belongs in an implementation spec.

## Native parser replacement path

Replace `ProofRawWalkBackend` in this order:

1. Implement native root resolver:
   - consume the Rust checkpoint scanner boundary from `EX-10`
   - validate checkpoint superblock checksum/magic/type
   - validate checkpoint-map chain enough to reject malformed state
   - resolve container OMAP
   - resolve selected volume superblock
   - resolve volume OMAP
   - resolve FS root tree
2. Implement native FS-record reader:
   - read `DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`, `SIBLING_MAP`
   - extract dstream or equivalent logical-size fields
   - extract `XATTR_SYMLINK_EA_NAME`
3. Keep the current `ParserOutput`, `NamespaceEntry`, `DirectoryAggregate`, and
   oracle diff contracts unchanged.
4. Re-run the proof fixture and `EX-04` corpus after each replacement stage.

## Exit criteria

The proof skeleton remains valid only while:

- it runs on detached image-backed APFS sources
- `poc_regression` exits `0`
- output matches oracle for path, type, file identity, logical size, and symlink
  target
- no code path claims live raw scanning, native parsing, physical accounting, or
  incremental reuse

Any broadening must be backed by a new `EX-*` artifact and summarized into the
affected `RL-*` logs before this implementation spec is expanded.
