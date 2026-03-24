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

## Known Facts
- Full correctness and incremental correctness are separate problems.
- Many edge cases will only appear under targeted demos.

## Unknowns / Open Questions
- What is the best oracle for each output metric?
- How do we compare against user-visible namespace on modern macOS?
- What test corpus is needed to cover APFS edge behavior?
- How do we detect silent incremental bugs?

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

## Interim Decisions
- Every optimization must be validated against a fresh full-scan oracle.

## Exit Criteria
- Automated regression suite exists.
- Golden corpus exists.
- Incremental engine is continuously compared against full reparse output.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-09 Cache Persistence and Invalidation