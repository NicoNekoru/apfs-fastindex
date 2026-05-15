use std::env;
use std::process::ExitCode;

const USAGE: &str = "usage: apfs-fastindex-scan [--summary] <raw-apfs-container-or-detached-image>";

fn main() -> ExitCode {
    let mut args = env::args();
    let _program = args
        .next()
        .unwrap_or_else(|| "apfs-fastindex-scan".to_string());

    let mut summary_only = false;
    let mut source_path: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            "--summary" => {
                if summary_only {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
                summary_only = true;
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
    let Some(path) = source_path else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    match apfs_fastindex::checkpoint_scan_source(&path) {
        Ok(output) => {
            if summary_only {
                println!("correctness_claim: {}", output.correctness_claim);
                println!("entries: {}", output.parser_output.entries.len());
                println!("aggregates: {}", output.parser_output.aggregates.len());
                println!("not_claimed:");
                for item in &output.not_claimed {
                    println!("  - {item}");
                }
                return ExitCode::SUCCESS;
            }
            match serde_json::to_string_pretty(&output) {
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
        Err(err) => {
            eprintln!("apfs-fastindex-scan: {err}");
            ExitCode::from(1)
        }
    }
}
