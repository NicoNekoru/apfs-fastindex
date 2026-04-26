use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args();
    let program = args
        .next()
        .unwrap_or_else(|| "apfs-fastindex-scan".to_string());
    let Some(path) = args.next() else {
        eprintln!("usage: {program} <raw-apfs-container-or-detached-image>");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("usage: {program} <raw-apfs-container-or-detached-image>");
        return ExitCode::from(2);
    }

    match apfs_fastindex::checkpoint_scan_source(&path) {
        Ok(output) => match serde_json::to_string_pretty(&output) {
            Ok(document) => {
                println!("{document}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("apfs-fastindex-scan: failed to serialize scan output: {err}");
                ExitCode::from(1)
            }
        },
        Err(err) => {
            eprintln!("apfs-fastindex-scan: {err}");
            ExitCode::from(1)
        }
    }
}
