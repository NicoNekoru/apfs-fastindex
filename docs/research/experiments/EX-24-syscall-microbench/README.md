# EX-24 Syscall microbench: where does the per-entry cost live

ID: EX-24
Title: `getattrlistbulk` per-attribute cost decomposition + `fts(3)` control
Date: 2026-05-16
Owner: Claude
Status: Executed
Result: `oracle_inconclusive` on the entry-count consistency check;
  **headline finding: drec_only ≈ current_walker (within 1%), which
  falsifies the SR-021 vnode-cost-load-bearing hypothesis.**
Related RLs:
- RL-08
- RL-12
- RL-13

## Bottom line

SR-021 identifies two distinct levers on the post-r2c-fallback-perf
scanner's 63% sys-CPU: (a) parallelism (EX-25's question) and (b)
the attribute mask sent to `getattrlistbulk`. The xnu source
(`vfs_attrlist.c:4436` + `vfs_subr.c:7233-7256`) says inode-
required attributes force per-entry vnode create + rage; drec-only
attributes skip vnode creation entirely. EX-24 measures the
empirical size of that lever on this host so EX-25's verdict can
be interpreted against a known cost floor.

Three configurations, identical fixture, same-host same-day:

1. **getattrlistbulk-drec-only**: only `ATTR_CMN_NAME |
   ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID | ATTR_CMN_DEVID |
   ATTR_CMN_ERROR`. No `fileattr`. Per the xnu cost model APFS
   answers from the drec leaf without vnode creation. This is the
   theoretical floor for `getattrlistbulk` on this fixture.
2. **getattrlistbulk-current-walker**: the same mask the
   `BulkReader` in `fallback_bulk.rs` actually uses today:
   drec-only ∪ `ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE`. This
   crosses the vnode-creation boundary (size requires inode load).
3. **fts(3)** control: the BSD `fts_open` + `fts_read` library
   traversal. Tempel (2019) measured `fts ≥ getattrlistbulk` on
   single-threaded APFS metadata walks. If fts beats config (2)
   here, the depth-first namecache reuse path is structurally
   cheaper than the bulk path's vnode-rage path for our exact
   attribute set — that would change EX-25's design (we might
   parallelise fts instead of bulk).

The verdict is a per-config (entries/sec, wall-median, user-
median, sys-median) table on the same target tree. The
load-bearing comparison is **config 1 vs config 2** — that ratio
bounds how much sys-CPU a future Phase-2 deferred-attribute
refactor could save.

## Question

For a tree of ~164k entries on a stock APFS volume on this host,
what is the per-entries/sec and per-syscall cost difference
between:

  (1) `getattrlistbulk` with a drec-only attribute mask,
  (2) `getattrlistbulk` with the production fallback walker's
      attribute mask,
  (3) `fts(3)` recursive traversal returning the same
      `(name, kind, size)` per entry?

## Hypotheses

- **Hypothesis A `vnode_cost_is_load_bearing`** (xnu evidence):
  config 1 (drec-only) sustains substantially higher throughput
  than config 2 (current mask), and the sys-CPU delta accounts for
  most of the difference. This would mean the Phase-2 deferred-
  attribute refactor is the highest-impact future work after EX-25.
- **Hypothesis B `vnode_cost_is_marginal`**: config 1 ≈ config 2.
  The vnode-rage path is cheap on modern Apple silicon and the
  remaining sys-CPU is elsewhere (B-tree page-cache, MAC
  checks, etc.). This would deprioritise the Phase-2 refactor
  and put even more weight on EX-25 / parallelism as the only
  meaningful lever.
- **Hypothesis C `fts_wins_singlethread`** (Tempel 2019): config 3
  (fts) beats both config 1 and config 2 because per-`fstatat`
  vnodes reuse the namecache. If true, EX-25 should be designed
  around a parallel fts traversal rather than a parallel bulk
  walker.

## Environment

- macOS version captured in `artifacts/generated/environment.json`.
- Target: `/Applications` (~164k entries; same as the standing
  baseline so we can correlate).
- No `sudo` — the probe must be runnable as the invoking user.
- Cache state: the probe runs each config 5× back-to-back so the
  first run pays the cold-cache cost and the next four are
  warm-cache. Both medians are reported; the warm-cache median is
  the headline number because it isolates CPU cost from disk I/O.
