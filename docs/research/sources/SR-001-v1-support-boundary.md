# SR-001 V1 Support Boundary

Status: Complete
Date: 2026-04-24
Type: Source Review
Related RLs:
- RL-08 Live Volume, Encryption, and Read Path
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-13 Format Drift, Compatibility, and Fallback
- RL-07 Size and Space Accounting

## Bottom line

The outside evidence supports a narrow raw-mode v1:

- one APFS volume
- one coherent filesystem state
- correct namespace for that volume
- `logical size` as the canonical metric
- explicit fallback outside a tested allowlist

The same evidence does not support treating raw parsing as the default path for
live common-user startup disks on modern macOS.

## Scope decision

Recommended initial raw-mode scope:

- offline images
- mounted lab volumes with simple, validated layouts
- explicitly stable APFS views where consistency can be demonstrated

Recommended out-of-scope conditions for initial raw mode:

- Finder-like merged `/` semantics on boot volumes
- unsupported encryption environments
- unsupported snapshots or sealed-volume states
- unsupported APFS feature combinations
- claims of exact physical/exclusive/shared accounting

## Evidence

### Runtime and deployment boundary

- `Spec | Apple/public docs:` modern macOS startup storage is built around a
  system/data volume group, and macOS 11+ boots from a signed system volume
  snapshot rather than a plain writable root.
  Sources:
  - [Signed system volume security](https://support.apple.com/guide/security/signed-system-volume-security-secd698747c9/web)
  - [WWDC19: What's New in Apple File Systems](https://developer.apple.com/videos/play/wwdc2019/710/)

- `Spec | Apple/public docs:` FileVault and modern platform security make the
  startup environment materially different from an ordinary external APFS test
  volume. On Apple silicon and T2 systems, storage protection and boot policy
  are first-class constraints, not parser details.
  Sources:
  - [Volume encryption with FileVault in macOS](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/1/web/1)
  - [Apple Platform Security](https://help.apple.com/pdf/security/en_US/apple-platform-security-guide.pdf)

- `Observation | open-source implementation:` `apfs-fuse` supports snapshots,
  sealed volumes, and software-encrypted volumes, but still explicitly says
  firmlinks are unsupported.
  Source:
  - [apfs-fuse README](https://github.com/sgan81/apfs-fuse)

- `Observation | open-source implementation:` `libfsapfs` is explicit about a
  narrower feature set and lists snapshots, T2 encryption, and several
  compression modes as unsupported.
  Source:
  - [libfsapfs README](https://github.com/libyal/libfsapfs)

- `Hypothesis | inferred from converging sources:` raw mode should begin as a
  lab-grade feature with a hard allowlist, not as the assumed default runtime
  path for arbitrary macOS startup disks.

### Snapshot and entitlement boundary

- `Observation | Apple/public discussion:` snapshot creation/manipulation is not
  broadly available to third-party tools; `com.apple.developer.vfs.snapshot` is
  a special entitlement and snapshot reversion is effectively not a public
  third-party workflow.
  Sources:
  - [fs_snapshot_create entitlement discussion](https://developer.apple.com/forums/thread/89635)
  - [APFS snapshot revert discussion](https://developer.apple.com/forums/thread/768708)

- `Hypothesis | inferred from converging sources:` "create a stable snapshot on
  demand" is not a safe baseline assumption for consumer-facing raw mode.

### Namespace semantics boundary

- `Spec | Apple/public docs:` Catalina and later use a system/data volume group,
  and firmlinks are the mechanism used to present a unified hierarchy.
  Source:
  - [WWDC19: What's New in Apple File Systems](https://developer.apple.com/videos/play/wwdc2019/710/)

- `Observation | reverse engineering writeup:` the visible boot-root layout
  diverges from a raw single-volume walk, and scanning both `/` and
  `/System/Volumes/Data` naively will double-count content.
  Sources:
  - [macOS 10.15 Volumes and Firmlink magic](https://www.swiftforensics.com/2019/10/macos-1015-volumes-firmlink-magic.html)
  - [macOS Catalina Boot Volume Layout](https://eclecticlight.co/2019/10/08/macos-catalina-boot-volume-layout/)

- `Hypothesis | inferred from converging sources:` raw single-volume namespace
  and boot-root namespace should be treated as separate product modes with
  separate validation.

### Size semantics boundary

- `Spec | Apple/public docs:` APFS supports clones, snapshots, sparse files, and
  shared free space across volumes. Public APIs distinguish logical size from
  allocated size, but do not provide a simple public rule for clone- and
  snapshot-aware exclusive attribution that a raw parser can blindly mirror.
  Sources:
  - [About Apple File System](https://developer.apple.com/documentation/foundation/file_system/about_apple_file_system)
  - [getattrlist(2)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/getattrlist.2.html)

- `Observation | open-source implementation:` compression, clone handling, and
  snapshot-aware accounting support differ substantially between third-party APFS
  parsers.

- `Hypothesis | inferred from converging sources:` `logical size` is the right
  canonical v1 metric; anything broader must be introduced later as an explicit
  mode with its own oracle.

## Allowlist for initial raw mode

Enable raw mode only when all of the following hold:

- environment is in the validated support matrix
- parser can identify one coherent filesystem state
- requested scope is one APFS volume, not merged boot-root semantics
- required APFS features are recognized and supported
- fallback conditions are not triggered

## Fallback triggers

Fall back to supported APIs when any of the following is true:

- checkpoint or state selection is ambiguous
- unsupported incompatible features are present
- unsupported snapshot or sealed-volume state is encountered
- required runtime privileges or decryption assumptions do not hold
- merged-root semantics are requested
- tested compatibility is missing

## Decision impact

- `spec.md` should describe a narrow raw-mode v1, not a generic live-system raw
  parser.
- `RL-08`, `RL-11`, and `RL-13` should carry explicit allowlist and fallback
  language.
- Experiments should target isolated APFS images and explicitly compare "raw
  single-volume" semantics to safer oracles before the repo attempts broader
  claims.
