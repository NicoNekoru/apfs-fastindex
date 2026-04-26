# SR-014 Native FS-record body contract

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-03
- RL-06
- RL-07
- RL-10
- RL-13

## Bottom line

The first native namespace/logical-size row emitter needs record-body decoding,
not more record-family taxonomy. The required body contract is:

- `DIR_REC`: parent directory object ID from the key, entry name from the key,
  child inode from `j_drec_val.file_id`, file type from `j_drec_val.flags &
  DREC_TYPE_MASK`, and optional hard-link sibling ID from
  `DREC_EXT_TYPE_SIBLING_ID`.
- `INODE`: inode ID from the key, `parent_id`, `private_id`, `mode`, link or
  child count, BSD/internal flags, `uncompressed_size` only when
  `INODE_HAS_UNCOMPRESSED_SIZE` is set, and inode extended fields including
  `INO_EXT_TYPE_DSTREAM`, `INO_EXT_TYPE_NAME`, and `INO_EXT_TYPE_SPARSE_BYTES`.
- `XATTR`: xattr name from the key, embedded-vs-stream flags and payload from
  `j_xattr_val`; `com.apple.fs.symlink` is required for symlink targets, while
  `com.apple.decmpfs` is a logical-size/accounting probe input rather than a
  namespace record.
- `SIBLING_LINK` / `SIBLING_MAP`: hard-link sibling ID, parent, name, and target
  inode mapping so path identity and file identity remain separate.
- dstream fields: `j_dstream_t.size` is the primary ordinary logical data size;
  `alloced_size`, crypto ID, read/write counters, `DSTREAM_ID.refcnt`, and file
  extents are not v1 logical-size outputs but must be surfaced as validation or
  future-accounting inputs.

This review answers one question: which exact record-body fields must the native
parser decode before it can emit oracle-checkable namespace and logical-size
rows?

## Evidence

### Spec

- Apple File System Reference says file-system objects are sorted record groups
  keyed first by object ID and then record type; files require `INODE`, symbolic
  links require `XATTR`, directories contain `DIR_REC`, and sibling maps require
  `SIBLING_MAP`.
- Apple defines `j_inode_val_t` with `parent_id`, `private_id`, timestamps,
  `internal_flags`, `nchildren`/`nlink`, protection class, BSD flags, owner,
  group, `mode`, `uncompressed_size`, and `xfields`.
- Apple defines `j_drec_key_t` / `j_drec_hashed_key_t` as the directory-entry
  name-bearing key, and `j_drec_val_t` as `file_id`, `date_added`, `flags`, and
  optional extended fields. The low bits under `DREC_TYPE_MASK` store the
  directory-entry file type.
- Apple defines `j_xattr_key_t` as the xattr name-bearing key and `j_xattr_val_t`
  as `flags`, `xdata_len`, and `xdata`, with either `XATTR_DATA_EMBEDDED` or
  `XATTR_DATA_STREAM` required.
- Apple defines inode/directory extended fields as an `xf_blob_t` header,
  `x_field_t` metadata, and eight-byte-aligned data; each extended-field type is
  unique inside the array. `DREC_EXT_TYPE_SIBLING_ID`, `INO_EXT_TYPE_DSTREAM`,
  `INO_EXT_TYPE_NAME`, and `INO_EXT_TYPE_SPARSE_BYTES` are the v1-relevant
  fields.
- Apple defines hard-link sibling records: `j_sibling_key_t.sibling_id`,
  `j_sibling_val_t.parent_id/name`, and `j_sibling_map_val_t.file_id` convert
  between sibling IDs and the underlying inode.
- Apple defines `j_dstream_t` as `size`, `alloced_size`, `default_crypto_id`,
  `total_bytes_written`, and `total_bytes_read`; `size` is the data size in
  bytes.

### Observation

- JT Sylve's inode/directory writeup converges with Apple's structures and
  emphasizes that every entry has inode and directory records, while hard links
  decouple path entries from the shared inode.
- JT Sylve's dstream writeup identifies the default data stream through
  `j_inode_val_t.private_id` and `INO_EXT_TYPE_DSTREAM`, and treats extended
  attributes as either embedded data or stream-backed data via
  `j_xattr_dstream_t`.
- `dissect.apfs` models the same minimal bodies for `j_inode_val`,
  `j_drec_key`, `j_drec_val`, `j_xattr_val`, `j_dstream_t`,
  `j_sibling_val`, and `j_sibling_map_val`.
- `linux-apfs-rw` models the same body fields and adds pragmatic hard-stop
  signals: recognized inode internal flags include sparse, cloned,
  uncompressed-size, sync-root, and snapshot-COW-exemption bits; xattr names
  include `com.apple.fs.symlink`, `com.apple.fs.firmlink`, and
  `com.apple.decmpfs`; unknown or reserved flag bits are tracked explicitly.
- `EX-10` already proves the native Rust traversal reaches the right record
  families on the proof fixture (`INODE`, `DIR_REC`, `DSTREAM_ID`, `XATTR`,
  `SIBLING_LINK`, `SIBLING_MAP`) but explicitly does not decode names, values,
  xfields, or logical-size rows.

### Hypothesis

- The smallest safe native parser slice is a record-body dumper that emits raw
  typed fields and validation notes, not final product entries. It should
  preserve enough structure for an oracle diff to identify whether the failure
  is body parsing, namespace assembly, or metric policy.
- For ordinary uncompressed files in the current detached-image allowlist,
  `INO_EXT_TYPE_DSTREAM.size` should match the POSIX logical-size oracle. If the
  dstream field is absent, zero, or inconsistent while compression/sparse flags
  or `com.apple.decmpfs` metadata are present, the source should move to `EX-09`
  instead of guessing a size.
- `DSTREAM_ID.refcnt`, file extents, `alloced_size`, and physical extent records
  are validation/future-accounting inputs for v1, not requirements for a
  namespace plus ordinary logical-size row.

## Open Limits

- Compression precedence remains open: `j_dstream_t.size`,
  `j_inode_val_t.uncompressed_size`, and `decmpfs` header size still need the
  `EX-09` metric-specific oracle.
- Sparse, clone, hard-link aggregate policy, and snapshot-retained bytes are not
  answered by this body contract.
- Case-folding and Unicode normalization are key-comparison issues as well as
  namespace-output issues; `EX-13` should record volume flags and names exactly
  before making normalization claims.
- Unsupported xattr streams, encrypted per-file data, sealed-volume file-extents,
  unknown record families, malformed variable-length fields, duplicate xfields,
  invalid UTF-8, or impossible lengths should remain fail-closed until a probe
  names a narrower behavior.

## Decision impact

- `RL-03`: native work can move from record-family counting to body-field dumps
  for the six v1 namespace families, with `DSTREAM_ID` treated as validation
  context rather than a logical-size source.
- `RL-06`: namespace rows require directory-key name, parent directory ID,
  `DIR_REC.file_id`, inode `mode`, symlink xattr payload, and sibling link/map
  records for hard links.
- `RL-07`: ordinary logical size is expected to come from inode dstream metadata,
  while compression and allocation metrics remain separate probes.
- `RL-10`: the next oracle must compare native field dumps against a same-run
  mounted/POSIX oracle and preserve `selected_xid` discipline from `EX-12`.
- `RL-13`: malformed variable-length fields, unknown flags, unsupported xattr
  stream forms, and mode-incompatible record families are hard-stop conditions.
- Exact next step: design `EX-13` as a native FS-record body oracle plan that
  dumps these fields under the `EX-12` validated OMAP/root context and compares
  them to mounted/POSIX and `go-apfs` evidence generated from the same fixture.
