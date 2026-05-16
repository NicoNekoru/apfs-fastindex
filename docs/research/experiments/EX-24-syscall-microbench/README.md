# EX-24 Syscall microbench: where does the per-entry cost live

ID: EX-24
Title: `getattrlistbulk` per-attribute cost decomposition + `fts(3)` control
Date: 2026-05-16
Owner: Claude
Status: Planned
Result: Pending
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

_(filled in after the run)_

## Artifacts Saved

- `artifacts/microbench.rs`
- `artifacts/probe_ex24.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex24-drec_only-runs.json`
- `artifacts/generated/ex24-current_walker-runs.json`
- `artifacts/generated/ex24-fts-runs.json`
- `artifacts/generated/summary.json`

## Interpretation

_(filled in after the run; the patterns to look for:)_

- `vnode_cost_is_load_bearing` → record the Phase-2 deferred-
  attribute refactor in RL-12 as the next highest-leverage perf
  direction after EX-25 lands.
- `vnode_cost_is_marginal` → write the EX-25 design knowing
  parallelism is essentially the only remaining lever.
- `fts_wins_singlethread` → rewrite EX-25's design to use fts
  inside each worker rather than `getattrlistbulk`.

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

- Run the probe. Record the verdict. Use the verdict to shape
  EX-25's design (worker pool around `getattrlistbulk`, around
  `fts(3)`, or with a drec-only first phase).
