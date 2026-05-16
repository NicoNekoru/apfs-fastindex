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
- `SR-019` allocated-size source precedence (R2-A entry)
- `SR-020` snapshot API and mount lifecycle (R2-B entry)

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

## R2 lanes (open)

R1 (Narrow Rust MWP) is complete. Two named R2 lanes are open and
explicitly scoped; they sit between Gate A (done) and Gate B
(unopened) and do **not** widen the support matrix to live boot disks,
encrypted runtime, or boot-root merged namespace.

- **R2-A — physical-size per file.** Promote per-file allocated bytes
  from "diagnostic" to an oracle-validated product metric. Investigation
  lives under an expanded `RL-07` ("R2-A direction" interim decision).
  Source-of-truth candidates: `j_dstream_t.alloced_size`,
  `j_file_extent_*` records, the volume's extent-reference tree. Same
  source class as R1 (detached `.dmg` + POSIX-mounted directory),
  same fail-closed contract. Entry plan: `SR-019` source review →
  `EX-22` same-run fixture probe against `st_blocks * 512`.
- **R2-B — snapshot-assisted scanning.** Take a free local APFS
  snapshot, mount it read-only, run the existing fallback walker
  against it, prove `NamespaceEntry`/`DirectoryAggregate` shape parity
  with the live-directory scan on unchanged data. Investigation lives
  under an expanded `RL-11` ("R2-B direction" interim decision).
  Entry plan: `SR-020` source review (snapshot API semantics, mount
  lifecycle) → `EX-23` fixture probe (live vs snapshot shape parity).
  Snapshot-retained byte accounting and sealed-system content stay
  out of R2-B scope.

Both R2 lanes share the discipline of R1: every claim has a named
oracle, a source review, and a probe that records both the positive
and negative case. Neither lane promotes any new entry into the Rust
crate until the corresponding `EX-*` records a passing shape oracle.

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
   missing sibling mappings, and drec/inode type mismatches. **Closed by
   EX-17** for the per-record cases (21 Rust unit tests in
   `crates/apfs-fastindex/src/fs_record_body.rs::tests`); cross-record
   cases (drec entry-type vs inode mode, sibling map closure) remain
   EX-19+ work.
4. Execute the logical-size precedence gate from `SR-017`: ordinary, sparse,
   cloned, hard-linked, symlink, and compressed files with public `st_size` and
   every raw candidate size source captured in the same selected state.
   **Closed by EX-19** for the proof-fixture shape (5/5 unique inodes
   match; compressed case picks `inode.uncompressed_size` per SR-017
   step 4 since the decmpfs header carries placeholder data).
5. Add the name/case fixture from `SR-018` before lookup-by-name is implemented:
   APFS hash, normalization, case-folding, and collision checks. Row enumeration
   can proceed earlier only if stored directory-key names are emitted verbatim.
   **Closed by EX-20** for row enumeration: CI and CS volumes both have
   Rust paths byte-for-byte matching POSIX traversal. Lookup-by-name
   semantics remain explicitly unclaimed.
6. Implement Rust FS-record body decoding only after gates 1-3 pass; enable
   logical-size rows after gate 4; enable lookup/search semantics after gate 5.
   **Body decoding closed by EX-18**, **logical-size rows closed by EX-19**,
   **row enumeration closed by EX-20**. **Rust MWP promoted** — the crate
   now emits `NamespaceEntry` and `DirectoryAggregate` rows under SR-017
   precedence and SR-018 name preservation, with a CLI `--summary` mode
   that prints the one-line correctness_claim and the `not_claimed`
   register. Lookup-by-name (APFS hash + normalization + case fold) is
   still NOT claimed.

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
- `EX-22` SR-019 allocated-size precedence; built the same-run
  fixture from EX-19 (ordinary, sparse, clone, hard link, symlink,
  `ditto --hfsCompression`), captured per-inode
  `(j_dstream_alloced_size, j_dstream_size, sparse_bytes,
  st_blocks * 512)` plus the FS-tree family histogram, and applied
  SR-019 precedence. Verdict
  `partial_validated_sr_019_alloced_size`: 4/5 emit-rows match
  `st_blocks * 512` (ordinary 4096↔4096; clone 4096↔4096;
  symlink 0↔0; compressed correctly fail_closed with the oracle
  `4096` listed as `not_claimed`); sparse.bin diverges by exactly
  `INO_EXT_TYPE_SPARSE_BYTES` (`alloced_size = 1056768` vs
  oracle `24576`; difference `1032192 = sparse_bytes`). This is
  the empirical confirmation of SR-019's recorded
  linux-apfs-rw-vs-apfsck disagreement on macOS-produced images.
  The Rust slice ships sparse explicitly fail-closed; an EX-22b
  sparse-corpus probe is the gate for promoting sparse rows.
- `EX-23` snapshot shape parity (best-effort, never-sudo);
  enumerated 9 mounted APFS volumes on the host, found the only
  present snapshot (sealed-system OS-update on `/`) is excluded
  by SR-020, and exited verdict `blocked_no_snapshots_at_all`
  with the reproducer for a privileged rerun. R2-B's shape-parity
  claim stays in `not_claimed` until a user-visible snapshot is
  mountable for the diff; the probe is re-runnable and will
  detect any future user-created TM local snapshot
  automatically. SR-020 already documented the entitlement gate
  (`fs_snapshot_create` needs root + `com.apple.developer.vfs.snapshot`),
  so this is the predicted negative-progress outcome on a clean
  dev workstation.
