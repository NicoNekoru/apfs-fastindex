//! EX-33: end-to-end performance bench for the search FFI.
//!
//! Runs a real scan against a caller-supplied path, then
//! exercises `apfs_scan_search_names` with a curated set of
//! query patterns that stress different parts of the pipeline:
//!
//! - One-letter queries (worst case for match count + ancestor walk)
//! - Short ASCII tokens (typical user input)
//! - Long substrings (longest sub-pattern; floor for memcmp work)
//! - Non-ASCII queries (force the Unicode path)
//! - Zero-match queries (best case for the inner loop)
//!
//! Each query runs 5 iterations; reports min / median / max
//! wall time + match count. Saves results as JSON for the
//! EX-33 README to ingest.
//!
//! Usage (from repo root):
//!   cargo build --release --example bench_search -p apfs-fastindex
//!   ./target/release/examples/bench_search /Users/kai
//!
//! Default target is `~/Projects/apfs-fastindex` (a small tree
//! that runs quickly for smoke checks). Pass any directory as
//! arg #1.

use std::ffi::CString;
use std::time::Instant;

use apfs_fastindex::ffi::{
    apfs_scan_directory, apfs_scan_free, apfs_scan_node_count, apfs_scan_search_names,
    apfs_search_results_count, apfs_search_results_free,
};

#[derive(Debug)]
struct QueryStats {
    query: &'static str,
    matches: usize,
    times_ns: Vec<u128>,
}

impl QueryStats {
    fn min_ns(&self) -> u128 {
        *self.times_ns.iter().min().unwrap_or(&0)
    }
    fn med_ns(&self) -> u128 {
        let mut v = self.times_ns.clone();
        v.sort_unstable();
        v[v.len() / 2]
    }
    fn max_ns(&self) -> u128 {
        *self.times_ns.iter().max().unwrap_or(&0)
    }
}

fn fmt_us(ns: u128) -> String {
    if ns >= 1_000_000 {
        format!("{:.2} ms", ns as f64 / 1_000_000.0)
    } else if ns >= 1_000 {
        format!("{:.1} µs", ns as f64 / 1_000.0)
    } else {
        format!("{ns} ns")
    }
}

/// Curated query set. Names mostly target Apple-silicon
/// `/Users/<me>` shape: matches against extension suffixes,
/// common directory names, and one-char misses.
const QUERIES: &[&str] = &[
    // One-letter ASCII. Highest match counts; stresses the
    // ancestor walk + the `contains` short-needle path.
    "e",
    "a",
    // Common extension suffix. Matches every file of that type.
    ".txt",
    ".log",
    // Short directory-name tokens. Typical user input shape.
    "Photo",
    "Library",
    "Cache",
    "node_modules",
    // Long substring. Floor for the `memchr`-style scan
    // (needle length increases per-comparison cost slightly).
    "com.apple.developer",
    // Non-ASCII to force the to_lowercase path away from
    // ASCII fast-folding.
    "Übersicht",
    // Zero-match: needle that doesn't appear anywhere. The
    // contains scan still touches every byte but never
    // matches → no ancestor walk.
    "zzzz_no_match_zzzz_aardvark",
];

const ITERATIONS_PER_QUERY: usize = 5;

