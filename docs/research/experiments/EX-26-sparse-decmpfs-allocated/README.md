# EX-26 SR-019 sparse + decmpfs allocated-size precedence

ID: EX-26
Title: SR-019 sparse and decmpfs allocated-size precedence
Date: 2026-05-20
Owner: Claude
Status: Closed
Result: `validated_sparse_and_decmpfs`
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
Related EXs:
- EX-19 SR-017 logical-size precedence (fixture reused)
- EX-22 SR-019 alloced-size precedence (the experiment this amends)
Related docs:
- `spec.md` (SR-019)
- Chapter 8 of the manual ("The size precedence rule")
- `docs/research/plans/coverage-correctness-roadmap.md` Phase 1

## Bottom line

**Validated.** Both fail-closed branches lift.

- **Sparse**: for regular files with a dstream and an
  `INO_EXT_TYPE_SPARSE_BYTES` xfield, the allocated bytes are
  `alloced_size - sparse_bytes`. Verified exactly across four sparse
  shapes (small HEAD/TAIL ~1 MiB hole, ~10 MiB hole, ~50 MiB hole,
  chunked 4 KiB-data-every-64 KiB).
- **Decmpfs**: for regular files with `com.apple.decmpfs`, the
  allocated bytes are the sum of the primary inode's dstream (often
  absent → 0) plus the `stream_dstream.alloced_size` of any
  stream-backed `com.apple.decmpfs` and any stream-backed
  `com.apple.ResourceFork` xattr. An embedded (non-stream) xattr
  contributes 0 because the compressed bytes live inline in the
  xattr payload. Verified on both ditto shapes:
  `compressed.txt` (xattr-stream-stored, decmpfs.stream_dstream=4096)
  and `compressed-big.bin` (resource-fork-stored,
  ResourceFork.stream_dstream=4096).

This replaces Hypotheses B/C/D in the original plan with the unified
Hypothesis F (`primary + decmpfs.stream_dstream +
ResourceFork.stream_dstream`). The on-disk reality is that the
`compression_type` byte in the decmpfs header tells you *where the
bytes live* (xattr inline / resource fork), but the bytes themselves
are always counted by summing the available `stream_dstream`
fields — no compression-type-conditional logic is needed at the
namespace layer.

Implementation lift landed in `crates/apfs-fastindex/src/namespace.rs`
behind the renamed `compute_allocated_size` helper, with 8 new unit
tests covering sparse, sparse-underflow, the two decmpfs storage
shapes, both-stream-backed decmpfs, all-embedded decmpfs, the EX-22
baseline (preserved), and the no-inode fail-closed case.

## Question

For sparse and decmpfs files on a same-run macOS-produced APFS fixture,
which public on-disk fields (or formulas over them) reproduce
`st_blocks * 512` exactly, so we can lift the SR-019 None-collapse
for those case classes?

The current SR-019 precedence (unchanged for non-sparse / non-decmpfs):

- `regular + dstream + no SPARSE_BYTES xfield → Some(dstream.alloced_size)`
- `regular + dstream + SPARSE_BYTES present → None` ← EX-26 candidate
- `regular + com.apple.decmpfs xattr → None`     ← EX-26 candidate
- `symlink → Some(0)`
- `directory → Some(0)`
- anything else → `None`

## Hypotheses and outcomes

- **Hypothesis A** `sparse_alloc_minus_sparse_bytes` — **HELD.**
  For regular files with a dstream xfield and
  `INO_EXT_TYPE_SPARSE_BYTES`,
  `dstream.alloced_size - sparse_bytes_xfield == st_blocks * 512`
  exactly. Verified across four sparse shapes:
  - `sparse.bin` (~1 MiB HEAD/TAIL hole): `1_056_768 - 1_032_192 == 24_576`
  - `sparse-medium.bin` (~10 MiB hole): `10_485_760 - 10_452_992 == 32_768`
  - `sparse-large.bin` (~50 MiB hole): `52_428_800 - 52_396_032 == 32_768`
  - `sparse-chunked.bin` (4 KiB-data-every-64 KiB, 2 MiB total):
    `2_097_152 - 1_572_864 == 524_288`

