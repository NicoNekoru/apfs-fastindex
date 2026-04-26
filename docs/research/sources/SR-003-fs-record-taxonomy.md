# SR-003 FS Record Taxonomy For Narrow V1

Status: Complete
Date: 2026-04-25
Type: Source Review
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle

## Bottom line

The outside evidence supports the repo's current narrow parser surface:

- `DIR_REC` records define directory membership and visible names.
- `INODE` records define file identity, type-supporting metadata, link counts,
  and inode extended fields.
- `INO_EXT_TYPE_DSTREAM` or equivalent inode size fields provide the primary
  logical-size source for ordinary files.
- `SIBLING_LINK` and `SIBLING_MAP` are required once hard links are in scope.
- `XATTR`, specifically symlink-bearing xattrs, is required for symlink target
  fidelity.
- `FILE_EXTENT`, extent-reference, snapshot metadata, and clone/shared-accounting
  machinery are deferred for v1 because they are not needed to enumerate a
  single-volume namespace or report logical size.

The review does not prove broad accounting semantics. It narrows v1 to the
records needed for namespace plus logical size and leaves allocated,
exclusive/shared, compression storage savings, and snapshot-retained bytes as
separate modes.

## Scope

This review answers one question:

- Which APFS file-system record families must the first raw parser understand to
  emit one raw volume's namespace, stable file identity, symlink targets, hard
  link relationships, and logical size?

Out of scope:

- physical allocated byte attribution
- exclusive/shared clone accounting
- snapshot-retained accounting
- merged System/Data/firmlink namespace synthesis
- file content extraction

## Evidence

### File-system tree organization

- `Spec | Apple/public docs:` APFS file-system metadata is stored in a
  file-system B-tree. File-system records are keyed so records for a given file
  system object can be found together, and the mount path reaches that tree via
  checkpoint -> container OMAP -> volume superblock -> volume OMAP -> root FS
  tree.
  Source:
  - [Apple File System Reference](https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf)

