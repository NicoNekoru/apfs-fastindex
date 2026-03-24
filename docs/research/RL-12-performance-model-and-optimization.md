# RL-12 Performance Model and Optimization

Status: Open
Priority: P1
Owner: TBD
Last Updated: TBD

## Core Question
- What are the true performance bottlenecks of raw APFS indexing, and which optimizations matter most?

## Why This Matters
- The project goal is not just correctness; it is WizTree-like responsiveness on repeated scans.

## Current Assumptions
- Initial scan will be slower than NTFS MFT scanning.
- Incremental scans may be very fast if subtree reuse is valid.
- Random I/O and object indirection are likely bottlenecks.

## Known Facts
- APFS lacks a flat metadata table.
- Raw traversal is structurally more complex than NTFS MFT walking.

## Unknowns / Open Questions
- Is the dominant cost:
  - raw I/O
  - OMAP lookup
  - B-tree parsing
  - cache lookup/merging
  - aggregate recomputation
- How much benefit comes from batching reads?
- How much parallelism is safe/useful?
- Does node-summary caching materially reduce CPU as well as I/O?

## Risks if We Get This Wrong
- Overengineering low-impact optimizations.
- Missing the real bottleneck.
- Benchmark claims that do not generalize.

## Planned Experiments / Demos
1. Profile full scans on small and large datasets.
2. Profile incremental scans under low, medium, and high churn.
3. Compare cold-cache vs warm-cache behavior.
4. Compare SSD, external SSD, and disk-image-backed test environments.

## Evidence Log
- [TBD] Baseline scan timings.
- [TBD] CPU vs I/O breakdown.
- [TBD] Incremental scan benchmarks.

## Interim Decisions
- No optimization should be considered "core" until profiled.

## Exit Criteria
- Baseline benchmark suite exists.
- Major bottlenecks are quantified.
- Optimization roadmap is ranked by measured impact.

## Related Logs
- RL-03 FS Tree Topology and Required Records
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation