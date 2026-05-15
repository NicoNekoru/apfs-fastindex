# EX-16 SR-015 xfield replay

ID: EX-16
Title: SR-015 xfield replay
Date: 2026-05-14
Owner: Claude
Status: Executed
Result: `validated_sr_015_cursor_rule`
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

EX-13 modeled inode/drec xfield decoding as four scored candidate layouts.
SR-015 reviewed six converged open-source implementations and Apple's
reference and concluded those four layouts are an EX-13 probe modeling
artifact: there is one source-backed cursor rule. Values start immediately
after `xf_blob_t + xf_num_exts * x_field_t` and each value occupies
`round_up(x_size, 8)` bytes. EX-16 re-runs the EX-13 fixture under that
single rule, records every per-field number SR-015 cited (`xf_num_exts`,
`xf_used_data`, metadata-table length, per-field `x_size` and padded length,
decoded values, unused trailing bytes), and asserts that both
`xf_used_data == sum(round_up(x_size, 8))` and the same namespace +
logical-size oracle rows still match. Pass condition for this gate is the
combined assertion. Failing means SR-015 needs an addendum or a per-record
exception is required.

## Question

- Does the SR-015 single-cursor xfield rule
  (`cursor += round_up(x_size, 8)` starting immediately after the metadata
  table) decode every required xfield in the EX-13 proof fixture
  unambiguously, and does it satisfy
  `xf_used_data == sum(round_up(x_size, 8))` for every required xfield while
  preserving namespace and logical-size oracle parity?

## Hypotheses

- Hypothesis A `validated_sr_015_cursor_rule`: yes. Every required inode and
  directory-entry xfield decodes deterministically under the single cursor
  rule, every `xf_used_data` matches the padded value-area byte count, and
  every namespace / ordinary logical-size oracle row still matches. The rule
  is safe to encode in Rust (in a later experiment).
- Hypothesis B `xfield_rule_insufficient`: at least one required record has
  `xf_used_data != sum(round_up(x_size, 8))`, or under the single cursor the
  decoded values disagree with the mounted oracle (sparse / clone / hard
  link). The rule needs either an SR-015 addendum or a fixture-specific
  exception before any Rust commitment.

## Environment

- macOS version: captured live in `artifacts/generated/environment.json`.
- APFS source: the EX-13 proof fixture rebuilt deterministically via
  `apfs_fastindex.poc_fixture.build_proof_fixture` (same operations as EX-13).
- Mounted phase: fixture creation and POSIX oracle capture.
- Raw phase: detached image reattached `-nomount -readonly`.
- Out of scope: encryption, snapshots, volume-group semantics,
  physical/shared/exclusive accounting, compression precedence beyond
  ordinary uncompressed dstream sizes (SR-017 territory).

## Oracle

- Mounted/POSIX traversal of the same APFS volume owns paths, entry types,
  file identity, symlink targets, hard-link grouping, and ordinary
  `logical_size`. This is the EX-13 oracle reused unchanged; what changes is
  the xfield cursor rule used by the Python decoder.
- `xf_used_data` is its own structural oracle: per SR-015 it should equal the
  sum of `round_up(x_size, 8)` over the record's xfields.

## Setup

1. Capture environment manifest.
2. Build the EX-13 proof fixture using `build_proof_fixture` (rename, move,
   hard link, sparse file, clone, append, symlink — same operations EX-13
   used so the resulting xfield set matches what SR-015 reasoned about).
3. Capture the mounted POSIX oracle (paths, types, file identity, symlink
   targets, logical_size per file).
4. Detach and reattach `-nomount -readonly`. Acquire the raw container path.
5. Run the Rust scanner once for `selected_checkpoint` context (EX-15 gate).

## Probe Steps

1. Read the raw container, parse FS-tree records under the Rust-selected
   checkpoint / volume / root context.
2. For every inode and drec record with a non-empty xfields tail, run the
   SR-015 single-cursor decoder and record: `xf_num_exts`, `xf_used_data`,
   metadata-table length, per-field `(x_type, x_flags, x_size,
   padded_length, decoded_value)`, total padded value-area length, unused
   trailing bytes inside the xfield blob.
3. For each required xfield, assert `xf_used_data == sum(padded_length)`.
4. Reconstruct paths, types, file identity, symlink targets, hard-link
   groups, and per-file logical_size from the decoded xfields plus the
   record bodies. Compare against the mounted POSIX oracle.
5. Save artifacts and one `summary.json` with verdict and per-record
   structural counts.

## Expected Observations

### If Hypothesis A is true

