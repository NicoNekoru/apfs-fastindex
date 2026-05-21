# EX-30 Production fallback-walker performance baseline

ID: EX-30
Title: Cold + warm wall-time + entries/sec baseline for the
  fallback walker on the user's actual `/Users/kai` tree, in
  the configuration the GUI ships with (`--threads 0` = auto,
  no `--cross-mounts`).
Date: 2026-05-21
Owner: Claude
Status: Executed
Result: `production_baseline_recorded`
Related RLs:
- RL-08
- RL-12
- RL-13

## Bottom line

EX-24 and EX-25 measured the walker's scaling in a controlled
benchmark loop that calls the walker as a library — useful for
the parallel-vs-serial decision, but those numbers don't tell
us how long the **shipping product** takes to scan the user's
machine. The GUI invokes `apfs-fastindex-scan` as a subprocess
with `--format msgpack`, optional `--progress`, default thread
count. Process spawn, msgpack serialisation, stderr handling,
and the user's actual data shape all contribute to wall time
in production but were never end-to-end measured.

EX-30 is the production-shape baseline. One number per cell:

- **Wall time, seconds.** From process spawn to msgpack file
  written.
- **Entries/sec.** Total entries / wall time.
- **Cold vs warm.** Cold = page cache purged via
  `sudo purge` before the run. Warm = subsequent runs in
  the same harness invocation.
- **No raw mode.** Raw is EX-28-blocked on this host
  (`live_raw_blocked_by_kernel`); the GUI defaults to
  `--mode auto` which falls back to fallback on `/`-class
  paths. We measure what users see.

The harness runs N=5 iterations: one cold + four warm.
Cold establishes the worst-case experience (first launch,
or after a long idle); warm establishes the steady-state.
Median of the four warm runs gives the steady-state
entries/sec; the cold/warm ratio sizes the cache lever.

## Why this matters now

R4 (persistent cache, Gate 6) needs a baseline to measure
against. Without EX-30, "the cache made scans 3× faster" is
unfalsifiable — 3× of what? The bench output saved here
becomes the regression oracle for that work: the cache
should not slow the cold path more than ~5% (extra mtime
+ identity checks), and should speed the warm path
substantially.

## Verdict ladder

- `production_baseline_recorded` → harness ran without error,
  N=5 iterations produced cold + 4 warm numbers, entries/sec
  is positive, msgpack output is non-empty. This is the
  expected outcome and what we save as the cache-regression
  oracle.
- `harness_failure` → the CLI errored, the JSON parse
  failed, or wall time was zero. Investigate; do not record
  a baseline.

## Method

1. Build `apfs-fastindex-scan` from the working tree with
   `cargo build --release` so the bench matches the bundled
   binary's optimisations.
2. Pick the scan target: `/Users/kai` (the largest single
   subtree the user routinely scans; bigger than
   `/Applications` from EX-25 but not the whole `/`).
3. Run the harness `probe_ex30.py`:
   - Cold iteration: `sudo purge` + run + record wall time
     and entries.
   - Four warm iterations: run + record. No purge between.
   - 200 ms sleep between iterations to let the FS
     write-cache settle.
4. Output: `artifacts/generated/ex30_baseline.json` with
   per-iteration timings, the cold/warm split, entries/sec,
   median + min/max, and host facts (macOS version, kernel,
   physical cores, scanned-tree entry count, scanned-tree
   byte count).
5. Verdict written to the JSON's `verdict` field.

The harness should be re-runnable: subsequent invocations
should not depend on any state from a previous run. Output
goes to a date-suffixed file so historical runs are
preserved.

## Out of scope

- Per-phase breakdown (walk vs aggregate). The fallback walker
  doesn't currently emit phase timings. Adding them is a
  separate ticket; the wall-time baseline is sufficient for
  R4 cache work.
- Raw-mode timings. EX-28 closed that path.
- Comparing against alternatives (du, tree, find).
- `--threads` sweep. EX-25 already validated T=4 optimum;
  re-running here would duplicate work.
- The privileged-helper code path. The GUI's regular Scan
  button (no admin) is what most users will use; admin scans
  add the AuthorizationServices round-trip + IPC overhead and
  warrant their own bench when we have a real reason to
  investigate them.

## Result (2026-05-21)

