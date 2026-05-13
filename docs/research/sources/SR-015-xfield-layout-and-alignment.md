# SR-015 Xfield Layout And Alignment

Status: Complete
Date: 2026-05-13
Type: Source Review
Related RLs:
- RL-03
- RL-06
- RL-07
- RL-10
- RL-13

## Bottom line

APFS xfield parsing can be deterministic for the narrow Rust parser, but the
rule is more specific than "align the data to 8 bytes." The source-backed rule
is:

- `xf_blob_t` begins at the first byte of the record's `xfields[]` tail.
- xfield metadata occupies `4 + xf_num_exts * sizeof(x_field_t)` bytes.
- xfield values start immediately after that metadata; there is no extra
  alignment of the value-area start.
- for each metadata entry in order, read exactly `x_size` bytes and then skip
  `round_up(x_size, 8) - x_size` padding bytes before the next value.
- `xf_used_data` is best treated as the padded value-area byte count, not as the
  total blob size.

This single rule explains EX-13's apparent split layouts. The sparse inode
matched the candidate named `unpacked_start_blob_relative_fields` because its
first value length needed value-area padding. Ordinary two-field inodes matched
the candidate named `record_relative_start_record_relative_fields` when the same
`cursor += round_up(x_size, 8)` rule happened to coincide with record-relative
alignment. The candidate names are misleading; the evidence favors one value
cursor rule, not per-record layout variation.

## Scope

This review answers one question:

- Is xfield data start/alignment deterministic across inode and directory-entry
  records, and what source-backed rule explains EX-13's candidate layouts?

Out of scope:

- all xfield type semantics beyond the v1 fields named here
- physical allocation or clone accounting
- native Rust implementation changes

## Sources reviewed

- Apple File System Reference, retrieved 2026-05-13:
  <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf>
- `linux-apfs-rw`, retrieved 2026-05-13, commit
  `628b6810e46bcdd423189d2c66295258e10090dc`:
  <https://github.com/linux-apfs/linux-apfs-rw/blob/628b6810e46bcdd423189d2c66295258e10090dc/xfield.c>
- `apfs-fuse`, retrieved 2026-05-13, commit
  `66b86bd525e8cb90f9012543be89b1f092b75cf3`:
  <https://github.com/sgan81/apfs-fuse/blob/66b86bd525e8cb90f9012543be89b1f092b75cf3/ApfsLib/ApfsDir.cpp>
- `dissect.apfs`, retrieved 2026-05-13, commit
  `8d8dbd2545ebb1d65c1cda144097ee15f783e233`:
  <https://github.com/fox-it/dissect.apfs/blob/8d8dbd2545ebb1d65c1cda144097ee15f783e233/dissect/apfs/objects/fs.py>
- `go-apfs`, retrieved 2026-05-13, commit
  `9f8e60d7a59141f2fa44747eddc2202a0d6a03d9`:
  <https://github.com/blacktop/go-apfs/blob/9f8e60d7a59141f2fa44747eddc2202a0d6a03d9/types/btree.go>
- `libfsapfs`, retrieved 2026-05-13, commit
  `f179325e5405d3b09a314348646e9898b722759f`:
  <https://github.com/libyal/libfsapfs/blob/f179325e5405d3b09a314348646e9898b722759f/libfsapfs/libfsapfs_inode.c>
- The Sleuth Kit, retrieved 2026-05-13, commit
  `463bbece6702dd5486f910ddc8a0216dc3640970`:
  <https://github.com/sleuthkit/sleuthkit/blob/463bbece6702dd5486f910ddc8a0216dc3640970/tsk/fs/apfs_fs.cpp>
- EX-13 local artifact:
  `docs/research/experiments/EX-13-native-fs-record-body-oracle/artifacts/generated/xfield-layout-summary.json`

## Spec

- Apple defines xfields as `xf_blob_t` followed by `x_field_t` entries and then
  data bytes. Each `x_field_t` carries `x_type`, `x_flags`, and `x_size`.
- Apple states each extended-field type is unique inside the xfield array.
- Apple defines the v1-relevant xfield types already used by SR-014:
  `DREC_EXT_TYPE_SIBLING_ID`, `INO_EXT_TYPE_NAME`,
  `INO_EXT_TYPE_DSTREAM`, and `INO_EXT_TYPE_SPARSE_BYTES`.

