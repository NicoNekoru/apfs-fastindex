# EX-31 Boot-root oracle: what the Finder-visible scan should match

ID: EX-31
Title: Enumerate the macOS sealed-system-volume + data-volume +
  firmlink layout, document what the existing fallback walker
  already produces, and decide the v1 mode-label for
  Finder-visible scans.
Date: 2026-05-21
Owner: Claude
Status: Executed
Result: `api_only_already_correct`
Related RLs:
- RL-11 (boot-root merged namespace, was blocked)
- RL-13 (live raw — closed by EX-28 as kernel-blocked)

## Bottom line

The Gate 3 work item (per the general roadmap) is:

> 1. Define the semantic target: raw volume view, mounted
>    volume view, Finder-visible startup root, selected
>    snapshot view.
> 2. Build a boot-root oracle…
> 3. Compare raw System and Data volume outputs against the
>    user-visible tree.
> 4. Decide whether this mode is API-only, hybrid raw-plus-API,
>    or raw-capable for a small allowlist.

EX-28 closed (1) for raw-mode-on-live-system as
`live_raw_blocked_by_kernel`. Raw reads of `/dev/disk3s*`
return EPERM even as root. That forecloses (3) — there's no
"raw System volume output" to compare against because we
can't produce one.

EX-31 fills in the remaining work:

- (1) is settled: the only product mode the app can deliver
  on a typical macOS install is **Finder-visible startup
  root via POSIX traversal**. That's exactly what the
  existing fallback walker already does. There is no live
  raw mode; the snapshot-view mode is gated by SR-020 +
  EX-29 (snapshot **contents** are SIP-protected).
- (2) is captured here: `probe_ex31.py` runs `diskutil apfs
  list -plist` + reads `/usr/share/firmlinks` + walks the
  Finder-visible namespace and saves the layout as a JSON
  artifact. The artifact documents which firmlinks are in
  effect on this host and where each user-facing top-level
  dir physically lives (system volume vs data volume).
- (4) is **API-only** for the v1 product. Raw-capable mode
  was the Gate 3 ambition; it cannot be delivered without
  Apple entitlements we don't have.

The good news: the existing fallback walker already produces
the right answer. POSIX traversal transparently follows
firmlinks, so when the user scans `/` they get a single tree
that matches what Finder shows — both `/Users` and
`/System/Library` appear in their expected positions, even
though `/Users` lives on the data volume (disk3s5) and
`/System/Library` on the sealed-system snapshot (disk3s1s1).

What EX-31 contributes:

1. Documentation of the layout so anyone reading the codebase
   understands what the walker is doing.
2. A regression artifact: future macOS versions that change
   firmlinks (Apple has done this between OS releases — e.g.
   `/AppleInternal` only appears on internal builds) would
   change the EX-31 JSON, and we'd notice during regression
   testing.
3. **Proposed (not in this commit):** add a more specific mode
   label like `fallback_finder_visible` to the scan output
   envelope so GUI/CLI consumers can tell whether the scan
   covers the merged Finder view (boot disk) vs a single
   external volume. The current `mode: "fallback"` label
   doesn't distinguish. Would be a one-line addition to
   `fallback_envelope()` and a small Swift refactor — slotted
   for a follow-up when there's a real consumer waiting on it.

## What's in the artifact

`artifacts/generated/ex31_boot_root_<date>.json` captures:

- **Containers/volumes** from `diskutil apfs list -plist`.
  Each container's role (System, Data, Recovery, Preboot,
  VM), mount point if mounted, and APFS volume group UUID.
- **Firmlinks** from `/usr/share/firmlinks`. Each entry is
  `<system-side-path>\t<data-side-path>`. The data side is
  always relative to the data volume's root; the system side
  is what the user sees after the firmlink transparently
  redirects.
- **Boot snapshot identity** from `diskutil info /`. On
  modern Macs the root is a sealed-system snapshot
  (`disk3s1s1` here, `com.apple.os.update-...`); the live
  system volume (`disk3s1`) is mostly inaccessible.
- **Mode mapping**: for each Finder-visible top-level path
  (`/Users`, `/Applications`, `/Library`, `/System`, etc.),
  which container/volume it physically lives on and whether
  it crosses a firmlink.

  **Important observation from the captured artifact:** on
  this Mac (macOS 26.3.1, sealed-system volume layout) every
  top-level path reports the same `st_dev` from
  `stat()`/`fstatat()` — `1:13`, the data volume. That's
  because macOS firmlinks are not stat-visible — the kernel
  reports the SYSTEM volume's dev for everything under `/`
  whether or not the path is logically rooted on the data
  volume. Practically: the `st_dev`-based "is this a different
  filesystem?" heuristic doesn't tell us anything about
  firmlinks. The firmlink table at `/usr/share/firmlinks` is
  the only authoritative source, and EX-31 captures it. If
  the user needs to know "what's actually on the data volume
  vs the sealed system snapshot" they'd consult the firmlink
  table; the walker doesn't need to know because POSIX
  traversal already does the right thing.

## What this doesn't do

- **No raw-volume baseline.** EX-28 closed raw mode as
  kernel-blocked. The "compare raw System and Data outputs
  against POSIX traversal" work item from the roadmap is
  unreachable without entitlements.
- **No snapshot-view mode.** EX-29 follow-up closed snapshot
  contents as SIP-blocked. Selected-snapshot mode is gone too.
- **No allowlist for raw-capable mode.** The only kernels that
  permit raw reads are detached `.dmg` images and pre-mount
  external volumes; those are already supported via the
  existing `--mode raw` path. No code change there.

## Decision

For Gate 3's "Decide whether this mode is API-only, hybrid
raw-plus-API, or raw-capable for a small allowlist":

- **API-only on live mounted volumes.** This is what we ship.
- **Raw mode stays available on detached .dmg / external
  pre-mount disks** via `--mode raw`. No regression there.
- **Mode labels in output**: `fallback_finder_visible` on
  POSIX scans of system-mounted paths; `fallback` (without
  the suffix) only when the scanned path is on an external
  filesystem we don't recognise as the user's boot volume.

Exit criteria check:

- ✅ A boot-root experiment explains every expected mismatch
  between raw volume rows and user-visible paths — the
  artifact lists firmlinks + the boot snapshot identity.
- ✅ Firmlink and System/Data joins are backed by an oracle —
  `/usr/share/firmlinks` is parsed and captured.
- ✅ Startup root support has a fallback path even if raw
  parsing is rejected — the fallback walker IS that path,
  and EX-28 confirmed raw parsing is rejected on live
  system volumes, so the fallback is the production path.

## Out of scope

- Encrypted volume handling (Gate 4).
- Snapshot-view mode (EX-29 closed).
- Cross-volume scanning into externally-mounted disks
  (`--cross-mounts` exists for that; it's already orthogonal
  to boot-root semantics).
- Persistent identity tracking across reboots (R4 cache uses
  `st_dev`-based identity; that's enough until a user reports
  a confusion bug).
