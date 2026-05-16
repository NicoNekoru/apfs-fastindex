# EX-22 SR-019 allocated-size precedence fixture

ID: EX-22
Title: SR-019 allocated-size precedence fixture
Date: 2026-05-16
Owner: Claude
Status: Executed
Result: `partial_validated_sr_019_alloced_size`
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

Per-inode breakdown (5 emit-rows + 1 fail-closed row; hard.txt
shares ordinary.txt's inode and is correctly absorbed into the
per-inode rule):

| path           | inode | kind       | picked                  | picked value | st_blocks×512 | match |
| -------------- | ----- | ---------- | ----------------------- | ------------ | ------------- | ----- |
| ordinary.txt   | 16    | regular    | `j_dstream_alloced_size`| 4096         | 4096          | ✓     |
| clone.txt      | 20    | regular    | `j_dstream_alloced_size`| 4096         | 4096          | ✓     |
| sparse.bin     | 19    | regular    | `j_dstream_alloced_size`| 1056768      | 24576         | ✗     |
| link.txt       | 23    | symlink    | `zero`                  | 0            | 0             | ✓     |
| compressed.txt | 24    | compressed | `fail_closed`           | n/a          | 4096          | n/a   |

Family histogram for the volume:

| raw_type | family            | count |
| -------- | ----------------- | ----- |
| 0x3      | inode             | 11    |
| 0x4      | xattr             | 10    |
| 0x5      | sibling_link      | 2     |
| 0x6      | dstream_id        | 5     |
| 0x8      | file_extent       | 8     |
| 0x9      | dir_rec           | 12    |
| 0xc      | sibling_map       | 2     |

No `extent_reference` (raw_type 0x2) records were emitted into the
inode-level FS tree for this fixture; the volume's
`extentref_tree_oid` points at the per-volume tree that the v1
walker does not yet enter.

### The sparse-file divergence

`sparse.bin` carries:

- `j_dstream_t.size = 1052897` (== `st_size`),
- `j_dstream_t.alloced_size = 1056768` (== `round_up(size, 4096)`),
- `INO_EXT_TYPE_SPARSE_BYTES = 1032192` (the unallocated bytes
  inside the logical extent).

Mounted oracle: `st_blocks = 48`, `st_blocks * 512 = 24576`.

`alloced_size` overstates the actual on-disk allocation by exactly
`1056768 - 24576 = 1032192` bytes — i.e. **by the sparse-bytes
hint**. This matches SR-019's recorded disagreement between the
two `linux-apfs-rw` projects: the kernel module writes
`alloced_size = round_up(ds_size, blocksize)`, and APFS on macOS
follows the *same* rule (so it would fail apfsck's
`Σ extent.len == alloced_size` check on this volume; we did not
run apfsck here, but the field arithmetic forces it).

### The Hypothesis worth following up

The identity `alloced_size - sparse_bytes = 1056768 - 1032192 =
24576 = st_blocks * 512` matches on this fixture. Apple defines
`INO_EXT_TYPE_SPARSE_BYTES` as the unallocated portion of the
logical extent (SR-019, §Spec), so by construction
`alloced_size - sparse_bytes` should equal the bytes the
filesystem actually committed to disk. This is a **Hypothesis**
backed by one data point and Apple's own field semantics; it is
**not** an Observation strong enough to encode as the Rust
emission rule. The right next step is an EX-22b sparse-corpus
probe (leading hole, trailing hole, multi-hole, no-hole-but-
SPARSE_BYTES-set, hole-larger-than-file edge cases) before SR-019
is amended.

### The compressed case

`compressed.txt` has no `j_dstream_t` xfield (decmpfs stores
small data inline in the xattr) and so step 1 of SR-019 cannot
fire; step 2 returns `fail_closed`. The oracle reports
`st_blocks * 512 = 4096` for this row — i.e. macOS *did* allocate
one block (presumably the xattr block that holds the decmpfs
payload). SR-019 v1 deliberately does not emit this number;
recording it as `not_claimed` is the correct posture until a
follow-up probe builds an oracle that distinguishes the decmpfs-
inline footprint from the resource-fork-backed footprint.

