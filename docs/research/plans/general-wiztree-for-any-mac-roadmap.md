# General WizTree-for-any-Mac Roadmap

Status: Active planning
Date: 2026-05-13
Scope: Long-range product roadmap from narrow validated parser to broad
macOS/APFS indexing product
Related docs:
- `spec.md`
- `docs/research/000-research-index.md`
- `docs/research/contracts/narrow-v1-parser-contract.md`
- `docs/implementation/000-implementation-index.md`
- `docs/implementation/rust-checkpoint-scanner.md`
- `docs/research/experiments/EX-14-xfield-layout-variant/README.md`

## Bottom Line

The path to a general WizTree-for-any-Mac product is not one raw parser that
blindly runs everywhere. It is a hybrid indexed scanner with explicit semantic
modes:

- a validated raw fast path where APFS state can be pinned and parsed safely,
- a supported-API fallback path for common live systems outside the raw
  allowlist,
- a Finder-visible boot-root mode only after System/Data, snapshot, and firmlink
  semantics have their own oracle,
- persistent incremental reuse only after native full-scan correctness and
  native subtree-reuse proofs exist,
- metric-specific size modes whose labels and formulas are proven separately.

The immediate implementation blocker is not broad product design. It is the
`EX-14` context failure:

```text
APFS object validation failed: checksum mismatch at block 1031
```

Until that checkpoint/OMAP-context blocker is explained, the project should not
move Rust FS-record body decoding or product namespace rows forward.

## Product Definition

A general product means:

- a normal macOS user can point the tool at a disk, volume, folder, or startup
  namespace and get a correct, understandable index;
- the product chooses the fastest safe backend for that source;
- the UI/API labels exactly what semantic mode and size metric are being shown;
- unsupported raw states fall back instead of producing plausible but unproven
  results;
- repeat scans become fast only where cache continuity has been proven.

It does not mean:

- raw APFS parsing on every source;
- bypassing FileVault or hardware-backed encryption;
- claiming Finder-visible `/` semantics from a raw single-volume parser;
- reporting physical/shared/exclusive bytes from logical-size evidence;
- reusing cached subtree summaries without a fresh full-parse oracle.

## Current State

### Resolved Enough To Build On

- Narrow v1 contract: one stable APFS volume, one coherent selected state,
  namespace plus logical size, fail closed outside the tested allowlist.
- Rust source gate: detached `.dmg` and caller-supplied raw APFS container
  devices.
- Rust checkpoint path: block-zero locator, descriptor scan, highest valid
  checkpoint candidate, selected `NXSB` decode.
- Rust checkpoint-map validation and container OMAP lookup.
- Rust volume superblock decode, volume support gating, volume OMAP lookup, and
  FS-tree root validation.
- Rust FS-tree traversal to record-family counts.
- Python proof path for namespace/logical-size parity on controlled fixtures.
- Source reviews for xfield layout, record-body fail-closed policy, logical-size
  precedence, and name/case behavior.

### Current Blockers

1. `EX-14` checksum mismatch at block `1031`.
   - Blocks the second xfield-layout fixture variant.
   - Must determine whether this is stale checkpoint selection, missing
     checkpoint-map/data-ring handling, an object validation bug, or a malformed
     source that should hard-stop.
2. Source-backed xfield replay is not yet executed.
   - `SR-015` gives one deterministic xfield cursor rule, but the replay
     artifact with `xf_used_data` validation still has to pass.
3. Rust FS-record body decoding is not implemented.
   - Body decoding must start as a field dump, not product rows.
4. Compression logical-size precedence still needs a same-run fixture.
   - Ordinary, sparse, clone, hard-link, and symlink logical size have evidence;
     compressed logical size remains gated.
5. Live startup, encrypted, snapshot, and boot-root modes remain outside raw v1.

## Roadmap Gates

### Gate 0: QA Discipline

Purpose:
Keep the project from turning research uncertainty into implementation debt.

Required state:

