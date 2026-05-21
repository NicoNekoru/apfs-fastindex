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
private key lives in the maintainer's macOS Keychain. Without
this setup, `make-release.sh --publish` will still upload the
GitHub release but skip the appcast update — auto-updaters
won't see the new version until the key is in place.

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
# 1. Bump the version. The crate's Cargo.toml is the canonical
#    source: APP_VERSION in make-release.sh derives from it.
#    Edit crates/apfs-fastindex/Cargo.toml:
#      version = "0.2.2"          # ← bump here

# 2. Commit the version bump.
git commit -am "release: prep v0.2.2"

# 3. Tag (annotated tag triggers the CI release workflow,
#    which uploads the zip; that's idempotent with your
#    local publish below so it's fine to do either order).
git tag -a v0.2.2 -m "v0.2.2"
git push origin main v0.2.2

# 4. Build + publish locally. This:
#    - bakes APP_VERSION=0.2.2 into Info.plist
#    - uploads the asset (no-op if CI beat you to it,
#      thanks to --clobber)
#    - signs the zip with sign_update (reads private key
#      from Keychain)
#    - appends a new <item> to appcast.xml
#    - prints the git add/commit/push hint for appcast.xml
./make-release.sh --publish --tag v0.2.2

# 5. Commit + push the appcast update.
git add appcast.xml
git commit -m "release: appcast v0.2.2"
git push
```

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

`.github/workflows/release.yml` runs `make-release.sh --publish`
on a fresh `macos-14` runner whenever a `v*` tag is pushed.
It can:

- Build the bundle + zip + upload the asset to the release.
- Bake the right `CFBundleShortVersionString` (it picks up
  `$GITHUB_REF_NAME`).

It can *not*:

- Sign the zip (no private key in the runner's Keychain →
  `sign_update` fails).
- Update or commit `appcast.xml`.

So the CI workflow gets the binary to users who download
manually, but auto-update is driven by the local `--publish`
run. If you want to automate the appcast update on CI, the
two pieces needed are:

1. Store the Ed25519 private key as a repo secret
   (`SPARKLE_ED_PRIVATE_KEY`) and import it into the runner's
   Keychain before `sign_update` runs.
2. Give the workflow `contents: write` (it already has that)
   plus a step to commit `appcast.xml` back to `main` after
   the script finishes.

Neither is implemented today — the security trade-off of
giving CI the signing key is a maintainer decision.

## Backfilling appcast entries for already-shipped releases

If a release exists on GitHub but isn't in `appcast.xml` yet,
`make-release.sh --publish` is idempotent — it picks up the
existing release via `gh release view`, signs the zip you
build locally, and appends the appcast entry. The asset
gets `--clobber`'d so the on-GitHub artifact is replaced
with the one you just built (which is fine — the version
inside matches the tag).

**Caveat:** if the existing release's zip contains a bundle
with a wrong `CFBundleShortVersionString` (e.g. one of the
v0.2.0 / v0.2.1 releases shipped before this fix landed),
you can't just sign that zip — the bundle inside would
report the wrong version after install. Bump the crate
version and cut a new tag instead.

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
