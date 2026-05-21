# EX-32 APFS encryption survey: what we can learn

ID: EX-32
Title: Survey the publicly-documented + reverse-engineered
  knowledge of APFS encryption, enumerate the encryption state
  of the host's volumes, and map what's reachable from an
  ad-hoc-signed third-party app vs what requires Apple
  entitlements.
Date: 2026-05-21
Owner: Claude
Status: In-progress (kickoff)
Result: pending
Related RLs:
- RL-08 (read-path matrix; encryption falls in here)
- RL-11 (live raw — closed by EX-28, also gates encrypted-raw)
- Apple Platform Security Guide (December 2024, P201, P204)
- _Apple File System Reference_ (Apple Developer, 2020) §
  Encrypted Volumes

## Why this exists

Apple ships per-file encryption + per-volume FileVault on
every Apple-silicon Mac. The *names* of the data structures
are documented in the _Apple File System Reference_:

- `apfs_superblock_t.apfs_keybag_loc`: physical location of
  the volume's encrypted keybag.
- `apfs_kb_locker_t` + `apfs_keybag_entry_t`: the keybag itself
  (encrypted wrapper around volume keys).
- `crypto_state_t`: per-file encryption state held in inode
  `XATTR`-equivalent fields.
- `wrapped_meta_crypto_state_t`: per-volume metadata encryption.
- `cp_key_class_t`: file protection class (NSFileProtection*).
- `INCOMPAT_ENC_ROLLED`: feature flag set during encryption
  rollover.

But the field-level semantics, the cryptographic primitives
applied at each layer, and the key derivation chain are
described only at a "this is how the system works" level —
the bit layouts of the keybag entries, the KDF input string
formats, the IV derivation per block, the relationship to the
Secure Enclave, etc., are not in the reference. Apple's
platform security guide has more on the cryptography but
even less on the on-disk format.

That gap is what makes this interesting. The disk-side
encryption layer is the **most opaque** part of APFS today,
and lots of what's "known" is third-party reverse engineering
(the `apfs-fuse` and `linux-apfs-rw` projects, blog posts from
the FileVault → APFS transition).

## Scope

This is a **research** experiment, not a feature. The
deliverable is documentation of what's known + a small probe
that enumerates the host's encryption state via public APIs.
No new app feature, no code that reads encrypted data — we
can't decrypt anyway without keys, and the raw extents are
EX-28-blocked under SIP regardless.

The kickoff splits into three phases:

### Phase A — Survey what's public

Document the on-disk structures relevant to encryption, with
references to Apple's docs + the open-source projects that
have done the reverse engineering. Goal: anyone reading the
codebase later can find the encryption corner of APFS without
re-doing the literature dig.

Topics for this phase:

1. **The keybag.** `apfs_keybag_loc` is a `prange_t`
   (block_start + block_count) on the volume superblock,
   pointing to one or more APFS objects of type
   `OBJECT_TYPE_MEDIA_KEYBAG`. Each is an
   `apfs_kb_locker_t` containing N `apfs_keybag_entry_t`
   records. Entries are typed by UUID:
   - `BAG_TYPE_WRAPPING_M_KEY`: the **volume wrapping key** —
     wraps the volume's actual master key, itself wrapped by
     either the user's password (FileVault) or the Secure
     Enclave's hardware-tied class key.
   - `BAG_TYPE_VOL_KEY`: the wrapped volume key, used when a
     volume is unlocked.
   - `BAG_TYPE_UNLOCK_RECORDS`: per-user unlock records — one
     per FileVault user, plus institutional recovery key,
     plus iCloud recovery key.
   - `BAG_TYPE_PASSPHRASE_HINT`: the password hint (plaintext).

2. **Per-file `crypto_state_t`.** Each inode has an entry in
   the FS-tree of type `OBJ_TYPE_CRYPTO_STATE` (0x7 in our
   `fs_record_body.rs:321` table). Body shape:
   - `cp_refcnt`: refcount.
   - `state`: a `wrapped_crypto_state_t` containing:
     - `refcnt`
     - `state.major_version`, `state.minor_version`
     - `state.cpflags` (protection mode flags)
     - `state.persistent_class` (`cp_key_class_t`)
     - `state.key_os_version`
     - `state.key_revision`
     - `state.key_len`
     - `state.persistent_key[]`: the wrapped per-file key.
   - The per-file key wraps the per-extent IV; the persistent
     key is wrapped by the class key.