- Every external claim lands in an `SR-*` note.
- Every empirical claim lands in an `EX-*` note with artifacts.
- Every durable conclusion is summarized back into affected `RL-*` logs.
- Every implementation promotion names the exact oracle it passed.
- Negative, blocked, and inconclusive results remain in the record.

Exit criteria:

- `cargo fmt --check`
- `cargo clippy -p apfs-fastindex --all-targets -- -D warnings`
- `cargo test -p apfs-fastindex`
- relevant probe or regression command
- updated roadmap/index/spec entries when the product boundary changes

### Gate 1: Native Narrow Rust Full Scan

Product mode:
`raw_single_volume_logical_size`

Target:
For detached or explicitly stable unencrypted APFS sources, the Rust CLI emits
namespace entries and logical-size directory aggregates that match the mounted
or POSIX oracle for the same selected state.

Work items:

1. Resolve the `EX-14` block `1031` context failure.
2. Replay `EX-13` with the `SR-015` xfield cursor rule and `xf_used_data`
   validation.
3. Add synthetic negative record-body cases from `SR-016`.
4. Implement Rust FS-record body field dumps:
   - `DIR_REC`
   - `INODE`
   - `XATTR`
   - `SIBLING_LINK`
   - `SIBLING_MAP`
   - dstream fields
5. Diff Rust field dumps against Python raw-body artifacts and POSIX facts.
6. Implement namespace assembly:
   - stored names preserved verbatim,
   - directory placement from `DIR_REC`,
   - file identity from inode IDs,
   - hard-link path identity separated from file identity,
   - symlink target from `com.apple.fs.symlink`.
7. Implement logical-size extraction for ordinary, sparse, cloned, hard-linked,
   and symlink rows.
8. Compute unique-inode logical directory aggregates.
9. Wire native output into `ParserOutput` and CLI JSON.

Exit criteria:

- Rust output matches the proof fixture oracle.
- Rust output matches the expanded pinned corpus where the source is supported.
- Unsupported record bodies produce typed hard stops.
- `not_claimed` still lists compression edge cases, live sources, physical
  accounting, boot-root semantics, encryption, and incremental reuse.

### Gate 2: Local Single-Volume Product Backend

Product mode:
Validated local APFS volume scan with raw fast path and safe fallback.

Target:
Convert the native full scan into a usable backend for simple local volumes
without pretending to support every Mac.

Work items:

1. Define the runtime source matrix as machine-readable data:
   - detached image,
   - external unencrypted APFS,
   - mounted image,
   - live non-startup APFS,
   - startup container,
   - encrypted volume,
   - sealed/snapshot volume,
   - multi-device/Fusion class.
2. For each matrix cell, record:
   - readable,
   - checkpoint-scanner-safe,
   - checkpoint-context-safe,
   - OMAP-root-safe,
   - namespace-logical-size-safe,
   - product-supported.
3. Add backend selection:
   - raw fast path when supported,
   - POSIX traversal fallback when raw is unsupported,
   - bulk attribute fallback only after measured correctness parity.
4. Add diagnostic output that explains why raw mode was rejected.
5. Add corpus fixtures for case-sensitive, case-insensitive, sparse, clone,
   hard link, symlink, xattr, Unicode, and malformed-body cases.

Exit criteria:

- A user-facing scan never silently downgrades correctness.
- Every raw rejection has a reason.
- Fallback output matches the same namespace/logical-size contract.
- The support matrix has at least one real external non-startup APFS cell.

### Gate 3: Finder-Visible macOS Namespace Mode

Product mode:
`os_visible_namespace`

Target:
Index what the user expects from normal macOS paths, especially on startup
disks, without conflating this with raw single-volume parsing.

Work items:

1. Define the semantic target:
   - raw volume view,
   - mounted volume view,
   - Finder-visible startup root,
   - selected snapshot view.
2. Build a boot-root oracle:
   - `diskutil apfs list`,
   - mounted volume roles,
   - volume-group UUIDs,
   - current snapshot identity,
   - firmlink table facts,
   - POSIX/API traversal of user-visible paths.