- `EX-21` fallback path skeleton; landed a POSIX-traversal-backed
  fallback in `src/apfs_fastindex/fallback_traversal.py` that emits the
  same `NamespaceEntry` + `DirectoryAggregate` shape as the Rust raw
  scanner. Probe: build the EX-13 proof fixture, run the fallback
  against the mounted image, detach + rescan with Rust, diff. First-run
  verdict `validated_fallback_skeleton`: 7/7 entries match (paths,
  entry_kinds, logical sizes, symlink target), 3/3 aggregates match,
  zero `file_id` divergence on this fresh fixture (POSIX inode and
  APFS virtual OID happen to coincide; v1 contract permits divergence).
  Gate-2 source classes (live boot, encryption, snapshot-assisted,
  boot-root) remain explicitly out of scope.
- `EX-20` SR-018 name/case fixture; built one `APFS` and one
  `Case-sensitive APFS` image, ran ASCII case duplicates + NFC vs NFD
  Unicode collisions, and asserted Rust `FsRecordDump.records` -based
  path reconstruction matches mounted POSIX traversal byte-for-byte on
  both volumes. Verdict `validated_sr_018_name_preservation`: CI shows
  `case_insensitive=true, normalization_insensitive=false` (4 entries,
  rejects both case + NFD duplicates); CS shows
  `case_insensitive=false, normalization_insensitive=true` (5 entries,
  allows case duplicate but still rejects NFD duplicate). Stored UTF-8
  bytes preserved on Rust side with no normalization or case folding.
  Lookup-by-name still NOT claimed by Rust.
- `EX-19` SR-017 logical-size precedence; built a same-run fixture covering
  ordinary, sparse, clone, hard link, symlink, and `ditto --hfsCompression`
  cases. Captured public `st_size`, inode `internal_flags`,
  `uncompressed_size`, `j_dstream_size`, `j_dstream_alloced_size`,
  `INO_EXT_TYPE_SPARSE_BYTES`, the `com.apple.decmpfs` header, and the
  `com.apple.fs.symlink` payload from the patched Rust scanner output.
  Verdict `validated_sr_017_precedence` on first run: all 5 unique inodes
  match `st_size` under SR-017 with zero mismatches, including the
  compressed case (`INODE_HAS_UNCOMPRESSED_SIZE` + `uncompressed_size`
  wins over the decmpfs header on this fixture). Hard-link case is
  covered by per-inode rule sharing.
- `EX-18` Rust body-field dump; rebuilt the EX-13 proof fixture, ran the
  patched Rust scanner with `FsRecordDump.records` emission, and diffed
  against the Python EX-13 decoder + EX-16 SR-015 xfield replay. Verdict
  `field_level_parity` on the first run: 53 records both sides, zero
  divergent fields. The Rust body decoder now matches the Python contract
  on the proof fixture for `DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`,
  `SIBLING_MAP`, and dstream xfields. Body-decoder promotion gates SR-015
  and SR-016 are now satisfied for the proof fixture; product
  `NamespaceEntry` rows still wait on SR-017 (EX-19) and SR-018 (EX-20).
- `EX-17` synthetic fail-closed record bodies; landed 21 Rust unit tests in
  `crates/apfs-fastindex/src/fs_record_body.rs::tests` covering every
  per-record SR-016 fail-closed case (short bodies, malformed names,
  duplicate/out-of-bounds xfields, xattr flag combinations, wrong xfield
  sizes, sibling_link name overflow, sibling_map short value, drec entry
  type outside POSIX allowlist, `xf_used_data` mismatch, xfield blob
  shorter than header). Each test asserts a typed
  `ScanError::InvalidObject` with the SR-016 substring. Total crate test
  count: 55/55. Cross-record SR-016 cases (drec-vs-inode mode mismatch,
  missing sibling_map for drec carrying `DREC_EXT_TYPE_SIBLING_ID`) remain
  EX-19+ work.
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
- `SR-019` opens R2-A: mines Apple's PDF (commit 2020-06-22) plus
  linux-apfs-rw / apfsprogs / libfsapfs / TSK / dissect.apfs /
  apfs-fuse / go-apfs to pin per-file allocated-bytes precedence on
  `j_dstream_t.alloced_size`. Surfaces the kernel-vs-fsck disagreement
  (linux-apfs-rw writes `alloced_size = round_up(ds_size, blocksize)`;
  apfsck enforces `Σ extent.len == alloced_size`) and the
  decoded-but-dropped pattern (libfsapfs/apfs-fuse/go-apfs all decode
  the field, none surfaces it). v1 emission: regular+dstream →
  `Some(alloced_size)`; regular+decmpfs → fail closed (`None`,
  `not_claimed`); symlink → `0`; dir → `0`; else fail closed.
  Extent-reference tree stays out of scope for R2-A (it is the
  exclusive/shared/snapshot-retained prerequisite).
- `SR-020` opens R2-B: mines xnu, manpages, Apple support docs, and
  community writeups for the user-space APFS snapshot surface on
  macOS 13-14. Bottom line: read-only enumeration is unprivileged
  (`fs_snapshot_list`, `diskutil apfs listSnapshots`,
  `tmutil listlocalsnapshots`); every mutating call needs root + the
  DTS-issued private entitlement `com.apple.developer.vfs.snapshot`;
  unprivileged callers can only use `tmutil localsnapshot` (no caller-
  supplied name; TM-included volumes only) for create, and need root
  for `mount_apfs -s` to mount. Implication: the R2-B Rust integration
  must take a `--snapshot <mountpoint>` flag that defers to an
  already-mounted snapshot; the scanner does not assume snapshot-
  create privilege. EX-23 is a best-effort probe that scans an
  existing TM local snapshot if present and records a
  `blocked_on_privilege` summary otherwise.

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
