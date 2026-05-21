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
) -> ExitCode {
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
            emit_fallback_output(output, pretty, slim, format)
        }
        Err(err) => {
            eprintln!("apfs-fastindex-scan: fallback: {err}");
            ExitCode::from(1)
        }
    }
}

fn emit_fallback_output(
    output: FallbackScanOutput,
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
        return emit_msgpack_stream(&output, slim);
    }
    let envelope = if slim {
        slim_fallback_envelope(&output)
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
    };
    emit_output(&envelope, pretty, format)
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
fn run_server_mode() -> ExitCode {
    use std::io::{BufRead, BufReader, Write};

    let threads = default_fallback_threads();

    // Handshake: tell the parent the helper is up and reading
    // stdin. The parent uses this to flip `adminMode` (title-bar
    // update) the moment auth completes, before the first scan
    // even starts.
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "ready\t1");
        let _ = out.flush();
    }

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(0) => return ExitCode::SUCCESS, // EOF: parent closed stdin.
            Ok(n) => n,
            Err(err) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "apfs-fastindex-scan: server: stdin read error: {err}"
                );
                return ExitCode::from(1);
            }
        };
        let _ = n; // silence unused-must-use; n is read above.
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('\t').collect();
        let mut stdout = std::io::stdout().lock();
        match parts.as_slice() {
            ["scan", path, out_msgpack, progress_log] => {
                let exit = run_server_scan(
                    path,
                    out_msgpack,
                    progress_log,
                    threads,
                );
                let _ = writeln!(stdout, "ok\t{exit}");
                let _ = stdout.flush();
            }
            ["scan-with-snapshots", path, out_msgpack, progress_log] => {
                let exit = run_server_scan_with_snapshots(
                    path,
                    out_msgpack,
                    progress_log,
                    threads,
                );
                let _ = writeln!(stdout, "ok\t{exit}");
                let _ = stdout.flush();
            }
            ["quit"] => {
                let _ = writeln!(stdout, "ok\t0");
                let _ = stdout.flush();
                return ExitCode::SUCCESS;
            }
            other => {
                let _ = writeln!(
                    stdout,
                    "err\tunknown command: {}",
                    other.join("\\t")
                );
                let _ = stdout.flush();
            }
        }
    }
}

/// Run one scan from the server loop. Writes msgpack to
/// `out_msgpack` and one JSON progress line per ~250 ms to
/// `progress_log`. Returns the process-equivalent exit code
/// (0 = success, non-zero = scan-side failure).
fn run_server_scan(
    path: &str,
    out_msgpack: &str,
    progress_log: &str,
    threads: usize,
) -> i32 {
    use std::fs::File;
    use std::io::Write;

    let mut progress_file = match File::create(progress_log) {
        Ok(f) => f,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: cannot open progress log {progress_log}: {err}"
            );
            return 1;
        }
    };
    let mut progress_writer = |event: ProgressEvent| {
        let _ = writeln!(
            progress_file,
            "{{\"scanned\":{},\"skipped\":{},\"bytes\":{},\"elapsed_ms\":{},\"terminal\":{}}}",
            event.scanned,
            event.skipped,
            event.bytes,
            event.elapsed.as_millis(),
            event.terminal
        );
        let _ = progress_file.flush();
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
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: scan {path} failed: {err}"
            );
            return 1;
        }
    };

    let bytes = match rmp_serde::to_vec_named(&scan) {
        Ok(b) => b,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: serialise {out_msgpack} failed: {err}"
            );
            return 1;
        }
    };
    if let Err(err) = std::fs::write(out_msgpack, &bytes) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "apfs-fastindex-scan: server: write {out_msgpack} failed: {err}"
        );
        return 1;
    }
    0
}

