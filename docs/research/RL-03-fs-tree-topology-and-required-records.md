# RL-03 FS Tree Topology and Required Records

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

## Core Question
- Which APFS trees and record types are required to build a complete directory index and size model?

## Why This Matters
- We need a minimal-but-correct parser surface for v1.
- Over-parsing increases complexity; under-parsing breaks correctness.

## Current Assumptions
- The FS tree contains enough metadata to reconstruct namespace and file metadata.
- Inodes, directory entries, and possibly extents are the minimum critical record families.
- The required tree set is mode-specific; namespace + logical size should be
  narrower than physical/shared accounting or boot-root synthesis.

## Known Facts
- APFS metadata is distributed across B-trees rather than a single flat table.
- Files, directories, and extents are represented as separate record types.
- Root discovery begins from the chosen checkpoint and flows through container
  OMAP, volume superblock, volume OMAP, and then the file-system root tree.
- Volume metadata also exposes additional roots such as extent-reference and
  snapshot metadata trees, but they may not all be required for every product
  mode.

## Unknowns / Open Questions
- What exact root objects must be discovered from volume metadata?
- Which record types are mandatory for:
  - namespace reconstruction
  - logical file size
  - physical allocated size
  - clone/shared extent accounting
- Are there relevant side trees beyond the obvious FS tree path?
- Which records can safely be ignored in v1?
- What explicit required-tree matrix should the repo use for each product mode?

## Risks if We Get This Wrong
- Missing files.
- Incorrect directory structure.
- Wrong size totals.
- Scope creep from parsing unnecessary structures.

## Planned Experiments / Demos
1. Enumerate all encountered record types on a small test volume.
2. Map each filesystem operation to affected record types:
   - create
   - delete
   - rename
   - chmod/chown
   - file growth
   - clone
3. Determine minimal record set needed for logical-size-only mode.
4. Determine extra record set needed for physical-size mode.

## Evidence Log
- [TBD] Record taxonomy notes.
- [TBD] Root discovery notes.
- [TBD] Minimal parser set notes.
- [2026-04-24] `SR-002` proposed a mode-specific root contract: for raw
  single-volume namespace + logical size, the first critical chain is checkpoint
  -> container OMAP -> volume superblock -> volume OMAP -> file-system root tree.
- [2026-04-24] `EX-03` produced the first source-backed required-record matrix:
  `DIR_REC` + `INODE` + logical-size-bearing inode fields for narrow v1, with
  `SIBLING_LINK` / `SIBLING_MAP` required when hard links are in scope.

## Interim Decisions
- Separate "required for namespace" from "required for accounting."
- Define required trees and required records by product mode, not one universal
  parser surface.
- Treat extent-reference and broader physical/shared-accounting machinery as
  outside the first namespace + logical-size parser target.

## Exit Criteria
- A required-record matrix exists for each product mode.
- We know exactly which trees must be traversed for v1.
- We can explain why excluded record types are non-blocking.

## Related Logs
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-12 Performance Model and Optimization