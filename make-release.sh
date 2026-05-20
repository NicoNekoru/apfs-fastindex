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
#   ./make-release.sh                       # release build (default)
#   PROFILE=debug ./make-release.sh         # debug build
#   ./make-release.sh --no-bundle           # stop after `swift build`; skip .app
#   ./make-release.sh --publish             # build, then publish a GitHub release
#   ./make-release.sh --publish --tag vX.Y.Z
#
# `--publish` zips app/ApfsFastindex.app and uploads it to a GitHub
# release via the `gh` CLI. Tag resolution order:
#   1. --tag <vX.Y.Z> on the command line
#   2. $GITHUB_REF_NAME (set by GitHub Actions on tag-push events)
#   3. v<crate version> from crates/apfs-fastindex/Cargo.toml
# The release is created if it doesn't exist; if it does, the asset
# is uploaded with --clobber so re-running the script after a fix
# overwrites the previous bundle.
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
PUBLISH=0
RELEASE_TAG=""
while [ $# -gt 0 ]; do
    case "$1" in
        --no-bundle) BUNDLE_APP=0 ;;
        --publish) PUBLISH=1 ;;
        --tag)
            shift
            if [ $# -eq 0 ]; then
                echo "make-release.sh: --tag requires a value (e.g. v0.1.0)" >&2
                exit 2
            fi
            RELEASE_TAG="$1"
            ;;
        --tag=*) RELEASE_TAG="${1#--tag=}" ;;
        -h|--help)
            sed -n '1,/^set -euo/p' "$0" | sed 's/^# \?//;$d'
            exit 0
            ;;
        *) echo "make-release.sh: unknown argument '$1'" >&2; exit 2 ;;
    esac
    shift
done

if [ "$PUBLISH" = "1" ] && [ "$BUNDLE_APP" = "0" ]; then
    echo "make-release.sh: --publish requires the .app bundle; remove --no-bundle" >&2
    exit 2
fi
if [ "$PUBLISH" = "1" ] && [ "$PROFILE" != "release" ]; then
    echo "make-release.sh: --publish requires PROFILE=release (got $PROFILE)" >&2
    exit 2
fi

# ---------------------------------------------------------------
# Step 1 — Rust crate.
# The build.rs hook regenerates the cbindgen header on every build,
# so we don't need a separate header-generation step. Output lands
# in target/$PROFILE/.
# ---------------------------------------------------------------
echo "==> [1/4] cargo build -p apfs-fastindex ($PROFILE)"
if [ "$PROFILE" = "release" ]; then
    cargo build --release -p apfs-fastindex
    cargo build --release -p apfs-fastindex --bin apfs-fastindex-scan
else
    cargo build -p apfs-fastindex
    cargo build -p apfs-fastindex --bin apfs-fastindex-scan
fi

# ---------------------------------------------------------------
# Step 2 — Stage the static library + canonical header into the
# SwiftPM systemLibrary shim. The shim's module.modulemap names
# `apfs_fastindex` and `apfs_fastindex.h`, so the file names below
# must match exactly.
#
# The header source is the **CI-checked canonical snapshot** at
# `crates/apfs-fastindex/include/apfs_fastindex.h`, not the
# per-build output at `target/<profile>/include/`. That single-
# sources the bridging copy: a hand-edit to the SwiftPM file
# can't drift undetected (audit r3 #F4), because the next run
# of `make-release.sh` overwrites it from the canonical.
#
# A consistency gate runs first: if `cargo build` produced a
# different header from the committed canonical, the script
# stops with a clear message. That catches the dev workflow
# where someone modified `ffi.rs` but hasn't yet committed the
# regenerated header — same failure mode CI catches, but local
# and immediate.
# ---------------------------------------------------------------
echo "==> [2/4] stage native bridge artifacts"
SHIM_DIR="$REPO_ROOT/app/Sources/CApfsFastindex"
mkdir -p "$SHIM_DIR"

CANONICAL_HEADER="$REPO_ROOT/crates/apfs-fastindex/include/apfs_fastindex.h"
GENERATED_HEADER="$REPO_ROOT/target/$PROFILE/include/apfs_fastindex.h"

if [ ! -f "$CANONICAL_HEADER" ]; then
    echo "make-release.sh: canonical header missing at $CANONICAL_HEADER" >&2
    echo "  cargo build produced one at $GENERATED_HEADER — copy it there and commit." >&2
    exit 1