/// Live scan + every user-visible TM local snapshot of the volume
/// the scan target lives on. The snapshots are mounted under
/// temporary directories, walked, their entries are prefixed with
/// `__snapshots__/<snap-name>/` so they fold into one tree, and the
/// mounts are torn down before this function returns regardless of
/// outcome.
///
/// Requires root (mount_apfs is privileged). Invoked from the
/// long-lived AdminSession helper, so the calling process already
/// has EUID 0.
fn run_server_scan_with_snapshots(
    path: &str,
    out_msgpack: &str,
    progress_log: &str,
    threads: usize,
) -> i32 {
    use std::fs::File;
    use std::io::Write;
    use std::path::PathBuf;

    let mut progress_file = match File::create(progress_log) {
        Ok(f) => f,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: cannot open progress log {progress_log}: {err}"
            );
            return 1;
        }
    };
    let mut emit_progress = |event: ProgressEvent| {
        let _ = writeln!(
            progress_file,
            "{{\"scanned\":{},\"skipped\":{},\"bytes\":{},\"elapsed_ms\":{},\"terminal\":{}}}",
            event.scanned,
            event.skipped,
            event.bytes,
            event.elapsed.as_millis(),
            event.terminal
        );
        let _ = progress_file.flush();
    };

    // 1. Resolve the volume the user's path lives on. We need the
    //    device path so mount_apfs has a `device` arg; the mount
    //    point so we can translate user-path → snapshot-relative
    //    path.
    let (live_mount_on, live_device) = match statfs_info(path) {
        Ok(pair) => pair,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: statfs({path}) failed: {err}"
            );
            return 1;
        }
    };

    // 2. Enumerate user-visible TM local snapshots of that volume.
    //    The mount-point form (`/`, `/System/Volumes/Data`, etc.)
    //    is the canonical input to `tmutil listlocalsnapshots`.
    let snap_query_mount = if live_mount_on == "/" {
        "/".to_string()
    } else {
        live_mount_on.clone()
    };
    let snapshots = match apfs_fastindex::snapshots::list_tmutil_snapshots(
        std::path::Path::new(&snap_query_mount),
    ) {
        Ok(entries) => entries
            .into_iter()
            .filter(|e| e.user_visible)
            .collect::<Vec<_>>(),
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: snapshot enumeration failed: {err}"
            );
            Vec::new()
        }
    };

    // 3. Mount each snapshot at a fresh temp dir. Track for
    //    teardown regardless of any subsequent failure.
    let scratch_root: PathBuf = std::env::temp_dir()
        .join(format!("apfs-fastindex-snap-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch_root);
    let mut mounted: Vec<(String, PathBuf)> = Vec::new();
    for snap in &snapshots {
        let mount_dir = scratch_root.join(format!("snap-{}", mounted.len()));
        if let Err(err) = std::fs::create_dir_all(&mount_dir) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: mkdir snapshot mount {mount_dir:?} failed: {err}"
            );
            continue;
        }
        match mount_snapshot(&snap.name, &live_device, &mount_dir) {
            Ok(()) => mounted.push((snap.name.clone(), mount_dir)),
            Err(err) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "apfs-fastindex-scan: server: mount_apfs -s {} on {} -> {mount_dir:?} \
                     failed: {err}",
                    snap.name,
                    live_device,
                );
                let _ = std::fs::remove_dir_all(&mount_dir);
            }
        }
    }

    // 4. Compute the per-snapshot path: relative to the live mount,
    //    rebased onto the snapshot mount point. Handles the firmlink
    //    case (path doesn't textually start with mnt_on; assume it
    //    sits at the volume root).
    let path_rel = path_relative_to_mount(path, &live_mount_on);

    // 5. Walk live + every successfully-mounted snapshot, merging
    //    entries.
    let live_options = FallbackOptions {
        cross_mounts: false,
        progress: Some(&mut emit_progress as &mut (dyn FnMut(ProgressEvent) + Send)),
        threads,
        skip_aggregates: true,
    };
    let live = match fallback_scan_path_with_options(path, live_options) {
        Ok(s) => s,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: live scan {path} failed: {err}"
            );
            cleanup_mounts(&mounted);
            let _ = std::fs::remove_dir_all(&scratch_root);
            return 1;
        }
    };

    let mut merged = live;
    for (snap_name, mount_dir) in &mounted {
        let snap_path = mount_dir.join(&path_rel);
        if !snap_path.exists() {
            // Snapshot might not contain the user's path (e.g.,
            // the subdirectory was added after the snapshot).
            // Skip silently — the user sees the live result + the
            // other snapshots.
            continue;
        }
        let snap_options = FallbackOptions {
            cross_mounts: false,
            progress: None,
            threads,
            skip_aggregates: true,
        };
        let snap = match fallback_scan_path_with_options(&snap_path, snap_options) {
            Ok(s) => s,
            Err(err) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "apfs-fastindex-scan: server: snapshot walk {snap_path:?} failed: {err}"
                );
                continue;
            }
        };
        merge_snapshot_into(&mut merged, snap, snap_name);
    }
    // Rebuild aggregates from the merged entries so per-directory
    // totals include snapshot subtrees.
    merged.parser_output.aggregates = apfs_fastindex::build_directory_aggregates(
        &merged.parser_output.entries,
    );

    let bytes = match rmp_serde::to_vec_named(&merged) {
        Ok(b) => b,
        Err(err) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "apfs-fastindex-scan: server: serialise merged scan failed: {err}"
            );
            cleanup_mounts(&mounted);
            let _ = std::fs::remove_dir_all(&scratch_root);
            return 1;
        }
    };
    if let Err(err) = std::fs::write(out_msgpack, &bytes) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "apfs-fastindex-scan: server: write {out_msgpack} failed: {err}"
        );
        cleanup_mounts(&mounted);
        let _ = std::fs::remove_dir_all(&scratch_root);
        return 1;
    }

    cleanup_mounts(&mounted);
    let _ = std::fs::remove_dir_all(&scratch_root);
    0
}

