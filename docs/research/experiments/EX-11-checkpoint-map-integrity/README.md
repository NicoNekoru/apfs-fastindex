# EX-11 Checkpoint map integrity

ID: EX-11
Title: Checkpoint map integrity
Date: 2026-04-26
Owner: GPT-5.5
Status: Executed
Result: Generated detached proof fixture produced a validated checkpoint context
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

`EX-10` proved only a preliminary checkpoint candidate scanner. `EX-11` executed
the next checkpoint gate against a generated detached APFS proof fixture: the
selected checkpoint's checkpoint-map chain reached `CHECKPOINT_MAP_LAST`, and
four mapped ephemeral objects were read and checksum/type/XID validated from the
checkpoint data area.

Existing `EX-03`/`EX-04`/`EX-06`/`EX-07` artifacts preserve JSON oracles and
stale raw device paths, but no reusable detached image files are stored in the
repo. The positive run therefore generated a fresh detached image-backed proof
fixture, and the missing real-image reuse was recorded in
`artifact-inventory.json`.

## Question

- Can a detached APFS source produce a validated checkpoint-map/ephemeral-object
  context for the highest candidate `NXSB`, and what malformed cases must fail
  closed?

## Hypothesis

- Hypothesis A: For detached image-backed APFS proof sources, the selected
  checkpoint's checkpoint-map chain can be walked from `nx_xp_desc_index` through
  the descriptor circular buffer until `CHECKPOINT_MAP_LAST`, and each mapped
  ephemeral object can be read and checksum-validated from the checkpoint data
  area.
- Hypothesis B: Checkpoint-map validation exposes unsupported layouts, malformed
  rings, or ambiguous ephemeral-object state that must block native OMAP/root
  traversal.

## Environment

Recommended first environment:

- detached unencrypted APFS images from the existing proof loop
- synthetic block images for malformed ring cases
- no mounted-live source support
- no encryption or snapshot source support

Record:

- block size
- checkpoint descriptor base/count/index/length
- checkpoint data base/count/index/length
- selected `NXSB` block and XID
- checkpoint-map object count
- mapped ephemeral object count
- unsupported layout flags

## Oracle

Use two oracles:

- Positive detached-image oracle:
  existing proof images whose current raw walker can complete root discovery and
  namespace comparison after detach.
- Synthetic failure oracle:
  hand-built block fixtures for:
  - missing `CHECKPOINT_MAP_LAST`
  - invalid `cpm_count`
  - map chain wraps past descriptor limit
  - mapped object size is zero or not block-aligned
  - short read from checkpoint data area
  - bad ephemeral-object checksum
  - non-contiguous descriptor layout

This oracle is valid because the question is checkpoint integrity, not
namespace output.

## Setup

- Add an artifact script only when executing:
  `artifacts/probe_ex11.py` or an equivalent Rust/Python fixture runner.
- Save one JSON artifact per source or synthetic malformed case under
  `artifacts/generated/`.

## Probe Steps

1. Select the highest checksum-valid candidate `NXSB`.
2. Read descriptor/data ring fields from that selected `NXSB`, not from block
   zero.
3. Locate the first checkpoint map at `nx_xp_desc_index`.
4. Validate each checkpoint-map object header, type, checksum, flags, and
   `cpm_count`.
5. Enumerate mappings and advance through the descriptor circular buffer until
   `CHECKPOINT_MAP_LAST`.
6. For each mapping, read the mapped ephemeral object from the checkpoint data
   area, including data-ring wrap if supported by the probe.
7. Validate mapped object size, checksum, type/subtype, and XID relationship to
   the selected checkpoint.
8. Emit a verdict for the checkpoint context:
   - `validated_checkpoint_context`
   - `unsupported_non_contiguous_descriptors`
   - `malformed_checkpoint_map`
   - `bad_ephemeral_object`
   - `short_read`
   - `not_tested`

## Expected Observations

### If Hypothesis A is true

- Detached proof images produce a complete checkpoint-map chain ending in
  `CHECKPOINT_MAP_LAST`.
- Mapped ephemeral objects are readable and checksum-valid.
- The result can feed the next OMAP lookup probe as a validated checkpoint
  context.

### If Hypothesis B is true

- The probe identifies one or more hard-stop cases before OMAP/root traversal.
- Unsupported layouts are reported as explicit verdicts, not silently skipped.

## Observed Results

- Executed by `artifacts/probe_ex11.py`.
- Existing proof artifact inventory:
  - JSON artifacts found: `42`
  - reusable `.dmg`/raw image artifacts found: `0`
  - conclusion: prior proof routes provide oracles but not reusable raw media.
- Positive generated detached proof fixture:
  - verdict: `validated_checkpoint_context`
  - selected checkpoint XID: `14`
  - selected checkpoint descriptor index: `3`
  - checkpoint descriptor/data layout: contiguous descriptor area and contiguous
    checkpoint data area
  - checkpoint-map objects walked: `1`
  - checkpoint map ended with `CHECKPOINT_MAP_LAST`: yes
  - checkpoint mappings enumerated: `4`
  - mapped ephemeral objects validated: `4`
  - mapped object OIDs: `1024`, `1025`, `1027`, `1029`
  - observed mapped types: spaceman, object type `0x11`, and two physical B-tree
    objects with subtype `0x9`
- Synthetic malformed cases:
  - cases executed: `7`
  - expectations matched: yes
  - validated positive control: `validated_checkpoint_context`
  - missing `CHECKPOINT_MAP_LAST`: `malformed_checkpoint_map`
  - invalid `cpm_count`: `malformed_checkpoint_map`
  - zero mapped-object size: `bad_ephemeral_object`
  - unaligned mapped-object size: `bad_ephemeral_object`
  - bad ephemeral-object checksum: `bad_ephemeral_object`
  - non-contiguous descriptor layout: `unsupported_non_contiguous_descriptors`

## Artifacts Saved

- `README.md`
- `artifacts/generated/oracle-contract.json`
- `artifacts/probe_ex11.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/artifact-inventory.json`
- `artifacts/generated/generated-proof-fixture.json`
- `artifacts/generated/synthetic-malformed-cases.json`
- `artifacts/generated/summary.json`

## Interpretation

- The generated detached fixture proves the next native gate shape: candidate
  `NXSB` selection can be strengthened into a validated checkpoint context by
  walking checkpoint maps and validating mapped ephemeral objects.
- Native OMAP/root parsing remains blocked on an OMAP lookup proof, but no longer
  on the checkpoint-map gate for the generated proof fixture shape.
- `EX-10` output remains a candidate scan state, not a complete coherent scan
  state.
- Existing `EX-06`/`EX-07` identity artifacts cannot be directly consumed by
  `EX-12` because their raw images were not preserved.

## What This Rules Out

- It rules out wiring native OMAP lookup directly to highest-candidate-XID
  selection.
- It rules out treating non-contiguous descriptor layouts as a best-effort scan
  of nearby blocks.
- It rules out accepting malformed checkpoint-map chains or invalid
  mapped-object sizes/checksums as recoverable v1 states.

## Impact on RLs

- RL-01: defines the missing checkpoint-map validation gate.
- RL-02: prevents premature resolver work from consuming incomplete checkpoint
  state.
- RL-10: adds a feature-specific oracle for checkpoint integrity.
- RL-13: names checkpoint-map hard-stop verdicts.

## Next Exact Step

- Preserve or regenerate raw media for `EX-06`/`EX-07`, then run `EX-12` OMAP
  lookup validation against a validated checkpoint context and the corresponding
  identity oracle.
