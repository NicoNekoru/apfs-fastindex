# EX-03 Pinned-state raw-vs-oracle proof loop

ID: EX-03
Title: Pinned-state raw-vs-oracle proof loop
Date: 2026-04-24
Owner: GPT-5.4
Status: Complete
Result: Positive
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle

## Bottom line

The narrow v1 parser target now has its first direct proof loop.

On a small APFS image-backed corpus, a mounted-view oracle was captured, the
image was detached to freeze one explicit raw state, and a raw walker then read
that pinned state from the detached container device.

For the seven non-root paths in the corpus, raw output matched the oracle with
zero missing paths, zero unexpected paths, and zero field mismatches for:

- path
- entry type
- stable file identity
- `logical size`
- symlink target fidelity in the tested case

The pinned highest checkpoint observed in the detached image was `xid = 14`.

## Question

- Can one explicitly pinned raw APFS state reproduce the same narrow-v1
  namespace and `logical size` output as the mounted-view oracle for the same
  controlled corpus?

## Hypothesis

- Hypothesis A: once the state is pinned, a raw parser can match the mounted
  oracle for narrow v1 on a small controlled corpus.
- Hypothesis B: even with an explicitly pinned state, raw parsing still misses
  key namespace or `logical size` facts needed for the first parser prototype.

## Environment

- Host OS: macOS `26.3.1`
- Probe volume:
  - fresh 160 MiB APFS image
  - case-insensitive
  - unencrypted
  - image-backed lab environment
- Raw parser harness:
  - repo-owned proof loop wrapper in `artifacts/probe_ex03.py`
  - experimental raw walker in `artifacts/rawwalk/`
  - `go-apfs` `v1.0.27` used as the raw traversal library inside that harness

## Oracle

- The oracle was the mounted-view filesystem state captured immediately before
  detaching the image.
- Comparison fields were:
  - relative path
  - entry type
  - file identity (`inode` / file ID)
  - `logical size`
  - symlink target string for symlink entries

This oracle is valid for the question because the experiment is scoped exactly
to narrow v1 output on one pinned state.

## Setup

- Created a fresh APFS image and mounted it.
- Applied a small mutation corpus derived from `EX-02`:
  - create + rename + move
  - hard link
  - sparse file
  - clone
  - append
  - symlink
- Captured the mounted-view oracle.
- Detached the image to freeze the final state.
- Re-attached the image with `-nomount`, identified the raw APFS container
  device, and recorded the latest visible checkpoint information.
- Ran the raw walker against that detached container device and diffed its JSON
  output against the oracle snapshot.

## Probe Steps

1. Create `src/` and `dst/`.
2. Create `src/base.txt`.
3. Rename it to `src/renamed.txt`.
4. Move it to `dst/moved.txt`.
5. Create hard link `dst/hard.txt`.
6. Create sparse file `dst/sparse.bin`.
7. Clone `dst/moved.txt` to `dst/clone.txt`.
8. Append to `dst/moved.txt`.
9. Create symlink `dst/link.txt -> moved.txt`.
10. Capture mounted oracle.
11. Detach image and pin raw state.
12. Walk the detached raw container and diff against the oracle.

## Expected Observations

### If Hypothesis A is true

- raw output should match the oracle for the tested narrow-v1 fields
- hard-linked paths should share one file identity in both views
- sparse-file and cloned-file `logical size` should not require physical/shared
  accounting
- symlink handling should remain a typed namespace entry rather than a plain
  regular file

### If Hypothesis B is true

- the raw walk should miss paths, mis-type entries, or disagree on file identity
  or `logical size`
- one of the remaining medium-confidence gaps should require a dedicated
  microprobe before a first parser plan can be written

## Observed Results

- Pinned raw state:
  - highest visible checkpoint `xid`: `14`
- Comparison result:
  - `matched: true`
  - `oracle_path_count: 7`
  - `raw_path_count: 7`
  - `missing_paths: []`
  - `unexpected_paths: []`
  - `mismatch_count: 0`
- Raw and oracle both reported:
  - `dst/moved.txt` and `dst/hard.txt` as distinct paths with shared file
    identity `20`
  - `dst/sparse.bin` logical size `1048576`
  - `dst/clone.txt` logical size `6`
  - `dst/link.txt` as a symlink with target `moved.txt`
- Aggregate summaries also matched:
  - naive logical total: `1048604`
  - unique-inode logical total: `1048593`

## Artifacts Saved

- `artifacts/probe_ex03.py`
- `artifacts/rawwalk/go.mod`
- `artifacts/rawwalk/go.sum`
- `artifacts/rawwalk/main.go`
- `artifacts/generated/environment.json`
- `artifacts/generated/oracle.json`
- `artifacts/generated/pinned-state.json`
- `artifacts/generated/raw-walk.json`
- `artifacts/generated/comparison.json`
- `artifacts/generated/summary.json`
- `artifacts/generated/run.json`

## Interpretation

- The repo now has one successful end-to-end proof that a pinned raw APFS state
  can reproduce the narrow v1 contract on a controlled corpus.
- This is the missing bridge between the earlier boundary-setting work (`EX-01`)
  and the required-record matrix (`EX-02`).
- In the tested environment, symlink target fidelity is no longer a vague
  medium-confidence claim; the raw walker recovered the correct symlink target
  and matched the mounted oracle.

## What This Rules Out

- It rules out the claim that the project still needs broad prerequisite
  research before writing a first parser prototype plan.
- It rules out treating symlink handling as an unbounded unknown in the narrow
  image-backed environment used here.
- It rules out the idea that narrow-v1 correctness already depends on
  physical/shared accounting machinery.

## Impact on RLs

- RL-01: state pinning is now not just a source-backed rule but a successful
  proof-loop assumption.
- RL-03: the current required-record matrix is good enough to drive a first
  parser plan in this environment.
- RL-06: hard links and symlinks have both been exercised through a raw-vs-oracle
  comparison.
- RL-07: `logical size` remains sufficient for the narrow v1 proof target.
- RL-10: the repo now has a concrete reusable "mounted oracle -> detach ->
  raw walk -> diff" validation loop.

## Next Exact Step

- Draft the first raw parser prototype plan around this closed contract and
  proof loop, without reopening broad research tracks.
