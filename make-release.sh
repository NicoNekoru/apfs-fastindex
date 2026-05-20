#!/usr/bin/env bash
# Single end-to-end release build for the apfs-fastindex native app.
#
# Pipeline:
#   1. cargo build -p apfs-fastindex             (Rust static lib + cbindgen header)
#   2. stage artifacts into app/Sources/CApfsFastindex/      (SwiftPM systemLibrary shim)
#   3. swift build -c <profile>                  (link the executable)
#   4. assemble app/ApfsFastindex.app            (bundle + Info.plist, Finder-launchable)
#
# Replaces the prior `build-native.sh` + `app/make-app.sh` pair —
# they ran `swift build` twice in sequence and split the .a-staging
# and bundle-assembly steps for no clear reason.
#
# Usage:
#   ./make-release.sh                # release build (default)
#   PROFILE=debug ./make-release.sh  # debug build
#   ./make-release.sh --no-bundle    # stop after `swift build`; skip .app
#
# After a successful run the app lives at:
#   app/ApfsFastindex.app
# Run it with:
#   open app/ApfsFastindex.app

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_ROOT"

PROFILE="${PROFILE:-release}"
case "$PROFILE" in
    release|debug) ;;
    *) echo "make-release.sh: PROFILE must be release or debug, got $PROFILE" >&2; exit 2 ;;
esac

BUNDLE_APP=1
for arg in "$@"; do
    case "$arg" in
        --no-bundle) BUNDLE_APP=0 ;;
        -h|--help)
            sed -n '1,/^set -euo/p' "$0" | sed 's/^# \?//;$d'
            exit 0
            ;;
        *) echo "make-release.sh: unknown argument '$arg'" >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------
# Step 1 — Rust crate.
# The build.rs hook regenerates the cbindgen header on every build,
# so we don't need a separate header-generation step. Output lands
# in target/$PROFILE/.
# ---------------------------------------------------------------
echo "==> [1/4] cargo build -p apfs-fastindex ($PROFILE)"
if [ "$PROFILE" = "release" ]; then
    cargo build --release -p apfs-fastindex
else
    cargo build -p apfs-fastindex
fi

# ---------------------------------------------------------------
# Step 2 — Stage the static library + generated header into the
# SwiftPM systemLibrary shim. The shim's module.modulemap names
# `apfs_fastindex` and `apfs_fastindex.h`, so the file names below
# must match exactly.
# ---------------------------------------------------------------
echo "==> [2/4] stage native bridge artifacts"
SHIM_DIR="$REPO_ROOT/app/Sources/CApfsFastindex"
mkdir -p "$SHIM_DIR"
cp "$REPO_ROOT/target/$PROFILE/libapfs_fastindex.a"      "$SHIM_DIR/libapfs_fastindex.a"
cp "$REPO_ROOT/target/$PROFILE/include/apfs_fastindex.h" "$SHIM_DIR/apfs_fastindex.h"

# ---------------------------------------------------------------
# Step 3 — Swift executable.
# ---------------------------------------------------------------
echo "==> [3/4] swift build -c $PROFILE"
SWIFT_FLAGS=""
if [ "$PROFILE" = "release" ]; then
    SWIFT_FLAGS="-c release"
fi
(cd "$REPO_ROOT/app" && swift build $SWIFT_FLAGS)

SWIFT_BUILD_DIR="$REPO_ROOT/app/.build/$PROFILE"
BIN="$SWIFT_BUILD_DIR/ApfsFastindex"
if [ ! -x "$BIN" ]; then
    echo "make-release.sh: swift build did not produce $BIN" >&2
    exit 1
fi

if [ "$BUNDLE_APP" = "0" ]; then
    echo "==> [4/4] skipping .app bundle (--no-bundle)"
    echo
    echo "Done. Executable: $BIN"
    exit 0
fi

# ---------------------------------------------------------------
# Step 4 — Assemble the .app bundle so Finder / Spotlight / `open`
# treat the binary as a real GUI app. Without a bundle macOS won't
# raise the window or give the process a dock icon.
# ---------------------------------------------------------------
echo "==> [4/4] assemble app/ApfsFastindex.app"
BUNDLE="$REPO_ROOT/app/ApfsFastindex.app"
BUNDLE_ID="com.apfsfastindex.app"
APP_VERSION="0.1.0"

# Find the SwiftPM-generated resource bundle (named
# `<Package>_<Target>.bundle` — usually
# `ApfsFastindex_ApfsFastindex.bundle`).
RESOURCE_BUNDLE=""
for candidate in "$SWIFT_BUILD_DIR"/*_ApfsFastindex.bundle; do
    if [ -d "$candidate" ]; then
        RESOURCE_BUNDLE="$candidate"
        break
    fi
done

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$BIN" "$BUNDLE/Contents/MacOS/ApfsFastindex"
if [ -n "$RESOURCE_BUNDLE" ]; then
    cp -R "$RESOURCE_BUNDLE" "$BUNDLE/Contents/Resources/"
fi

cat > "$BUNDLE/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleName</key>
    <string>apfs-fastindex</string>
    <key>CFBundleDisplayName</key>
    <string>apfs-fastindex</string>
    <key>CFBundleExecutable</key>
    <string>ApfsFastindex</string>
    <key>CFBundleIconFile</key>
    <string></string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$APP_VERSION</string>
    <key>CFBundleVersion</key>
    <string>$APP_VERSION</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF


# ---------------------------------------------------------------
# Codesign — ad-hoc (identity `-`), hardened runtime, with the
# `app.entitlements` file. No developer cert / notarization;
# locally-built bundles still launch because Gatekeeper allows
# ad-hoc-signed apps built on the same machine. For
# distribution, swap `-` for a real Developer ID identity.
#
# Hardened runtime is the new-since-Catalina lockdown profile;
# it disables a few legacy macOS conveniences (JIT, unsigned
# dylibs, dyld env vars) that the app doesn't use. Pairs with
# `app/app.entitlements` to grant the specific exceptions the
# context menu / file viewer needs.
# ---------------------------------------------------------------
ENTITLEMENTS="$REPO_ROOT/app/app.entitlements"
if [ -f "$ENTITLEMENTS" ]; then
    echo "==> [5/5] codesign --options runtime (ad-hoc)"
    codesign \
        --force \
        --sign - \
        --options runtime \
        --entitlements "$ENTITLEMENTS" \
        --timestamp=none \
        "$BUNDLE"
    # Verify the signature stuck. `--strict` catches bundles
    # where the executable is signed but a nested resource
    # isn't (we don't have nested bundles today, but the check
    # is free and protects against future regressions).
    codesign --verify --strict --verbose=1 "$BUNDLE" 2>&1 | sed 's/^/    /'
fi

echo
echo "Done. App: $BUNDLE"
echo "Run with: open $BUNDLE"
