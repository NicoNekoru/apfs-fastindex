# RL-08 Live Volume, Encryption, and Read Path

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-05-14 (EX-21)

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
- [2026-04-24] `EX-01` found that the startup container on this machine blocked
  unprivileged raw reads of `/dev/rdisk3`, while a mounted image-backed APFS lab
  volume remained probeable. That supports a narrow initial raw-mode allowlist.
- [2026-04-25] `SR-004` consolidated runtime evidence into a support matrix:
  detached unencrypted image-backed sources remain the initial allowlist, while
  live startup disks, unsupported encryption, snapshot entitlement assumptions,
  Fusion, and merged-root requirements are fallback or hard-stop conditions.
- [2026-04-26] `EX-05` showed that a mounted image-backed APFS lab volume can be
  raw-readable during churn, but that does not make it raw-supported. The
  current proof walker resolved latest state and produced output that matched
  neither the baseline nor final mounted oracle.
- [2026-04-26] `EX-08` designed the read-path support matrix and verdict
  vocabulary: `readable`, `parsable`, `validatable`, and `supported` are
  separate claims. Hardware-sensitive cells remain pending until media exists.
- [2026-04-26] `SR-010` reaffirmed that snapshots, sealed system volumes,
  System/Data volume groups, and firmlinks are product-mode boundaries, not
  parser edge cases for raw single-volume v1.
- [2026-04-26] `SR-011` made encryption a source-gate issue: FileVault,
  hardware-backed internal storage, encrypted OMAP values, and key rolling are
  fallback conditions until unlock/decryption/oracle behavior is explicitly
  tested.
- [2026-04-26] First `EX-08` safe-host execution tested three cells. Detached
  unencrypted APFS `.dmg` remained `supported` for narrow v1 proof work and
  matched oracle. Mounted unencrypted APFS `.dmg` was raw-readable and parsable
  but did not match the mounted oracle, so it remains
  `readable_not_supported`. The startup container unprivileged raw-read attempt
  failed with `Operation not permitted` on `/dev/rdisk3`, with FileVault,
  encryption, signed snapshot, sealed root, and volume-group facts recorded.
- [2026-04-26] `SR-012` and `EX-08` were tightened into explicit gate semantics:
  checkpoint-scanner-safe, checkpoint-context-safe, OMAP-root-safe,
  namespace-logical-size-safe, and product-supported are separate verdicts.
- [2026-05-14] Observation: `EX-21` lands the spec's fall-back side in
  `src/apfs_fastindex/fallback_traversal.py`. A POSIX traversal walks any
  mounted directory and emits the same `NamespaceEntry` +
  `DirectoryAggregate` shape as the Rust raw scanner. The probe verifies
  shape parity on the proof fixture (7/7 entries, 3/3 aggregates). Gate-2
  source classes (live boot, encryption, snapshot-assisted, boot-root
  merged) remain out of scope; the fallback skeleton today covers only
  the locally-mounted-directory cell.

## Interim Decisions
- Deployment constraints are first-class product constraints, not implementation
  details.
- Initial raw mode should target isolated, validated APFS environments first.
- Live common-user startup-disk support should default to fallback unless
  experiments prove a narrower safe path.
- Image-backed APFS volumes are valid early research targets because they let us
  probe raw semantics without assuming startup-disk viability.
- Snapshot-assisted online scanning should not be assumed as a generic
  third-party pinning primitive until entitlement and oracle constraints are
  proven.
- Distinguish `raw-readable` from `raw-supported` in every support matrix cell.
  Mounted lab images are useful probes, but v1 support remains detached or
  explicitly stable unless pinning is proven.
- Use the `EX-08` verdict vocabulary for future source-gate work:
  `supported`, `readable_not_supported`, `parsable_not_validated`,
  `fallback_required`, `blocked_privilege`, `blocked_unavailable_hardware`, and
  `not_tested`.
- Detached unencrypted images remain the native Rust target. Encrypted,
  mounted-live, startup, snapshot, and merged-root sources must not be accepted
  as raw-supported merely because some bytes are readable.
- Current safe-host matrix has only one supported raw cell: detached
  unencrypted image-backed APFS for narrow v1 proof work. Mounted image-backed
  APFS remains a probe cell, not product support, until selected-XID enforcement
  or a valid stable oracle exists.
- Future read-path cells must record source-gate, container, volume, and
  requested-mode blocker fields before broadening support.

## Exit Criteria
- Supported environment matrix.
- Required privilege model.
- Decision on raw-only vs hybrid strategy.
- Explicit raw-mode allowlist and fallback triggers.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-13 Format Drift, Compatibility, and Fallback