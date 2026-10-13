#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APFS_TEST_SHIM="$REPO_ROOT/app/Sources/CApfsFastindex"
if [ ! -f "$APFS_TEST_SHIM/libapfs_fastindex.a" ]; then
    echo "Run ./make-release.sh --no-bundle before the file-action tests." >&2
    exit 1
fi
APFS_TEST_BUILD_DIR="$(mktemp -d /tmp/apfs-file-actions.XXXXXX)"
APFS_TEST_SDK="$(xcrun --sdk macosx --show-sdk-path)"
APFS_TEST_TARGET="$(uname -m)-apple-macosx13.0"

# Compile the actual app sources. SwiftPM does not allow the same source
# files in two targets, and copying the menu implementation would miss bugs.
swiftc -swift-version 5 -sdk "$APFS_TEST_SDK" -target "$APFS_TEST_TARGET" \
    -parse-as-library -emit-library -emit-module -module-name ApfsCore \
    "$REPO_ROOT"/app/Sources/ApfsCore/*.swift \
    -o "$APFS_TEST_BUILD_DIR/libApfsCore.dylib"
swiftc -swift-version 5 -sdk "$APFS_TEST_SDK" -target "$APFS_TEST_TARGET" \
    -parse-as-library -I "$APFS_TEST_BUILD_DIR" -I "$APFS_TEST_SHIM" \
    -L "$APFS_TEST_BUILD_DIR" -L "$APFS_TEST_SHIM" -lApfsCore -lapfs_fastindex \
    -Xlinker -rpath -Xlinker "$APFS_TEST_BUILD_DIR" \
    "$REPO_ROOT/app/Sources/ApfsFastindex/NativeBridge.swift" \
    "$REPO_ROOT/app/Sources/ApfsFastindex/SnapshotDetect.swift" \
    "$REPO_ROOT/app/Sources/ApfsFastindex/FileActions.swift" \
    "$REPO_ROOT/app/Tests/FileActionsTests.swift" \
    -o "$APFS_TEST_BUILD_DIR/apfs-file-action-tests"
"$APFS_TEST_BUILD_DIR/apfs-file-action-tests"
