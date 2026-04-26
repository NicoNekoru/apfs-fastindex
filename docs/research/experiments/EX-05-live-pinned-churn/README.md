# EX-05 Live pinned churn

ID: EX-05
Title: Live pinned churn
Date: 2026-04-26
Owner: GPT-5.5
Status: Complete
Result: Negative for current live-latest proof path
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

A mounted APFS lab image remained raw-readable while writes continued, but the
current proof raw walker did not prove live pinned correctness. It resolves the
latest visible state rather than a selected historical XID, and the raw walk
captured during churn matched neither the baseline mounted oracle nor the final
mounted oracle.

Observed in the first run:

- baseline checkpoint XID: `9`
- live raw-walk start XID: `9`
- live raw-walk end XID: `37`
- final checkpoint XID: `37`
- raw walk error: none
- raw walk entry count during churn: `253`
- baseline mounted oracle entry count: `303`
- final mounted oracle entry count: `383`
- raw walk matched baseline oracle: `false`
- raw walk matched final oracle: `false`

This keeps live raw scanning outside the v1 allowlist unless the parser can
resolve every object against one chosen scan XID or use a stable snapshot/API
oracle.

## Question

- Can a mounted APFS lab image be raw-scanned from one chosen state while writes
  continue, or does live raw mode need a stronger pinning mechanism or fallback?

## Hypothesis

- Hypothesis A: A mounted APFS lab image can be raw-read during write churn and
  current raw output can still be validated against one stable state.
- Hypothesis B: Mounted raw reads are possible, but current latest-state raw
  traversal is not a correctness model for live scans because the scan anchor
  moves or cannot be tied to a valid oracle.

## Environment

- Host OS: recorded in `artifacts/generated/environment.json`
- Probe volume:
  - fresh 256 MiB APFS image
  - case-insensitive
  - unencrypted
  - mounted for the full probe
- Raw walker:
  - `EX-03` `go-apfs` raw walker
  - important limitation: resolves latest visible state; does not enforce a
    historical selected XID

## Oracle

Two mounted-view oracles were captured:

- baseline oracle before churn
- final oracle after churn settled

These are valid for testing whether the raw walk during churn happened to match
one named mounted state. They are not sufficient to prove a true historical-XID
pinned scan because the current raw walker cannot request the baseline XID for
every OMAP lookup.

## Setup

- Added `artifacts/probe_ex05.py`.
- Created a fresh mounted APFS image.
- Seeded `stable/` with `300` files and an empty `hot/` directory.
- Captured the baseline mounted oracle and checkpoint.
- Ran the raw walker against the mounted image's raw container while a background
  mutator created `80` files under `hot/`.
- Captured the final mounted oracle and checkpoint.

## Probe Steps

1. Create a fresh APFS image and mount it.
2. Locate the APFS container device and normalize it to `/dev/rdisk*`.
3. Seed the volume with `300` stable files.
4. Capture baseline mounted oracle and checkpoint.
5. Start the raw walk and concurrently mutate `hot/`.
6. Record checkpoint samples during mutation.
7. Capture final mounted oracle and checkpoint.
8. Compare the live raw walk to baseline and final oracles.

## Expected Observations

### If Hypothesis A is true

- The raw walk either matches the baseline oracle, the final oracle, or a clearly
  named stable oracle state.
- Checkpoint movement does not affect the ability to validate output against one
  pinned state.

### If Hypothesis B is true

- The mounted image is raw-readable, but checkpoint XID advances during the scan.
- The raw output does not have a valid named oracle, or the current harness cannot
  prove which state it represents.
- The result reinforces fail-closed raw mode for live mounted sources.

## Observed Results

- Checkpoint XID moved from `9` to `37` during the live raw scan/mutation window.
- Mutation checkpoint samples observed XIDs:
  `10, 11, 13, 15, 16, 18, 20, 21, 23, 25, 26, 28, 30, 31, 33, 35`.
- The raw walker completed without an error in `0.147` seconds.
- The raw walk emitted `253` entries.
- Baseline oracle had `303` entries.
- Final oracle had `383` entries.
- Raw output did not match baseline:
  - `49` baseline paths were missing from raw output.
  - no type/identity/size mismatches were observed for intersecting paths.
- Raw output did not match final:
  - `129` final paths were missing from raw output.
  - no type/identity/size mismatches were observed for intersecting paths.

## Artifacts Saved

- `artifacts/probe_ex05.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/baseline-oracle.json`
- `artifacts/generated/final-oracle.json`
- `artifacts/generated/live-raw-walk.json`
- `artifacts/generated/summary.json`
- `artifacts/generated/run.json`

## Interpretation

- Raw access to a mounted image-backed lab volume is operationally possible.
- The current proof walker is not a live pinned scanner. It resolves latest state
  and cannot prove that the output belongs to one selected baseline XID.
- The mismatch against both baseline and final oracles is enough to reject
  "mounted latest raw walk under churn" as a correctness model.
- The likely safe product choices remain:
  - detached/stable raw sources for raw mode
  - a native resolver that enforces one chosen XID for every object lookup
  - snapshot/API fallback for live mounted sources

## What This Rules Out

- It rules out treating current `go-apfs` latest-state walking as proof of live
  raw-scan correctness.
- It rules out broadening v1 raw mode from detached/stable sources to mounted
  live sources based only on raw readability.
- It does not rule out a future live mode with a true pinned-XID resolver or a
  valid snapshot-assisted oracle.

## Impact on RLs

- RL-01: reinforces that live raw scans must pin a selected state or fail closed.
- RL-08: mounted image-backed raw reads are useful for lab probes but not yet a
  supported product read path.
- RL-10: live-state validation needs a named pinned oracle; baseline/final
  mounted oracles are insufficient when the raw walker resolves latest.
- RL-13: support matrices should distinguish "raw-readable" from "raw-supported."

## Next Exact Step

- Keep v1 raw mode scoped to detached or explicitly stable sources.
- For any future live raw support, implement resolver-level `max_xid` enforcement
  and rerun this probe with a true historical-XID raw walker.
