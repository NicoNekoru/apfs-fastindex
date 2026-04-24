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
- External or image-backed APFS volumes are the safest initial raw-mode target.
- Live startup-disk raw parsing should not be assumed viable just because APFS is
  documented.
- A hybrid strategy is likely, with raw mode limited by an explicit allowlist and
  supported-API fallback outside that boundary.

## Known Facts
- APFS volumes may be encrypted.
- Live systems introduce permission, consistency, and supportability concerns.
- Modern macOS startup storage is not a plain single-volume namespace; system and
  data volume behavior, snapshots, and sealed-system behavior matter operationally.
- Snapshot creation/manipulation is not a generally available third-party
  baseline due to entitlement restrictions.
- Third-party APFS parsers commonly support a narrower runtime matrix than
  "any APFS volume on any Mac."

## Unknowns / Open Questions
- Can we raw-read mounted APFS volumes without privileged helpers?
- How does FileVault affect access to metadata we need?
- Is scanning raw device data on a live system supportable and safe?
- What are the practical permission requirements?
- Is a snapshot-based system API path needed for online scans?
- Which environments belong in the initial raw-mode allowlist versus immediate
  fallback?
- Can a file-backed APFS lab image provide a sufficiently realistic first probe
  for consistency work before touching more constrained environments?

## Risks if We Get This Wrong
- The design works only in lab conditions.
- Product requires unacceptable privileges.
- Raw scanning fails on the most common user setups.
- The spec silently promises startup-disk behavior that should really be an
  advanced or unsupported mode.

## Planned Experiments / Demos
1. Unencrypted external APFS volume scan.
2. Encrypted APFS volume scan.
3. Internal system/data volume scan on current macOS.
4. Compare raw-device path vs API-assisted path for live consistency.

## Evidence Log
- [TBD] Privilege requirement notes.
- [TBD] Encrypted volume access notes.
- [TBD] Live mounted-volume behavior notes.
- [2026-04-24] `SR-001` narrowed the initial raw-mode target to offline or
  explicitly stable single-volume contexts and identified live startup-disk raw
  parsing as a support-boundary question rather than a default assumption.

## Interim Decisions
- Deployment constraints are first-class product constraints, not implementation
  details.
- Initial raw mode should target isolated, validated APFS environments first.
- Live common-user startup-disk support should default to fallback unless
  experiments prove a narrower safe path.

## Exit Criteria
- Supported environment matrix.
- Required privilege model.
- Decision on raw-only vs hybrid strategy.
- Explicit raw-mode allowlist and fallback triggers.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-13 Format Drift, Compatibility, and Fallback