# EX-29 Local-snapshot extent-set contribution

ID: EX-29
Title: Local-snapshot extent-set contribution
Date: 2026-05-20
Owner: Claude
Status: Closed (host returned `blocked_no_user_snapshots`;
enumeration + harness landed in Rust).
Result: `blocked_no_user_snapshots`. The 2026-05-20 Apple
silicon host has zero user-visible TM local snapshots; only
1 sealed-system OS-update snapshot exists on `disk3s1s1`, which
SR-020 excludes from user-reclaimable accounting. EX-29's
reusable enumeration module + gated harness are wired so a
future host with TM snapshots — or a future macOS that unblocks
EX-28's Hypothesis C — produces the next verdict without code
changes.
Related RLs:
- RL-12 Snapshots and Time Machine local snapshots (TBD reference)
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
Related EXs:
- EX-27 clone-dedup via extent-reference tree (provides the extent-set machinery EX-29 reuses)
- EX-28 root mode + raw-on-live (needed to mount snapshots: `mount_apfs -s` requires root)
- EX-26 sparse/decmpfs (independent; lands first)
Related docs:
- `spec.md` SR-020 (snapshot creation entitlement; not relevant here — EX-29 reads existing snapshots)
- Chapter 11 of the manual ("The support matrix") — adds snapshot row
- Chapter 13 of the manual (architecture; new row in the status bar)
- `docs/research/plans/coverage-correctness-roadmap.md` Phase 4

## Bottom line

EX-28's verdict (`live_raw_blocked_by_kernel` on Apple silicon
under SIP, 2026-05-20) ruled out the original EX-29 plan's
raw-extent-set-diff path: `mount_apfs -s` produces a snapshot
device node whose raw reads are gated by the same kernel
security policy that blocks the live data partition. EX-29
therefore redesigns around oracles that are actually available
on a stock Apple silicon macOS host:

1. **Unprivileged snapshot enumeration** via
   `tmutil listlocalsnapshots <mount>` and the cross-check
   `diskutil apfs listSnapshots <mount>`. Both run without root.
2. **Sealed-system filter**: snapshots whose name matches the
   `com.apple.os.update-*` pattern are the OS-update boot
   snapshot — SR-020 excludes these from any "reclaimable"
   accounting because the user can't delete them and their
   bytes aren't user-visible.
3. **Bytes oracle for user-visible TM local snapshots**: no
   public read-only API surfaces a per-snapshot reclaimable
   byte count. `tmutil thinlocalsnapshots <mount>` deletes the
   snapshot and reports what was reclaimed — destructive, not
   suitable as an oracle.

The probe enumerates the host's snapshots, applies the SR-020
filter, and exits with one of three verdicts:

- `validated_snapshot_enumeration` (host has ≥1 user-visible
  TM local snapshot; enumeration produces the count + names
  for the future status-bar row, with bytes left unclaimed).
- `blocked_no_user_snapshots` (host has only sealed-system
  snapshots, or none at all; same shape EX-23 found on this
  host class — no user-reclaimable snapshot bytes to count).
- `probe_exception` (tmutil / diskutil unavailable, or output
  parse fails).

