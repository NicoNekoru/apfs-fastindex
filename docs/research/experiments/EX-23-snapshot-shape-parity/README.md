# EX-23 Snapshot shape parity (live vs snapshot walk)

ID: EX-23
Title: Snapshot shape parity (live-directory walk vs snapshot-mount walk)
Date: 2026-05-16
Owner: Claude
Status: Executed (best-effort)
Result: `blocked_no_snapshots_at_all` (first run, 2026-05-16)
Related RLs:
- RL-06 Namespace Reconstruction
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

EX-23 tests the R2-B shape-parity claim: **for unchanged data, the
existing fallback walker produces byte-identical `NamespaceEntry` +
`DirectoryAggregate` rows whether it is pointed at the live volume
mountpoint or at a read-only snapshot mountpoint of the same
volume.** Per SR-020 the *snapshot create + mount* step is
entitlement-gated (root + `com.apple.developer.vfs.snapshot`), so
the scanner cannot stage its own snapshot inside an unprivileged
test process. The probe is therefore best-effort:

- It enumerates the snapshot inventory of every reachable APFS
  volume via the unprivileged
  `tmutil listlocalsnapshots <mount>` and
  `diskutil apfs listSnapshots -plist <volume>` commands.
- For every snapshot it finds, it attempts to discover whether
  the snapshot is already mounted at some user-readable path
  (e.g. `/Volumes/com.apple.TimeMachine.localsnapshots/...`) by
  scanning `mount(8)` output for a `snapshot=` flag.
- If an already-mounted snapshot is found and the user has the
  matching live mountpoint, the probe walks both with the
  existing Rust fallback (`apfs-fastindex-scan --mode fallback
  <path>`) and diffs the shape on the intersection of unchanged
  paths.
- If no snapshot is mounted (the common case on a clean dev
  workstation), the probe records the snapshot inventory it did
  see and exits with verdict `blocked_no_mounted_user_snapshot`
  plus the exact `sudo mount_apfs -s <name> <mountpoint>`
  command a privileged user could run to unblock it.

The verdict slug is therefore one of:

- `validated_snapshot_shape_parity` — at least one
  (live-mountpoint, snapshot-mountpoint) pair was diffed and
  every unchanged path matched.
- `shape_divergence` — at least one diff failed on an unchanged
  path; the failure is recorded per-path.
- `blocked_no_mounted_user_snapshot` — no snapshot was already
  mounted at a user-readable path; the inventory and the
  reproducer command are saved so a privileged user can re-run.
- `blocked_no_snapshots_at_all` — no snapshots are present on
  any reachable APFS volume (the sealed-system `com.apple.os.update-*`
  snapshot is explicitly skipped per SR-020).
- `oracle_inconclusive` — an unrecoverable error before the diff
  step.

A `blocked_*` verdict is **not** a failure. It is the expected
outcome on a clean unprivileged sandbox where no TM local
snapshot has been taken yet. The probe's job is to record the
inventory and the reproducer, not to coerce a snapshot into
existence.

## Question

- Does the existing fallback walker produce identical
  `NamespaceEntry` + `DirectoryAggregate` rows when pointed at a
  read-only APFS snapshot mountpoint and at the live mountpoint
  of the same volume, on the intersection of paths that did not
  change between the snapshot moment and the live walk?

## Hypotheses

- Hypothesis A `validated_snapshot_shape_parity`: yes. For every
  path that exists in both the live walk and the snapshot walk
  with the same `(entry_kind, file_id, logical_size,
  symlink_target)`, the rows match. Paths that exist only in one
  side (creations / deletions during the test window) are
  recorded but do not fail the verdict.
- Hypothesis B `shape_divergence`: at least one unchanged path
  diverges between the two walks. Most likely cause: a
  snapshot-specific quirk (sealed-system snapshot, mounted
  read-only-with-snapshot-only-extents) that exposes a
  field-level difference our walker does not yet account for.
  The probe records the divergent path and `(expected, actual)`
  pair so RL-11 / RL-13 can update.
- Hypothesis C `blocked_no_mounted_user_snapshot`: no candidate
  mounted snapshot was reachable. SR-020 documents this as the
  default state on a stock developer Mac.

## Environment

- macOS version captured in `artifacts/generated/environment.json`.
- Probe runs as the invoking user; **no `sudo` is ever
  invoked.** SR-020 records that `mount_apfs -s` needs root, so
  the probe must accept that mounting is the user's
  responsibility.
- Source: the host machine's mounted APFS volumes; no fixture
  `.dmg` is built because per SR-020 a fresh `.dmg` cannot be
  snapshotted by an unprivileged process.

## Oracle

