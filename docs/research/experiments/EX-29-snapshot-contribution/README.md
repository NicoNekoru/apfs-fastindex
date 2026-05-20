# EX-29 Local-snapshot extent-set contribution

ID: EX-29
Title: Local-snapshot extent-set contribution
Date: 2026-05-20
Owner: Claude
Status: Planned (skeleton)
Result: Pending
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

(Pending — skeleton.)

Time Machine creates local snapshots on the boot volume's data volume.
Their bytes are counted in `df`'s "used" and Finder's volume capacity
display, but the scanner walks only the live volume's fs-tree and so
those bytes appear as "missing" on the user's mental model. EX-29
mounts each local snapshot via `mount_apfs -s <snap> /Volumes/...`
(root required → depends on EX-28's root path), runs the raw parser
against the snapshot's fs-tree to extract its extent set, and
differences against the live volume's extent set to compute the
snapshot's *unique contribution* — bytes that exist only in the
snapshot and would be reclaimed if the snapshot were deleted.

Surfaced in the app as a status-bar row: "3 local snapshots:
12.4 GB reclaimable".

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

## Setup

(To be detailed in `artifacts/probe_ex29.py` once the methodology
lands.)

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

(Pending — populated when `artifacts/probe_ex29.py` is committed.)

## Verdict slugs

- `snapshot_contribution_validated`: Hypothesis A holds. New
  status-bar row lands.
- `snapshot_contribution_validated_with_divergence`: Hypothesis B.
  Raw-derived number is authoritative; manual documents the
  divergence from `tmutil`.
- `snapshot_contribution_blocked`: Hypothesis C. App reports
  snapshot count only; bytes stay unmeasured.

## Implementation deltas if validated

- New app surface: status-bar row counting local snapshots and
  summing reclaimable bytes.
- New parser code: snapshot enumeration via `mount_apfs -s` (or
  reading the snapshot metadata directly from the raw container
  if `mount_apfs` proves unreliable — TBD).
- New FFI: a `snapshot_summary` payload separate from
  `parser_output.entries`.
- Chapter 11 of the manual: new row in the support matrix.
- A new integration test (gated on root + the existence of local
  snapshots) asserts the EX-29 contribution matches `tmutil
  thinlocalsnapshots` within ε.

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