- **Hypothesis F** `decmpfs_stream_dstream_sum` — **HELD** (supersedes
  the original Hypotheses B/C/D split). For decmpfs files, the
  allocated bytes are the sum of stream-backed allocations:
  - `inode.dstream.alloced_size` (often absent for decmpfs → 0)
  - `xattrs[com.apple.decmpfs].stream_dstream.alloced_size`
  - `xattrs[com.apple.ResourceFork].stream_dstream.alloced_size`
  Each defaults to 0 if absent or if the xattr is embedded (carries
  its bytes inline, occupying no extents).
  Verified on both ditto-produced shapes:
  - `compressed.txt` (52 KiB compressible JSON, ditto stream-backed
    the `com.apple.decmpfs` xattr; decmpfs.stream_dstream.alloced =
    4096, no ResourceFork data fork): picks 4096, oracle 4096.
  - `compressed-big.bin` (256 KiB compressible binary, ditto
    inlined the `com.apple.decmpfs` header with
    `compression_type = 8` lzvn fork-stored and put the compressed
    bytes in `com.apple.ResourceFork.stream_dstream`,
    alloced = 4096): picks 4096, oracle 4096.
  - `compressed-random.bin` (incompressible payload): ditto chose
    not to compress; no `com.apple.decmpfs` xattr emitted; falls
    through to the EX-22 baseline path (`dstream.alloced_size`).
    Oracle 131072, picked 131072.

The compression-type-conditional split that originally distinguished
Hypotheses B and C (fork-stored vs. xattr-inline) collapses into a
single rule once you observe that *both* xattr carriers expose their
allocated bytes via the same `stream_dstream` field. The
`compression_type` byte indicates *which* xattr carries the bytes,
but EX-26's sum-formula is shape-agnostic — it picks up whichever
xattr is stream-backed and ignores embedded ones.

(Original Hypotheses B, C, D, E retained in the git history for
reference; the validated rule is Hypothesis A for sparse and
Hypothesis F for decmpfs.)

## Environment

- macOS version captured at probe time via
  `artifacts/generated/environment.json`.
- APFS source: a generated detached `.dmg` containing the fixture
  files. Same shape as EX-19/EX-22 plus three new sparse variants
  (small/medium/large by hole pattern) and three new decmpfs variants
  (type-4 fork-stored, type-7 xattr-stored small, type-7 xattr-stored
  near-page-boundary). 9-12 fixture files total.
- Mounted phase: fixture creation + POSIX oracle capture
  (`stat -f`, `lsmacattr`, `xattr -p com.apple.decmpfs`).
- Raw phase: detached image, reattached `-nomount -readonly`,
  Rust parser produces `FsRecordDump.records`.
- Out of scope: container overhead, multi-volume containers, encrypted
  volumes, snapshots.

## Oracle

- **Per-inode `st_blocks * 512`** from mounted POSIX `stat(2)`. Same
  oracle EX-22 used. Justified there: SR-019's review pinned this as
  the only oracle that holds case-by-case on macOS.
- **Raw side**: `FsRecordDump.records` from
  `apfs_fastindex_native_dump`, identical to EX-22.
- **Additional reads needed for EX-26**:
  - The `INO_EXT_TYPE_SPARSE_BYTES` xfield value (already in the body
    decoder per EX-18; surface it on `InodeBody`).
  - The `com.apple.decmpfs` xattr payload, specifically the
    `compression_type` byte and `uncompressed_size` field. The xattr
    body decoder lands these as `XattrBody { ..., raw: Vec<u8> }`;
    EX-26 parses the decmpfs header inline.

## Setup

(Detailed in `artifacts/probe_ex26.py` once the methodology lands.)

Outline:

1. Capture environment manifest.
2. Build a fresh APFS image containing:
   - The EX-19/EX-22 baseline (ordinary, sparse-small, clone, hard
     link, symlink, decmpfs-via-ditto).
   - New sparse variants: 1 MiB hole + 1 MiB data; 10 MiB sparse with
     scattered holes; 100 MiB mostly-sparse.
   - New decmpfs variants: `ditto --hfsCompression` on a small JSON
     (likely xattr-stored), on a 200 KiB binary (likely fork-stored),
     and on a near-page-boundary file (16 KiB) to test the type-7
     hypothesis at the boundary.
3. Detach and reattach `-nomount -readonly`.
4. Run the Rust scanner; capture `parser_output.entries` with
   `allocated_size = None` for the fail-closed rows, plus the
   `InodeBody` xfields + `XattrBody` payloads for those inodes.