3. Compare raw System and Data volume outputs against the user-visible tree.
4. Decide whether this mode is API-only, hybrid raw-plus-API, or raw-capable for
   a small allowlist.
5. Preserve mode labels in output so raw-volume totals and Finder-visible totals
   are never mixed.

Exit criteria:

- A boot-root experiment explains every expected mismatch between raw volume
  rows and user-visible paths.
- Firmlink and System/Data joins are backed by an oracle.
- Startup root support has a fallback path even if raw parsing is rejected.

### Gate 4: Encryption And Live Runtime Support

Product mode:
Common live Mac scanning.

Target:
Handle FileVault and live mounted volumes through supported paths first, and raw
paths only where runtime access and state pinning are proven.

Work items:

1. Expand `EX-08` read-path matrix:
   - FileVault startup disk,
   - unlocked encrypted external APFS,
   - hardware-backed internal storage,
   - live non-startup APFS under churn,
   - snapshot-assisted candidate paths.
2. Define required privileges and whether a helper is acceptable.
3. Test whether any public or supportable snapshot mechanism can pin a live
   source for comparison.
4. Classify encryption cases:
   - raw metadata readable,
   - raw metadata encrypted,
   - API fallback required,
   - unsupported without privilege/user action.
5. Add user-facing mode and permission reporting.

Exit criteria:

- Common live systems have a correct fallback path.
- Raw mode is enabled only for cells that are supported, not merely readable.
- The product can explain privilege requirements before scanning.

### Gate 5: Size Semantics Beyond Logical Size

Product modes:

- `logical_size`
- `allocated_size`
- `compressed_logical_size`
- `physical_allocated_size`
- `shared_size`
- `exclusive_size`
- `snapshot_retained_size`

Target:
Make size reporting useful without pretending APFS has one universal "size on
disk" answer.

Work items:

1. Finish compressed logical-size fixture from `SR-017`.
2. Define allocated-size oracle for ordinary and sparse files.
3. Build clone/shared extent fixtures.
4. Parse file extents and extent-reference trees only when the metric requires
   them.
5. Add snapshot-retained fixtures and decide whether that metric belongs to
   files, directories, snapshots, or a separate retained-space view.
6. Label every metric in output and UI.

Exit criteria:

- Each metric has a formula, oracle, and mismatch note.
- Directory aggregates state whether they are additive, unique-inode, shared,
  exclusive, or retained.
- Physical/shared/exclusive modes are never inferred from logical-size tests.

### Gate 6: Incremental Scanning And Persistent Cache

Product mode:
Fast repeat scans.

Target:
Make repeated scans fast by reusing validated summaries, while keeping full
reparse correctness as the oracle.

Work items:

1. Rerun the `EX-07` mutation grid through native parsing with preserved raw
   media.
2. Define cache identity:
   - source identity,
   - volume UUID,
   - scan state,
   - OMAP domain,
   - OID,
   - object XID,
   - physical address,
   - checksum/content hash,
   - object type/subtype,
   - parser version,
   - summary schema version.
3. Prove subtree reuse against fresh native full parses.
4. Design cache storage and crash-safe writes.
5. Define invalidation:
   - parser/schema change,
   - missing continuity,
   - source identity mismatch,
   - unsupported feature drift,
   - checkpoint rollback,
   - snapshot switch,
   - unknown record-body or metric dependency.
6. Add kill/restart and corruption tests.

Exit criteria:

- Incremental output equals fresh full parse for the same selected state.
- Cache corruption or partial writes cannot produce stale successful output.
- The product can always force and verify a full reparse.

### Gate 7: Performance Engineering

Product mode:
Production fast path.

Target:
Make scans fast enough to justify the product without optimizing unvalidated
paths.

Work items:

1. Add benchmark harness with:
   - source class,
   - support gate reached,
   - entry count,
   - FS-tree node count,
   - cold/warm cache state,
   - raw read time,
   - OMAP/root time,
   - FS-record decode time,
   - namespace assembly time,
   - aggregate time,
   - cache load/save time,
   - oracle diff time when applicable.