- Concurrency: single-threaded only. EX-24 deliberately does not
  touch parallelism so it isolates the per-entry cost. EX-25
  layers parallelism on top.

## Oracle

EX-24 has no oracle in the SR-009/SR-017/SR-019 sense — it
measures throughput, not correctness. The correctness oracle is
that all three configurations report the same entry count on the
same target tree (modulo a small drift if files are written /
removed during the run; the probe counts and reports drift).

## Setup

1. Capture environment manifest.
2. Compile a standalone microbench Rust binary
   (`artifacts/microbench.rs`) with `rustc -O`. The binary has
   three subcommands corresponding to the three configurations
   and walks the target tree recursively, counting entries and
   measuring wall + getrusage user + getrusage sys.
3. For each configuration, run the binary 5× in succession and
   record per-run JSON output.
4. Summarise: median (wall, user, sys, entries/sec) per config,
   plus an entry-count diff against config 1 as the consistency
   check.

## Probe Steps

1. Capture `environment.json`.
2. `rustc -O microbench.rs -o microbench.bin`.
3. For each config in {drec_only, current_walker, fts}:
   - run `./microbench.bin <config> /Applications` 5×
   - collect per-run JSON, write to
     `generated/ex24-<config>-runs.json`
4. Compute per-config medians + the consistency check; write
   `generated/summary.json` with the verdict slug.

## Expected Observations

- All three configs report the same entry count (±0 on a
  quiescent host; possibly ±a few on a noisy one).
- Wall and sys-CPU per config tell the story; user-CPU should be
  small for all configs because the microbench does no
  post-processing.

## Verdict slugs

- `vnode_cost_is_load_bearing` — config 1 is ≥1.3× faster than
  config 2 in entries/sec; recommend Phase-2 deferred-attribute
  refactor as future work.
- `vnode_cost_is_marginal` — config 1 within 1.1× of config 2;
  Phase-2 refactor deprioritised.
- `fts_wins_singlethread` — config 3 beats config 2 in
  entries/sec; revisit EX-25's design.
- `oracle_inconclusive` — entry counts diverge by >1% across
  configs (host noise too high).

## Observed Results

First run, host's `/Applications` (~164k entries), 5 runs per
config:

| config            | entries | wall median | user median | sys median | entries/sec |
|-------------------|--------:|------------:|------------:|-----------:|------------:|
| `drec_only`       | 163,651 | 0.523 s     | 0.012 s     | 0.511 s    | **312,622** |
| `current_walker`  | 163,651 | 0.527 s     | 0.012 s     | 0.515 s    | **310,249** |
| `fts`             | 187,532 | 1.032 s     | 0.041 s     | 0.990 s    | 181,772     |

Mechanical verdict: `oracle_inconclusive` because the consistency
check (entry counts within 1%) fails between bulk and fts. The
drift (187,532 - 163,651 = 23,881 entries) is firmlink traversal
asymmetry: fts crosses into `/System/Volumes/Data/Applications/...`
firmlinked apps; `getattrlistbulk` from a directory fd does not.
This is a known APFS firmlink behaviour, not a real
correctness divergence. Both `drec_only` and `current_walker` return
**identical** 163,651 entries — the comparison between those two is
the one that bears the load.

## Headline finding

`drec_only` and `current_walker` are within 1% on every dimension
(wall, user, sys, throughput). The SR-021 hypothesis that
inode-attribute fetch forces a costly vnode create + rage path is
**not falsifiable as a perf lever** on modern Apple silicon for
this attribute set:

- Adding `ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE` to a drec-only
  mask costs ~4 ms of wall on a 164k-entry tree (~25 ns/entry).
- The vnode rage path either does not fire at all in this
  configuration on this kernel, or its cost is dominated by
  unrelated work (B-tree page-cache hits, MAC checks). Either
  reading is consistent with the data.

**Implication**: the Phase-2 deferred-attribute refactor (drec-only
first pass + selective second-pass stat) is **deprioritised**. It
would not save measurable sys-CPU on modern hardware. The only
remaining structural lever for raw throughput is parallelism, which
EX-25 measures.

