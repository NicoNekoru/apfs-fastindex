#!/usr/bin/env bash
# Build a real .app bundle for ApfsFastindex.
#
# `swift run` works for fast iteration but fights macOS's activation
# rules (no bundle = no GUI app). This script produces a proper
# ApfsFastindex.app that Finder, Spotlight, and `open` treat as a
# first-class GUI program — no policy hacks needed.

set -euo pipefail

cd "$(dirname "$0")"

CONFIGURATION="${CONFIGURATION:-release}"
BUNDLE="ApfsFastindex.app"
BUNDLE_ID="com.apfsfastindex.app"
APP_VERSION="0.1.0"

echo "[1/5] swift build -c $CONFIGURATION"
swift build -c "$CONFIGURATION"

BUILD_DIR=".build/$CONFIGURATION"
BIN="$BUILD_DIR/ApfsFastindex"
if [[ ! -x "$BIN" ]]; then
    echo "swift build did not produce $BIN" >&2
    exit 1
fi

# Find the SwiftPM-generated resource bundle (named "<Package>_<Target>.bundle").
RESOURCE_BUNDLE=""
for candidate in "$BUILD_DIR"/*_ApfsFastindex.bundle; do
    if [[ -d "$candidate" ]]; then
        RESOURCE_BUNDLE="$candidate"
        break
    fi
done
if [[ -z "$RESOURCE_BUNDLE" ]]; then
    echo "could not find SwiftPM resource bundle under $BUILD_DIR" >&2
    exit 1
fi

echo "[2/5] reset $BUNDLE"
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"

echo "[3/5] copy executable + resources"
cp "$BIN" "$BUNDLE/Contents/MacOS/ApfsFastindex"
cp -R "$RESOURCE_BUNDLE" "$BUNDLE/Contents/Resources/"

echo "[4/5] write Info.plist"
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

echo "[5/5] bundle ready: $(pwd)/$BUNDLE"
echo ""
echo "Run with:"
echo "    open $(pwd)/$BUNDLE"
echo "Or drag into /Applications."
