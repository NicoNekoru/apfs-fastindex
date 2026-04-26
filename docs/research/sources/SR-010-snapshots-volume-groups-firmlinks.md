# SR-010 Snapshots, Volume Groups, And Firmlinks

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-08
- RL-10
- RL-11
- RL-13

## Bottom line

Snapshots, sealed system volumes, System/Data volume groups, and firmlinks are
product semantics boundaries, not parser edge cases. Raw v1 remains one APFS
volume at one selected state; Finder-visible boot-root output requires a
separate mode and oracle.

This review answers one question: what boundaries keep raw v1 from accidentally
claiming modern macOS root semantics?

## Evidence

### Spec

- Apple documents Catalina's split System/Data volume model and Big Sur's signed
  system volume snapshot model.
- Apple developer security material says SSV uses APFS snapshots and verifies
  system content in the read path.

### Observation

- SwiftForensics and Eclectic Light document that firmlinks merge System and Data
  volume paths into a user-visible root that differs from walking one raw volume
  or naively walking both visible paths.
- Eclectic Light's snapshot writeup describes snapshots as point-in-time volume
  states with snapshot metadata, extent references, copied volume superblock
  state, and XID identity; snapshot operations require restricted entitlements.
- Carbon Copy Cloner's APFS volume-group notes summarize the product-level
  effect: Catalina introduced System/Data volume groups, Big Sur boots from a
  read-only immutable System snapshot, and firmlinks make the two volumes appear
  as one user-facing volume.
- Eclectic Light's volume-role notes highlight that roles and volume-group UUIDs
  live in APFS volume metadata, while `diskutil apfs list` exposes them as
  operational facts.
- `apfs-fuse` supports snapshot/sealed volume mounting but still lists firmlinks
  as unsupported.
- `libfsapfs` lists snapshots as unsupported.

### Hypothesis

- A native raw parser can inspect roles and flags as source-gate facts, but it
  should not synthesize merged-root semantics until a separate boot-root oracle
  exists.
- A future boot-root mode would need to join raw volume roles, volume-group IDs,
  firmlink tables, mounted snapshot identity, and user-visible API output. None
  of those are needed for raw single-volume namespace plus logical size.

## Open Limits

- No repo experiment compares raw System/Data volume output with Finder-visible
  root output.
- Snapshot source selection and mounted snapshot validation are unimplemented.
- Cryptex and newer macOS presentation layers remain unmodeled.
- The future boot-root oracle would need privileged/current-host facts and is
  therefore not a portable unit fixture.

## Decision impact

- `RL-11`: raw v1 output is one-volume output only.
- `RL-08`: snapshot-assisted online scanning cannot be assumed as a third-party
  pinning primitive.
- `EX-08`: record roles, volume-group IDs, snapshot/seal flags, and firmlink
  oracle availability as support-matrix facts.
- Exact next step: keep boot-root mode explicitly blocked until native
  single-volume parsing is stable, then design a separate boot-root oracle
  experiment that compares System/Data raw views to user-visible POSIX/API output
  and `/usr/share/firmlinks`.