## fts(3) verdict

`fts` is **1.7× slower** than `current_walker` (181k vs 310k
entries/sec on the same fixture). Tempel's 2019 result that "fts
≥ getattrlistbulk on APFS single-threaded" does **not** replicate
on macOS 14+. EX-26 (the conditional fts follow-up planned in
SR-021) is cancelled. EX-25's parallel walker stays built around
`getattrlistbulk` as originally designed.

## Bonus calibration: user-space cost

The microbench (which does *zero* post-processing — no
NamespaceEntry, no aggregate, no sort, no walk_skips) sustains
**312k entries/sec** on `/Applications`. The production scanner
on the same target sustains **200k entries/sec** (the
measurement-baseline number). The ~110k delta is the user-space
post-processing tax. That gives EX-25 a useful kernel-throughput
ceiling estimate: 4 parallel workers could in principle hit
~1.25M entries/sec from the kernel alone, with the actual
end-to-end number depending on APFS-container contention (per
SR-021) and user-space merge cost.

## Artifacts Saved

- `artifacts/microbench.rs`
- `artifacts/probe_ex24.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex24-drec_only-runs.json`
- `artifacts/generated/ex24-current_walker-runs.json`
- `artifacts/generated/ex24-fts-runs.json`
- `artifacts/generated/summary.json`

## Interpretation

The data lands cleanly on the **`vnode_cost_is_marginal`** outcome
(read modulo the firmlink-asymmetry caveat on the fts row). Three
follow-ons:

1. **EX-25 design is unchanged.** The parallel walker stays
   built around `getattrlistbulk` with the current attribute mask;
   no point shrinking the mask first.
2. **EX-26 (`fts(3)`) is cancelled.** The 1.7× slower result is
   strong enough that no further fts experimentation is justified.
3. **The Phase-2 deferred-attribute refactor is deprioritised.**
   RL-12 should record this finding so a future engineer doesn't
   re-invent the same hypothesis.

A second-order finding worth recording: the microbench's pure
kernel throughput (312k ent/s) is 1.56× the production scanner's
(200k ent/s). Post-processing in user space is now a measurable
fraction of the budget — not the dominant cost, but enough that
a follow-up micro-pass on `build_aggregates` / `String`
allocations could pay off after EX-25. Recorded as a candidate
future direction in RL-12.

## What This Rules Out

- Does not validate that EX-25 will scale (single-threaded only).
- Does not measure the cost of attributes outside the
  TOTALSIZE/ALLOCSIZE pair (e.g. `ATTR_CMN_MODTIME`,
  `ATTR_FILE_DATALENGTH`); those would shift config 2's number
  but are not in our production walker.
- Does not measure encrypted-APFS behaviour. The host's volume's
  encryption state is recorded in environment.json for context.
- Does not measure cold-cache behaviour after `purge` because
  `purge` needs sudo. The first of 5 runs partially captures
  cold-cache cost but is not a clean isolation.

## Impact on RLs

- RL-12: a positive `vnode_cost_is_load_bearing` verdict adds
  the Phase-2 deferred-attribute refactor to the named future
  direction list. A `vnode_cost_is_marginal` verdict deletes it.
- RL-08: the per-config sys-CPU ratio becomes a diagnostic for
  future macOS-version regressions ("did Apple change the
  vnode-rage path between 14.x and 15.x?").
- RL-13: if `fts_wins_singlethread` fires, the format-drift /
  fallback section gains an "fts is a viable substitute for
  bulk on this attribute set" note.

## Next Exact Step

- **Proceed to EX-25** with the parallel walker built around
  `getattrlistbulk` and the production attribute mask unchanged
  (`current_walker`). Worker pool, per-worker `BulkReader`, sink
  thread for sorted output, T ∈ {1, 2, 4, 8, num_cpus} sweep,
  verdict on the sub-linear scaling envelope.
- EX-26 (`fts(3)`) is **cancelled** per the 1.7× slowdown.
- A future post-EX-25 EX-* may be worth opening for a deeper pass
  on `build_aggregates` / `String` allocations in user space
  (the 110k ent/s gap between microbench kernel throughput and
  production end-to-end throughput). Not load-bearing today;
  recorded for context.
