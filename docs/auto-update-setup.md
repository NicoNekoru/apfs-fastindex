# Auto-update setup (Sparkle 2 + GitHub releases)

The app uses [Sparkle 2](https://sparkle-project.org) for in-app
updates, with the appcast served from `appcast.xml` on the
`main` branch
(https://raw.githubusercontent.com/NicoNekoru/apfs-fastindex/main/appcast.xml).
Daily background check on launch + manual "Check for Updates…"
under the application menu.

## One-time setup

Sparkle verifies every downloaded update against an EdDSA
public key embedded in the app's `Info.plist`. The matching
private key lives in the maintainer's macOS Keychain. Publishing
requires both keys. The release script stops before publishing if
the public key is missing or signing fails.

### 1. Build the app once so SwiftPM unpacks Sparkle

```sh
./make-release.sh
```

This drops two tools alongside Sparkle's xcframework under
`app/.build/artifacts/sparkle/Sparkle/bin/`:

- `sign_update` — signs release zips. `make-release.sh
  --publish` invokes it for you.
- `generate_keys` — one-time keypair generator. You run this
  manually, once.

### 2. Generate the EdDSA keypair

```sh
app/.build/artifacts/sparkle/Sparkle/bin/generate_keys
```

Stores the private key in your login Keychain (service
`https://sparkle-project.org`, account `ed25519`) and prints
the public key to stdout.

### 3. Save the public key in the repo

```sh
echo -n "<the base64 public key>" > app/sparkle-public-key.txt
```

`make-release.sh` reads this whitespace-trimmed and splices
it into `Info.plist`'s `SUPublicEDKey` on every build. The
public key is non-secret — commit it.

## Per-release ritual

**Run locally**, not from CI. The CI workflow on `macos-14`
runners has no access to your Keychain, so its `sign_update`
calls fail and the appcast is never updated. (See "CI
limitations" below for the longer story.)

```sh
# Commit the release's source changes and pass CI first.
# Then build and publish the next patch version.
./make-release.sh --publish --patch
```

The script bumps the crate version and lockfile, builds with the
selected macOS SDK, checks the executable's SDK metadata, assembles
and signs the app, and creates ZIP and DMG downloads. It signs the
ZIP for Sparkle, prepends the appcast entry, commits the release,
creates an annotated tag, and pushes the commit and tag before
creating the GitHub release. Do not create the tag yourself.

Use `--tag v0.3.2` instead of `--patch` when the version has already
been bumped and tested. Both paths require committed source changes.
Use a Rust toolchain whose standard library supports macOS 13.

The Homebrew tap's version and DMG checksum are updated after
publication. Its submodule pointer gets a separate follow-up commit,
so the release tag remains tied to the source used for the build.

Existing v0.1.0+ installs see the update on their next daily
background check (or instantly if the user picks "Check for
Updates…").

## Why hardcoded version strings break updates silently

Three coupled values must agree for Sparkle to ever offer an
update:

| Where | Value | Source |
|---|---|---|
| `Info.plist` of the running app | `CFBundleShortVersionString` | `APP_VERSION` in build |
| `Info.plist` of the new app (inside the zip) | `CFBundleShortVersionString` | `APP_VERSION` of that build |
| `appcast.xml` `<item>` | `sparkle:version` | `APP_VERSION` at publish |

If `APP_VERSION` is hardcoded (we shipped that bug in v0.1.0,
v0.2.0, v0.2.1 — every release contained the same `0.1.0`
bundle regardless of the tag), Sparkle decides the "available"
version equals the running version and silently reports
"you're up to date" forever.

`make-release.sh` now derives `APP_VERSION` from, in priority
order: `--tag vX.Y.Z` (CLI), `$GITHUB_REF_NAME` (CI tag push),
or `crates/apfs-fastindex/Cargo.toml`'s `version =` line.
A missing version is a hard error.

## CI limitations

`.github/workflows/release.yml` is manual-only. Tag pushes do not
publish releases. A fresh runner can build the app, but cannot
complete publication because it has no private signing key.

To automate publication on CI, the two pieces needed are:

1. Store the Ed25519 private key as a repo secret
   (`SPARKLE_ED_PRIVATE_KEY`) and import it into the runner's
   Keychain before `sign_update` runs.
2. Give the workflow `contents: write` (it already has that)
   plus a step to commit `appcast.xml` back to `main` after
   the script finishes.

Neither is implemented today — the security trade-off of
giving CI the signing key is a maintainer decision.

## Published releases are immutable

Each publication needs a new version and tag. Do not rerun the
publish command for an existing release or replace its downloads.
For a broken release, fix the source and publish a new patch version.

## Failure modes worth knowing

- **Empty `SUPublicEDKey` in Info.plist.** Sparkle silently
  refuses to install updates. Every check reports "up to
  date." Fix: ensure `app/sparkle-public-key.txt` exists and
  re-build.
- **`sign_update` not found.** SwiftPM didn't unpack Sparkle.
  Run `swift package clean` inside `app/` and rebuild.
- **dyld can't find `Sparkle.framework`.** `make-release.sh`
  didn't copy the framework into `Contents/Frameworks/`.
  Re-run; the script picks it up from
  `app/.build/<triple>/release/Sparkle.framework`.
- **"code signature … different Team IDs"** when launching.
  `app/app.entitlements` already includes
  `com.apple.security.cs.disable-library-validation` to
  bypass this — if you stripped it, add it back.
- **Appcast has zero `<item>` entries despite releases
  existing.** The publish path either wasn't run locally
  (CI alone can't update the appcast) or `sign_update`
  failed silently. Re-run `make-release.sh --publish --tag
  vX.Y.Z` locally and check the stderr.
