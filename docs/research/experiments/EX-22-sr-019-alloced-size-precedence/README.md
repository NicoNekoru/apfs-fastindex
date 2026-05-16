# EX-22 SR-019 allocated-size precedence fixture

ID: EX-22
Title: SR-019 allocated-size precedence fixture
Date: 2026-05-16
Owner: Claude
Status: Planned
Result: Pending
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

SR-019 picks `j_dstream_t.alloced_size` as the only publishable
per-file allocated-bytes candidate and assigns each case class a step
in a fail-closed precedence: regular+dstream → `Some(alloced_size)`;
regular+decmpfs → fail closed (`None`, `not_claimed`); symlink → `0`;
directory → `0`; everything else → fail closed. EX-22 reuses the
EX-19 same-run fixture (ordinary, sparse, clone, hard link, symlink,
`ditto --hfsCompression`) and asserts that the SR-019 precedence
reproduces the macOS public oracle `st_blocks * 512` per inode for
the cases the rule emits, while the decmpfs case is recorded as
fail-closed without being treated as a mismatch.

The probe also captures the sum of `j_file_extent_val_t` lengths per
inode as a *diagnostic* column. SR-019 records that linux-apfs-rw
(kernel write path) and apfsprogs (apfsck) disagree on whether
`Σ extent.len == alloced_size`; capturing both lets the experiment
record any divergence on macOS-created images without basing the
emitted column on it.

## Question

- For ordinary, sparse, cloned, hard-linked, symlink, and
  `ditto --hfsCompression` files on a detached APFS fixture, does
  the SR-019 precedence
  (regular+dstream → `j_dstream_t.alloced_size`;
  regular+decmpfs → fail closed;
  symlink → 0;
  directory → 0)
  reproduce mounted POSIX `st_blocks * 512` for every entry the rule
  emits, while leaving the decmpfs case explicitly unclaimed?

## Hypotheses

- Hypothesis A `validated_sr_019_precedence`: yes. Every case class
  that the rule emits matches the oracle exactly, and the decmpfs
  case lands in `not_claimed` without producing a row mismatch.
- Hypothesis B `partial_validated`: at least one
  non-compressed case class diverges from `st_blocks * 512`. The
  probe records per-inode which step picked, the picked value, and
  the oracle, so SR-019 can be amended for that case class without
  blocking the others.
- Hypothesis C `extent_sum_divergence`: SR-019 step 1 matches the
  oracle for every non-compressed case, but the diagnostic
  `Σ j_file_extent_val_t.len` *also* matches it across all such
  cases. This would weaken SR-019's preference for `alloced_size`
  over the extent sum on macOS-produced fixtures (Linux's writer
  disagrees with apfsck — macOS may not). Records the divergence
  but does not change the chosen v1 emission.

## Environment

- macOS version captured in `artifacts/generated/environment.json`.
- APFS source: generated `.dmg` containing one ordinary, sparse,
  cloned, hard-linked, symlink, and compressed file. Identical
  fixture shape to EX-19.
- Mounted phase: fixture creation and POSIX oracle capture
  (`st_blocks`, `st_size`, `UF_COMPRESSED`).
- Raw phase: detached image, reattached `-nomount -readonly`.
- Out of scope: exclusive bytes, shared bytes, snapshot-retained
  bytes, decompressed-on-disk reconciliation, container-level used
  / free accounting.

## Oracle

- Mounted POSIX `st_blocks * 512` is the per-inode oracle for the
  cases that SR-019 emits a value for. SR-019's review pinned this
  as the only oracle for which case-by-case validation makes sense
  on macOS, because no internal-field equation has been agreed
  across readers.
- The Rust crate's `FsRecordDump.records` (EX-18 parity) is the
  raw-side parser; per-inode `dstream.alloced_size` is read from
  the inode body's xfields, no separate Python parser is needed.
- For the decmpfs case the oracle is "the SR-019 step picks
  `fail_closed` and the row is **omitted** from the mismatch
  table; SR-019 documents this as `not_claimed`." A run that
  *also* reports `st_blocks * 512 == 0` for the decmpfs row would
  be a strong observation; runs where it is non-zero still pass
  Hypothesis A.

## Setup

1. Capture environment manifest.
2. Build a fresh APFS image identical in shape to EX-19. Inside it:
   - one ordinary file of known size
   - one sparse file with hole + tail data
   - clone via `cp -c`
   - hard link via `os.link`
   - symlink
   - one ordinary file copied via `ditto --hfsCompression`
