# EX-15 Block-1031 context replay

ID: EX-15
Title: Block-1031 context replay
Date: 2026-05-14
Owner: Claude
Status: Executed
Result: `validated_fs_tree_internal_oid_resolution_gap`
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-03 FS Tree Topology and Required Records
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

Hypothesis (c) — a Rust validation bug — holds. The EX-14 blocker is not
stale-checkpoint selection, a missing checkpoint-map traversal rule, or a
malformed image: it is `fs_records::walk_fs_node` treating FS-tree internal
node values as **physical paddrs** when they are actually **virtual OIDs**
that must be resolved through the volume OMAP at the selected XID.

The retained EX-15 fixture reproduces the EX-14 signature exactly: four
checkpoint candidates (xids 18-21), all passing SR-005 / SR-007 / SR-006
validation in Python; `fsck_apfs -n` returns clean; `go-apfs identitydump`
succeeds end-to-end. The single failure surface is Rust's FS-tree internal
walk: at xid 21 the volume OMAP maps `root_tree_oid=1028 -> paddr=501`; that
root node is `level=1, is_root=true, is_leaf=false, nkeys=2` with internal
values `1031` and `1030`. The current Rust code reads block `1031` directly,
which is an unwritten (all-zero) block, and the Fletcher-64 mismatch surfaces
as the EX-14 abort.

Block 1031 is the **virtual OID** of the second FS-tree child; the volume
OMAP maps `oid=1031 -> paddr=505 (xid=20)`. Once the FS-tree walker resolves
that OID through the volume OMAP, the EX-14 blocker resolves.

Fix landed in `crates/apfs-fastindex/src/fs_records.rs`: the FS-tree walker
now takes the volume OMAP resolver and performs `(child_oid, max_xid)`
lookups before reading any child node. Re-running Rust against the retained
fixture publishes `selected_checkpoint` with `fs_records_dumped_count = 1`.

## Question

- For the EX-14 fixture shape, what role does block 1031 play in the selected
  checkpoint, and which of (a) stale-checkpoint selection, (b) a missing
  checkpoint-map / data-ring traversal rule, (c) a Rust `validate_object_block`
  bug, or (d) a genuinely malformed source explains the checksum mismatch?

## Hypotheses

- Hypothesis (a) `stale_checkpoint_selection`: the chosen NXSB (highest xid)
  references a `nx_omap_oid` or OMAP B-tree node whose data area was not fully
  durable when the image was detached; falling back to the next-highest valid
  checkpoint produces a self-consistent context.
- Hypothesis (b) `missing_traversal_rule`: block 1031 belongs to a checkpoint
  ring or ephemeral path the current Rust walker is not consulting (e.g., the
  checkpoint data area, an ephemeral object mapping looked up through the
  checkpoint map, or a sibling structure required to read the container OMAP).
- Hypothesis (c) `rust_validation_bug`: the bytes at block 1031 do satisfy
  Fletcher-64 / object-header expectations when validated against the right
  rule, but the current Rust validation gate is asking the wrong question
  (wrong storage class, wrong max-xid, wrong oid-vs-paddr assumption).
- Hypothesis (d) `malformed_source`: block 1031's bytes are genuinely
  inconsistent with any APFS rule under any candidate checkpoint; the image
  itself is malformed and the fail-closed boundary is correct (record signature
  in SR-016 register).

## Environment

- macOS version: captured live in `artifacts/generated/environment.json`.
- APFS source: generated unencrypted APFS `.dmg` rebuilt from EX-14
  operations; detached for the raw phase.
- Mounted phase: fixture creation only.
- Raw phase: detached image reattached `-nomount -readonly`.
- Out of scope: live startup disks, encryption, snapshots, merged volume-group
  semantics, physical/shared/exclusive accounting, compression precedence.

## Oracle

- `fsck_apfs -n` against the detached image runs Apple's own consistency
  checker for the same container. A clean `fsck_apfs -n` says "the image is
  internally consistent at *some* checkpoint" and is the strongest negative
  evidence against hypothesis (d).
- `go-apfs identitydump` (the converged third-party reader bundled under
  `EX-06-identity-tracking/artifacts/identitydump`) selects its own checkpoint
  and OMAP context. If it succeeds where Rust fails, that says some checkpoint
  is reachable on this image and pins the divergence to Rust's selection
  policy or walk.
- Python raw-byte replay of obj-header validation uses the same SR-005 / SR-007
  rules expressed by the Rust crate, but lets us evaluate every NXSB candidate
  (not just the highest) and surface intermediate state.

## Setup