## Artifacts Saved

- `artifacts/probe_ex22.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex22-fixture-operations.json`
- `artifacts/generated/ex22-mounted-posix-oracle.json`
- `artifacts/generated/ex22-rust-records.json`
- `artifacts/generated/ex22-precedence-table.json`
- `artifacts/generated/summary.json`

## Interpretation

- SR-019 step 1 is correct for the *non-sparse* regular cases
  (ordinary, clone, and by sharing, hard-link) on macOS: the
  on-disk `j_dstream_t.alloced_size` matches `st_blocks * 512`
  exactly because `round_up(ds_size, blocksize)` and "actual
  allocated extent bytes" coincide when no extent is sparse.
- Step 1 is **not** correct for sparse files on macOS-produced
  images. The Rust slice must therefore split step 1: `regular +
  dstream + no SPARSE_BYTES xfield` is safe to emit;
  `regular + dstream + SPARSE_BYTES xfield present` must
  fail-closed in v1 (and is a Hypothesis worth promoting via
  EX-22b once a broader sparse corpus passes the
  `alloced_size - sparse_bytes` rule).
- The symlink rule is unchanged: both candidates are 0; the
  oracle is 0; the rule emits 0; match by construction.
- The decmpfs fail-closed rule is unchanged. Note that
  `st_blocks * 512 = 4096` on the decmpfs row is **not** a
  mismatch by SR-019 v1 — it is exactly the case the rule
  refuses to emit, and the oracle column survives in the
  artifact for the future EX-22-compression probe to consume.
- Hypothesis to record (not promote): on macOS,
  `allocated_size = alloced_size - sparse_bytes`. Apple's own
  description of `INO_EXT_TYPE_SPARSE_BYTES` makes this an
  algebraic identity, not a coincidence. EX-22b must close it.

## What This Rules Out

- Rules out Hypothesis A (`validated_sr_019_precedence`) for the
  full case set on the proof fixture. The rule as written in
  SR-019 v1 is over-broad: step 1 must be split for sparse vs
  non-sparse regular files.
- Does not rule out the Rust slice for the non-sparse cases.
  Ordinary, clone, hard-link, and symlink rows can move forward
  under the amended precedence: regular + dstream + no
  SPARSE_BYTES → `Some(alloced_size)`; regular + dstream + has
  SPARSE_BYTES → `None`; regular + decmpfs → `None`;
  symlink/dir → `Some(0)`; else → `None`.
- Does not rule out the `alloced_size - sparse_bytes` rule for a
  future product mode. Records it as a Hypothesis; EX-22b is the
  vehicle.
- Does not validate any exclusive/shared/snapshot-retained
  metric. SR-019 explicitly excludes those; the extent-reference
  tree was not entered by the walker in this probe (the family
  histogram shows zero `extent_reference` raw_type 0x2 rows in
  the FS tree, consistent with the volume keeping
  `j_phys_ext_*` records out of the FS tree and in the dedicated
  `extentref_tree_oid` tree).
- Does not validate `Σ file_extent.len == alloced_size` on
  macOS-created images. The probe captures *counts* (8
  file_extent records across the fixture) but does not decode
  `j_file_extent_val_t` bodies; that is its own follow-up if and
  when needed.

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

- Implement the Rust slice for the amended SR-019 precedence:
  emit `Some(alloced_size)` for `regular + dstream + no
  SPARSE_BYTES`; emit `Some(0)` for symlink and directory; emit
  `None` for `regular + dstream + has SPARSE_BYTES`,
  `regular + decmpfs`, and anything else. Extend the
  `--summary` `not_claimed` register to spell out the sparse and
  decmpfs fail-closed cases by name.
- Scope an `EX-22b` sparse-corpus probe (leading hole, trailing
  hole, multi-hole, large-relative-to-file hole, zero-hole-but-
  SPARSE_BYTES-set boundary) to test whether
  `alloced_size - sparse_bytes` reproduces `st_blocks * 512` for
  every shape. If yes, amend SR-019 with the algebraic identity
  and promote sparse rows from `None` to `Some(_)` in a follow-up
  Rust slice.
