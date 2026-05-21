use std::env;
use std::path::Path;
use std::process::ExitCode;

use std::io::Write;

use apfs_fastindex::{
    fallback_scan_path_with_options, FallbackOptions, FallbackScanOutput, ProgressEvent,
};

const USAGE: &str = "usage: apfs-fastindex-scan [--summary] [--pretty] [--slim] \
                     [--format json|msgpack] [--mode raw|fallback|auto] [--cross-mounts] \
                     [--progress] [--threads N] <source-path>\n\
                     source-path may be:\n  \
                     - a detached APFS .dmg image (raw mode)\n  \
                     - a raw APFS container device (/dev/rdiskN) (raw mode)\n  \
                     - a locally mounted directory (fallback mode)\n\
                     --pretty prints indented JSON (default is compact; large scans become \
                     hundreds of MB pretty-printed which strains in-browser JSON.parse). \
                     Implies --format json; ignored under --format msgpack.\n\
                     --slim drops fields the viz does not consume (file_id, aggregates, null \
                     symlink targets, scan_state) so the output fits comfortably in a browser.\n\
                     --format json|msgpack picks the wire encoding. Default json (preserves \
                     the standalone HTML viz's drop-a-file affordance). msgpack uses named-\
                     keyed maps via rmp-serde, ~3x smaller payload + ~3-6x faster client-side \
                     decode for the WKWebView shell.\n\
                     --cross-mounts lets the fallback walker descend into directories on a \
                     different device than the root (default: stop at mount boundaries).\n\
                     --progress writes one JSON object every 250 ms to stderr describing scan \
                     progress (fallback mode only; raw mode emits no progress today). The \
                     parallel walker fires the same events from a dedicated progress thread, \
                     sampled from per-worker atomic counters.\n\
                     --threads N picks the parallel-walker worker count for fallback mode. \
                     Default is min(hw.physicalcpu, 4) per EX-25's 2.47x-at-T=4 verdict; \
                     beyond T=4 the APFS container lock fires and scaling regresses. Pass \
                     --threads 1 for single-threaded (preserves live --progress).";

