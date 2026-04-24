# RL-10 Validation Corpus and Oracle

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

## Core Question
- How do we prove the parser and incremental engine are correct?

## Why This Matters
- Reverse-engineered raw parsing needs a disciplined correctness process.
- Performance claims are irrelevant if correctness is not measurable.

## Current Assumptions
- We need both:
  - a golden test corpus
  - an oracle for comparison
- The oracle may vary by feature:
  - POSIX traversal for namespace
  - system tools for sizes
  - snapshots for stable comparison
- Negative and inconclusive results should be preserved as first-class evidence,
  not discarded.

## Known Facts
- Full correctness and incremental correctness are separate problems.
- Many edge cases will only appear under targeted demos.
- "The oracle" is not one thing; validation must be feature-specific and scoped
  to the exact product mode under test.

## Unknowns / Open Questions
- What is the best oracle for each output metric?
- How do we compare against user-visible namespace on modern macOS?
- What test corpus is needed to cover APFS edge behavior?
- How do we detect silent incremental bugs?
- What minimum artifact set should every experiment save so that future work can
  reuse it?

## Risks if We Get This Wrong
- Shipping a parser that appears to work on happy-path volumes only.
- Regression blindness as reverse engineering progresses.

## Planned Experiments / Demos
1. Build a corpus matrix covering:
   - create/delete
   - rename/move
   - hard links
   - sparse files
   - clones
   - compression
   - snapshots
   - case-sensitive names
   - Unicode edge cases
2. Compare raw parser output to POSIX/API traversal on stable snapshots.
3. Run incremental scans after each mutation and diff against a fresh full scan.
4. Add fuzz-style small-volume mutation sequences and compare results.

## Evidence Log
- [TBD] Initial corpus definition.
- [TBD] Oracle comparison method.
- [TBD] Incremental diffing framework notes.
- [2026-04-24] The research documentation schema was split into `RL-*`, `SR-*`,
  and `EX-*` artifacts, with experiments required to record environment, oracle,
  expected outcomes, observed results, and what the result rules out.
- [2026-04-24] `EX-01` combined a mounted-view oracle with direct raw checkpoint
  observation, which is the pattern future live-state probes should follow.
- [2026-04-24] `EX-03` established a reusable mutation corpus covering rename,
  move, hard link, sparse file, clone, symlink, and case behavior across both
  case-insensitive and case-sensitive APFS volumes.

## Interim Decisions
- Every optimization must be validated against a fresh full-scan oracle.
- Namespace, size, incremental behavior, and boot-root semantics may require
  different oracles.
- Raw outputs belong in `artifacts/`, while distilled conclusions belong in the
  experiment `README.md` and then back in the relevant `RL-*` logs.

## Oracle Matrix

- `Raw single-volume namespace`:
  POSIX/API traversal of the same chosen volume or stable mounted view.
- `Logical size`:
  public file metadata APIs and tools that report logical size for the chosen
  files and directories.
- `Allocated size`:
  public metadata APIs only for explicitly scoped cases; do not generalize to
  clone-, compression-, or snapshot-heavy semantics without proof.
- `Incremental correctness`:
  compare incremental output against a fresh full reparse of the same selected
  state.
- `Boot-root or merged namespace semantics`:
  only use a user-visible macOS root oracle when that exact semantic mode is the
  question under test.

## Artifact Policy

Every `EX-*` note should save at least:

- environment manifest
- oracle definition
- exact setup and probe steps
- expected A/B observations
- observed results
- artifact list
- interpretation
- what the result rules out
- impact on related `RL-*` logs
- next exact step

Negative and inconclusive results remain valid evidence if they narrow the
design space.

## Exit Criteria
- Automated regression suite exists.
- Golden corpus exists.
- Incremental engine is continuously compared against full reparse output.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-09 Cache Persistence and Invalidation