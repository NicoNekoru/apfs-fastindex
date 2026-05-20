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