2. Measure before optimizing.
3. Optimize in this order unless measurements contradict it:
   - read batching and block cache,
   - OMAP lookup batching,
   - FS-tree traversal locality,
   - allocation reduction in record decoding,
   - parallel decode where ordering is not semantic,
   - aggregate recomputation shortcuts after cache proofs.
4. Track memory ceilings for large volumes.

Exit criteria:

- Benchmarks are tied to correctness artifacts.
- Performance claims name source class and semantic mode.
- Optimization does not widen the support matrix.

### Gate 8: Product Packaging And UX

Product mode:
General user-facing application.

Target:
A user can scan common Mac storage and understand results, support boundaries,
and fallbacks.

Work items:

1. Define CLI and library API first.
2. Add a desktop UI after backend semantics are stable.
3. Surface:
   - scan mode,
   - source class,
   - size metric,
   - fallback reason,
   - unsupported raw gates,
   - last full-parse validation,
   - cache status.
4. Add export formats for observer/debug use.
5. Add installer/helper design only after privilege requirements are known.
6. Add telemetry-free diagnostic bundles for bug reports:
   - source facts,
   - feature masks,
   - parser gate reached,
   - error category,
   - no file contents by default.

Exit criteria:

- The UI cannot hide degraded mode.
- User-facing totals are traceable to a semantic mode and metric.
- Bug reports contain enough metadata to reproduce parser-gate decisions.

## Release Shape

### R0: Research Harness

Audience:
project developers only.

Includes:
existing Python probes, Rust scanner, source reviews, generated artifacts.

Must not claim:
product scans.

### R1: Narrow Rust MWP

Audience:
developers and controlled testers.

Includes:
raw single-volume namespace plus logical size for detached/stable unencrypted
APFS sources.

Must not claim:
startup disk, encryption, live churn, boot-root, physical accounting, or cache
reuse.

### R2: Hybrid Single-Volume Scanner

Audience:
technical users.

Includes:
raw fast path for supported sources and API fallback for unsupported local
single-volume scans.

Must not claim:
Finder-visible startup root unless Gate 3 is complete.

### R3: macOS Visible Namespace Scanner

Audience:
normal users.

Includes:
clear boot-root or mounted-view semantics and fallback behavior.

Must not claim:
physical/shared/exclusive metrics unless Gate 5 is complete.

### R4: Fast Repeat-Scan Product

Audience:
normal users.

Includes:
persistent cache and incremental scans where validated.

Must not claim:
cache reuse outside the exact validated support matrix.

## Blocker Register

| Blocker | Status | Owning Track | Next Action |
| --- | --- | --- | --- |
| `EX-14` checksum mismatch at block `1031` | Open | RL-01, RL-02, RL-10, RL-13 | Focused checkpoint/OMAP-context replay for the retained fixture shape |
| xfield replay with `xf_used_data` | Open | RL-03, RL-06, RL-07, RL-10 | Replay `EX-13` using the `SR-015` cursor rule |
| Rust body-field dump | Blocked | Gate 1 | Implement only after context and xfield replay gates pass |
| compressed logical-size fixture | Open | RL-07, RL-10 | Same-run fixture with public `st_size`, inode uncompressed size, decmpfs size, and dstream size |
| live startup support | Open | RL-08, RL-11, RL-13 | Expand read-path matrix and define fallback-first behavior |
| boot-root merged namespace | Blocked | RL-11 | Wait for native raw single-volume output, then build boot-root oracle |
| persistent cache | Blocked | RL-04, RL-05, RL-09 | Wait for native full scan, then rerun mutation grid |
| physical/shared/exclusive accounting | Blocked | RL-07, RL-11 | Metric-specific extent and snapshot probes |

## Quality Bar

A roadmap item is not done when code exists. It is done when:

- the claim has a named mode,
- the mode has an oracle,
- positive and negative cases are saved,
- support boundaries are machine-readable,
- the implementation fails closed outside those boundaries,
- the docs explain what changed,
- tests and probes pass in the current environment or record why they cannot.

This project should prefer a slower honest scanner over a fast scanner whose
semantic mode cannot be named.
