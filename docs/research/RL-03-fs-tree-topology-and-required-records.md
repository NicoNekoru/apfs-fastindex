# RL-03 FS Tree Topology and Required Records

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-05-14

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
- [2026-05-13] Observation: `EX-14` attempted the required second xfield-layout
  fixture pass, but the retained fixture blocked before FS-record body decode.
  Rust reached source gating and checkpoint descriptor scanning on a detached
  unencrypted APFS image (`4` valid candidates, `highest_xid=20`) but returned
  no `selected_checkpoint` after `APFS object validation failed: checksum
  mismatch at block 1031`. The mounted/POSIX oracle was saved (`16` entries,
  `10` files, `2` sparse candidates), but no xfield records were decoded.
- [2026-05-13] Spec/Observation: `SR-015` explains EX-13's apparent xfield
  layout split as one source-backed cursor rule: values start immediately after
  `xf_blob_t` metadata and each value consumes `round_up(x_size, 8)` bytes.
- [2026-05-13] Spec/Observation: `SR-016` turns record-body parsing into an
  explicit gate for fixed-size bodies, variable-length names, xfields, xattrs,
  sibling records, and drec/inode type consistency before namespace rows emit.
- [2026-05-14] Observation: `EX-18` proves Rust body-field dump parity with
  the Python EX-13 + EX-16 parser on the proof fixture. 53 records on both
  sides, identical `(node_paddr, entry_index)` set, zero divergent fields
  across `(family, key, value, key_len, value_len, xfields,
  xfield_used_data, xfield_padded_total, xfield_unused_trailing_bytes,
  validation_notes, object_id, raw_type)`. The Rust crate's
  `FsRecordDump.records` now carries DIR_REC/INODE/XATTR/SIBLING_LINK/
  SIBLING_MAP/DSTREAM_ID rows under SR-015 cursor + SR-016 hard-stop
  gates; product `NamespaceEntry` rows still wait on SR-017 (EX-19) and
  SR-018 (EX-20).
- [2026-05-14] Observation: `EX-17` enforces the SR-016 fail-closed
  boundary via 21 Rust unit tests in
  `crates/apfs-fastindex/src/fs_record_body.rs::tests`, one per
  per-record case (short bodies, malformed names, duplicate xfields,
  xfield bounds, xattr flag combinations, wrong xfield sizes, sibling_link
  name overflow, sibling_map short value, drec entry type outside POSIX
  allowlist, `xf_used_data` mismatch, xfield blob shorter than header).
  Each test asserts a typed `ScanError::InvalidObject` with an SR-016
  substring. Cross-record cases (drec entry-type vs inode mode, missing
  sibling_map for drec with `DREC_EXT_TYPE_SIBLING_ID`) remain EX-19+
  work.
- [2026-05-14] Observation: `EX-16` executed the SR-015 single cursor replay
  on the EX-13 proof fixture. All `14` records with xfields satisfy
  `xf_used_data == sum(round_up(x_size, 8))`; per-record oracle constraints
  (inode name in dir_rec/sibling name set, dstream size equals mounted
  logical_size, sparse_bytes within logical size, drec sibling_id maps to the
  child file_id) all pass with `0` failures; namespace and per-file
  logical-size reconstruction (using the SR-015-decoded xfields) matches the
  mounted POSIX oracle with `0` mismatches. The sparse-file inode that
  EX-13 had needed candidate scoring for (`oid=23`, three xfields:
  NAME=11→16, DSTREAM=40, SPARSE_BYTES=8) decodes cleanly under one rule.
  SR-015 is now an Observation backed by an executed probe and may be
  encoded into Rust body decoding (gated by EX-18 byte-for-byte field diff).
- [2026-05-14] Observation: `EX-15` resolved the EX-14 block-1031 blocker. The
  FS-tree topology contract is tighter than the prior Rust code expected:
  FS-trees are **virtual** B-trees, and each internal-node 8-byte value is a
  **child virtual OID**, not a physical paddr. The walker must resolve every
  internal value through the volume OMAP at `(child_oid, max_xid)` before
  reading the child block. Confirmed against `linux-apfs-rw` /
  `dissect.apfs` / `go-apfs` traversal behavior, against `fsck_apfs -n`,
  and via two synthetic regression tests
  (`fs_tree_internal_value_is_virtual_oid_resolved_via_omap` and
  `fs_tree_internal_oid_missing_from_omap_is_hard_stop`). Patched in
  `crates/apfs-fastindex/src/fs_records.rs`; `dump_fs_records` now requires a
  `&OmapResolver` argument, and the caller in `lib.rs` passes the volume OMAP
  resolver it already opens for `root_tree_virtual_oid` lookup. Re-running
  Rust against the EX-15 fixture now publishes `selected_checkpoint` with
  `fs_records_dumped_count = 1` and family counts `inode=20, xattr=17,
  sibling_link=2, dstream_id=12, file_extent=18, dir_rec=21, sibling_map=2`.

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
- `EX-14` means the next body-field variant is blocked by checkpoint/root
  context on the variant fixture. Do not implement Rust FS-record body structs
  until a focused checkpoint-context successor explains the block `1031`
  checksum failure or produces a stable selected root for the variant.
  **Closed by EX-15**: the EX-14 blocker was an FS-tree internal-OID
  resolution bug, not a checkpoint/OMAP-context problem. With the patch,
  Rust now reaches the FS-record family dumper on the EX-14 fixture shape;
  Rust FS-record body structs remain gated by EX-16 (SR-015 xfield replay)
  and EX-18 (Rust body field dump diff against Python).
- Replace EX-13 candidate scoring with the deterministic xfield cursor rule only
  after a replay gate records `xf_used_data` and revalidates the saved body
  oracle. Do not port the candidate scorer into Rust. **Done by EX-16**:
  cursor rule validated; candidate scorer stays in Python history only.

## Exit Criteria
- A required-record matrix exists for each product mode.
- We know exactly which trees must be traversed for v1.
- We can explain why excluded record types are non-blocking.

## Related Logs
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-12 Performance Model and Optimization
