# EX-27 Clone-dedup via extent-reference tree

ID: EX-27
Title: Clone-dedup via extent-reference tree (`oxr_t`)
Date: 2026-05-20
Owner: Claude
Status: Probe closed; Rust port pending
Result: `validated_clone_dedup`
Approach: Python-direct (validate the formula off detached `.dmg`
before adding any Rust parser surface; same discipline as EX-13 →
EX-18 / EX-22 → EX-26).
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
Related EXs:
- EX-22 SR-019 alloced-size precedence (clone case currently emits per-instance allocated bytes)
- EX-26 SR-019 sparse + decmpfs precedence (lifts the other two fail-closed branches; lands first because it shares the EX-22 fixture/methodology)
Related docs:
- `spec.md` (the metric the new "Real" column is added to)
- Chapter 8 of the manual ("The size precedence rule") — adds a section
- Chapter 13 of the manual (architecture; new metric column surfaced through FFI)
- `docs/research/plans/coverage-correctness-roadmap.md` Phase 2

## Bottom line

**Validated.** The Python-direct probe reproduces the on-disk
deduplicated allocated bytes exactly. The validation oracle is the
extent-reference tree itself (the on-disk authority for which physical
extents exist and how many references point at each); `du -A` on macOS
turned out to be an apparent-size oracle (it reads `st_blocks` via
`lstat`, which does **not** dedup clones), so per-path `du` is not a
usable oracle for this metric.

