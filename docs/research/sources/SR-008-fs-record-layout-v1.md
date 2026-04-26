# SR-008 FS Record Layout For V1

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-03
- RL-06
- RL-07
- RL-10

## Bottom line

The v1 FS-record surface remains narrow but not trivial: native parsing needs
`DIR_REC`, `INODE`, logical-size-bearing dstream fields, symlink `XATTR`,
`SIBLING_LINK`, and `SIBLING_MAP`. File extents and extent-reference parsing are
not prerequisites for single-volume namespace plus logical size.

This review answers one question: which FS record layouts should be decoded
before the repo claims native namespace output?

## Evidence

### Spec

- Apple File System Reference defines APFS file-system records as heterogeneous
  records in the file-system tree and defines record families for inodes,
  xattrs, file extents, directory records, sibling links, and sibling maps.

### Observation

- JT Sylve's FS-tree writeup says FS-tree keys encode object ID in the low
  60 bits and record type in the high four bits, and records for one file-system
  object are stored together by object ID then type.
- JT Sylve's inode/directory writeup describes directory records as hierarchy
  placement and inode records as object metadata; hard links require sibling
  records rather than path identity collapse.
- `go-apfs` and `dissect.apfs` model inodes, directory entries, xattrs, sibling
  relationships, dstreams, and symlink target extraction as first-class parser
  concerns.
- `EX-03` and `EX-04` matched mounted oracles for paths, types, file identity,
  logical size, hard links, symlink targets, sparse files, clones, Unicode, and
  case-sensitive/case-insensitive image variants.

### Hypothesis

- Native FS parsing should first emit the existing `NamespaceEntry` shape and
  preserve path graph and file identity separately. Any record family outside
  that output should be deferred unless an oracle mismatch demands it.

## Open Limits

- Native parsing still needs exact binary field tests for each record family.
- Compression field precedence remains covered by logical-size parity only, not
  by full storage semantics.
- Directory stats and file info records are not ruled out forever; they are only
  not required for the current v1 output.

## Decision impact

- `RL-03`: the record matrix is source-backed enough to drive native parser
  modules after checkpoint and OMAP are native.
- `RL-06`: hard links remain core, not a UI polish pass.
- Exact next step: `EX-10` has completed the read-only FS-tree record-family
  dumper. Continue with `SR-014`/`EX-13`: decode record bodies as a field dump
  and compare those fields against same-run mounted/POSIX namespace and
  logical-size oracles before emitting product namespace entries.
