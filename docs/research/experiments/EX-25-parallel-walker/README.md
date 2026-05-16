# EX-25 Parallel directory walker: thread-count scaling on APFS

ID: EX-25
Title: T ∈ {1, 2, 4, 8, num_cpus} scaling of `getattrlistbulk` across
  worker threads on a single APFS container
Date: 2026-05-16
Owner: Claude
Status: Planned
Result: Pending
Related RLs:
- RL-08
- RL-12
- RL-13

## Bottom line

SR-021 identifies parallel directory walk as the highest-leverage
remaining perf lever after r2c-fallback-perf. EX-24 falsified the
attribute-mask alternative (drec_only and current_walker are
within 1%, so the vnode-rage cost is not a useful lever).
EX-25 measures the empirical scaling envelope of a per-directory
worker-pool walker against the same target as the standing
baseline.

The headline question is one number per thread count:
**entries/sec on `/Applications` at T ∈ {1, 2, 4, 8, num_cpus}**,
warm cache, median of 5 runs. SR-021's prior-art envelope
(Healey + Szorc-2018 + Apple DTS) predicts roughly:

  - T=1: matches EX-24's 310k ent/s within noise
  - T=2: ~1.6-1.8× (≈500-560k ent/s)
  - T=4: ~2.5-3.2× (≈775k-990k ent/s)
  - T=8: ~3.5-4.5× (plateaus due to APFS container contention)
  - T=num_cpus: marginal or negative

The falsification signal is sys-CPU per thread: if the per-thread
sys-CPU grows faster than linearly with T, we have re-discovered
the APFS container lock and should back off. Specifically:

  - Linear scaling regime: `sum_of_per_thread_sys_cpu / T ≈
    single_thread_sys_cpu`. Each thread does the same work.
  - Contention regime: `sum_of_per_thread_sys_cpu / T >>
    single_thread_sys_cpu`. Threads spend extra time waiting on
    locks; the kernel does *more* total work to deliver the same
    answer.

Per Szorc-2018, the contention regime is real and catastrophic
at T > physical cores. The probe explicitly samples T at and
beyond physical cores so we can see the curve.

## Question

For a tree of ~164k entries on `/Applications`, what is the
entries/sec throughput of a worker-pool `getattrlistbulk` walker
as a function of thread count T, and at what T does sys-CPU per
thread start to grow super-linearly?

## Hypotheses

- **Hypothesis A `validated_parallel_scaling`** (SR-021 best case):
  T=4 reaches ~2.5-3.2× of T=1, T=8 plateaus or rises slightly,
  T=num_cpus regresses. sys-CPU per thread stays roughly flat
  through T=4.
- **Hypothesis B `partial_scaling_then_plateau`**: T=2 reaches
  ~1.5-1.7× of T=1 but T=4 is no better than T=2. Either the
  APFS container lock fires earlier than expected on this kernel
  or the user-space merge step bottlenecks before kernel does.
- **Hypothesis C `single_thread_optimal`**: no thread count
  beats T=1 by more than 10%. The convergent OSS evidence
  (dumac, macdirstat, jwalk) does not transfer to this host.
  Most pessimistic outcome; would force a re-design.