#[derive(Copy, Clone, PartialEq, Eq)]
enum OutputFormat {
    Json,
    Msgpack,
    /// Sequence of msgpack 2-element arrays, one per record:
    ///   `[header,  {mode, correctness_claim, source, not_claimed}]`
    ///   `[entry,   {path, entry_kind, logical_size, allocated_size, …}]`
    ///   …repeating one entry record per row…
    ///   `[trailer, {done: true, entry_count: N}]`
    /// The viz can start consuming records as bytes arrive — first
    /// paint can land mid-stream once a few thousand entries have
    /// been seen. Fallback mode only today; raw mode keeps the
    /// bulk format. See `viz/index.html`'s streaming decoder.
    MsgpackStream,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    Raw,
    Fallback,
    Auto,
}

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args
        .next()
        .unwrap_or_else(|| "apfs-fastindex-scan".to_string());

    // Long-lived privileged-helper mode (admin-session productisation):
    // when the GUI's `AdminSession` spawns this binary via
    // osascript-with-administrator-privileges, it passes `--server` as
    // the single argument. The CLI then reads tab-delimited commands
    // from stdin (one per line) and acts on them — reusing the same
    // privileged process across all subsequent scans means the auth
    // dialog pops once per session, not once per scan.
    //
    // Protocol:
    //   stdin:  scan<TAB><path><TAB><out_msgpack><TAB><progress_log>\n
    //           quit\n
    //   stdout: ready<TAB>1\n     (emitted once at startup)
    //           ok<TAB><exit>\n   (after each scan)
    //           err<TAB><msg>\n   (malformed command, missing args, etc.)
    //
    // All stdout writes are explicitly flushed so the parent never
    // waits behind pipe buffering.
    // Collect args once so we can both peek for `--server` and
    // continue parsing the rest if it isn't present.
    let args: Vec<String> = args.collect();
    if args.iter().any(|a| a == "--server") {
        return run_server_mode();
    }
    let args = args.into_iter();

    let mut summary_only = false;
    let mut pretty = false;
    let mut slim = false;
    let mut format = OutputFormat::Json;
    let mut mode = Mode::Auto;
    let mut source_path: Option<String> = None;
    let mut pending_mode_value = false;
    let mut pending_threads_value = false;
    let mut pending_format_value = false;
    let mut cross_mounts = false;
    let mut progress = false;
    let mut threads_arg: Option<usize> = None;
    // R4 / EX-30 cache. Off by default (opt-in via --cache) until
    // we've validated cache identity + invalidation against a few
    // more real-world traces. The flag enables both read and write
    // paths; --no-cache forces a fresh scan but still writes
    // (useful for refreshing a stale entry).
    let mut cache_enabled = false;
    let mut cache_force_refresh = false;
    for arg in args {
        if pending_mode_value {
            mode = match arg.as_str() {
                "raw" => Mode::Raw,
                "fallback" => Mode::Fallback,
                "auto" => Mode::Auto,
                other => {
                    eprintln!("apfs-fastindex-scan: unknown --mode value {other:?}");
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            pending_mode_value = false;
            continue;
        }
        if pending_threads_value {
            threads_arg = match parse_threads(arg.as_str()) {
                Ok(n) => Some(n),
                Err(msg) => {
                    eprintln!("apfs-fastindex-scan: --threads: {msg}");
                    return ExitCode::from(2);
                }
            };
            pending_threads_value = false;
            continue;
        }
        if pending_format_value {
            format = match parse_format(arg.as_str()) {
                Ok(f) => f,
                Err(msg) => {
                    eprintln!("apfs-fastindex-scan: --format: {msg}");
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            pending_format_value = false;
            continue;
        }
        match arg.as_str() {
            "--summary" => {
                if summary_only {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
                summary_only = true;
            }
            "--mode" => {
                pending_mode_value = true;
            }
            "--cross-mounts" => {
                cross_mounts = true;
            }
            "--pretty" => {
                pretty = true;
            }
            "--slim" => {
                slim = true;
            }
            "--progress" => {
                progress = true;
            }
            "--cache" => {
                cache_enabled = true;
            }
            "--no-cache" => {
                cache_force_refresh = true;
                cache_enabled = true;
            }
            "--threads" => {
                pending_threads_value = true;
            }
            other if other.starts_with("--threads=") => {
                let value = &other["--threads=".len()..];
                threads_arg = match parse_threads(value) {
                    Ok(n) => Some(n),
                    Err(msg) => {
                        eprintln!("apfs-fastindex-scan: --threads: {msg}");
                        return ExitCode::from(2);
                    }
                };
            }
            "--format" => {
                pending_format_value = true;
            }
            other if other.starts_with("--format=") => {
                let value = &other["--format=".len()..];
                format = match parse_format(value) {
                    Ok(f) => f,
                    Err(msg) => {
                        eprintln!("apfs-fastindex-scan: --format: {msg}");
                        eprintln!("{USAGE}");
                        return ExitCode::from(2);
                    }
                };
            }
            other if other.starts_with("--mode=") => {
                let value = &other["--mode=".len()..];
                mode = match value {
                    "raw" => Mode::Raw,
                    "fallback" => Mode::Fallback,
                    "auto" => Mode::Auto,
                    _ => {
                        eprintln!("apfs-fastindex-scan: unknown --mode value {value:?}");
                        eprintln!("{USAGE}");
                        return ExitCode::from(2);
                    }
                };
            }
            other if other.starts_with("--") => {
                eprintln!("apfs-fastindex-scan: unknown flag {other}");
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            _ => {
                if source_path.is_some() {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
                source_path = Some(arg);
            }
        }
    }
    if pending_mode_value {
        eprintln!("apfs-fastindex-scan: --mode requires a value");
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if pending_threads_value {
        eprintln!("apfs-fastindex-scan: --threads requires a value");
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if pending_format_value {
        eprintln!("apfs-fastindex-scan: --format requires a value");
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if pretty && format == OutputFormat::Msgpack {
        eprintln!("apfs-fastindex-scan: warning: --pretty has no effect under --format msgpack");
    }
    let Some(path) = source_path else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    let effective_mode = match mode {
        Mode::Raw | Mode::Fallback => mode,
        Mode::Auto => auto_detect_mode(&path),
    };

    match effective_mode {
        Mode::Raw => {
            if cross_mounts {
                eprintln!("apfs-fastindex-scan: warning: --cross-mounts has no effect in raw mode");
            }
            if progress {
                eprintln!("apfs-fastindex-scan: warning: --progress has no effect in raw mode (no streaming hooks yet)");
            }
            if threads_arg.is_some() {
                eprintln!("apfs-fastindex-scan: warning: --threads has no effect in raw mode (the raw decoder is single-threaded by design; raw-tree b-trees are walked in order)");
            }
            run_raw(&path, summary_only, pretty, slim, format)
        }
        Mode::Fallback => {
            let threads = threads_arg.unwrap_or_else(default_fallback_threads);
            run_fallback(
                &path,
                summary_only,
                cross_mounts,
                pretty,
                slim,
                progress,
                threads,
                format,
                cache_enabled,
                cache_force_refresh,
            )
        }
        Mode::Auto => unreachable!("auto resolves to Raw or Fallback above"),
    }
}

/// Parse `--format json|msgpack|msgpack-stream` into the
/// `OutputFormat` enum. Rejects anything else so the user doesn't
/// accidentally get a silent JSON fallback after typoing the
/// encoding name.
fn parse_format(value: &str) -> Result<OutputFormat, String> {
    match value {
        "json" => Ok(OutputFormat::Json),
        "msgpack" => Ok(OutputFormat::Msgpack),
        "msgpack-stream" => Ok(OutputFormat::MsgpackStream),
        other => Err(format!(
            "unknown value {other:?}; expected json, msgpack, or msgpack-stream"
        )),
    }
}

/// Parse `--threads N` into a strict positive integer. Reject 0 and
/// non-numeric values so the caller doesn't accidentally pass
/// "--threads=auto" expecting it to mean "default."
fn parse_threads(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        Ok(_) => Err("must be >= 1".to_string()),
        Err(err) => Err(format!("not a positive integer: {err}")),
    }
}

/// CLI default thread count for fallback mode, per EX-25's verdict.
/// On Apple silicon hosts the optimum is T=4 (2.47× of T=1); beyond
/// that the APFS container lock fires and sys-CPU grows super-
/// linearly. We clamp to `hw.physicalcpu` so smaller hosts don't
/// over-subscribe their physical cores.
fn default_fallback_threads() -> usize {
    const CEILING: usize = 4;
    let physical = physical_cpu_count();
    physical.clamp(1, CEILING)
}

/// Read `hw.physicalcpu` via `sysctlbyname`. Falls back to
/// `std::thread::available_parallelism()` (which on macOS returns
/// logical CPUs) if the sysctl fails. The fallback is safe because
/// `default_fallback_threads` clamps the result to `<= CEILING` so
/// even a logical-CPU overshoot is bounded.
#[cfg(target_os = "macos")]
fn physical_cpu_count() -> usize {
    let name = std::ffi::CString::new("hw.physicalcpu").expect("static cstring");
    let mut value: i32 = 0;
    let mut size: libc::size_t = std::mem::size_of::<i32>();
    // SAFETY: sysctlbyname writes at most `size` bytes into `value`;
    // value is a valid &mut i32 and size is the matching length.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut i32 as *mut std::ffi::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && value > 0 {
        value as usize
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }
}

#[cfg(not(target_os = "macos"))]
fn physical_cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn auto_detect_mode(path: &str) -> Mode {
    if path.starts_with("/dev/") {
        return Mode::Raw;
    }
    let p = Path::new(path);
    if p.is_dir() {
        return Mode::Fallback;
    }
    if p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("dmg"))
        .unwrap_or(false)
    {
        return Mode::Raw;
    }
    // Default to raw and let the source gate reject if unsupported.
    Mode::Raw
}

fn run_raw(
    path: &str,
    summary_only: bool,
    pretty: bool,
    slim: bool,
    format: OutputFormat,
) -> ExitCode {
    match apfs_fastindex::checkpoint_scan_source(path) {
        Ok(output) => {
            if summary_only {
                print_summary(
                    "raw",
                    &output.correctness_claim,
                    output.parser_output.entries.len(),
                    output.parser_output.aggregates.len(),
                    &output.not_claimed,
                );
                return ExitCode::SUCCESS;
            }
            if slim {
                let envelope = slim_raw_envelope(&output);
                emit_output(&envelope, pretty, format)
            } else {
                emit_output(&output, pretty, format)
            }
        }
        Err(err) => {
            eprintln!("apfs-fastindex-scan: {err}");
            ExitCode::from(1)
        }
    }
}

fn run_fallback(
    path: &str,
    summary_only: bool,
    cross_mounts: bool,
    pretty: bool,
    slim: bool,
    progress: bool,
    threads: usize,
    format: OutputFormat,
    cache_enabled: bool,
    cache_force_refresh: bool,
) -> ExitCode {
    // R4 cache hot-path. When `--cache` is on, compute the
    // directory signature first and consult the cache. On hit:
    // stream the cached msgpack straight to stdout. On miss:
    // fall through to the fresh walker + write the cache after.
    //
    // `--no-cache` forces a fresh scan but still updates the
    // cache; that's the "refresh" gesture.
    //
    // Cache is fallback-only and msgpack-only (the JSON path
    // sees no benefit because most callers using it are
    // dev/debug). The cache_emit_msgpack helper below validates
    // both preconditions before serving from cache.
    let cache_state = if cache_enabled {
        match cache_check(std::path::Path::new(path), cache_force_refresh) {
            Ok(state) => state,
            Err(err) => {
                eprintln!(
                    "apfs-fastindex-scan: cache: probe failed: {err} (falling through to fresh scan)"
                );
                CacheState::Miss(None)
            }
        }
    } else {
        CacheState::Disabled
    };
    if let CacheState::Hit { ref scan_blob_path, ref manifest } = cache_state {
        if let Some(exit) = cache_emit_msgpack(
            scan_blob_path,
            manifest,
            summary_only,
            format,
        ) {
            return exit;
        }
        // Format wasn't msgpack-stream-compatible, or the blob
        // failed to read. Fall through to the fresh walk.
    }

    let mut progress_writer = |event: ProgressEvent| {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "{{\"scanned\":{},\"skipped\":{},\"bytes\":{},\"elapsed_ms\":{},\"terminal\":{}}}",
            event.scanned,
            event.skipped,
            event.bytes,
            event.elapsed.as_millis(),
            event.terminal
        );
    };
    let options = FallbackOptions {
        cross_mounts,
        progress: if progress {
            Some(&mut progress_writer as &mut (dyn FnMut(ProgressEvent) + Send))
        } else {
            None
        },
        threads,
        // CLI emits aggregates in its JSON output; keep them
        // computed. FFI / SwiftUI uses the tree's subtree
        // totals directly and flips this to `true`.
        skip_aggregates: false,
    };
    match fallback_scan_path_with_options(path, options) {
        Ok(output) => {
            if summary_only {
                print_summary_with_skips(
                    "fallback",
                    &output.correctness_claim,
                    output.parser_output.entries.len(),
                    output.parser_output.aggregates.len(),
                    output.parser_output.walk_skips.len(),
                    &output.not_claimed,
                );
                return ExitCode::SUCCESS;
            }
            // Stash for cache write (after emission, so we don't
            // delay stdout for cache I/O — the cache write is
            // fire-and-forget for the next run's benefit).
            let cache_key_sig = match cache_state {
                CacheState::Miss(probe) => probe,
                _ => None,
            };
            let entry_count = output.parser_output.entries.len();
            let agg_count = output.parser_output.aggregates.len();
            let exit = emit_fallback_output(&output, pretty, slim, format);
            if cache_enabled && exit == ExitCode::SUCCESS {
                if let Some((key, sig)) = cache_key_sig {
                    if let Err(err) =
                        cache_write_msgpack(&key, sig, entry_count, agg_count, &output)
                    {
                        eprintln!("apfs-fastindex-scan: cache: write failed: {err}");
                    }
                }
            }
            exit
        }
        Err(err) => {
            eprintln!("apfs-fastindex-scan: fallback: {err}");
            ExitCode::from(1)
        }
    }
}

enum CacheState {
    Disabled,
    /// No usable cache entry. The `Option` carries the computed
    /// `(key, signature)` so a subsequent successful scan can
    /// write it back without recomputing.
    Miss(Option<(apfs_fastindex::cache::CacheKey, u64)>),
    Hit {
        scan_blob_path: std::path::PathBuf,
        manifest: apfs_fastindex::cache::CacheManifest,
    },
}

/// Compute the cache identity, probe the directory signature,
/// and look up the cache. Returns the resulting state.
///
/// **Known v1 cost.** The probe (dir-only walk of the scan
/// root) is naive `read_dir` + `symlink_metadata`. On stable
/// trees up to ~10k dirs it's effectively free (< 50 ms); on
/// the 172k-dir `/Users/kai` tree it's ~37 s — slower than
/// the full `getattrlistbulk`-based scan that would otherwise
/// run, and the signature changes between consecutive runs
/// because of background mtime churn (browser caches, Spotlight,
/// log rotation) so the cache never actually hits on that
/// tree. Net effect on `/Users/kai`-class scans: the cache
/// makes things slower.
///
/// The fix is a follow-up: have the walker emit its own
/// per-directory signature inline (cheap getattrlistbulk
/// already knows the dir mtime/ctime), then store per-subtree
/// signatures in the manifest so a partial change invalidates
/// only one subtree. That's a meaningful refactor — out of
/// scope for the v1 commit. Documented in
/// `docs/research/experiments/EX-30-perf-baseline/README.md`.
///
/// `force_refresh` skips the lookup step (treats every hit as
/// stale) but still probes so the recomputed signature lands
/// in the refreshed manifest.
fn cache_check(
    path: &std::path::Path,
    force_refresh: bool,
) -> Result<CacheState, apfs_fastindex::cache::CacheError> {
    use apfs_fastindex::cache;
    let key = cache::CacheKey::for_path(path)?;
    let (sig, _) = cache::compute_directory_signature(path)?;
    if !force_refresh {
        if let Some((manifest, blob)) = cache::cache_lookup(&key, sig)? {
            eprintln!(
                "apfs-fastindex-scan: cache: hit ({} entries, {} dir aggregates, msgpack {} bytes)",
                manifest.entry_count, manifest.aggregate_count, manifest.scan_msgpack_bytes
            );
            return Ok(CacheState::Hit {
                scan_blob_path: blob,
                manifest,
            });
        }
    }
    Ok(CacheState::Miss(Some((key, sig))))
}

/// Stream the cached msgpack blob to stdout. Returns
/// `Some(ExitCode)` if the cache served the request, `None` if
/// the format is incompatible and the caller should fall
/// through. Currently the cache only serves
/// `--format msgpack`; JSON and msgpack-stream go through the
/// walker.
fn cache_emit_msgpack(
    scan_blob_path: &std::path::Path,
    manifest: &apfs_fastindex::cache::CacheManifest,
    summary_only: bool,
    format: OutputFormat,
) -> Option<ExitCode> {
    if summary_only {
        // Synthesize a summary from the manifest — entries +
        // aggregates are recorded there, the correctness_claim
        // is fixed-text. Cheaper than reading the msgpack.
        print_summary_with_skips(
            "fallback",
            "(cache hit; correctness_claim deferred to next refresh)",
            manifest.entry_count,
            manifest.aggregate_count,
            0,
            &[],
        );
        return Some(ExitCode::SUCCESS);
    }
    if format != OutputFormat::Msgpack {
        return None;
    }
    let bytes = match std::fs::read(scan_blob_path) {
        Ok(b) => b,
        Err(err) => {
            eprintln!(
                "apfs-fastindex-scan: cache: read {scan_blob_path:?} failed: {err}; refreshing"
            );
            return None;
        }
    };
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&bytes).is_err() {
        return Some(ExitCode::from(1));
    }
    Some(ExitCode::SUCCESS)
}

/// Serialise the fresh scan to msgpack (envelope-shaped, same
/// as the live emission produces) and persist via
/// `cache_save`. Errors propagate so the caller can log; cache
/// failures must never bring down the scan.
fn cache_write_msgpack(
    key: &apfs_fastindex::cache::CacheKey,
    signature: u64,
    entry_count: usize,
    aggregate_count: usize,
    output: &FallbackScanOutput,
) -> Result<(), apfs_fastindex::cache::CacheError> {
    // Cache the envelope shape (not the raw FallbackScanOutput)
    // so a cache-hit byte-for-byte matches what the live path
    // would have written to stdout. Cache currently only serves
    // non-slim msgpack; slim is rarely used outside the viz
    // dev loop and not worth caching separately.
    let envelope = fallback_envelope(output, false);
    let bytes = rmp_serde::to_vec_named(&envelope)
        .map_err(|e| apfs_fastindex::cache::CacheError::Serialize(e.to_string()))?;
    apfs_fastindex::cache::cache_save(key, signature, entry_count, aggregate_count, &bytes)
}

fn emit_fallback_output(
    output: &FallbackScanOutput,
    pretty: bool,
    slim: bool,
    format: OutputFormat,
) -> ExitCode {
    // The streaming format unbundles the envelope into a sequence
    // of records (header → entries → trailer). The viz consumes
    // those records as bytes arrive so the first paint can land
    // mid-stream. Falls through the slim flag — slim drops the
    // file_id / aggregate / walk_skips noise the streaming
    // consumer doesn't need anyway.
    if format == OutputFormat::MsgpackStream {
        return emit_msgpack_stream(output, slim);
    }
    let envelope = fallback_envelope(output, slim);
    emit_output(&envelope, pretty, format)
}

/// Construct the wire-shape envelope a downstream consumer
/// (viz, Swift app, cache reader) expects. Split out of
/// `emit_fallback_output` so the cache write path produces a
/// byte-for-byte match with the live emission — the cache hit
/// must serve the same bytes the fresh scan would, otherwise
/// a refresh-vs-cache-hit cycle subtly changes downstream
/// behaviour.
fn fallback_envelope(output: &FallbackScanOutput, slim: bool) -> serde_json::Value {
    if slim {
        slim_fallback_envelope(output)
    } else {
        // Wrap the fallback output so consumers can read `mode` first
        // and pick the right schema. Raw output stays in its existing
        // top-level shape for backward compatibility.
        serde_json::json!({
            "mode": "fallback",
            "parser_output": output.parser_output,
            "correctness_claim": output.correctness_claim,
            "not_claimed": output.not_claimed,
        })
    }
}

/// Stream the fallback scan output as a sequence of msgpack
/// 2-element arrays. Each top-level value is `[kind, payload]`;
/// the viz reads them one at a time and inserts entries into a
/// growing tree as they arrive.
///
/// Wire shape (msgpack pseudocode):
///   `[ "header",  { mode, correctness_claim, source, not_claimed } ]`
///   `[ "entry",   { path, entry_kind, logical_size, allocated_size?, symlink_target? } ]`
///   …repeated…
///   `[ "trailer", { done: true, entry_count: N } ]`
///
/// `slim` drops the `allocated_size`/`symlink_target` fields when
/// they're null (matching the existing slim envelope shape) so
/// the wire stays terse.
fn emit_msgpack_stream(output: &FallbackScanOutput, slim: bool) -> ExitCode {
    let parser = &output.parser_output;
    let stdout_unlocked = std::io::stdout();
    let mut stdout = stdout_unlocked.lock();

    // Header record. The viz reads this first to set `mode`,
    // `correctness_claim`, the source descriptor, and the
    // `not_claimed` list (so the SR-019 "unclaimed" provenance
    // shows up immediately, before any entry arrives).
    let header_payload = serde_json::json!({
        "mode": "fallback",
        "correctness_claim": output.correctness_claim,
        "not_claimed": output.not_claimed,
        "source": parser.source,
        "backend_name": parser.backend_name,
    });
    if let Err(err) = write_stream_record(&mut stdout, "header", &header_payload) {
        eprintln!("apfs-fastindex-scan: stream header: {err}");
        return ExitCode::from(1);
    }
    // Flush the header so the viz can start drawing the
    // breadcrumb / status bar before any entries land.
    if let Err(err) = stdout.flush() {
        eprintln!("apfs-fastindex-scan: stream flush after header: {err}");
        return ExitCode::from(1);
    }

    // Entry records. Slim payload matches `slim_entries` so the
    // viz's existing entry shape works unchanged on the receiving
    // side.
    let mut emitted: u64 = 0;
    for entry in &parser.entries {
        let payload = if slim {
            slim_stream_entry(entry)
        } else {
            full_stream_entry(entry)
        };
        if let Err(err) = write_stream_record(&mut stdout, "entry", &payload) {
            eprintln!("apfs-fastindex-scan: stream entry {emitted}: {err}");
            return ExitCode::from(1);
        }
        emitted += 1;
        // Flush every ~4096 entries (≈ a 256 KB chunk at typical
        // sizes) so the viz sees a steady byte stream instead of
        // a stdout-pipe-buffered batch dump. The throughput cost
        // of an explicit flush at this cadence is in the noise
        // (~µs each), but the responsiveness win is real on a
        // /-scale scan.
        if emitted % 4096 == 0 {
            let _ = stdout.flush();
        }
    }

    // Trailer record. `done: true` lets the viz commit any final
    // rendering and dismiss the loading spinner.
    let trailer_payload = serde_json::json!({
        "done": true,
        "entry_count": emitted,
    });
    if let Err(err) = write_stream_record(&mut stdout, "trailer", &trailer_payload) {
        eprintln!("apfs-fastindex-scan: stream trailer: {err}");
        return ExitCode::from(1);
    }
    let _ = stdout.flush();
    ExitCode::SUCCESS
}

/// Emit one `[kind, payload]` msgpack record to `writer`. The
/// 2-element array framing is what the JS streaming decoder
/// switches on to dispatch records to their handlers.
fn write_stream_record<W: std::io::Write, P: serde::Serialize>(
    writer: &mut W,
    kind: &str,
    payload: &P,
) -> Result<(), Box<dyn std::error::Error>> {
    let record = (kind, payload);
    rmp_serde::encode::write_named(writer, &record)?;
    Ok(())
}

fn slim_stream_entry(entry: &apfs_fastindex::NamespaceEntry) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "path".to_string(),
        serde_json::Value::String(entry.path.to_string()),
    );
    obj.insert(
        "entry_kind".to_string(),
        serde_json::to_value(entry.entry_kind).unwrap(),
    );
    obj.insert(
        "logical_size".to_string(),
        serde_json::Value::Number(entry.logical_size.into()),
    );
    if let Some(alloc) = entry.allocated_size {
        obj.insert(
            "allocated_size".to_string(),
            serde_json::Value::Number(alloc.into()),
        );
    } else {
        // Explicit null so the viz's None-collapse logic
        // (SR-019 / EX-22) sees the unclaimed marker.
        obj.insert("allocated_size".to_string(), serde_json::Value::Null);
    }
    if let Some(target) = &entry.symlink_target {
        obj.insert(
            "symlink_target".to_string(),
            serde_json::Value::String(target.to_string()),
        );
    }
    serde_json::Value::Object(obj)
}

fn full_stream_entry(entry: &apfs_fastindex::NamespaceEntry) -> serde_json::Value {
    // For the non-slim case, serialise the entry verbatim. Cheaper
    // than building the map by hand and stays in sync if the
    // entry struct grows new fields.
    serde_json::to_value(entry).unwrap_or(serde_json::Value::Null)
}

/// Build a viz-tuned envelope from a fallback scan: drop `file_id`,
/// `aggregates`, `scan_state`, and null `symlink_target` entries. The
/// treemap viz reconstructs aggregates from the slimmed entry list.
fn slim_fallback_envelope(output: &FallbackScanOutput) -> serde_json::Value {
    serde_json::json!({
        "mode": "fallback",
        "correctness_claim": output.correctness_claim,
        "not_claimed": output.not_claimed,
        "parser_output": {
            "source": output.parser_output.source,
            "backend_name": output.parser_output.backend_name,
            "entries": slim_entries(&output.parser_output.entries),
            "aggregates": [],
            "walk_skips": output.parser_output.walk_skips,
        },
    })
}

fn slim_raw_envelope(output: &apfs_fastindex::CheckpointScanOutput) -> serde_json::Value {
    serde_json::json!({
        "mode": "raw",
        "correctness_claim": output.correctness_claim,
        "not_claimed": output.not_claimed,
        "parser_output": {
            "source": output.parser_output.source,
            "backend_name": output.parser_output.backend_name,
            "entries": slim_entries(&output.parser_output.entries),
            "aggregates": [],
            "walk_skips": output.parser_output.walk_skips,
        },
    })
}

fn slim_entries(entries: &[apfs_fastindex::NamespaceEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|entry| {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "path".to_string(),
                serde_json::Value::String(entry.path.to_string()),
            );
            obj.insert(
                "entry_kind".to_string(),
                serde_json::to_value(entry.entry_kind).unwrap(),
            );
            obj.insert(
                "logical_size".to_string(),
                serde_json::Value::Number(entry.logical_size.into()),
            );
            if let Some(target) = &entry.symlink_target {
                obj.insert(
                    "symlink_target".to_string(),
                    serde_json::Value::String(target.to_string()),
                );
            }
            serde_json::Value::Object(obj)
        })
        .collect()
}

fn emit_output<T: serde::Serialize>(value: &T, pretty: bool, format: OutputFormat) -> ExitCode {
    match format {
        OutputFormat::Json => emit_json(value, pretty),
        OutputFormat::Msgpack => emit_msgpack(value),
        OutputFormat::MsgpackStream => {
            // Streaming is fallback-only today — raw scans go
            // through the bulk path. Falling back to bulk msgpack
            // keeps the wire compatible with the viz's
            // Content-Type sniff, just not the streaming
            // codepath.
            eprintln!(
                "apfs-fastindex-scan: warning: --format msgpack-stream is supported only in \
                 fallback mode; falling back to --format msgpack for this raw scan"
            );
            emit_msgpack(value)
        }
    }
}

fn emit_json<T: serde::Serialize>(value: &T, pretty: bool) -> ExitCode {
    // Compact is the default because large scans become hundreds of MB
    // pretty-printed and JSON.parse chokes in the browser. --pretty opts
    // back into the multi-line form for human inspection of small scans.
    let serialized = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    };
    match serialized {
        Ok(document) => {
            println!("{document}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("apfs-fastindex-scan: failed to serialize scan output: {err}");
            ExitCode::from(1)
        }
    }
}

fn emit_msgpack<T: serde::Serialize>(value: &T) -> ExitCode {
    // `to_vec_named` produces msgpack maps keyed by field name —
    // 1:1 with the JSON shape so the same viz code path can drop
    // either encoding's parse result into `window.ingest`. The
    // payload is ~3× smaller on the wire than compact JSON and
    // ~3-6× faster to decode in WebKit (no UTF-8 → JS-string
    // intermediate, no JSON.parse's reflective object building).
    match rmp_serde::to_vec_named(value) {
        Ok(bytes) => {
            let mut stdout = std::io::stdout().lock();
            if let Err(err) = stdout.write_all(&bytes) {
                eprintln!("apfs-fastindex-scan: failed to write msgpack output: {err}");
                return ExitCode::from(1);
            }
            // No trailing newline — msgpack is a binary framing.
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("apfs-fastindex-scan: failed to serialize msgpack output: {err}");
            ExitCode::from(1)
        }
    }
}

fn print_summary(
    mode: &str,
    correctness_claim: &str,
    entry_count: usize,
    aggregate_count: usize,
    not_claimed: &[String],
) {
    print_summary_with_skips(
        mode,
        correctness_claim,
        entry_count,
        aggregate_count,
        0,
        not_claimed,
    );
}

fn print_summary_with_skips(
    mode: &str,
    correctness_claim: &str,
    entry_count: usize,
    aggregate_count: usize,
    skip_count: usize,
    not_claimed: &[String],
) {
    println!("mode: {mode}");
    println!("correctness_claim: {correctness_claim}");
    println!("entries: {entry_count}");
    println!("aggregates: {aggregate_count}");
    println!("walk_skips: {skip_count}");
    println!("not_claimed:");
    for item in not_claimed {
        println!("  - {item}");
    }
}

/// Long-lived privileged-helper loop. Reads tab-delimited commands
/// from stdin, runs scans, writes status lines to stdout. See the
/// `--server` block in `main` for the full protocol contract.
///
/// Each `ok`/`err`/`ready` line is flushed immediately so the GUI
/// parent observes them in real time. The scan itself writes
/// msgpack to the caller-supplied output path and JSON progress
/// events (one per line, ~250 ms cadence) to the progress path.
/// Privileged-helper server-mode protocol (JSON-line, audit
/// C1 + H1 + H2 fix).
///
/// Audit (V2) replaced the original tab-delimited framing
/// with a JSON-line protocol. The earlier shape took the
/// output and progress paths from the parent's command
/// (`scan\t<path>\t<out>\t<prog>`) — a user-writable tmpdir
/// attacker could pre-place a symlink at one of those paths
/// and the root helper would follow it to /etc/sudoers.d/foo
/// or similar. JSON-line plus helper-chosen tempfiles closes
/// that primitive:
///
/// - **C1**: every per-scan output is created via
///   `tempfile::NamedTempFile` (mkstemp under the hood —
///   atomic `O_EXCL`, mode `0600`, fresh random name). The
///   parent never names a path the helper will write.
/// - **H1**: payloads are JSON; arbitrary control bytes in
///   the path field (TAB, newline, NUL) are handled by
///   serde_json rather than crashing through a fragile
///   tab-split parser.
/// - **H2**: the helper returns the chosen paths to the
///   parent in the reply. The parent stat-verifies root
///   ownership before reading — defence-in-depth on top of
///   the mkstemp-created file already being root-owned in
///   the sticky `/tmp`.
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum HelperCommand {
    /// Run one scan. Helper picks the output + progress
    /// paths and reports them back.
    Scan { path: String },
    /// Helper unlinks each path *if* it created it (tracked
    /// in the helper's owned-paths list). Failure to unlink
    /// is logged to stderr but doesn't fail the call.
    Release { paths: Vec<String> },
    /// Helper drains its remaining created paths and exits.
    Quit,
    /// Unknown command — handled in the dispatcher.
    #[serde(other)]
    Unknown,
}

#[derive(serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum HelperReply<'a> {
    /// Sent once on startup so the parent knows the helper
    /// is alive and reading. `version` lets a future
    /// non-compatible protocol bump be detected by the
    /// parent.
    Ready { version: u32 },
    /// Periodic progress update during a running scan.
    /// Streamed inline on stdout — same channel as the
    /// final Ok/Err — so the parent can update its UI
    /// without polling a side file. Closes the second
    /// half of the C1 attack surface (the original progress
    /// file lived at a parent-supplied path).
    Progress {
        scanned: u64,
        skipped: u64,
        bytes: u64,
        elapsed_ms: u128,
        terminal: bool,
    },
    /// Scan succeeded. `out_path` is helper-chosen, in /tmp,
    /// root-owned mode 0600 (via mkstemp).
    Ok {
        out_path: &'a str,
        exit_code: i32,
    },
    /// Scan failed; message goes to the parent's status UI.
    Err { message: String },
    /// Quit acknowledged. Helper exits immediately after
    /// sending this.
    Bye,
}

