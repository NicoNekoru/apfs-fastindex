# SR-005 Checkpoint Validation Details

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

The first native checkpoint scanner should tighten the current proof skeleton in
small fail-closed steps:

1. validate block zero as an APFS container superblock locator
2. reject non-contiguous checkpoint descriptor layouts until their B-tree range
   map is implemented
3. scan descriptor blocks for checkpoint superblocks
4. accept checkpoint candidates only after magic, object type, object ID where
   applicable, XID, and checksum validation
5. treat checkpoint-map parsing and ephemeral-object validation as the next
   native milestone before OMAP/root traversal claims

This review does not close native OMAP/root discovery. It only sharpens the
minimum validation that belongs before the Rust scanner reports a pinned
`scan_xid` as anything stronger than a candidate.

## Spec

- `Spec | Apple/public reference as summarized by prior SR-002:` APFS object
  headers contain checksum, object ID, transaction ID, object type, and subtype.
  Object type flags distinguish virtual, physical, ephemeral, encrypted,
  no-header, and nonpersistent objects. Parser validation must mask type flags
  from type identifiers rather than compare the raw field naively.
  Source:
  - [Apple File System Reference](https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf)

- `Spec | Apple/public reference as summarized by prior SR-002:` block zero is
  the locator for checkpoint metadata, not proof of the newest state. The mount
  path selects a valid checkpoint superblock from the checkpoint descriptor area.
  Source:
  - [Apple File System Reference](https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf)

## Observation

- `Observation | reverse-engineering writeup:` an APFS object header is
  little-endian and starts with `o_cksum`, `o_oid`, `o_xid`, `o_type`, and
  `o_subtype`. Fletcher-64 is calculated over the object data after the first
  eight checksum bytes; a mismatch means the object may be partially flushed or
  corrupted.
  Source:
  - [Anatomy of an APFS Object](https://jtsylve.blog/post/2022/12/01/Anatomy-of-an-APFS-Object)

- `Observation | reverse-engineering writeup:` block zero should be validated by
  object type, `NXSB` magic, block size, and checksum before it is used to locate
  the checkpoint descriptor area. If the block size is larger than the initial
  4 KiB read, the block-zero superblock must be re-read at the container block
  size.
  Source:
  - [NX Superblock Objects](https://jtsylve.blog/post/2022/12/06/APFS-NX-Superblock)

- `Observation | reverse-engineering writeup:` `nx_xp_desc_base` uses its most
  significant bit to distinguish a contiguous descriptor area from a
  non-contiguous descriptor range map. In the non-contiguous case, the remaining
  bits point to a B-tree root that maps logical descriptor offsets to physical
  block ranges.
  Source:
  - [NX Superblock Objects](https://jtsylve.blog/post/2022/12/06/APFS-NX-Superblock)

- `Observation | reverse-engineering writeup:` descriptor-area objects include
  checkpoint superblocks and checkpoint maps. Checkpoint superblock candidates
  should be validated like block zero and the valid candidate with the highest
  `xid` should be selected. If later parsing fails, trying the next-highest
  checkpoint can be a recovery strategy, but v1 should not silently slide to a
  different state without recording the fallback.
  Source:
  - [NX Superblock Objects](https://jtsylve.blog/post/2022/12/06/APFS-NX-Superblock)

- `Observation | reverse-engineering writeup:` checkpoint maps are part of the
  selected checkpoint, live in the checkpoint descriptor area, and map ephemeral
  objects stored in the checkpoint data area. They are walked from
  `nx_xp_desc_index` as a circular buffer until `CHECKPOINT_MAP_LAST`, with each
  checkpoint-map object requiring checksum validation.
  Source:
  - [Checkpoint Maps and Ephemeral Objects](https://jtsylve.blog/post/2022/12/07/APFS-Checkpoint-Maps)

- `Observation | open-source implementation:` `apfsprogs` validates block zero
  magic, checksum, and object ID, scans descriptor blocks for `NXSB` objects,
  ignores older and checksum-failing candidates, and reports when no valid
  checkpoint superblock exists. It also rejects unsupported checkpoint descriptor
  trees and checks descriptor/data index bounds.
  Source:
  - [linux-apfs/apfsprogs `apfsck/super.c`](https://github.com/linux-apfs/apfsprogs/blob/master/apfsck/super.c)

- `Observation | reverse-engineering writeup:` OMAP lookup is XID-aware and
  domain-specific. Container and volume OMAPs have separate virtual address
  spaces; active lookup chooses the highest usable `xid` for an object, while
  snapshot lookup ignores mappings newer than the snapshot XID.
  Source:
  - [Object Maps](https://jtsylve.blog/post/2022/12/12/APFS-OMAP)

## Hypothesis

- `Hypothesis | implementation boundary:` for the first Rust scanner, candidate
  checkpoint discovery can safely be narrower than full native parsing if its
  output explicitly records unsupported validation gaps and fail-closed errors.
  It should not feed namespace traversal until checksum validation,
  checkpoint-map validation, and OMAP/root resolution are implemented.

- `Hypothesis | support boundary:` falling back to the next-highest checkpoint
  may be useful later for damaged images, but v1 should first hard-stop on a
  malformed latest checkpoint unless a documented recovery policy and oracle are
  added.

## Open Limits

- This review does not validate non-contiguous checkpoint descriptor trees.
- It does not prove real-image behavior for damaged checkpoints or unclean
  shutdown recovery.
- It does not close checkpoint-map membership validation or ephemeral-object
  loading.
- It does not implement OMAP/root discovery.

## Decision impact

- `RL-01` should treat checksum-validated checkpoint selection and
  checkpoint-map validation as separate milestones. The current Python skeleton
  reaches descriptor scanning, not full checkpoint integrity validation.
- `RL-02` should continue to require `omap_context + oid + scan_xid`; source
  review reinforces that XID filtering is part of object resolution, not an
  optional cache concern.
- The Rust starter may implement block-zero reading, contiguous descriptor-area
  scanning, and fail-closed unsupported-layout errors now, but must not claim
  native APFS parsing until checkpoint maps, container OMAP, volume OMAP, FS
  root, and required FS records are implemented and oracle-validated.
- Exact next step: run the Rust scanner from `EX-10` against detached APFS images
  from the existing proof loop and compare its selected highest candidate XID
  against saved pinned-state artifacts before integrating it with the Python
  skeleton.