- `Observation | reverse engineering writeup:` JT Sylve describes FS-tree keys
  as ordered first by object identifier and then by record type, with a file
  system object's records stored sequentially. The published record taxonomy
  includes `INODE`, `XATTR`, `SIBLING_LINK`, `DSTREAM_ID`, `CRYPTO_STATE`,
  `FILE_EXTENT`, `DIR_REC`, `DIR_STATS`, `SNAP_NAME`, `SIBLING_MAP`, and
  `FILE_INFO`.
  Source:
  - [File System Trees](https://jtsylve.blog/post/2022/12/15/APFS-FSTrees)

### Namespace records

- `Observation | reverse engineering writeup:` Each APFS file-system entry has
  both an inode record and directory record. The inode stores metadata such as
  timestamps, ownership, type-related mode data, permissions, link count, and
  inode extended fields. Directory records store hierarchy placement, name, file
  id, date added, type flags, and directory-entry extended fields.
  Source:
  - [Inode and Directory Records](https://jtsylve.blog/post/2022/12/16/APFS-Inode-and-Directory-Records)

- `Observation | reverse engineering writeup:` A single inode can be referenced
  by more than one directory record, which is the key namespace fact for hard
  links. Directory records can also carry a sibling identifier extended field.
  Source:
  - [Inode and Directory Records](https://jtsylve.blog/post/2022/12/16/APFS-Inode-and-Directory-Records)

- `Observation | open-source implementation:` `go-apfs` path lookup and listing
  follow `DIR_REC` entries to child file ids, then load the child's FS records.
  It uses inode dstream fields for size and reads `XATTR_SYMLINK_EA_NAME` for
  symlink targets.
  Source:
  - [go-apfs `apfs.go`](https://github.com/blacktop/go-apfs/blob/main/apfs.go)

- `Observation | open-source implementation:` `dissect.apfs` models directory
  entries, inode records, xattrs, sibling ids, compression metadata, and inode
  size selection as first-class FS record concerns.
  Source:
  - [dissect.apfs objects](https://github.com/fox-it/dissect.apfs)

### Logical size records

- `Observation | reverse engineering writeup:` Inode extended fields include
  `INO_EXT_TYPE_DSTREAM`, which stores stream metadata, and
  `INO_EXT_TYPE_SPARSE_BYTES`, which records sparse bytes. The inode flags
  include `INODE_IS_SPARSE`, `INODE_WAS_CLONED`, `INODE_WAS_EVER_CLONED`, and
  `INODE_HAS_UNCOMPRESSED_SIZE`.
  Source:
  - [Inode and Directory Records](https://jtsylve.blog/post/2022/12/16/APFS-Inode-and-Directory-Records)

- `Observation | field research:` APFS sparse files can have large logical size
  while occupying only the extents with real data; APFS records sparse state via
  inode flags, sparse-byte extended fields, and file extents for the non-null
  data. This reinforces that logical size and allocated size are different
  metrics.
  Source:
  - [APFS: How sparse files work](https://eclecticlight.co/2024/06/08/apfs-how-sparse-files-work/)

- `Observation | field research:` APFS clones are distinct files with distinct
  inodes that initially share data blocks. Hard links, by contrast, are multiple
  namespace entries for one file object. This distinction matters for logical
  aggregate policy even before physical accounting is attempted.
  Source:
  - [APFS: Files and clones](https://eclecticlight.co/2024/03/20/apfs-files-and-clones/)

### Deferred records and trees

- `Hypothesis | inferred from converging sources:` `FILE_EXTENT` and the
  extent-reference tree are required for physical, shared, and snapshot-aware
  accounting, but not for the first single-volume namespace plus logical-size
  parser.

- `Hypothesis | inferred from converging sources:` snapshot metadata and
  firmlink/volume-group presentation belong to separate product modes. They
  should not expand the narrow v1 parser surface unless the chosen target is a
  snapshot or user-visible boot-root namespace.

## Required-record matrix

For raw single-volume namespace plus logical size:

- Required tree roots:
  - file-system root tree
- Required record families:
  - `DIR_REC` for directory membership, names, type flags, and child file ids
  - `INODE` for stable file identity, link count, mode/type support, and inode
    extended fields
  - `INO_EXT_TYPE_DSTREAM` or equivalent size-bearing inode fields for ordinary
    logical size
  - `XATTR` for symlink target fidelity through `XATTR_SYMLINK_EA_NAME`
  - `SIBLING_LINK` and `SIBLING_MAP` when hard-link relationships are present
- Required policies:
  - preserve path graph and file identity as separate concepts
  - emit symlink entries without following them
  - count unique file identities once per aggregate root for canonical v1
    logical directory totals

For future physical/shared/snapshot-aware accounting:

- Likely additional roots and records:
  - file extent records
  - extent-reference tree
  - snapshot metadata tree and snapshot metadata extensions
  - clone/share interpretation tied to snapshots and extent references

## Open Limits

- Compressed files need a tighter v1 policy: logical size should prefer the
  uncompressed logical size when the inode or compression metadata exposes it,
  but the exact field precedence should be validated in a corpus.
- Unicode normalization and case behavior are source-backed at the flag level
  and experiment-backed at the visible behavior level, but still need a broader
  raw-vs-oracle corpus.
- The review does not prove any physical, shared, exclusive, or
  snapshot-retained accounting metric.

## Decision impact

- `RL-03` can treat the narrow required-record set as source-backed, not merely
  experiment-derived.
- `RL-06` should keep path graph and file identity separate and keep hard links
  in core namespace design.
- `RL-07` should keep logical size as v1 and defer allocated/shared/snapshot
  metrics until extent-reference work has its own proof loop.
- `EX-04` should validate the matrix against a broader pinned raw-vs-oracle
  corpus before the repo-owned parser broadens beyond the existing image-backed
  allowlist.
