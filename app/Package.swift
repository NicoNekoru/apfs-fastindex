// swift-tools-version:5.9
import PackageDescription

// The renderer is native: SwiftUI + an `NSView` subclass that
// draws via Core Graphics, fed by an in-process Rust crate via
// a static-library FFI bridge. No WKWebView, no bundled HTML.
//
// Build flow: run `make-release.sh` from the repo root. It
// runs `cargo build` for the apfs-fastindex crate, copies
// `libapfs_fastindex.a` + the cbindgen-generated header into
// `Sources/CApfsFastindex/`, then invokes `swift build` so the
// `.systemLibrary` shim below picks them up and links the Rust
// code statically into the executable.
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
        // Pure-Swift utility library — nothing SwiftUI- or
        // AppKit-coupled. Lives here so the app target and the
        // FFI test runner can both depend on it without one
        // pulling in the other. Currently carries the
        // path-containment helper used by the right-click menu's
        // security guard (audit fix #5).
        .target(
            name: "ApfsCore",
            path: "Sources/ApfsCore"
        ),
        .executableTarget(
            name: "ApfsFastindex",
            dependencies: ["CApfsFastindex", "ApfsCore"],
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
        ),
        // Swift-side FFI test runner. A `.executableTarget`
        // rather than a `.testTarget` because XCTest requires a
        // full Xcode install; we want the tests to run under
        // the bare Command Line Tools toolchain too (and in any
        // headless CI). The runner exits 0 on success, non-zero
        // on the first failed assertion.
        //
        // Same `-L` flag the app uses so the static lib
        // (staged by `make-release.sh`) resolves at link time.
        // Run via: `swift run --package-path app apfs-ffi-tests`.
        .executableTarget(
            name: "apfs-ffi-tests",
            dependencies: ["CApfsFastindex", "ApfsCore"],
            path: "Tests/ApfsFastindexTests",
            linkerSettings: [
                .unsafeFlags([
                    "-L", "Sources/CApfsFastindex",
                ])
            ]
        )
    ]
)
