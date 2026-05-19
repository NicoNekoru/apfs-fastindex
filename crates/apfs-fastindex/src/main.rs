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

fn emit_output<T: serde::Serialize>(
    value: &T,
    pretty: bool,
    format: OutputFormat,
) -> ExitCode {
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
