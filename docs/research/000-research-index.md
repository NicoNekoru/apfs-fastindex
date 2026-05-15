# APFS Indexer Research Index

Purpose:
Track unresolved technical questions for a high-performance APFS indexing
engine, and make every research step durable enough that future humans and
agents do not need to reconstruct context from scratch.

## Research Rules

- Treat every claim as one of:
  - `Spec`: backed by public documentation
  - `Observation`: confirmed empirically or by converging implementations
  - `Hypothesis`: plausible but not yet proven
- Do not let raw notes become the canonical record. Distill them back into the
  appropriate `RL-*` logs.
- Every artifact must change at least one of:
  - what we believe
  - what we rule out
  - what we do next
- If an artifact does not update an `RL-*` log or define the next exact step, it
  is probably too vague.

## Artifact Types

### `RL-*` Research Logs

Use `RL-*` files for durable question-led synthesis:

- the core question
- why it matters
- current assumptions
- known facts
- open unknowns
- risks
- planned probes
- evidence log
- interim decisions
- exit criteria

`RL-*` files are living synthesis, not raw evidence dumps.

### `SR-*` Source Reviews

Use `sources/SR-###-slug.md` for compact reviews of external evidence on one
coherent topic.

Every `SR-*` file must:

- open with `Bottom line`
- declare `Related RLs`
- separate `Spec`, `Observation`, and `Hypothesis`
- end with `Decision impact`

Current source reviews:

- `SR-001` V1 support boundary
- `SR-002` checkpoint, OMAP, and root-discovery contract
- `SR-003` FS record taxonomy for narrow v1
- `SR-004` runtime read path and encryption boundary
- `SR-005` checkpoint validation details for the Rust scanner
- `SR-006` OMAP lookup semantics and failure cases
- `SR-007` object-header fail-closed validation
- `SR-008` FS record layout for native v1 parsing
- `SR-009` compression and logical-size precedence
- `SR-010` snapshots, volume groups, and firmlink boundaries
- `SR-011` encryption and runtime read-path boundary
- `SR-012` format drift and feature-bit allowlisting
- `SR-013` checkpoint map integrity and ephemeral-object validation
- `SR-014` native FS-record body contract
- `SR-015` xfield layout and alignment
- `SR-016` record-body fail-closed boundary
- `SR-017` logical-size source precedence
- `SR-018` name normalization and case behavior

### `EX-*` Experiment Notes

Use `experiments/EX-###-slug/README.md` for one controlled probe or mutation
program.

Each experiment directory may contain:

- `README.md` for distilled results
- `artifacts/` for manifests, scripts, raw outputs, and diff snapshots

Every `EX-*` note must record:

- environment
- oracle
- exact setup
- exact steps
- expected outcomes for competing hypotheses
- observed results
- interpretation
- what the experiment rules out
- impact on related `RL-*` logs

Negative or inconclusive results are first-class artifacts.

## Oracle Policy

Validation is feature-specific.
Do not speak of "the oracle" as if one tool answers everything.

Current oracle policy:

- namespace oracle:
  POSIX/API traversal of the chosen volume or stable view
- logical-size oracle:
  public file metadata APIs and tools that report logical size
- FS-record body oracle:
  same-run mounted/POSIX namespace and logical-size facts plus raw native field
  dumps under a declared selected XID
- allocated-size oracle:
  public file metadata APIs only for explicitly supported cases
- incremental oracle:
  fresh full reparse compared against the incremental path
- boot-root semantics oracle:
  user-visible macOS namespace only when the product mode explicitly targets it

Every experiment must state which oracle it uses and why that oracle is valid
for the exact question being tested.

## Documentation Layout

- `RL-*` files: distilled rolling synthesis
- `sources/`: external source reviews
- `experiments/`: controlled probes and their artifacts
- implementation-facing docs:
  - `contracts/narrow-v1-parser-contract.md`
  - `plans/general-wiztree-for-any-mac-roadmap.md`
  - `plans/first-raw-parser-prototype-plan.md`
  - `../implementation/000-implementation-index.md`
  - `../implementation/narrow-v1-proof-parser-skeleton.md`
- living manual:
  - `../manual/apfs-fastindex-manual.tex`
- templates:
  - `001-research-template.md`
  - `002-source-review-template.md`
  - `003-experiment-template.md`

Narrative-tightness rule:

- each new artifact should answer one primary question
- keep raw notes in `artifacts/`
- keep durable conclusions in `README.md`
- summarize the implication back into the relevant `RL-*` logs

## Staged Gates

The repo should think in staged gates, not a flat P0/P1 list.

Gate A: minimally correct v1 parser

- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle

Gate B: support boundary

- RL-08 Live Volume, Encryption, and Read Path
- RL-13 Format Drift, Compatibility, and Fallback

Gate C: safe incremental scanning

- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation

Gate D: broader product semantics and optimization

- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-12 Performance Model and Optimization

Long-range product roadmap:

- `plans/general-wiztree-for-any-mac-roadmap.md`: staged route from the narrow
  Rust full scan to a hybrid WizTree-for-any-Mac product, including support
  matrix expansion, boot-root semantics, live/encrypted runtime behavior,
  metric-specific accounting, persistent incremental cache, performance, and
  packaging gates.

## Current Parser Gates Enabled By SR-015..SR-018

1. Isolate the `EX-14` upstream context blocker: the expanded fixture cannot
   validate body layout until the checksum mismatch at block `1031` is explained
   or reproduced as a malformed-source hard stop. **Closed by EX-15** (root
   cause: FS-tree internal-node values are virtual OIDs requiring volume-OMAP
   resolution; patched in `crates/apfs-fastindex/src/fs_records.rs`).
2. Replay `EX-13` with the source-backed xfield cursor rule from `SR-015`,
   recording `xf_used_data`, padded value lengths, decoded fields, and the same
   namespace/logical-size comparison. **Closed by EX-16**: 14/14 records
   pass `xf_used_data == sum(round_up(x_size, 8))`; namespace and
   logical-size oracle parity preserved.
3. Add synthetic negative record-body cases from `SR-016`: short fixed bodies,
   malformed names, duplicate/out-of-bounds xfields, invalid xattr forms,
   missing sibling mappings, and drec/inode type mismatches.
4. Execute the logical-size precedence gate from `SR-017`: ordinary, sparse,
   cloned, hard-linked, symlink, and compressed files with public `st_size` and
   every raw candidate size source captured in the same selected state.
5. Add the name/case fixture from `SR-018` before lookup-by-name is implemented:
   APFS hash, normalization, case-folding, and collision checks. Row enumeration
   can proceed earlier only if stored directory-key names are emitted verbatim.
6. Implement Rust FS-record body decoding only after gates 1-3 pass; enable
   logical-size rows after gate 4; enable lookup/search semantics after gate 5.

## Current Experiment Tracks

- `EX-01` live checkpoint consistency and runtime boundary
- `EX-02` required-record taxonomy for narrow v1
- `EX-03` pinned-state raw-vs-oracle proof loop
- `EX-04` expanded pinned raw-vs-oracle corpus
- `EX-05` live pinned churn; mounted image raw reads worked operationally, but
  latest-state raw output did not match baseline or final mounted oracles under
  churn
- `EX-06` OID, paddr, XID, checksum identity tracking
- `EX-07` subtree reuse proof probe; first execution found zero false reuse for
  exact node-identity matches in the detached lab corpus
- `EX-08` read-path support matrix; first safe-host execution supports detached
  unencrypted image-backed APFS for narrow v1 proof work, keeps mounted images
  `readable_not_supported`, and records startup raw read as `blocked_privilege`
- `EX-09` accounting probe design; keeps physical/shared/exclusive/compression
  and snapshot-retained metrics outside v1 until metric-specific evidence exists
- `EX-10` Rust checkpoint scanner; native Rust read-only path that now covers
  source gating, descriptor scanning, NX superblock decoding, checkpoint-map
  validation, container/volume OMAP `(oid, max_xid)` resolution, volume
  superblock decoding under the v1 feature allowlist, FS-tree root validation
  against the volume OMAP, and a read-only FS-tree record-family dump. Probe
  asserts that no validation gaps and no unsupported FS-record families are
  reported on the proof fixture. Native namespace emission and logical-size
  decoding remain unimplemented.
- `EX-11` checkpoint map integrity design; validates checkpoint-map chains and
  mapped ephemeral objects before native OMAP/root traversal. First execution
  validated a generated detached proof fixture and matched synthetic malformed
  checkpoint-map hard-stop cases.
