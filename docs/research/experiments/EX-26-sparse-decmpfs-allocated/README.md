# EX-26 SR-019 sparse + decmpfs allocated-size precedence

ID: EX-26
Title: SR-019 sparse and decmpfs allocated-size precedence
Date: 2026-05-20
Owner: Claude
Status: Planned
Result: Pending
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

(Pending — to be filled at execution.)

EX-22 left two SR-019 case classes fail-closed:

1. `regular + dstream + INO_EXT_TYPE_SPARSE_BYTES present → None`
2. `regular + com.apple.decmpfs xattr → None`

EX-22's diagnostics observed that for the sparse case the on-disk
`j_dstream_t.alloced_size` overstates `st_blocks * 512` by exactly the
`sparse_bytes` xfield value. EX-26 formalises that observation as an
oracle hypothesis, runs the same probe against an expanded fixture, and
if Hypothesis A holds, lifts SR-019 for sparse to emit
`Some(alloced_size - sparse_bytes)` instead of `None`.

For decmpfs, EX-26 distinguishes the two on-disk storage shapes (xattr-
stored compressed bytes vs. resource-fork-stored compressed bytes, see
the decmpfs `compression_type` byte) and proposes a per-shape oracle.
Decmpfs is the harder case; partial validation (sparse lifts, decmpfs
stays closed with a sharper diagnostic) is an acceptable result —
EX-22 explicitly modelled the same kind of partial outcome.

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

## Hypotheses

- **Hypothesis A** `sparse_alloc_minus_sparse_bytes`: For regular files
  with a dstream xfield and `INO_EXT_TYPE_SPARSE_BYTES`, the relation
  `dstream.alloced_size - sparse_bytes_xfield == st_blocks * 512`
  holds exactly across the fixture. EX-22 observed this for the single
  EX-19 sparse case; EX-26 widens the fixture (small / medium / large
  sparse files; sparse-then-dense; dense-then-sparse) to test
  generality.

- **Hypothesis B** `decmpfs_fork_stored`: For decmpfs files whose
  `compression_type` byte indicates resource-fork storage (types 4-6,
  i.e. compressed bytes live in a `com.apple.ResourceFork` xattr with
  its own dstream), the file's *primary* dstream's `alloced_size`
  equals `st_blocks * 512`. Rationale: in this shape the data fork is
  empty but the file's `st_blocks` includes the resource fork; if the
  primary dstream tracks both, they line up.

- **Hypothesis C** `decmpfs_xattr_stored`: For decmpfs files whose
  `compression_type` byte indicates xattr-inline storage (types 7-9 in
  practice; the compressed bytes live inline in the `com.apple.decmpfs`
  xattr), there is no extent allocation for the data fork. The
  candidate oracle is `st_blocks * 512 == 0` — i.e. the file occupies
  no extra blocks beyond the inode itself, and SR-019 emits `Some(0)`
  for this case. If `st_blocks` consistently reports a small non-zero
  number on a fresh APFS volume for this case class, the candidate is
  `Some(0)` if we trust the inline xattr to not consume blocks, and we
  document the observed `st_blocks` divergence as residual container-
  overhead.

- **Hypothesis D** `decmpfs_inconclusive`: Neither B nor C holds
  cleanly across the fixture. The kernel reports `st_blocks` in a
  way that doesn't match a simple combination of dstream fields and
  the decmpfs xattr header. SR-019 keeps the fail-closed branch for
  decmpfs; EX-26 documents the per-type divergence and proposes a
  follow-up EX-* to mine the apfsck source for the formula apfs
  uses internally. Sparse still lifts.

- **Hypothesis E** `sparse_inconclusive`: Hypothesis A fails on at
  least one fixture variant. Highly unlikely given EX-22's earlier
  observation, but recorded for completeness. EX-26 documents the
  divergence and SR-019 stays fail-closed for sparse too.

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

(Pending — populated when `artifacts/probe_ex26.py` is committed.
Will follow EX-22's structure: fixture build, oracle capture, raw
parse, per-inode SR-019 decision, summary emit.)

## Verdict slugs

- `validated_sparse_and_decmpfs`: Hypothesis A + B + C all hold.
  SR-019 lifts both case classes; chapter 8 updated; namespace.rs
  emits the new values.
- `validated_sparse_only`: Hypothesis A holds; B or C fails.
  SR-019 lifts sparse; decmpfs stays fail-closed with a per-type
  diagnostic in `summary.json`. Result documented as Hypothesis D.
  This is the most-likely partial outcome.
- `oracle_inconclusive_sparse`: Hypothesis A also fails (surprising
  given EX-22). SR-019 unchanged; experiment records the divergence
  and proposes a follow-up reading of macOS write-path source.
- `oracle_inconclusive_overall`: Rust scanner did not publish
  `selected_checkpoint` — rerun the upstream EX-15-style gate.

## Implementation deltas if validated

- `crates/apfs-fastindex/src/namespace.rs::allocated_size`: replace
  the `regular + dstream + sparse_bytes → None` branch with
  `Some(alloced_size - sparse_bytes)`. Identical to EX-22's existing
  branch shape, just a different return value.
- For decmpfs (if B/C hold): replace the `regular + decmpfs → None`
  branch with a `match` on the `compression_type` byte and the
  per-type formula.
- Chapter 8 of the manual gets a new section "EX-26: SR-019 sparse
  and decmpfs precedence", reusing the chapter 8 framing.
- A new Rust regression test in `namespace.rs::tests` constructs
  synthetic InodeBody + XattrBody fixtures matching the EX-26
  cases and asserts the picked value.

## Risk / fallback

- Hypothesis A fails: very unlikely; EX-22 already saw the relation
  on one fixture. Fallback: documented partial result, no code
  change.
- Hypothesis B/C fail: likely partial outcome. Document per-type
  divergence; sparse still lifts. Decmpfs becomes a follow-up
  experiment (EX-26b or EX-30) once we've read the macOS write-path
  source.
- The decmpfs `compression_type` byte may have undocumented values
  on macOS-produced fixtures. The probe captures the byte verbatim
  and reports per-type counts so a surprise type shows up in the
  artifacts.

## Not in scope

- Container-level overhead reconciliation.
- Sparse-files-with-clones; clone-of-decmpfs. Both are pathological
  combinations that would need their own experiments. Recorded for
  EX-27 as the "shape interaction" to confirm doesn't break the
  EX-27 clone-dedup math.