Surfaced in the app: a status-bar row counting user-visible
local snapshots. If a future macOS version or non-sealed disk
unblocks the raw-extent-diff path, EX-29's Rust harness
(analogous to EX-28's) can produce the reclaimable-bytes number;
until then, the row shows the count alone with an honest note
about the byte gap.

## Question

For a boot volume with at least one local snapshot from
`tmutil listlocalsnapshots /`, can the raw parser:

1. Mount the snapshot and walk its fs-tree to extract its extent set
   (a set of `(physical_block_addr, length)` pairs).
2. Compute `snapshot.extents - live.extents` to get the bytes unique
   to the snapshot.
3. Validate against `tmutil thinlocalsnapshots <date>` which reports
   what would be reclaimed if a snapshot were deleted.

## Hypotheses

- **Hypothesis A** `extent_set_diff_matches_tmutil`: The difference
  `snapshot.extents - live.extents`, summed by length, matches
  `tmutil thinlocalsnapshots` to within a small epsilon (`tmutil`'s
  estimate is itself approximate).
- **Hypothesis B** `extent_set_diff_is_authoritative`: The
  computed diff and `tmutil`'s estimate disagree by more than ε.
  EX-29 picks the raw-derived number as authoritative (it's
  ground truth from the on-disk extent sets); `tmutil`'s estimate
  is used as a sanity check.
- **Hypothesis C** `mount_apfs_unreliable`: `mount_apfs -s` either
  fails, requires specific entitlements EX-28's root path doesn't
  carry, or produces a fs-tree that the raw parser can't read
  cleanly. EX-29 falls back to reporting only the snapshot count
  + names; per-snapshot contribution stays unmeasured.

## Environment

- macOS version captured at probe time.
- Target: a developer machine with at least 3 local snapshots
  (`tmutil listlocalsnapshots /` returns 3+).
- Privilege: root (EX-28's path) required for `mount_apfs -s`.
- Cache state: cold for the validation runs.
- Out of scope: snapshot *creation* (SR-020 entitlement); encrypted
  containers; multi-volume containers' cross-volume snapshot
  arithmetic.

## Oracle

- **`tmutil thinlocalsnapshots <ISO date>`**: macOS-bundled estimate
  of reclaimable bytes if the named snapshot were deleted. Used as
  the primary oracle.
- **`df` "used" delta**: take `df` before and after deleting a
  snapshot (in a probe-controlled run on a dev machine), compare
  to EX-29's computed contribution.
- **Self-consistency**: sum of all snapshots' unique contributions
  plus the live volume's extents should equal the volume's total
  used (modulo container overhead).

## Setup (Python-direct + Rust enumeration)

The probe at `artifacts/probe_ex29.py` runs unprivileged and is
read-only. It enumerates `tmutil listlocalsnapshots <mount>` and
`diskutil apfs listSnapshots <mount>` for `/` and
`/System/Volumes/Data`, applies the SR-020 sealed-system filter,
and emits one of the verdicts listed above.

The Rust crate carries the same logic in
`crates/apfs-fastindex/src/snapshots.rs`:

- Public parsers `parse_tmutil_output(&str) -> Vec<SnapshotEntry>`
  and `parse_diskutil_output(&str) -> Vec<SnapshotEntry>`. The
  `SnapshotEntry` carries `name`, `user_visible` (the SR-020
  filter result), and optional `uuid` + `xid` for diskutil
  entries.
- `list_tmutil_snapshots` / `list_diskutil_snapshots` /
  `enumerate_mount` execute the underlying tool and parse.
- `classify(&[SnapshotEnumeration]) -> SnapshotVerdict` returns
  one of `Enumerated`, `NoUserSnapshots`, or
  `ToolingUnavailable`. Same shape as EX-28's `LiveScanOutcome`
  for consistency.

Tests:

- 8 unit tests in `snapshots::tests` covering the empty case,
  user-visible parses, the sealed-system filter, the actual
  on-host diskutil fixture (locked against the real string),
  multi-snapshot parse, and the three classifier variants.
- 2 integration tests in `tests/ex29_snapshot_enumeration.rs`:
  - `ex29_enumerate_host_snapshots`: runs unconditionally, cross-
    checks the SR-020 prefix rule against the classifier.
  - `ex29_mount_apfs_extent_diff`: gated on
    `APFS_FASTINDEX_EX29_SNAPSHOT_DEVICE` +
    `APFS_FASTINDEX_EX29_LIVE_DEVICE`; reuses EX-28's
    `LiveScanOutcome` classifier so EPERM cleanly records the
    EX-28 Hypothesis C verdict.

Outline:

1. Enumerate local snapshots via `tmutil listlocalsnapshots /`.
2. For each: mount via
   `mount_apfs -s <snap> /dev/diskNsM /Volumes/apfs-ex29-snap-N`.
3. Run the raw parser against the mounted snapshot's device node,
   extracting its extent set (reuses the EX-27 walker).
4. Compute set differences:
   - `snap.extents - live.extents` (unique to snapshot)
   - `live.extents - snap.extents` (unique to live)
   - shared = intersection
5. Cross-check against `tmutil thinlocalsnapshots <date>`.
6. Optional, dev-machine only: delete one snapshot, re-run `df`,
   compare reclaimed bytes against the EX-29 prediction.

## Probe Steps

Reproducible (~1 second, unprivileged):

```sh
python3 docs/research/experiments/EX-29-snapshot-contribution/artifacts/probe_ex29.py
cat docs/research/experiments/EX-29-snapshot-contribution/artifacts/generated/summary.json
```

Output on the 2026-05-20 host:

```json
{
  "sealed_system_excluded_count": 1,
  "user_visible_diskutil_count": 0,
  "user_visible_tmutil_count": 0,
  "verdict": "blocked_no_user_snapshots"
}
```

For a future verdict (user creates a TM local snapshot, or moves
to a non-sealed external disk), rerun the same probe — no code
change required.

## Verdict slugs

- `validated_snapshot_enumeration`: host has ≥1 user-visible
  TM local snapshot. The enumeration produces the count + names
  for the future status-bar row; reclaimable bytes stay
  unclaimed (no public read-only oracle; EX-28 Hypothesis C
  rules out the raw-extent-diff path on stock Apple silicon).
- `blocked_no_user_snapshots` **(landed 2026-05-20)**: host has
  only sealed-system snapshots, or none at all. Same shape EX-23
  found. The Rust enumeration is structurally correct but has
  nothing to surface.
- `snapshot_extent_diff_blocked_by_ex28_c`: a future host runs
  the gated `ex29_mount_apfs_extent_diff` test and hits the
  same `EPERM` that closed EX-28. The harness records the
  cross-experiment link and exits cleanly.
- `probe_exception`: tmutil / diskutil unavailable, or parse
  failed.

## Implementation deltas landed

- `crates/apfs-fastindex/src/snapshots.rs`: enumeration module
  with public parsers, classifier, and a `SnapshotVerdict` enum.
- 8 unit tests + 2 integration tests (one unconditional, one
  gated on `APFS_FASTINDEX_EX29_SNAPSHOT_DEVICE`).
- Manual chapter 11 records the new EX-29 row.
- `docs/research/000-research-index.md` registers EX-29 with the
  empirical verdict.

## Implementation deltas deferred

- FFI surface (`apfs_list_local_snapshots`, etc.) for the Swift
  app to render the status-bar row. Deferred to a UX commit;
  the Rust enumeration is ready when the UX work happens.
- Per-snapshot reclaimable bytes. No public read-only macOS
  oracle exists; EX-28 Hypothesis C blocks the raw-extent-diff
  alternative on this host class. The harness for the diff path
  is wired (`ex29_mount_apfs_extent_diff`); if a future host or
  macOS version unblocks it, the bytes column comes online with
  no code change.

## Risk / fallback

- `mount_apfs -s` may fail under various macOS configurations
  (sealed-system-volume / cryptex shape). Probe captures
  per-snapshot mount results so failures are visible.
- The extent-set diff is expensive: comparing two extent sets of
  ~5M extents each is O(N) with a hash set but RSS-heavy. We may
  cap the snapshot extent extraction to extents larger than a
  threshold to keep RSS bounded.
- If `tmutil thinlocalsnapshots` is consistently 5-10% off, that's
  acceptable; we record the divergence pattern and pick our
  number.

## Not in scope

- Snapshot creation (SR-020).
- Surfacing per-file "this file is also in N snapshots" in the
  treemap. That'd be a per-extent join; computationally heavy and
  unclear UX. Defer.
- Multi-volume-container arithmetic where snapshots span volumes.
