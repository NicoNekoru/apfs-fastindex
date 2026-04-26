# SR-011 Encryption And Runtime Read Path

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-08
- RL-13

## Bottom line

Encryption support is a source-gate decision before it is a parser feature.
Detached unencrypted images remain the native Rust target; FileVault, hardware
backed internal storage, key rolling, encrypted OMAP values, and unsupported
software-encrypted sources require fallback until unlock, decryption, and oracle
behavior are explicitly tested.

This review answers one question: when may raw mode read encrypted APFS sources?

## Evidence

### Spec

- Apple Platform Security and FileVault documentation describe APFS volume
  encryption using AES-XTS, volume encryption keys, key encryption keys, Secure
  Enclave participation on Apple silicon/T2 Macs, and credential/recovery-key
  requirements.
- Apple says APFS volume and metadata contents are encrypted with volume keys on
  protected internal storage.

### Observation

- `apfs-fuse` supports some software-encrypted volumes and recovery-key/password
  flows, but lists hardware-encrypted T2 volumes as unsupported.
- `libfsapfs` supports some encryption but lists T2 encryption as unsupported.
- JT Sylve's OMAP writeup notes OMAP value flags for encrypted objects and crypto
  generation.
- `EX-01` observed unprivileged raw read failure against this host's startup
  container while image-backed lab sources remained readable.

### Hypothesis

- Raw v1 should not attempt to be an encryption product. It may later support
  explicitly unlocked software-encrypted external images, but only as a
  support-matrix cell with raw-read, checkpoint, OMAP, and oracle evidence.

## Open Limits

- No external encrypted APFS media has been probed in this repo.
- The parser does not implement keybag handling, decryption, or encrypted object
  reads.
- Hardware-backed internal encryption behavior is intentionally not simulated.

## Decision impact

- `RL-08`: distinguish raw-readable, decrypted-readable, parsable, validatable,
  and supported.
- `RL-13`: encryption flags and OMAP encrypted values are hard stops until the
  matrix has real evidence.
- Exact next step: execute the encrypted external-volume cell of `EX-08` only
  when matching media is attached; do not add encryption code to the Rust
  checkpoint scanner.
