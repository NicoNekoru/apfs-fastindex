# EX-04 Expanded pinned raw-vs-oracle corpus

ID: EX-04
Title: Expanded pinned raw-vs-oracle corpus
Date: 2026-04-25
Owner: GPT-5.5
Status: Complete
Result: Positive
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

The narrow v1 parser contract survived a broader pinned-state corpus on both a
case-insensitive APFS image and a case-sensitive APFS image.

The probe captured a mounted-view oracle, detached the image to freeze a raw
state, walked the detached raw APFS container through the existing raw walker,
and compared normalized output. Both cases matched exactly:

- case-insensitive APFS: `18` raw paths, `18` oracle paths, `0` mismatches,
  pinned highest checkpoint `xid = 21`
- case-sensitive APFS: `19` raw paths, `19` oracle paths, `0` mismatches,
  pinned highest checkpoint `xid = 22`

The corpus covered cross-directory hard links, a 2 MiB sparse file, clone
creation followed by source mutation, symlink target fidelity, Unicode names,
case behavior, a FIFO special file, and a `ditto --hfsCompression` candidate.

## Question

- Does the narrow parser contract still hold on a broader but controlled
  image-backed corpus, or does it immediately expose another required record
  family or semantic policy gap?

## Hypothesis

- Hypothesis A: raw output for a pinned image-backed state can still match the
  mounted oracle for path, entry type, file identity, logical size, symlink
  target, and aggregate facts across the expanded corpus.
- Hypothesis B: the expanded corpus exposes a mismatch that identifies the next
  required record family or narrows the v1 contract.

## Environment

- Host OS: recorded in `artifacts/generated/environment.json`
- Raw traversal library: existing `EX-03` raw walker using `go-apfs`
- Probe volumes:
  - fresh 256 MiB case-insensitive APFS image
  - fresh 256 MiB case-sensitive APFS image
- Encryption: none
- Runtime mode: mounted oracle first, then detached raw state

## Oracle

The oracle was a mounted-view filesystem walk of the same volume immediately
before detach. The raw comparison was performed only after detach, so the raw
side reads one pinned state rather than a live moving checkpoint.

Comparison fields:

- relative path
- entry type
- file identity (`inode` / file id)
- logical size for files and symlinks
- symlink target string

This oracle is valid for the question because the experiment is scoped to
single-volume namespace plus logical size, not physical/shared accounting or
merged boot-root semantics.

## Setup

- Added `artifacts/probe_ex04.py`.
- The script reuses the `EX-03` raw walker and writes generated JSON artifacts
  under `artifacts/generated/`.
- Each APFS image is temporary; only the distilled artifacts are retained.

## Probe Steps

1. Create base directories: `src`, `dst`, `nested/branch/leaf`, `unicode`, and
   `special`.
2. Create, rename, and move `src/base.txt` into `dst/moved.txt`.
3. Create a hard link at `nested/branch/leaf/hard-across-dir.txt`.
4. Create `dst/sparse-2m.bin` with real data at the start and end.
5. Clone `dst/moved.txt` to `dst/clone.txt` using `cp -c`.
6. Append to `dst/moved.txt` after cloning.
7. Create `dst/link.txt` pointing to the cross-directory hard-link path.
8. Try Unicode names: precomposed `cafe-é.txt`, decomposed `cafe-é.txt`, and
   `東京.txt`.
9. Try case-collision names: `CaseName.txt` and `casename.txt`.
10. Create `special/queue.fifo`.
11. Create `dst/compressed-candidate.txt` with `ditto --hfsCompression`.
12. Capture mounted oracle.
13. Detach image, reattach with `-nomount`, identify raw APFS container, record
    checkpoint candidates, run raw walker, and compare.

## Expected Observations

### If Hypothesis A is true

- raw and oracle path sets should match
- hard-linked paths should share file identity
- sparse and cloned files should match logical size without physical accounting
- symlink target should be recovered
- case-sensitive and case-insensitive volumes should diverge only where the
  mounted oracle also diverges
- unsupported or unusual entries should either match as typed `other(...)`
  entries or become the next explicit gap

