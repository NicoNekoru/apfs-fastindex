# SR-002 Checkpoint, OMAP, and Root-Discovery Contract

Status: Complete
Date: 2026-04-24
Type: Source Review
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-03 FS Tree Topology and Required Records
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

There is enough public and reverse-engineered information to define a concrete
entry contract for a narrow raw parser:

1. read block 0 only as a pointer to checkpoint metadata
2. scan the checkpoint descriptor area
3. choose the valid `nx_superblock_t` with the highest `xid`
4. reject malformed checkpoint state and fall back to an older checkpoint
5. resolve volume superblocks through the container OMAP
6. resolve the file-system root through the volume OMAP
7. walk only the tree roots required for the requested product mode

The parser must carry transaction context and OMAP context all the way down.
The sources do not support a bare-`oid` resolver model.

## Parser entry contract

### 1. Checkpoint selection

- `Spec | Apple/public docs:` block 0 is the place to start, but not the place
  to stop. Apple documents block 0 as the copy of the container superblock used
  to locate the checkpoint descriptor area, then says mount should select the
  valid `nx_superblock_t` with the highest transaction identifier and validate
  the checkpoint's ephemeral objects.
  Source:
  - [Apple File System Reference](https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf)

- `Observation | reverse engineering writeup:` JT Sylve independently describes
  the same flow and explicitly warns that block 0 is not guaranteed to be the
  newest valid state after an unclean unmount.
  Source:
  - [NX Superblock Objects](https://jtsylve.blog/post/2022/12/06/APFS-NX-Superblock)

- `Observation | open-source implementation:` `apfsprogs` implements this by
  scanning the checkpoint descriptor area and keeping the valid `NXSB` with the
  highest `xid`.
  Source:
  - [linux-apfs/apfsprogs](https://github.com/linux-apfs/apfsprogs)

- `Contract:` a scan root is a chosen checkpoint, not "whatever objects happen
  to be newest individually."

### 2. OMAP context

- `Spec | Apple/public docs:` Apple documents OMAP as the structure that maps
  virtual object identity in transaction context to physical storage.

- `Observation | reverse engineering writeup:` the container and each volume
  have their own OMAP, each with a distinct virtual address space. Looking up an
  object requires knowing which OMAP owns it.
  Sources:
  - [Object Maps](https://jtsylve.blog/post/2022/12/12/APFS-OMAP)
  - [Volume Superblock Objects](https://jtsylve.blog/post/2022/12/13/APFS-Volume-Superblock)

- `Observation | reverse engineering writeup:` OMAP keys are ordered by
  `(oid, xid)`, and active-state lookup effectively chooses the highest usable
  `xid` for that `oid` in the relevant OMAP.

- `Observation | open-source implementation:` third-party parsers consistently
  implement XID-aware OMAP lookup rather than plain `oid -> paddr`.
  Sources:
  - [go-apfs](https://github.com/blacktop/go-apfs)
  - [linux-apfs/linux-apfs-rw](https://github.com/linux-apfs/linux-apfs-rw)

- `Contract:` a resolver input is at least:
  - OMAP context
  - `oid`
  - target scan state (`xid` or snapshot context)

### 3. Root discovery

- `Spec | Apple/public docs:` after the latest valid checkpoint is selected, the
  container superblock provides the container OMAP and the set of volume OIDs.

- `Observation | reverse engineering writeup:` the volume superblock then
  provides the volume OMAP and the key tree roots, including:
  - `apfs_root_tree_oid`
  - `apfs_extentref_tree_oid`
  - `apfs_snap_meta_tree_oid`
  - newer integrity or `fext` tree fields on some systems
  Source:
  - [Volume Superblock Objects](https://jtsylve.blog/post/2022/12/13/APFS-Volume-Superblock)

- `Observation | reverse engineering + open-source implementation:` for a narrow
  single-volume namespace + logical-size parser, the critical chain is:
  - checkpoint
  - container OMAP
  - volume superblock
  - volume OMAP
  - file-system root tree

- `Hypothesis | inferred from converging sources:` `apfs_extentref_tree_oid`
  looks important for physical/shared accounting and some validation work, but
  not for the first namespace + logical-size parser target.

## Minimum root set by mode

### Mode: raw single-volume namespace + logical size

Required first:

- latest valid checkpoint
- container OMAP
- selected volume superblock
- selected volume OMAP
- file-system root tree

Likely not required in the first cut:

- extent-reference tree
- snapshot metadata tree
- firmlink or volume-group synthesis
- newer sealed-volume support trees, unless the chosen target environment
  requires them

### Mode: physical/shared accounting

Likely expands to include:

- extent-reference tree
- additional file extent and shared-storage interpretation
- possibly snapshot-aware interpretation

### Mode: boot-root / merged namespace

Expands beyond raw root discovery into:

- volume-role interpretation
- firmlink handling
- explicit merged-view semantics

## Fail-closed conditions

Treat the following as raw-mode hard stops until explicitly supported:

- malformed checkpoint state
- unsupported checkpoint-descriptor layout
- unsupported incompatible feature bits
- unexpected object type or subtype during root discovery
- unknown OMAP layout details required for the chosen environment
- root fields indicating revert, unusual snapshot state, or other non-steady
  volume state that the parser does not yet model

## Decision impact

- `RL-01` should define checkpoint selection as a concrete algorithm, not a
  vague "pick latest" idea.
- `RL-02` should treat OMAP resolution as `(omap, oid, xid-context)` rather
  than `oid -> block`.
- `RL-03` should define required trees by product mode instead of one universal
  parser surface.
- `RL-13` should treat non-standard root-discovery and feature combinations as
  fallback triggers until proven.
