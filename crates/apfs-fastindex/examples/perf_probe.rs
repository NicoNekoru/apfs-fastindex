//! Standalone perf probe + ablation rig for the indexing path.
//!
//! Usage:
//!   cargo run --release --example perf_probe -- <path> [threads]
//!
//! Per-phase timings (walk / sort / aggregates) print to stderr
//! when `APFS_PHASE_TIMINGS=1` is set — handy for the ablation
//! sweeps below.
//!
//! Also runs a custom counting allocator that totals every
//! `alloc` / `dealloc` call so we can quantify per-phase
//! allocation pressure without external profilers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use apfs_fastindex::fallback::{fallback_scan_path_with_options, FallbackOptions};
use apfs_fastindex::tree::{Tree, TreeNode};
use apfs_fastindex::NamespaceEntry;

/// Counting wrapper around the system allocator. Tracks total
/// allocations + bytes allocated since process start; the probe
/// snapshots before/after each phase. Cost per call: two relaxed
/// atomics — single-digit ns, swamped by the actual allocator
/// work, so timing impact is negligible (verified by comparing
/// the wall-clock with/without the wrapper on a sample run).
struct CountingAlloc;

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

#[derive(Copy, Clone)]
struct AllocStats {
    count: u64,
    bytes: u64,
}

fn snap() -> AllocStats {
    AllocStats {
        count: ALLOC_COUNT.load(Ordering::Relaxed),
        bytes: ALLOC_BYTES.load(Ordering::Relaxed),
    }
}

fn delta(before: AllocStats, after: AllocStats) -> AllocStats {
    AllocStats {
        count: after.count.saturating_sub(before.count),
        bytes: after.bytes.saturating_sub(before.bytes),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: perf_probe <path> [threads]");
        std::process::exit(2);
    });
    let threads: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    let opts = FallbackOptions {
        cross_mounts: false,
        threads,
        progress: None,
    };

    let pre_scan = snap();
    let scan_t0 = Instant::now();
    let out = fallback_scan_path_with_options(&path, opts).expect("scan failed");
    let scan_ms = scan_t0.elapsed().as_secs_f64() * 1000.0;
    let post_scan = snap();
    let scan_allocs = delta(pre_scan, post_scan);

    let entries = out.parser_output.entries;
    let entry_count = entries.len();

    let pre_tree = snap();
    let tree_t0 = Instant::now();
    let tree = Tree::build(&entries);
    let tree_ms = tree_t0.elapsed().as_secs_f64() * 1000.0;
    let post_tree = snap();
    let tree_allocs = delta(pre_tree, post_tree);
    let node_count = tree.nodes.len();

    let entry_struct_bytes =
        entry_count as u64 * std::mem::size_of::<NamespaceEntry>() as u64;
    let node_struct_bytes =
        node_count as u64 * std::mem::size_of::<TreeNode>() as u64;
    let rss_bytes = peak_rss_bytes();

    println!("path                = {}", path);
    println!("threads             = {}", threads);
    println!(
        "bulk buf            = {} KiB",
        std::env::var("APFS_BULK_BUF_KIB").unwrap_or_else(|_| "64".to_string())
    );
    println!("scan wall           = {:.1} ms", scan_ms);
    println!("tree wall           = {:.1} ms", tree_ms);
    println!("entry count         = {}", entry_count);
    println!("node count          = {}", node_count);
    println!(
        "size_of NamespaceEntry = {}",
        std::mem::size_of::<NamespaceEntry>()
    );
    println!("size_of TreeNode    = {}", std::mem::size_of::<TreeNode>());
    println!(
        "entry vec structs   = {:.1} MiB",
        entry_struct_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "tree node structs   = {:.1} MiB",
        node_struct_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "peak RSS            = {:.1} MiB",
        rss_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "scan rate           = {:.2} µs/entry",
        scan_ms * 1000.0 / entry_count as f64
    );
    println!(
        "tree rate           = {:.2} µs/entry",
        tree_ms * 1000.0 / entry_count as f64
    );
    println!(
        "scan allocs         = {} ({:.1} MiB)",
        scan_allocs.count,
        scan_allocs.bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "tree allocs         = {} ({:.1} MiB)",
        tree_allocs.count,
        tree_allocs.bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "scan allocs/entry   = {:.2}",
        scan_allocs.count as f64 / entry_count.max(1) as f64
    );
}

/// `getrusage(RUSAGE_SELF).ru_maxrss`. On macOS the units are
/// *bytes*; on Linux they're KiB. We're macOS-only.
#[cfg(target_os = "macos")]
fn peak_rss_bytes() -> u64 {
    use std::mem::MaybeUninit;
    unsafe {
        let mut usage: MaybeUninit<libc::rusage> = MaybeUninit::uninit();
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) == 0 {
            usage.assume_init().ru_maxrss as u64
        } else {
            0
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn peak_rss_bytes() -> u64 {
    0
}
