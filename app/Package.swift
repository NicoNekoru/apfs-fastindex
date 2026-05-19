// swift-tools-version:5.9
import PackageDescription

// The native renderer links a static Rust library
// (libapfs_fastindex.a) via a system-library shim target. The
// pre-build flow is:
//
//   1. `cargo build --release -p apfs-fastindex` (in the repo
//      root) produces `target/release/libapfs_fastindex.a` and
//      the cbindgen-generated header.
//   2. A helper script (`build-native.sh` at the repo root) copies
//      both into `Sources/CApfsFastindex/`.
//   3. `swift build --package-path app` picks them up via the
//      `.systemLibrary` shim below.
//
// Static linkage means the executable contains the full crate's
// code (~17 MB today); no dylib loading, no rpath, no DYLD_*
// env vars at run time. Future packaging tweaks can swap to
// `.dylib` if size matters; the FFI surface is the same.
let package = Package(
    name: "ApfsFastindex",
    platforms: [
        .macOS(.v13)
    ],
    targets: [
        .systemLibrary(
            name: "CApfsFastindex",
            path: "Sources/CApfsFastindex"
        ),
        .executableTarget(
            name: "ApfsFastindex",
            dependencies: ["CApfsFastindex"],
            path: "Sources/ApfsFastindex",
            resources: [
                .copy("Resources/viz")
            ],
            linkerSettings: [
                // Tells the linker where to find
                // `libapfs_fastindex.a` (named by the modulemap's
                // `link "apfs_fastindex"` directive). Relative to
                // the package root (`app/`).
                .unsafeFlags([
                    "-L", "Sources/CApfsFastindex",
                ])
            ]
        )
    ]
)
