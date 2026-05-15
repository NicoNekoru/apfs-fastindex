# EX-19 SR-017 logical-size precedence fixture

ID: EX-19
Title: SR-017 logical-size precedence fixture
Date: 2026-05-14
Owner: Claude
Status: Executed
Result: `validated_sr_017_precedence`
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

SR-017 names a five-step logical-size precedence table for the v1
namespace + logical-size product mode. EX-19 builds one same-run APFS
fixture covering each case (ordinary, sparse, cloned, hard-linked,
symlink, compressed via `ditto --hfsCompression`), captures every raw
candidate size source (inode `internal_flags`, inode `uncompressed_size`,
`j_dstream_t.size`, `INO_EXT_TYPE_SPARSE_BYTES`, `com.apple.decmpfs`
header `uncompressed_size`, `com.apple.fs.symlink` xattr payload), applies
the SR-017 precedence, and asserts the result equals the mounted POSIX
`st_size` for every entry. Pass condition: zero mismatches across all
cases or, if compression sprawls, a clean pass for the non-compressed
cases with the compression sub-case documented as a separate sub-EX.

## Question

- For ordinary, sparse, cloned, hard-linked, symlink, and compressed
  files on a detached APFS fixture, does the SR-017 precedence
  (`INODE_HAS_UNCOMPRESSED_SIZE` -> `inode.uncompressed_size` ->
  `com.apple.decmpfs.uncompressed_size` -> `j_dstream_t.size` -> zero;
  symlinks use the `com.apple.fs.symlink` xattr payload byte length)
  reproduce the mounted POSIX `st_size` for every entry?

## Hypotheses

- Hypothesis A `validated_sr_017_precedence`: yes. Every case (including
  the compressed one) reproduces public `st_size` under SR-017 with zero
  mismatches.
- Hypothesis B `precedence_gap`: at least one case mismatches, or one of
  the candidate size sources cannot be captured cleanly. The probe records
  per-entry which candidate would have been picked under each rule and
  what `st_size` actually is, so SR-017 can be amended or a compression
  sub-EX scoped.

## Environment

- macOS version captured in `artifacts/generated/environment.json`.
- APFS source: generated `.dmg` containing one ordinary, sparse, cloned,
  hard-linked, symlink, and compressed file.
- Mounted phase: fixture creation and POSIX oracle capture.
- Raw phase: detached image, reattached `-nomount -readonly`.
- Out of scope: physical/shared/exclusive byte accounting, resource-fork
  compression, snapshot-retained size, dataless cloud files,
  decompressed file-content reads.

## Oracle

- Mounted POSIX `st_size` is the per-entry oracle. Symlink `st_size` is
  the target byte length.
- The Rust crate's `FsRecordDump.records` (EX-18 parity, field-level
  identical to Python EX-13/EX-16) is the raw-side parser. We do not run
  a separate Python parser here because EX-18 already pinned cross-tool
  field equality.

## Setup

1. Capture environment manifest.
2. Build a fresh APFS image. Inside it:
   - one ordinary file of known size
   - one sparse file with hole + tail data
   - clone via `cp -c`
   - hard link via `os.link`
   - symlink
   - one ordinary file copied via `ditto --hfsCompression` into a target
     that should land in APFS as `UF_COMPRESSED` + `com.apple.decmpfs`
3. Capture mounted POSIX `st_size`, `st_blocks`, `UF_COMPRESSED` flag, and
   xattr inventory for each entry.
4. Detach and reattach `-nomount -readonly`.
5. Run the Rust scanner; collect `FsRecordDump.records`.

## Probe Steps

1. For each mounted entry, capture `st_size` (oracle).
2. Group Rust records by `object_id`. For each inode, capture:
   - `internal_flags`
   - `uncompressed_size`
   - `has_uncompressed_size`
   - `dstream.size`, `dstream.alloced_size`
   - `sparse_bytes`
3. For every xattr the inode owns, capture name + body (payload_hex,
   payload_utf8, embedded/stream flags). Decode `com.apple.decmpfs`
   header `magic`, `compression_type`, `uncompressed_size` from the
   embedded payload.
4. For every symlink, capture `com.apple.fs.symlink` target string.
5. Apply SR-017 precedence per-entry and compare to `st_size`.
6. Save per-entry breakdown so SR-017 can be cited line-by-line.

## Expected Observations

### If Hypothesis A is true

- Every entry's SR-017 result equals `st_size`. No fallback to zero is
  needed for any case other than possibly metadata-empty special files.

### If Hypothesis B is true

- At least one mismatch. The probe records:
  - which case (ordinary/sparse/clone/hard/symlink/compressed)
  - which SR-017 step picked
  - the candidate values
  - the actual `st_size`

## Observed Results

