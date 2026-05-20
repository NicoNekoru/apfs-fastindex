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

R2 expands the fallback set to include **APFS-snapshot-backed scanning**
as an explicit support-matrix cell. Snapshots are now an R2 research
lane (see RL-11) rather than purely deferred. The investigation
boundary: take a free local snapshot, mount it read-only, run the
existing fallback or raw walker against it, and validate that the
resulting `NamespaceEntry`/`DirectoryAggregate` rows match the
non-snapshot baseline for unchanged data. Anything beyond that
(snapshot-retained byte accounting, cross-snapshot diffs, sealed
system volume content via user snapshots) requires further explicit
scope approval.

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

The following are explicitly in-scope for R2 research and are tracked as
named investigation lanes (see RL-07b):

- **Physical / allocated size per file**. The size source candidates
  (`j_dstream_t.alloced_size`, file-extent records, extent-reference
  tree) are already partially decoded by the v1 body parser; R2 promotes
  them from "diagnostic" to an oracle-validated product metric. EX-22
  validated the ordinary / clone / hard-link / symlink / directory
  cases against `st_blocks * 512`; EX-26 extended that to sparse files
  (`alloced_size - sparse_bytes`) and decmpfs-compressed files (sum of
  stream-backed xattr dstreams' allocated bytes), so every shape the
  macOS write-path produces now emits a value the parser can defend.
  The extent-reference tree (clone-deduplicated allocated bytes) is a
  separate R5 phase (EX-27) for the "Real Bytes" metric.

The following remain longer-range future modes; opening them requires
explicit support-matrix approval and a dedicated oracle:

- exclusive size (per-inode block ownership versus clones and snapshots)
- shared size (cross-inode shared blocks)
- snapshot-retained attribution

If any of these later modes are added, they must be clearly labeled and
must not be implied to reconcile with all APFS and macOS tools
simultaneously unless proven. An emitted metric whose oracle is not
green stays in `not_claimed` and must not be presented in user-facing
output.

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

R1 (Narrow Rust MWP) is complete:

1. ✅ resolved the `EX-14` checkpoint/OMAP-context blocker (EX-15)
2. ✅ replayed `EX-13` body decoder with the source-backed xfield rule (EX-16)
3. ✅ added synthetic fail-closed record-body cases (EX-17)
4. ✅ implemented Rust FS-record body field dumps (EX-18)
5. ✅ promoted Rust output to namespace/logical-size rows after oracle parity
   (EX-19 + EX-20 + Rust MWP smoke)
6. ✅ landed the POSIX fallback walker (EX-21) and resilience pass

R2 lanes (open, explicitly scoped, no Gate-2 broadening yet):

7. **R2-A: physical-size per file.** Promote `j_dstream_t.alloced_size`
   plus file-extent and extent-reference records from "diagnostic"
   to an oracle-validated product metric. Same source class (detached
   `.dmg` + POSIX-mounted directory), same fail-closed contract. Tracked
   under an expanded `RL-07` (see `RL-07b` evidence notes).
8. **R2-B: snapshot-assisted scanning.** Take a free local APFS
   snapshot, mount it read-only, scan it with the existing walkers,
   prove field parity with the non-snapshot baseline. Tracked under
   `RL-11`. Anything beyond shape parity (snapshot-retained accounting,
   sealed-system content) remains out of scope.
9. **R2-C: scanner ergonomics.** `getattrlistbulk` perf for the
   fallback walker, stderr progress streaming, native macOS shell.
   No emission-contract changes.

Gate-2 work (encryption, live boot disk, boot-root merged namespace,
incremental cache) still requires separate explicit approval. Cache
design, subtree reuse, and repeat-scan performance remain out of v1.

## 13. Long-Term Product Roadmap

The general WizTree-for-any-Mac product path is tracked in:

- `docs/research/plans/general-wiztree-for-any-mac-roadmap.md`

That roadmap defines staged gates from the narrow Rust full scan to a hybrid
consumer product:

1. native narrow Rust full scan
2. local single-volume backend with safe fallback
3. Finder-visible macOS namespace mode
4. encryption and live runtime support
5. size semantics beyond logical size
6. incremental scanning and persistent cache
7. performance engineering
8. product packaging and user experience

The intended broad product is hybrid, not raw-only. Raw parsing should be used
only when the source class, semantic mode, and metric have matching evidence;
other cases should fall back to supported APIs or remain unsupported.
