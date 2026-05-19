// swift-tools-version:5.9
import PackageDescription

// The renderer is native: SwiftUI + an `NSView` subclass that
// draws via Core Graphics, fed by an in-process Rust crate via
// a static-library FFI bridge. No WKWebView, no bundled HTML.
//
// Pre-build flow (run from the repo root):
//
//   1. `cargo build --release -p apfs-fastindex` produces
//      `target/release/libapfs_fastindex.a` and the
//      cbindgen-generated header.
//   2. `build-native.sh` copies both into
//      `Sources/CApfsFastindex/`.
//   3. `swift build --package-path app` picks them up via the
//      `.systemLibrary` shim below and statically links the
//      Rust code into the executable.
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