Per-inode breakdown (5 unique inodes; `hard.txt` shares
`ordinary.txt`'s inode and therefore the same precedence pick):

| path           | inode | kind       | SR-017 step picked          | picked value | mounted st_size | match |
| -------------- | ----- | ---------- | --------------------------- | ------------ | --------------- | ----- |
| ordinary.txt   | 16    | regular    | `j_dstream_size`            | 29           | 29              | ✓     |
| sparse.bin     | 19    | regular    | `j_dstream_size`            | 1052897      | 1052897         | ✓     |
| clone.txt      | 20    | regular    | `j_dstream_size`            | 29           | 29              | ✓     |
| link.txt       | 23    | symlink    | `symlink_target_len`        | 12           | 12              | ✓     |
| compressed.txt | 24    | compressed | `inode_uncompressed_size`   | 53248        | 53248           | ✓     |

Notable per-row evidence:

- **sparse.bin** has `j_dstream_size=1052897` (matches public size) AND
  `INO_EXT_TYPE_SPARSE_BYTES=1032192`. SR-017 step 2 correctly treats
  `SPARSE_BYTES` as an allocation hint, **not** the logical size.
- **clone.txt** shares no records with `ordinary.txt`'s body decoder
  output — each inode carries its own `j_dstream_size`. SR-017 step 3
  reduces to step 1 cleanly.
- **hard.txt** does not appear as a separate row because the SR-017 rule
  is per-inode; both `hard.txt` and `ordinary.txt` resolve to inode 16
  and pick `j_dstream_size=29 == st_size`.
- **link.txt** has no dstream and no decmpfs xattr. SR-017 step 5 picks
  the `com.apple.fs.symlink` payload byte length (12 = `len("ordinary.txt")`).
- **compressed.txt** has `INODE_HAS_UNCOMPRESSED_SIZE` set and
  `inode.uncompressed_size = 53248 = st_size`. The `com.apple.decmpfs`
  header reports `uncompressed_size = 437` and
  `compression_type = 0` (placeholder bytes), so SR-017 step 4's
  preference for the inode flag is necessary — falling back to the
  decmpfs header would have been wrong on this fixture. There is no
  `j_dstream_t` on this inode (which is consistent with APFS storing
  compressed data in `com.apple.decmpfs` for small files).

Verdict: `validated_sr_017_precedence`. Zero mismatches across all five
unique inodes including the compressed case. EX-19 closes the
compression sub-case in-band; no EX-19b needed for the proof fixture
shape.

## Artifacts Saved

- `artifacts/probe_ex19.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex19-fixture-operations.json`
- `artifacts/generated/ex19-mounted-posix-oracle.json`
- `artifacts/generated/ex19-rust-records.json`
- `artifacts/generated/ex19-precedence-table.json`
- `artifacts/generated/summary.json`

## Interpretation

- SR-017 is the right precedence rule for the v1 logical-size product
  mode on the proof-fixture corpus. The compressed case (where
  `j_dstream_t` is absent and the decmpfs header carries placeholder
  data) is the most load-bearing assertion: it specifically requires the
  `INODE_HAS_UNCOMPRESSED_SIZE`-first preference and would fail if
  step 4 was reordered.
- `INO_EXT_TYPE_SPARSE_BYTES` decoded under SR-015 stays separate from
  `j_dstream_size`. Rust output already carries both fields per inode in
  `FsRecordDump.records`; emitter code should consume `j_dstream_size`
  for logical size and keep `sparse_bytes` as an allocation hint only.
- Hard-link rows do not need a separate precedence column. The
  per-inode rule plus the existing SIBLING_LINK / SIBLING_MAP records
  (already decoded by EX-18) is sufficient.

## What This Rules Out

- Rules out hypothesis B `precedence_gap` for the v1 corpus shape.
- Does not rule out compression edge cases where the decmpfs payload is
  the only valid size source (e.g., resource-fork-backed compression, or
  inodes where `INODE_HAS_UNCOMPRESSED_SIZE` is clear). Those would
  exercise SR-017 step 4's second clause and are out of EX-19 scope.
- Does not validate physical, shared, exclusive, snapshot-retained, or
  decompressed accounting — SR-017 explicitly excludes those.

## Impact on RLs

- RL-07: a positive verdict pins the SR-017 precedence as the rule the
  Rust namespace emitter will use. A compression-only mismatch isolates
  the compression sub-case for an EX-19b probe without blocking the
  ordinary/sparse/clone/hard/symlink emission gate.
- RL-10: the per-entry precedence breakdown becomes the regression
  artifact future logical-size changes must clear.
- RL-13: any case where the SR-017 fallback would be "zero" or "fail
  closed" lands here so EX-21 (fallback path) can match policy.

## Next Exact Step

- Run the probe end-to-end; on positive verdict for all cases, proceed
  to EX-20 (SR-018 name/case fixture). On compression mismatch only,
  scope EX-19b for compression and proceed with the non-compressed
  precedence promoted into Rust per the Rust MWP gate.