- `EX-12` OMAP lookup contract; executed end-to-end via a self-paired probe
  that builds a fresh proof fixture, runs the native Rust scanner and
  `go-apfs identitydump` against the same `/dev/rdiskN`, replays
  obj-header validation at every Rust-returned paddr, re-runs SR-006
  lower-bound on Rust's published OMAP samples, and confirms cross-tool
  agreement on `root_tree.oid`. Verdict: `validated_omap_lookup_contract`
  for the proof fixture. `(paddr, object_xid)` divergence between Rust
  (selected_xid 14) and `go-apfs` (apparent selected_xid 12) is recorded
  as a `go_apfs_active_state_observation` caveat. Resolver hard stops now
  cover `OMAP_VAL_DELETED` (negative result) plus `OMAP_VAL_ENCRYPTED`,
  `OMAP_VAL_NOHEADER`, `OMAP_VAL_CRYPTO_GENERATION`, unknown
  `omap_val_t.flags` bits, and OMAP-phys
  `ENCRYPTING`/`DECRYPTING`/`KEYROLLING`/`CRYPTO_GENERATION_FLAG`/
  unknown-bit hard stops at OMAP open time, all covered by Rust unit
  tests on synthetic OMAPs.
- `EX-13` native FS-record body oracle; executed as a Python-first raw-byte
  experiment after `EX-12`. It decoded `DIR_REC`, `INODE`, `XATTR`,
  `SIBLING_LINK`, `SIBLING_MAP`, and dstream field candidates, reconstructed all
  mounted paths, and preserved same-run mounted/POSIX plus `go-apfs` observer
  artifacts. Verdict: `validated_native_record_body_contract`; the sparse-file
  mismatch was resolved by recording candidate xfield layouts and selecting the
  blob-relative data alignment that matches `j_dstream_t.size` and
  `INO_EXT_TYPE_SPARSE_BYTES`. The extended probe now writes
  `xfield-layout-summary.json`; `4` non-row-critical records still have
  top-score layout ambiguity, so keep the next fixture-variant pass in Python
  before moving this rule into Rust.
- `EX-14` xfield layout variant; executed as the Python-first successor to
  `EX-13`, but returned `oracle_inconclusive` before xfield comparison. The
  retained detached unencrypted fixture saved a mounted/POSIX oracle and Rust
  context artifact; Rust reached source gating and found `4` valid checkpoint
  candidates (`highest_xid=20`) but returned no `selected_checkpoint` after
  `APFS object validation failed: checksum mismatch at block 1031`. EX-15
  closed this blocker.
- `EX-16` SR-015 xfield replay; re-ran EX-13's proof fixture under the SR-015
  single cursor rule (`cursor += round_up(x_size, 8)` starting immediately
  after the metadata table). All `14` records with xfields satisfy
  `xf_used_data == sum(round_up(x_size, 8))` and namespace + logical-size
  oracle parity is preserved. Verdict `validated_sr_015_cursor_rule`; SR-015
  may now be encoded in Rust body decoding (gated by EX-18 byte-for-byte
  field diff). The sparse-file inode that EX-13 needed candidate scoring for
  decodes cleanly under one rule.
- `EX-15` block-1031 context replay; rebuilt the EX-14 fixture deterministically,
  proved with `fsck_apfs -n`, `go-apfs identitydump`, and a Python SR-005 /
  SR-007 / SR-006 replay that the image is well-formed and every NXSB candidate
  validates. Root cause was hypothesis (c): `fs_records::walk_fs_node` treated
  FS-tree internal-node values as physical paddrs, but FS-trees are virtual and
  the 8-byte internal value is a child virtual OID requiring `(oid, max_xid)`
  resolution through the volume OMAP. Block `1031` was the bare OID
  misinterpreted as a paddr. Fix landed in `crates/apfs-fastindex/src/fs_records.rs`
  with two synthetic regression tests; Rust now publishes `selected_checkpoint`
  with `fs_records_dumped_count = 1` on the EX-14 fixture shape.
- `SR-015` through `SR-018` tightened the post-EX-13 parser gates: xfields have
  one source-backed padded-value cursor rule, malformed record bodies fail
  closed before row emission, logical size has a scoped precedence table, and
  stored UTF-8 names must be preserved without claiming full APFS lookup
  semantics.

## Research Tracks

- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-03 FS Tree Topology and Required Records
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-08 Live Volume, Encryption, and Read Path
- RL-09 Cache Persistence and Invalidation
- RL-10 Validation Corpus and Oracle
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-12 Performance Model and Optimization
- RL-13 Format Drift, Compatibility, and Fallback
