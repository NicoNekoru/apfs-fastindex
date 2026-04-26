# SR-012 Format Drift And Feature Allowlisting

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-08
- RL-12
- RL-13

## Bottom line

The parser should use allowlisted feature and layout checks, not optimistic
best-effort parsing. APFS feature bits, volume roles, sealed/integrity metadata,
Fusion, dataless snapshots, encryption rolling, and newer file-extent trees must
be recorded early and rejected until the exact product mode supports them.

This review answers one question: how should native raw mode handle APFS format
drift?

## Evidence

### Spec

- Apple File System Reference exposes container and volume feature,
  read-only-compatible, and incompatible feature fields.
- Volume superblocks carry role, root tree, extent-reference tree, snapshot
  metadata tree, OMAP, and newer metadata-root fields that can change required
  parser work by mode.

### Observation

- JT Sylve's volume-superblock writeup identifies incompatible and
  role/feature-related flags for normalization, dataless snapshots, sealed
  volumes, volume groups, integrity metadata, and newer file extent trees.
- `apfs-fuse`, `libfsapfs`, `go-apfs`, `linux-apfs-rw`, and `dissect.apfs` each
  expose different unsupported states or version ceilings.
- `linux-apfs-rw` rejects unsupported checkpoint descriptor trees and has
  explicit feature checks during mount.
- `linux-apfs-rw` and JT Sylve enumerate concrete volume roles and feature bits:
  `SYSTEM`, `DATA`, `PREBOOT`, `RECOVERY`, `VM`, `BACKUP`, volume-group system
  inode-space support, case-insensitive and normalization-insensitive modes,
  dataless snapshots, encrypted-rolled state, incomplete restore, sealed volume,
  PFK, extent preallocation, and secondary filesystem roots.
- `linux-apfs-rw` exposes container flags such as software crypto and Fusion
  handling; it also treats Fusion tier mapping as a distinct block-address
  concern.
- Eclectic Light's incompatibility writeups show clone, sparse, snapshot, and
  cross-volume behavior diverging in ways that matter to user-facing claims.

### Hypothesis

- The product should have two levels of allowlisting: source-gate allowlisting
  before raw reads, and parser-feature allowlisting after the selected container
  and volume superblocks are decoded.
- Checkpoint-scanner support and namespace-parser support should be separate
  verdicts. A source may be safe enough to report checkpoint candidates while
  still blocked for namespace output by volume roles, feature bits, encryption,
  snapshot state, or unsupported root-tree layout.

## Allowlist Fields To Record

Source-gate facts:

- source class: detached image, raw device, mounted image, external volume,
  startup container, snapshot source, Fusion/multi-device
- mounted state
- privilege level
- encryption visibility/unlock state
- oracle availability for the requested mode

Container facts:

- block size and block count
- container UUID
- container flags and feature masks
- checkpoint descriptor layout: contiguous versus non-contiguous
- Fusion or tiered-device indicators
- container OMAP object ID and expected type

Volume facts:

- volume OID and UUID
- volume role
- volume group UUID
- volume feature, read-only-compatible, and incompatible masks
- volume flags and metadata crypto state
- root tree, extent-reference tree, snapshot metadata tree, OMAP, and any newer
  integrity/fext/secondary-root fields
- case and normalization mode
- snapshot/revert fields such as root-to-XID and revert-to-XID

Requested-mode blockers:

- raw v1 namespace mode rejects System/Data merged-root requirements, sealed
  system volume semantics, snapshot source semantics, unsupported encryption,
  unsupported OMAP/value flags, unsupported non-contiguous descriptor layouts,
  and unsupported feature bits that affect namespace or logical-size output.

## Open Limits

- The repo does not yet have a cross-macOS APFS feature corpus.
- Feature-bit meanings need exact constants in native code before enforcement.
- Some read-only-compatible features may be acceptable for checkpoint scanning
  but not for namespace output.
- The support matrix still lacks external encrypted and external unencrypted
  media cells.

## Decision impact

- `RL-13`: feature-bit allowlisting is a native parser requirement, not a UI
  warning.
- `RL-12`: performance measurements are invalid on sources outside the supported
  feature matrix.
- `EX-08`: support cells should add the allowlist fields above so future verdicts
  can distinguish checkpoint-safe, root-discovery-safe, namespace-safe, and
  product-supported sources.
- Exact next step: extend `EX-08` artifacts to record these field groups without
  broadening support; promote individual bits only after a focused probe
  validates them for the requested product mode.
