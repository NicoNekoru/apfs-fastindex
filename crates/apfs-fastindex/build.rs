//! Build script for `apfs-fastindex`.
//!
//! Runs cbindgen at every `cargo build` so the C header Swift
//! imports stays in lockstep with the Rust `#[no_mangle] extern
//! "C"` surface in `src/ffi.rs`. The header is written to
//! `target/<profile>/include/apfs_fastindex.h` — predictable
//! path the SwiftPM build script copies into the package's
//! private headers directory.
//!
//! Re-runs only when the FFI source or cbindgen config changes,
//! so iterative builds against the non-FFI parts of the crate
//! don't pay the cbindgen cost.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let crate_dir =
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    // Workspace `target/` is two levels up from this crate's
    // manifest; the SwiftPM build script knows the same path
    // shape and reads the header from there.
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let workspace_target = PathBuf::from(&crate_dir)
        .parent()
        .expect("crate dir has parent")
        .parent()
        .expect("workspace has root")
        .join("target");
    let include_dir = workspace_target.join(&profile).join("include");
    let _ = std::fs::create_dir_all(&include_dir);
    let header_path = include_dir.join("apfs_fastindex.h");

    let config = cbindgen::Config::from_file(PathBuf::from(&crate_dir).join("cbindgen.toml"))
        .expect("cbindgen.toml should parse");
    // Fail closed on cbindgen errors. The previous behaviour
    // logged a `cargo:warning=` and continued — silent ABI
    // drift between the Rust `#[no_mangle]` surface and the
    // header Swift consumes was the audit's #7 concern. Now any
    // cbindgen failure (parse error, unsupported type, broken
    // config) panics the build script and stops cargo cold.
    let bindings = cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
        .unwrap_or_else(|err| {
            panic!("apfs-fastindex: cbindgen failed: {err}");
        });
    bindings.write_to_file(&header_path);
    println!(
        "cargo:warning=apfs-fastindex: header written to {}",
        header_path.display()
    );
}