3. Capture mounted POSIX `st_blocks`, `st_size`, `UF_COMPRESSED`,
   and xattr inventory for each entry.
4. Detach and reattach `-nomount -readonly`.
5. Run the Rust scanner; collect `FsRecordDump.records` and the
   FS-tree family histogram.

## Probe Steps

1. For each mounted entry, capture `st_blocks` and compute
   `st_blocks * 512` (oracle).
2. Group Rust records by `object_id`. For each inode, capture:
   - `internal_flags`, `has_uncompressed_size`,
     `uncompressed_size`, `mode`
   - `dstream.alloced_size`, `dstream.size`
   - `sparse_bytes`
3. For every inode, walk the family histogram and record the
   *count* of `file_extent` (raw_type 0x8) and
   `extent_reference` (raw_type 0x2) records associated with the
   inode's `object_id` from `family_counts`. (Body decoding for
   raw_type 0x8 / 0x2 is not in the Rust allowlist yet — counts are
   captured as diagnostic only.)
4. For every xattr the inode owns, capture name + body
   (embedded/stream flag, payload_hex) so the precedence rule can
   identify decmpfs and symlink cases.
5. Apply SR-019 precedence per inode and compare to the oracle.
6. Save per-entry breakdown so SR-019 can be cited line-by-line.

## Expected Observations

### If Hypothesis A is true

- For ordinary / sparse / clone / hard-link rows: picked value
  equals `st_blocks * 512` exactly.
- For symlink rows: picked value is `0` and oracle is `0`.
- For compressed rows: SR-019 step returns `fail_closed`; the row
  is recorded in `fail_closed_rows` and excluded from
  `mismatches`.

### If Hypothesis B is true

- At least one non-compressed row's `alloced_size` diverges from
  `st_blocks * 512`. The probe records the case class, picked
  value, oracle value, and the diagnostic
  `Σ j_file_extent_val_t.len`.

### If Hypothesis C is true (orthogonal, observational)

- Every non-compressed row's `alloced_size` equals the oracle AND
  equals the diagnostic extent sum. The probe records this
  convergence (SR-019's preference for the dstream field over the
  extent sum is justified by linux-apfs-rw's actual reader, not by
  on-disk equality on the proof fixture).

## Observed Results

_(filled in after the run)_

## Artifacts Saved

- `artifacts/probe_ex22.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex22-fixture-operations.json`
- `artifacts/generated/ex22-mounted-posix-oracle.json`
- `artifacts/generated/ex22-rust-records.json`
- `artifacts/generated/ex22-precedence-table.json`
- `artifacts/generated/summary.json`

## Interpretation

_(filled in after the run)_

## What This Rules Out

_(filled in after the run; expected scope:)_

- Does not rule out per-inode mismatches on Gate-2 source classes
  (live boot disk, encryption, snapshot-assisted, boot-root).
- Does not validate exclusive, shared, or snapshot-retained
  accounting. SR-019 explicitly excludes those.
- Does not validate `Σ file_extent.len == alloced_size` on
  macOS-created images; the probe captures *counts* but does not
  decode `j_file_extent_val_t` bodies, so it cannot record the
  byte-level sum without a follow-up EX-22b that extends the Rust
  body decoder.

## Impact on RLs

- RL-07: a positive verdict promotes SR-019's precedence to the
  rule the Rust namespace emitter implements for the
  `allocated_size` column. A `partial_validated` verdict identifies
  exactly which case classes are safe to emit and which need a
  follow-up SR / EX.
- RL-10: the per-entry breakdown becomes the regression artifact
  future allocated-size changes must clear.
- RL-13: decmpfs handling is the fail-closed gate; if the probe
  finds the decmpfs row's `st_blocks * 512` is non-zero (a
  resource-fork case, say), the result still passes Hypothesis A
  *and* records that fail-closed was the right v1 posture.

## Next Exact Step

- Run the probe end-to-end; on Hypothesis A, proceed to the Rust
  slice (gate `allocated_size: Option<u64>` into `NamespaceEntry`
  with the fail-closed precedence wired through, extend
  `--summary`'s `not_claimed` register, and update the smoke test).
- On Hypothesis B, do **not** promote `alloced_size` into the
  Rust crate. Scope a follow-up `EX-22b` per the divergent case
  class.
