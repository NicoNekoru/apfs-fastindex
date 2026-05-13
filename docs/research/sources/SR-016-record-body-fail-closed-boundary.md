# SR-016 Record Body Fail-closed Boundary

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

The narrow Rust parser should reject malformed record bodies before namespace or
logical-size rows are emitted. For v1, "fail closed" means hard-stop the
requested raw output on malformed required records, and classify unsupported but
well-formed record-body features instead of guessing.

Immediate v1 hard stops:

- required FS-tree key/value shorter than the fixed struct for its record family
- variable-length name field whose declared length exceeds the key/value bytes
- required namespace names that are not NUL-terminated or not valid UTF-8
- xfield blob shorter than header, metadata table out of bounds, duplicate
  xfield type, value cursor out of bounds, or `xf_used_data` mismatch
- required xfield with wrong size, such as dstream not 40 bytes, sparse bytes
  not 8 bytes, or sibling ID not 8 bytes
- xattr value whose flags select neither or both embedded/stream data, whose
  `xdata_len` disagrees with the body length, or whose stream body is not
  `j_xattr_dstream_t`
- symlink inode without an embedded `com.apple.fs.symlink` payload that decodes
  to the mounted target string
- directory-entry type and inode `mode` type disagreeing for the same row
- hard-link directory record with `DREC_EXT_TYPE_SIBLING_ID` but missing or
  inconsistent `SIBLING_LINK` / `SIBLING_MAP` records
- compressed-size metadata conflicts that would affect logical-size output

## Scope

This review answers one question:

- What exact malformed or unsupported record-body cases should Rust fail closed
  on before v1 row emission?

Out of scope:

- checkpoint-map and OMAP gates already covered by SR-005, SR-006, SR-007, and
  SR-013 except where record-body decisions depend on selected-XID discipline
- recovery/carving behavior
- file-content decompression or physical accounting

## Sources reviewed

- Apple File System Reference, retrieved 2026-05-13:
  <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf>
- `linux-apfs-rw`, retrieved 2026-05-13, commit
  `628b6810e46bcdd423189d2c66295258e10090dc`:
  <https://github.com/linux-apfs/linux-apfs-rw/blob/628b6810e46bcdd423189d2c66295258e10090dc/key.c>,
  <https://github.com/linux-apfs/linux-apfs-rw/blob/628b6810e46bcdd423189d2c66295258e10090dc/xattr.c>,
  <https://github.com/linux-apfs/linux-apfs-rw/blob/628b6810e46bcdd423189d2c66295258e10090dc/symlink.c>
- `libfsapfs`, retrieved 2026-05-13, commit
  `f179325e5405d3b09a314348646e9898b722759f`:
  <https://github.com/libyal/libfsapfs/blob/f179325e5405d3b09a314348646e9898b722759f/libfsapfs/libfsapfs_inode.c>,
  <https://github.com/libyal/libfsapfs/blob/f179325e5405d3b09a314348646e9898b722759f/libfsapfs/libfsapfs_directory_record.c>
- `go-apfs`, retrieved 2026-05-13, commit
  `9f8e60d7a59141f2fa44747eddc2202a0d6a03d9`:
  <https://github.com/blacktop/go-apfs/blob/9f8e60d7a59141f2fa44747eddc2202a0d6a03d9/types/btree.go>
- `dissect.apfs`, retrieved 2026-05-13, commit
  `8d8dbd2545ebb1d65c1cda144097ee15f783e233`:
  <https://github.com/fox-it/dissect.apfs/blob/8d8dbd2545ebb1d65c1cda144097ee15f783e233/dissect/apfs/objects/fs.py>
- The Sleuth Kit, retrieved 2026-05-13, commit
  `463bbece6702dd5486f910ddc8a0216dc3640970`:
  <https://github.com/sleuthkit/sleuthkit/blob/463bbece6702dd5486f910ddc8a0216dc3640970/tsk/fs/apfs_compat.cpp>
- SR-014 and EX-13 local artifacts in this repository.

## Spec

- Apple defines fixed record bodies for `j_inode_val_t`, `j_drec_val_t`,
  `j_xattr_val_t`, `j_sibling_val_t`, `j_sibling_map_val_t`,
  `j_dstream_t`, and `j_xattr_dstream_t`.
