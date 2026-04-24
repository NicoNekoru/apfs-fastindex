# EX-01 Live checkpoint consistency and runtime boundary

ID: EX-01
Title: Live checkpoint consistency and runtime boundary
Date: 2026-04-24
Owner: GPT-5.4
Status: Complete
Result: Positive
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

A simple mounted APFS lab image is raw-readable and useful for controlled
experiments, but the latest valid checkpoint is a moving target under write
churn. On this machine, unprivileged raw access to the live startup container
failed with `Operation not permitted`, and the mounted root is already a sealed,
snapshot-backed, FileVault-involved volume-group environment. That is enough to
keep initial raw mode scoped to offline or explicitly stable contexts rather than
normal live startup-disk operation.

## Question

- Can a raw read of a mounted APFS volume remain coherent under concurrent
  writes, or must raw mode be limited to offline or explicit stable-state
  contexts?

## Hypothesis

- Hypothesis A: live APFS raw scanning is stable enough to be a reasonable
  default target, even on common-user systems.
- Hypothesis B: live APFS raw scanning can be useful in tightly controlled lab
  environments, but it still requires an explicit chosen state and should not be
  treated as the default path on common-user startup systems.

## Environment

- macOS version: `26.3.1 (a)` on this host
- Startup root environment:
  - root mounted at `/`
  - APFS snapshot-backed
  - sealed
  - FileVault present
  - APFS volume-group identifier present
  - unprivileged raw read of `/dev/rdisk3` failed
- Lab probe environment:
  - new 128 MiB APFS disk image created with `hdiutil`
  - mounted live
  - case-insensitive
  - image-backed, not encrypted

## Oracle

- For the live image probe, the oracle was a mounted-view filesystem walk of the
  same volume after each mutation.
- This oracle is valid for the specific question because the experiment is about
  whether the raw checkpoint anchor moves while user-visible content changes, not
  about validating a raw parser's reconstructed namespace.

## Setup

- Added a reproducible probe script:
  `artifacts/probe_ex01.py`
- The script:
  - collects host/runtime facts
  - tests raw read access to the startup container
  - creates and mounts a fresh APFS disk image
  - reads the container superblock and checkpoint descriptor area directly from
    the raw APFS container device
  - forces eight write/sync mutations
  - records the highest visible checkpoint `xid` and the mounted tree after each
    step

## Probe Steps

1. Read host runtime facts and try a raw read against the startup container.
2. Create and mount a fresh APFS lab image.
3. Resolve the mounted image's APFS container device and read checkpoint
   descriptors directly from the raw device.
4. Run eight forced-sync mutations:
   - create file
   - append
   - rename
   - create directory
   - move file into directory
   - create second file
   - append second file
   - delete second file
5. After each mutation, record:
   - highest visible checkpoint `xid`
   - mounted tree snapshot

## Expected Observations

### If Hypothesis A is true

- live mounted state should behave like a stable default target
- checkpoint anchor should not be an operational problem during ordinary churn
- startup raw access should at least look plausible as a default path

### If Hypothesis B is true

- the lab image should be good enough for controlled probes, but the highest
  visible checkpoint should move under write churn
- startup-disk raw access should hit permission or semantic barriers
- the right conclusion should be "pin a chosen state or fall back", not "read
  latest continuously"

## Observed Results

- Startup raw access:
  - attempt to read `/dev/rdisk3` failed with
    `PermissionError: [Errno 1] Operation not permitted`
- Startup root facts recorded by the probe:
  - mounted root is snapshot-backed
  - root is sealed
  - FileVault is involved
  - an APFS volume-group identifier is present
- Live mounted lab image:
  - initial highest checkpoint `xid`: `2`
  - final highest checkpoint `xid`: `10`
  - unique observed highest `xid` values:
    `2, 3, 4, 5, 6, 7, 8, 9, 10`
- Each forced-sync mutation advanced the visible latest checkpoint state on the
  mounted image.

## Artifacts Saved

- `artifacts/probe_ex01.py`
- `artifacts/generated/host-environment.json`
- `artifacts/generated/startup-diskutil-apfs-list.txt`
- `artifacts/generated/live-image-probe.json`
- `artifacts/generated/summary.json`

## Interpretation

- A mounted APFS volume can be probed raw in a controlled image-backed lab setup.
- That does not make "latest visible state" a stable scan root. The experiment
  shows that the latest checkpoint keeps moving during ordinary writes when the
  volume is mounted and active.
- The startup environment on this machine is already a stacked semantic/runtime
  case: sealed snapshot, volume group, and FileVault-related constraints.
- Combined with the raw device access failure, that makes live startup-disk raw
  scanning a bad default assumption for v1.

## What This Rules Out

- It rules out treating "read the latest checkpoint on a live system" as a
  stable default mode.
- It rules out assuming the common-user startup disk is a simple continuation of
  the lab image case.
- It rules out postponing runtime boundary work until after parser design.

## Impact on RLs

- RL-01: checkpoint selection must be an explicit pinning step, not a drifting
  "always latest" read model.
- RL-08: initial raw mode should target offline or explicitly stable contexts,
  not general live startup-disk support.
- RL-10: live-state probes need both a raw anchor observation and a mounted-view
  oracle.
- RL-13: raw-mode allowlist should exclude environments like this startup root
  unless future work proves a supported path.

## Next Exact Step

- Pair this runtime-boundary result with the required-record experiment so the
  repo can move forward on a narrow parser target: one raw APFS volume, one
  chosen stable state, correct namespace, logical size only.