fi
if ! diff -q "$CANONICAL_HEADER" "$GENERATED_HEADER" >/dev/null 2>&1; then
    echo "make-release.sh: canonical header is stale." >&2
    echo "  cargo build produced a different header at $GENERATED_HEADER." >&2
    echo "  Run: cp $GENERATED_HEADER $CANONICAL_HEADER && git add -p" >&2
    echo "  (this catches the same drift CI's cbindgen-drift job catches.)" >&2
    exit 1
fi

cp "$REPO_ROOT/target/$PROFILE/libapfs_fastindex.a" "$SHIM_DIR/libapfs_fastindex.a"
cp "$CANONICAL_HEADER" "$SHIM_DIR/apfs_fastindex.h"

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

# The "Scan as Administrator…" menu item spawns the CLI as a
# privileged subprocess via osascript. Ship the CLI inside the
# bundle so Bundle.main.url(forAuxiliaryExecutable:) can find it
# without depending on whatever apfs-fastindex-scan happens to
# live in $PATH on the user's machine.
CLI_BIN="$REPO_ROOT/target/$PROFILE/apfs-fastindex-scan"
if [ ! -x "$CLI_BIN" ]; then
    echo "make-release.sh: cargo did not produce $CLI_BIN" >&2
    exit 1
fi
cp "$CLI_BIN" "$BUNDLE/Contents/MacOS/apfs-fastindex-scan"

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

if [ "$PUBLISH" != "1" ]; then
    exit 0
fi

# ---------------------------------------------------------------
# Step 6 — Publish a GitHub release with the .app bundle.
#
# Resolve the tag, then zip the bundle (a tar archive would strip
# extended attributes and the ad-hoc codesignature, so zip is the
# only macOS-friendly choice). `gh release create` is idempotent
# the way we use it: if the release already exists we fall through
# to `gh release upload --clobber` so a re-run replaces the asset.
# ---------------------------------------------------------------
if [ -z "$RELEASE_TAG" ]; then
    if [ -n "${GITHUB_REF_NAME:-}" ] && [ "${GITHUB_REF_TYPE:-}" = "tag" ]; then
        RELEASE_TAG="$GITHUB_REF_NAME"
    else
        # Fall back to the crate version. `cargo pkgid` would be more
        # robust, but it requires a clean lockfile and network access
        # in some configurations; a grep keeps the publish path free
        # of cargo-side preconditions.
        CRATE_VERSION="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' \
            "$REPO_ROOT/crates/apfs-fastindex/Cargo.toml")"
        if [ -z "$CRATE_VERSION" ]; then
            echo "make-release.sh: could not determine release tag." >&2
            echo "  pass --tag vX.Y.Z or set GITHUB_REF_NAME." >&2
            exit 1
        fi
        RELEASE_TAG="v$CRATE_VERSION"
    fi
fi

if ! command -v gh >/dev/null 2>&1; then
    echo "make-release.sh: gh CLI not found; install from https://cli.github.com/" >&2
    exit 1
fi

echo "==> [6/6] publish GitHub release $RELEASE_TAG"
ARCH="$(uname -m)"
ASSET_NAME="ApfsFastindex-$RELEASE_TAG-macos-$ARCH.zip"
ASSET_PATH="$REPO_ROOT/app/$ASSET_NAME"

rm -f "$ASSET_PATH"
# `ditto -c -k --sequesterRsrc --keepParent` is Apple's recommended
# way to zip a .app: it preserves resource forks, symlinks, and the
# codesignature; plain `zip -r` mangles all three.
ditto -c -k --sequesterRsrc --keepParent "$BUNDLE" "$ASSET_PATH"

if gh release view "$RELEASE_TAG" >/dev/null 2>&1; then
    echo "    release $RELEASE_TAG exists; uploading asset with --clobber"
    gh release upload "$RELEASE_TAG" "$ASSET_PATH" --clobber
else
    echo "    creating release $RELEASE_TAG"
    gh release create "$RELEASE_TAG" "$ASSET_PATH" \
        --title "$RELEASE_TAG" \
        --generate-notes
fi

echo
echo "Published $ASSET_NAME to release $RELEASE_TAG."
