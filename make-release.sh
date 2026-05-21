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
# Cargo.toml version bump (publish path only).
#
# Why this happens here, before the build: the crate's
# Cargo.toml is the canonical version source. APP_VERSION
# (computed below at bundle-assembly time) reads from it. The
# build bakes APP_VERSION into Info.plist. The appcast item
# (later, in the publish section) carries the same number as
# `<sparkle:version>`. All three must agree, so we bump the
# canonical source first, then everything downstream reads
# the new value automatically.
#
# Triggers only when:
#   --publish is set, AND
#   --tag vX.Y.Z is given, AND
#   the current Cargo.toml version != X.Y.Z
#
# Without --tag the script uses whatever's already in
# Cargo.toml (dev / re-release of the current version).
# ---------------------------------------------------------------
if [ "$PUBLISH" = "1" ] && [ -n "$RELEASE_TAG" ]; then
    TARGET_VERSION="${RELEASE_TAG#v}"
    CRATE_TOML="$REPO_ROOT/crates/apfs-fastindex/Cargo.toml"
    CURRENT_VERSION="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$CRATE_TOML")"
    if [ "$CURRENT_VERSION" != "$TARGET_VERSION" ]; then
        echo "==> [0/6] bump Cargo.toml: $CURRENT_VERSION -> $TARGET_VERSION"
        # Targeted sed: only the first `version = "..."` line
        # (the `[package].version` field at the top of the
        # file). BSD sed (macOS) doesn't support the
        # `0,/regex/s//replacement/` GNU shorthand — the
        # back-reference `//` is rejected with "first RE may
        # not be empty" inside an address range. Spell the
        # pattern out twice instead: `1,/regex/` as the
        # address and the same pattern in the s command so the
        # substitution fires on (and only on) the first match.
        # `-i.bak` is required for BSD sed compatibility; the
        # .bak file is removed immediately after.
        sed -i.bak \
            "1,/^version = \"$CURRENT_VERSION\"/s/^version = \"$CURRENT_VERSION\"/version = \"$TARGET_VERSION\"/" \
            "$CRATE_TOML"
        rm -f "$CRATE_TOML.bak"
        # Sanity-check: the sed must have actually rewritten
        # the file. A silent no-op (e.g. if the pattern format
        # ever drifts) would otherwise leave the bundle on the
        # old version and produce a "phantom" release where
        # the appcast advertises vX.Y.Z but the zip inside
        # says something else.
        NEW_VERSION="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$CRATE_TOML")"
        if [ "$NEW_VERSION" != "$TARGET_VERSION" ]; then
            echo "make-release.sh: Cargo.toml bump failed." >&2
            echo "  Wanted $TARGET_VERSION, file still reports $NEW_VERSION." >&2
            exit 1
        fi
        # Refresh Cargo.lock so its `apfs-fastindex` entry
        # matches the new manifest version. `cargo update -p`
        # is the minimum-impact way to do this; it touches
        # only the one package's entry in the lockfile.
        cargo update -p apfs-fastindex --offline 2>/dev/null \
            || cargo update -p apfs-fastindex
    fi
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
# Step 6 — Single-command release.
#
# `make-release.sh --publish --tag v0.2.2` does everything:
#
#   1. Zip the bundle (already built above with APP_VERSION baked
#      in, which the earlier-in-the-script bump path set up).
#   2. Sign the zip via `sign_update` (reads private key from
#      Keychain).
#   3. Append a new <item> to appcast.xml with the deterministic
#      GitHub release-asset URL and the signature.
#   4. Commit Cargo.toml + appcast.xml as "release: vX.Y.Z".
#   5. Create the annotated tag locally.
#   6. Push the commit and tag.
#   7. `gh release create` the release with the zip attached.
#
# The Cargo.toml version bump happened earlier (before the
# build) when --publish + --tag was detected — see the
# `bump_cargo_version_for_publish` block.
#
# Pre-flight: every dependency we'll touch must be ready, and
# the working tree must be clean enough that we won't sweep
# unrelated changes into the release commit.
# ---------------------------------------------------------------
if ! command -v gh >/dev/null 2>&1; then
    echo "make-release.sh: gh CLI not found; install from https://cli.github.com/" >&2
    exit 1
fi
if [ ! -f "$SPARKLE_PUBKEY_FILE" ]; then
    echo "make-release.sh: app/sparkle-public-key.txt missing." >&2
    echo "  Run app/.build/artifacts/sparkle/Sparkle/bin/generate_keys once," >&2
    echo "  then echo -n the printed public key into app/sparkle-public-key.txt." >&2
    exit 1
fi
SIGN_UPDATE_BIN="$(
    find "$REPO_ROOT/app/.build/artifacts/sparkle/Sparkle/bin" \
        -name sign_update -type f 2>/dev/null | head -1
)"
if [ -z "$SIGN_UPDATE_BIN" ]; then
    echo "make-release.sh: sign_update not found under app/.build." >&2
    echo "  Did 'swift build' run? Sparkle should unpack its tools there." >&2
    exit 1
fi

RELEASE_TAG="v$APP_VERSION"
ARCH="$(uname -m)"
ASSET_NAME="ApfsFastindex-$RELEASE_TAG-macos-$ARCH.zip"
ASSET_PATH="$REPO_ROOT/app/$ASSET_NAME"

