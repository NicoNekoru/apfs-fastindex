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

# APP_VERSION resolution (canonical source: the crate version
# in `crates/apfs-fastindex/Cargo.toml`).
#
# Why this matters: APP_VERSION ends up in CFBundleShortVersionString
# AND in `<sparkle:version>` in the appcast item. Sparkle compares
# the running app's CFBundleShortVersionString against the
# appcast item's <sparkle:version> to decide if an update is
# available. If APP_VERSION lies (e.g. hardcoded "0.1.0" while
# we cut a `v0.2.1` tag), every release ships a bundle that
# reports the wrong version, no user ever sees an update offer,
# and auto-update is silently broken.
#
# Three precedence tiers, highest first:
#
#   1. `--tag vX.Y.Z` strips the leading `v` and uses X.Y.Z.
#      Lets a re-tagged build override the crate manifest.
#   2. `$GITHUB_REF_NAME` for CI tag-pushed workflows (same
#      strip rule).
#   3. `version =` line in `crates/apfs-fastindex/Cargo.toml`.
#      The single-source-of-truth fallback for dev builds.
#
# A missing version is a hard error rather than a silent
# default — the previous bug (hardcoded "0.1.0") was caused by
# exactly that kind of stand-in.
APP_VERSION=""
if [ -n "$RELEASE_TAG" ]; then
    APP_VERSION="${RELEASE_TAG#v}"
elif [ -n "$GITHUB_REF_NAME" ] && [[ "$GITHUB_REF_NAME" =~ ^v[0-9] ]]; then
    APP_VERSION="${GITHUB_REF_NAME#v}"
fi
if [ -z "$APP_VERSION" ]; then
    APP_VERSION="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' \
        "$REPO_ROOT/crates/apfs-fastindex/Cargo.toml")"
fi
if [ -z "$APP_VERSION" ]; then
    echo "make-release.sh: could not determine APP_VERSION." >&2
    echo "  pass --tag vX.Y.Z or set the version in Cargo.toml." >&2
    exit 1
fi
echo "    APP_VERSION=$APP_VERSION"

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

# Sparkle.framework — copied into Contents/Frameworks/ so the
# main binary's dyld can find it at runtime. SwiftPM only stages
# the framework next to the binary at build time; we have to
# ship it inside the bundle ourselves. The framework includes
# Sparkle's nested helpers (Autoupdate, Updater.app,
# Installer.xpc, Downloader.xpc) which the codesign step below
# re-signs via `--deep`.
SPARKLE_FRAMEWORK_SRC="$SWIFT_BUILD_DIR/Sparkle.framework"
if [ ! -d "$SPARKLE_FRAMEWORK_SRC" ]; then
    echo "make-release.sh: swift build did not produce $SPARKLE_FRAMEWORK_SRC" >&2
    echo "  (Sparkle is a SwiftPM dependency — re-run `swift build` to repopulate.)" >&2
    exit 1
fi
mkdir -p "$BUNDLE/Contents/Frameworks"
# `cp -R` preserves symlinks (Sparkle.framework is symlink-heavy
# under Versions/Current).
cp -R "$SPARKLE_FRAMEWORK_SRC" "$BUNDLE/Contents/Frameworks/"

# Sparkle auto-update configuration. Reads the EdDSA public key
# from `app/sparkle-public-key.txt` if present; the corresponding
# private key lives in the maintainer's Keychain (one-time
# setup: `sign_update --generate-keys`). Without the public-key
# file the build still succeeds, but auto-updates are disabled
# (Sparkle refuses to install unsigned updates) — useful for dev
# builds where the dev hasn't done the one-time setup yet.
SPARKLE_PUBKEY_FILE="$REPO_ROOT/app/sparkle-public-key.txt"
SPARKLE_PUBKEY=""
if [ -f "$SPARKLE_PUBKEY_FILE" ]; then
    SPARKLE_PUBKEY="$(tr -d ' \t\r\n' < "$SPARKLE_PUBKEY_FILE")"