## Observation

- `linux-apfs-rw` starts value data after the blob header and metadata table,
  reads xfield values in table order, and advances by `round_up(x_size, 8)`.
  Its insert path updates `xf_used_data` by the padded value length and writes
  `xf_used_data + metadata_length` as the total xfield collection length.
- `apfs-fuse` uses the same value cursor in `ApfsDir.cpp`: `xdata` starts at
  `obj->xfields + sizeof(xf_blob_t) + num_exts * sizeof(x_field_t)` and then
  advances by `((x_size + 7) & ~7)` for both inode and directory-record
  xfields.
- `dissect.apfs` reads `blob.xf_data`, yields `field.x_size` bytes for each
  xfield, and seeks to the next eight-byte boundary inside that value stream.
- `go-apfs` reads the blob header, then the metadata entries, then each value,
  and seeks by the padding needed to make each individual value length a
  multiple of eight.
- `libfsapfs` computes `value_data_offset = fixed_record_size + 4 +
  number_of_extended_fields * 4`, reads `value_data_size`, and then advances by
  `value_data_size` plus trailing padding to the next multiple of eight.
- The Sleuth Kit starts xfield data after the xfield headers and advances by
  `(ext.len + 7) & 0xFFF8`, matching the same padded-value rule.
- EX-13's sparse inode has `xf_num_exts = 3`, so the source-backed value start
  is 16. The name value has size 11 and therefore consumes 16 bytes; the dstream
  starts at 32, which is the candidate EX-13 scored as
  `unpacked_start_blob_relative_fields`.
- EX-13's ordinary clone inode has `xf_num_exts = 2`, so the source-backed value
  start is 12. Its dstream value is already 40 bytes, so the name starts at
  52. That happens to match EX-13's `record_relative_start_record_relative_fields`
  candidate, but the underlying operation is still "add padded value length."

## Hypothesis

- The apparent EX-13 split across three selected candidate labels is a probe
  modeling artifact. The narrow parser should implement the converged
  open-source rule above and keep the exact candidate scorer out of Rust.
- `xf_used_data` should be validated against the padded value-area byte count.
  A record where the metadata table fits and the value cursor fits but
  `xf_used_data` disagrees should be rejected until a fixture proves a legitimate
  counterexample.

## Conflict Note

- Apple wording around aligned xfield data is easy to read as "the value-area
  start is independently eight-byte aligned." The implementations above do not
  do that. They start values immediately after the metadata table and pad each
  value's occupied length.
- Smallest resolving probe: replay EX-13 with a source-backed xfield decoder
  that records `xf_num_exts`, `xf_used_data`, metadata length, each `x_size`,
  each padded length, decoded field values, and unused trailing bytes. The probe
  passes only if all namespace/logical-size rows still match and every required
  xfield satisfies `xf_used_data == sum(round_up(x_size, 8))`.

## Open Limits

- The current EX-13 artifacts do not record `xf_used_data`, so the validation
  rule above still needs a replay artifact before Rust enforces it.
- This review does not validate unknown xfield types. Unknown types can be
  skipped structurally only when the requested product mode does not depend on
  them and the xfield blob itself is well formed.
- The next fixture should still include at least one inode where a non-multiple
  length field precedes `DSTREAM`, and one directory-entry xfield record, so the
  rule is exercised outside the current sparse case.

## Decision impact

- `RL-03`: FS-record body parsing can move from candidate scoring to one
  deterministic xfield cursor rule after an EX-13 replay gate records
  `xf_used_data`.
- `RL-06`: hard-link sibling IDs and inode names should be decoded through the
  source-backed xfield rule, not through per-record layout guesses.
- `RL-07`: sparse-file logical size can continue to use dstream size once the
  xfield replay proves the deterministic rule against the saved sparse record.
- `RL-10`: add an xfield replay gate before Rust body parsing: same raw EX-13
  fixture, source-backed cursor rule, `xf_used_data` validation, same oracle
  comparison.
- `RL-13`: malformed xfield blobs, duplicate xfield types, value cursors that
  exceed the record body, or `xf_used_data` mismatches are fail-closed body
  gates for v1.