# Make sure the only files dirty in the working tree are ones
# we know how to handle (Cargo.toml from our pre-build bump
# and any earlier-staged but unrelated changes are blocked).
DIRTY="$(
    git -C "$REPO_ROOT" status --porcelain \
        | grep -vE '(^.. Cargo\.lock|^.. crates/apfs-fastindex/Cargo\.toml|^.. appcast\.xml|^\?\?)'
)"
if [ -n "$DIRTY" ]; then
    echo "make-release.sh: working tree has unrelated changes; commit or stash first:" >&2
    echo "$DIRTY" >&2
    exit 1
fi

# Repo owner/name for the deterministic asset URL. We don't
# need a network round-trip for this — GitHub's release-asset
# download URL is a stable function of (owner, repo, tag,
# filename) and gh repo view reads from the local .git/config
# remote.
REPO_FULL_NAME="$(
    gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null
)"
if [ -z "$REPO_FULL_NAME" ]; then
    echo "make-release.sh: could not resolve repo owner/name via gh." >&2
    exit 1
fi
ASSET_URL="https://github.com/$REPO_FULL_NAME/releases/download/$RELEASE_TAG/$ASSET_NAME"

echo "==> [6/6] release $RELEASE_TAG"

# 1. Zip.
rm -f "$ASSET_PATH"
# `ditto -c -k --sequesterRsrc --keepParent` is Apple's
# recommended way to zip a .app: preserves resource forks,
# symlinks, and the codesignature; plain `zip -r` mangles all
# three.
ditto -c -k --sequesterRsrc --keepParent "$BUNDLE" "$ASSET_PATH"

# 2. Sign the zip. sign_update prints one line in the shape
#    `sparkle:edSignature="..." length="..."`.
SIGN_OUTPUT="$("$SIGN_UPDATE_BIN" "$ASSET_PATH")"
ED_SIG="$(echo "$SIGN_OUTPUT" | sed -n 's/.*sparkle:edSignature="\([^"]*\)".*/\1/p')"
ASSET_LEN="$(echo "$SIGN_OUTPUT" | sed -n 's/.*length="\([^"]*\)".*/\1/p')"
if [ -z "$ED_SIG" ] || [ -z "$ASSET_LEN" ]; then
    echo "make-release.sh: sign_update output unexpected: $SIGN_OUTPUT" >&2
    exit 1
fi

# 3. Append <item> to appcast.xml. Inserted right after
#    </language> so the newest entry is first under <channel>.
#
# The item is written to a temp file and `getline`-read inside
# awk because awk's `-v var=value` rejects newlines with
# "newline in string" — the previous flow tried that and
# crashed the splice, which (under set -e) silently aborted
# the publish before the commit.
APPCAST="$REPO_ROOT/appcast.xml"
APPCAST_TMP="$APPCAST.tmp"
ITEM_TMP="$(mktemp)"
trap 'rm -f "$ITEM_TMP" "$APPCAST_TMP"' EXIT
PUB_DATE="$(date -u +'%a, %d %b %Y %H:%M:%S +0000')"
cat > "$ITEM_TMP" <<XML
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

awk -v item_file="$ITEM_TMP" '
    /<\/language>/ {
        print
        print ""
        while ((getline line < item_file) > 0) print line
        close(item_file)
        next
    }
    { print }
' "$APPCAST" > "$APPCAST_TMP"
mv "$APPCAST_TMP" "$APPCAST"

# Sanity check: the splice must have produced a file that
# actually contains the new <item>. A silent failure here
# would otherwise commit a no-op appcast.xml and ship a
# release the auto-updater can't see.
if ! grep -q "<title>$RELEASE_TAG</title>" "$APPCAST"; then
    echo "make-release.sh: appcast.xml splice failed — file has no <item> for $RELEASE_TAG." >&2
    exit 1
fi
echo "    appcast.xml: prepended <item> for $RELEASE_TAG"

# 4. Commit Cargo.toml (already bumped above) + appcast.xml.
git -C "$REPO_ROOT" add \
    crates/apfs-fastindex/Cargo.toml \
    Cargo.lock \
    appcast.xml
git -C "$REPO_ROOT" commit -m "release: $RELEASE_TAG" \
    --quiet
echo "    git: committed Cargo.toml + appcast.xml"

# 5. Annotated tag pointing at the commit we just made.
if git -C "$REPO_ROOT" rev-parse "$RELEASE_TAG" >/dev/null 2>&1; then
    echo "make-release.sh: tag $RELEASE_TAG already exists locally." >&2
    echo "  Delete it (git tag -d $RELEASE_TAG) or pick a different tag." >&2
    exit 1
fi
git -C "$REPO_ROOT" tag -a "$RELEASE_TAG" -m "$RELEASE_TAG"
echo "    git: tagged $RELEASE_TAG"

# 6. Push commit + tag. The user is expected to be on a branch
#    that's tracking the upstream they want to push to (usually
#    main). git push --follow-tags ensures the new tag rides
#    along with the commit.
BRANCH="$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD)"
git -C "$REPO_ROOT" push --follow-tags origin "$BRANCH"
echo "    git: pushed $BRANCH + $RELEASE_TAG to origin"

# 7. gh release create — uploads the zip atomically with the
#    release creation. `gh release create` accepts the tag
#    we just pushed and refuses if the tag doesn't exist on
#    the remote, which we just handled.
gh release create "$RELEASE_TAG" "$ASSET_PATH" \
    --title "$RELEASE_TAG" \
    --generate-notes
echo "    gh: created release $RELEASE_TAG with $ASSET_NAME"

echo
echo "✅ Released $RELEASE_TAG."
echo "   Existing v$( awk -F'"' '/^version/ { print $2; exit }' \
    "$REPO_ROOT/crates/apfs-fastindex/Cargo.toml" )+ installs"
echo "   will see the update on their next daily check."