/// `statfs(path)` -> `(mnt_on, mnt_from)`. Errors propagate as
/// io::Error.
fn statfs_info(path: &str) -> std::io::Result<(String, String)> {
    use std::ffi::CString;
    let c_path = CString::new(path).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains an interior NUL",
        )
    })?;
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut buf as *mut libc::statfs) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mnt_on = unsafe {
        std::ffi::CStr::from_ptr(buf.f_mntonname.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    let mnt_from = unsafe {
        std::ffi::CStr::from_ptr(buf.f_mntfromname.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    Ok((mnt_on, mnt_from))
}

/// Best-effort translation: user-supplied path → path relative to
/// the volume's mount root. Handles two cases:
///
/// 1. Path is textually under `mnt_on`. Just strip the prefix.
/// 2. Path is firmlinked into the volume (e.g. `/Users/kai`
///    statfs's onto `/System/Volumes/Data`). Strip the leading
///    `/` and assume the path lives at the volume root — true for
///    every standard macOS firmlink (`/Users`, `/Library`,
///    `/private/var/...`).
///
/// Returns a `PathBuf` containing the relative path with no
/// leading slash.
fn path_relative_to_mount(path: &str, mnt_on: &str) -> std::path::PathBuf {
    let trimmed_mnt = mnt_on.trim_end_matches('/');
    if !trimmed_mnt.is_empty() {
        if let Some(rest) = path.strip_prefix(trimmed_mnt) {
            let cleaned = rest.trim_start_matches('/');
            return std::path::PathBuf::from(cleaned);
        }
    }
    // Fallback: firmlinked path. Strip leading `/`.
    std::path::PathBuf::from(path.trim_start_matches('/'))
}

/// `mount_apfs -s <snap-name> <device> <mount-point>`. Returns
/// `Ok(())` on success, `Err(message)` on failure.
fn mount_snapshot(
    snap_name: &str,
    device: &str,
    mount_point: &std::path::Path,
) -> Result<(), String> {
    let status = std::process::Command::new("/sbin/mount_apfs")
        .arg("-s")
        .arg(snap_name)
        .arg(device)
        .arg(mount_point)
        .status()
        .map_err(|e| format!("spawn failed: {e}"))?;
    if !status.success() {
        return Err(format!("mount_apfs exited {}", status));
    }
    Ok(())
}

/// `umount <mount-point>`. Best-effort; errors logged to stderr.
fn cleanup_mounts(mounted: &[(String, std::path::PathBuf)]) {
    use std::io::Write;
    for (_, mount_dir) in mounted {
        let status = std::process::Command::new("/sbin/umount")
            .arg(mount_dir)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "apfs-fastindex-scan: server: umount {mount_dir:?} exited {s}"
                );
            }
            Err(err) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "apfs-fastindex-scan: server: umount {mount_dir:?} spawn failed: {err}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(mount_dir);
    }
}

/// Append a snapshot's entries into the merged scan, prefixing
/// each path with `__snapshots__/<snap-name>/` so the merged tree
/// keeps the two subtrees separate. `correctness_claim` /
/// `not_claimed` from the snapshot scan are dropped — the merged
/// claim remains the live scan's claim because that's the scan
/// the user actually asked for.
fn merge_snapshot_into(
    merged: &mut FallbackScanOutput,
    snap: FallbackScanOutput,
    snap_name: &str,
) {
    use apfs_fastindex::{EntryKind, NamespaceEntry};

    // Add a synthetic `__snapshots__` parent Dir entry once per
    // merge group so the tree-list panel shows it as a navigable
    // subtree. Idempotent: if a previous merge already added it,
    // subsequent merges find it and skip.
    if !merged
        .parser_output
        .entries
        .iter()
        .any(|e| &*e.path == "__snapshots__")
    {
        merged.parser_output.entries.push(NamespaceEntry {
            path: "__snapshots__".into(),
            entry_kind: EntryKind::Dir,
            file_id: 0,
            logical_size: 0,
            symlink_target: None,
            allocated_size: Some(0),
            real_size: Some(0),
        });
    }

    let prefix = format!("__snapshots__/{snap_name}");
    merged
        .parser_output
        .entries
        .reserve(snap.parser_output.entries.len());
    for entry in snap.parser_output.entries.into_iter() {
        // Always include the snapshot's path under the synthetic
        // `__snapshots__/<name>/` directory. Empty-path entries
        // (the snapshot's root, if any) become the synthetic
        // directory itself.
        let new_path = if entry.path.is_empty() {
            prefix.clone()
        } else {
            format!("{prefix}/{}", entry.path)
        };
        merged.parser_output.entries.push(NamespaceEntry {
            path: new_path.into_boxed_str(),
            entry_kind: entry.entry_kind,
            file_id: entry.file_id,
            logical_size: entry.logical_size,
            symlink_target: entry.symlink_target,
            allocated_size: entry.allocated_size,
            real_size: entry.real_size,
        });
    }
    // Walk-skips from the snapshot side are not merged — they
    // would surface paths that no longer exist on the live FS
    // and would be confusing in the status-bar elide pill.
}

#[cfg(test)]
mod tests {
    use super::*;
    use apfs_fastindex::{
        EntryKind, NamespaceEntry, ParserOutput, ScanState, SourceDescriptor,
    };
    use std::path::PathBuf;

    fn entry(path: &str, logical: u64) -> NamespaceEntry {
        NamespaceEntry {
            path: path.into(),
            entry_kind: EntryKind::File,
            file_id: 0,
            logical_size: logical,
            symlink_target: None,
            allocated_size: Some(logical),
            real_size: Some(logical),
        }
    }

    fn empty_output(entries: Vec<NamespaceEntry>) -> FallbackScanOutput {
        FallbackScanOutput {
            parser_output: ParserOutput {
                source: SourceDescriptor {
                    requested_path: PathBuf::from("/"),
                    raw_container_path: "/".to_string(),
                    source_kind: "test".to_string(),
                    allowlist_reason: "test".to_string(),
                },
                scan_state: ScanState {
                    block_size: 4096,
                    descriptor_blocks: 0,
                    descriptor_base: 0,
                    descriptor_base_non_contiguous: false,
                    highest_xid: 0,
                    candidate_count: 0,
                    validation_gaps: vec![],
                },
                backend_name: "test".to_string(),
                entries,
                aggregates: vec![],
                walk_skips: vec![],
            },
            correctness_claim: String::new(),
            not_claimed: vec![],
        }
    }

    /// path_relative_to_mount: textual prefix case — path is
    /// physically under the mount root.
    #[test]
    fn path_relative_strips_textual_prefix() {
        let p = path_relative_to_mount("/System/Volumes/Data/Users/kai", "/System/Volumes/Data");
        assert_eq!(p, PathBuf::from("Users/kai"));
    }

    /// path_relative_to_mount: firmlink case — path doesn't
    /// textually match mnt_on but statfs reports the path lives
    /// on a different mount. Fall back to leading-/-strip.
    #[test]
    fn path_relative_handles_firmlinked_path() {
        // `/Users/kai` statfs's to `/System/Volumes/Data` on a
        // standard macOS install via firmlink. Textual strip
        // fails (no prefix); fallback strips leading `/`.
        let p = path_relative_to_mount("/Users/kai", "/System/Volumes/Data");
        assert_eq!(p, PathBuf::from("Users/kai"));
    }

    /// path_relative_to_mount: path on root mount.
    #[test]
    fn path_relative_strips_root_mount() {
        let p = path_relative_to_mount("/tmp/foo", "/");
        assert_eq!(p, PathBuf::from("tmp/foo"));
    }

    /// path_relative_to_mount: path equals mount root.
    #[test]
    fn path_relative_at_mount_root_is_empty() {
        let p = path_relative_to_mount("/System/Volumes/Data", "/System/Volumes/Data");
        assert_eq!(p, PathBuf::from(""));
    }

    /// merge_snapshot_into: snapshot entries get prefixed with
    /// `__snapshots__/<name>/` so the merged tree has them in a
    /// separate subtree from the live entries.
    #[test]
    fn merge_prefixes_snapshot_entries() {
        let live = vec![
            entry("Documents/foo.txt", 100),
            entry("Documents/bar.txt", 200),
        ];
        let snap = vec![
            entry("Documents/foo.txt", 100),
            entry("Documents/deleted.txt", 500),
        ];

        let mut merged = empty_output(live);
        merge_snapshot_into(
            &mut merged,
            empty_output(snap),
            "com.apple.TimeMachine.2026-05-20-100000.local",
        );

        let paths: Vec<&str> = merged
            .parser_output
            .entries
            .iter()
            .map(|e| &*e.path)
            .collect();
        assert!(paths.contains(&"Documents/foo.txt"));
        assert!(paths.contains(&"Documents/bar.txt"));
        assert!(paths.contains(
            &"__snapshots__/com.apple.TimeMachine.2026-05-20-100000.local/Documents/foo.txt"
        ));
        assert!(paths.contains(
            &"__snapshots__/com.apple.TimeMachine.2026-05-20-100000.local/Documents/deleted.txt"
        ));
        // Live (2) + snapshot (2) + synthetic `__snapshots__`
        // parent Dir (1) = 5.
        assert_eq!(merged.parser_output.entries.len(), 5);
        assert!(paths.contains(&"__snapshots__"));
    }

    /// merge_snapshot_into: entries from multiple snapshots stay
    /// in separate __snapshots__/<name>/ subtrees.
    #[test]
    fn merge_keeps_snapshots_separate() {
        let mut merged = empty_output(vec![entry("a.txt", 100)]);
        merge_snapshot_into(
            &mut merged,
            empty_output(vec![entry("a.txt", 100)]),
            "snap-1",
        );
        merge_snapshot_into(
            &mut merged,
            empty_output(vec![entry("a.txt", 100)]),
            "snap-2",
        );

        let paths: Vec<&str> = merged
            .parser_output
            .entries
            .iter()
            .map(|e| &*e.path)
            .collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&"__snapshots__/snap-1/a.txt"));
        assert!(paths.contains(&"__snapshots__/snap-2/a.txt"));
        // Live (1) + snap-1 (1) + snap-2 (1) + synthetic
        // `__snapshots__` Dir (1, added once across both merges) = 4.
        assert_eq!(merged.parser_output.entries.len(), 4);
    }

    /// build_directory_aggregates is exposed publicly via the
    /// crate root re-export; the merge flow re-aggregates after
    /// adding snapshot subtrees so per-directory totals include
    /// them. The synthetic `__snapshots__` Dir entry that
    /// merge_snapshot_into adds shows up in the aggregates.
    #[test]
    fn rebuild_aggregates_includes_snapshot_subtree() {
        fn dir(path: &str) -> NamespaceEntry {
            NamespaceEntry {
                path: path.into(),
                entry_kind: EntryKind::Dir,
                file_id: 0,
                logical_size: 0,
                symlink_target: None,
                allocated_size: Some(0),
                real_size: Some(0),
            }
        }
        // Live scan: typical fallback-walker output — Dir
        // entries for every directory, File entries beneath.
        let mut merged = empty_output(vec![
            dir("Documents"),
            entry("Documents/foo.txt", 100),
        ]);
        // Snapshot scan would have the same Dir-then-File
        // shape. After the merge they get prefixed with
        // __snapshots__/<name>/.
        merge_snapshot_into(
            &mut merged,
            empty_output(vec![dir("Documents"), entry("Documents/foo.txt", 100)]),
            "snap-1",
        );
        merged.parser_output.aggregates =
            apfs_fastindex::build_directory_aggregates(&merged.parser_output.entries);

        let agg_paths: Vec<&str> = merged
            .parser_output
            .aggregates
            .iter()
            .map(|a| a.path.as_str())
            .collect();
        // The synthetic snapshot subtree must produce aggregate
        // rows: the `__snapshots__` container, and the
        // `__snapshots__/snap-1/Documents` directory.
        assert!(
            agg_paths.contains(&"__snapshots__"),
            "expected `__snapshots__` aggregate; got {agg_paths:?}"
        );
        assert!(
            agg_paths
                .iter()
                .any(|p| p.starts_with("__snapshots__/snap-1")),
            "expected `__snapshots__/snap-1*` aggregate; got {agg_paths:?}"
        );
    }

    /// merge_snapshot_into is called once per snapshot; the
    /// synthetic `__snapshots__` parent Dir should only be added
    /// once (idempotent across repeated calls).
    #[test]
    fn merge_adds_snapshots_parent_dir_once() {
        let mut merged = empty_output(vec![entry("a.txt", 100)]);
        merge_snapshot_into(&mut merged, empty_output(vec![entry("a.txt", 100)]), "s1");
        merge_snapshot_into(&mut merged, empty_output(vec![entry("a.txt", 100)]), "s2");
        let count = merged
            .parser_output
            .entries
            .iter()
            .filter(|e| &*e.path == "__snapshots__")
            .count();
        assert_eq!(count, 1, "`__snapshots__` parent should appear once");
    }
}
