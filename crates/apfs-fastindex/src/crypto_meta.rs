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

/// Per-file `crypto_state_t` body — what lives in FS-records
/// of object type `0x7` (CRYPTO_STATE; see
/// `fs_record_body.rs:321`). Same wire shape as
/// `VolumeMetaCryptoState`'s header but with `key_len > 0`
/// and a trailing wrapped-key blob.
///
/// Wire layout (`j_crypto_state_val_t`):
///
/// ```text
/// offset  size  field
///   0      4    refcnt                  // hard-link / clone refcount
///   4     20    wrapped_crypto_state_t  // same shape as apfs_meta_crypto
///  24     N    persistent_key[key_len]  // wrapped key, opaque ciphertext
/// ```
///
/// **The `persistent_key[]` bytes are encrypted.** We never
/// try to unwrap them. We surface the raw bytes so callers
/// can fingerprint (SHA256 / equality-check) or log them
/// without re-implementing the parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PerFileCryptoState {
    /// `j_crypto_state_val_t.refcnt`. Number of distinct file
    /// records that share this wrapped key — bumped on hard
    /// link / clone, decremented on unlink.
    pub refcnt: u32,
    /// Algorithm + class metadata (the `wrapped_crypto_state_t`
    /// header). Same field set as `VolumeMetaCryptoState`.
    pub major_version: u16,
    pub minor_version: u16,
    pub cpflags: u32,
    pub persistent_class: u32,
    pub persistent_class_name: Option<&'static str>,
    pub key_os_version: u32,
    pub key_revision: u16,
    /// Length in bytes of `wrapped_key`. **Non-zero for
    /// per-file records** (typically 32 = AES-256-XTS key);
    /// zero would be the volume-meta_crypto-style descriptor.
    pub key_len: u16,
    /// Wrapped persistent key bytes. Opaque ciphertext —
    /// wrapped by the file's protection-class key, which is
    /// in turn wrapped by Secure Enclave hardware keys on
    /// Apple silicon. Length always matches `key_len`.
    pub wrapped_key: Vec<u8>,
}

/// Tag values for `apfs_keybag_entry_t.ke_tag`. Identifies
/// what kind of unlock record an entry is. Apple's
/// documentation calls these `BAG_TYPE_*`. Values cross-
/// referenced with linux-apfs-rw + `diskutil apfs
/// listCryptoUsers` output.
pub fn bag_entry_tag_name(tag: u16) -> Option<&'static str> {
    match tag {
        // Volume-keybag entry tags.
        2 => Some("BAG_TYPE_VOL_KEY"),
        3 => Some("BAG_TYPE_UNLOCK_RECORDS"),
        4 => Some("BAG_TYPE_PASSPHRASE_HINT"),
        // Container-/media-keybag entry tag.
        5 => Some("BAG_TYPE_WRAPPING_M_KEY"),
        // Sealed-volume + integrity tags (newer).
        6 => Some("BAG_TYPE_VOLUME_M_KEY"),
        _ => None,
    }
}

/// `apfs_kb_locker_t` — the container-/volume-level keybag
/// outer framing. Lives at the block address pointed to by
/// `nx_mkb_locker` (container-level) or `apfs_keybag_loc`
/// (volume-level). The locker contains N
/// `apfs_keybag_entry_t` records.
///
/// Wire layout:
///
/// ```text
/// offset  size   field
///   0      2     kl_version    // currently 2; bump = format change
///   2      2     kl_nkeys      // number of entries
///   4      4     kl_nbytes     // size in bytes of all entries combined
///   8      8     kl_padding    // zeros, alignment
///  16      N    kl_entries[]  // variable
/// ```
///
/// **Important caveat**: on a typical encrypted volume the
/// locker block is itself encrypted (wrapped by the
/// `nx_keylocker`'s effective wrapping key). Decoding the
/// framing requires either an unencrypted volume (legacy
/// mode, or a synthetic research image) or a decrypted blob.
/// Our parser doesn't decrypt; this decoder is for the
/// "given an already-decrypted blob, what's in it" case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeybagLocker {
    pub version: u16,
    pub nkeys: u16,
    pub nbytes: u32,
    pub entries: Vec<KeybagEntry>,
}