**The validated formula** (joining APFS's three relevant
trees — fs-tree's `inode` records, fs-tree's `file_extent` records, and
the standalone extent-reference tree's `phys_ext` records):

1. For each `file_extent` record (keyed by `dstream_id`, not inode
   obj_id — clones share a single dstream), split its physical byte
   range against the `phys_ext` records it overlaps. Each `phys_ext`
   record carries a `refcnt` (the count of file_extents that reference
   that physical range). Sub-extents not covered by any `phys_ext`
   record have implicit `refcnt = 1`.
2. For each dstream, sum `sub_extent.length / sub_extent.refcnt`
   across all its file_extents → `dstream_dedup_bytes`.
3. Each inode's "Real Bytes" = `dstream_dedup_bytes / dstream.refcnt`
   where `dstream.refcnt` is the count of inodes sharing this dstream
   (from the `dstream_id` record).

**The invariants** (validated exactly on the EX-27 fixture):

- `Σ over dstreams of dstream_dedup_bytes == Σ over phys_ext records
  of (length_blocks × block_size)` exactly.
- `Σ over inodes of (dstream_dedup_bytes / dstream.refcnt) ==
  Σ over phys_ext records of (length_blocks × block_size)` modulo
  integer-division rounding of ≤ `(refcnt - 1)` bytes per
  clone-shared dstream.

The second invariant is what makes the metric sum cleanly across a
treemap: each clone's "Real Bytes" share, summed over a directory,
equals the unique on-disk bytes in that subtree.

**Next step** (separate engineering session): port the formula to the
Rust crate. New surface required:

- B-tree walker for the extent-reference tree (its storage class is
  `OBJECT_TYPE_PHYSICAL` on hdiutil-created images — the OID is a
  paddr directly, no OMAP lookup needed).
- Body decoders for `file_extent` (raw_type 0x8) and `phys_ext`
  (raw_type 0x2) records. These currently emit `Unsupported` from
  the fs-record body decoder.
- A new `real_size` column on `NamespaceEntry`, exposed through FFI
  to the SwiftUI shell as the third option in the metric picker
  (Logical / Allocated / Real). The fallback walker (no raw access)
  emits `real_size == allocated_size` since it has no refcount info.

The fixture, methodology, and exact per-shape numbers are all
captured in `artifacts/probe_ex27.py` and `artifacts/generated/`;
the Rust port has a precise oracle to verify against.

## Question

For a fixture containing one or more `cp -c`-created clone families, can
the raw parser walk the extent-reference tree, recover per-extent
refcounts, and compute the volume's deduplicated allocated share such
that the total matches `du -A` on the mounted oracle?

## Hypotheses and outcomes

- **Hypothesis A** `extent_ref_tree_provides_refcounts` — **HELD,
  with the correction that the refcount is per-physical-extent (not
  per-file-extent), and file_extent records are keyed by dstream_id
  (which clones share), not inode obj_id.** The extent-reference
  tree's `phys_ext` records yield `(paddr_first, length_blocks,
  refcnt, owning_obj_id)` quadruples. The "refcnt" here is the
  number of file_extent records that reference the physical extent
  — typically 1 for non-shared extents, ≥2 for snapshot-retained or
  cross-dstream-shared extents (e.g. after partial-share clones).
- **Hypothesis B** `du_minus_A_is_the_oracle` — **FALSE.** `du -A`
  on macOS reports apparent size (logical) via `st_blocks * 512`,
  which the kernel returns from `lstat` without clone-dedup
  consideration. Each clone reports its full dstream size. `du -A`
  is therefore the un-deduped sum across a directory; useless as
  a per-path oracle for "Real Bytes". This was the productive
  discovery — there is no public per-file macOS oracle for
  clone-deduplicated bytes. Volume-level totals (`df` "used", or
  `Σ phys_ext.length` from the extent-reference tree itself) are
  the only public oracles, and the latter is the authoritative
  internal ground truth.
- **Hypothesis C** `refcounts_diverge` — **FALSE.** The phys_ext
  refcnt and dstream_id refcnt mechanisms are self-consistent on
  macOS-produced fixtures; the math closes within ≤(refcnt - 1)
  byte rounding per clone-shared dstream.

The corrected understanding emerged during the probe: APFS clones
share a single `dstream` object (not separate copies of file_extent
records). A clone's inode has `inode.private_id` pointing at the
shared dstream's obj_id. The `dstream_id` record records the count
of inodes referencing each dstream. Per-inode "Real Bytes" is
`dstream_dedup_bytes ÷ dstream.refcnt`.

The partial-share clone shape (`family-c`: 1 MiB cloned then 256
KiB rewritten in the middle) is the case that exercises phys_ext
refcnt > 1: src and clone share the unmodified extents (refcnt=2),
and the rewritten middle becomes a new phys_ext (refcnt=1) attached
to a new dstream id (35) for the clone. Both src and clone report
640 KiB of "Real Bytes" — the 384 KiB shared region counted at half
+ the side's exclusive 256 KiB.

## Environment

- macOS version captured at probe time.
- APFS source: same generated `.dmg` as EX-19/EX-22 (already contains
  `cp -c` clones), plus three new larger clone families:
  - 100 MiB ordinary file, cloned 4 times (5 instances total).
  - 100 MiB file, cloned once, then the clone modified to break sharing
    on half its extents.
  - 10 MiB sparse file, cloned 2 times (interaction with EX-26 sparse
    case if validated).
- Raw phase only: detached image, reattached `-nomount -readonly`.
- Out of scope: live system disk (EX-28's responsibility), snapshots
  (EX-29's responsibility).

## Oracle

- **Authoritative ground truth**: `Σ over phys_ext records of
  (length_blocks × block_size)`. The extent-reference tree
  enumerates every physical extent the volume's spaceman has
  handed out for file content; the refcnt tells us how many
  references each extent has. Walking it and summing length
  yields the deduplicated total on disk.
- **`du -A` per path is NOT a usable oracle** (apparent / logical
  size on macOS — does not dedup clones). Recorded in
  `ex27-mounted-posix-oracle.json` for completeness; the row-level
  comparison surfaces the un-dedup gap as expected.
- **`df` "used" delta** is a cross-check at the volume level. On
  EX-27's 256 MiB fixture the container overhead is bounded by a
  few hundred KiB, so `phys_ext sum + container overhead ≈ df
  used` to ~3 significant figures. Not used as a primary oracle
  because container overhead requires its own oracle.

## Setup (Python-direct)

The probe reads the on-disk APFS format directly off the detached
`.dmg`, reusing EX-13's B-tree node parsing primitives. No Rust
parser surface is added in this experiment; the Rust port lands
later as a separate engineering step once the formula validates.

Bridge from the existing Rust scanner:

- Run `apfs-fastindex-scan <raw-container>` to get the selected
  checkpoint, block size, container OMAP, volume superblock, and
  the resolved `root_tree_lookup.paddr` (fs-tree root, already
  surfaced today).
- The Rust scanner also reports `volume.summary.extentref_tree_oid`
  but does not yet resolve it through the volume OMAP. The probe
  does this resolution in Python by re-running the OMAP lookup
  (sibling of the fs-tree root lookup) — adding 4 lines to the
  Rust scanner would also work, but the Python-direct contract
  for EX-27 keeps the change local.

New on-disk decoders the probe needs:

- `j_file_extent_key_t` (16 bytes): `hdr.obj_id_and_type` (8) +
  `logical_addr` (8). The high 4 bits of `hdr` are
  `APFS_TYPE_FILE_EXTENT = 0x8`; the low 60 bits are the inode's
  obj_id. The probe uses the inode obj_id to attribute extents.
- `j_file_extent_val_t` (24 bytes): `len_and_flags` (8) +
  `phys_block_num` (8) + `crypto_id` (8). The high 4 bits of
  `len_and_flags` are flags; the low 60 bits are length in
  *bytes*. `phys_block_num` is the first physical block of the
  extent.
- `j_phys_ext_key_t` (8 bytes): `hdr.obj_id_and_type` where the
  low 60 bits of `hdr` are the first physical block paddr.
- `j_phys_ext_val_t` (20 bytes): `len_and_kind` (8) +
  `owning_obj_id` (8) + `refcnt` (4). The high 4 bits of
  `len_and_kind` are the kind (`APFS_KIND_NEW = 1`); the low 60
  bits are length in *blocks* (note the unit difference vs.
  file-extent values, which are in bytes).

## Probe Steps

The probe at `artifacts/probe_ex27.py` does the following
end-to-end (mirrors EX-26's structure):

1. Capture `environment.json` (`sw_vers`, tool paths, timestamp).
2. Build a fresh 1 GiB APFS image (larger than EX-26's 256 MiB
   because clone families intentionally need physical bytes
   beyond a single 4 KiB block to make the dedup math
   distinguishable from rounding noise).
3. Build the fixture:
   - EX-22 baseline (ordinary, clone, hard-link, symlink) for
     regression protection.
   - **Clone family A** (small): 64 KiB compressible file cloned
     4 times (5 instances total). Pre-EX-27 reading: 5 * 64 KiB
     = 320 KiB. EX-27 reading: 64 KiB (one extent, refcnt = 5).
   - **Clone family B** (large): 1 MiB file cloned twice (3
     instances). Pre-EX-27: 3 MiB; EX-27: 1 MiB.
   - **Clone family C** (partial-share): 1 MiB file, cloned;
     then 256 KiB at the middle of the clone is rewritten,
     breaking sharing on that section. Pre-EX-27: 2 MiB; EX-27:
     1 MiB (shared) + 256 KiB (only on clone).
4. Capture POSIX oracle:
   - `du -A` per file and per directory (`-A` reports
     deduplicated allocated bytes on macOS).
   - `st_blocks * 512` per inode (pre-EX-27 per-inode allocated;
     the un-deduped reference).
5. Detach mounted; reattach `-nomount -readonly` to get
   `/dev/rdiskN`.
6. Run `apfs-fastindex-scan` to obtain the selected checkpoint,
   block size, container OMAP, and `volume.root_tree_lookup`.
7. In Python: open the raw device, resolve
   `volume.extentref_tree_oid` through the volume OMAP (whose
   `omap_oid` is already in the Rust output) at the same XID.
8. Walk the extent-reference tree, decode every leaf record as
   `(paddr, length_blocks, refcnt)`. Build a map
   `paddr_first → (length_blocks, refcnt)`.
9. Walk the fs-tree (re-run EX-13's walker), decode every
   `file_extent` leaf as `(inode_obj_id, logical_addr,
   phys_block_num, length_bytes)`. Index by `inode_obj_id`.
10. For each inode, compute the deduplicated allocated bytes as
    `Σ over file_extents of (length_blocks * block_size / refcnt)`
    where `length_blocks = length_bytes / block_size` and
    `refcnt` is looked up by `phys_block_num` (the file-extent's
    first physical block should be the same as the phys_ext
    record's `paddr_first`).
11. Compare against the oracle's `du -A` for each path.
12. Emit `ex27-precedence-table.json` and `summary.json`. Verdict
    slugs as listed below.

## Edge cases the probe explicitly addresses

- **Inode shares extents with itself.** If the file-extent walk
  attributes a paddr to inode 7, and the phys_ext record for that
  paddr has `refcnt = 2`, the math must still divide by 2 — the
  second reference might be from a clone we haven't yet
  encountered or might be from the inode's own duplicate
  attribution (rare but possible after `cp -c` then truncate).
- **File-extent crosses multiple phys_ext records.** A single
  file extent of length 1 MiB might map to two phys_ext records
  if the underlying allocation was non-contiguous. The probe
  splits the file-extent at phys_ext boundaries by looking up
  intermediate paddrs.
- **Decmpfs interaction.** Decmpfs files have no `file_extent`
  records on the data fork; their compressed bytes live in xattr
  dstreams (per EX-26). The probe excludes decmpfs files from
  the EX-27 dedup math because EX-26's rule already handles
  them; EX-27's contribution is the *delta* over EX-26.

## Verdict slugs

- `validated_clone_dedup` (landed): `Σ dstream dedup = Σ phys_ext
  bytes` exactly, and `Σ per-inode share = Σ phys_ext bytes` modulo
  integer-division rounding of ≤ refcnt-1 bytes per clone-shared
  dstream.
- `validated_clone_dedup_with_divergence`: dstream totals close but
  per-inode share residue exceeds the rounding bound (would indicate
  a dstream.refcnt vs. inode-count miscount).
- `oracle_inconclusive_clone_dedup`: `Σ dstream dedup ≠ Σ phys_ext
  bytes` — the file_extent decode or phys_ext walk is wrong.
- `probe_blocked_no_extentref`: extent-reference tree storage class
  is virtual and the Rust scanner's volume OMAP lookup did not
  surface `extentref_tree_lookup` (the OMAP doesn't carry the
  mapping at the selected XID).
- `probe_blocked_omap_overflow`: an internal node of one of the
  B-trees references an OID outside the Rust scanner's 8-entry
  `volume_omap.sample_mappings`. Not realised on the EX-27 fixture
  (both trees fit in single leaf-root nodes).

## Per-shape result table

From `ex27-precedence-table.json`:

| path                  | inode | private_id | dstream.refcnt | st_blocks*512 | dstream.raw | dstream.dedup | inode share |
|-----------------------|------:|-----------:|---------------:|--------------:|------------:|--------------:|------------:|
| family-a/src.bin      |    23 |         23 |              5 |        65,536 |      65,536 |        65,536 |      13,107 |
| family-a/clone-1.bin  |    24 |         23 |              5 |        65,536 |      65,536 |        65,536 |      13,107 |
| family-a/clone-2.bin  |    25 |         23 |              5 |        65,536 |      65,536 |        65,536 |      13,107 |
| family-a/clone-3.bin  |    26 |         23 |              5 |        65,536 |      65,536 |        65,536 |      13,107 |
| family-a/clone-4.bin  |    27 |         23 |              5 |        65,536 |      65,536 |        65,536 |      13,107 |
| family-b/src.bin      |    29 |         29 |              3 |     1,048,576 |   1,048,576 |     1,048,576 |     349,525 |
| family-b/clone-1.bin  |    30 |         29 |              3 |     1,048,576 |   1,048,576 |     1,048,576 |     349,525 |
| family-b/clone-2.bin  |    31 |         29 |              3 |     1,048,576 |   1,048,576 |     1,048,576 |     349,525 |
| family-c/src.bin      |    33 |         33 |              1 |     1,048,576 |   1,048,576 |       655,360 |     655,360 |
| family-c/clone.bin    |    34 |         35 |              1 |     1,048,576 |   1,048,576 |       655,360 |     655,360 |
| ordinary.txt          |    16 |         16 |              1 |           512 |       4,096 |         4,096 |       4,096 |
| hard.txt              |    16 |         16 |              1 |           512 |       4,096 |         4,096 |       4,096 |
| link.txt              |    21 |       —    |              — |           512 |          —  |            —  |           0 |

- Family-a: 5 clones share a single 64 KiB extent. Each
  reports 13,107 = 65,536 ÷ 5 as its "Real Bytes" share. Sum:
  65,535 (1-byte rounding residue).
- Family-b: 3 clones share a 1 MiB extent. Each reports 349,525 =
  1,048,576 ÷ 3. Sum: 1,048,575 (1-byte rounding residue).
- Family-c: src + clone after the middle 256 KiB was rewritten.
  Each reports 640 KiB = 384 KiB shared at refcnt=2 (192 KiB)
  + 256 KiB exclusive (counted in full) + 384 KiB shared at
  refcnt=2 (192 KiB). The shared paddrs end up split into three
  phys_ext records (96 + 64 + 96 blocks at the boundary).
- `ordinary.txt` and `hard.txt` share inode 16; the row appears
  twice because the path-to-inode map produces it for both names.

## Rust port plan (next session)

- Add `j_file_extent_*` body decoding to
  `crates/apfs-fastindex/src/fs_record_body.rs` (raw_type 0x8).
  Currently emits `Unsupported`.
- Add `j_phys_ext_*` body decoding for the extent-reference tree's
  records (raw_type 0x2 in the extentref tree, *not* the fs-tree).
  Currently emits `Unsupported`.
- Add an extent-reference-tree walker in `fs_records.rs` (or a
  sibling module): reuse the existing `BtreeNode` parser; for
  internal nodes, branch on the tree's storage class (`OBJECT_TYPE_
  PHYSICAL` vs. `OBJECT_TYPE_VIRTUAL`) to decide whether to OMAP-
  resolve internal-node values.
- Surface the extent-reference records on `FsRecordDump` (e.g.
  `pub extent_refs: Vec<PhysExtRecord>`).
- Implement the dstream-aware dedup math in `namespace.rs`:
  build `dstream_id → refcnt`, `dstream_id → file_extents`,
  `paddr → phys_ext`, then per-inode `real_size = dstream_dedup ÷
  dstream.refcnt`. Allocate the rounding residue to the source
  inode (the one whose obj_id equals private_id) so per-directory
  totals match the phys_ext sum exactly.
- New `real_size: Option<u64>` field on `NamespaceEntry`. The
  fallback walker emits `real_size == allocated_size` (no refcount
  info available without raw access — honest about coverage).
- New native-renderer metric picker option: Logical / Allocated /
  Real, alongside the EX-26-amended Allocated.
- Chapter 8 of the manual: new "EX-27: Real Bytes" section.

Regression test: synthetic InodeBody + dstream_id + file_extent +
phys_ext fixtures matching the EX-27 fixture's three clone-family
shapes, asserting the dedup math reproduces the validated table
above.

## Risk / fallback log

- Extent-reference tree turned out to be `OBJECT_TYPE_PHYSICAL`
  (not virtual) on hdiutil-created `.dmg` fixtures, so the OID is
  the paddr directly — no OMAP lookup. The probe handles both
  storage classes; live boot-volume disks may differ but the
  Python-direct probe doesn't validate that case (EX-28 covers
  live raw mode).
- `du -A` turned out to be apparent-size, not deduplicated. The
  probe records this divergence and uses `Σ phys_ext bytes` as
  the authoritative oracle; per-path `du -A` is recorded as a
  side reference.
- Per-inode rounding residue: integer division of
  `dstream_dedup ÷ refcnt` loses ≤(refcnt-1) bytes per
  clone-shared dstream. The Rust port will allocate the residue
  to the source inode (the one whose obj_id == private_id) so
  per-directory totals match `Σ phys_ext bytes` exactly. The
  visualisation impact is sub-pixel.

## Not in scope

- Live system disk (EX-28).
- Snapshot extent contribution (EX-29).
- Sparse-of-clone or decmpfs-of-clone pathological shapes; EX-26's
  fixture explicitly carves those out.
- The Rust port itself, which is the next engineering phase. This
  EX-27 closure validates the formula; the implementation lands
  separately.