### If Hypothesis B is true

- comparison should show missing paths, unexpected paths, type mismatches, file
  identity mismatches, logical-size mismatches, or symlink target mismatches
- the mismatch should identify which record family or policy needs a focused
  follow-up before parser work broadens

## Observed Results

- Case-insensitive APFS:
  - `matched: true`
  - oracle path count: `18`
  - raw path count: `18`
  - mismatch count: `0`
  - pinned highest checkpoint `xid`: `21`
  - canonical unique-inode logical total: `2115621`
- Case-sensitive APFS:
  - `matched: true`
  - oracle path count: `19`
  - raw path count: `19`
  - mismatch count: `0`
  - pinned highest checkpoint `xid`: `22`
  - canonical unique-inode logical total: `2115623`
- Both cases:
  - preserved shared file identity for `dst/moved.txt` and
    `nested/branch/leaf/hard-across-dir.txt`
  - preserved symlink target
    `../nested/branch/leaf/hard-across-dir.txt`
  - reported `dst/sparse-2m.bin` logical size `2097152`
  - identified `special/queue.fifo` as `other(DT_FIFO)`
  - rejected the decomposed Unicode sibling after the precomposed name existed
  - created `dst/compressed-candidate.txt`, but it occupied the same allocated
    bytes as its source in this small fixture, so this did not prove compressed
    storage semantics
- The case-collision probe behaved as expected:
  - case-insensitive APFS rejected `casename.txt` after `CaseName.txt`
  - case-sensitive APFS allowed both names

## Artifacts Saved

- `artifacts/probe_ex04.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex04ci-oracle.json`
- `artifacts/generated/ex04ci-pinned-state.json`
- `artifacts/generated/ex04ci-raw-walk.json`
- `artifacts/generated/ex04ci-comparison.json`
- `artifacts/generated/ex04ci-run.json`
- `artifacts/generated/ex04cs-oracle.json`
- `artifacts/generated/ex04cs-pinned-state.json`
- `artifacts/generated/ex04cs-raw-walk.json`
- `artifacts/generated/ex04cs-comparison.json`
- `artifacts/generated/ex04cs-run.json`
- `artifacts/generated/summary.json`

## Interpretation

- The narrow parser surface remains viable across this expanded image-backed
  corpus.
- `SR-003`'s record matrix is strong enough for the next repo-owned parser step:
  `DIR_REC`, `INODE`, logical-size-bearing inode fields, symlink xattrs, and
  hard-link records remain the active surface.
- The experiment strengthens the case for preserving path identity and file
  identity separately. Cross-directory hard links matched because both oracle and
  raw output kept shared file identity explicit.
- Sparse and clone fixtures again show that namespace plus logical size does not
  require physical/shared accounting.
- The compression candidate is intentionally not a compression proof. It only
  shows that this fixture did not break logical-size parity. A dedicated
  accounting probe is still needed before compressed physical-size claims.

## What This Rules Out

- It rules out the claim that the first raw-vs-oracle success was only a tiny
  seven-path fluke.
- It rules out treating FIFO/special-file typing as an immediate blocker for
  narrow v1 in the tested allowlist.
- It rules out broadening v1 into extent-reference or physical/shared accounting
  merely because sparse files, clones, and a compression candidate are present.

## Impact on RLs

- RL-03: the narrow required-record surface survived a broader corpus.
- RL-06: hard links, symlinks, Unicode names, case behavior, and a FIFO all fit
  the current namespace policy in the tested image-backed allowlist.
- RL-07: logical-size parity held for regular, sparse, cloned, hard-linked, and
  symlink entries; compressed physical semantics remain unproven.
- RL-10: the mounted oracle -> detach -> raw walk -> diff pattern is now reusable
  across both case-insensitive and case-sensitive images.
- RL-13: detached image-backed APFS remains inside the allowlist; this result
  should not be generalized to live startup disks or unsupported feature sets.

## Next Exact Step

- Move from corpus expansion to identity tracking: run an OID/paddr/XID/checksum
  probe before designing any persistent cache or subtree-reuse algorithm.
