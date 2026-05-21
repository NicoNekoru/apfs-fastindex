# Homebrew tap setup (one-time)

The `homebrew/` directory is a git submodule pointing at the
`homebrew-apfs-fastindex` tap repo. `make-release.sh --publish`
automatically updates the cask file there with the new version +
DMG SHA-256, commits, and pushes — so brew users see updates on
their next `brew update`.

This file walks the one-time setup needed before that auto-sync
can land.

## 1. Create the tap repo on GitHub

The repo name **must** be `homebrew-<tap-name>` for `brew tap`
to find it. Use the same `<tap-name>` we want users to type:

```sh
gh repo create NicoNekoru/homebrew-apfs-fastindex \
    --public \
    --description "Homebrew tap for the apfs-fastindex macOS app"
```

Leave it empty — no README, no license, no `git init` on
GitHub's side. We push the initial commit from the local
submodule.

## 2. Push the local submodule contents

The submodule was initialised at `homebrew/` with a starter
commit containing `Casks/apfs-fastindex.rb`. The remote is
already set to the tap-repo URL. Just push:

```sh
cd homebrew
git push -u origin main
cd ..
```

If `git push` errors with `non-fast-forward`, GitHub created
some default content on the remote — overwrite with
`git push -u --force origin main`.

## 3. Verify the tap works

```sh
brew tap NicoNekoru/apfs-fastindex
brew install --cask apfs-fastindex
```

`brew install --cask` downloads the .dmg from the GitHub
release, verifies the SHA-256 against the value in the cask,
mounts it, copies `ApfsFastindex.app` into `/Applications`,
and strips `com.apple.quarantine` automatically — no
right-click-→-Open Gatekeeper dance.

## What every release does automatically

Once steps 1–3 are done, `./make-release.sh --publish --tag
vX.Y.Z` runs the full pipeline:

1. Bumps `crates/apfs-fastindex/Cargo.toml` to `X.Y.Z` + lock.
2. Builds the bundle with that version.
3. Codesigns, zips, signs the zip with `sign_update`.
4. Generates a `.dmg` alongside the `.zip`.
5. Splices a new `<item>` into `appcast.xml`.
6. Commits Cargo.toml + Cargo.lock + appcast.xml in the main
   repo, tags `vX.Y.Z`, pushes.
7. `gh release create` uploads both `.zip` and `.dmg`.
8. **Updates `homebrew/Casks/apfs-fastindex.rb`** with the new
   version + DMG SHA-256, commits in the submodule, pushes the
   tap repo, then records the new submodule commit in the main
   repo's tree (via `git commit --amend` of the release commit
   + a single force-with-lease push).

Three update channels stay in lockstep from one command:
Sparkle's appcast, the GitHub release page, and the Homebrew
tap.

## Submitting upstream to homebrew/homebrew-cask later

When the project has a few stable releases under its belt:

1. Fork `homebrew/homebrew-cask` on GitHub.
2. Copy `homebrew/Casks/apfs-fastindex.rb` into the fork at
   `Casks/a/apfs-fastindex.rb` (Homebrew shards by first
   letter).
3. Run `brew audit --new-cask apfs-fastindex` locally — fix
   any lint issues it surfaces.
4. PR the change. Homebrew's bot auto-bumps the version on
   subsequent releases once the cask uses a Sparkle-style
   `version-anchored` URL.

Users will then be able to `brew install --cask apfs-fastindex`
with no `brew tap` step. Keep the self-hosted tap up to date in
parallel until the upstream merge lands.

## Failure modes

- **`make-release.sh --publish` says "tap submodule not
  initialised — skipping".** The `homebrew/` directory isn't a
  git repo (or `Casks/apfs-fastindex.rb` is missing). Re-run
  this doc from step 1.
- **Cask install fails with "no matching SHA-256".** The
  release-asset DMG was re-uploaded or the cask was edited
  out-of-band. Compare `shasum -a 256` of the downloaded DMG
  with the cask's `sha256` field; rerun `--publish` to refresh.
- **`brew install --cask` complains "App is from an unverified
  developer".** Should not happen with brew because it strips
  quarantine — but if it does, the cask is missing a
  `quarantine: false` directive (we don't need it for ad-hoc
  signed apps in a brew install; verify the cask matches the
  template).