/// One `apfs_keybag_entry_t` record. The `keydata` payload
/// is variable-length and **opaque** — we surface it as raw
/// bytes for callers to fingerprint or pass through.
///
/// Wire layout:
///
/// ```text
///   0     16   ke_uuid       // identifier
///  16      2   ke_tag        // BAG_TYPE_*
///  18      2   ke_keylen     // length of ke_keydata
///  20      4   ke_padding    // alignment
///  24      N   ke_keydata    // opaque, ke_keylen bytes
/// ```
///
/// Each entry is 8-byte-aligned: a record with `ke_keylen=5`
/// takes 24 + 5 = 29 bytes raw, but the next entry starts at
/// offset (29 + 7) & ~7 = 32. Our parser handles the padding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeybagEntry {
    /// `ke_uuid` formatted as the standard 8-4-4-4-12 hex.
    /// Compare against `WELL_KNOWN_CRYPTO_USER_UUIDS` in the
    /// EX-32 probe to identify Apple's hardcoded recovery
    /// slot UUIDs (Personal Recovery, iCloud Recovery External).
    pub uuid_hex: String,
    pub tag: u16,
    pub tag_name: Option<&'static str>,
    pub keylen: u16,
    pub keydata: Vec<u8>,
}

impl KeybagLocker {
    /// Decode from a block that's been (already) decrypted.
    /// Fail-closed on truncated headers, entry-length
    /// overflow, or an entry that runs past the declared
    /// `kl_nbytes` budget.
    pub fn from_decrypted_block(block: &[u8]) -> Result<Self, &'static str> {
        if block.len() < 16 {
            return Err("keybag locker shorter than 16-byte header");
        }
        let version = le_u16(block, 0);
        let nkeys = le_u16(block, 2);
        let nbytes = le_u32(block, 4);
        // Skip kl_padding at 0x08..0x10.
        let entries_start = 16usize;
        let entries_end = entries_start
            .checked_add(nbytes as usize)
            .ok_or("kl_nbytes arithmetic overflow")?;
        if entries_end > block.len() {
            return Err("kl_nbytes exceeds block capacity");
        }

        let mut entries: Vec<KeybagEntry> = Vec::with_capacity(nkeys as usize);
        let mut cursor = entries_start;
        for _ in 0..nkeys {
            if cursor + 24 > entries_end {
                return Err("keybag entry header runs past kl_nbytes");
            }
            let uuid_hex = format_uuid(&block[cursor..cursor + 16]);
            let tag = le_u16(block, cursor + 16);
            let keylen = le_u16(block, cursor + 18);
            // padding at cursor+20..cursor+24
            let key_start = cursor + 24;
            let key_end = key_start
                .checked_add(keylen as usize)
                .ok_or("ke_keylen arithmetic overflow")?;
            if key_end > entries_end {
                return Err("keybag entry keydata runs past kl_nbytes");
            }
            entries.push(KeybagEntry {
                uuid_hex,
                tag,
                tag_name: bag_entry_tag_name(tag),
                keylen,
                keydata: block[key_start..key_end].to_vec(),
            });
            // 8-byte-align the next entry's start.
            cursor = (key_end + 7) & !7usize;
        }
        Ok(Self {
            version,
            nkeys,
            nbytes,
            entries,
        })
    }
}