- **Hypothesis D `pathological_contention`**: T=num_cpus
  *regresses* below T=1 (per Szorc-2018's 18-procs-worse-than-12
  finding or sharkdp/fd#1131's 1212× slowdown on WSL2). The
  probe must guard against this and pick a safe default.

## Environment

- macOS version captured in `artifacts/generated/environment.json`
  (including `sysctl -n hw.physicalcpu` and `hw.logicalcpu`).
- Target: `/Applications` (same as EX-24 and the standing
  baseline).
- No `sudo`.
- Each (config, run) is preceded by a 200 ms sleep so previous
  runs' I/O quiesces. We do not `purge` (needs sudo); the first
  run pays cold-cache and subsequent runs are warm — the median
  is the headline number.

## Oracle

Like EX-24, EX-25 measures throughput, not correctness. The
consistency check is: every thread count returns the same entry
count (the work-queue traversal is deterministic in shape, not
in worker assignment).

## Setup

1. Capture environment manifest including `hw.physicalcpu` and
   `hw.logicalcpu`.
2. Compile `parallel_microbench.rs` with `rustc -O` (raw FFI, no
   cargo).
3. Sweep T ∈ {1, 2, 4, 8, hw.logicalcpu}; for each T run the
   binary 5× on `/Applications` and record per-run JSON output
   including `(wall, user, sys, entries, threads_used)`.
4. Compute per-T medians + the scaling ratio against T=1 + the
   sys-CPU-per-thread metric.

## Probe Steps

1. `rustc -O parallel_microbench.rs -o parallel_microbench.bin`.
2. For each T:
   - run `./parallel_microbench.bin <T> /Applications` 5×;
     sleep 200 ms between runs.
   - record per-run JSON.
3. Apply the verdict ladder:
   - if (T=4 entries/sec ≥ 2.0× T=1) AND (T=4 sys/thread ≤ 1.2×
     T=1 sys): `validated_parallel_scaling`.
   - if (T=2 entries/sec ≥ 1.4× T=1) AND no further T helps:
     `partial_scaling_then_plateau`.
   - if (max T speedup ≤ 1.1× T=1): `single_thread_optimal`.
   - if (T=num_cpus < T=1): `pathological_contention`.
   - else: `partial_signal`.
4. Pick the default thread count:
   - `validated_parallel_scaling` → default 4
   - `partial_scaling_then_plateau` → default 2
   - `single_thread_optimal` or `pathological_contention` →
     keep default 1; record the failure mode in the Rust slice's
     `not_claimed`.

## Expected Observations

(See hypothesis section.)

## Observed Results

_(filled in after the run)_

## Artifacts Saved

- `artifacts/parallel_microbench.rs`
- `artifacts/probe_ex25.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex25-t<N>-runs.json` (one per T)
- `artifacts/generated/summary.json`

## Interpretation

_(filled in after the run; the patterns to look for:)_

- `validated_parallel_scaling` → land the parallel walker in
  `fallback.rs` behind a `--threads N` flag with default
  `min(physical_cores, 4)`. Update measurement-baseline.md with
  the new per-T numbers.
- `partial_scaling_then_plateau` → land at default T=2, with
  `--threads` still available.
- `single_thread_optimal` → do not change the production walker;
  record the result as a negative finding in RL-12 and let the
  evidence stand.
- `pathological_contention` → same as single_thread_optimal,
  with an extra RL-12 warning that explicit user-requested
  `--threads N` could harm rather than help on this kernel.

## What This Rules Out

- Does not measure cold-cache behaviour after `purge` (needs sudo).
- Does not measure encrypted-APFS behaviour.
- Does not measure mixed-volume scans (parallelism may behave
  differently across volume boundaries; in the current product
  contract we don't cross them by default anyway).
- Does not exercise the sink / merge logic that the production
  walker will need (sorted output across threads). The
  microbench skips output collection entirely so the measured
  throughput is the kernel ceiling; the production walker will
  pay a small merge cost on top.

## Impact on RLs

- RL-12: a positive verdict promotes parallelism to the named
  primary perf lever for the fallback walker. A negative
  verdict closes the lane and pushes future work to the
  user-space post-processing micro-pass (per the EX-24 bonus
  finding).
- RL-08: the per-T sys-CPU growth ratio is a diagnostic for
  future macOS-version regressions on APFS container contention.
- RL-13: a `pathological_contention` verdict adds an explicit
  fallback policy ("if T > 4 and sys-time/T > 1.5× T=1, refuse
  to parallelise").

## Next Exact Step

- Run the probe.
- On `validated_parallel_scaling` or `partial_scaling_then_plateau`,
  implement the parallel walker in the Rust crate
  (`crates/apfs-fastindex/src/fallback.rs`) behind a `--threads N`
  flag, with the EX-25-picked default, and measure end-to-end with
  the production scanner against the standing baseline.
- On `single_thread_optimal` or `pathological_contention`, do not
  ship parallelism; record the negative result and close the lane.
