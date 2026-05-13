# EX-14 Xfield layout variant

ID: EX-14
Title: Xfield layout variant
Date: 2026-05-13
Owner: GPT-5
Status: Executed
Result: `oracle_inconclusive`; the retained fixture blocked before raw body decode
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

`EX-13` validated Python raw-byte FS-record body decoding on one proof fixture,
but xfield layout selection still depended on scored candidates. `EX-14` tried
to run the required second fixture-variant pass before Rust body decoding. The
retained run did not reach xfield comparison: the Rust context provider found
valid checkpoint candidates, then aborted with `APFS object validation failed:
checksum mismatch at block 1031` before it could publish `selected_checkpoint`
or an FS-tree root.

Rust FS-record body decoding remains blocked. The next exact step is a focused
checkpoint/OMAP-context replay for this fixture shape, not product
`NamespaceEntry` emission.

## Question

- Can a second detached APFS fixture variant resolve xfield layout candidates
  deterministically enough to encode Rust body-field dumping?

## Hypotheses

- Hypothesis A: A same-run fixture variant with additional xfield-bearing inode
  shapes leaves no product-critical unresolved xfield candidates after
  cross-record and POSIX logical-size constraints are applied.
- Hypothesis B: The variant still leaves product-critical candidate ambiguity,
  or an earlier parser gate blocks the xfield comparison.

## Environment

- macOS/tool manifest: `artifacts/generated/environment.json`
- APFS source: generated unencrypted APFS `.dmg`
- Mounted phase: fixture creation and POSIX oracle capture
- Raw phase: detached image reattached `-nomount -readonly`
- Out of scope: live startup disks, encryption, snapshots, merged volume-group
  semantics, physical/shared/exclusive accounting, and compression precedence

The probe required approval outside the normal filesystem sandbox because
sandboxed `hdiutil create` returned `Device not configured`.

## Oracle

- Mounted/POSIX traversal owns visible paths, entry types, stable file identity,
  symlink targets, ordinary logical sizes, and sparse candidates.
- Rust owns the selected checkpoint/root context. Without a usable
  `selected_checkpoint`, Python body decoding must not proceed.
- Python raw-body decoding and xfield candidate scoring would own the layout
  comparison only after the Rust context is available.

## Setup

The retained fixture intentionally stayed close to the EX-13 proof shape while
adding xfield-relevant variants:

- directories: `src`, `dst`, `names`, `sparse`
- rename and move into `dst/moved.txt`
- hard link `dst/hard.txt`
- two sparse files, including `sparse/sparse-unaligned-name.bin`
- clone `dst/moved.txt -> dst/clone.txt`, followed by source append
- symlink `dst/link.txt -> moved.txt`
- varied filename lengths under `names/`

Broader user-xattr, Unicode/case, and compression probes were skipped in the
retained run after earlier attempts hit the same Rust context blocker.

## Probe Steps

1. Create and mount the APFS image.
2. Build the retained xfield-layout fixture and capture the mounted/POSIX oracle.
3. Detach and reattach the image `-nomount -readonly`.
4. Run the Rust scanner for selected checkpoint/root context.
5. If Rust publishes context, parse FS-tree record bodies with the EX-13 Python
   helpers and compare xfield layout candidates.
6. Save generated artifacts under `artifacts/generated/`.

## Expected Observations

### If Hypothesis A is true

- Rust publishes `selected_checkpoint` and root-tree context.
- Python raw reconstruction matches the mounted namespace/logical-size oracle.
- `unresolved_xfield_record_count = 0`, or remaining alternatives decode to
  identical values.

### If Hypothesis B is true

- The probe records the mismatch or earlier blocker and keeps Rust body decoding
  closed.

## Observed Results

- Verdict: `oracle_inconclusive`.
- Mounted/POSIX oracle for `EX14CI` was saved:
  - entries: `16`
  - directories: `5`
  - files: `10`
  - symlinks: `1`
  - hard-link paths: `dst/hard.txt`, `dst/moved.txt`
  - sparse candidates: `sparse/sparse-1m.bin`,
    `sparse/sparse-unaligned-name.bin`
  - unique-inode logical total: `2097323`
- Rust scanner reached the source gate and checkpoint descriptor scan:
  - block size: `4096`
  - descriptor base: `1`
  - descriptor blocks: `8`
  - valid checkpoint candidates: `4`
  - highest XID: `20`
- Rust did not return `selected_checkpoint`.
- Rust `scan_state.validation_gaps` recorded:
  - `native dump aborted: APFS object validation failed: checksum mismatch at block 1031`
  - `checkpoint map validation not completed`
  - `container OMAP resolution not completed`
  - `volume superblock decoding not completed`
  - `FS-tree record dumping not completed`
- Because selected checkpoint/root context was unavailable, namespace diff,
  xattr diff, raw body dump, and xfield layout comparison were not run.
- A same-session rerun of `EX-13` still produced
  `validated_native_record_body_contract`, so this is not a blanket failure of
  the proof fixture path.

## Artifacts Saved

- `artifacts/probe_ex14.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex14ci-fixture-operations.json`
- `artifacts/generated/ex14ci-mounted-posix-oracle.json`
- `artifacts/generated/ex14ci-rust-context.json`
- `artifacts/generated/ex14ci-native-record-body-dump.json`
- `artifacts/generated/ex14ci-xfield-layout-summary.json`
- `artifacts/generated/ex14ci-comparison.json`
- `artifacts/generated/summary.json`

## Interpretation

- Observation: The immediate xfield-layout question remains unanswered because
  the Rust context provider failed first.
- Observation: The current native path is validated on the EX-13 proof fixture
  but not yet robust across this slightly denser fixture variant.
- Hypothesis: The next blocker is in checkpoint-context/object validation for
  the selected state, not in FS-record body decoding itself.

## What This Rules Out

- It rules out moving directly from EX-13 into Rust body decoding on the
  assumption that the context provider is stable across the next fixture variant.
- It does not rule out the EX-13 xfield candidate policy, because no xfield
  records were decoded in this run.

## Impact on RLs

- `RL-03`: body-field decoding remains behind a stable checkpoint/OMAP/root
  context for the variant fixture.
- `RL-06`: product namespace rows remain blocked; no path reconstruction claim
  follows from this run.
- `RL-07`: logical-size body decoding remains blocked for the variant; sparse
  physical/shared/exclusive accounting remains out of scope.
- `RL-10`: adds an inconclusive same-run body-parser fixture and preserves the
  context-provider failure as first-class evidence.
- `RL-13`: adds a compatibility/fail-closed data point: a detached, unencrypted
  fixture can pass descriptor scanning but fail native context validation before
  record bodies.

## Next Exact Step

- Create a focused checkpoint-context successor for the EX-14 fixture shape that
  preserves the offending context around block `1031`, determines whether the
  failure is stale checkpoint selection, a missing checkpoint-map/data-ring rule,
  or a Rust validation bug, and only then rerun the xfield layout variant pass.
