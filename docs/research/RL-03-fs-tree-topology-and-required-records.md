# RL-03 FS Tree Topology and Required Records

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-04-26

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
- [2026-04-24] `EX-02` produced the first source-backed required-record matrix:
  `DIR_REC` + `INODE` + logical-size-bearing inode fields for narrow v1, with
  `SIBLING_LINK` / `SIBLING_MAP` required when hard links are in scope.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` consolidated the current v1
  required-tree and required-record set, including symlink-bearing metadata as a
  bounded follow-up gap rather than a reason to widen scope.
- [2026-04-24] `EX-03` closed that remaining gap inside the tested image-backed
  allowlist: the raw walker matched the mounted oracle for hard links, sparse
  files, clones, and symlink target fidelity with zero path or field mismatches.
- [2026-04-25] `SR-003` turned the narrow-v1 record matrix into a source-backed
  contract: `DIR_REC`, `INODE`, dstream or equivalent logical-size fields,
  symlink-bearing `XATTR`, and hard-link `SIBLING_LINK` / `SIBLING_MAP` records
  are in scope; file extents, extent-reference, snapshot metadata, and
  physical/shared accounting remain future modes.
- [2026-04-25] `EX-04` validated that matrix against a broader pinned
  raw-vs-oracle corpus on both case-insensitive and case-sensitive APFS images:
  raw and oracle path sets matched with zero mismatches.
- [2026-04-26] `SR-008` reconfirmed the native v1 record surface:
  `DIR_REC`, `INODE`, dstream or equivalent logical-size fields, symlink
  `XATTR`, and `SIBLING_LINK` / `SIBLING_MAP`; file extents and
  extent-reference parsing remain outside namespace plus logical-size v1.
- [2026-04-26] `EX-10` extended the Rust path with a read-only FS-tree
  record-family dumper (`crates/apfs-fastindex/src/fs_records.rs`). On the
  proof fixture (53 leaf records, 16 unique object IDs) it counted the v1
  scope families exactly: `inode=12`, `dir_rec=13`, `dstream_id=6`,
  `xattr=9`, `sibling_link=2`, `sibling_map=2`, plus `file_extent=9` outside
  v1 namespace scope. `unsupported_record_count=0`. The dumper does not
  decode record bodies, names, or extents. Empirical correction distilled
  into the code: the FS-tree root is reached via the volume OMAP and
  therefore carries a virtual `obj_phys_t`, not physical, so the validator
  must not require `o_oid == paddr` for FS-tree blocks.
- [2026-04-26] Spec/Observation: `SR-014` narrows the next native parser surface
  from record families to required record-body fields. `DIR_REC` must provide
  parent-directory key identity, name, child `file_id`, entry type, and optional
  sibling ID; `INODE` must provide identity, parent/private IDs, mode, link or
  child count, flags, xfields, and dstream metadata; `XATTR` must provide names
  and embedded/stream payload metadata; sibling link/map records must preserve
  hard-link path identity versus target inode identity.
- [2026-04-26] Hypothesis: `EX-13` is the next FS-tree gate. It should dump those
  body fields under the `EX-12` validated OMAP/root context and compare them to
  same-run mounted/POSIX and cross-tool observers before product namespace rows
  are emitted.
- [2026-04-26] Observation: `EX-13` executed as a Python raw-byte record-body
  probe. It decoded `53` FS-tree records with family counts matching `EX-10`
  (`inode=12`, `dir_rec=13`, `dstream_id=6`, `xattr=9`, `sibling_link=2`,
  `sibling_map=2`, `file_extent=9`) and reconstructed all `8` mounted paths with
  no path/type/file-identity mismatches. After the probe recorded and scored
  explicit xfield layout candidates, the sparse-file inode dstream decoded to
  the public logical size and the verdict became
  `validated_native_record_body_contract`.
- [2026-04-26] Observation: The extended `EX-13` probe now writes
  `xfield-layout-summary.json`. The current fixture has `14` records with
  xfields and selected layouts split across `8`
  `record_relative_start_record_relative_fields`, `5`
  `unpacked_start_record_relative_fields`, and `1`
  `unpacked_start_blob_relative_fields`. Four non-row-critical records still
  have true top-score ambiguity, so xfield layout policy remains a Python
  research surface even though the proof fixture rows validate.

## Interim Decisions
- Separate "required for namespace" from "required for accounting."
- Define required trees and required records by product mode, not one universal
  parser surface.
- Treat extent-reference and broader physical/shared-accounting machinery as
  outside the first namespace + logical-size parser target.
- The first parser prototype should target only the record families needed to
  satisfy the narrow v1 contract.
- In the tested narrow-v1 allowlist, symlink target extraction can now be
  treated as part of the active parser surface rather than a speculative add-on.
- The next parser-surface proof should be a broader pinned raw-vs-oracle corpus,
  not more broad taxonomy research.
- Native FS-record parsing should begin as a record dumper with raw key type,
  object ID, decoded family, and unsupported-record counts before it emits
  product namespace entries.
- Native FS-record parsing has enough proof-fixture evidence to advance from a
  family dump to a Python body-field oracle. Product namespace entry emission is
  still a later implementation step, not a claim made by `EX-13`.
- Keep the next body-field variant work in Python. The record-family surface is
  stable enough for experiments, but xfield layout selection should be exercised
  across more field orderings before it is encoded in Rust.

## Exit Criteria
- A required-record matrix exists for each product mode.
- We know exactly which trees must be traversed for v1.
- We can explain why excluded record types are non-blocking.

## Related Logs
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-12 Performance Model and Optimization