# RL-11 Snapshots, Volume Groups, and Firmlinks

Status: Open
Priority: P1
Owner: TBD
Last Updated: 2026-04-26

## Core Question
- What exactly are we indexing: a raw APFS volume, a snapshot, a volume group, or the user-visible merged namespace?

## Why This Matters
- Modern macOS filesystem presentation is not always a simple one-volume tree.
- Product semantics must be clear before UI and accounting choices are made.

## Current Assumptions
- A narrow v1 should target a single APFS volume namespace first.
- User-visible macOS layout on modern startup disks is a separate semantic mode,
  not something raw single-volume parsing should imply by default.

## Known Facts
- Snapshots exist and can preserve historical block references.
- Modern macOS uses system/data volume relationships.
- User-visible paths may not map 1:1 to raw volume internals.
- Firmlinks and related boot-volume presentation create a merged namespace that
  differs from a raw one-volume walk.
- Third-party APFS tooling commonly treats firmlink-aware presentation as a
  distinct and incompletely solved problem.

## Unknowns / Open Questions
- Should v1 ignore snapshots, surface them, or optionally index them?
- How do firmlinks affect apparent directory structure?
- Should `/` be represented as merged view or as underlying volume views?
- How do shared-container semantics affect "used space" reporting?
- What does a user expect from a WizTree-like APFS tool on macOS?
- What exact boot-root mismatches should be documented as expected in a raw
  single-volume mode?

## Risks if We Get This Wrong
- Results may be technically correct but feel wrong to users.
- Namespace and size totals may not match Finder expectations.
- The product may accidentally promise Finder semantics when it actually returns
  raw-volume semantics.

## Planned Experiments / Demos
1. Compare raw volume tree vs Finder-visible tree on a modern macOS install.
2. Create snapshots and observe accounting differences.
3. Examine system/data volume interactions in common paths.
4. Decide whether product mode should be "raw volume" or "OS-visible namespace."

## Evidence Log
- [TBD] Snapshot behavior notes.
- [TBD] Firmlink observations.
- [TBD] Volume-group namespace notes.
- [2026-04-24] `SR-001` concluded that raw single-volume namespace and
  Finder-visible boot-root namespace should be treated as separate product modes.
- [2026-04-26] `EX-08` matrix design keeps System/Data volume groups, signed
  system volumes, snapshots, and Finder-visible merged root out of raw v1 unless
  a separate product mode and oracle are defined.
- [2026-04-26] `EX-09` accounting design treats snapshot-retained bytes as a
  separate product/accounting semantic rather than part of raw v1 logical-size
  output.
- [2026-04-26] `SR-010` consolidated Apple, forensic, and open-source evidence:
  snapshots, SSV, System/Data volume groups, and firmlinks require separate
  product modes and oracles rather than expansion of raw single-volume v1.
- [2026-04-26] `SR-010` was tightened with future boot-root evidence
  requirements: roles, volume-group UUIDs, mounted snapshot identity, firmlink
  table interpretation, and user-visible POSIX/API output must be joined in a
  separate oracle before any Finder-visible root mode exists.

## Interim Decisions
- Keep boot-root semantics out of core parser design until raw single-volume
  semantics are stable.
- The default raw-mode v1 target is one APFS volume, not merged `/`.
- In support matrices, startup/System/Data and snapshot cells should be
  `fallback_required` for raw v1 rather than treated as parser failures. They
  are different semantic targets.
- Snapshot-retained space should not be attributed to files or directories in
  v1 logical-size mode.
- Native raw code may record volume role, snapshot, and seal indicators for
  source-gate decisions, but it must not synthesize Finder-visible root output
  until a boot-root oracle exists.
- A future boot-root experiment is blocked until native raw single-volume output
  is stable enough to compare as one input to a merged-root oracle.
- `/usr/share/firmlinks` and `diskutil apfs list` are candidate diagnostic
  inputs for that future mode, not v1 parser dependencies.

## Exit Criteria
- Explicit product scope statement.
- Chosen handling model for snapshots and firmlinks.
- UI/accounting impact documented.
- A documented comparison between raw single-volume output and boot-root output
  on a modern macOS system.

## Related Logs
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-08 Live Volume, Encryption, and Read Path