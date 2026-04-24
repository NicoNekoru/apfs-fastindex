# APFS High-Performance Filesystem Indexing (WizTree-like)

## Research-Grounded Technical Spec

This document defines the current intended target for the project.
It is intentionally narrower than the original concept note.

The repo's current research state does not yet justify a broad claim like
"fast raw APFS indexing on normal live Macs with safe incremental reuse."
The first defensible target is a correct, narrow parser mode that can be
validated rigorously and expanded later.

## 1. Project Goal

Long-term goal:

- Build a WizTree-like indexer for APFS
- Reduce repeat-scan cost by exploiting APFS structure where that is proven safe
- Remain explicit about correctness, support boundaries, and fallback behavior

Current v1 goal:

- Parse one APFS volume
- At one coherent filesystem state
- Reconstruct a correct directory namespace
- Report `logical size` as the canonical file and directory metric
- Validate all results against a stable oracle

## 2. Why The Scope Is Narrower

APFS does not expose an NTFS-style flat metadata table.
Instead, the parser must handle:

- checkpoint selection
- object-map resolution
- heterogeneous file-system records
- copy-on-write versioning
- format and deployment variability across modern macOS systems

Research completed so far supports a narrow full-scan target before a broader
incremental engine.

The following ideas remain unproven enough that they are not part of the v1
contract:

- `node_cache[oid]` as a safe persistent identity model
- `unchanged node => unchanged subtree` as an implementation guarantee
- exact physical/exclusive/shared accounting across clones, compression, sparse
  files, and snapshots
- raw parsing as the default mode on live, common-user startup disks
- Finder-like merged `/` semantics from raw single-volume parsing

## 3. V1 Product Contract

### 3.1 Supported Output

V1 raw mode aims to produce:

- a complete path tree for one APFS volume
- stable parent/child relationships for that volume's namespace
- per-file `logical size`
- per-directory aggregates derived from `logical size`

### 3.2 Deliberately Excluded From V1

The following are not part of the initial correctness contract:

- safe persistent incremental reuse across runs
- exact "size on disk" reconciliation on clone-heavy or snapshot-heavy data
- exclusive/shared byte attribution
- merged system/data/firmlink/cryptex view of the macOS boot root
- best-effort parsing of unsupported or weakly understood APFS variants

### 3.3 Supported Raw-Mode Environment

The initial raw-mode target is:

- offline images, or
- explicitly stable APFS views, or
- tightly controlled lab volumes that are simple enough to validate

The initial raw-mode target is not:

- "whatever the user is currently booted from"
- every FileVault or hardware-backed encryption configuration
- every live mounted startup-disk scenario

If the environment falls outside the tested raw-mode allowlist, the product
should fall back to safer supported APIs rather than guessing.

## 4. Support Boundary And Fallback

### 4.1 Raw Mode Allowlist

Raw mode should only be considered when all of the following hold:

- the volume/container layout is in the tested support matrix
- a coherent filesystem state can be identified
- required feature bits and tree layouts are recognized
- the relevant object maps can be resolved without ambiguity
- the requested product mode is "single APFS volume" rather than "user-visible
  merged root"

### 4.2 Fallback Triggers

Raw mode should fail closed and fall back when any of the following is true:

- checkpoint selection is ambiguous or malformed
- object resolution yields unexpected object type/subtype results
- unsupported incompatible feature flags are present
- the environment depends on unsupported encryption, snapshot, or volume-group
  semantics
- the volume is outside the validated compatibility matrix
- the product request requires merged-root semantics rather than raw
  single-volume semantics

### 4.3 Fallback Modes

Fallback strategy should be explicit:

- POSIX traversal for maximum support
- bulk attribute APIs where they improve performance safely
- snapshot-assisted workflows only where supported and operationally realistic

## 5. Full-Scan Model For V1

The current baseline raw pipeline is:

```text
read container superblock copy at block 0
-> locate checkpoint descriptor area
-> select latest valid checkpoint
-> load checkpoint state
-> resolve container OMAP and target volume superblock
-> resolve volume OMAP and FS tree root
-> traverse required trees/records for one volume
-> reconstruct namespace
-> compute logical-size aggregates
-> compare output to oracle
```

This is a full reparse model.
V1 does not assume persistent cache reuse across runs.

## 6. Minimum Parser Surface

The v1 parser is expected to answer the following questions correctly:

- which checkpoint / transaction defines the scan
- which OMAP is authoritative for each object being resolved
- which tree roots must be loaded to enumerate one volume
- which record families are required for namespace reconstruction
- which additional fields are required for `logical size`

Research still needs to close the exact minimum required-record matrix.
That matrix, not intuition, should define the parser surface.

## 7. Correctness Standard

The parser is only considered correct when:

- it reads from one coherent filesystem state
- namespace output matches the selected oracle
- file and directory `logical size` output matches the selected oracle
- known edge cases are covered by a repeatable mutation corpus
- unsupported states trigger fallback rather than silent best-effort behavior

Correctness for full scans and correctness for incremental reuse are separate
gates.

## 8. Incremental Scanning Status

Incremental scanning remains a research track, not a v1 promise.

Before any persistent incremental design is encoded into the main spec, the
repo still needs proof for:

- safe cache identity
- OID and block reuse behavior under churn
- subtree reuse conditions under APFS-specific tree updates
- conservative invalidation rules
- validation against a fresh full-parse oracle

No current design in this repo should assume that `oid` alone is a safe cache
key or that an unchanged node automatically implies a reusable subtree summary.

## 9. Size Semantics

The canonical v1 metric is:

- `logical size`

The following remain future modes pending more evidence:

- allocated size
- exclusive size
- shared size
- snapshot-retained attribution

If later modes are added, they must be clearly labeled and must not be implied
to reconcile with all APFS and macOS tools simultaneously unless proven.

## 10. Namespace Semantics

The initial parser target is:

- one raw APFS volume

The initial parser target is not:

- the Finder-visible boot-root view assembled from system/data volume groups,
  firmlinks, snapshots, and related presentation layers

If the product later grows a "boot-root" or "OS-visible namespace" mode, that
should be specified as a separate semantic mode with its own oracle and support
matrix.

## 11. Validation Requirement

Every research or implementation step should end in durable evidence:

- source review notes for external claims
- experiment notes for probes and mutations
- explicit links back into the affected `RL-*` logs

Performance claims are non-goals until correctness and support boundaries are
stable.

## 12. Near-Term Roadmap

Near-term work should prioritize:

1. support-boundary and fallback definition
2. checkpoint / OMAP / root-discovery contract
3. required-record matrix for namespace + logical size
4. experiment and oracle infrastructure
5. small controlled APFS probes

Only after those are stable should the project move cache design, subtree
reuse, and repeat-scan performance back to the center of the spec.
