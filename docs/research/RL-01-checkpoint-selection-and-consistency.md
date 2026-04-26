# RL-01 Checkpoint Selection and Consistency

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-04-26

## Core Question
- How do we reliably choose a single valid APFS transaction/checkpoint (XID) as the scan root?
- How do we guarantee that all objects resolved during a scan belong to one coherent filesystem state?

## Why This Matters
- The entire correctness model depends on scanning a single consistent view.
- Incremental reuse is invalid if objects are accidentally mixed across transactions.

## Current Assumptions
- APFS checkpoints represent consistent transactional states.
- A scan should anchor itself to one XID and resolve all objects relative to that XID.
- If checkpoint selection is ambiguous or inconsistent, cache reuse must be disabled.
- Block 0 should be treated as a locator for checkpoint metadata, not as an
  unconditional source of current state.

## Known Facts
- APFS is copy-on-write and transactional.
- Superblock/checkpoint structures determine current filesystem state.
- A "latest" state exists, but the exact selection rules need to be confirmed.
- Apple and third-party parsers converge on "latest valid checkpoint" rather
  than "block 0 is current."

## Unknowns / Open Questions
- What is the exact algorithm for selecting the latest valid checkpoint?
- How do we validate checkpoint chain integrity?
- What are the failure modes on unclean shutdown?
- Can we safely raw-read a mounted volume while writes continue?
- Are there edge cases where "latest" visible metadata is not the correct scan root?
- Should we prefer a mounted snapshot over raw latest checkpoint for live scanning?
- Which checkpoint layouts or failure modes should be immediate fallback
  conditions in v1?

## Risks if We Get This Wrong
- Silent corruption in index results.
- False incremental reuse.
- Non-reproducible scans.
- Disagreement with system APIs and user-visible filesystem state.

## Planned Experiments / Demos
1. Compare checkpoint selection across cleanly unmounted and actively mounted volumes.
2. Generate filesystem churn during scan and verify whether chosen XID remains stable.
3. Simulate unclean shutdown in a VM and inspect checkpoint recoverability.
4. Compare raw parse results against a snapshot-based or API-based stable view.

## Evidence Log
- [TBD] Initial checkpoint parsing notes.
- [TBD] Mounted-volume consistency observations.
- [TBD] Crash-recovery observations.
- [2026-04-24] `SR-002` defined the current entry contract: use block 0 only to
  locate the checkpoint descriptor area, then choose the valid `nx_superblock_t`
  with the highest `xid` and reject malformed checkpoint state.
