# R5 Coverage-Correctness Roadmap

Status: Active planning
Date: 2026-05-20
Scope: Close the headline gap between "bytes the scanner reports" and "bytes
the volume actually contains" by validating four new oracles in sequence.
Each lands as one EX-* experiment with a code change and a manual chapter
update; the sequence is ordered so each builds infrastructure the next one
reuses.

Related docs:
- `spec.md`
- `docs/research/000-research-index.md`
- `docs/research/plans/general-wiztree-for-any-mac-roadmap.md`
- `docs/research/experiments/EX-22-sr-019-alloced-size-precedence/README.md`
- Chapter 8 of the manual (size precedence; the section R5 amends most)
- Chapter 13 of the manual (architecture; R5 doesn't change the FFI shape
  but documents the new metric columns the app surfaces)

## Bottom Line

After R4 (UX polish, indexing perf, security r1/r2/r3 audit), the project's
correctness-first identity has a few visible-to-users gaps that the
parser-layer could close but currently fails closed on:

- **Sparse and decmpfs allocated size are unclaimed.** SR-019 step "regular +
  dstream + `INO_EXT_TYPE_SPARSE_BYTES` present → `None`" and "regular +
  `com.apple.decmpfs` xattr → `None`" both fail closed because the EX-22
  fixture didn't pin an oracle. Result: every macOS-shipped framework, every
  Xcode binary, every system cache reports `unclaimed` on the Allocated
  metric. That's most of the volume on a typical machine.
- **Clones double-count.** APFS clones share extents on disk; the scanner
  counts each clone instance at full size. A typical developer's `~` has
  10-20% of its byte total in clone relationships (Time Machine local
  snapshots, App Store updates, Photos library, Xcode caches, the macOS
  install itself). The scanner's "logical" is right; "allocated" over-
  counts by the cloned share. There's no per-extent refcount accessible via
  `getattrlist` — closing this needs raw-mode extent-reference-tree
  walking.
- **Raw mode hasn't been validated on a live system volume.** Today the raw
  parser runs against detached `.dmg` files only. Running it against
  `/dev/disk*` of the mounted boot volume requires root + an oracle-parity
  probe over multiple successive scans. Once unlocked, this is the
  infrastructure for clone-dedup and snapshot contribution on the user's
  actual machine.
- **Snapshots are invisible.** `tmutil listlocalsnapshots /` reports the
  snapshot names; the bytes they hold are part of `volume.used` but never
  appear in our walk. Users see "missing bytes" between scanner total and
  Finder's "Macintosh HD" capacity. Snapshot contribution is structurally
  the same problem as clone-dedup — extent set differencing.

This roadmap turns each of those four gaps into a numbered experiment that
lands a closing piece. The ordering optimises for shipping a visible
improvement at the end of each phase rather than batching the whole pack at
the end.

## Sequencing

| Phase | EX  | Scope                                              | Estimate | Visible delta                                                          |
| ----- | --- | -------------------------------------------------- | -------- | ---------------------------------------------------------------------- |
| 1     | 26  | Sparse + decmpfs allocated-size precedence         | ~1 week  | Allocated column populates for system frameworks, Xcode, decmpfs trees |
| 2     | 27  | Clone-dedup against detached `.dmg` (extent refs)  | 2-4 wk   | New "Real Bytes" metric (Logical / Allocated / Real); WizTree-class    |
| 3     | 28  | Root mode + raw-parser-against-live-system-volume  | ~2 wk    | Raw fast path runs on the user's actual disk, not just detached `.dmg` |
| 4     | 29  | Snapshot extent-set contribution                   | ~2 wk    | "N local snapshots: 12.4 GB reclaimable" surfaced in the GUI           |

Total ~8 weeks of work, every step shipping a manual update + a measured
delta against the existing fail-closed columns.

## Dependencies

```text
EX-26 (sparse/decmpfs)  --[independent]
EX-27 (clone-dedup)     --[needs the extent-reference-tree walker]
EX-28 (raw-on-live)     --[uses EX-27's parser; needs root + privileged subprocess]
EX-29 (snapshots)       --[reuses EX-27's extent-set machinery + EX-28's mount-with-root]
```

- EX-26 is independent. Lifts SR-019 step-by-step where the new oracle holds;
  no new parser surface required.
- EX-27 introduces the extent-reference-tree (`oxr_t`) walker. This is the
  reusable machinery for the rest of the cluster.
- EX-28 doesn't change parser correctness; it validates the existing parser
  runs cleanly against a live `/dev/diskNsM`. Requires root because non-
  removable disks aren't world-readable. The privileged-subprocess shape
  (`osascript ... with administrator privileges` spawning the CLI with
  `--format msgpack-stream`) is the simplest path; SMAppService is the
  longer-term shape if user-friction matters.
- EX-29 reuses EX-27's extent-set extraction; the snapshot-specific work is
  mounting `mount_apfs -s <snap> /Volumes/...` (root required) and
  differencing the snapshot's extent set against the live volume's.

## Out of scope for R5

Documented explicitly so the scope doesn't drift:

- **Snapshot creation.** R5 reads existing local snapshots. Creating new
  ones requires the DTS-issued private entitlement (`com.apple.developer.
  vfs.snapshot`); SR-020 already documents the gate. Out of scope until
  a separate licence track if ever.
- **Encrypted-volume runtime semantics.** Locked / FileVault-suspended
  containers stay unsupported. R5 assumes the raw parser path runs on an
  unlocked volume (boot or user); encrypted-at-rest is a separate cell of
  the support matrix.
- **Container-level overhead.** Spaceman, OMAP, checkpoint maps, reaper —
  the volume metadata that never appears as files. Surfacing this as a
  separate row would be honest but adds a UI surface (where does
  "container overhead: 23 MB" live in the treemap?). Defer.
- **Diff two scans.** Useful, mentioned in the long-form roadmap discussion,
  but unrelated to the coverage gap R5 is closing. Lands separately when
  scoped.

## Anticipated outputs at end of R5

By the end of phase 4:

- Manual chapter 8 documents three SR-019 cases that previously fail-
  closed (sparse → `alloced_size - sparse_bytes`; decmpfs xattr-stored;
  decmpfs fork-stored) with EX-26 as the validation oracle.
- The native renderer offers a third metric: Logical / Allocated / Real.
  Real is clone-deduplicated allocated bytes. Validated against
  `du -A <path>` on the fixture and via the raw parser on detached
  `.dmg`s.
- The app supports "Scan as administrator…" which spawns a privileged
  subprocess running the CLI; EX-28 validates that the raw fast path
  produces stable shape parity against the mounted-volume oracle over
  three successive scans on a live system disk.
- Local snapshots show as a row in the status bar with their unique
  contribution to disk usage. Validated against
  `tmutil listlocalsnapshots` + `mount_apfs -s` + extent-set
  differencing on the EX-29 fixture.

## Risk-and-fallback log

Per the project's research-discipline norms, each experiment may not
validate cleanly. The fallback for each:

- **EX-26 (sparse case fails to pin)**: the sparse hypothesis
  (`alloced_size - sparse_bytes`) has been *observed* by EX-22 to hold;
  EX-26 is mostly formalising that observation. Failure here would be
  surprising. If decmpfs is the harder case (likely), EX-26 ships the
  sparse lift alone and decmpfs stays fail-closed with a sharper
  diagnostic.
- **EX-27 (clone-dedup math diverges from `du -A`)**: the extent-reference
  tree gives us refcounts directly; the math is `Σ extent.length /
  refcount`. If this disagrees with `du -A` on the fixture, document the
  divergence (linux-apfs-rw / apfsprogs / macOS-write-path are known to
  disagree on related counts) and pick the macOS oracle.
- **EX-28 (live raw fails parity)**: the raw parser was developed against
  detached images; a live volume has concurrent writes during the scan
  window. If the checkpoint-selection logic doesn't stabilise across
  three successive scans (each ~108 s on `/`), the fallback is to require
  the user to unmount-and-remount as read-only first (similar to
  `vnodebench` discipline), or fall back to the fallback walker even
  with root. EX-28 is the validation gate, not a feature.
- **EX-29 (snapshot extent diff doesn't match `tmutil thinlocalsnapshots`'s
  reclaim estimate)**: `tmutil`'s estimate is itself approximate. If our
  number differs, document the formula and the divergence; the user-facing
  number should be ours (raw-derived) with `tmutil`'s as a sanity check.

## Status

- EX-26: **Planned** — methodology populated this turn (this commit).
- EX-27, EX-28, EX-29: **Planned (skeleton)** — Bottom Line + Question +
  Hypotheses captured this turn; Setup / Probe Steps populated when each
  experiment is executed.
