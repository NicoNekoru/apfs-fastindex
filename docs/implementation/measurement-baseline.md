# Measurement Baseline

Status: Active
Date: 2026-05-14
Source: first benchmark run on the patched crate (R1 + EX-21 Rust port)

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