- The live-volume walk *is* the oracle: by definition, a
  snapshot was a copy of the live state at the moment it was
  taken, so the snapshot walk must agree with the live walk on
  the intersection of unchanged paths.
- Paths that differ in `(entry_kind, file_id, logical_size,
  symlink_target)` between the two walks are real failures only
  if the underlying file has not changed between the snapshot
  moment and the live walk. The probe widens the "unchanged"
  set conservatively (intersection of paths, identical
  `(entry_kind, file_id)`).
- `allocated_size` is **not** part of the shape-parity oracle
  here. SR-019 / EX-22 record the per-file allocated number, but
  the fallback walker emits `Some(st_blocks * 512)` for files;
  a path that has not changed in content can still report a
  different `st_blocks` between snapshot and live if APFS
  rebalanced the inode's extents. R2-B's claim is namespace +
  logical-size parity, not allocation parity; the
  `allocated_size` diff is recorded as a diagnostic.

## Setup

1. Capture environment manifest including a list of all mounted
   APFS volumes (`mount`, `diskutil apfs list -plist`).
2. For every mounted APFS volume:
   - run `tmutil listlocalsnapshots <mountpoint>`
   - run `diskutil apfs listSnapshots -plist <volume-device>`
3. Build a list of `(volume-device, mountpoint, snapshot-name)`
   triples, skipping any snapshot whose name starts with
   `com.apple.os.update-` (SR-020: sealed-system OS-update
   snapshots are out of scope) and any volume that the running
   user cannot `os.access(...)` for read.
4. For every triple, search `mount(8)` output for a line that
   names the snapshot as part of an already-mounted entry. If
   found, that snapshot is the candidate.
5. For every candidate, run the Rust fallback walker twice:
   `apfs-fastindex-scan --mode fallback <live-mountpoint>` and
   `apfs-fastindex-scan --mode fallback <snapshot-mountpoint>`,
   capturing both outputs verbatim.
6. Diff the two outputs on the intersection of `entry.path`s.

## Probe Steps

1. Capture environment + APFS-volume inventory.
2. For each volume, list snapshots with both APIs and union the
   results.
3. Parse `mount(8)` output to find any snapshot mountpoints.
4. If zero candidates: emit `blocked_no_snapshots_at_all` or
   `blocked_no_mounted_user_snapshot` with the inventory + a
   ready-to-paste `sudo mount_apfs -s` reproducer command (one
   per discovered snapshot).
5. If one or more candidates: run the Rust scanner against each
   pair, normalise the output, diff `(entry_kind, file_id,
   logical_size, symlink_target)` per path, and verdict.

## Expected Observations

### If Hypothesis A is true

- For every (live, snapshot) pair tested, every path in the
  intersection has identical `(entry_kind, file_id,
  logical_size, symlink_target)`. Per-path
  `allocated_size` may differ; recorded as diagnostic.

### If Hypothesis B is true

- At least one unchanged path diverges. The probe records the
  divergent path, the snapshot identity, and the field that
  diverged.

### If Hypothesis C is true

- The probe records every snapshot it could see (TM local +
  APFS-native named) but found none already mounted at a
  user-readable path. The summary includes a reproducer
  command like
  `sudo mount_apfs -s com.apple.TimeMachine.YYYY-MM-DD-HHMMSS.local
  / /tmp/apfsfi-ex23-snapshot-mountpoint` so a privileged user
  can rerun without re-deriving the snapshot identity.

## Observed Results

First-run verdict (host inventory captured 2026-05-16):
`blocked_no_snapshots_at_all`.

The probe enumerated 9 mounted APFS volumes on the host (`/`,
`/System/Volumes/{VM,Preboot,Update,xarts,iSCPreboot,Hardware,Data}`,
`/Volumes/inkcal.sh 1.0.0-arm64`). Of those:

- `/dev/disk3s1s1 -> /` reported **one** snapshot via
  `diskutil apfs listSnapshots -plist`:
  `com.apple.os.update-5514FF97DEE9C60C7FBF462B06A418D5FC4A882D5AF41D2BAF0A1419FB9B9F86`,
  XID 2293965, Purgeable=No. SR-020 records this as the
  sealed-system OS-update snapshot, which the probe skips by
  name prefix.
- Every other volume reported zero snapshots in both
  `tmutil listlocalsnapshots` and
  `diskutil apfs listSnapshots`.

After the sealed-system filter, the probe had zero user-visible
snapshots anywhere on the host. The probe therefore exited
`blocked_no_snapshots_at_all` and saved the inventory for a
future privileged rerun (the reproducer block in `summary.json`
is empty because there is nothing to mount; to unblock,
`tmutil localsnapshot` must first be run on any TM-included
volume to produce a `com.apple.TimeMachine.*.local` snapshot,
*then* `sudo mount_apfs -s ...` can mount it).