- Apple defines directory entry type as the low bits of `j_drec_val_t.flags`
  under `DREC_TYPE_MASK`.
- Apple defines xattrs as either embedded data or stream data, with
  `j_xattr_val_t.xdata_len` describing the xdata body.
- Apple defines xfields as typed, uniquely occurring entries inside one blob.

## Observation

- `linux-apfs-rw` treats malformed category keys as corruption when directory
  and xattr names lack final NUL bytes or are shorter than their fixed key
  headers.
- `linux-apfs-rw` validates xattr name length against `j_xattr_key_t.name_len`,
  requires NUL termination, requires stream-backed xattrs to have a
  `j_xattr_dstream_t`-sized body, and requires embedded xattr body length to
  equal `xdata_len`.
- `linux-apfs-rw` symlink resolution reads `com.apple.fs.symlink` and rejects a
  target that is empty or not NUL-terminated.
- `libfsapfs` validates inode and directory-entry xfield offsets before reading
  value data, rejects duplicate inode name xfields, rejects zero or out-of-bounds
  inode xfield values, and rejects unsupported inode/directory extended-field
  types in the contexts where it chooses to parse them.
- `go-apfs` returns parse errors when binary reads of fixed values, xfield
  headers, or typed xfield bodies fail. Its xattr parser separates embedded and
  stream forms.
- `dissect.apfs` models the same typed bodies and exposes symlink xattrs,
  sibling IDs, inode mode/type, xfield values, and xattr stream forms as
  first-class parser state.
- The Sleuth Kit rejects invalid APFS inodes, reads symlink targets from the
  symlink xattr, rejects multiple compression records, and reports mismatches
  between the compressed BSD flag and compression records as warnings in its
  forensic UI.
- EX-13 proved row emission depends on body-field consistency, not merely record
  family counts: rows matched only after the xfield interpretation produced
  plausible dstream and sparse-byte values.

## Hypothesis

- The Rust parser should split failures into:
  - `malformed_record_body`: impossible lengths, impossible xfield layout,
    missing terminators, invalid UTF-8 where a path string is required, or fixed
    structure size mismatch.
  - `unsupported_record_body`: well-formed but outside v1, such as stream-backed
    symlink targets, resource-fork compression precedence, unknown drec flag bits
    that affect type, or unsupported xattr flag combinations.
  - `body_field_mismatch`: row-relevant cross-record inconsistency, such as
    drec type versus inode mode disagreement or hard-link sibling records that
    do not close the path-to-inode mapping.
- Unknown xattrs that are neither symlink nor size-policy inputs should be
  preserved or skipped as opaque metadata. Unknown xfield types on required
  inode/drec records should be structurally skipped only if the xfield blob is
  valid and no requested output depends on that type.

## Open Limits

- The exact reserved-bit policy for `j_drec_val_t.flags` still needs a corpus.
  V1 should accept known file types under `DREC_TYPE_MASK` and fail closed if
  other bits appear and the parser cannot prove they are irrelevant.
- Stream-backed symlink targets might be legitimate on some volumes, but the
  current allowlist has only embedded symlink payload evidence.
- This review does not broaden compression support. It only turns unresolved
  compression precedence into a body gate.
- Valid UTF-8 is required for v1 namespace strings. A future forensic mode could
  preserve raw byte paths differently, but that is outside the current product
  contract.

## Decision impact

- `RL-03`: record-family discovery is no longer enough; each v1 family needs
  fixed-size, variable-length, and cross-record validation before row emission.
- `RL-06`: namespace assembly must fail closed on malformed names, missing
  symlink payloads, hard-link sibling inconsistencies, and drec/inode type
  mismatches.
- `RL-07`: logical-size output must fail closed on unresolved compression
  conflicts or missing required dstream-size inputs for non-compressed regular
  files.
- `RL-10`: add synthetic negative body cases to the next parser oracle, not just
  positive detached-image fixtures.
- `RL-13`: encode body-failure categories as compatibility gates:
  `malformed_record_body`, `unsupported_record_body`, and `body_field_mismatch`.