fi
# Appcast lives on main; raw.githubusercontent serves it.
# Hard-coded to the upstream `apfs-fastindex` repo. Forks that
# want their own update channel can override SUFeedURL post-build
# via `defaults write` or patch this string.
SPARKLE_APPCAST_URL="https://raw.githubusercontent.com/NicoNekoru/apfs-fastindex/main/appcast.xml"

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
    <!-- Sparkle (auto-update) keys. SUFeedURL is the appcast on
         main; SUPublicEDKey is the EdDSA verification key Sparkle
         uses to check every download before installing. Empty
         pubkey disables updates rather than allowing unsigned
         installs — the safe default for dev builds without the
         one-time key setup. -->
    <key>SUFeedURL</key>
    <string>$SPARKLE_APPCAST_URL</string>
    <key>SUPublicEDKey</key>
    <string>$SPARKLE_PUBKEY</string>
    <key>SUEnableAutomaticChecks</key>
    <true/>
    <key>SUAutomaticallyUpdate</key>
    <false/>
    <key>SUScheduledCheckInterval</key>
    <integer>86400</integer>
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

    # Sign Sparkle.framework's nested helpers first. Sparkle 2
    # ships an Autoupdate binary, an Updater.app, and two XPC
    # services (Installer + Downloader) inside the framework;
    # macOS requires each to be signed in inside-out order
    # before the framework itself, then the outer app.
    # `--force` overrides any pre-existing ad-hoc signature
    # from the SwiftPM build; `--deep` would normally cascade
    # but is documented as "best effort" in Apple's manpage,
    # so we sign the leaves explicitly.
    SPARKLE_FW="$BUNDLE/Contents/Frameworks/Sparkle.framework"
    if [ -d "$SPARKLE_FW" ]; then
        # Resolve `Versions/Current` once so the loop below
        # doesn't traverse symlinks twice.
        SPARKLE_VER="$SPARKLE_FW/Versions/Current"
        for nested in \
            "$SPARKLE_VER/XPCServices/Installer.xpc" \
            "$SPARKLE_VER/XPCServices/Downloader.xpc" \
            "$SPARKLE_VER/Autoupdate" \
            "$SPARKLE_VER/Updater.app"
        do
            if [ -e "$nested" ]; then
                codesign --force --sign - --options runtime \
                    --timestamp=none "$nested"
            fi
        done
        codesign --force --sign - --options runtime \
            --timestamp=none "$SPARKLE_FW"
    fi

    codesign \
        --force \
        --sign - \
        --options runtime \
        --entitlements "$ENTITLEMENTS" \
        --timestamp=none \
        "$BUNDLE"
    # Verify the signature stuck. `--strict` catches bundles
    # where the executable is signed but a nested resource
    # isn't (e.g. the Sparkle framework helpers above).
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
    # APP_VERSION was already resolved above (CLI tag, then
    # GITHUB_REF_NAME, then Cargo.toml). Reuse it so the tag the
    # release lands under matches the version baked into the
    # bundle's Info.plist — without that match, Sparkle's version
    # comparison would treat the new release as the same as the
    # currently-running app and never offer the update.
    RELEASE_TAG="v$APP_VERSION"
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

# ---------------------------------------------------------------
# Sparkle appcast update.
#
# Sign the zip with `sign_update` (Sparkle's CLI tool, ships
# inside the Sparkle SwiftPM checkout under
# artifacts/Sparkle/bin/) and append a new <item> to
# appcast.xml. The maintainer then commits + pushes
# appcast.xml so the next user-side daily check picks up the
# release.
#
# Locating `sign_update`: SwiftPM unzips Sparkle's xcframework
# under .build/artifacts/. The exact path varies by Sparkle
# version, so we glob for the binary and fall back to a
# clear error if we can't find it.
#
# When the EdDSA public-key file is missing (dev hasn't done
# the one-time `sign_update --generate-keys` setup), skip the
# appcast update entirely. The GitHub release still lands; the
# release just isn't yet visible to auto-updaters until the
# maintainer signs it manually.
# ---------------------------------------------------------------
if [ ! -f "$SPARKLE_PUBKEY_FILE" ]; then
    echo
    echo "    Sparkle: app/sparkle-public-key.txt missing — skipping appcast."
    echo "    One-time setup:"
    echo "      1. find .build -name sign_update -type f   # in app/"
    echo "      2. <path-to-sign_update> --generate-keys"
    echo "      3. The public key prints to stdout; write it to"
    echo "         app/sparkle-public-key.txt (single line, no newline)."
    echo "      4. Re-run make-release.sh --publish."
    exit 0