fn write_reply<W: std::io::Write>(out: &mut W, reply: &HelperReply<'_>) {
    // One JSON object per line. The helper writes to the
    // parent's socketpair (via AuthorizationExecuteWithPrivileges);
    // the framing is newline-delimited to keep the parent's
    // line-reader straightforward.
    let _ = serde_json::to_writer(&mut *out as &mut dyn std::io::Write, reply);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn run_server_mode() -> ExitCode {
    use std::io::{BufRead, BufReader, Write};

    // Audit M4: tear down the helper if the parent goes
    // away. The OS already closes the parent's end of our
    // socketpair on parent exit, which makes our next
    // `read_line` return EOF — so the loop's existing
    // `Ok(0)` arm handles graceful + crash exit. But if the
    // helper is blocked inside the walker (e.g. a multi-
    // second I/O on /Users) when the parent crashes, we
    // wouldn't notice for the duration of that scan. The
    // kqueue watcher spawns a background thread that
    // `NOTE_EXIT`-monitors the parent PID and aborts the
    // helper the moment it fires. Cheap insurance: one
    // syscall + one wait.
    spawn_parent_death_watcher();

    let threads = default_fallback_threads();

    // Track every tempfile we hand out. On quit (or on a
    // matching `release` command) we unlink them. The parent
    // can't unlink them itself because they're root-owned in
    // sticky `/tmp` — only we (root) can. Bounded by helper
    // lifetime, which is typically the GUI process lifetime.
    let mut owned_paths: Vec<std::path::PathBuf> = Vec::new();

    {
        let mut out = std::io::stdout().lock();
        write_reply(&mut out, &HelperReply::Ready { version: 1 });
    }

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(0) => {
                // Parent closed stdin (likely exited or
                // shutdown). Drain owned tempfiles before
                // returning so they don't accumulate in /tmp.
                cleanup_owned_paths(&owned_paths);
                return ExitCode::SUCCESS;
            }
            Ok(n) => n,
            Err(err) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "apfs-fastindex-scan: server: stdin read error: {err}"
                );
                cleanup_owned_paths(&owned_paths);
                return ExitCode::from(1);
            }
        };
        let _ = n;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        let cmd: HelperCommand = match serde_json::from_str(trimmed) {
            Ok(c) => c,
            Err(err) => {
                let mut out = std::io::stdout().lock();
                write_reply(
                    &mut out,
                    &HelperReply::Err {
                        message: format!("invalid JSON command: {err}"),
                    },
                );
                continue;
            }
        };
        match cmd {
            HelperCommand::Scan { path } => {
                // Defence-in-depth: refuse control bytes in
                // path. `serde_json` already accepts them
                // (JSON strings can carry any UTF-8) but our
                // downstream consumers shouldn't have to
                // worry about embedded TAB/LF/NUL.
                if path.contains('\0') {
                    let mut out = std::io::stdout().lock();
                    write_reply(
                        &mut out,
                        &HelperReply::Err {
                            message: "path contains a NUL byte".to_string(),
                        },
                    );
                    continue;
                }
                match run_server_scan(&path, threads) {
                    Ok((out_path, exit_code)) => {
                        owned_paths.push(out_path.clone());
                        let mut out = std::io::stdout().lock();
                        write_reply(
                            &mut out,
                            &HelperReply::Ok {
                                out_path: out_path.to_string_lossy().as_ref(),
                                exit_code,
                            },
                        );
                    }
                    Err(message) => {
                        let mut out = std::io::stdout().lock();
                        write_reply(&mut out, &HelperReply::Err { message });
                    }
                }
            }
            HelperCommand::Release { paths } => {
                // Only release paths the helper actually
                // created (sanity check against the tracked
                // list); ignores anything else so the parent
                // can't ask us to unlink arbitrary files.
                for p in paths {
                    let pb = std::path::PathBuf::from(&p);
                    if owned_paths.iter().any(|own| own == &pb) {
                        let _ = std::fs::remove_file(&pb);
                        owned_paths.retain(|own| own != &pb);
                    } else {
                        let _ = writeln!(
                            std::io::stderr().lock(),
                            "apfs-fastindex-scan: server: release refused for non-owned path {p}"
                        );
                    }
                }
                let mut out = std::io::stdout().lock();
                // No specific reply payload — the parent
                // doesn't need anything beyond "ack". We
                // emit an empty-out-path Ok so the protocol
                // stays JSON-uniform.
                write_reply(
                    &mut out,
                    &HelperReply::Ok {
                        out_path: "",
                        exit_code: 0,
                    },
                );
            }
            HelperCommand::Quit => {
                cleanup_owned_paths(&owned_paths);
                let mut out = std::io::stdout().lock();
                write_reply(&mut out, &HelperReply::Bye);
                return ExitCode::SUCCESS;
            }
            HelperCommand::Unknown => {
                let mut out = std::io::stdout().lock();
                write_reply(
                    &mut out,
                    &HelperReply::Err {
                        message: "unknown command".to_string(),
                    },
                );
            }
        }
    }
}

