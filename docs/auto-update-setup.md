# Auto-update setup (Sparkle 2 + GitHub releases)

The app uses [Sparkle 2](https://sparkle-project.org) for in-app
updates, with the appcast served from `appcast.xml` on the
`main` branch of this repo
(https://raw.githubusercontent.com/NicoNekoru/apfs-fastindex/main/appcast.xml).
Daily background check on app launch + manual
"Check for Updates…" under the application menu.

This document captures the one-time setup the maintainer
needs to do, and the per-release flow that's now baked into
`make-release.sh --publish`.

## One-time setup

Sparkle verifies every downloaded update against an EdDSA
public key embedded in the app's `Info.plist`. The matching
private key lives in the maintainer's Keychain. **Without
this setup, `make-release.sh --publish` will still upload the
GitHub release but skip the appcast update — auto-updaters
won't see the new version until the keys are in place.**

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

(Sparkle ≤ 1.x had a combined `sign_update --generate-keys`;
2.x split keygen into its own binary. Don't be fooled by old
docs.)

### 2. Generate the EdDSA keypair

```sh
app/.build/artifacts/sparkle/Sparkle/bin/generate_keys
```

`generate_keys` writes the private key into your login
Keychain (service `https://sparkle-project.org`, account
`ed25519`) and prints the public key to stdout, framed like:

```
Generating a new signing key. This may take a moment, depending on your machine.
A key has been generated and saved in your keychain. Add the `SUPublicEDKey` key to
the Info.plist of each app for which you intend to use Sparkle for distributing
updates. It should appear like this:

    <key>SUPublicEDKey</key>
    <string>VRrRWtzTzuBeo4dnQViq6ymvbmR+jMbnaRakpJnYmM8=</string>
```

### 3. Stash the public key in the repo

```sh
# substitute your actual key from step 2
echo -n "VRrRWtzTzuBeo4dnQViq6ymvbmR+jMbnaRakpJnYmM8=" \
    > app/sparkle-public-key.txt
```

(Use `echo -n` so there's no trailing newline; the build
script `tr`s newlines out either way, but a clean file is
easier to diff.)

The file holds the public key on a single line, no trailing
newline. `make-release.sh` reads it (whitespace-trimmed) and
splices it into `Info.plist`'s `SUPublicEDKey` on every build.

**Commit this file.** The public key is non-secret by design;
clients use it to verify downloaded updates. Losing it means
re-keying every existing user.

The private key in Keychain is the secret. Don't ever check
that in.

### 4. Verify the wiring

```sh
./make-release.sh
plutil -p app/ApfsFastindex.app/Contents/Info.plist | \
    grep -E "SUFeedURL|SUPublicEDKey|SUEnableAutomaticChecks"
```

Expected:

```
  "SUFeedURL" => "https://raw.githubusercontent.com/.../main/appcast.xml"
  "SUPublicEDKey" => "rs3HBd5jjyAtnQS+7g6XSnh+Itmwms2LP1mtRq8Zkho="
  "SUEnableAutomaticChecks" => 1
```

If `SUPublicEDKey` is empty, the file is missing or the
build script didn't pick it up. Sparkle treats an empty key
as "updates disabled" — safe, but no auto-updates happen.

## Per-release flow

Once the keys are set up:

```sh
./make-release.sh --publish --tag v0.2.0
```

The publish path now:

1. Builds the .app bundle, embedding the Sparkle Info.plist keys.
2. Codesigns the bundle (ad-hoc) including the bundled
   Sparkle.framework and its nested XPC/Updater helpers.
3. Zips the bundle via `ditto -c -k --sequesterRsrc --keepParent`.
4. Calls `gh release create / upload` to push the zip to the
   GitHub release tagged by `--tag`.
5. Runs `sign_update` on the zip — prints
   `sparkle:edSignature="…" length="…"`.
6. Reads the asset URL back from GitHub, builds a new
   `<item>` block, splices it into `appcast.xml`
   newest-first.
7. Prints the `git add` / `git commit` / `git push` line you
   need to run to make the appcast visible to existing
   installs.

So the maintainer's release ritual:

```sh
# bump version (Cargo.toml, APP_VERSION in make-release.sh)
./make-release.sh --publish --tag v0.2.0

# the script tells you to run this:
git add appcast.xml
git commit -m "release: v0.2.0"
git push
```

Existing users running the app will see the update on their
next daily background check (or instantly if they pick
"Check for Updates…" from the app menu).

## Testing the integration

Sparkle has a [test mode](https://sparkle-project.org/documentation/sandboxing/)
but for our purposes the simplest end-to-end check is:

1. Bump `APP_VERSION` to `0.2.0` (or whatever's next) in
   `make-release.sh`. Don't bump `Cargo.toml`'s crate version
   yet — that's not what Sparkle reads.
2. Run `./make-release.sh --publish --tag v0.2.0`. The
   appcast gets the new item.
3. Commit and push `appcast.xml`.
4. Install the .app via the old release tag's zip
   (downgrade) — `open` it and launch.
5. Click "apfs-fastindex" → "Check for Updates…". Sparkle
   should find v0.2.0, show release notes, prompt to update.

## Failure modes worth knowing

- **Empty `SUPublicEDKey` in Info.plist.** Sparkle silently
  refuses to install updates. Every check reports "up to
  date." Fix: ensure `app/sparkle-public-key.txt` exists and
  re-build.
- **`sign_update` not found.** SwiftPM didn't unpack Sparkle
  cleanly. Run `swift package clean` inside `app/` and
  rebuild.
- **dyld can't find `Sparkle.framework`.** Means
  `make-release.sh` didn't copy the framework into
  `Contents/Frameworks/`. Re-run; the script picks it up
  from `app/.build/<triple>/release/Sparkle.framework`.
- **"code signature … not valid for use in process: mapping
  process and mapped file (non-platform) have different
  Team IDs"** when launching. The hardened-runtime library
  validation is rejecting the ad-hoc-signed framework.
  `app/app.entitlements` already includes
  `com.apple.security.cs.disable-library-validation` to
  bypass this — if you stripped it, add it back.
- **"User-canceled" loop on the update prompt.** Sparkle's
  installer launches a privileged subprocess (Autoupdate) to
  replace the bundle. On ad-hoc-signed builds this can
  occasionally prompt for auth. Not a bug; expected.