This is the expected outcome on a clean developer workstation
that has not been used as a TM-included machine. The probe is
intentionally re-runnable: when a snapshot becomes available
(either because the user took one, or because a privileged
caller created one), the probe will detect and diff it
automatically.

## Artifacts Saved

- `artifacts/probe_ex23.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex23-snapshot-inventory.json`
- `artifacts/generated/ex23-mount-table.json`
- `artifacts/generated/ex23-shape-diff.json` (only when a pair
  was diffed)
- `artifacts/generated/summary.json`

## Interpretation

- `blocked_no_snapshots_at_all` is the first-run verdict, and it
  is exactly the outcome SR-020 predicted for an unprivileged
  process on a dev workstation without prior TM-snapshot usage.
  The lane is not failing — it is correctly entitlement-gated.
- The Rust integration can still land. R2-B's product surface
  is a `--snapshot <mountpoint>` flag on the existing fallback
  scanner that consumes an already-mounted snapshot directory
  (the user owns the `sudo mount_apfs -s` step). The scanner's
  behaviour does not change; the flag is a label that tags the
  source as a snapshot in `correctness_claim` and `not_claimed`
  so the consumer can describe what they scanned. **The
  shape-parity claim** itself stays `not_claimed` until a
  privileged rerun of EX-23 produces a positive verdict; until
  then the flag accurately tells the user "this is a snapshot
  scan; I have not validated that it matches the live walk on
  this host."
- A future positive verdict would unlock:
  - moving the snapshot-mount cell of the RL-08 support matrix
    from `not_claimed` to `fallback_supported`;
  - adding a manual chapter on snapshot-assisted scanning; and
  - a separate probe class for snapshot-retained byte
    accounting (which requires R2-A's allocated-size column to
    converge first).
- If a future rerun produces `shape_divergence`, the most
  likely failure modes to investigate are:
  - paths emitted only in the snapshot walk because the
    snapshot mount exposes a different name normalization
    (SR-018);
  - `file_id` differences (APFS virtual OIDs are stable across
    snapshots, but the kernel may publish them via different
    `st_ino` values inside a snapshot mount);
  - symlink-target divergence (would indicate the snapshot
    reread the xattr differently).

## What This Rules Out

- Does not rule out divergence under encryption, sealed-system
  volumes, or non-APFS snapshot-like surfaces. Those are
  Gate-2.
- Does not validate snapshot-retained byte accounting (per SR-019
  + EX-22 that work needs a separate probe).
- Does not test snapshot mount lifecycle under churn (the probe
  reads, it does not race writes against the snapshot).
- Does not enumerate sealed-system OS-update snapshots
  (`com.apple.os.update-*`); SR-020 explicitly excludes them.

## Impact on RLs

- RL-11: a positive verdict closes the R2-B exit criterion
  ("EX-23 records shape parity between live-directory and
  snapshot-directory scans on an unchanged subtree"). A
  `blocked_*` verdict keeps R2-B open and parks the
  `--snapshot` Rust flag behind the unblock condition.
- RL-08: a positive verdict adds a new
  "mounted-snapshot directory" cell to the read-path support
  matrix as a `fallback_supported` source class.
- RL-10: the per-path shape diff becomes a regression artifact
  for any future change to the fallback walker.
- RL-13: a `shape_divergence` verdict would surface a
  fallback-walker bug; the probe's per-path failure record
  becomes the test case.

## Next Exact Step

- Land the `--snapshot <mountpoint>` Rust CLI flag in R2-B's
  Rust slice (it sugars over `--mode fallback <mountpoint>`
  plus a snapshot-source tag in `correctness_claim` /
  `not_claimed`). The flag is operationally useful immediately
  (it makes the snapshot scanning story documentable); the
  shape-parity claim itself stays in `not_claimed` until a
  privileged rerun of EX-23 produces
  `validated_snapshot_shape_parity`.
- A privileged user wanting to advance the lane should run:
  ```
  tmutil localsnapshot /                    # create snapshot (TM volumes)
  tmutil listlocalsnapshots /               # discover its name
  sudo mkdir -p /tmp/apfsfi-ex23-snap
  sudo mount_apfs -s <snapshot-name> / /tmp/apfsfi-ex23-snap
  python3 docs/research/experiments/EX-23-snapshot-shape-parity/artifacts/probe_ex23.py
  sudo umount /tmp/apfsfi-ex23-snap
  ```
- Open `EX-23b` if a positive rerun is later achievable, to
  validate the diff under multiple snapshot ages and across the
  System / Data volume pair.