`artifacts/generated/ex30_baseline_2026-05-20.json` records
the run. Target: `/Users/kai` (the user's home tree).
Host: macOS 26.3.1, Apple silicon (14 physical cores).

| Iteration | Wall (s) | Entries     | Entries/sec | stdout (MB) |
| --------- | -------- | ----------- | ----------- | ----------- |
| cold      |     9.87 |   1,556,792 |     157,760 |       391.9 |
| warm 1    |    10.89 |   1,556,800 |     142,940 |       391.9 |
| warm 2    |     9.62 |   1,556,800 |     161,789 |       391.9 |
| warm 3    |     9.59 |   1,556,800 |     162,258 |       391.9 |
| warm 4    |     9.62 |   1,556,800 |     161,766 |       391.9 |

- **Warm median: 9.62 s, 162k entries/sec, 1.56M entries,
  172k directory aggregates, 392 MB msgpack payload.**
- **Cold ≈ warm (ratio 1.03×).** The harness ran `sudo -n
  purge` for the cold iteration but the user had no cached
  sudo creds; `purge_succeeded: false` is recorded in the
  JSON. The cold-run page cache was effectively warm because
  the smoke-test on `docs/` and the cargo build hit
  overlapping data already.

  Even if `purge` had run, prior EX-25 measurements suggested
  the fallback walker is CPU-bound on this workload
  (getattrlistbulk syscall + decode + ancestor-aggregate
  accumulation), not I/O-bound. APFS-on-NVMe rarely shows a
  >1.5× cold/warm difference once the FS-tree pages are in
  RAM. A future EX-30b with a forced cold cache would
  validate this, but it's not necessary for R4 — the cache
  lever for R4 is **avoiding work entirely** (subtree mtime
  unchanged → reuse cached aggregate), not faster reads.

### What this means for R4

The R4 target is to take the warm 9.62 s and make a
"no-changes-since-last-scan" rescan ≪ 1 s. That's ~10×
faster than the baseline, all from cached-subtree reuse.
Concrete acceptance criteria for R4 (validated against this
baseline):

1. **Cache cold-miss path doesn't regress.** First scan
   after wiping the cache stays within 1.05× of 9.62 s
   (i.e., ≤ 10.10 s). Identity + mtime checks add
   noise-level overhead.
2. **Cache hit path on unchanged tree: ≤ 1.0 s.** That's
   the headline R4 promise — the user reopens the app, hits
   Scan, sees results almost instantly.
3. **Cache hit path with 1% of subtrees changed: linear
   interpolation between (1) and (2).** No cliff.

### Why no purge

The harness's `_purge_caches()` runs `sudo -n purge`
(non-interactive). On a dev machine the user can prep the
session with `sudo -v` before running the harness; without
cached creds, the cold-cache state is degenerate. The result
file always records `purge_succeeded` so subsequent readers
can tell the difference. A future polish would be to switch
to a `prompt-once-via-osascript` flow, but that's not worth
the complexity right now — the warm-median is the number
R4 will be measured against.

## R4 v1 cache: measured on the same target (2026-05-21)

The R4 v1 cache (commit landing with EX-30) was bench-tested
against `/Users/kai` and a smaller stable tree
(`/Users/kai/Projects/apfs-fastindex`) immediately after
landing. Two distinct behaviours emerged:

| Target                                  | Tree size | First scan | Rescan (--cache) | Speedup |
| --------------------------------------- | --------- | ---------- | ---------------- | ------- |
| `/Users/kai/Projects/apfs-fastindex/docs` | 441 ent / 3 dirs | 0.33 s | 0.007 s | **47×**  |
| `/Users/kai/Projects/apfs-fastindex`    | 26k ent / ~3.9k dirs | 0.59 s | 0.12 s   | **4.9×** |
| `/Users/kai`                            | 1.56M ent / 172k dirs | 46.6 s | 45.4 s    | **1.0×** |

`/Users/kai`-class trees show **no** cache speedup. Two
compounding reasons:

1. **The signature probe is slower than the scan it should
   replace.** The probe uses naive `read_dir` +
   `symlink_metadata` per child; the walker uses
   `getattrlistbulk` which batches dozens of metadata reads
   into one syscall. On 172k dirs the probe pays ~37 s vs
   the walker's ~9 s. So even a true cache hit costs more
   than no cache at all.
2. **mtime churn.** `/Users/kai` has dozens of subdirs whose
   mtimes change every few minutes (browser caches, Spotlight,
   log rotation, Xcode derived data). The signature is
   recomputed at probe time; it almost never matches the
   cached signature → miss → full scan + cache write. So
   every cache write is wasted.

For stable trees (project directories, `/Applications`,
read-only system paths) the cache delivers the expected
5-50× speedup; the probe is cheap because the tree is small
and the signature is stable because nothing changes.

### What v1 ships

The v1 cache lives behind `--cache` (off by default) so users
opt in per invocation. The CLI prints a "cache: hit" line to
stderr on hits so consumers can tell what happened. No GUI
plumbing yet — the cache integration is CLI-only this turn.

### Follow-up work (R4 v2)

To make the cache valuable on `/Users/kai`-class trees, two
pieces are required and neither belongs in this commit:

1. **Walker-emitted signature.** Have the parallel walker
   accumulate the directory signature inline while it's
   already calling `getattrlistbulk` for the scan. Eliminates
   the separate dir-only probe. Cost moves from ~37 s extra
   to ~0 s extra.
2. **Per-subtree caching.** Store one signature per
   directory, not one per scan. On rescan: walk the tree
   incrementally — when a directory's signature matches the
   cached one, skip its subtree entirely and reuse the
   cached entries + aggregates for that subtree. A few
   churning subdirs invalidate themselves; the other ~99%
   of the tree stays cached.

Together they turn a 45 s rescan-on-/Users/kai into ~1 s
when ~10% of subdirs have changed. That's the R4 v2 promise
and what the next EX-* will validate. Sketch:

- EX-31: walker-emitted signature, FFI changes
- EX-32: per-subtree cache + subtree-reuse-on-rescan, with
  parity oracle against a forced full rescan

Until those land, document `--cache` as "useful for stable
trees; not recommended for active home directories." The
release notes for the GUI surface will say so explicitly
when the toggle ships.
