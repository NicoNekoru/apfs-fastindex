# Measurement Baseline

Status: Active
Date: 2026-05-16 (r2c-fallback-perf microopt pass)
Source: first benchmark run on the patched crate (R1 + EX-21 Rust port);
re-measured after the r2c-fallback-perf micro-optimisation pass
(buffer-reuse BulkReader, HashMap + `&str` aggregate walk,
`EntryKind: Copy`, drop unconditional `relative_str.clone()`, switch
to `sort_unstable_by`).

This page records the first reproducible measurement of
`apfs-fastindex-scan` against three reference targets. It is the standing
baseline that any future performance claim must compare against.

Reproducer:

```sh
# Tiny correctness baseline: detached .dmg via raw mode
PYTHONPATH=src python3 -m apfs_fastindex.bench --proof-fixture --mode raw --repeat 5

# Medium tree: this repo
PYTHONPATH=src python3 -m apfs_fastindex.bench \
    --target /Users/kai/Projects/apfs-fastindex --mode fallback --repeat 5

# Larger real tree: /Applications
PYTHONPATH=src python3 -m apfs_fastindex.bench --target /Applications --mode fallback --repeat 3
```

Host: macOS host listed in `src/apfs_fastindex/bench.py` output; release
build (`cargo build --release`).

| target                      | backend            | entries     | wall (median) | entries/sec | CPU user / sys (median) |
| --------------------------- | ------------------ | ----------- | ------------- | ----------- | ------------------------ |
| EX-13 proof fixture         | raw                | 7           | 0.23 s        | 30          | 13 ms / 15 ms            |
| apfs-fastindex repo         | fallback (std)     | 9,124       | 0.07 s        | 130,734     | 15 ms / 48 ms            |
| `/Applications` tree        | fallback (std)     | 163,667     | 1.28 s        | 127,513     | 410 ms / 728 ms          |
| `/` whole-machine scan      | fallback (std)     | 5,251,546   | 129.6 s       | 40,510      | 16.3 s / 47.8 s          |
| `/Applications` tree        | fallback (bulk)    | 163,667     | 1.04 s        | 157,155     | 347 ms / 608 ms          |
| `/Users` user-data scan     | fallback (bulk)    | 1,304,073   | 26.65 s       | 48,933      | 2.92 s / 6.62 s          |
| `/` whole-machine scan      | fallback (bulk)    | 5,260,624   | 108.7 s       | 48,380      | 14.85 s / 29.80 s        |

## r2c-fallback-perf micro-optimisation pass (2026-05-16)

Same host, same `apfs-fastindex-scan --mode fallback` driver, same
bench script (`PYTHONPATH=src python3 -m apfs_fastindex.bench`,
median of 5 runs), comparing the parent commit of the perf branch
against its tip. The perf branch lands four changes:

- `BulkReader` owns one 64 KiB `getattrlistbulk` buffer and one
  output `Vec` across the whole walk (was a fresh
  `vec![0u8; 65_536]` per directory; ~200k dirs × 64 KiB on a
  `/`-scan).
- `build_aggregates` switched from
  `BTreeMap<String, BTreeMap<u64, ..>>` keyed by owned ancestor
  paths to `HashMap<&str, HashMap<u64, ..>>` keyed by slices
  borrowed from `entries[i].path`. The old shape walked ancestors
  as a `Vec<String>` per file (~25M owned-String allocations on a
  5M-row tree). Same rewrite mirrored in `namespace.rs` (raw path).
- `EntryKind: Copy` removes pointless `.clone()` calls in the
  per-entry walker hot path.
- `relative_str.clone()` per entry collapsed: the mount-boundary
  branch is the only one that ever needed a second copy; the
  common path now moves the `String` into `NamespaceEntry`. All
  three `sort_by` callsites switched to `sort_unstable_by`
  (paths inside one walk are unique).

| target              | metric                  | before        | after         | delta              |
| ------------------- | ----------------------- | ------------- | ------------- | ------------------ |
| apfs-fastindex repo | median wall             | 58.6 ms       | 53.9 ms       | **−8%**            |
| apfs-fastindex repo | user CPU (median)       | 21 ms         | 17 ms         | **−19%**           |
| apfs-fastindex repo | sys CPU (median)        | 32 ms         | 32 ms         | flat               |
| apfs-fastindex repo | throughput              | 323,721 ent/s | 351,757 ent/s | **+9%**            |
| `/Applications`     | median wall             | 950 ms        | 816 ms        | **−14%**           |
| `/Applications`     | user CPU (median)       | 340 ms        | 227 ms        | **−33%**           |
| `/Applications`     | sys CPU (median)        | 535 ms        | 515 ms        | flat               |
| `/Applications`     | throughput              | 172,283 ent/s | 200,512 ent/s | **+16%**           |

The user-CPU delta is the clean signal: −19% on the small tree,
−33% on the medium tree. Larger trees should see closer to the
medium-tree percentage, because the aggregate phase scales with
entry count and that is where the
`BTreeMap<String, ..>` ↔ `HashMap<&str, ..>` substitution does
the most work. System CPU is flat (the syscall count is
unchanged; only the per-directory allocator pressure is gone).
The whole-machine `/` scan was not re-measured in this pass
because the host's filesystem cache state would dominate any
single re-measurement; the medium-tree percentages are the
load-bearing data points.

## r2c-syscall-perf-research parallel-walker pass (2026-05-16)

