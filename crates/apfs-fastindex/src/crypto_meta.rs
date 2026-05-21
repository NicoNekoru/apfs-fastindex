//! Encryption-metadata decoders for the APFS container + volume
//! superblocks (EX-32 research scratchpad).
//!
//! Both superblocks already pass through our existing parsers
//! (`container::decode_container_summary`,
//! `volume::decode_volume_summary`) but we throw away the
//! encryption-related fields. This module is the staging
//! ground for parsing them. **Not yet wired into the production
//! `ContainerSummary` / `VolumeSummary` types**: the research
//! branch carries the parser + tests; the decision to surface
//! these fields in the FFI/serialised output happens after
//! Phase C of EX-32.
//!
//! ## What we decode
//!
//! Two container-superblock fields and one volume-superblock
//! field. All three contain **no plaintext keys** — they're
//! block-address pointers or algorithm-version metadata. The
//! actual wrapped keys live at the referenced blocks and are
//! encrypted under either the user's password (FileVault) or
//! a Secure Enclave hardware-tied class key (Apple silicon
//! Data Protection); neither is reachable from an ad-hoc-signed
//! app on macOS.
//!
//! ### `nx_keylocker` (container superblock, offset 0x510)
//!
//! 16 bytes = `prange_t` = (`pr_start_paddr: u64`,
//! `pr_block_count: u64`). Points at the container's effective
//! wrapping key — the key the OS unwraps the per-volume keys
//! with at unlock time. `(0, 0)` on an unencrypted container.
//!
//! ### `nx_mkb_locker` (container superblock, offset 0x570)
//!
//! 16 bytes, same `prange_t` shape. Points at the **media
//! keybag locker**, the container-level structure that holds
//! per-volume keybag metadata. On encrypted containers this
//! is non-zero; on unencrypted it's `(0, 0)`. The media keybag
//! itself is encrypted; we only know where it lives.
//!
//! ### `apfs_meta_crypto` (volume superblock, offset 0x60)
//!
//! 20 bytes = `wrapped_meta_crypto_state_t`. **Does not
//! contain a wrapped key** — just describes how the volume's
//! FS-tree metadata is encrypted: algorithm version, OS
//! version that wrote it, key revision, key length. The
//! actual metadata is encrypted block-by-block with a key
//! derived from the master key + volume UUID; this struct
//! is the algorithm-version pin, not the key.
//!
//! ## Byte layouts
//!
//! Derived from the _Apple File System Reference_ (Apple
//! Developer, 2020) and cross-checked against
//! `linux-apfs-rw`'s `apfs_raw.h`. Every offset here is in
//! the parsed block (after object-header validation).
//!
//! All multi-byte fields are little-endian (APFS is the same
//! endianness as macOS's native byte order, which is LE on
//! arm64 + x86_64).

use serde::Serialize;

use crate::block_io::{le_u16, le_u32, le_u64};

/// Offset of `nx_keylocker` inside the container superblock
/// block. The first 0x510 bytes are the `obj_phys` header +
/// the container-management fields up through `nx_fusion_uuid`.
const NX_KEYLOCKER_OFFSET: usize = 0x510;

/// Offset of `nx_mkb_locker`. Sits after `nx_keylocker` +
/// `nx_ephemeral_info[4]` (32 bytes) +
/// `nx_test_oid`/`nx_fusion_mt_oid`/`nx_fusion_wbc_oid`/
/// `nx_fusion_wbc` (40 bytes) + `nx_newest_mounted_version`
/// (8 bytes) → 0x510 + 16 + 32 + 40 + 8 = 0x570.
const NX_MKB_LOCKER_OFFSET: usize = 0x570;

/// Offset of `apfs_meta_crypto` inside the volume superblock
/// block. Sits at the start of the per-volume crypto state,
/// after the basic counts up through `apfs_fs_alloc_count`
/// (0x58). 0x58 + 8 = 0x60.
const APFS_META_CRYPTO_OFFSET: usize = 0x60;

