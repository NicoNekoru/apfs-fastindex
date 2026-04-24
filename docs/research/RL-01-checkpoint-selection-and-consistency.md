# RL-01 Checkpoint Selection and Consistency

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

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

## Interim Decisions
- A scan must never intentionally mix transactions.
- If consistency cannot be proven, fall back to safer supported methods.
- Checkpoint selection should be documented as an algorithm, not a heuristic.
- Live raw scanning must pin a chosen state; it cannot treat "latest while the
  volume keeps changing" as a valid correctness model.
- The first parser prototype should only target raw sources that can be pinned
  to one coherent state.

## Exit Criteria
- Defined algorithm for selecting scan XID.
- Defined validation rules for checkpoint chain integrity.
- Defined fallback behavior for ambiguous or damaged states.
- Reproducible scan consistency across repeated runs.

## Related Logs
- RL-02 OMAP and Object Resolution
- RL-08 Live Volume, Encryption, and Read Path
- RL-09 Cache Persistence and Invalidation