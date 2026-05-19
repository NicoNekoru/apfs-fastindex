//! Standalone perf probe for the indexing path.
//!
//! Usage:
//!   cargo run --release --example perf_probe -- <path> [threads]
//!
//! Reports:
//!   - wall time for `fallback_scan_path` (walker + per-entry stat)
//!   - wall time for `Tree::build` (synthesised tree)
//!   - entry / node counts
//!   - peak RSS (parsed from `getrusage`)
//!   - heap size of the entry vec + tree node vec (struct-only,
//!     not including String/Vec backing storage)

use std::time::Instant;

use apfs_fastindex::fallback::{fallback_scan_path_with_options, FallbackOptions};
use apfs_fastindex::tree::{Tree, TreeNode};
use apfs_fastindex::NamespaceEntry;

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
    let scan_t0 = Instant::now();
    let out = fallback_scan_path_with_options(&path, opts).expect("scan failed");
    let scan_ms = scan_t0.elapsed().as_secs_f64() * 1000.0;
    let entries = out.parser_output.entries;
    let entry_count = entries.len();

    let tree_t0 = Instant::now();
    let tree = Tree::build(&entries);
    let tree_ms = tree_t0.elapsed().as_secs_f64() * 1000.0;
    let node_count = tree.nodes.len();

    let entry_struct_bytes =
        entry_count as u64 * std::mem::size_of::<NamespaceEntry>() as u64;
    let node_struct_bytes =
        node_count as u64 * std::mem::size_of::<TreeNode>() as u64;
    let rss_bytes = peak_rss_bytes();

    println!("path                = {}", path);
    println!("threads             = {}", threads);
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