1. Reuse the EX-14 `build_variant_corpus` operations to rebuild a fixture with
   the same directories, renames, hard link, sparse files, clone, append, and
   symlink (no user xattrs, no Unicode/case probes, no compression — same as
   EX-14 retained).
2. After fixture creation and POSIX oracle capture, `hdiutil detach` the
   mounted volume, then `hdiutil attach -plist -nomount -readonly` to obtain
   a stable raw container path.

## Probe Steps

1. Capture environment manifest.
2. Rebuild the EX-14 fixture and snapshot the mounted/POSIX oracle.
3. Detach, reattach `-nomount -readonly`, normalize to `/dev/rdiskN`.
4. Run `fsck_apfs -n` against the raw container; capture full stdout/stderr.
5. Read block zero, parse the descriptor area, enumerate all NXSB candidates.
6. For each candidate (descending xid) replay the header + checkpoint-map +
   container OMAP open + container OMAP B-tree walk + volume superblock decode
   step by step. Record (success | first-failing-block | failure-reason).
7. For every failing block found in step 6 (definitely block 1031 for xid=20,
   but any other block surfaced by lower xids too), dump the raw 4096 bytes,
   compute Fletcher-64 in Python, parse the `obj_phys_t` header, and decide
   which role the block was supposed to play.
8. Run `go-apfs identitydump` against the same raw container; capture its
   chosen checkpoint and any roots it reports.
9. Run the current Rust `apfs-fastindex-scan` binary for parity with the
   EX-14 saved artifact.
10. Decide which hypothesis (a)-(d) holds; if (a)-(c), state the exact rule
    or code change to make.

## Expected Observations

### If Hypothesis (a) is true

- xid 20 fails at block 1031; xid 19 (or 18 / 17) reaches `selected_checkpoint`
  and decodes the volume superblock cleanly.
- `fsck_apfs -n` returns clean.
- `go-apfs identitydump` succeeds (because it likely picks a different
  checkpoint).

### If Hypothesis (b) is true

- Block 1031 carries an APFS-valid object header (Fletcher-64 matches) but
  isn't reachable through the current Rust walk path. The right next step is a
  named SR-* tightening the traversal rule, plus a Rust unit test.

### If Hypothesis (c) is true

- The block satisfies Fletcher-64 in Python but Rust reports otherwise, or the
  block's storage flag / oid / xid expectation in the current code is too tight
  for the role it plays. Patch and unit-test.

### If Hypothesis (d) is true

- The bytes at block 1031 do not satisfy Fletcher-64 for any candidate
  checkpoint and `fsck_apfs -n` also flags the volume. Record the signature in
  SR-016 (record-body fail-closed boundary) as a documented fail-closed source.

## Observed Results

- Deterministic rebuild produced the same EX-14 signature on the **unpatched**
  Rust scanner: four valid NXSB candidates at xids 18-21 (initial run) or
  17-20 (second run with the same operations), highest valid candidate
  selected, then `APFS object validation failed: checksum mismatch at block
  1031` before `selected_checkpoint`.
- `fsck_apfs -n` returned `0` on the raw container; no warnings.
- `go-apfs identitydump` (via the bundled helper) returned a fully populated
  namespace for the same container with no observer errors.
- Python `validate_object_header` replay (SR-005 + SR-007 rules in
  `probe_ex15.py`) accepted every NXSB candidate, every checkpoint-map block,
  every container OMAP-phys + B-tree node, every volume superblock, every
  volume OMAP-phys + B-tree node, every FS-tree root header, and every FS-tree
  internal child once the **virtual OID** was resolved through the volume OMAP.
- Block `1031` raw bytes: 4096 bytes of `0x00`, stored checksum `0x00..00`,
  computed Fletcher-64 `0xffffffffffffffff`. The block is unallocated.
- The FS-tree root at paddr `501` (volume OMAP entry `oid=1028 -> paddr=501,
  xid=19`) is `level=1, is_root=true, is_leaf=false, nkeys=2`. The two
  internal entries have 8-byte values `1031` and `1030`. Both values are
  **virtual OIDs**, not paddrs. The volume OMAP maps `oid=1031 -> paddr=505
  (xid=20)` and `oid=1030 -> paddr=497 (xid=19)`.
- Root cause: `crates/apfs-fastindex/src/fs_records.rs::walk_fs_node` treated
  each internal-node value as a direct paddr, so it read block 1031 (the bare
  OID misinterpreted as a paddr) and tripped the Fletcher-64 gate.