fn cleanup_owned_paths(paths: &[std::path::PathBuf]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

/// Audit M4: spawn a background thread that watches the
/// parent process for exit via `kqueue` + `NOTE_EXIT`. When
/// the parent dies, the helper exits immediately rather than
/// continuing to hold root privileges. Belt-and-suspenders
/// to the OS's stdin-EOF-on-parent-death behaviour, which
/// the main loop already handles — but if the helper is
/// blocked in `fallback_scan_path_with_options` during the
/// parent crash, kqueue is the only signal that reaches us
/// before the scan completes.
///
/// Failure modes (kqueue not available, parent already gone,
/// EVFILT_PROC race): the function returns silently and the
/// helper falls back to stdin-EOF detection. Better than
/// nothing.
#[cfg(target_os = "macos")]
fn spawn_parent_death_watcher() {
    use std::thread;
    let parent_pid: i32 = unsafe { libc::getppid() };
    if parent_pid <= 1 {
        // PID 1 (launchd) or unknown — refuse to watch.
        return;
    }
    thread::spawn(move || unsafe {
        let kq = libc::kqueue();
        if kq < 0 {
            return;
        }
        // Register interest in NOTE_EXIT on the parent PID.
        let mut sub: libc::kevent = std::mem::zeroed();
        sub.ident = parent_pid as libc::uintptr_t;
        sub.filter = libc::EVFILT_PROC;
        sub.flags = libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT;
        sub.fflags = libc::NOTE_EXIT;
        let n = libc::kevent(kq, &sub, 1, std::ptr::null_mut(), 0, std::ptr::null());
        if n < 0 {
            libc::close(kq);
            return;
        }
        // Block until the parent exits. `kevent` with a NULL
        // timeout waits indefinitely; the one-shot
        // registration auto-removes after firing.
        let mut out: libc::kevent = std::mem::zeroed();
        let _ = libc::kevent(
            kq,
            std::ptr::null(),
            0,
            &mut out,
            1,
            std::ptr::null(),
        );
        // Parent went away. Exit immediately — we don't have
        // a useful action to take on a parentless root
        // helper. Skip cleanup of `owned_paths` because we
        // can't reach the main thread's state; the OS will
        // unlink-on-reboot for sticky-tmp files anyway, and
        // the parent's `quit` path is the normal cleanup
        // route.
        libc::_exit(0);
    });
}

#[cfg(not(target_os = "macos"))]
fn spawn_parent_death_watcher() {
    // No-op on non-Darwin builds. The crate's only ship target
    // is macOS; non-macOS builds are for unit-test convenience.
}

/// Run one scan from the server loop. Picks its own
/// `NamedTempFile` for the output msgpack via mkstemp (no
/// caller-supplied paths — see C1 fix in the protocol doc).
/// Streams progress events as JSON lines on stdout so the
/// parent gets live updates without a side progress file
/// (which was the second half of the C1 attack surface).
///
/// Returns `(out_path, exit_code)` so the dispatcher can
/// include the path in the final Ok reply. The parent
/// verifies the path's root ownership before reading.
fn run_server_scan(
    path: &str,
    threads: usize,
) -> Result<(std::path::PathBuf, i32), String> {
    use std::io::Write;

    // mkstemp-style: prefix + suffix in /tmp, mode 0600,
    // O_EXCL. `NamedTempFile` owns the file's lifetime; we
    // call `keep()` to detach so the path survives past the
    // function return and is unlinked by the protocol's
    // `release` / `quit` cleanup.
    let tmp_out = match tempfile::Builder::new()
        .prefix("apfs-fastindex-out-")
        .suffix(".msgpack")
        .tempfile_in(std::env::temp_dir())
    {
        Ok(t) => t,
        Err(err) => {
            return Err(format!("could not create output tempfile: {err}"));
        }
    };

    // The progress callback writes one JSON line per event
    // directly to stdout, holding the per-line lock so multi-
    // thread writes don't interleave. The walker invokes the
    // callback from a dedicated progress thread (one event
    // ≈ every 250 ms), so contention with the main thread's
    // final Ok write is negligible.
    let mut progress_writer = |event: ProgressEvent| {
        let mut out = std::io::stdout().lock();
        let reply = HelperReply::Progress {
            scanned: event.scanned,
            skipped: event.skipped,
            bytes: event.bytes,
            elapsed_ms: event.elapsed.as_millis(),
            terminal: event.terminal,
        };
        let _ = serde_json::to_writer(&mut out, &reply);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    };
    let options = FallbackOptions {
        cross_mounts: false,
        progress: Some(&mut progress_writer as &mut (dyn FnMut(ProgressEvent) + Send)),
        threads,
        skip_aggregates: false,
    };

    let scan = match fallback_scan_path_with_options(path, options) {
        Ok(s) => s,
        Err(err) => {
            return Err(format!("scan {path} failed: {err}"));
        }
    };

    let bytes = match rmp_serde::to_vec_named(&scan) {
        Ok(b) => b,
        Err(err) => return Err(format!("msgpack serialise failed: {err}")),
    };
    if let Err(err) = std::fs::write(tmp_out.path(), &bytes) {
        return Err(format!("write output file failed: {err}"));
    }

    // Detach the tempfile from RAII so the path survives
    // past this function. The dispatcher tracks the path in
    // `owned_paths` and unlinks it on `release` or `quit`.
    let (_, out_path) = match tmp_out.keep() {
        Ok(pair) => pair,
        Err(err) => return Err(format!("keep output tempfile failed: {err}")),
    };

    Ok((out_path, 0))
}
