# RL-11 Snapshots, Volume Groups, and Firmlinks

Status: Open
Priority: P1
Owner: TBD
Last Updated: TBD

## Core Question
- What exactly are we indexing: a raw APFS volume, a snapshot, a volume group, or the user-visible merged namespace?

## Why This Matters
- Modern macOS filesystem presentation is not always a simple one-volume tree.
- Product semantics must be clear before UI and accounting choices are made.

## Current Assumptions
- v1 may target a single APFS volume namespace.
- User-visible macOS layout may differ due to firmlinks and volume-group behavior.

## Known Facts
- Snapshots exist and can preserve historical block references.
- Modern macOS uses system/data volume relationships.
- User-visible paths may not map 1:1 to raw volume internals.

## Unknowns / Open Questions
- Should v1 ignore snapshots, surface them, or optionally index them?
- How do firmlinks affect apparent directory structure?
- Should `/` be represented as merged view or as underlying volume views?
- How do shared-container semantics affect "used space" reporting?
- What does a user expect from a WizTree-like APFS tool on macOS?

## Risks if We Get This Wrong
- Results may be technically correct but feel wrong to users.
- Namespace and size totals may not match Finder expectations.

## Planned Experiments / Demos
1. Compare raw volume tree vs Finder-visible tree on a modern macOS install.
2. Create snapshots and observe accounting differences.
3. Examine system/data volume interactions in common paths.
4. Decide whether product mode should be "raw volume" or "OS-visible namespace."

## Evidence Log
- [TBD] Snapshot behavior notes.
- [TBD] Firmlink observations.
- [TBD] Volume-group namespace notes.

## Interim Decisions
- Keep this out of core parser design until raw single-volume semantics are stable.

## Exit Criteria
- Explicit product scope statement.
- Chosen handling model for snapshots and firmlinks.
- UI/accounting impact documented.

## Related Logs
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-08 Live Volume, Encryption, and Read Path