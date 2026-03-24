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

## Known Facts
- APFS is copy-on-write and transactional.
- Superblock/checkpoint structures determine current filesystem state.
- A "latest" state exists, but the exact selection rules need to be confirmed.

## Unknowns / Open Questions
- What is the exact algorithm for selecting the latest valid checkpoint?
- How do we validate checkpoint chain integrity?
- What are the failure modes on unclean shutdown?
- Can we safely raw-read a mounted volume while writes continue?
- Are there edge cases where "latest" visible metadata is not the correct scan root?
- Should we prefer a mounted snapshot over raw latest checkpoint for live scanning?

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

## Interim Decisions
- A scan must never intentionally mix transactions.
- If consistency cannot be proven, fall back to safer supported methods.

## Exit Criteria
- Defined algorithm for selecting scan XID.
- Defined validation rules for checkpoint chain integrity.
- Defined fallback behavior for ambiguous or damaged states.
- Reproducible scan consistency across repeated runs.

## Related Logs
- RL-02 OMAP and Object Resolution
- RL-08 Live Volume, Encryption, and Read Path
- RL-09 Cache Persistence and Invalidation