- [2026-04-24] `EX-01` showed that on a mounted APFS lab image the highest
  visible checkpoint `xid` moved from `2` to `10` across eight forced-sync
  mutations, confirming that "latest" is a moving target during live writes.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` formalized the v1 rule that a
  parser run must pin one chosen scan state for its full lifetime and must not
  treat a drifting live latest checkpoint as valid input.
- [2026-04-24] `EX-03` implemented that rule in practice: the mounted oracle was
  captured first, the image was detached to pin the raw state, and the detached
  image's highest visible checkpoint `xid` was `14` for the successful raw
  versus oracle comparison.
- [2026-04-26] `EX-05` tested a mounted image-backed raw walk while writes
  continued. The mounted image was raw-readable, but the current latest-state
  raw walker matched neither the baseline mounted oracle nor the final mounted
  oracle while checkpoint XID moved from `9` to `37`.
- [2026-04-26] `SR-005` sharpened the checkpoint-validation sequence: validate
  block zero as a locator, reject non-contiguous descriptor layouts until their
  range map is implemented, scan descriptor blocks for checksum-valid `NXSB`
  candidates, and treat checkpoint-map validation as the next native milestone
  before OMAP/root traversal claims.
- [2026-04-26] First `EX-08` safe-host execution added another mounted-source
  warning: a quiescent mounted image was raw-readable and parsable but did not
  match the mounted oracle with the current proof backend. Detached image-backed
  APFS remained the supported proof cell.
- [2026-04-26] `SR-005` tightened checkpoint selection into an implementation
  gate: read block zero as a locator, reject unsupported descriptor layouts,
  validate candidate `NXSB` magic/type/checksum, and choose the highest valid
  candidate `o_xid`.
- [2026-04-26] `EX-10` added the first native Rust checkpoint scanner boundary
  with a synthetic unit oracle for contiguous descriptor areas, short-read hard
  stops, and highest-valid-XID selection. A proof-fixture smoke run then
  exercised `.dmg` source gating against a detached APFS image and reported
  highest candidate XID `14` with four checkpoint candidates.
- [2026-04-26] `SR-013` split checkpoint work into two gates: candidate `NXSB`
  selection and checkpoint-map/ephemeral-object validation. The latter requires
  walking checkpoint-map objects from `nx_xp_desc_index` through the descriptor
  circular buffer until `CHECKPOINT_MAP_LAST`.
- [2026-04-26] `EX-11` was designed as the checkpoint-map integrity probe for
  detached proof images and synthetic malformed descriptor/data rings.
- [2026-04-26] `EX-11` executed on a generated detached APFS proof fixture after
  inventory confirmed older proof routes kept JSON oracles but no reusable raw
  images. The selected checkpoint XID `14` produced one checkpoint-map object
  with `CHECKPOINT_MAP_LAST` and four checksum/type/XID-validated mapped
  ephemeral objects.
- [2026-04-26] `EX-10` extended the Rust path to also walk the checkpoint map
  ring after candidate selection. On the proof fixture the scanner emitted
  `selected_checkpoint.checkpoint_map.last_flag_seen=true`, one map block with
  four mappings (spaceman, reaper, two B-tree ephemeral objects), and no
  validation gaps. An empirical correction was distilled into the code:
  `checkpoint_map_phys_t` blocks themselves carry an `OBJ_PHYSICAL` storage
  flag, not `OBJ_EPHEMERAL`. Checkpoint-map validation is now backed by a
  Rust unit-tested scanner plus an asserting probe.

## Interim Decisions
- A scan must never intentionally mix transactions.
- If consistency cannot be proven, fall back to safer supported methods.
- Checkpoint selection should be documented as an algorithm, not a heuristic.
- Live raw scanning must pin a chosen state; it cannot treat "latest while the
  volume keeps changing" as a valid correctness model.
- The first parser prototype should only target raw sources that can be pinned
  to one coherent state.
- A live mounted raw mode requires resolver-level enforcement of the selected
  XID for every object lookup or a stable snapshot/API oracle. Raw readability
  alone is not sufficient.
- The Rust/native checkpoint scanner may report candidate checkpoint discovery
  before full checkpoint-map validation, but that output must not be described
  as a complete coherent scan state until checkpoint maps and downstream object
  resolution are validated.
- The first native Rust claim is only candidate scan-state identification for
  allowlisted detached sources. It must not be described as OMAP/root discovery
  or APFS indexing.
- Non-contiguous checkpoint descriptor areas remain unsupported until a
  checkpoint mapping-tree probe exists.
- Native root discovery must wait for a validated checkpoint context, not merely
  a highest-candidate `scan_xid`.
- Checkpoint-map hard stops include missing `CHECKPOINT_MAP_LAST`, impossible
  `cpm_count`, descriptor/data ring wrap beyond limits, invalid mapped-object
  size, short reads, and ephemeral-object checksum failures.
- For the generated detached proof-fixture shape, checkpoint candidate selection
  can now be promoted to a validated checkpoint context. This does not yet prove
  OMAP lookup or namespace traversal.

## Exit Criteria
- Defined algorithm for selecting scan XID.
- Defined validation rules for checkpoint chain integrity.
- Defined fallback behavior for ambiguous or damaged states.
- Reproducible scan consistency across repeated runs.

## Related Logs
- RL-02 OMAP and Object Resolution
- RL-08 Live Volume, Encryption, and Read Path
- RL-09 Cache Persistence and Invalidation