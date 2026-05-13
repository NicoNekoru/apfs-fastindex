# SR-017 Logical-size Source Precedence

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

For v1 namespace plus logical size, the parser should report public logical
bytes, not allocated bytes. Source-backed precedence is:

1. regular uncompressed files: `INO_EXT_TYPE_DSTREAM.j_dstream_t.size`; if no
   dstream and no compression metadata, report zero.
2. sparse files: still `j_dstream_t.size`; `INO_EXT_TYPE_SPARSE_BYTES` is a
   sparse/allocation hint, not the logical size.
3. cloned files: still that file's dstream logical size; clone flags,
   `private_id`, file extents, and `DSTREAM_ID.refcnt` do not change per-file
   logical bytes.
4. compressed regular files: `j_inode_val_t.uncompressed_size` wins when
   `INODE_HAS_UNCOMPRESSED_SIZE` is set; otherwise the `com.apple.decmpfs`
   header's uncompressed size wins; if compression indicators conflict or the
   uncompressed-size source is unavailable, fail closed.
5. symlinks: logical size is the symlink target byte length from
   `com.apple.fs.symlink`, excluding the terminating NUL observed in APFS xattr
   payloads.

This does not support physical, shared, exclusive, compressed-on-disk, or
snapshot-retained accounting.

## Scope

This review answers one question:

- Which logical-size source wins for ordinary, sparse, compressed, cloned, and
  symlink records?

Out of scope:

- decompressed file-content reads
- allocated/shared/exclusive formulas
- resource-fork compression algorithm support
- directory aggregate policy beyond consuming per-file logical sizes

## Sources reviewed

- Apple File System Reference, retrieved 2026-05-13:
  <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf>
- `dissect.apfs`, retrieved 2026-05-13, commit
  `8d8dbd2545ebb1d65c1cda144097ee15f783e233`:
  <https://github.com/fox-it/dissect.apfs/blob/8d8dbd2545ebb1d65c1cda144097ee15f783e233/dissect/apfs/objects/fs.py>,
  <https://github.com/fox-it/dissect.apfs/blob/8d8dbd2545ebb1d65c1cda144097ee15f783e233/dissect/apfs/stream.py>
- `apfs-fuse`, retrieved 2026-05-13, commit
  `66b86bd525e8cb90f9012543be89b1f092b75cf3`:
  <https://github.com/sgan81/apfs-fuse/blob/66b86bd525e8cb90f9012543be89b1f092b75cf3/apfsfuse/ApfsFuse.cpp>
- `libfsapfs`, retrieved 2026-05-13, commit
  `f179325e5405d3b09a314348646e9898b722759f`:
  <https://github.com/libyal/libfsapfs/blob/f179325e5405d3b09a314348646e9898b722759f/libfsapfs/libfsapfs_inode.c>,
  <https://github.com/libyal/libfsapfs/blob/f179325e5405d3b09a314348646e9898b722759f/libfsapfs/libfsapfs_file_entry.c>
- The Sleuth Kit, retrieved 2026-05-13, commit
  `463bbece6702dd5486f910ddc8a0216dc3640970`:
  <https://github.com/sleuthkit/sleuthkit/blob/463bbece6702dd5486f910ddc8a0216dc3640970/tsk/fs/apfs_fs.cpp>,
  <https://github.com/sleuthkit/sleuthkit/blob/463bbece6702dd5486f910ddc8a0216dc3640970/tsk/fs/apfs_compat.cpp>
- `linux-apfs-rw`, retrieved 2026-05-13, commit
  `628b6810e46bcdd423189d2c66295258e10090dc`:
  <https://github.com/linux-apfs/linux-apfs-rw/blob/628b6810e46bcdd423189d2c66295258e10090dc/symlink.c>
- EX-04 and EX-13 local artifacts in this repository.

## Spec

- Apple defines `j_dstream_t.size` separately from `alloced_size`.
- Apple defines `j_inode_val_t.uncompressed_size` and the
  `INODE_HAS_UNCOMPRESSED_SIZE` flag.
- Apple defines `INO_EXT_TYPE_SPARSE_BYTES` separately from dstream size.
- Apple defines xattrs, including embedded and stream-backed data, as distinct
  FS-tree record bodies.

## Observation

- `dissect.apfs` implements inode size precedence as:
  `INODE_HAS_UNCOMPRESSED_SIZE` -> inode `uncompressed_size` ->
  `com.apple.decmpfs` header `uncompressed_size` -> dstream `size` -> zero.
- `dissect.apfs` compressed streams are initialized with the decmpfs header's
  `uncompressed_size`, and resource-fork-backed compression is treated as a
  separate content stream problem.
- `apfs-fuse` reports compressed regular-file `st_size` from inode
  `uncompressed_size` when the flag is set. Otherwise it reads
  `com.apple.decmpfs` and uses that header's size for supported inline
  algorithms. For non-compressed regular files it uses inode dstream size, and
  falls back to zero if no dstream is present.
- `libfsapfs` extracts inode data-stream `used_size` as the data stream size
  and separately reads compressed-data headers, mapping supported compression
  methods before using compressed header `uncompressed_data_size`.
- The Sleuth Kit stores ordinary APFS object `_size` from the inode dstream
  `size` and records `_size_on_disk` from dstream `alloced_size`. Its compat
  layer separately reads decmpfs headers and treats multiple compression
  records as corruption.
- `linux-apfs-rw` reads symlink targets from `com.apple.fs.symlink` and rejects
  empty or non-NUL-terminated targets. EX-13 observed symlink xdata length 10
  for target `moved.txt\0` and public logical size 9.
- EX-13 matched a sparse file's public logical size of 1048576 from dstream
  size while `INO_EXT_TYPE_SPARSE_BYTES` decoded to 1015808, proving those fields
  must not be swapped.
- EX-04 and EX-13 matched clone and hard-link logical sizes without parsing
  extent-reference or shared-block accounting.

## Hypothesis

- Rust v1 can safely emit per-file logical size for ordinary, sparse, cloned,
  hard-linked, and symlink rows using the precedence above once the source-backed
  xfield decoder from SR-015 is replayed.
- Compression should remain supported only for logical size when the chosen
  uncompressed-size source is present and internally consistent. Any request for
  compressed storage savings or physical bytes must stay outside v1.
- A compressed file with `UF_COMPRESSED` or `com.apple.decmpfs` but no valid
  `INODE_HAS_UNCOMPRESSED_SIZE` or decmpfs uncompressed-size source should
  produce `unsupported_record_body`, not size zero.

## Open Limits

- The repo still needs a generated compressed fixture where public `st_size`,
  inode `uncompressed_size`, decmpfs header size, and dstream size are all
  captured in the same selected state.
- Resource-fork compression is not validated for v1 logical size, even if some
  implementations can read it.
- Dataless cloud files and snapshot-retained sizes are not covered.
- Directory `st_size` is not a product logical-byte metric for this parser.

## Decision impact

- `RL-07`: logical-size precedence is now specific enough for ordinary, sparse,
  cloned, hard-linked, and symlink records; compression remains gated by a
  same-run precedence fixture.
- `RL-06`: symlink target extraction is both namespace data and the symlink
  logical-size source.
- `RL-10`: the compression oracle must preserve every candidate raw source
  separately and compare them against public `st_size`.
- `RL-13`: compression indicator conflicts, missing decmpfs metadata, multiple
  compression records, or unsupported compression forms are fail-closed gates
  for v1 logical-size output.
