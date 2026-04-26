# RL-12 Performance Model and Optimization

Status: Open
Priority: P1
Owner: TBD
Last Updated: 2026-04-26

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
- [2026-04-26] Measurement gate added after `EX-05`, `EX-07`, and `EX-09`
  planning: performance work should begin with reproducible full-scan and
  simulated-incremental timings, but no optimization should become an
  implementation requirement until measured against validated correctness
  artifacts.
- [2026-04-26] `SR-012` added a compatibility gate for benchmarks: timing a
  source outside the raw-mode feature/layout allowlist is not valid performance
  evidence for the product mode.
- [2026-04-26] `EX-08` split raw support into checkpoint-scanner-safe,
  checkpoint-context-safe, OMAP-root-safe, namespace-logical-size-safe, and
  product-supported gates. Benchmarks should name the highest gate reached.

## Interim Decisions
- No optimization should be considered "core" until profiled.
- Benchmark plans must name the correctness artifact they are timing. Timing a
  live latest-state scan that cannot be validated is not useful performance
  evidence.
- First measurements should report:
  - image size and source class
  - entry count and FS-tree node count
  - cold/warm cache condition
  - raw read time
  - OMAP/root-resolution time when native parsing exists
  - FS-record traversal time
  - aggregate construction time
  - oracle comparison time
- Treat the `go run` proof backend as a correctness harness, not a production
  performance baseline.
- Treat the Rust checkpoint scanner as a correctness and source-gate component,
  not a performance milestone. Benchmarking should wait until native OMAP/root
  resolution exists.
- Do not compare timings across different support gates as if they were the same
  product mode.

## Exit Criteria
- Baseline benchmark suite exists.
- Major bottlenecks are quantified.
- Optimization roadmap is ranked by measured impact.

## Related Logs
- RL-03 FS Tree Topology and Required Records
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation