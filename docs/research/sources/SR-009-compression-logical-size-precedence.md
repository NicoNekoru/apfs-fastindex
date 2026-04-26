# SR-009 Compression And Logical-Size Precedence

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-07
- RL-10
- RL-13

## Bottom line

Compression is not a reason to expand v1 into physical accounting, but it is a
reason to keep logical-size precedence explicit. V1 should report public
logical size and prefer uncompressed logical-size metadata when APFS exposes it;
compressed storage savings, compressed extents, resource forks, and `decmpfs`
algorithms remain outside v1.

This review answers one question: how should compression affect v1 size parsing?

## Evidence

### Spec

- Apple documents APFS logical file behavior separately from allocation features
  such as clones and sparse files, and public APIs expose logical file size
  independently of allocated storage.

### Observation

- JT Sylve's inode writeup lists inode flags including sparse, cloned, and
  `INODE_HAS_UNCOMPRESSED_SIZE`, and identifies dstream extended fields as size
  carriers.
- JT Sylve's data-stream writeup defines `j_dstream.size` as logical data size
  and distinguishes it from `alloced_size`; sparse extents can make allocated
  bytes smaller than logical bytes.
- JT Sylve's xattr/data-stream writeup shows that extended attributes can be
  embedded or stored as their own data streams, which matters because APFS
  compression metadata may live outside the ordinary file data stream.
- `dissect.apfs` models `j_dstream.size`, xattrs, `decmpfs_header`, compression
  algorithms, and inode `size` as parser-visible concepts.
- `dissect.apfs` exposes inode `uncompressed_size`, the
  `HAS_UNCOMPRESSED_SIZE` flag, `decmpfs_header.uncompressed_size`, and
  `DecmpfsStream` as distinct concerns.
- Yogesh Khatri's forensic APFS notes warn that compressed files can have logical
  size set to zero in ordinary metadata and require parsing the compressed data
  header, inline or resource-fork backed, to recover the real uncompressed
  logical size.
- `apfs-fuse` and `libfsapfs` both document incomplete compression support
  across algorithms, especially LZFSE and method variants.
- `EX-04` did not prove compression storage semantics; its compression candidate
  still matched logical-size output and did not justify physical accounting.

### Hypothesis

- The v1 parser should treat compressed-file logical size as supported only when
  it can reconcile public `st_size` with one of the source-backed fields:
  dstream logical size, inode `uncompressed_size`, or decmpfs uncompressed size.
  If ordinary dstream size is zero and no validated uncompressed-size source is
  available, raw mode should fail closed for that file/source class rather than
  reporting zero.

## Open Limits

- The repo still lacks a fixture that reliably creates compressed APFS files with
  meaningful allocated-byte divergence.
- The exact precedence among dstream size, uncompressed-size flags, decmpfs
  xattrs, and resource-fork data needs `EX-09` execution.
- Dataless cloud files are not covered.
- Symlink xattr size and resource-fork size are namespace fidelity concerns but
  should not be folded into regular-file logical byte totals without an oracle.

## Decision impact

- `RL-07`: logical size remains v1; physical/compressed storage savings stay
  deferred.
- `RL-13`: unsupported compression algorithms are source-gate hard stops only
  when they affect requested output or raw read correctness.
- `EX-09`: must capture public `st_size`, raw dstream size, inode
  uncompressed-size fields, decmpfs xattr/resource-fork headers, and allocated
  observations separately.
- Exact next step: update `EX-09` so compressed logical-size precedence can be
  tested before native FS-record output claims compressed-file support.
