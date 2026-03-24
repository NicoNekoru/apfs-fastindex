# RL-08 Live Volume, Encryption, and Read Path

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

## Core Question
- Under what deployment conditions can we actually read APFS metadata raw and reliably?

## Why This Matters
- A theoretically correct raw parser may still be unusable on real macOS systems.
- Product viability depends on access method, privileges, and encryption realities.

## Current Assumptions
- External/unencrypted test volumes are easiest.
- Internal mounted encrypted volumes may impose additional constraints.
- A hybrid approach may ultimately be required.

## Known Facts
- APFS volumes may be encrypted.
- Live systems introduce permission, consistency, and supportability concerns.

## Unknowns / Open Questions
- Can we raw-read mounted APFS volumes without privileged helpers?
- How does FileVault affect access to metadata we need?
- Is scanning raw device data on a live system supportable and safe?
- What are the practical permission requirements?
- Is a snapshot-based system API path needed for online scans?

## Risks if We Get This Wrong
- The design works only in lab conditions.
- Product requires unacceptable privileges.
- Raw scanning fails on the most common user setups.

## Planned Experiments / Demos
1. Unencrypted external APFS volume scan.
2. Encrypted APFS volume scan.
3. Internal system/data volume scan on current macOS.
4. Compare raw-device path vs API-assisted path for live consistency.

## Evidence Log
- [TBD] Privilege requirement notes.
- [TBD] Encrypted volume access notes.
- [TBD] Live mounted-volume behavior notes.

## Interim Decisions
- Deployment constraints are first-class product constraints, not implementation details.

## Exit Criteria
- Supported environment matrix.
- Required privilege model.
- Decision on raw-only vs hybrid strategy.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-13 Format Drift, Compatibility, and Fallback