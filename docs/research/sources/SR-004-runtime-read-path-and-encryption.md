# SR-004 Runtime Read Path And Encryption Boundary

Status: Complete
Date: 2026-04-25
Type: Source Review
Related RLs:
- RL-08 Live Volume, Encryption, and Read Path
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

The external evidence supports a strict raw-mode support matrix:

- Detached APFS images and explicitly stable lab sources are the correct first
  raw-mode target.
- Mounted lab images are useful for controlled probes, but live "latest"
  checkpoint reads are not a correctness model.
- Live startup disks on modern macOS are a stacked runtime case involving
  System/Data volume groups, signed system volume snapshots, FileVault or
  platform encryption, permissions, and user-visible firmlink presentation.
- Open-source APFS readers repeatedly document feature ceilings around
  snapshots, firmlinks, Fusion, hardware-backed encryption, compression variants,
  and format drift.

Therefore v1 raw mode should remain an allowlisted expert/offline path. Common
live user systems should fall back to supported APIs unless a later experiment
proves a narrower safe raw path.

## Scope

This review answers one question:

- Which runtime sources should raw mode accept, reject, or reserve for targeted
  probes before broad product support is promised?

Out of scope:

- implementing encryption unlock flows
- snapshot creation or revert workflows
- boot-root merged namespace synthesis
- performance benchmarking

## Evidence

### Live startup storage is not a simple APFS volume

