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
    // Authorization Services, it passes `--server` as the single
    // argument. The CLI then reads JSON-line commands from stdin.
    // Reusing the same privileged process across subsequent scans
    // means the auth dialog pops once per session, not once per scan.
    //
    // Protocol version 2 uses one JSON object per line. The parent
    // sends `scan`, `release`, and `quit` commands; the helper emits
    // `ready`, `progress`, `ok`, `err`, and `bye` events.
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

#[allow(clippy::too_many_arguments)] // Existing CLI argument boundary.
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
    let cache_enabled = cache_enabled && cache_options_supported(cross_mounts, slim, summary_only);
    let mut cache_state = if cache_enabled {
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
    if let CacheState::Hit {
        ref scan_blob_path,
        ref manifest,
        ..
    } = cache_state
    {
        if let Some(exit) = cache_emit_msgpack(scan_blob_path, manifest, format) {
            return exit;
        }
        // Format wasn't msgpack-stream-compatible, or the blob
        // failed to read. Fall through to the fresh walk.
    }
    // A blob can be replaced after lookup by another writer. If the
    // actual emission bytes fail validation, retain the probe so the
    // fresh scan also repairs the cache rather than repeatedly missing.
    cache_state = match cache_state {
        CacheState::Hit { probe, .. } => CacheState::Miss(Some(probe)),
        state => state,
    };

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
            // Failed per-entry reads must be retried on the next request.
            // A metadata signature cannot certify that a partial scan is complete.
            if cache_enabled
                && exit == ExitCode::SUCCESS
                && output.parser_output.walk_skips.is_empty()
            {
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

fn cache_options_supported(cross_mounts: bool, slim: bool, summary_only: bool) -> bool {
    // The validator uses default mount boundaries, and persisted envelopes
    // contain full entries/aggregates rather than slim or summary schemas.
    !cross_mounts && !slim && !summary_only
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
        probe: (apfs_fastindex::cache::CacheKey, u64),
    },
}

/// Compute the cache identity, probe the directory signature,
/// and look up the cache. Returns the resulting state.
///
/// **Known cost.** The probe (metadata walk of the scan
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
/// Directory-only signatures cannot validate file resizes. Faster reuse
/// needs a journal-backed invalidation design, not speculative directory
/// timestamp checks.
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
            return Ok(CacheState::Hit {
                scan_blob_path: blob,
                manifest,
                probe: (key, sig),
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
    format: OutputFormat,
) -> Option<ExitCode> {
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
    if !manifest.matches_scan_msgpack(&bytes) {
        eprintln!(
            "apfs-fastindex-scan: cache: integrity mismatch for {scan_blob_path:?}; refreshing"
        );
        return None;
    }
    eprintln!(
        "apfs-fastindex-scan: cache: hit ({} entries, {} dir aggregates, msgpack {} bytes)",
        manifest.entry_count, manifest.aggregate_count, manifest.scan_msgpack_bytes
    );
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
        if emitted.is_multiple_of(4096) {
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

/// Long-lived privileged-helper server using a JSON-line protocol.
/// Each reply is flushed immediately so the GUI observes progress
/// and terminal events in real time.
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
/// - **H2**: the helper writes through the original mkstemp
///   descriptor, then transfers the completed mode-0600 file
///   to the GUI's UID. The parent verifies ownership, type,
///   link count, and mode before reading.
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum HelperCommand {
    /// Run one scan. Helper picks the output + progress
    /// paths and reports them back. `owner_uid` is the GUI's
    /// effective UID; the helper transfers the completed 0600
    /// output file to it so the unprivileged GUI can read it.
    Scan {
        path: String,
        owner_uid: u32,
        /// Missing or zero preserves the default for older clients.
        threads: Option<usize>,
    },
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
    /// Scan succeeded. `out_path` is helper-chosen and mode
    /// 0600, owned by the requesting GUI.
    Ok { out_path: &'a str, exit_code: i32 },
    /// Scan failed; message goes to the parent's status UI.
    Err { message: String },
    /// Quit acknowledged. Helper exits immediately after
    /// sending this.
    Bye,
}

fn write_reply<W: std::io::Write>(out: &mut W, reply: &HelperReply<'_>) -> std::io::Result<()> {
    // One JSON object per line. The helper writes to the
    // parent's socketpair (via AuthorizationExecuteWithPrivileges);
    // the framing is newline-delimited to keep the parent's
    // line-reader straightforward.
    serde_json::to_writer(&mut *out as &mut dyn std::io::Write, reply)
        .map_err(std::io::Error::other)?;
    out.write_all(b"\n")?;
    out.flush()
}

fn run_server_mode() -> ExitCode {
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

    // Track every tempfile we hand out. On quit or a matching
    // `release` command we unlink it. The list is bounded by the
    // helper lifetime, normally the GUI process lifetime.
    let mut owned_paths: Vec<std::path::PathBuf> = Vec::new();
    let result = run_server_loop(&mut owned_paths);
    // One cleanup path also covers startup/progress/terminal reply
    // failures. A broken connection cannot receive another command.
    cleanup_owned_paths(&owned_paths);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: I/O error: {err}"
            );
            ExitCode::from(1)
        }
    }
}

fn run_server_loop(owned_paths: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    use std::io::{BufReader, Write};
    let default_threads = default_fallback_threads();

    {
        let mut out = std::io::stdout().lock();
        write_reply(&mut out, &HelperReply::Ready { version: 2 })?;
    }

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        match read_helper_line(&mut reader, &mut line) {
            Ok(0) => {
                return Ok(());
            }
            Ok(_) => {}
            Err(err) => return Err(err),
        };
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
                )?;
                continue;
            }
        };
        match cmd {
            HelperCommand::Scan {
                path,
                owner_uid,
                threads,
            } => {
                // JSON framing preserves tabs and newlines in paths.
                // NUL cannot occur in a filesystem path.
                if path.contains('\0') {
                    let mut out = std::io::stdout().lock();
                    write_reply(
                        &mut out,
                        &HelperReply::Err {
                            message: "path contains a NUL byte".to_string(),
                        },
                    )?;
                    continue;
                }
                let threads = threads
                    .filter(|&count| count > 0)
                    .unwrap_or(default_threads);
                match run_server_scan(&path, threads, owner_uid) {
                    Ok((out_path, exit_code)) => {
                        owned_paths.push(out_path.clone());
                        let mut out = std::io::stdout().lock();
                        write_reply(
                            &mut out,
                            &HelperReply::Ok {
                                out_path: out_path.to_string_lossy().as_ref(),
                                exit_code,
                            },
                        )?;
                    }
                    Err(message) => {
                        let mut out = std::io::stdout().lock();
                        write_reply(&mut out, &HelperReply::Err { message })?;
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
                        if remove_owned_path(&pb) {
                            owned_paths.retain(|own| own != &pb);
                        }
                    } else {
                        let _ = writeln!(
                            std::io::stderr().lock(),
                            "apfs-fastindex-scan: server: release refused for non-owned path {p:?}"
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
                )?;
            }
            HelperCommand::Quit => {
                let mut out = std::io::stdout().lock();
                write_reply(&mut out, &HelperReply::Bye)?;
                return Ok(());
            }
            HelperCommand::Unknown => {
                let mut out = std::io::stdout().lock();
                write_reply(
                    &mut out,
                    &HelperReply::Err {
                        message: "unknown command".to_string(),
                    },
                )?;
            }
        }
    }
}

