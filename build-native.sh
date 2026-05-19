#!/bin/sh
# Build the native macOS app (Rust crate + SwiftPM executable).
#
# Steps:
#   1. `cargo build --release -p apfs-fastindex` produces the
#      static lib (libapfs_fastindex.a) + cbindgen-generated
#      C header (apfs_fastindex.h).
#   2. Both are copied into `app/Sources/CApfsFastindex/`, where
#      the SwiftPM `systemLibrary` shim target picks them up.
#   3. `swift build` links the static lib into the executable.
#
# Run from the repo root:
#   sh build-native.sh
#
# For a release build of just the Rust side (e.g. iterating on
# the FFI without re-running swift), invoke
# `cargo build --release -p apfs-fastindex` directly; the
# cbindgen `build.rs` regenerates the header on every change to
# `src/ffi.rs` or `cbindgen.toml`.

set -eu

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_ROOT"

PROFILE="${PROFILE:-release}"
case "$PROFILE" in
    release|debug) ;;
    *) echo "build-native.sh: PROFILE must be release or debug, got $PROFILE" >&2; exit 2 ;;
esac

echo "==> Building apfs-fastindex Rust crate ($PROFILE)"
if [ "$PROFILE" = "release" ]; then
    cargo build --release -p apfs-fastindex
else
    cargo build -p apfs-fastindex
fi

echo "==> Staging static lib + header for SwiftPM"
SHIM_DIR="$REPO_ROOT/app/Sources/CApfsFastindex"
mkdir -p "$SHIM_DIR"
cp "$REPO_ROOT/target/$PROFILE/libapfs_fastindex.a"      "$SHIM_DIR/libapfs_fastindex.a"
cp "$REPO_ROOT/target/$PROFILE/include/apfs_fastindex.h" "$SHIM_DIR/apfs_fastindex.h"

echo "==> Building SwiftPM target"
cd "$REPO_ROOT/app"
swift build $([ "$PROFILE" = "release" ] && echo "-c release")

echo "==> Done. Executable:"
echo "    $REPO_ROOT/app/.build/$([ "$PROFILE" = "release" ] && echo "release" || echo "debug")/ApfsFastindex"
