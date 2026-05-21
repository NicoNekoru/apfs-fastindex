# EX-33 Search FFI performance study

ID: EX-33
Title: Profile the `apfs_scan_search_names` FFI on real-scale
  data; quantify dominant cost; ship one meaningful
  optimization if there's headroom.
Date: 2026-05-21
Owner: Claude
Status: Executed
Result: `validated_flat_buffer_memchr_lift`
Related RLs:
- RL-12 (perf engineering)
- RL-13 (UX latency targets)

## Bottom line

The c69b483 → 813d0bd refactor (pre-lowercased name cache +
search-in-Rust) brought per-keystroke search latency from
multi-second to ~30-55 ms on a 1.56 M-entry `/Users/kai`
scan. That was a ~100× win but still ~10-20× off the
memory-bandwidth floor (~50 MB of name bytes / ~20 GB/s ≈
2.5 ms).

The remaining headroom came from two compounding costs the
803-LOC initial implementation didn't address:

1. **Cache-unfriendly memory layout.** `tree.names_lower:
   Vec<String>` puts each name on the heap behind a
   separate allocation. The search loop's hot path is
   "scan every name with the same needle"; 1.5 M
   random-pointer reads (one per name header) is ~10-20×
   slower than 1.5 M sequential bytes, because the CPU
   prefetches contiguous buffers but not heap-scattered
   `String`s.

2. **Per-call needle preprocessing.** `str::contains`
   builds its searcher state once per call. With 1.5 M
   haystacks per search, that's 1.5 M searcher
   constructions, all for the same needle.

EX-33 attacks both:

1. Replace `Vec<String>` with a flat `Vec<u8>` +
   cumulative `Vec<u32>` offset table.
2. Build one `memchr::memmem::Finder` per search; reuse
   across every haystack.

After the lift (measured against the 1.56 M-entry
`/Users/kai` baseline, 5 iterations per query, median
reported):

| Query                              | Before (813d0bd)   | After (EX-33)       | Speedup |
| ---------------------------------- | ------------------ | ------------------- | ------- |
| `"e"` (1.1 M matches)              | 49.75 ms           |  42.08 ms           |  1.18×  |
| `"a"` (983 k matches)              | 51.17 ms           |  42.71 ms           |  1.20×  |
| `".txt"`                           | 32.53 ms           |  10.14 ms           |  3.21×  |
| `".log"`                           | 29.04 ms           |  11.98 ms           |  2.42×  |
| `"Photo"`                          | 32.39 ms           |   9.42 ms           |  3.44×  |
| `"Library"`                        | 41.70 ms           |  11.83 ms           |  3.53×  |
| `"Cache"`                          | 37.32 ms           |  12.70 ms           |  2.94×  |
| `"node_modules"`                   | 53.24 ms           |   9.53 ms           |  5.59×  |
| `"com.apple.developer"`            | 34.63 ms           |   5.30 ms           |  6.53×  |
| `"Übersicht"` (non-ASCII)          | 46.51 ms           |  11.23 ms           |  4.14×  |
| `"zzzz_no_match_zzzz_aardvark"`    | 27.48 ms           |   2.28 ms           | 12.05×  |

Zero-match queries hit the floor (2.28 ms ≈ memory-
bandwidth limit on the 50 MB name buffer). Longer needles
benefit more from `memchr`'s SIMD substring search — the
Vec<String> + str::contains path was building a Two-Way
searcher per name; the Finder amortises that across all
1.5 M haystacks.

The shorter-needle, high-match cases (`"e"`, `"a"`) see
modest gains because their cost is dominated by the
ancestor walk — every match adds a parent-chain walk into
the keep-set's `FxHashSet`, and at 1 M+ matches that's
many millions of inserts. Not addressed in this commit;
sits below the "diminishing-returns" line for typical
queries (users almost never search single characters).

## Why this matters

User-facing latency targets:

| Latency       | Perceived as          | Source                     |
| ------------- | --------------------- | -------------------------- |
| < 16 ms       | instant (60 Hz frame) | Nielsen Norman             |
| < 100 ms      | feels live            | Card / Robertson / Mackinlay |
| < 1 s         | "still responsive"    | RAIL                       |

Typical user queries (4-12 chars) now land 9-12 ms median
on a 1.5 M-entry tree — comfortably inside "feels live"
and within range of "instant" for hot-cache repeats.

## Method

`crates/apfs-fastindex/examples/bench_search.rs` runs a
real scan via the existing FFI, then exercises
`apfs_scan_search_names` against a curated query set with
5 iterations per query. Saved as a JSON artifact for the
next iteration to diff against:

- Target: `/Users/kai` (1.57 M nodes)
- Host: macOS 26.3.1, Apple silicon (14 physical cores)
- Build: `cargo build --release --example bench_search`
- Iterations: 5 per query, median reported

Query shapes:

- **One-letter ASCII** (`"e"`, `"a"`): highest match
  counts; stresses the ancestor walk + the `memchr`
  short-needle path.
- **Extension suffixes** (`".txt"`, `".log"`): matches
  every file of that type. Realistic ~few-thousand
  matches.
- **Short dir-name tokens** (`"Photo"`, `"Library"`,
  `"Cache"`, `"node_modules"`): typical user input shape.
- **Long substring** (`"com.apple.developer"`): floor for
  the `memchr` scan (needle length increases per-comparison
  cost slightly but SIMD pays off).
- **Non-ASCII** (`"Übersicht"`): forces the Unicode path
  on the query side (`to_lowercase()` allocates), no
  matches typically.
- **Zero-match** (`"zzzz_no_match_zzzz_aardvark"`): the
  inner loop touches every byte but never matches → no
  ancestor walks. This is the floor of the `contains` work.

## What changed in code

1. **`tree::Tree`** — replaced `names_lower: Vec<String>`
   with `names_lower_buf: Vec<u8>` +
   `names_lower_offsets: Vec<u32>`. Population in
   `Tree::build` reuses a scratch `String` per node to
   avoid the per-node allocation `to_lowercase()` does.

2. **`tree::Tree::name_lower_bytes(idx) -> &[u8]`** — O(1)
   accessor for tests + any future caller that needs a
   single name.

3. **`ffi::apfs_scan_search_names`** — drives the inner
   loop with one `memchr::memmem::Finder` per call,
   reusing across every haystack slice. Same FFI surface,
   same semantics; only the internals changed.

4. **`memchr = "2"`** promoted from transitive dep to a
   direct one. Already in the lockfile via `quick-xml`
   and others; zero additional footprint.

## Headroom not addressed

For pathological 1-character queries the ancestor walk
dominates. Three angles for a future EX-33b if needed:

- **Skip-walk shortcut for "match everything" queries.**
  If matches > N% of nodes, just return all node indices
  (the keep-set IS the full tree). Single `Vec::from_iter`
  vs N parent walks.
- **Trigram pre-filter.** Build a per-node 3-gram set at
  scan time; for queries ≥ 3 chars, skip nodes whose
  trigrams don't include the query's trigrams. Probably
  not worth it — typical search latency is already inside
  100 ms.
- **ASCII fast path for `to_lowercase` on the query.** For
  ASCII-only queries (the common case), skip the Unicode
  folder and just byte-flip. Saves ~µs of setup. Below
  noise for typical workloads.

None are landing in this commit. The current numbers
satisfy the "feels live" latency budget across every
realistic query shape.

## Artifact

`artifacts/generated/ex33_search_bench_2026-05-21.json`
holds the post-lift numbers (target / node count / scan
time / per-query times across 5 iterations / min / median
/ max). Re-running the bench is a single command:

    cargo build --release --example bench_search -p apfs-fastindex
    ./target/release/examples/bench_search /Users/kai