// This is a private GUI protocol. A bounded frame prevents malformed
// input from growing a privileged helper's command buffer without limit.
const MAX_HELPER_COMMAND_BYTES: usize = 1024 * 1024;

fn read_helper_line<R: std::io::BufRead>(
    reader: &mut R,
    line: &mut String,
) -> std::io::Result<usize> {
    use std::io::{BufRead, Read};
    let count = reader
        .take((MAX_HELPER_COMMAND_BYTES + 1) as u64)
        .read_line(line)?;
    if line.len() > MAX_HELPER_COMMAND_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "helper command exceeds the maximum frame size",
        ));
    }
    Ok(count)
}

fn cleanup_owned_paths(paths: &[std::path::PathBuf]) {
    for p in paths {
        remove_owned_path(p);
    }
}

fn remove_owned_path(path: &std::path::Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: remove {path:?} failed: {err}"
            );
            false
        }
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
    let _ = thread::Builder::new()
        .name("helper-parent-watch".into())
        .spawn(move || unsafe {
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
            loop {
                let count = libc::kevent(kq, std::ptr::null(), 0, &mut out, 1, std::ptr::null());
                if count < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                if count != 1 || !parent_exit_event(&out, parent_pid) {
                    libc::close(kq);
                    return;
                }
                break;
            }
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

#[cfg(target_os = "macos")]
fn parent_exit_event(event: &libc::kevent, parent_pid: i32) -> bool {
    event.ident == parent_pid as libc::uintptr_t
        && event.filter == libc::EVFILT_PROC
        && event.flags & libc::EV_ERROR == 0
        && event.fflags & libc::NOTE_EXIT != 0
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
/// verifies the path's ownership and mode before reading.
fn run_server_scan(
    path: &str,
    threads: usize,
    owner_uid: u32,
) -> Result<(std::path::PathBuf, i32), String> {
    use std::os::fd::AsRawFd;

    // uid_t's all-ones value means "leave ownership unchanged" to
    // fchown, rather than a UID to which the result can be transferred.
    if owner_uid == u32::MAX {
        return Err("invalid output owner UID".to_string());
    }

    // mkstemp-style: prefix + suffix in /tmp, mode 0600,
    // O_EXCL. `NamedTempFile` owns the file's lifetime; we
    // call `keep()` to detach so the path survives past the
    // function return and is unlinked by the protocol's
    // `release` / `quit` cleanup.
    let mut tmp_out = match tempfile::Builder::new()
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
    let mut progress_error = None;
    let mut progress_writer = |event: ProgressEvent| {
        if progress_error.is_some() {
            return;
        }
        let mut out = std::io::stdout().lock();
        let reply = HelperReply::Progress {
            scanned: event.scanned,
            skipped: event.skipped,
            bytes: event.bytes,
            elapsed_ms: event.elapsed.as_millis(),
            terminal: event.terminal,
        };
        if let Err(err) = write_reply(&mut out, &reply) {
            progress_error = Some(err);
        }
    };
    let options = FallbackOptions {
        cross_mounts: false,
        progress: Some(&mut progress_writer as &mut (dyn FnMut(ProgressEvent) + Send)),
        threads,
        // Only this private GUI transport omits legacy aggregates.
        // The GUI builds its tree from entries, just like direct FFI
        // scans. Public CLI output keeps its aggregate contract.
        skip_aggregates: true,
    };

    let scan = match fallback_scan_path_with_options(path, options) {
        Ok(s) => s,
        Err(err) => {
            return Err(format!("scan {path} failed: {err}"));
        }
    };
    if let Some(err) = progress_error {
        return Err(format!("write progress reply failed: {err}"));
    }
    // Write through NamedTempFile's original mkstemp descriptor.
    // Re-opening the pathname here would let a watcher replace
    // the directory entry before a root process opened it.
    write_helper_scan_output(tmp_out.as_file_mut(), &scan)?;

    // The helper runs as root in production, while the GUI that
    // consumes this file does not. Keep the mkstemp permissions
    // at 0600 and transfer the completed file to the requesting
    // GUI rather than weakening it to group/world-readable.
    #[cfg(unix)]
    let current_uid = {
        use std::os::unix::fs::MetadataExt;
        tmp_out
            .as_file()
            .metadata()
            .map_err(|err| format!("stat output file failed: {err}"))?
            .uid()
    };
    #[cfg(not(unix))]
    let current_uid = owner_uid;
    if current_uid != owner_uid {
        let rc = unsafe {
            libc::fchown(
                tmp_out.as_file().as_raw_fd(),
                owner_uid as libc::uid_t,
                libc::gid_t::MAX,
            )
        };
        if rc != 0 {
            return Err(format!(
                "transfer output file to uid {owner_uid} failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = tmp_out
            .as_file()
            .metadata()
            .map_err(|err| format!("verify output file failed: {err}"))?;
        if !metadata.is_file()
            || metadata.uid() != owner_uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(
                "output file failed ownership, type, link count, or mode check".to_string(),
            );
        }
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

fn write_helper_scan_output<W: std::io::Write>(
    file: W,
    scan: &FallbackScanOutput,
) -> Result<(), String> {
    // Borrow the original mkstemp descriptor. Never reopen the output
    // pathname, and flush before ownership transfer or success reply.
    let mut writer = std::io::BufWriter::with_capacity(1024 * 1024, file);
    rmp_serde::encode::write_named(&mut writer, scan)
        .map_err(|err| format!("msgpack serialise/write failed: {err}"))?;
    writer
        .flush()
        .map_err(|err| format!("flush output file failed: {err}"))
}

#[cfg(test)]
mod helper_protocol_tests {
    use super::*;
    use std::io::{self, Cursor, Write};

    struct FailingWriter {
        fail_write: bool,
        fail_flush: bool,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "synthetic write failure",
                ))
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "synthetic flush failure",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn replies_preserve_json_framing_and_report_write_or_flush_failures() {
        let mut bytes = Vec::new();
        write_reply(
            &mut bytes,
            &HelperReply::Err {
                message: "tab\tnewline\n".into(),
            },
        )
        .unwrap();
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
        let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reply["message"], "tab\tnewline\n");
        for writer in [
            FailingWriter {
                fail_write: true,
                fail_flush: false,
            },
            FailingWriter {
                fail_write: false,
                fail_flush: true,
            },
        ] {
            assert!(write_reply(&mut { writer }, &HelperReply::Bye).is_err());
        }
    }

    #[test]
    fn helper_frames_are_bounded_and_do_not_consume_the_next_command() {
        let mut reader = Cursor::new(b"{\"op\":\"quit\"}\n{\"op\":\"quit\"}\n");
        let mut line = String::new();
        assert!(read_helper_line(&mut reader, &mut line).unwrap() > 0);
        assert_eq!(line, "{\"op\":\"quit\"}\n");
        line.clear();
        assert!(read_helper_line(&mut reader, &mut line).unwrap() > 0);
        line.clear();
        assert_eq!(read_helper_line(&mut reader, &mut line).unwrap(), 0);
        let mut reader = Cursor::new(vec![b'x'; MAX_HELPER_COMMAND_BYTES + 20]);
        assert_eq!(
            read_helper_line(&mut reader, &mut line).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(line.len(), MAX_HELPER_COMMAND_BYTES + 1);
    }

    #[test]
    fn buffered_helper_serialization_is_byte_exact_and_flush_errors_are_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), b"content").unwrap();
        let output = fallback_scan_path_with_options(
            dir.path(),
            FallbackOptions {
                skip_aggregates: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut bytes = Vec::new();
        write_helper_scan_output(&mut bytes, &output).unwrap();
        assert_eq!(bytes, rmp_serde::to_vec_named(&output).unwrap());
        for writer in [
            FailingWriter {
                fail_write: true,
                fail_flush: false,
            },
            FailingWriter {
                fail_write: false,
                fail_flush: true,
            },
        ] {
            assert!(write_helper_scan_output(writer, &output).is_err());
        }
    }

    #[test]
    fn helper_serialization_keeps_using_the_original_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let path = file.path().to_owned();
        let moved = dir.path().join("original-descriptor");
        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"replacement must remain unchanged").unwrap();
        let scan_root = tempfile::tempdir().unwrap();
        let output = fallback_scan_path_with_options(
            scan_root.path(),
            FallbackOptions {
                skip_aggregates: true,
                ..Default::default()
            },
        )
        .unwrap();
        write_helper_scan_output(file.as_file_mut(), &output).unwrap();
        assert_eq!(
            std::fs::read(&moved).unwrap(),
            rmp_serde::to_vec_named(&output).unwrap()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"replacement must remain unchanged"
        );
    }

    #[test]
    fn private_helper_omits_aggregates_and_preserves_every_other_field() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/α\t\n"), b"content").unwrap();
        std::fs::hard_link(dir.path().join("sub/α\t\n"), dir.path().join("alias")).unwrap();
        symlink("sub/α\t\n", dir.path().join("link")).unwrap();
        let expected = fallback_scan_path_with_options(
            dir.path(),
            FallbackOptions {
                skip_aggregates: true,
                threads: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let (path, exit) =
            run_server_scan(dir.path().to_str().unwrap(), 1, unsafe { libc::geteuid() }).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let actual: FallbackScanOutput = rmp_serde::from_slice(&bytes).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(remove_owned_path(&path));
        assert_eq!(exit, 0);
        assert_eq!(actual, expected);
        assert!(actual.parser_output.aggregates.is_empty());
        assert!(metadata.is_file());
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.mode() & 0o777, 0o600);
        let public = apfs_fastindex::fallback_scan_path(dir.path()).unwrap();
        assert!(!public.parser_output.aggregates.is_empty());
    }

    #[test]
    fn helper_refuses_the_fchown_no_change_uid_sentinel() {
        let err = run_server_scan("not-a-directory", 1, u32::MAX).unwrap_err();
        assert_eq!(err, "invalid output owner UID");
    }

    #[test]
    fn failed_unlink_remains_retryable() {
        let dir = tempfile::tempdir().unwrap();
        // remove_file must not remove directories or forget the failed
        // release, so shutdown can retry it. The test never elevates.
        assert!(!remove_owned_path(dir.path()));
        assert!(dir.path().is_dir());
        assert!(remove_owned_path(&dir.path().join("already-gone")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_successful_parent_exit_event_triggers_shutdown() {
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        event.ident = 123;
        event.filter = libc::EVFILT_PROC;
        event.fflags = libc::NOTE_EXIT;
        assert!(parent_exit_event(&event, 123));
        assert!(!parent_exit_event(&event, 124));
        event.flags = libc::EV_ERROR;
        assert!(!parent_exit_event(&event, 123));
        event.flags = 0;
        event.fflags = 0;
        assert!(!parent_exit_event(&event, 123));
        event.fflags = libc::NOTE_EXIT;
        event.filter = libc::EVFILT_READ;
        assert!(!parent_exit_event(&event, 123));
    }

    #[test]
    fn scan_command_preserves_worker_preferences_and_legacy_default() {
        for threads in [None, Some(0), Some(1), Some(2)] {
            let mut command = serde_json::json!({
                "op": "scan", "path": "/tmp", "owner_uid": 501,
            });
            if let Some(count) = threads {
                command["threads"] = serde_json::json!(count);
            }
            let parsed: HelperCommand = serde_json::from_value(command).unwrap();
            match parsed {
                HelperCommand::Scan {
                    threads: actual, ..
                } => assert_eq!(actual, threads),
                _ => panic!("expected a scan command"),
            }
        }
    }

    #[test]
    fn scan_command_rejects_negative_worker_count() {
        let command = serde_json::json!({
            "op": "scan", "path": "/tmp", "owner_uid": 501, "threads": -1,
        });
        assert!(serde_json::from_value::<HelperCommand>(command).is_err());
    }
}
#[test]
fn cache_does_not_reuse_incompatible_mount_or_output_options() {
    assert!(cache_options_supported(false, false, false));
    assert!(!cache_options_supported(true, false, false));
    assert!(!cache_options_supported(false, true, false));
    assert!(!cache_options_supported(false, false, true));
}