/// `cp_key_class_t` values — the file-protection class IDs
/// referenced from per-file `crypto_state_t` records. Apple's
/// `cprotect.h` is the canonical reference; the values are
/// stable across macOS versions (changing them would break
/// every existing encrypted file).
pub fn cp_key_class_name(class: u32) -> Option<&'static str> {
    // Lower 5 bits carry the class; upper bits carry flags.
    // EFFECTIVE_CLASSMASK = 0x1f.
    match class & 0x1f {
        0 => Some("CPROTECT_CLASS_DIR_NONE"),
        1 => Some("CPROTECT_CLASS_A (NSFileProtectionComplete)"),
        2 => Some("CPROTECT_CLASS_B (NSFileProtectionCompleteUnlessOpen)"),
        3 => Some("CPROTECT_CLASS_C (NSFileProtectionCompleteUntilFirstUserAuthentication)"),
        4 => Some("CPROTECT_CLASS_D (NSFileProtectionNone)"),
        // Class F is internal-only ("kernel I/O") — used for
        // files the OS reads before the user unlocks the
        // device, like the keybag itself.
        6 => Some("CPROTECT_CLASS_F (internal / no protection)"),
        _ => None,
    }
}

/// Container-level encryption metadata. Read from the
/// `nx_superblock_t` block; safe to parse on any container
/// (the fields are zero on unencrypted ones, and zero is a
/// valid "no locker" sentinel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainerCryptoMeta {
    /// `nx_keylocker.pr_start_paddr`. The container's
    /// effective wrapping key lives at this block.
    pub keylocker_block_start: u64,
    /// `nx_keylocker.pr_block_count`. Usually 1 on encrypted
    /// containers; 0 on unencrypted.
    pub keylocker_block_count: u64,
    /// `nx_mkb_locker.pr_start_paddr`. The media keybag
    /// locker — points at the container-level structure that
    /// indexes per-volume keybags.
    pub media_keybag_block_start: u64,
    /// `nx_mkb_locker.pr_block_count`. Non-zero on encrypted
    /// containers.
    pub media_keybag_block_count: u64,
}

impl ContainerCryptoMeta {
    /// Decode from a raw `nx_superblock_t` block. Caller is
    /// responsible for having already validated the object
    /// header + magic (see `container::decode_container_summary`).
    pub fn from_nx_superblock(block: &[u8]) -> Self {
        Self {
            keylocker_block_start: le_u64(block, NX_KEYLOCKER_OFFSET),
            keylocker_block_count: le_u64(block, NX_KEYLOCKER_OFFSET + 8),
            media_keybag_block_start: le_u64(block, NX_MKB_LOCKER_OFFSET),
            media_keybag_block_count: le_u64(block, NX_MKB_LOCKER_OFFSET + 8),
        }
    }

    /// True iff the container has a keylocker (i.e., is
    /// encrypted at the container layer). Equivalent to
    /// `keylocker_block_count > 0`.
    pub fn has_keylocker(&self) -> bool {
        self.keylocker_block_count > 0
    }

    /// True iff the container has a media keybag locker.
    /// Equivalent to `media_keybag_block_count > 0`.
    pub fn has_media_keybag(&self) -> bool {
        self.media_keybag_block_count > 0
    }
}

/// Per-volume metadata-encryption state. The actual per-file
/// `crypto_state_t` lives in inode `XATTR`-equivalent fields
/// (we already see them at object type `0x7` in
/// `fs_record_body.rs`). This is the *volume-level metadata*
/// crypto descriptor — describes how the volume's FS-tree
/// itself is encrypted on disk.
///
/// Contains **no key bytes**. The persistent-key field is
/// always zero-length on meta_crypto (the struct uses a
/// 0-byte trailing array there); the real key is derived at
/// runtime from the volume's master key + UUID and lives only
/// in kernel memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeMetaCryptoState {
    /// `state.major_version`. Always 5 on modern (≥ macOS
    /// 10.13) volumes; bump signals a format change.
    pub major_version: u16,
    pub minor_version: u16,
    /// `cpflags`. Bit field; documented values in cprotect.h.
    pub cpflags: u32,
    /// Raw `cp_key_class_t`. Decode via `cp_key_class_name`.
    pub persistent_class: u32,
    pub persistent_class_name: Option<&'static str>,
    /// `key_os_version`. Encoded macOS version that wrote
    /// this descriptor (32-bit packed: major.minor.patch).
    pub key_os_version: u32,
    /// `key_revision`. Bumped on key rotation; useful to
    /// detect "this volume has been re-keyed since I last
    /// saw it."
    pub key_revision: u16,
    /// `key_len`. Length in bytes of the trailing
    /// `persistent_key[]` field. **Zero for meta_crypto** —
    /// per-volume metadata crypto doesn't carry a key here.
    /// Non-zero on per-file `crypto_state_t` records, which
    /// is how we know this is the meta_crypto descriptor.
    pub key_len: u16,
}

