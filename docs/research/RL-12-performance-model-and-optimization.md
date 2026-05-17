# RL-12 Performance Model and Optimization

Status: Open (parallel-walker slice landed; remaining lanes documented)
Priority: P1
Owner: TBD
Last Updated: 2026-05-16 (EX-25)

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
- [2026-05-16] **`SR-021` synthesises four independent research
  reports** (xnu source, Spotlight, OSS-tool survey, jwalk/rayon
  prior art) into a cost map for the post-r2c-fallback-perf
  scanner's remaining ~500 ms / 63% sys-CPU on /Applications.
  Four levers identified: parallelism (highest leverage),
  attribute-mask shrink (deferred Phase-2 refactor), Spotlight
  (ruled out structurally), `fts(3)` (worth A/B testing).
  Bigger `getattrlistbulk` buffer is **not a lever** (syscall
  framing < 0.1% of budget at ~2k syscalls/sec).
- [2026-05-16] **`EX-24` falsifies the attribute-mask lever** on
  modern Apple silicon: drec_only (312k ent/s) and
  current_walker (310k ent/s, with `ATTR_FILE_TOTALSIZE` +
  `ATTR_FILE_ALLOCSIZE`) are within 1% on every dimension —
  contradicts SR-021's prior-evidence-based hypothesis that the
  vnode-rage path (xnu `vfs_attrlist.c:4436` +
  `vfs_subr.c:7233-7256`) imposes a measurable cost. The Phase-2
  deferred-attribute refactor is **deprioritised**; recording it
  here so a future engineer doesn't re-invent the hypothesis.
  Bonus: `fts(3)` is 1.7× slower than `getattrlistbulk` on this
  fixture; Tempel-2019's reverse finding does not replicate on
  macOS 14+; EX-26 cancelled.
- [2026-05-16] **`EX-25` validates parallel scaling at T=4**
  (2.47× of T=1 on /Applications) and **reproduces the
  Szorc-2018 / Apple-DTS-2025 APFS-container-lock prediction**
  at T > 4: T=8 paid 4× T=1 sys-CPU for 1.94× throughput; T=14
  paid 9.3× sys for 1.38× throughput. The Rust slice (landed
  this commit) defaults `--threads min(hw.physicalcpu, 4)` and
  produces +56% end-to-end throughput on /Applications
  (200k → 313k ent/s); cumulative since pre-perf baseline is
  **+82% (1.82× over 172k ent/s)**. Test count delta: 69 → 70
  (`parallel_walker_matches_serial_shape`).
- [2026-05-16] Future direction: the production scanner's
  end-to-end throughput (313k ent/s at T=4) is bounded below
  the EX-25 microbench's kernel ceiling (776k ent/s at T=4)
  by ~460k ent/s of user-space post-processing tax —
  `NamespaceEntry` allocation, per-worker merge, final
  `sort_unstable_by`, `build_aggregates`. This is the next
  bounded lever if more performance is required. Not load-
  bearing today.

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
- **R2-C parallel walker (post-EX-25)**: ship a per-directory
  worker pool by default with `--threads min(hw.physicalcpu, 4)`.
  The 4 ceiling keeps the scanner clear of the APFS container-
  lock regime that fires at T > 4 on modern macOS. `--threads N`
  lets users tune their own host; `--threads 1` reverts to the
  serial walker and re-enables live `--progress` events (silenced
  in parallel mode because the FnMut callback contract is not
  Send). Larger ceilings should not be considered without a
  fresh EX that re-measures the contention shape on the target
  kernel — Szorc-2018's catastrophic-at-high-T finding has been
  reproduced on Apple silicon in 2026.

## Exit Criteria
- Baseline benchmark suite exists.
- Major bottlenecks are quantified.
- Optimization roadmap is ranked by measured impact.

## Related Logs
- RL-03 FS Tree Topology and Required Records
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation