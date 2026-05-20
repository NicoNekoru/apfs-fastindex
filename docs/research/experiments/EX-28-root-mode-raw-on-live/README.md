# EX-28 Root mode + raw parser on live system volume

ID: EX-28
Title: Root mode + raw parser on live system volume
Date: 2026-05-20
Owner: Claude
Status: Planned (skeleton)
Result: Pending
Related RLs:
- RL-01 Checkpoint Selection
- RL-08 Identity and Incremental Caching
- RL-10 Validation Corpus and Oracle
- RL-13 (TBD — privileged-subprocess shape)
Related EXs:
- EX-15 SR-014 checkpoint selection (the gate this experiment runs on a live volume)
- EX-27 clone-dedup (the parser surface this experiment validates against a live disk)
- EX-26 sparse/decmpfs (lands first; doesn't depend on raw mode)
Related docs:
- `spec.md` (SR-014; the live-volume support cell of the matrix)
- Chapter 10 of the manual ("Identity") — re-runs identity on a live disk
- Chapter 11 of the manual ("The support matrix") — adds a new row
- `docs/research/plans/coverage-correctness-roadmap.md` Phase 3

## Bottom line

(Pending — skeleton.)

The raw parser today runs only against detached `.dmg` images because
`/dev/disk*` for non-removable disks isn't world-readable. EX-28
validates the raw fast path against the live mounted system volume
(`/dev/diskNsM` for the boot volume), behind a privileged-subprocess
shape (`osascript ... with administrator privileges` spawning the
CLI). The validation is shape-parity across three successive scans:
the same fs-tree shape (entry count, name/identity, sizes) should
emerge from the raw path that the fallback walker produces from the
same mounted volume in the same scan window.

This is a validation experiment, not a correctness experiment — the
parser is unchanged. The deliverable is a documented "Scan as
administrator…" path in the app and an updated support-matrix row.

## Question

Running the existing raw parser against the live boot volume's
`/dev/diskNsM`, with concurrent writes happening during the scan
window:

1. Does SR-014 checkpoint selection stabilise across three successive
   scans? (Each scan picks *a* checkpoint; the parser fail-closes on
   any non-deterministic field. The three scans don't need to pick
   the same XID, but each scan needs to produce a self-consistent
   shape.)
2. Does the resulting fs-tree shape agree with the fallback walker's
   traversal of the mounted volume, modulo entries that came/went
   during the scan window?

## Hypotheses

- **Hypothesis A** `live_raw_parity`: For three successive raw scans
  of the boot volume (each ~108 s in the cold-cache fallback
  baseline; raw is faster), each produces a self-consistent shape
  (no SR-014 fail-closure), and the shape agrees with a fallback-
  walker scan of the same volume within the symmetric difference
  expected from concurrent writes (~10s of files for a 60-second
  window).
- **Hypothesis B** `requires_remount_readonly`: SR-014 fail-closes on
  the live volume because concurrent writes destabilise the
  checkpoint window. The fallback is to require the user to unmount
  the data volume and remount it read-only first, similar to
  `vnodebench`'s discipline.
- **Hypothesis C** `live_raw_unreliable`: Even with no obvious
  concurrent writes, parity diverges in ways the symmetric-
  difference accounting can't explain. EX-28 records the divergence
  pattern and the live-volume raw path stays unsupported; "Scan as
  administrator…" unlocks TCC paths but still uses the fallback
  walker rather than raw.

## Environment

- macOS version captured at probe time.
- Target: the boot volume `/`, accessed via the corresponding
  `/dev/diskNsM` device node.
- Privilege: scans run as root, spawned by an `osascript ... with
  administrator privileges` subprocess from the app or by `sudo`
  from the CLI for development.
- Cache state: cold (`purge` between runs).
- Out of scope for EX-28: snapshot extent contribution (EX-29);
  parser-side correctness deltas (EX-27 carries those independently).

## Oracle

- **Fallback walker on the same mounted volume** during the same scan
  window. Produces a `parser_output.entries` list that the raw
  output is compared against.
- **Three successive raw scans**: shape-parity across the three is
  the SR-014 stability test.
- **Symmetric-difference budget**: a small number of `~` or
  `/private/var/folders` paths that legitimately churn during a
  ~60s scan window. EX-28 sets the budget at <100 entries on an
  idle machine and tightens or loosens based on observed
  baseline.

## Setup

(To be detailed in `artifacts/probe_ex28.py` once the methodology
lands.)

Outline:

1. Build a privileged-subprocess harness that spawns
   `apfs-fastindex-scan` with the boot volume's device node as the
   raw source. For development, use `sudo` directly.
2. Run a cold-cache fallback-walker scan of the same volume as the
   baseline.
3. Run three successive cold-cache raw scans.
4. Compute:
   - Per-scan: did SR-014 fail-close anywhere?
   - Across scans: shape symmetric difference.
   - Against fallback: shape symmetric difference, attributing
     differences to either (a) write-window churn or (b) raw-vs-
     fallback divergence.
5. Emit `summary.json` with one of the verdict slugs below.

## Probe Steps

(Pending — populated when `artifacts/probe_ex28.py` is committed.)

## Verdict slugs

- `live_raw_validated`: Hypothesis A holds. The "Scan as
  administrator…" path runs raw; support-matrix row added.
- `live_raw_requires_readonly_remount`: Hypothesis B holds. The
  "Scan as administrator…" path documents the remount step, falls
  back to fallback walker if the volume isn't read-only.
- `live_raw_unreliable`: Hypothesis C. "Scan as administrator…"
  stays on the fallback walker. Raw stays detached-`.dmg`-only.

## Implementation deltas if validated

- New app surface: "Scan as administrator…" command, spawning a
  privileged subprocess. UI shape TBD — at minimum a menu item
  with a confirmation sheet.
- CLI: no change — already accepts a `/dev/disk*` path.
- Chapter 11 of the manual: new row in the support matrix
  ("Live boot volume, raw, root → validated by EX-28").
- A new Rust integration test (gated on root + the explicit
  opt-in env var) runs the three-successive-scans probe and
  asserts the parity within the symmetric-difference budget.

## Risk / fallback

- The privileged-subprocess shape is fragile: macOS may show a
  scary password prompt; SMAppService is the long-term shape but
  requires Developer ID signing. For EX-28 itself, `sudo` from
  the CLI is sufficient.
- Three scans of `/` at ~108s each is ~5 minutes of probe time;
  acceptable for a one-off validation.
- If Hypothesis B holds, the user-flow degrades to "remount as
  read-only first" — not a happy path. We may decide to ship
  with `live_raw_requires_readonly_remount` documented and not
  attempt the remount automatically.

## Not in scope

- SMAppService / proper helper-tool signing — that's a
  productisation step after EX-28 validates the shape.
- Encrypted-at-rest containers (locked / FileVault-suspended).
- Snapshot enumeration / extent diff — EX-29.
