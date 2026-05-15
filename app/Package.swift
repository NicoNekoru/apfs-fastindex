// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "ApfsFastindex",
    platforms: [
        .macOS(.v13)
    ],
    targets: [
        .executableTarget(
            name: "ApfsFastindex",
            path: "Sources/ApfsFastindex",
            resources: [
                .copy("Resources/viz")
            ]
        )
    ]
)