5. For each inode, compute the candidate per Hypothesis A/B/C.
6. Compare against POSIX `st_blocks * 512`.
7. Emit `summary.json` with one of the verdict slugs below.

## Probe Steps

The probe at `artifacts/probe_ex26.py` runs end-to-end:

1. Capture `environment.json` (`sw_vers`, tool paths, timestamp).
2. `hdiutil create -size 256m -fs APFS -volname EX26CI -nospotlight`.
3. Attach mounted; build the fixture (EX-19 baseline +
   3 sparse + 3 decmpfs variants); capture
   `ex26-mounted-posix-oracle.json` recording `st_blocks * 512` per
   inode. Detach.
4. `hdiutil attach -nomount -readonly`; pick the APFS container
   `/dev/rdiskN`.
5. `cargo run --bin apfs-fastindex-scan -- <raw-container>`; capture
   `ex26-rust-records.json`.
6. For each inode in the oracle: apply Hypothesis A (sparse) or
   Hypothesis F (decmpfs) or the EX-22 baseline; compare against
   `st_blocks * 512`; emit `ex26-precedence-table.json` and
   `summary.json`.
7. Detach the raw image; clean up the scratch tree.

## Verdict slugs

- `validated_sparse_and_decmpfs` (landed): Hypotheses A + F both
  hold across the fixture. SR-019 lifts both case classes.
- `validated_sparse_only`: Hypothesis A holds; Hypothesis F has
  decmpfs mismatches.
- `oracle_inconclusive_sparse`: Hypothesis A also fails.
- `oracle_inconclusive_overall`: Rust scanner did not publish
  `selected_checkpoint` — rerun the upstream EX-15-style gate, or
  any EX-22 baseline row mismatched (the prerequisite for trusting
  the EX-26 hypotheses).

## Implementation deltas

Landed in this commit:

- `crates/apfs-fastindex/src/namespace.rs::allocated_size` now
  delegates to a free helper `compute_allocated_size(entry_type,
  inode, xattrs)` so the rule can be unit-tested directly. The new
  branches:
  - sparse: `Some(alloced_size.saturating_sub(sparse_bytes))`
  - decmpfs: `Some(primary +
    decmpfs.stream_dstream.alloced + ResourceFork.stream_dstream.alloced)`
- New constant `XATTR_RFORK_NAME = "com.apple.ResourceFork"`.
- 8 new unit tests in `namespace.rs::tests`:
  - `ex26_sparse_subtracts_sparse_bytes_from_alloced`
  - `ex26_sparse_underflow_saturates_at_zero`
  - `ex26_regular_emits_dstream_alloced_size` (EX-22 baseline preserved)
  - `ex26_decmpfs_xattr_stream_stored`
  - `ex26_decmpfs_resource_fork_stored`
  - `ex26_decmpfs_both_streams_sum`
  - `ex26_decmpfs_all_embedded_picks_zero`
  - `ex26_symlink_and_dir_emit_zero`
  - `ex26_regular_without_inode_returns_none`
- Module doc comment updated to describe the new EX-26 branches.
- Chapter 8 of the manual updated to add an EX-26 section.
- Top-level `README.md` "Capabilities" line updated to drop the
  "fail closed on sparse and decmpfs" caveat.

## Risk / fallback (recorded, not realised)

- Hypothesis A failure: not realised; the formula held exactly across
  four sparse shapes in addition to EX-22's original case.
- Hypothesis F failure: not realised. The original Hypothesis B/C
  split (compression-type-conditional) was unnecessary; the
  stream-dstream sum is shape-agnostic. Future shape variants that
  carry compressed bytes via a different xattr name (none observed
  on macOS 14-15) would require revisiting.

## Residual unknowns

- Sparse-files-with-clones; clone-of-decmpfs. Both are pathological
  shape interactions that would need their own experiments. EX-27
  treats these as "shape interactions" to confirm don't break the
  EX-27 clone-dedup math.
- Container-level overhead reconciliation. Out of scope for EX-26.
- Files with both a primary dstream AND a stream-backed
  `com.apple.decmpfs` xattr. EX-26's formula sums both, which is
  the conservative correct answer for any future variant where this
  combination appears; the EX-26 fixture didn't exercise this case
  (no ditto-produced shape on macOS 14-15 puts compressed bytes in
  the primary dstream).