- Fix landed: `walk_fs_node` now takes `&OmapResolver` (the volume OMAP), and
  for every internal entry it performs `volume_omap.lookup(reader, block_size,
  child_oid, max_xid)` before reading the child node. A typed hard stop is
  returned when the OMAP has no mapping for the child OID at the selected XID.
  `dump_fs_records` propagates the OMAP resolver from the caller in `lib.rs`.
- Two regression tests added in `fs_records::tests`:
  - `fs_tree_internal_value_is_virtual_oid_resolved_via_omap` builds a
    synthetic 8-block image where the FS-tree root's internal value is the
    OID `1030`. With the patch, the walker reads paddr 7 via the volume OMAP
    and counts the DIR_REC leaf record correctly. Without the patch, it would
    short-read on block 1030.
  - `fs_tree_internal_oid_missing_from_omap_is_hard_stop` asserts that an
    internal value pointing at an OID with no OMAP mapping returns a typed
    `InvalidObject` error mentioning the missing OID, instead of silently
    skipping or attempting a paddr fallback.
- After the patch, re-running the Rust scanner against the retained EX-15
  fixture publishes `selected_checkpoint` at xid 21, with
  `native_validation.fs_records_dumped_count = 1`, volume status
  `supported`, and FS-record family counts: 20 inode, 17 xattr, 2 sibling
  link, 12 dstream_id, 18 file_extent, 21 dir_rec, 2 sibling_map (total
  `leaf_record_count = 92` across `leaf_node_count = 2` and
  `index_node_count = 1`).
- `probe_ex15.py` reports `verdict = rust_now_selects_context` (it rebuilds
  the fixture each run and exercises the patched Rust binary).

## Artifacts Saved

- `artifacts/probe_ex15.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex15-fixture-operations.json`
- `artifacts/generated/ex15-mounted-posix-oracle.json`
- `artifacts/generated/ex15-rust-context.json`
- `artifacts/generated/ex15-candidate-replay.json`
- `artifacts/generated/ex15-failing-blocks.json`
- `artifacts/generated/ex15-go-apfs.json`
- `artifacts/generated/ex15-fsck.json`
- `artifacts/generated/summary.json`

## Interpretation

- The EX-14 blocker was hypothesis (c): a Rust traversal rule was missing,
  not stale-checkpoint selection or a malformed image. FS-tree internal
  values are virtual OIDs that must be resolved through the volume OMAP at
  the selected XID; the prior Rust code read them as direct paddrs.
- The bug was previously masked because the EX-10 / EX-11 / EX-12 proof
  fixtures had FS-trees small enough to fit in a single leaf node (no
  internal entries to follow). The EX-14 fixture is the first one that
  causes the FS-tree to grow a real internal level.
- This finding does **not** change checkpoint selection policy. All four
  EX-15 NXSB candidates pass SR-005 / SR-007 strict validation in Python; a
  recorded-fallback rule remains a separate, future tightening question.

## What This Rules Out

- Rules out hypothesis (a) `stale_checkpoint_selection` as the cause of the
  EX-14 blocker for this fixture shape: Python replay confirmed all four
  candidates were self-consistent at SR-005 / SR-006 / SR-007 strictness.
- Rules out hypothesis (b) `missing_checkpoint_map_or_data_ring_rule`: the
  descriptor ring and ephemeral / container-OMAP paths were never the
  problem. Block 1031 was never reachable via any descriptor / data area
  walk.
- Rules out hypothesis (d) `malformed_source_signature`: `fsck_apfs -n` is
  clean and `go-apfs identitydump` succeeds. The image is well-formed.

## Impact on RLs

- RL-01: no policy change. Recorded under "candidate scanning is independent
  of FS-tree internal traversal correctness."
- RL-02: extends OMAP-resolver responsibilities. The volume OMAP resolver is
  now used twice per FS-tree walk: once to resolve `root_tree_oid`, then
  again for every internal-node entry. Lower-bound `(oid, max_xid)`
  semantics from SR-006 are reused unchanged.
- RL-03: tightens the FS-tree topology contract — internal-node values are
  virtual OIDs, not paddrs, and require an OMAP resolution step before any
  read. Add this to the SR-008 contract as an Observation backed by
  `linux-apfs-rw` / `go-apfs` traversal behavior.
- RL-10: adds the EX-15 fixture-replay datapoint (positive: synthetic
  resolution test; negative: hard-stop on missing OMAP mapping).
- RL-13: no new fail-closed signature; the EX-14 signature was a Rust bug.

## Next Exact Step

- Re-run EX-14 xfield-layout variant against the patched crate (now that
  `selected_checkpoint` publishes successfully) and proceed to EX-16
  (SR-015 xfield replay) per the immediate work order.
