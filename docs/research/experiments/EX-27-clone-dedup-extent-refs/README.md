# EX-27 Clone-dedup via extent-reference tree

ID: EX-27
Title: Clone-dedup via extent-reference tree (`oxr_t`)
Date: 2026-05-20
Owner: Claude
Status: Planned (skeleton)
Result: Pending
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

(Pending — skeleton.)

APFS clones share extents on disk. The current scanner counts each clone's
allocated bytes at full per-instance value; the volume's actual allocated
share is `Σ extent.length / refcount`. EX-27 introduces an extent-
reference-tree (`oxr_t` / "object extent reference") walker, computes the
deduplicated allocated bytes for the EX-19/EX-22 fixture (which already
contains clones from `cp -c`), and validates the result against the
mounted oracle `du -A` on the same fixture.

Outcome lands a new "Real Bytes" metric column alongside Logical and
Allocated. WizTree on NTFS is the closest analogue; macOS has no other
GUI tool that reports clone-deduplicated allocated bytes.

## Question

For a fixture containing one or more `cp -c`-created clone families, can
the raw parser walk the extent-reference tree, recover per-extent
refcounts, and compute the volume's deduplicated allocated share such
that the total matches `du -A` on the mounted oracle?

## Hypotheses

- **Hypothesis A** `extent_ref_tree_provides_refcounts`: The
  extent-reference tree as documented (Apple File System Reference,
  "Extent References" section) stores one record per shared extent with
  a refcount field. Walking the tree yields `(physical_block_addr,
  length, refcount)` triples sufficient to compute
  `Σ length / refcount` over the volume; per-file allocated becomes
  `Σ length_i / refcount_i` over that file's extents.
- **Hypothesis B** `du_minus_A_is_the_oracle`: `du -A <path>` on macOS
  reports clone-deduplicated allocated bytes (it does — the `-A` flag
  documents this). EX-27's computed number should match `du -A` on a
  per-directory basis exactly.
- **Hypothesis C** `refcounts_diverge`: Either A or B fails. macOS,
  apfsprogs, and linux-apfs-rw are documented elsewhere as disagreeing
  on related accounting; if so, EX-27 picks the macOS oracle (`du -A`)
  as authoritative and documents the per-tool divergence.

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

- **Per-directory `du -A <path>`** on the mounted fixture. The `-A` flag
  on macOS-bundled `du(1)` reports actual disk usage with clone-share
  divided. Cross-checked against the volume's `df` "used" minus
  container overhead for the whole-volume case.
- **Raw side**: extent-reference-tree walk via new parser surface
  `FsRecordDump.extent_refs: Vec<(paddr, length, refcount)>`.
- **Per-file decomposition**: for each inode, its file-extent records
  (`j_file_extent_val_t`) point at physical blocks; join against the
  extent-ref tree to look up the refcount of each extent.

## Setup

(To be detailed in `artifacts/probe_ex27.py` once the methodology is
locked.)

Outline:

1. Build the extended fixture (EX-19/EX-22 baseline + three new clone
   families).
2. Capture POSIX oracle: `du -A` per file, per directory, whole-volume.
3. Detach and reattach `-nomount -readonly`.
4. New parser code: walk the extent-reference subtree of the
   omap-rooted B-tree; emit `extent_refs` alongside the existing
   records.
5. For each inode, compute the deduplicated allocated bytes by
   summing per-extent `length / refcount`.
6. Compare against `du -A` at file and directory granularity.
7. If parity within the fixture, validate same on a representative
   live home directory using the fallback walker's traversal to
   enumerate files and `getattrlist` to fetch `st_blocks` for the
   non-deduped reference. (Live-volume raw parity is EX-28's concern;
   EX-27 stays on detached `.dmg`.)

## Probe Steps

(Pending — populated when `artifacts/probe_ex27.py` is committed.)

## Verdict slugs

- `validated_clone_dedup`: Hypothesis A + B hold. New "Real" metric
  lands; FFI gains a third bytes column.
- `validated_clone_dedup_with_divergence`: parity within ε for most
  files but a documented per-file divergence pattern (e.g., partial-
  share clones where modification broke sharing on a subset of
  extents). EX-27 ships the metric with the divergence pattern
  documented; manual chapter 8 records the edge case.
- `oracle_inconclusive_clone_dedup`: Hypothesis A or B fails. New
  metric doesn't ship; experiment records the divergence and proposes
  follow-up (read the macOS write-path source for the formula `du -A`
  uses internally).

## Implementation deltas if validated

- New parser code: extent-reference-tree walker in the raw module.
- New FFI surface: `apfs_scan_directory_with_progress` and the
  fallback path both gain a `real_size` column on the entry; the
  fallback path emits the same value as `allocated_size` (no
  refcount info available without raw access), so the metric is
  honest about its coverage.
- New chapter 8 section: "EX-27: Clone-deduplicated allocated bytes".
- New native-renderer metric picker option: "Real" alongside
  Logical / Allocated.
- A new Rust regression test in the raw module asserts the
  computed deduplicated total matches a precomputed fixture
  oracle.

## Risk / fallback

- Extent-reference subtree turns out to be encrypted or otherwise
  not readable without the volume encryption key: unlikely (we
  already read it implicitly during normal extent resolution), but
  if it happens, the metric stays unsupported and the manual
  documents why.
- Refcount divergence with `du -A` on the fixture: the macOS
  oracle wins; document the divergence per-tool.
- Performance: the extent-reference tree is the same order-of-
  magnitude as the fs-tree. The walker reuses the existing
  parallel-walker / mutex-shard discipline. If walk time doubles
  on a `/`-scale scan, the metric is opt-in via a settings toggle
  rather than always-on.

## Not in scope

- Live system disk (EX-28).
- Snapshot extent contribution (EX-29).
- Sparse-of-clone or decmpfs-of-clone pathological shapes; EX-26's
  fixture explicitly carves those out.
