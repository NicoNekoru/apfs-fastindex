# First Raw Parser Prototype Plan

Status: Draft
Date: 2026-04-24
Scope: First implementation plan after the narrow parser contract and pinned-state proof loop
Depends on:
- `docs/research/contracts/narrow-v1-parser-contract.md`
- `docs/research/experiments/EX-03-pinned-state-raw-vs-oracle/README.md`
- `spec.md`

## Bottom line

The repo now has enough evidence to plan the first real raw parser prototype
without reopening broad prerequisite research.

That prototype should stay deliberately small:

- one APFS volume
- one chosen coherent state
- correct namespace
- `logical size`
- fail closed outside the tested allowlist

## Entry Criteria

This prototype plan is justified because the repo now has:

- a closed narrow-v1 contract in `contracts/narrow-v1-parser-contract.md`
- one successful pinned-state proof loop in `EX-03`
- no active medium-confidence gap inside the current image-backed allowlist

## Non-Goals

The first prototype must not attempt:

- incremental reuse across runs
- allocated, exclusive, or shared accounting
- merged boot-root semantics
- broad live-startup-disk support
- "best effort" parsing of unsupported environments

## Proposed Pipeline

```text
raw source
-> support/allowlist gate
-> checkpoint pinning
-> container root discovery
-> volume root discovery
-> FS record walk
-> namespace entries
-> logical-size aggregates
-> oracle diff
```

## First Module Boundaries

The first prototype should be split into modules that match the contract rather
than the future performance architecture.

### 1. Source Gate

Responsibility:

- accept one raw source
- confirm it is inside the tested allowlist
- reject unsupported environments before traversal starts

Inputs:

- source path or device path
- requested mode: raw single-volume namespace + `logical size`

Outputs:

- validated source descriptor or fallback decision

### 2. Scan Pinning

Responsibility:

- read block 0
- locate checkpoint descriptor area
- choose the valid checkpoint with the highest `xid`
- record one pinned `scan_xid`

Outputs:

- `ScanState`

### 3. Root Resolver

Responsibility:

- resolve container OMAP
- resolve selected volume superblock
- resolve selected volume OMAP
- resolve the file-system root tree
- validate checksum, OID, XID, and expected type/subtype at each step

Outputs:

- `ResolvedRoots`

### 4. FS Record Reader

Responsibility:

- fetch FS records for one file ID in the pinned state
- expose only the record families needed for narrow v1

Required record families:

- `DIR_REC`
- `INODE`
- size-bearing inode or dstream fields
- `SIBLING_LINK` / `SIBLING_MAP` when present
- symlink-bearing xattr path validated in `EX-03`

Outputs:

- typed record groups by file ID

### 5. Namespace Builder

Responsibility:

- walk directory records from the file-system root
- emit one namespace entry per visible path
- preserve shared file identity for hard-linked paths
- emit symlink nodes without following them

Outputs:

- flat namespace entry list
- parent/child path graph

### 6. Aggregate Builder

Responsibility:

- compute per-entry `logical size`
- compute per-directory unique-inode logical totals

Outputs:

- file metrics
- directory aggregates

### 7. Regression Harness

Responsibility:

- compare prototype output to the locked oracle corpus
- fail fast on path, type, file identity, or `logical size` mismatches

Outputs:

- pass/fail diff report

## Minimal Data Structures

The first prototype should prefer a few explicit structures over a rich object
model.

### `SourceDescriptor`

Fields:

- source path
- source kind (`image`, `device`)
- allowed mode
- support-gate verdict

### `ScanState`

Fields:

- block size
- pinned `scan_xid`
- checkpoint descriptor base
- checkpoint descriptor length

### `ResolvedObject`

Fields:

- owning OMAP context
- logical object ID
- resolved physical address
- object XID
- object type
- object subtype

### `ResolvedRoots`

Fields:

- selected volume object ID
- container OMAP root
- volume OMAP root
- file-system root OID

### `NamespaceEntry`

Fields:

- path
- parent path
- entry kind
- file ID
- `logical size`
- optional symlink target

### `DirectoryAggregate`

Fields:

- directory path
- unique-inode logical total
- contributing file IDs

The first prototype does not need persistent cache records, block reuse history,
or performance-oriented subtree summaries.

## First Implementation Stages

### Stage 1: Environment Gate And Pinning

Goal:

- load a supported raw source
- pin one coherent scan state

Success condition:

- produce a `ScanState` for the same class of detached image used in `EX-03`

### Stage 2: Resolver And Root Discovery

Goal:

- implement container OMAP -> volume superblock -> volume OMAP -> fs root chain

Success condition:

- resolve the root tree with explicit validation and no best-effort shortcuts

### Stage 3: Flat Namespace Entries

Goal:

- emit flat entries with path, type, file ID, `logical size`, and symlink target

Success condition:

- reproduce the `EX-03` path set exactly

### Stage 4: Directory Aggregates

Goal:

- add unique-inode logical totals for directories

Success condition:

- match the `EX-03` naive and unique-inode aggregate facts where applicable

### Stage 5: Regression Runner

Goal:

- make the `EX-03` proof loop reusable as a prototype regression check

Success condition:

- one command or test target diffs prototype output against the saved oracle

## First Regression Checks

The initial regression target should be the exact artifact set created by
`EX-03`.

Required checks:

- path set matches `artifacts/generated/oracle.json`
- entry kinds match
- file IDs match oracle inode identities
- `logical size` matches for:
  - regular files
  - sparse files
  - hard-linked files
  - cloned files
  - symlinks
- symlink target matches for `dst/link.txt`

Secondary checks:

- directory aggregate policy matches the contract
- unsupported source or unsupported feature path fails closed

## Explicit Cut List

If the prototype starts growing beyond the contract, stop and cut back.

Immediate cut triggers:

- adding extent-reference parsing
- adding snapshot metadata traversal
- adding merged-root semantics
- adding cache persistence
- broadening the support matrix without a fresh proof loop

## Recommended Starting Shape

The prototype should begin as a correctness-first parser skeleton, even if it is
not fast yet.

A reasonable first code layout is:

- `source_gate`
- `scan_state`
- `resolver`
- `fs_records`
- `namespace`
- `aggregate`
- `oracle_diff`

This plan is intentionally language-agnostic.
If the prototype starts in Python to stay close to the existing experiment code,
that is acceptable as long as the module boundaries above are preserved.

## Exit Condition

This prototype plan is complete when the first code implementation can:

- run on the same detached image-backed allowlist used in `EX-03`
- emit namespace entries and `logical size`
- fail closed on unsupported states
- diff itself against the saved oracle artifacts

At that point the project can begin writing parser code around a closed contract
instead of rediscovering prerequisites.