After r2c-fallback-perf landed, the remaining cost on
`/Applications` was ~63% sys-CPU — structurally kernel-side
work that user-space micro-opts could not touch. SR-021
synthesised four parallel research reports (xnu source,
Spotlight, OSS-tool survey, jwalk/rayon prior art) into a
single cost map; EX-24 falsified the attribute-mask alternative
(drec_only ≈ current_walker within 1%); EX-25 measured the
parallel-walker scaling envelope and verdicted T=4 as the
optimum on Apple silicon (the APFS container lock identified
by Szorc-2018 / Apple-DTS-2025 fires beyond T=4).

The Rust slice landed a `--threads N` flag on
`apfs-fastindex-scan` with default `min(hw.physicalcpu, 4)`.
Per-worker `BulkReader`, shared `WorkQueue`
(`Mutex<Vec<WalkFrame>>` + outstanding-counter + condvar +
atomic-bool done flag). On-join, per-worker entry Vecs are
concat-merged on the main thread; the existing
`sort_unstable_by` + `build_aggregates` pass then normalises
ordering. Test count 69 → 70
(`parallel_walker_matches_serial_shape` asserts byte-for-byte
identical output across schedulers).

End-to-end measurement on `/Applications`, same host as the
r2c-fallback-perf table above, median of 5 runs each side:

| target          | metric            | T=1 (post-fallback) | T=4 (this slice) | delta                |
| --------------- | ----------------- | ------------------- | ---------------- | -------------------- |
| `/Applications` | median wall       | 816 ms              | 523 ms           | **−36%**             |
| `/Applications` | throughput        | 200,512 ent/s       | 312,879 ent/s    | **+56%**             |
| `/Applications` | user CPU (median) | 227 ms              | 256 ms           | +13% (worker setup)  |
| `/Applications` | sys CPU (median)  | 515 ms              | 802 ms           | +56% (kernel scales) |

EX-25 microbench numbers for context (pure-kernel throughput
without post-processing, same host, same target):

| T  | wall    | sys     | sys/T   | ent/s    | speedup vs T=1 |
|----|---------|---------|---------|----------|----------------|
| 1  | 0.521 s | 0.508 s | 0.508 s | 314,380  | 1.00×          |
| 2  | 0.316 s | 0.616 s | 0.308 s | 517,437  | 1.65×          |
| **4** | **0.211 s** | 0.819 s | **0.205 s** | **776,196** | **2.47×**  |
| 8  | 0.268 s | 2.076 s | 0.260 s | 609,748  | 1.94× (regress)|
| 14 | 0.378 s | 4.717 s | 0.337 s | 432,866  | 1.38× (catastrophic) |

Cumulative `/Applications` throughput across both perf passes:
**172,283 → 312,879 ent/s = +82%** (1.82× cumulative).

The user-CPU gap (production 256 ms vs microbench 20 ms at T=4)
quantifies the user-space post-processing tax —
`NamespaceEntry` allocation, per-worker merge, final
`sort_unstable_by`, `build_aggregates`. Recorded as a candidate
future optimisation in RL-12; not load-bearing today.

System CPU rose with T as the agents predicted; the sub-linear
shape (sys/T = 0.205 s at T=4 vs 0.508 s at T=1, then jumping
to 0.260 s at T=8 with total sys 4× T=1) is exactly the APFS
container-lock signature. Defaulting to T=4 stays in the
favourable regime; explicit `--threads N` is available for
users who want to test their host's curve.

## Pre-perf historical context

"std" rows used `std::fs::read_dir` + `symlink_metadata` per entry. "bulk"
rows used the macOS `getattrlistbulk` backend in
`crates/apfs-fastindex/src/fallback_bulk.rs`. The whole-machine
comparison is the apples-to-apples one:

- `/` wall: **130 s → 109 s** (16% faster end-to-end on cold cache).
- `/` system CPU: **48 s → 30 s** (38% less time in syscalls).
- `/Applications` warm-ish: **127k → 157k entries/s** (24% throughput
  win).

Wall speedup is bounded by disk I/O on a cold scan; CPU/sys drop is
the cleaner signal that bulk fetch is doing the right thing
(fewer kernel transitions per entry). Hot-cache scans will close more
of the gap toward the throughput ceiling. Symlinks still cost one
extra `readlink` per entry because `getattrlistbulk` does not return
the link target.

Notes:

- The proof-fixture raw number is dominated by `hdiutil attach` startup;
  the 7-entry walk itself is in the low milliseconds. Raw-mode throughput
  on a meaningful detached image is not yet measured because the repo does
  not carry a large detached APFS sample.
- Fallback mode sustains ~130k entries/sec on both tested trees. CPU
  breakdown is roughly 35% user / 65% system — almost all of the system
  time is `lstat` + `read_dir`. Swapping in `getattrlistbulk` (macOS bulk
  attribute fetch) is the next bounded perf optimization; the
  `fallback.rs` module is split so that swap is local.
- Linear extrapolation no longer needed: the real `/` scan number above
  is the load-bearing data point. 5.25M entries in ~130 s on cold cache
  puts the fallback path inside the same order of magnitude as the
  reference Windows tools' first-run scan times on similar-sized
  volumes, before any APFS-specific bulk syscall work. Subsequent runs
  with warm OS cache should approach the `/Applications` rate (~125k
  entries/s).

Out of scope here: encrypted runtime, live boot disk, snapshot-assisted,
incremental cache. Those would change the baseline shape and need their
own oracles + measurements.