fn main() {
    let target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|h| format!("{h}/Projects/apfs-fastindex"))
                .unwrap_or_else(|_| "/tmp".into())
        });
    println!("=== EX-33 search bench ===");
    println!("target: {target}");

    let c_target = CString::new(target.clone()).expect("path has interior NUL");

    let t = Instant::now();
    let scan = apfs_scan_directory(c_target.as_ptr(), 4, false);
    let scan_elapsed = t.elapsed();
    if scan.is_null() {
        eprintln!("scan failed; aborting");
        std::process::exit(1);
    }
    let node_count = apfs_scan_node_count(scan);
    println!("scan:    {:>10}, {} nodes", fmt_us(scan_elapsed.as_nanos()), node_count);

    let mut stats: Vec<QueryStats> = Vec::new();
    for &query in QUERIES {
        let cq = CString::new(query).expect("query has interior NUL");
        let mut times: Vec<u128> = Vec::with_capacity(ITERATIONS_PER_QUERY);
        let mut last_count: usize = 0;
        for _ in 0..ITERATIONS_PER_QUERY {
            let t = Instant::now();
            let r = apfs_scan_search_names(scan, cq.as_ptr());
            let elapsed = t.elapsed();
            last_count = if r.is_null() {
                0
            } else {
                let c = apfs_search_results_count(r);
                apfs_search_results_free(r);
                c
            };
            times.push(elapsed.as_nanos());
        }
        stats.push(QueryStats {
            query,
            matches: last_count,
            times_ns: times,
        });
    }

    println!();
    println!("{:<32} {:>10} {:>12} {:>10} {:>10} {:>10}",
             "query", "matches", "kept-set", "min", "median", "max");
    for s in &stats {
        // `matches` here is the FFI return — the keep-set
        // (matches + ancestors + root). The actual "matched
        // by name" count isn't returned by the current FFI.
        // We report it as the kept-set size, which is what
        // the UI uses.
        println!(
            "{:<32} {:>10} {:>12} {:>10} {:>10} {:>10}",
            format!("{:?}", s.query),
            "—",
            s.matches,
            fmt_us(s.min_ns()),
            fmt_us(s.med_ns()),
            fmt_us(s.max_ns())
        );
    }

    // JSON dump for the EX-33 README ingestion.
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into());
    let json_path = format!(
        "/tmp/ex33_search_bench_{}.json",
        chrono_today_iso().unwrap_or_else(|| "today".into()),
    );
    let mut json = String::new();
    json.push_str("{\n");
    json.push_str(&format!("  \"target\": {:?},\n", target));
    json.push_str(&format!("  \"node_count\": {},\n", node_count));
    json.push_str(&format!("  \"scan_ns\": {},\n", scan_elapsed.as_nanos()));
    json.push_str(&format!("  \"iterations_per_query\": {},\n", ITERATIONS_PER_QUERY));
    json.push_str(&format!("  \"host\": {:?},\n", host));
    json.push_str("  \"queries\": [\n");
    for (i, s) in stats.iter().enumerate() {
        json.push_str("    {\n");
        json.push_str(&format!("      \"query\": {:?},\n", s.query));
        json.push_str(&format!("      \"kept_set_size\": {},\n", s.matches));
        json.push_str(&format!(
            "      \"times_ns\": [{}],\n",
            s.times_ns
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        json.push_str(&format!("      \"min_ns\": {},\n", s.min_ns()));
        json.push_str(&format!("      \"med_ns\": {},\n", s.med_ns()));
        json.push_str(&format!("      \"max_ns\": {}\n", s.max_ns()));
        json.push_str(if i + 1 < stats.len() { "    },\n" } else { "    }\n" });
    }
    json.push_str("  ]\n");
    json.push_str("}\n");
    if std::fs::write(&json_path, &json).is_ok() {
        println!();
        println!("output: {json_path}");
    }

    apfs_scan_free(scan);
}

/// Tiny chrono-free today-as-iso-string. Avoids pulling in a
/// dep just for the bench's output filename.
fn chrono_today_iso() -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let days_since_epoch = now.as_secs() / 86400;
    // Convert Unix days → YYYY-MM-DD via a hand-rolled
    // calendar (good enough for filenames; not a real
    // chrono replacement).
    let (y, m, d) = days_to_ymd(days_since_epoch as i64);
    Some(format!("{:04}-{:02}-{:02}", y, m, d))
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Algorithm: shift epoch from 1970-01-01 to a date with
    // 400-year-cycle alignment (0000-03-01), then unpack.
    let mut z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096] — already i64
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = y + if m <= 2 { 1 } else { 0 };
    z = days; // silence unused-mut, kept for clarity
    let _ = z;
    (y, m, d)
}