fi

SIGN_UPDATE_BIN="$(
    find "$REPO_ROOT/app/.build" -name sign_update -type f -perm +u+x 2>/dev/null | head -1
)"
if [ -z "$SIGN_UPDATE_BIN" ]; then
    echo
    echo "    Sparkle: sign_update binary not found under app/.build."
    echo "    swift build should have unpacked it from the Sparkle SwiftPM"
    echo "    artifact. The GitHub release was uploaded but the appcast was"
    echo "    NOT updated; auto-update clients won't see this version until"
    echo "    you re-run make-release.sh --publish after a clean build."
    exit 1
fi

echo
echo "==> Sparkle: sign + appcast update"
SIGN_OUTPUT="$("$SIGN_UPDATE_BIN" "$ASSET_PATH")"
# `sign_update` prints one line like:
#   sparkle:edSignature="..." length="12345"
# Parse out the signature and length so we can drop them into
# the appcast item.
ED_SIG="$(echo "$SIGN_OUTPUT" | sed -n 's/.*sparkle:edSignature="\([^"]*\)".*/\1/p')"
ASSET_LEN="$(echo "$SIGN_OUTPUT" | sed -n 's/.*length="\([^"]*\)".*/\1/p')"
if [ -z "$ED_SIG" ] || [ -z "$ASSET_LEN" ]; then
    echo "    sign_update output not in the expected shape; got:"
    echo "    $SIGN_OUTPUT"
    exit 1
fi

# Build the new appcast item. Released URL is the GitHub
# release-asset download URL — gh release returns it via the
# `gh release view` JSON view.
ASSET_URL="$(
    gh release view "$RELEASE_TAG" --json assets \
        --jq ".assets[] | select(.name == \"$ASSET_NAME\") | .url" 2>/dev/null
)"
if [ -z "$ASSET_URL" ]; then
    echo "    Could not resolve asset URL for $ASSET_NAME on release $RELEASE_TAG."
    exit 1
fi

PUB_DATE="$(date -u +'%a, %d %b %Y %H:%M:%S +0000')"
NEW_ITEM=$(cat <<XML
        <item>
            <title>$RELEASE_TAG</title>
            <pubDate>$PUB_DATE</pubDate>
            <sparkle:version>$APP_VERSION</sparkle:version>
            <sparkle:shortVersionString>$APP_VERSION</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
            <enclosure
                url="$ASSET_URL"
                length="$ASSET_LEN"
                type="application/octet-stream"
                sparkle:edSignature="$ED_SIG"
            />
        </item>
XML
)

# Insert the new item right after `<channel>...</description>`
# header block. The marker we splice on is `</language>` so
# new items consistently land at the top (newest-first).
APPCAST="$REPO_ROOT/appcast.xml"
APPCAST_TMP="$APPCAST.tmp"
awk -v item="$NEW_ITEM" '
    /<\/language>/ {
        print
        print ""
        print item
        next
    }
    { print }
' "$APPCAST" > "$APPCAST_TMP"
mv "$APPCAST_TMP" "$APPCAST"

echo "    appcast.xml updated with $RELEASE_TAG."
echo
echo "    Next step: commit appcast.xml and push to main so the"
echo "    daily background check on existing installations sees"
echo "    the new release:"
echo "      git add appcast.xml && git commit -m 'release: $RELEASE_TAG' && git push"