- Every inode with xfields has `xf_used_data == sum(round_up(x_size, 8))`.
- Every required xfield decodes to a value that matches the mounted oracle
  (inode name, dstream size for ordinary files, sparse-bytes inside the
  oracle's logical size, dir_rec sibling ID consistent with sibling_map).
- Namespace and ordinary logical-size oracle parity: zero mismatches.

### If Hypothesis B is true

- At least one required record has `xf_used_data` mismatch, decode error, or
  an oracle-incompatible decoded value. The record's raw bytes and metadata
  are recorded for SR-015 follow-up.

## Observed Results

- Rebuilt the EX-13 proof fixture deterministically (same operations as
  `build_proof_fixture`: src/, dst/, rename, move, hard link, sparse 1 MiB,
  clone, append, symlink).
- `selected_checkpoint` from the patched Rust path: xid `14`, block size
  `4096`. Volume `1026`, root tree at paddr resolved through volume OMAP.
- Decoded `14` records carrying xfields under the **single** SR-015 cursor
  rule (`cursor = metadata_end; cursor += round_up(x_size, 8)`):

  | oid | family   | xf_num_exts | x_size set                       | xf_used_data | sum(round_up(x_size,8)) |
  | --- | -------- | ----------- | -------------------------------- | ------------ | ----------------------- |
  | 2   | inode    | 1           | 5                                | 8            | 8                       |
  | 3   | inode    | 1           | 12                               | 16           | 16                      |
  | 16  | inode    | 1           | 4                                | 8            | 8                       |
  | 17  | inode    | 1           | 4                                | 8            | 8                       |
  | 17  | dir_rec  | 1           | 8                                | 8            | 8                       |
  | 17  | dir_rec  | 1           | 8                                | 8            | 8                       |
  | 18  | inode    | 1           | 11                               | 16           | 16                      |
  | 19  | inode    | 2           | 15, 40                           | 56           | 56                      |
  | 20  | inode    | 2           | 10, 40                           | 56           | 56                      |
  | 23  | inode    | 3           | 11, 40, 8 (NAME+DSTREAM+SPARSE)  | 64           | 64                      |
  | 24  | inode    | 2           | 40, 10                           | 56           | 56                      |
  | 26  | inode    | 1           | 9                                | 16           | 16                      |
  | 27  | inode    | 2           | 17, 40                           | 64           | 64                      |
  | 28  | inode    | 2           | 17, 40                           | 64           | 64                      |

- Structural pass: `xf_used_data == sum(round_up(x_size, 8))` for all
  `14/14` records. No unused trailing bytes inside any xfield blob.
- Oracle pass: namespace and per-file logical-size reconstruction (built
  using the SR-015-decoded `INO_EXT_TYPE_DSTREAM.size`,
  `INO_EXT_TYPE_NAME`, `INO_EXT_TYPE_SPARSE_BYTES`, and
  `DREC_EXT_TYPE_SIBLING_ID`) matches the mounted POSIX oracle with zero
  missing/unexpected paths and zero mismatches.
- Per-record oracle constraint checks (`INO_EXT_TYPE_NAME` ∈ dir_rec or
  sibling_link names for the same inode, `INO_EXT_TYPE_DSTREAM.size` equals
  the mounted `st_size`, `INO_EXT_TYPE_SPARSE_BYTES` ≤ logical size for
  sparse files, `DREC_EXT_TYPE_SIBLING_ID -> file_id` consistent with
  `sibling_map`) all passed with 0 failures.
- Verdict: `validated_sr_015_cursor_rule`. The SR-015 single cursor rule is
  sufficient for the EX-13 proof fixture, including the sparse-file inode
  that EX-13 had previously needed candidate scoring to resolve.

## Artifacts Saved

- `artifacts/probe_ex16.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex16-fixture-operations.json`
- `artifacts/generated/ex16-mounted-posix-oracle.json`
- `artifacts/generated/ex16-rust-context.json`
- `artifacts/generated/ex16-xfield-replay.json`
- `artifacts/generated/ex16-comparison.json`
- `artifacts/generated/summary.json`

## Interpretation

- The EX-13 "split across selected candidate layouts" was a probe modeling
  artifact, exactly as SR-015 predicted. Under the single cursor rule the
  evidence is unambiguous: every required xfield in the proof fixture
  decodes to the same field set EX-13 had to score across four candidates.
- `xf_used_data` doubles as a structural oracle: it is a per-record exact
  predictor of `sum(round_up(x_size, 8))`. Any record where the equality
  fails is a fail-closed signal SR-016 should reference.
- The sparse-file inode (`oid=23`) has the three-xfield case
  (NAME=11 → padded 16; DSTREAM=40 → padded 40; SPARSE_BYTES=8 → padded 8;
  `xf_used_data=64=16+40+8`) that previously needed candidate scoring; it
  now decodes cleanly under the cursor rule.

## What This Rules Out

- Rules out hypothesis B `xfield_rule_insufficient` on the EX-13 fixture
  shape: no per-record exception is required to decode the v1-relevant
  xfields.
- Does not yet rule out per-record exceptions in fixtures with directory
  entries that have non-multiple `x_size` xfields preceding `DSTREAM`,
  records with non-v1 xfield types (e.g. `INO_EXT_TYPE_FS_UUID`,
  `INO_EXT_TYPE_PURGEABLE_FLAGS`), or records whose `xf_used_data` is
  recorded by a different APFS source version. EX-17 will probe such
  malformed/edge cases as fail-closed signatures.

## Impact on RLs

- RL-03: promote the SR-015 single cursor rule from `Source Review` to
  `Observation backed by an executed probe on the proof fixture`. Rust
  body-field decoding may now implement this rule, gated by EX-18's
  byte-for-byte diff against the Python field dump.
- RL-07: ordinary uncompressed logical-size source for files in the
  detached-image allowlist is `INO_EXT_TYPE_DSTREAM.size`. SR-017 still
  owns the broader precedence (compression, sparse delta, decmpfs); the
  dstream half is no longer the open question.
- RL-10: the xfield structural assertion (`xf_used_data` equality) is a
  validation oracle in its own right. Future body probes must record it
  per inode/dir_rec and fail closed on mismatch.
- RL-13: no SR-015 addendum required for the EX-13 fixture; SR-016 picks up
  the malformed/edge fail-closed signatures as already planned.

## Next Exact Step

- Proceed to EX-17 synthetic fail-closed record bodies (SR-016) — same
  cursor rule must hard-stop on duplicate xfields, out-of-bounds
  `x_size`, malformed names, type mismatches, and `xf_used_data`
  disagreements.
