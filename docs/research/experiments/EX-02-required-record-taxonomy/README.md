# EX-02 Required-record taxonomy for narrow v1

ID: EX-02
Title: Required-record taxonomy for narrow v1
Date: 2026-04-24
Owner: GPT-5.4
Status: Complete
Result: Positive
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle

## Bottom line

The narrow v1 target still looks defensible:

- one APFS volume
- correct namespace
- `logical size` only

But the experiment shows that even this narrow target is not just `DIR_REC +
INODE` in the hand-wavy sense. A minimally correct v1 must account for:

- directory-entry driven path changes
- inode identity persisting across rename and move
- hard-link semantics that immediately break naive path-summed totals
- case-sensitive versus case-insensitive naming behavior
- sparse files where logical size diverges sharply from allocated bytes

The best current required-record matrix is:

- definitely required for narrow v1:
  - `DIR_REC`
  - `INODE`
  - inode size / dstream-related fields used for `logical size`
- required when hard links are present:
  - `SIBLING_LINK`
  - `SIBLING_MAP`
- likely needed for richer metadata fidelity:
  - `XATTR`
  - symlink-target-related metadata as used by real parsers
- not required for narrow v1 namespace + logical-size correctness:
  - extent-reference tree and physical/shared-accounting machinery

## Question

- What is the smallest record set that reproduces correct single-volume
  namespace and logical size, and what additional records are only needed for
  physical accounting or richer edge semantics?

## Hypothesis

- Hypothesis A: there is a clean narrow-v1 parser surface for raw single-volume
  namespace + `logical size`.
- Hypothesis B: once realistic namespace features appear, the parser surface
  immediately balloons into full physical/shared accounting and broader product
  semantics.

## Environment

- Two fresh APFS disk images created for the probe:
  - case-insensitive APFS
  - case-sensitive APFS
- Both mounted live and exercised through the same mutation corpus
- Not encrypted
- Image-backed lab environment

## Oracle

- The oracle was the mounted-view filesystem state after each mutation:
  - path listing
  - inode number
  - link count
  - logical size
  - allocated byte estimate from public metadata (`st_blocks * 512`)
- This oracle is valid for the question because the experiment is validating
  what a narrow parser must reproduce at the namespace and `logical size` layer.

## Setup

- Added a reproducible probe script:
  `artifacts/probe_ex02.py`
- The script creates fresh case-insensitive and case-sensitive APFS images and
  runs the same mutation corpus against both.

## Probe Steps

1. Create `src/` and `dst/` directories.
2. Create `src/base.txt`.
3. Rename it to `src/renamed.txt`.
4. Move it to `dst/moved.txt`.
5. Create a hard link `dst/hard.txt`.
6. Create a sparse file `dst/sparse.bin`.
7. Clone `dst/moved.txt` to `dst/clone.txt` using `cp -c`.
8. Append to `dst/moved.txt`.
9. Create symlink `dst/link.txt`.
10. Run a case probe with `CaseName.txt` and `casename.txt`.

## Expected Observations

### If Hypothesis A is true

- rename and move should preserve inode identity while changing directory-entry
  placement
- sparse files should separate logical from allocated size without forcing full
  physical-accounting support into v1
- hard links should expose a clear policy edge without invalidating a narrow
  namespace + logical-size mode

### If Hypothesis B is true

- the mutation corpus should immediately show that narrow v1 is unrealistic
- logical size should already depend on physical/shared accounting machinery
- case and hard-link behavior should make a minimal parser surface impossible to
  describe

## Observed Results

- Rename and move:
  - the same inode (`20`) survived create -> rename -> move in both volume
    variants
  - this reinforces that path changes are directory-entry work layered on top of
    stable file identity
- Hard links:
  - both `dst/moved.txt` and `dst/hard.txt` shared inode `20`
  - naive path-summed logical total became `12`
  - unique-inode logical total stayed `6`