- `Spec | Apple/public docs:` macOS 10.15 introduced a read-only system volume,
  and macOS 11+ adds signed system volume protection using APFS snapshots. SSV
  verifies file data and metadata in the read path and can restore the prior
  system via APFS snapshots if an update fails.
  Source:
  - [Signed system volume security](https://support.apple.com/guide/security/signed-system-volume-security-secd698747c9/web)

- `Observation | reverse engineering / forensic writeup:` Catalina and later
  present System and Data volumes as one logical root using firmlinks. Tools that
  traverse raw volume data or both `/` and `/System/Volumes/Data` naively can
  double-count or produce output that does not match the user-visible root.
  Source:
  - [macOS 10.15 Volumes and Firmlink magic](https://www.swiftforensics.com/2019/10/macos-1015-volumes-firmlink-magic.html)

- `Hypothesis | inferred from converging sources:` raw single-volume namespace
  and Finder-visible boot-root namespace are separate product modes. Raw v1
  should not claim merged `/` behavior.

### Snapshot operations are not a general third-party pinning primitive

- `Observation | Apple/public discussion:` snapshot manipulation depends on
  privileged and Apple-granted entitlements such as
  `com.apple.developer.vfs.snapshot`; revert workflows involve private APFS
  entitlement behavior. This makes "create a stable APFS snapshot on demand" a
  poor baseline assumption for a consumer raw scanner.
  Sources:
  - [fs_snapshot_create entitlement discussion](https://developer.apple.com/forums/thread/89635)
  - [APFS snapshot revert discussion](https://developer.apple.com/forums/thread/768708)

- `Observation | field research:` APFS snapshots preserve a point-in-time volume
  state with snapshot metadata, extent-reference state, a volume superblock OID,
  and the last XID included. Snapshot deletion and retention affect extent
  accounting and require separate semantics.
  Source:
  - [APFS: Snapshots](https://eclecticlight.co/2024/04/08/apfs-snapshots/)

- `Hypothesis | project boundary:` snapshot-assisted online scanning may still
  be valuable, but it is not the v1 raw-mode foundation. It belongs behind its
  own entitlement, oracle, and support-matrix research.

### Encryption and platform security constrain raw access

- `Spec | Apple/public docs:` FileVault and platform security protect APFS data
  with keys tied to user credentials and platform hardware. On modern Macs, the
  startup environment is materially different from an unencrypted external APFS
  image.
  Sources:
  - [Volume encryption with FileVault in macOS](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/1/web/1)
  - [Apple Platform Security](https://help.apple.com/pdf/security/en_US/apple-platform-security-guide.pdf)

- `Observation | open-source implementation:` `apfs-fuse` supports software
  encrypted volumes and mounting snapshots/sealed volumes, but explicitly lists
  firmlinks and T2 hardware-encrypted volumes as unsupported.
  Source:
  - [apfs-fuse](https://github.com/sgan81/apfs-fuse)

- `Observation | open-source implementation:` `libfsapfs` supports some
  compression and encryption features but lists snapshots, Fusion, LZFSE and
  other compression methods, and T2 encryption as unsupported. Its issue
  discussions also note that APFS keeps evolving over time.
  Source:
  - [libfsapfs](https://github.com/libyal/libfsapfs)

- `Hypothesis | project boundary:` encryption should be a hard gate until the
  read path, unlock source, key state, and object decryption behavior are
  explicitly supported and tested.

### Format drift and feature gates are normal, not exceptional

- `Observation | open-source implementation:` independent readers support
  different slices of APFS. `apfs-fuse`, `libfsapfs`, `linux-apfs-rw`,
  `apfsprogs`, `go-apfs`, and `dissect.apfs` all expose different maturity
  levels, TODOs, unsupported states, or feature ceilings.

- `Observation | reverse engineering writeup:` volume superblocks expose feature
  and incompatible-feature flags for case behavior, dataless snapshots, rolled
  encryption, normalization-insensitivity, incomplete restore, sealed volumes,
  volume group behavior, integrity metadata, and newer file extent trees.
  Source:
  - [Volume Superblock Objects](https://jtsylve.blog/post/2022/12/13/APFS-Volume-Superblock)

- `Hypothesis | inferred from converging sources:` raw mode should use an
  allowlist plus hard-stop gates, not optimistic best-effort parsing. Unknown or
  unsupported feature bits are correctness risks.

## Support Matrix Draft

Initial raw-mode allowlist:

- Detached, unencrypted, image-backed APFS containers that match the tested
  layout from `EX-03` and `EX-04`.
- Explicitly stable APFS sources where one scan state can be selected and held
  for the full run.
- Mounted lab images only for experiments whose oracle and consistency question
  are documented.

Immediate raw-mode hard stops:

- checkpoint selection is ambiguous, malformed, or not pinnable
- non-contiguous checkpoint layouts or checkpoint-map details are required but
  unsupported by the parser
- object checksum, object header, XID, type, or subtype validation fails
- required OMAP context is ambiguous
- unsupported incompatible feature bits are present
- FileVault, T2, Apple silicon, hardware-backed, key-rolling, or otherwise
  unsupported encryption is required to read metadata
- Fusion or multi-device storage is required
- requested product mode is Finder-visible merged root rather than one raw APFS
  volume
- snapshot, sealed-volume, revert, dataless snapshot, or integrity-tree semantics
  are required by the target mode but unsupported
- parser version has no validated corpus for the source's APFS/macOS feature set

Fallback candidates:

- POSIX traversal for maximum compatibility
- bulk attribute APIs where they are faster and preserve correctness
- snapshot-assisted API workflows only if entitlements, privilege model, and
  oracle policy are explicitly proven

## Open Limits

- This review does not prove whether privileged raw reads of an external
  encrypted APFS volume can be productized.
- It does not prove whether a live mounted non-startup APFS volume can be pinned
  safely without detach.
- It does not define user-facing degraded-mode UI. It only defines technical
  fallback triggers.

## Decision impact

- `RL-08` should keep deployment/read-path constraints as first-class research,
  not implementation detail.
- `RL-13` should treat feature-bit support and environment support as allowlist
  gates.
- `RL-11` remains outside raw v1 because System/Data/firmlink output is a
  separate semantic mode.
- `spec.md` already reflects this boundary closely enough; no spec correction is
  required from this review.