/// Format 16 raw UUID bytes as the canonical 8-4-4-4-12
/// uppercase hex grouping (matches `diskutil apfs
/// listCryptoUsers` output).
fn format_uuid(bytes: &[u8]) -> String {
    debug_assert_eq!(bytes.len(), 16);
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-\
         {:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

impl PerFileCryptoState {
    /// Decode from a raw FS-record body buffer. The first
    /// 4 bytes are the refcount; the next 20 are the
    /// `wrapped_crypto_state_t` header; the remainder is the
    /// wrapped key blob of length `key_len`.
    ///
    /// Returns `Err` if the body is shorter than the header
    /// (24 bytes) or shorter than `24 + key_len`. Both
    /// indicate a malformed record; the caller should
    /// fail-closed.
    pub fn from_body(body: &[u8]) -> Result<Self, &'static str> {
        if body.len() < 24 {
            return Err("crypto_state body shorter than j_crypto_state_val_t header (24 bytes)");
        }
        let refcnt = le_u32(body, 0);
        let persistent_class = le_u32(body, 12);
        let key_len = le_u16(body, 22);
        let key_end = 24usize.saturating_add(key_len as usize);
        if body.len() < key_end {
            return Err("crypto_state body shorter than declared key_len");
        }
        Ok(Self {
            refcnt,
            major_version: le_u16(body, 4),
            minor_version: le_u16(body, 6),
            cpflags: le_u32(body, 8),
            persistent_class,
            persistent_class_name: cp_key_class_name(persistent_class),
            key_os_version: le_u32(body, 16),
            key_revision: le_u16(body, 20),
            key_len,
            wrapped_key: body[24..key_end].to_vec(),
        })
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

    /// Build a synthetic `j_crypto_state_val_t` body: 4-byte
    /// refcount + 20-byte wrapped_crypto_state_t header +
    /// `key_len` trailing key bytes. Returns the raw buffer
    /// the parser would see in an FS-record body slice.
    fn build_per_file_crypto_body(
        refcnt: u32,
        major: u16,
        cpflags: u32,
        persistent_class: u32,
        key_os_version: u32,
        key_revision: u16,
        wrapped_key: &[u8],
    ) -> Vec<u8> {
        let key_len = wrapped_key.len() as u16;
        let mut body = vec![0u8; 24 + wrapped_key.len()];
        body[0..4].copy_from_slice(&refcnt.to_le_bytes());
        body[4..6].copy_from_slice(&major.to_le_bytes());
        // minor_version stays 0
        body[8..12].copy_from_slice(&cpflags.to_le_bytes());
        body[12..16].copy_from_slice(&persistent_class.to_le_bytes());
        body[16..20].copy_from_slice(&key_os_version.to_le_bytes());
        body[20..22].copy_from_slice(&key_revision.to_le_bytes());
        body[22..24].copy_from_slice(&key_len.to_le_bytes());
        body[24..].copy_from_slice(wrapped_key);
        body
    }

    /// Typical per-file crypto_state record: Class C (default
    /// for Apple-silicon Data Protection on the data volume),
    /// 32-byte AES-256-XTS wrapped key, refcount 1.
    #[test]
    fn per_file_crypto_state_class_c_aes_256_xts() {
        let key = [0xa5u8; 32]; // synthetic; real keys are
                                //  AES-Keywrapped ciphertext.
        let body = build_per_file_crypto_body(
            1,                  // refcnt
            5,                  // major_version
            0,                  // cpflags
            3,                  // persistent_class = CLASS_C
            0x14_04_00_00,      // macOS 20.4
            2,                  // key_revision
            &key,
        );
        let parsed = PerFileCryptoState::from_body(&body).expect("decode");
        assert_eq!(parsed.refcnt, 1);
        assert_eq!(parsed.major_version, 5);
        assert_eq!(parsed.persistent_class, 3);
        assert!(parsed
            .persistent_class_name
            .unwrap()
            .starts_with("CPROTECT_CLASS_C"));
        assert_eq!(parsed.key_revision, 2);
        assert_eq!(parsed.key_len, 32);
        assert_eq!(parsed.wrapped_key, key.to_vec());
    }

    /// Hard-linked files share their crypto_state record;
    /// refcnt > 1 means N inode records all point to this
    /// key.
    #[test]
    fn per_file_crypto_state_handles_high_refcount() {
        let body = build_per_file_crypto_body(7, 5, 0, 3, 0, 0, &[0u8; 32]);
        let parsed = PerFileCryptoState::from_body(&body).expect("decode");
        assert_eq!(parsed.refcnt, 7);
    }

    /// Truncated body — shorter than the 24-byte header —
    /// must fail-closed. A real FS-record with this shape
    /// would indicate B-tree corruption; the parser's
    /// fail-closed contract says we surface an error, not
    /// silently truncate.
    #[test]
    fn per_file_crypto_state_rejects_truncated_header() {
        let body = vec![0u8; 23];
        let err = PerFileCryptoState::from_body(&body).expect_err("must fail");
        assert!(err.contains("24 bytes"), "got: {err}");
    }

    /// Header declares key_len > body capacity. Also a
    /// fail-closed case — a record that says "32-byte key
    /// follows" but has only 16 bytes is malformed.
    #[test]
    fn per_file_crypto_state_rejects_truncated_key() {
        let mut body = vec![0u8; 24 + 16]; // says 32-byte key,
                                           //  but only 16 bytes
                                           //  follow
        // refcnt = 1
        body[0] = 1;
        // major = 5
        body[4] = 5;
        // key_len = 32 at offset 22
        body[22..24].copy_from_slice(&32u16.to_le_bytes());
        let err = PerFileCryptoState::from_body(&body).expect_err("must fail");
        assert!(err.contains("key_len"), "got: {err}");
    }

    /// Class-F records (the keybag's own protection class)
    /// are decodable — the kernel reads them before the user
    /// authenticates, so they're effectively unprotected
    /// but still routed through the same encryption pipeline
    /// for shape uniformity.
    #[test]
    fn per_file_crypto_state_class_f_keybag_style() {
        let body = build_per_file_crypto_body(1, 5, 0, 6, 0, 0, &[0x42u8; 16]);
        let parsed = PerFileCryptoState::from_body(&body).expect("decode");
        assert_eq!(parsed.persistent_class, 6);
        assert!(parsed
            .persistent_class_name
            .unwrap()
            .starts_with("CPROTECT_CLASS_F"));
    }

    /// Append one `apfs_keybag_entry_t` to a buffer at the
    /// current 8-byte-aligned cursor. Returns the new cursor
    /// after the entry + alignment padding. Used by the
    /// keybag tests to build synthetic lockers.
    fn append_keybag_entry(
        buf: &mut Vec<u8>,
        uuid: &[u8; 16],
        tag: u16,
        keydata: &[u8],
    ) {
        let start = buf.len();
        buf.extend_from_slice(uuid);
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&(keydata.len() as u16).to_le_bytes());
        buf.extend_from_slice(&[0u8; 4]); // ke_padding
        buf.extend_from_slice(keydata);
        // 8-byte-align the next entry.
        let used = buf.len() - start;
        let aligned = (used + 7) & !7;
        buf.resize(start + aligned, 0);
    }

    /// Roundtrip one entry: tag = BAG_TYPE_VOL_KEY, 16-byte
    /// synthetic keydata.
    #[test]
    fn keybag_locker_decodes_single_entry() {
        let mut entries = Vec::new();
        let uuid = [0x42u8; 16];
        let key = [0xaau8; 32];
        append_keybag_entry(&mut entries, &uuid, 2 /* VOL_KEY */, &key);
        let mut block = Vec::new();
        block.extend_from_slice(&2u16.to_le_bytes()); // kl_version
        block.extend_from_slice(&1u16.to_le_bytes()); // kl_nkeys
        block.extend_from_slice(&(entries.len() as u32).to_le_bytes()); // kl_nbytes
        block.extend_from_slice(&[0u8; 8]); // kl_padding
        block.extend_from_slice(&entries);
        block.resize(block.len() + 64, 0); // tail padding (block
                                            // can be larger than
                                            // kl_nbytes)
        let parsed = KeybagLocker::from_decrypted_block(&block).expect("decode");
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.nkeys, 1);
        assert_eq!(parsed.entries.len(), 1);
        let e = &parsed.entries[0];
        assert_eq!(e.tag, 2);
        assert_eq!(e.tag_name, Some("BAG_TYPE_VOL_KEY"));
        assert_eq!(e.keylen, 32);
        assert_eq!(e.keydata, key.to_vec());
        // UUID format: hyphenated uppercase hex.
        assert_eq!(e.uuid_hex, "42424242-4242-4242-4242-424242424242");
    }

    /// Multi-entry locker with each well-known tag value
    /// surfaced via tag_name. Mirrors the four-entry shape
    /// our EX-32 host probe found in `diskutil apfs
    /// listCryptoUsers` output.
    #[test]
    fn keybag_locker_decodes_four_entry_unlock_records_shape() {
        let mut entries = Vec::new();
        // Personal Recovery User (well-known UUID).
        let prk_uuid = [
            0xEB, 0xC6, 0xC0, 0x64, 0x00, 0x00, 0x11, 0xAA,
            0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC, 0xAC,
        ];
        append_keybag_entry(&mut entries, &prk_uuid, 3 /* UNLOCK_RECORDS */, &[0xaa; 64]);
        // iCloud Recovery External Key (well-known UUID).
        let icloud_uuid = [
            0x64, 0xC0, 0xC6, 0xEB, 0x00, 0x00, 0x11, 0xAA,
            0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC, 0xAC,
        ];
        append_keybag_entry(&mut entries, &icloud_uuid, 3, &[0xbb; 64]);
        // Synthetic local Open Directory user UUID.
        append_keybag_entry(&mut entries, &[0x11; 16], 3, &[0xcc; 64]);
        // Synthetic iCloud Recovery escrow.
        append_keybag_entry(&mut entries, &[0x22; 16], 3, &[0xdd; 64]);

        let mut block = Vec::new();
        block.extend_from_slice(&2u16.to_le_bytes()); // version
        block.extend_from_slice(&4u16.to_le_bytes()); // nkeys
        block.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        block.extend_from_slice(&[0u8; 8]); // padding
        block.extend_from_slice(&entries);
        block.resize(block.len() + 128, 0);

        let parsed = KeybagLocker::from_decrypted_block(&block).expect("decode");
        assert_eq!(parsed.entries.len(), 4);
        assert_eq!(parsed.entries[0].uuid_hex, "EBC6C064-0000-11AA-AA11-00306543ECAC");
        assert_eq!(parsed.entries[1].uuid_hex, "64C0C6EB-0000-11AA-AA11-00306543ECAC");
        for e in &parsed.entries {
            assert_eq!(e.tag_name, Some("BAG_TYPE_UNLOCK_RECORDS"));
        }
    }

    /// Truncated block (shorter than the 16-byte header) must
    /// fail-closed, not panic.
    #[test]
    fn keybag_locker_rejects_truncated_header() {
        let block = vec![0u8; 15];
        let err = KeybagLocker::from_decrypted_block(&block).expect_err("must fail");
        assert!(err.contains("16-byte"), "got: {err}");
    }

    /// kl_nbytes declares more than block capacity → fail.
    #[test]
    fn keybag_locker_rejects_nbytes_overflow() {
        let mut block = vec![0u8; 32];
        block[0..2].copy_from_slice(&2u16.to_le_bytes()); // version
        block[2..4].copy_from_slice(&1u16.to_le_bytes()); // nkeys
        block[4..8].copy_from_slice(&1_000_000u32.to_le_bytes()); // huge nbytes
        let err = KeybagLocker::from_decrypted_block(&block).expect_err("must fail");
        assert!(err.contains("kl_nbytes"), "got: {err}");
    }

    /// An entry whose keydata claims to extend past nbytes
    /// also fails-closed. Tests the inner-loop bounds check.
    #[test]
    fn keybag_locker_rejects_entry_overflow() {
        let mut entries = Vec::new();
        // Single entry, but lie about keylen.
        entries.extend_from_slice(&[0u8; 16]); // uuid
        entries.extend_from_slice(&2u16.to_le_bytes()); // tag
        entries.extend_from_slice(&999u16.to_le_bytes()); // keylen = 999
        entries.extend_from_slice(&[0u8; 4]); // padding
        // No keydata follows.
        let mut block = Vec::new();
        block.extend_from_slice(&2u16.to_le_bytes());
        block.extend_from_slice(&1u16.to_le_bytes());
        block.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        block.extend_from_slice(&[0u8; 8]);
        block.extend_from_slice(&entries);
        block.resize(block.len() + 32, 0);
        let err = KeybagLocker::from_decrypted_block(&block).expect_err("must fail");
        assert!(err.contains("keydata"), "got: {err}");
    }

    /// `bag_entry_tag_name` decodes the documented values.
    /// Unknown tags return None — keep the door open for
    /// future Apple-added values without panicking.
    #[test]
    fn bag_entry_tag_name_decodes_known_values() {
        assert_eq!(bag_entry_tag_name(2), Some("BAG_TYPE_VOL_KEY"));
        assert_eq!(bag_entry_tag_name(3), Some("BAG_TYPE_UNLOCK_RECORDS"));
        assert_eq!(bag_entry_tag_name(4), Some("BAG_TYPE_PASSPHRASE_HINT"));
        assert_eq!(bag_entry_tag_name(5), Some("BAG_TYPE_WRAPPING_M_KEY"));
        assert_eq!(bag_entry_tag_name(6), Some("BAG_TYPE_VOLUME_M_KEY"));
        assert_eq!(bag_entry_tag_name(999), None);
    }

}