- Sparse files:
  - `dst/sparse.bin` had logical size `1048576`
  - allocated bytes were only `32768`
  - this is exactly the kind of divergence that argues for `logical size` as the
    canonical v1 metric
- Clones:
  - `cp -c` succeeded on both images
  - `dst/clone.txt` appeared as a separate inode with the same logical size as
    the source file at creation time
  - nothing in the namespace oracle required physical/shared accounting just to
    list it correctly
- Case behavior:
  - case-insensitive APFS rejected exclusive creation of `casename.txt` after
    `CaseName.txt`
  - case-sensitive APFS allowed both names
- Symlinks:
  - symlink entries appeared distinctly in the oracle and therefore cannot be
    treated as ordinary regular files

## Source-backed required-record matrix

This matrix combines the mutation corpus with current external evidence from
`SR-002`, JT Sylve's FS tree posts, `go-apfs`, and the Apple APFS FAQ.

- Path names and directory membership:
  best current record view is `DIR_REC`.
  Confidence: high.
  Why: rename and move change visible paths while preserving inode identity.

- File identity and type:
  best current record view is `INODE`.
  Confidence: high.
  Why: the file survives rename and move as one inode and must still be typed
  correctly.

- `logical size` for regular files:
  best current record view is `INODE` plus dstream/size-related inode fields.
  Confidence: medium-high.
  Why: sparse and cloned files still need correct logical size without pulling
  in physical accounting.

- Hard-link-correct path graph:
  best current record view is `INODE` plus `SIBLING_LINK` / `SIBLING_MAP` when
  present.
  Confidence: high.
  Why: the hard-link step shows why path identity and inode identity cannot be
  conflated.

- Case-sensitive / case-insensitive lookup semantics:
  best current record view is directory-key name hashing and normalization
  rules.
  Confidence: high.
  Why: the case probe shows that key semantics differ by volume mode even when
  path bytes are preserved.

- Symlink target fidelity:
  best current record view is likely `INODE` plus symlink/xattr-related
  metadata used by real parsers.
  Confidence: medium.
  Why: the oracle proves symlinks matter, but this experiment did not inspect
  raw records directly.

- Physical/shared accounting:
  best current record view is extent-reference and richer extent machinery.
  Confidence that this is deferrable for v1: high.
  Why: sparse and clone observations show why this belongs to a later metric
  mode, not the first parser target.

## Artifacts Saved

- `artifacts/probe_ex02.py`
- `artifacts/generated/case-insensitive.json`
- `artifacts/generated/case-sensitive.json`
- `artifacts/generated/summary.json`

## Interpretation

- Narrow v1 is still viable, but it is narrower in exactly the way the repo now
  claims:
  - single volume
  - namespace correctness
  - logical size only
- Hard links are the clearest reason not to oversimplify namespace work. A path
  tree and an inode graph are related but not identical.
- Sparse files and clones support the choice to postpone physical/shared
  accounting without postponing namespace + logical-size parsing.
- Case behavior confirms that namespace work must respect volume mode and name
  semantics from the start.

## What This Rules Out

- It rules out treating hard links as a trivial post-processing detail.
- It rules out conflating logical totals with path-count totals once hard links
  exist.
- It rules out jumping to physical/shared accounting just because sparse files
  and clones exist.
- It rules out assuming one name-handling policy across case-insensitive and
  case-sensitive APFS volumes.

## Impact on RLs

- RL-03: the required-record matrix should explicitly separate:
  - namespace + logical size
  - hard-link support
  - physical/shared accounting
- RL-06: path reconstruction must account for inode stability, directory-entry
  movement, case behavior, and hard-link semantics.
- RL-07: `logical size` remains the correct v1 metric; hard links and sparse
  files show why broader size modes need separate policy.
- RL-10: the mutation corpus and oracle format are good enough to reuse in later
  parser validation.

## Next Exact Step

- Use this matrix to update the `RL-*` logs, then keep the next parser target
  deliberately boring: one volume, one chosen state, correct namespace, and
  logical size only.
