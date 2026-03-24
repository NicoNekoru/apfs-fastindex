# RL-07 Size and Space Accounting

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

## Core Question
- What size metrics will the product report, and how can those metrics be computed correctly on APFS?

## Why This Matters
- "Size" is not a single concept on APFS.
- A WizTree-like tool lives or dies by whether its size numbers make sense.

## Current Assumptions
- Logical file size is easier than physical allocated size.
- Physical accounting may require extent-level analysis.
- Clones, sparse files, and compression complicate "size on disk."

## Known Facts
- APFS supports copy-on-write sharing and modern allocation features.
- Shared storage means naive subtree summation may overcount.

## Unknowns / Open Questions
- Which metrics should v1 support?
  - logical size
  - physical allocated size
  - exclusive size
  - shared size
- How do clones affect per-file and per-directory totals?
- How are sparse files represented?
- How is compression represented?
- Do snapshots retain blocks that should or should not be attributed?
- How should we explain differences from Finder/du if semantics differ?

## Risks if We Get This Wrong
- Totals that are internally inconsistent.
- Overcounting clone/shared data.
- User confusion due to mismatch with macOS tooling.

## Planned Experiments / Demos
1. Sparse file demo: create large sparse files and compare logical vs physical.
2. Clone demo: clone large files and inspect accounting changes.
3. Compression demo: compare compressible and incompressible files.
4. Directory aggregate demo: verify parent totals under mixed file types.

## Evidence Log
- [TBD] Logical vs physical comparison notes.
- [TBD] Clone accounting notes.
- [TBD] Compression notes.

## Interim Decisions
- v1 may need to distinguish "logical size" mode from "physical accounting" mode.

## Exit Criteria
- Defined product-facing size semantics.
- Formula/algorithm for each reported metric.
- A list of known mismatch cases versus other tools.

## Related Logs
- RL-03 FS Tree Topology and Required Records
- RL-05 Subtree Reuse Correctness
- RL-11 Snapshots, Volume Groups, and Firmlinks