3. **Class keys.** Each `cp_key_class_t` maps to a "class
   key" stored in the keybag. On Apple silicon, class keys
   are typically wrapped by hardware keys tied to the Secure
   Enclave, so even a user with the password can't extract
   the class key off-device.

4. **`INCOMPAT_ENC_ROLLED`.** Set while a volume is mid-
   transition between two encryption states (e.g., FileVault
   being turned on or being key-rotated). The rolling state
   itself is tracked in a separate object the superblock
   points to; we don't parse it today.

5. **Per-volume metadata encryption.** `wrapped_meta_crypto_state_t`
   on the volume superblock describes how the FS-tree itself
   is encrypted on disk. Metadata is AES-256-XTS with a key
   derived from the master key + the volume UUID. (This is
   why even raw block reads of an encrypted volume return
   ciphertext — no plaintext FS-tree without the key.)

6. **`APFS_FS_ONEKEY`.** A historical mode where all files in
   a volume share one encryption key. Rare on modern macOS;
   most volumes use per-file keys. The flag is `0x8` in our
   `volume.rs:20` constants.

7. **`APFS_FS_UNENCRYPTED`.** Set on data volumes during the
   1.5-day window between OS installation and the user
   enabling FileVault. After that, the bit clears and
   encryption is on.

### Phase B — Probe the host

A Python probe (`probe_ex32.py`) enumerates the user's
volumes via `diskutil apfs list -plist` and `diskutil info`,
records which are encrypted, which use FileVault, the
encryption-state-rolled flag if visible, and the keybag
locations (which are reported in plain text in
`diskutil apfs list` for legibility — Apple isn't hiding
them, they're just useless without the key).

Saves to `artifacts/generated/ex32_host_state_<date>.json`.

### Phase C — What's reachable + decision

For each on-disk structure in Phase A, the deliverable is
a one-line "reachable from this app: y/n, why". The decision
at the end: do we want the parser to grow encryption
understanding (decoding the unencrypted parts of the keybag,
labeling entries, exposing a "this volume is encrypted by X
mechanism" surface in the UI), or stay out of it entirely?

The honest answer is probably:

- **Surface encryption state in the UI** (volume is encrypted
  yes/no, with what mode if visible). Cheap, useful, no
  decryption involved.
- **Document the on-disk format** in the code comments + this
  doc so the next contributor doesn't re-do the work.
- **Defer actual decryption** to a separate ticket if/when
  the entitlement story changes. We can't read the encrypted
  blocks anyway (EX-28).

But that's a Phase-C decision; Phase A + B are
straightforward exploration.

## Out of scope

- **Decrypting anything.** No keys, no entitlement, no path.
- **Reading encrypted raw blocks.** EX-28 closed this.
- **Re-implementing FileVault.** Not even theoretically; the
  Secure Enclave is involved on Apple silicon and there is no
  public API to ask it to unwrap a class key for us.
- **Modifying the encryption state of any volume.** Read-only
  research. Never invoke `diskutil apfs encryptVolume` or
  similar from this experiment.

## References

Linkable today, archived in the artifact for resilience:

- _Apple File System Reference_ (Apple Developer, 2020):
  https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf
- _Apple Platform Security_ (December 2024 release):
  https://help.apple.com/pdf/security/en-us/apple-platform-security-guide.pdf
  §"Data Protection overview", §"Volume encryption with FileVault".
- `apfs-fuse` source — has practical decoding of keybag entries
  for unencrypted research images:
  https://github.com/sgan81/apfs-fuse
- `linux-apfs-rw` — closer to Apple's documented field names:
  https://github.com/linux-apfs/linux-apfs-rw

The probe records the URLs at run time so this README staying
in sync with Apple's evolving documentation isn't a hard
dependency.