impl VolumeMetaCryptoState {
    /// Decode from a raw `apfs_superblock_t` block. Caller is
    /// responsible for having already validated the object
    /// header + magic (see `volume::decode_volume_summary`).
    pub fn from_apfs_superblock(block: &[u8]) -> Self {
        let base = APFS_META_CRYPTO_OFFSET;
        let persistent_class = le_u32(block, base + 8);
        Self {
            major_version: le_u16(block, base),
            minor_version: le_u16(block, base + 2),
            cpflags: le_u32(block, base + 4),
            persistent_class,
            persistent_class_name: cp_key_class_name(persistent_class),
            key_os_version: le_u32(block, base + 12),
            key_revision: le_u16(block, base + 16),
            key_len: le_u16(block, base + 18),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 4 KiB block, write a u64 at `offset` little-
    /// endian. Synthetic-only — we don't have an encrypted
    /// .dmg fixture; tests target the byte-layout math.
    fn block_with_u64s(pairs: &[(usize, u64)]) -> Vec<u8> {
        let mut block = vec![0u8; 4096];
        for (offset, value) in pairs {
            block[*offset..*offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        block
    }

    fn block_with_u16(block: &mut [u8], offset: usize, value: u16) {
        block[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn block_with_u32(block: &mut [u8], offset: usize, value: u32) {
        block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Unencrypted container → both lockers report (0, 0).
    #[test]
    fn container_crypto_meta_zero_on_unencrypted() {
        let block = vec![0u8; 4096];
        let meta = ContainerCryptoMeta::from_nx_superblock(&block);
        assert_eq!(meta.keylocker_block_start, 0);
        assert_eq!(meta.keylocker_block_count, 0);
        assert_eq!(meta.media_keybag_block_start, 0);
        assert_eq!(meta.media_keybag_block_count, 0);
        assert!(!meta.has_keylocker());
        assert!(!meta.has_media_keybag());
    }

    /// Encrypted container shape: both lockers populated.
    #[test]
    fn container_crypto_meta_reads_synthetic_encrypted() {
        let block = block_with_u64s(&[
            (NX_KEYLOCKER_OFFSET, 0x1234),
            (NX_KEYLOCKER_OFFSET + 8, 1),
            (NX_MKB_LOCKER_OFFSET, 0x5678),
            (NX_MKB_LOCKER_OFFSET + 8, 4),
        ]);
        let meta = ContainerCryptoMeta::from_nx_superblock(&block);
        assert_eq!(meta.keylocker_block_start, 0x1234);
        assert_eq!(meta.keylocker_block_count, 1);
        assert_eq!(meta.media_keybag_block_start, 0x5678);
        assert_eq!(meta.media_keybag_block_count, 4);
        assert!(meta.has_keylocker());
        assert!(meta.has_media_keybag());
    }

    /// A container could have the wrapping-key locker but no
    /// media keybag locker (older format, transitional state).
    /// has_keylocker and has_media_keybag are independent.
    #[test]
    fn container_crypto_meta_locker_independence() {
        let block = block_with_u64s(&[
            (NX_KEYLOCKER_OFFSET, 0x1234),
            (NX_KEYLOCKER_OFFSET + 8, 1),
            // mkb locker unset
        ]);
        let meta = ContainerCryptoMeta::from_nx_superblock(&block);
        assert!(meta.has_keylocker());
        assert!(!meta.has_media_keybag());
    }

    /// `apfs_meta_crypto` shape on an unencrypted volume.
    /// Apple still writes the algorithm-version pin even on
    /// unencrypted volumes (major_version=5, everything else
    /// zero) so this isn't all-zero in practice — but a
    /// synthetic all-zero block parses cleanly.
    #[test]
    fn volume_meta_crypto_zero_block() {
        let block = vec![0u8; 4096];
        let meta = VolumeMetaCryptoState::from_apfs_superblock(&block);
        assert_eq!(meta.major_version, 0);
        assert_eq!(meta.persistent_class, 0);
        assert_eq!(meta.persistent_class_name, Some("CPROTECT_CLASS_DIR_NONE"));
        assert_eq!(meta.key_len, 0);
    }

    /// Synthetic encrypted-volume meta_crypto: typical
    /// Apple-silicon Data Protection shape (Class C is the
    /// default for the data volume — files readable after
    /// first user auth).
    #[test]
    fn volume_meta_crypto_class_c_data_volume() {
        let mut block = vec![0u8; 4096];
        let base = APFS_META_CRYPTO_OFFSET;
        block_with_u16(&mut block, base, 5); // major_version
        block_with_u16(&mut block, base + 2, 0); // minor_version
        block_with_u32(&mut block, base + 4, 0); // cpflags
        block_with_u32(&mut block, base + 8, 3); // persistent_class = CLASS_C
        block_with_u32(&mut block, base + 12, 0x14_04_00_00); // key_os_version (macOS 20.4)
        block_with_u16(&mut block, base + 16, 1); // key_revision
        block_with_u16(&mut block, base + 18, 0); // key_len: 0 for meta_crypto

        let meta = VolumeMetaCryptoState::from_apfs_superblock(&block);
        assert_eq!(meta.major_version, 5);
        assert_eq!(meta.persistent_class, 3);
        assert!(
            meta.persistent_class_name.unwrap().starts_with("CPROTECT_CLASS_C"),
            "got: {:?}",
            meta.persistent_class_name
        );
        assert_eq!(meta.key_revision, 1);
        assert_eq!(meta.key_len, 0, "meta_crypto must not carry a key");
    }

    /// Per-file `crypto_state_t` records also use the
    /// `wrapped_crypto_state_t` shape but with a non-zero
    /// `key_len` and a trailing wrapped key. We don't parse
    /// those here (they live in FS-record bodies, not the
    /// superblock) but the class-name resolver is shared.
    /// Smoke-test the protection-class decoder for each
    /// well-known class.
    #[test]
    fn cp_key_class_name_decodes_known_classes() {
        assert_eq!(cp_key_class_name(0), Some("CPROTECT_CLASS_DIR_NONE"));
        assert!(cp_key_class_name(1).unwrap().starts_with("CPROTECT_CLASS_A"));
        assert!(cp_key_class_name(2).unwrap().starts_with("CPROTECT_CLASS_B"));
        assert!(cp_key_class_name(3).unwrap().starts_with("CPROTECT_CLASS_C"));
        assert!(cp_key_class_name(4).unwrap().starts_with("CPROTECT_CLASS_D"));
        assert!(cp_key_class_name(6).unwrap().starts_with("CPROTECT_CLASS_F"));
        assert_eq!(cp_key_class_name(5), None, "class 5 is unused/reserved");
        assert_eq!(cp_key_class_name(0x1f & 7), None, "class 7 is unused");
    }

    /// Upper flag bits on `cp_key_class_t` (CP_EFFECTIVE_FLAGS
    /// mask = 0xe0) shouldn't confuse the class lookup. The
    /// real class is in the low 5 bits.
    #[test]
    fn cp_key_class_name_masks_off_flag_bits() {
        // 0xc3 = flags 0xc0 + class 3 (Class C).
        let name = cp_key_class_name(0xc3).expect("Class C with flags");
        assert!(name.starts_with("CPROTECT_CLASS_C"));
    }
}
