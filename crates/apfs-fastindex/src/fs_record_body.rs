//! FS-record body decoder for the v1 namespace + logical-size record set.
//!
//! `SR-008` and `SR-014` pin the families this module decodes: `DIR_REC`,
//! `INODE`, `XATTR`, `SIBLING_LINK`, `SIBLING_MAP`, plus the
//! `INO_EXT_TYPE_DSTREAM` / `INO_EXT_TYPE_NAME` /
//! `INO_EXT_TYPE_SPARSE_BYTES` / `DREC_EXT_TYPE_SIBLING_ID` xfields used by
//! those families.
//!
//! `SR-015` (proven by `EX-16`) says xfield values start immediately after
//! `xf_blob_t` plus the metadata table and each value occupies
//! `round_up(x_size, 8)` bytes; this module implements that single cursor
//! rule.
//!
//! `SR-016` defines the fail-closed boundary: every malformed body must
//! produce a typed `ScanError::InvalidObject`. The decoders below treat the
//! following as hard stops:
//!
//! - body shorter than the family's fixed struct
//! - variable-length name longer than the available key/value bytes
//! - non-UTF-8 name bytes (after stripping at most one trailing NUL)
//! - empty required name
//! - xfield blob shorter than `xf_blob_t` header
//! - xfield metadata table that runs past the blob length
//! - xfield value cursor that runs past the blob length
//! - `xf_used_data != sum(round_up(x_size, 8))`
//! - duplicate xfield types inside one blob
//! - required xfield value with the wrong fixed size
//!   (`INO_EXT_TYPE_DSTREAM != 40`, `INO_EXT_TYPE_SPARSE_BYTES != 8`,
//!   `DREC_EXT_TYPE_SIBLING_ID != 8`)
//! - xattr value shorter than the 4-byte fixed header
//! - xattr flags that set neither nor both of embedded/stream
//! - xattr embedded body whose length disagrees with `xdata_len`
//! - xattr stream body shorter than `j_xattr_dstream_t`
//! - `j_drec_val_t.flags & DREC_TYPE_MASK` carrying an unknown POSIX type
//! - sibling-link / sibling-map records with malformed names or missing
//!   `file_id` field

use serde::Serialize;

use crate::block_io::{le_u16, le_u32, le_u64};
use crate::ScanError;

// ---- public types ------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsRecordRow {
    pub node_paddr: u64,
    pub entry_index: u32,
    pub object_id: u64,
    pub raw_type: u8,
    pub family: &'static str,
    pub key_len: u32,
    pub value_len: u32,
    pub key: FsRecordKey,
    pub value: FsRecordValue,
    pub validation_notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FsRecordKey {
    Plain,
    Named {
        raw_key_form: &'static str,
        name_len: u32,
        name: String,
        name_bytes_hex: String,
    },
    SiblingLink {
        sibling_id: u64,
    },
    /// `j_file_extent_key_t` — high 60 bits of `hdr` are the dstream_id
    /// (shared by clones), low 60 bits are repeated as `object_id` on the
    /// row; the additional `logical_addr` is the file extent's logical
    /// offset.
    FileExtent {
        logical_addr: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FsRecordValue {
    Inode(InodeBody),
    Xattr(XattrBody),
    SiblingLink(SiblingLinkBody),
    DstreamId {
        refcnt: Option<u32>,
    },
    DirRec(DirRecBody),
    SiblingMap {
        file_id: Option<u64>,
    },
    FileExtent(FileExtentBody),
    /// Well-formed but outside v1 namespace + logical-size scope; the row is
    /// counted but no further fields are produced.
    Unsupported {
        reason: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InodeBody {
    pub parent_id: u64,
    pub private_id: u64,
    pub internal_flags: u64,
    pub nchildren_or_nlink: i32,
    pub bsd_flags: u32,
    pub owner: u32,
    pub group: u32,
    pub mode: u16,
    pub uncompressed_size: u64,
    pub has_uncompressed_size: bool,
    pub xfields: Vec<XfieldEntry>,
    pub xfield_used_data: u32,
    pub xfield_padded_total: u32,
    pub xfield_unused_trailing_bytes: i32,
    pub dstream: Option<DstreamFields>,
    pub sparse_bytes: Option<u64>,
    pub inode_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirRecBody {
    pub file_id: u64,
    pub date_added: u64,
    pub flags: u16,
    pub entry_type: u8,
    pub sibling_id: Option<u64>,
    pub xfields: Vec<XfieldEntry>,
    pub xfield_used_data: u32,
    pub xfield_padded_total: u32,
    pub xfield_unused_trailing_bytes: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct XattrBody {
    pub flags: u16,
    pub xdata_len: u16,
    pub embedded: bool,
    pub stream: bool,
    pub payload_hex: String,
    pub payload_utf8: Option<String>,
    pub stream_xattr_obj_id: Option<u64>,
    pub stream_dstream: Option<DstreamFields>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiblingLinkBody {
    pub parent_id: u64,
    pub name_len: u16,
    pub name: String,
    pub name_bytes_hex: String,
}

/// `j_file_extent_val_t` — 24 bytes: `len_and_flags` (8) +
/// `phys_block_num` (8) + `crypto_id` (8). High 4 bits of
/// `len_and_flags` are flags; low 60 bits are length in *bytes*.
///
/// EX-27 (clone-dedup): each file_extent record points at a physical
/// extent. Multiple clones reference the same paddr through the
/// extent-reference tree's `phys_ext` records; the refcnt there is
/// what makes dedup work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FileExtentBody {
    pub length_bytes: u64,
    pub flags: u8,
    pub phys_block_num: u64,
    pub crypto_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct XfieldEntry {
    pub x_type: u8,
    pub x_flags: u8,
    pub x_size: u16,
    pub padded_length: u32,
    pub value_hex: String,
    pub interpreted: Option<XfieldInterpreted>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum XfieldInterpreted {
    U64 { value: u64 },
    Utf8 { value: String },
    Dstream { value: DstreamFields },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DstreamFields {
    pub size: u64,
    pub alloced_size: u64,
    pub default_crypto_id: u64,
    pub total_bytes_written: u64,
    pub total_bytes_read: u64,
}

// ---- constants ---------------------------------------------------------- //

const FS_OBJECT_ID_MASK: u64 = (1u64 << 60) - 1;
const FS_RECORD_TYPE_SHIFT: u32 = 60;

const INODE_FIXED_SIZE: usize = 0x5C;
const DREC_PREFIX_SIZE: usize = 18;
const XATTR_VALUE_HEADER_SIZE: usize = 4;
const SIBLING_LINK_FIXED_PREFIX: usize = 10; // 8 parent_id + 2 name_len
const SIBLING_MAP_VALUE_SIZE: usize = 8;
const DSTREAM_SIZE: usize = 40;
const J_XATTR_DSTREAM_SIZE: usize = 8 + DSTREAM_SIZE;

const INODE_HAS_UNCOMPRESSED_SIZE: u64 = 0x0004_0000;

const J_DREC_LEN_MASK: u32 = 0x0000_03FF;
const DREC_TYPE_MASK: u16 = 0x000F;

/// `j_drec_val_t.flags & DREC_TYPE_MASK` POSIX types we accept in v1.
/// Values come from `dirent.h` (DT_*) which APFS reuses.
const DT_FIFO: u8 = 1;
const DT_CHR: u8 = 2;
const DT_DIR: u8 = 4;
const DT_BLK: u8 = 6;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;
const DT_SOCK: u8 = 12;
const DT_WHT: u8 = 14;

const XATTR_DATA_STREAM: u16 = 0x0001;
const XATTR_DATA_EMBEDDED: u16 = 0x0002;
const XATTR_FILE_SYSTEM_OWNED: u16 = 0x0004;
const XATTR_KNOWN_FLAG_BITS: u16 =
    XATTR_DATA_STREAM | XATTR_DATA_EMBEDDED | XATTR_FILE_SYSTEM_OWNED;

pub(crate) const RAW_TYPE_INODE: u8 = 3;
pub(crate) const RAW_TYPE_XATTR: u8 = 4;
pub(crate) const RAW_TYPE_SIBLING_LINK: u8 = 5;
pub(crate) const RAW_TYPE_DSTREAM_ID: u8 = 6;
pub(crate) const RAW_TYPE_FILE_EXTENT: u8 = 8;
pub(crate) const RAW_TYPE_DIR_REC: u8 = 9;
pub(crate) const RAW_TYPE_SIBLING_MAP: u8 = 12;

/// `j_file_extent_val_t` is 24 bytes minimum; `crypto_id` (last 8) may be
/// absent on some encoders, but Apple-produced volumes emit all 24.
const FILE_EXTENT_VALUE_MIN: usize = 24;

const INO_EXT_TYPE_SNAP_XID: u8 = 1;
const INO_EXT_TYPE_DELTA_TREE_OID: u8 = 2;
const INO_EXT_TYPE_DOCUMENT_ID: u8 = 3;
const INO_EXT_TYPE_NAME: u8 = 4;
const INO_EXT_TYPE_PREV_FSIZE: u8 = 5;
const INO_EXT_TYPE_RESERVED_6: u8 = 6;
const INO_EXT_TYPE_FINDER_INFO: u8 = 7;
const INO_EXT_TYPE_DSTREAM: u8 = 8;
const INO_EXT_TYPE_RESERVED_9: u8 = 9;
const INO_EXT_TYPE_DIR_STATS_KEY: u8 = 10;
const INO_EXT_TYPE_FS_UUID: u8 = 11;
const INO_EXT_TYPE_RESERVED_12: u8 = 12;
const INO_EXT_TYPE_SPARSE_BYTES: u8 = 13;
const INO_EXT_TYPE_RDEV: u8 = 14;
const INO_EXT_TYPE_PURGEABLE_FLAGS: u8 = 15;
const INO_EXT_TYPE_ORIG_SYNC_ROOT_ID: u8 = 16;

const DREC_EXT_TYPE_SIBLING_ID: u8 = 1;

// ---- public entry point ------------------------------------------------- //

/// Decode one FS-tree leaf entry into a structured row.
///
/// Returns `Err(ScanError::InvalidObject(...))` on every SR-016 hard-stop
/// case. The caller is responsible for hard-stopping the whole walk; this
/// decoder never silently skips a required record.
pub fn decode_fs_record(
    node_paddr: u64,
    entry_index: u32,
    key: &[u8],
    value: &[u8],
) -> Result<FsRecordRow, ScanError> {
    if key.len() < 8 {
        return Err(ScanError::InvalidObject(format!(
            "FS-tree leaf key at node {node_paddr} entry {entry_index} shorter than j_key_t"
        )));
    }
    let key_word = u64::from_le_bytes(key[0..8].try_into().expect("u64 j_key_t header"));
    let object_id = key_word & FS_OBJECT_ID_MASK;
    let raw_type = (key_word >> FS_RECORD_TYPE_SHIFT) as u8;
    let family = record_family_name(raw_type);

    let key_decoded = decode_key(raw_type, key, node_paddr, entry_index)?;
    let mut validation_notes: Vec<String> = Vec::new();
    let value_decoded = decode_value(
        raw_type,
        value,
        node_paddr,
        entry_index,
        &mut validation_notes,
    )?;

    Ok(FsRecordRow {
        node_paddr,
        entry_index,
        object_id,
        raw_type,
        family,
        key_len: key.len() as u32,
        value_len: value.len() as u32,
        key: key_decoded,
        value: value_decoded,
        validation_notes,
    })
}

pub(crate) fn record_family_name(raw_type: u8) -> &'static str {
    match raw_type {
        0x0 => "any",
        0x1 => "snap_metadata",
        0x2 => "extent_reference",
        0x3 => "inode",
        0x4 => "xattr",
        0x5 => "sibling_link",
        0x6 => "dstream_id",
        0x7 => "crypto_state",
        0x8 => "file_extent",
        0x9 => "dir_rec",
        0xa => "dir_stats",
        0xb => "snap_name",
        0xc => "sibling_map",
        0xd => "file_info",
        _ => "unknown",
    }
}

// ---- key decoding ------------------------------------------------------- //

fn decode_key(
    raw_type: u8,
    key: &[u8],
    node_paddr: u64,
    entry_index: u32,
) -> Result<FsRecordKey, ScanError> {
    match raw_type {
        RAW_TYPE_DIR_REC => decode_drec_key(key, node_paddr, entry_index),
        RAW_TYPE_XATTR => decode_xattr_key(key, node_paddr, entry_index),
        RAW_TYPE_SIBLING_LINK => decode_sibling_link_key(key, node_paddr, entry_index),
        RAW_TYPE_FILE_EXTENT => decode_file_extent_key(key, node_paddr, entry_index),
        _ => Ok(FsRecordKey::Plain),
    }
}

fn decode_file_extent_key(
    key: &[u8],
    node_paddr: u64,
    entry_index: u32,
) -> Result<FsRecordKey, ScanError> {
    if key.len() < 16 {
        return Err(ScanError::InvalidObject(format!(
            "file_extent key at node {node_paddr} entry {entry_index} shorter than \
             j_file_extent_key_t (16 bytes), got {}",
            key.len()
        )));
    }
    let logical_addr = le_u64(key, 8);
    Ok(FsRecordKey::FileExtent { logical_addr })
}

fn decode_drec_key(
    key: &[u8],
    node_paddr: u64,
    entry_index: u32,
) -> Result<FsRecordKey, ScanError> {
    // Prefer the hashed form (j_drec_hashed_key_t) which is what every
    // current macOS APFS volume produces. Fall back to the unhashed form if
    // the hashed length read does not fit. SR-016: if neither form fits,
    // hard-stop instead of guessing.
    if key.len() >= 12 {
        let hash_word = le_u32(key, 8);
        let name_len = hash_word & J_DREC_LEN_MASK;
        if name_len > 0 && (12 + name_len as usize) <= key.len() {
            let name_bytes = &key[12..12 + name_len as usize];
            let name = decode_name(name_bytes, "drec_hashed_key", node_paddr, entry_index)?;
            return Ok(FsRecordKey::Named {
                raw_key_form: "hashed",
                name_len,
                name,
                name_bytes_hex: to_hex(name_bytes),
            });
        }
    }
    if key.len() < 10 {
        return Err(ScanError::InvalidObject(format!(
            "drec key at node {node_paddr} entry {entry_index} shorter than 10-byte j_drec_key_t"
        )));
    }
    let name_len = le_u16(key, 8) as u32;
    if name_len == 0 || 10 + name_len as usize > key.len() {
        return Err(ScanError::InvalidObject(format!(
            "drec key at node {node_paddr} entry {entry_index} has name_len {name_len} \
             that does not fit in {} bytes",
            key.len()
        )));
    }
    let name_bytes = &key[10..10 + name_len as usize];
    let name = decode_name(name_bytes, "drec_key", node_paddr, entry_index)?;
    Ok(FsRecordKey::Named {
        raw_key_form: "unhashed",
        name_len,
        name,
        name_bytes_hex: to_hex(name_bytes),
    })
}

fn decode_xattr_key(
    key: &[u8],
    node_paddr: u64,
    entry_index: u32,
) -> Result<FsRecordKey, ScanError> {
    if key.len() < 10 {
        return Err(ScanError::InvalidObject(format!(
            "xattr key at node {node_paddr} entry {entry_index} shorter than 10-byte j_xattr_key_t"
        )));
    }
    let name_len = le_u16(key, 8) as u32;
    if name_len == 0 || 10 + name_len as usize > key.len() {
        return Err(ScanError::InvalidObject(format!(
            "xattr key at node {node_paddr} entry {entry_index} has name_len {name_len} \
             that does not fit in {} bytes",
            key.len()
        )));
    }
    let name_bytes = &key[10..10 + name_len as usize];
    let name = decode_name(name_bytes, "xattr_key", node_paddr, entry_index)?;
    Ok(FsRecordKey::Named {
        raw_key_form: "xattr",
        name_len,
        name,
        name_bytes_hex: to_hex(name_bytes),
    })
}

fn decode_sibling_link_key(
    key: &[u8],
    node_paddr: u64,
    entry_index: u32,
) -> Result<FsRecordKey, ScanError> {
    if key.len() < 16 {
        return Err(ScanError::InvalidObject(format!(
            "sibling_link key at node {node_paddr} entry {entry_index} shorter than 16 bytes"
        )));
    }
    Ok(FsRecordKey::SiblingLink {
        sibling_id: le_u64(key, 8),
    })
}

/// SR-016 name validation: the bytes must be valid UTF-8 after stripping at
/// most one trailing NUL. An empty (or pure-NUL) name is rejected.
fn decode_name(
    name_bytes: &[u8],
    context: &str,
    node_paddr: u64,
    entry_index: u32,
) -> Result<String, ScanError> {
    if name_bytes.is_empty() {
        return Err(ScanError::InvalidObject(format!(
            "{context} at node {node_paddr} entry {entry_index} has empty name"
        )));
    }
    let stripped: &[u8] = if name_bytes.last() == Some(&0u8) {
        &name_bytes[..name_bytes.len() - 1]
    } else {
        name_bytes
    };
    if stripped.is_empty() {
        return Err(ScanError::InvalidObject(format!(
            "{context} at node {node_paddr} entry {entry_index} has only-NUL name"
        )));
    }
    if stripped.contains(&0) {
        return Err(ScanError::InvalidObject(format!(
            "{context} at node {node_paddr} entry {entry_index} contains embedded NUL"
        )));
    }
    std::str::from_utf8(stripped)
        .map(|s| s.to_string())
        .map_err(|err| {
            ScanError::InvalidObject(format!(
                "{context} at node {node_paddr} entry {entry_index} is not valid UTF-8: {err}"
            ))
        })
}

// ---- value decoding ----------------------------------------------------- //

fn decode_value(
    raw_type: u8,
    value: &[u8],
    node_paddr: u64,
    entry_index: u32,
    notes: &mut Vec<String>,
) -> Result<FsRecordValue, ScanError> {
    match raw_type {
        RAW_TYPE_INODE => Ok(FsRecordValue::Inode(decode_inode(
            value,
            node_paddr,
            entry_index,
            notes,
        )?)),
        RAW_TYPE_XATTR => Ok(FsRecordValue::Xattr(decode_xattr(
            value,
            node_paddr,
            entry_index,
        )?)),
        RAW_TYPE_SIBLING_LINK => Ok(FsRecordValue::SiblingLink(decode_sibling_link(
            value,
            node_paddr,
            entry_index,
        )?)),
        RAW_TYPE_DSTREAM_ID => {
            let refcnt = if value.len() >= 4 {
                Some(le_u32(value, 0))
            } else {
                None
            };
            Ok(FsRecordValue::DstreamId { refcnt })
        }
        RAW_TYPE_DIR_REC => Ok(FsRecordValue::DirRec(decode_drec(
            value,
            node_paddr,
            entry_index,
            notes,
        )?)),
        RAW_TYPE_SIBLING_MAP => {
            if value.len() < SIBLING_MAP_VALUE_SIZE {
                return Err(ScanError::InvalidObject(format!(
                    "sibling_map value at node {node_paddr} entry {entry_index} shorter than 8 bytes"
                )));
            }
            Ok(FsRecordValue::SiblingMap {
                file_id: Some(le_u64(value, 0)),
            })
        }
        RAW_TYPE_FILE_EXTENT => Ok(FsRecordValue::FileExtent(decode_file_extent(
            value,
            node_paddr,
            entry_index,
        )?)),
        _ => Ok(FsRecordValue::Unsupported {
            reason: "record family is outside the v1 body decoder allowlist",
        }),
    }
}

fn decode_inode(
    value: &[u8],
    node_paddr: u64,
    entry_index: u32,
    notes: &mut Vec<String>,
) -> Result<InodeBody, ScanError> {
    if value.len() < INODE_FIXED_SIZE {
        return Err(ScanError::InvalidObject(format!(
            "inode value at node {node_paddr} entry {entry_index} shorter than j_inode_val_t \
             ({INODE_FIXED_SIZE} bytes)"
        )));
    }
    let internal_flags = le_u64(value, 0x30);
    let xfield_blob = &value[INODE_FIXED_SIZE..];
    let xfield_result = decode_xfields(xfield_blob, "inode", node_paddr, entry_index)?;
    let mut dstream: Option<DstreamFields> = None;
    let mut sparse_bytes: Option<u64> = None;
    let mut inode_name: Option<String> = None;
    for field in &xfield_result.fields {
        match (&field.interpreted, field.x_type) {
            (Some(XfieldInterpreted::Dstream { value }), INO_EXT_TYPE_DSTREAM) => {
                dstream = Some(value.clone());
            }
            (Some(XfieldInterpreted::U64 { value }), INO_EXT_TYPE_SPARSE_BYTES) => {
                sparse_bytes = Some(*value);
            }
            (Some(XfieldInterpreted::Utf8 { value }), INO_EXT_TYPE_NAME) => {
                inode_name = Some(value.clone());
            }
            _ => {}
        }
    }
    if xfield_result.unused_trailing_bytes < 0 {
        notes.push(format!(
            "xfield padded total exceeds inode blob length ({} bytes); inspect candidate \
             SR-016 signature",
            xfield_blob.len()
        ));
    }
    Ok(InodeBody {
        parent_id: le_u64(value, 0x00),
        private_id: le_u64(value, 0x08),
        internal_flags,
        nchildren_or_nlink: i32::from_le_bytes(
            value[0x38..0x3c]
                .try_into()
                .expect("i32 nchildren_or_nlink"),
        ),
        bsd_flags: le_u32(value, 0x44),
        owner: le_u32(value, 0x48),
        group: le_u32(value, 0x4c),
        mode: le_u16(value, 0x50),
        uncompressed_size: le_u64(value, 0x54),
        has_uncompressed_size: (internal_flags & INODE_HAS_UNCOMPRESSED_SIZE) != 0,
        xfields: xfield_result.fields,
        xfield_used_data: xfield_result.xf_used_data,
        xfield_padded_total: xfield_result.padded_total,
        xfield_unused_trailing_bytes: xfield_result.unused_trailing_bytes,
        dstream,
        sparse_bytes,
        inode_name,
    })
}

fn decode_drec(
    value: &[u8],
    node_paddr: u64,
    entry_index: u32,
    notes: &mut Vec<String>,
) -> Result<DirRecBody, ScanError> {
    if value.len() < DREC_PREFIX_SIZE {
        return Err(ScanError::InvalidObject(format!(
            "drec value at node {node_paddr} entry {entry_index} shorter than j_drec_val_t \
             ({DREC_PREFIX_SIZE} bytes)"
        )));
    }
    let flags = le_u16(value, 0x10);
    let entry_type = (flags & DREC_TYPE_MASK) as u8;
    if !is_known_drec_entry_type(entry_type) {
        return Err(ScanError::InvalidObject(format!(
            "drec value at node {node_paddr} entry {entry_index} has unknown POSIX entry type \
             {entry_type:#x}"
        )));
    }
    let xfield_blob = &value[DREC_PREFIX_SIZE..];
    let xfield_result = decode_xfields(xfield_blob, "drec", node_paddr, entry_index)?;
    let mut sibling_id: Option<u64> = None;
    for field in &xfield_result.fields {
        if field.x_type == DREC_EXT_TYPE_SIBLING_ID {
            if let Some(XfieldInterpreted::U64 { value }) = &field.interpreted {
                sibling_id = Some(*value);
            }
        }
    }
    if xfield_result.unused_trailing_bytes < 0 {
        notes.push(format!(
            "xfield padded total exceeds drec blob length ({} bytes); inspect candidate \
             SR-016 signature",
            xfield_blob.len()
        ));
    }
    Ok(DirRecBody {
        file_id: le_u64(value, 0),
        date_added: le_u64(value, 8),
        flags,
        entry_type,
        sibling_id,
        xfields: xfield_result.fields,
        xfield_used_data: xfield_result.xf_used_data,
        xfield_padded_total: xfield_result.padded_total,
        xfield_unused_trailing_bytes: xfield_result.unused_trailing_bytes,
    })
}

fn decode_xattr(value: &[u8], node_paddr: u64, entry_index: u32) -> Result<XattrBody, ScanError> {
    if value.len() < XATTR_VALUE_HEADER_SIZE {
        return Err(ScanError::InvalidObject(format!(
            "xattr value at node {node_paddr} entry {entry_index} shorter than \
             j_xattr_val_t ({XATTR_VALUE_HEADER_SIZE} bytes)"
        )));
    }
    let flags = le_u16(value, 0);
    let xdata_len = le_u16(value, 2);
    let unknown_bits = flags & !XATTR_KNOWN_FLAG_BITS;
    if unknown_bits != 0 {
        return Err(ScanError::InvalidObject(format!(
            "xattr value at node {node_paddr} entry {entry_index} has unknown flag bits \
             {unknown_bits:#x}"
        )));
    }
    let embedded = flags & XATTR_DATA_EMBEDDED != 0;
    let stream = flags & XATTR_DATA_STREAM != 0;
    if embedded == stream {
        return Err(ScanError::InvalidObject(format!(
            "xattr value at node {node_paddr} entry {entry_index} sets both or neither of \
             XATTR_DATA_EMBEDDED/XATTR_DATA_STREAM (flags={flags:#x})"
        )));
    }
    let payload = &value[XATTR_VALUE_HEADER_SIZE..];
    if payload.len() != xdata_len as usize {
        return Err(ScanError::InvalidObject(format!(
            "xattr value at node {node_paddr} entry {entry_index} body length {} disagrees \
             with xdata_len {xdata_len}",
            payload.len()
        )));
    }
    let mut body = XattrBody {
        flags,
        xdata_len,
        embedded,
        stream,
        payload_hex: to_hex(payload),
        payload_utf8: None,
        stream_xattr_obj_id: None,
        stream_dstream: None,
    };
    if embedded {
        // Symlink targets are stored embedded; preserve the UTF-8 form if it
        // decodes, but do not fail on non-UTF-8 here (xattr payloads can be
        // arbitrary bytes). The caller's symlink check enforces UTF-8 for
        // `com.apple.fs.symlink` only.
        if let Ok(text) = std::str::from_utf8(strip_trailing_nul(payload)) {
            body.payload_utf8 = Some(text.to_string());
        }
    } else {
        if payload.len() < J_XATTR_DSTREAM_SIZE {
            return Err(ScanError::InvalidObject(format!(
                "xattr value at node {node_paddr} entry {entry_index} is stream-backed but body \
                 length {} is shorter than j_xattr_dstream_t ({J_XATTR_DSTREAM_SIZE})",
                payload.len()
            )));
        }
        body.stream_xattr_obj_id = Some(le_u64(payload, 0));
        body.stream_dstream = Some(parse_dstream(&payload[8..8 + DSTREAM_SIZE])?);
    }
    Ok(body)
}

fn decode_sibling_link(
    value: &[u8],
    node_paddr: u64,
    entry_index: u32,
) -> Result<SiblingLinkBody, ScanError> {
    if value.len() < SIBLING_LINK_FIXED_PREFIX {
        return Err(ScanError::InvalidObject(format!(
            "sibling_link value at node {node_paddr} entry {entry_index} shorter than 10 bytes"
        )));
    }
    let parent_id = le_u64(value, 0);
    let name_len = le_u16(value, 8);
    let end = SIBLING_LINK_FIXED_PREFIX + name_len as usize;
    if name_len == 0 || end > value.len() {
        return Err(ScanError::InvalidObject(format!(
            "sibling_link value at node {node_paddr} entry {entry_index} has name_len {name_len} \
             that does not fit in {} bytes",
            value.len()
        )));
    }
    let name_bytes = &value[SIBLING_LINK_FIXED_PREFIX..end];
    let name = decode_name(name_bytes, "sibling_link_value", node_paddr, entry_index)?;
    Ok(SiblingLinkBody {
        parent_id,
        name_len,
        name,
        name_bytes_hex: to_hex(name_bytes),
    })
}

/// `j_file_extent_val_t` (24 bytes): `len_and_flags` (8) +
/// `phys_block_num` (8) + `crypto_id` (8). High 4 bits of
/// `len_and_flags` are flags; low 60 bits are length in *bytes*.
///
/// Note: APFS's published j_file_extent_val_t is 24 bytes. Older
/// fixtures or compressed variants may omit `crypto_id` (16-byte
/// short form), but every macOS-produced volume we've validated
/// against emits 24 bytes; the decoder is strict.
fn decode_file_extent(
    value: &[u8],
    node_paddr: u64,
    entry_index: u32,
) -> Result<FileExtentBody, ScanError> {
    if value.len() < FILE_EXTENT_VALUE_MIN {
        return Err(ScanError::InvalidObject(format!(
            "file_extent value at node {node_paddr} entry {entry_index} \
             shorter than {FILE_EXTENT_VALUE_MIN} bytes (got {})",
            value.len()
        )));
    }
    let len_and_flags = le_u64(value, 0);
    let length_bytes = len_and_flags & FS_OBJECT_ID_MASK;
    let flags = ((len_and_flags >> FS_RECORD_TYPE_SHIFT) as u8) & 0xF;
    let phys_block_num = le_u64(value, 8);
    let crypto_id = le_u64(value, 16);
    Ok(FileExtentBody {
        length_bytes,
        flags,
        phys_block_num,
        crypto_id,
    })
}

fn parse_dstream(bytes: &[u8]) -> Result<DstreamFields, ScanError> {
    if bytes.len() < DSTREAM_SIZE {
        return Err(ScanError::InvalidObject(format!(
            "dstream value is {} bytes, expected {DSTREAM_SIZE}",
            bytes.len()
        )));
    }
    Ok(DstreamFields {
        size: le_u64(bytes, 0),
        alloced_size: le_u64(bytes, 8),
        default_crypto_id: le_u64(bytes, 16),
        total_bytes_written: le_u64(bytes, 24),
        total_bytes_read: le_u64(bytes, 32),
    })
}

fn strip_trailing_nul(bytes: &[u8]) -> &[u8] {
    if bytes.last() == Some(&0) {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    }
}

fn is_known_drec_entry_type(value: u8) -> bool {
    matches!(
        value,
        DT_FIFO | DT_CHR | DT_DIR | DT_BLK | DT_REG | DT_LNK | DT_SOCK | DT_WHT
    )
}

// ---- xfield decoder (SR-015 single cursor rule + SR-016 hard stops) ---- //

struct XfieldResult {
    fields: Vec<XfieldEntry>,
    xf_used_data: u32,
    padded_total: u32,
    unused_trailing_bytes: i32,
}

fn decode_xfields(
    blob: &[u8],
    context: &str,
    node_paddr: u64,
    entry_index: u32,
) -> Result<XfieldResult, ScanError> {
    if blob.is_empty() {
        return Ok(XfieldResult {
            fields: Vec::new(),
            xf_used_data: 0,
            padded_total: 0,
            unused_trailing_bytes: 0,
        });
    }
    if blob.len() < 4 {
        return Err(ScanError::InvalidObject(format!(
            "{context} xfield blob at node {node_paddr} entry {entry_index} is {} bytes; need \
             4 for xf_blob_t",
            blob.len()
        )));
    }
    let xf_num_exts = le_u16(blob, 0) as usize;
    let xf_used_data = le_u16(blob, 2) as u32;
    let metadata_end = 4usize
        .checked_add(xf_num_exts.checked_mul(4).ok_or_else(|| {
            ScanError::InvalidObject(format!(
                "{context} xfield metadata length overflow at node {node_paddr} \
                 entry {entry_index}"
            ))
        })?)
        .ok_or_else(|| {
            ScanError::InvalidObject(format!(
                "{context} xfield metadata end overflow at node {node_paddr} \
                 entry {entry_index}"
            ))
        })?;
    if metadata_end > blob.len() {
        return Err(ScanError::InvalidObject(format!(
            "{context} xfield metadata at node {node_paddr} entry {entry_index} runs past \
             blob length {}",
            blob.len()
        )));
    }

    let mut seen_types: Vec<u8> = Vec::with_capacity(xf_num_exts);
    let mut cursor = metadata_end;
    let mut fields: Vec<XfieldEntry> = Vec::with_capacity(xf_num_exts);
    let mut padded_total: u32 = 0;
    for index in 0..xf_num_exts {
        let meta_off = 4 + index * 4;
        let x_type = blob[meta_off];
        let x_flags = blob[meta_off + 1];
        let x_size = le_u16(blob, meta_off + 2);
        if seen_types.contains(&x_type) {
            return Err(ScanError::InvalidObject(format!(
                "{context} xfield blob at node {node_paddr} entry {entry_index} carries \
                 duplicate x_type {x_type:#x}"
            )));
        }
        seen_types.push(x_type);
        let end_value = cursor.checked_add(x_size as usize).ok_or_else(|| {
            ScanError::InvalidObject(format!(
                "{context} xfield value cursor overflow at node {node_paddr} entry {entry_index}"
            ))
        })?;
        if end_value > blob.len() {
            return Err(ScanError::InvalidObject(format!(
                "{context} xfield value at node {node_paddr} entry {entry_index} cursor {cursor} \
                 size {x_size} exceeds blob length {}",
                blob.len()
            )));
        }
        let data = &blob[cursor..end_value];
        let padded_length = round_up_8(x_size as u32);
        validate_required_xfield_size(context, x_type, x_size, node_paddr, entry_index)?;
        let interpreted = interpret_xfield(x_type, data);
        fields.push(XfieldEntry {
            x_type,
            x_flags,
            x_size,
            padded_length,
            value_hex: to_hex(data),
            interpreted,
        });
        padded_total = padded_total.checked_add(padded_length).ok_or_else(|| {
            ScanError::InvalidObject(format!(
                "{context} xfield padded total overflow at node {node_paddr} entry {entry_index}"
            ))
        })?;
        cursor = cursor.checked_add(padded_length as usize).ok_or_else(|| {
            ScanError::InvalidObject(format!(
                "{context} xfield cursor overflow at node {node_paddr} entry {entry_index}"
            ))
        })?;
        if cursor > blob.len() && index + 1 < xf_num_exts {
            return Err(ScanError::InvalidObject(format!(
                "{context} xfield padded cursor {cursor} at node {node_paddr} entry {entry_index} \
                 exceeds blob length {} with more fields to read",
                blob.len()
            )));
        }
    }

    if xf_used_data != padded_total {
        return Err(ScanError::InvalidObject(format!(
            "{context} xfield blob at node {node_paddr} entry {entry_index} has \
             xf_used_data={xf_used_data} but sum(round_up(x_size, 8))={padded_total}"
        )));
    }

    let total_consumed = metadata_end as i64 + padded_total as i64;
    let unused = blob.len() as i64 - total_consumed;
    Ok(XfieldResult {
        fields,
        xf_used_data,
        padded_total,
        unused_trailing_bytes: unused as i32,
    })
}

fn validate_required_xfield_size(
    context: &str,
    x_type: u8,
    x_size: u16,
    node_paddr: u64,
    entry_index: u32,
) -> Result<(), ScanError> {
    let (expected, name) = match (context, x_type) {
        ("inode", INO_EXT_TYPE_DSTREAM) => (Some(40u16), "INO_EXT_TYPE_DSTREAM"),
        ("inode", INO_EXT_TYPE_SPARSE_BYTES) => (Some(8u16), "INO_EXT_TYPE_SPARSE_BYTES"),
        ("drec", DREC_EXT_TYPE_SIBLING_ID) => (Some(8u16), "DREC_EXT_TYPE_SIBLING_ID"),
        _ => (None, ""),
    };
    if let Some(expected_size) = expected {
        if x_size != expected_size {
            return Err(ScanError::InvalidObject(format!(
                "{context} xfield {name} at node {node_paddr} entry {entry_index} has x_size \
                 {x_size}, expected {expected_size}"
            )));
        }
    }
    Ok(())
}

fn interpret_xfield(x_type: u8, data: &[u8]) -> Option<XfieldInterpreted> {
    match x_type {
        // u64-valued xfields the parser tracks. INO_EXT_TYPE_DOCUMENT_ID (3),
        // INO_EXT_TYPE_SNAP_XID (1), and *_RDEV (14) all encode 8-byte u64
        // values. DREC_EXT_TYPE_SIBLING_ID (1, drec context) is also a u64.
        INO_EXT_TYPE_SNAP_XID
        | INO_EXT_TYPE_DELTA_TREE_OID
        | INO_EXT_TYPE_DOCUMENT_ID
        | INO_EXT_TYPE_PREV_FSIZE
        | INO_EXT_TYPE_SPARSE_BYTES
        | INO_EXT_TYPE_RDEV
        | INO_EXT_TYPE_ORIG_SYNC_ROOT_ID => {
            if data.len() == 8 {
                Some(XfieldInterpreted::U64 {
                    value: le_u64(data, 0),
                })
            } else {
                None
            }
        }
        INO_EXT_TYPE_NAME => {
            let stripped = strip_trailing_nul(data);
            match std::str::from_utf8(stripped) {
                Ok(text) => Some(XfieldInterpreted::Utf8 {
                    value: text.to_string(),
                }),
                Err(_) => None,
            }
        }
        INO_EXT_TYPE_DSTREAM => {
            if data.len() >= DSTREAM_SIZE {
                match parse_dstream(&data[..DSTREAM_SIZE]) {
                    Ok(d) => Some(XfieldInterpreted::Dstream { value: d }),
                    Err(_) => None,
                }
            } else {
                None
            }
        }
        // Reserved or out-of-scope xfield types are recorded structurally
        // but not interpreted.
        INO_EXT_TYPE_RESERVED_6
        | INO_EXT_TYPE_FINDER_INFO
        | INO_EXT_TYPE_RESERVED_9
        | INO_EXT_TYPE_DIR_STATS_KEY
        | INO_EXT_TYPE_FS_UUID
        | INO_EXT_TYPE_RESERVED_12
        | INO_EXT_TYPE_PURGEABLE_FLAGS => None,
        _ => None,
    }
}

fn round_up_8(value: u32) -> u32 {
    (value + 7) & !7
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

// ---- tests -------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_io::{put_u16, put_u64};

    fn drec_hashed_key(object_id: u64, name: &str) -> Vec<u8> {
        let mut key = Vec::new();
        let key_word =
            (object_id & FS_OBJECT_ID_MASK) | ((RAW_TYPE_DIR_REC as u64) << FS_RECORD_TYPE_SHIFT);
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u32 + 1; // include trailing NUL
        let hash: u32 = 0;
        key.extend_from_slice(&key_word.to_le_bytes());
        key.extend_from_slice(&(hash | (name_len & J_DREC_LEN_MASK)).to_le_bytes());
        key.extend_from_slice(name_bytes);
        key.push(0);
        key
    }

    fn drec_value(file_id: u64, entry_type: u8) -> Vec<u8> {
        let mut value = vec![0u8; DREC_PREFIX_SIZE];
        put_u64(&mut value, 0, file_id);
        put_u64(&mut value, 8, 0);
        put_u16(&mut value, 0x10, entry_type as u16);
        value
    }

    fn inode_value() -> Vec<u8> {
        vec![0u8; INODE_FIXED_SIZE]
    }

    fn inode_value_with_xfields(xfields: &[(u8, u8, &[u8])]) -> Vec<u8> {
        let mut blob = Vec::new();
        let num = xfields.len() as u16;
        let mut used: u32 = 0;
        // Header placeholders.
        blob.extend_from_slice(&num.to_le_bytes());
        blob.extend_from_slice(&0u16.to_le_bytes());
        // Metadata table.
        for (x_type, x_flags, data) in xfields {
            blob.push(*x_type);
            blob.push(*x_flags);
            blob.extend_from_slice(&(data.len() as u16).to_le_bytes());
        }
        // Values, padded per SR-015.
        for (_, _, data) in xfields {
            blob.extend_from_slice(data);
            let pad = round_up_8(data.len() as u32) - data.len() as u32;
            blob.extend(std::iter::repeat_n(0u8, pad as usize));
            used += round_up_8(data.len() as u32);
        }
        // Patch xf_used_data.
        blob[2..4].copy_from_slice(&(used as u16).to_le_bytes());
        let mut value = inode_value();
        value.extend_from_slice(&blob);
        value
    }

    fn xattr_key(object_id: u64, name: &str) -> Vec<u8> {
        let mut key = Vec::new();
        let key_word =
            (object_id & FS_OBJECT_ID_MASK) | ((RAW_TYPE_XATTR as u64) << FS_RECORD_TYPE_SHIFT);
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u16 + 1;
        key.extend_from_slice(&key_word.to_le_bytes());
        key.extend_from_slice(&name_len.to_le_bytes());
        key.extend_from_slice(name_bytes);
        key.push(0);
        key
    }

    fn xattr_value(flags: u16, payload: &[u8]) -> Vec<u8> {
        let mut value = Vec::new();
        value.extend_from_slice(&flags.to_le_bytes());
        value.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        value.extend_from_slice(payload);
        value
    }

    fn inode_key(object_id: u64) -> Vec<u8> {
        let key_word =
            (object_id & FS_OBJECT_ID_MASK) | ((RAW_TYPE_INODE as u64) << FS_RECORD_TYPE_SHIFT);
        key_word.to_le_bytes().to_vec()
    }

    // ---- positive baseline ---- //

    #[test]
    fn decode_inode_with_dstream_and_name_xfields() {
        let name = b"hello\x00\x00\x00"; // 8-byte name with trailing NUL padding
        let mut dstream = vec![0u8; DSTREAM_SIZE];
        put_u64(&mut dstream, 0, 1234);
        put_u64(&mut dstream, 8, 4096);
        put_u64(&mut dstream, 16, 0);
        put_u64(&mut dstream, 24, 1234);
        put_u64(&mut dstream, 32, 0);
        let value = inode_value_with_xfields(&[
            (INO_EXT_TYPE_NAME, 0, &name[..6]),
            (INO_EXT_TYPE_DSTREAM, 0, &dstream),
        ]);
        let key = inode_key(42);
        let row = decode_fs_record(100, 0, &key, &value).expect("inode decode succeeds");
        match row.value {
            FsRecordValue::Inode(inode) => {
                assert_eq!(inode.inode_name.as_deref(), Some("hello"));
                let d = inode.dstream.unwrap();
                assert_eq!(d.size, 1234);
                assert_eq!(d.total_bytes_written, 1234);
                assert_eq!(inode.xfield_used_data, inode.xfield_padded_total);
            }
            other => panic!("unexpected value variant: {other:?}"),
        }
    }

    #[test]
    fn decode_drec_with_sibling_id() {
        let key = drec_hashed_key(2, "alpha.txt");
        let mut sib = vec![0u8; 8];
        put_u64(&mut sib, 0, 7777);
        let mut value = drec_value(42, DT_REG);
        let blob = {
            let mut b = Vec::new();
            b.extend_from_slice(&1u16.to_le_bytes()); // xf_num_exts
            b.extend_from_slice(&8u16.to_le_bytes()); // xf_used_data
            b.push(DREC_EXT_TYPE_SIBLING_ID);
            b.push(0);
            b.extend_from_slice(&8u16.to_le_bytes()); // x_size=8
            b.extend_from_slice(&sib);
            b
        };
        value.extend_from_slice(&blob);
        let row = decode_fs_record(100, 1, &key, &value).expect("drec decode succeeds");
        match row.value {
            FsRecordValue::DirRec(drec) => {
                assert_eq!(drec.file_id, 42);
                assert_eq!(drec.entry_type, DT_REG);
                assert_eq!(drec.sibling_id, Some(7777));
            }
            other => panic!("unexpected value variant: {other:?}"),
        }
    }

    #[test]
    fn decode_sibling_link_with_name() {
        let mut key = vec![0u8; 16];
        let key_word =
            (5u64 & FS_OBJECT_ID_MASK) | ((RAW_TYPE_SIBLING_LINK as u64) << FS_RECORD_TYPE_SHIFT);
        put_u64(&mut key, 0, key_word);
        put_u64(&mut key, 8, 42);
        let mut value = Vec::new();
        value.extend_from_slice(&100u64.to_le_bytes()); // parent_id
        let name = b"file.txt\x00";
        value.extend_from_slice(&(name.len() as u16).to_le_bytes());
        value.extend_from_slice(name);
        let row = decode_fs_record(100, 0, &key, &value).expect("sibling_link decode succeeds");
        match row.value {
            FsRecordValue::SiblingLink(body) => {
                assert_eq!(body.parent_id, 100);
                assert_eq!(body.name, "file.txt");
            }
            other => panic!("unexpected value variant: {other:?}"),
        }
    }

    // ---- SR-016 fail-closed cases ---- //

    #[test]
    fn hard_stops_on_short_inode_value() {
        let key = inode_key(42);
        let short = vec![0u8; INODE_FIXED_SIZE - 1];
        let err = decode_fs_record(100, 0, &key, &short).expect_err("short inode is rejected");
        assert!(
            matches!(err, ScanError::InvalidObject(r) if r.contains("shorter than j_inode_val_t")),
            "unexpected error variant",
        );
    }

    #[test]
    fn hard_stops_on_short_drec_value() {
        let key = drec_hashed_key(2, "a");
        let short = vec![0u8; DREC_PREFIX_SIZE - 1];
        let err = decode_fs_record(100, 0, &key, &short).expect_err("short drec is rejected");
        assert!(
            matches!(err, ScanError::InvalidObject(r) if r.contains("shorter than j_drec_val_t"))
        );
    }

    #[test]
    fn hard_stops_on_drec_name_too_long() {
        // hashed key with hash | name_len indicating a name that would run
        // past the end of the key bytes.
        let mut key = Vec::new();
        let key_word =
            (2u64 & FS_OBJECT_ID_MASK) | ((RAW_TYPE_DIR_REC as u64) << FS_RECORD_TYPE_SHIFT);
        key.extend_from_slice(&key_word.to_le_bytes());
        // hash << 10 | name_len; name_len=200 but only 4 actual bytes follow.
        let hash_word: u32 = 200u32 & J_DREC_LEN_MASK;
        key.extend_from_slice(&hash_word.to_le_bytes());
        key.extend_from_slice(b"abc\x00");
        let value = drec_value(1, DT_REG);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("name_len too long rejected");
        // Falls through to unhashed branch then fails; either way is a typed
        // hard stop on a name-length lie.
        assert!(matches!(err, ScanError::InvalidObject(r)
            if r.contains("name_len") || r.contains("not valid UTF-8") || r.contains("embedded NUL")));
    }

    #[test]
    fn hard_stops_on_embedded_nul_in_drec_name() {
        // hashed form with a name "ab\0cd"
        let mut key = Vec::new();
        let key_word =
            (2u64 & FS_OBJECT_ID_MASK) | ((RAW_TYPE_DIR_REC as u64) << FS_RECORD_TYPE_SHIFT);
        key.extend_from_slice(&key_word.to_le_bytes());
        let name = b"ab\x00cd\x00";
        let hash_word: u32 = (name.len() as u32) & J_DREC_LEN_MASK;
        key.extend_from_slice(&hash_word.to_le_bytes());
        key.extend_from_slice(name);
        let value = drec_value(1, DT_REG);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("embedded NUL rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("embedded NUL")));
    }

    #[test]
    fn hard_stops_on_non_utf8_drec_name() {
        let mut key = Vec::new();
        let key_word =
            (2u64 & FS_OBJECT_ID_MASK) | ((RAW_TYPE_DIR_REC as u64) << FS_RECORD_TYPE_SHIFT);
        key.extend_from_slice(&key_word.to_le_bytes());
        let name: &[u8] = &[0xff, 0xff, 0xff, 0x00];
        let hash_word: u32 = (name.len() as u32) & J_DREC_LEN_MASK;
        key.extend_from_slice(&hash_word.to_le_bytes());
        key.extend_from_slice(name);
        let value = drec_value(1, DT_REG);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("non-utf8 rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("not valid UTF-8")));
    }

    #[test]
    fn hard_stops_on_unknown_drec_entry_type() {
        let key = drec_hashed_key(2, "alpha.txt");
        let value = drec_value(1, 0x9); // not in the known POSIX set
        let err = decode_fs_record(100, 0, &key, &value).expect_err("unknown drec type rejected");
        assert!(
            matches!(err, ScanError::InvalidObject(r) if r.contains("unknown POSIX entry type"))
        );
    }

    #[test]
    fn hard_stops_on_duplicate_xfield_types() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&2u16.to_le_bytes()); // xf_num_exts
        blob.extend_from_slice(&16u16.to_le_bytes()); // xf_used_data
        blob.push(INO_EXT_TYPE_DOCUMENT_ID);
        blob.push(0);
        blob.extend_from_slice(&8u16.to_le_bytes());
        blob.push(INO_EXT_TYPE_DOCUMENT_ID);
        blob.push(0);
        blob.extend_from_slice(&8u16.to_le_bytes());
        blob.extend_from_slice(&[0u8; 8]);
        blob.extend_from_slice(&[0u8; 8]);
        let mut value = inode_value();
        value.extend_from_slice(&blob);
        let key = inode_key(42);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("duplicate xfield rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("duplicate x_type")));
    }

    #[test]
    fn hard_stops_on_xf_used_data_mismatch() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u16.to_le_bytes());
        blob.extend_from_slice(&7u16.to_le_bytes()); // wrong used_data
        blob.push(INO_EXT_TYPE_NAME);
        blob.push(0);
        blob.extend_from_slice(&5u16.to_le_bytes()); // x_size=5, padded=8
        blob.extend_from_slice(b"hello\x00\x00\x00");
        let mut value = inode_value();
        value.extend_from_slice(&blob);
        let key = inode_key(42);
        let err =
            decode_fs_record(100, 0, &key, &value).expect_err("xf_used_data mismatch rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("xf_used_data=")));
    }

    #[test]
    fn hard_stops_on_xfield_value_out_of_bounds() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u16.to_le_bytes());
        blob.extend_from_slice(&16u16.to_le_bytes());
        blob.push(INO_EXT_TYPE_NAME);
        blob.push(0);
        blob.extend_from_slice(&20u16.to_le_bytes()); // claims 20 bytes
        blob.extend_from_slice(b"only_a_few"); // 10 bytes
        let mut value = inode_value();
        value.extend_from_slice(&blob);
        let key = inode_key(42);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("value out of bounds rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("exceeds blob length")));
    }

    #[test]
    fn hard_stops_on_wrong_dstream_size() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u16.to_le_bytes());
        blob.extend_from_slice(&32u16.to_le_bytes());
        blob.push(INO_EXT_TYPE_DSTREAM);
        blob.push(0);
        blob.extend_from_slice(&32u16.to_le_bytes()); // wrong: should be 40
        blob.extend_from_slice(&[0u8; 32]);
        let mut value = inode_value();
        value.extend_from_slice(&blob);
        let key = inode_key(42);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("wrong dstream size rejected");
        assert!(matches!(err, ScanError::InvalidObject(r)
            if r.contains("INO_EXT_TYPE_DSTREAM") && r.contains("expected 40")));
    }

    #[test]
    fn hard_stops_on_wrong_sibling_id_size() {
        // Build a drec value with a 4-byte SIBLING_ID xfield.
        let key = drec_hashed_key(2, "alpha.txt");
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u16.to_le_bytes());
        blob.extend_from_slice(&8u16.to_le_bytes());
        blob.push(DREC_EXT_TYPE_SIBLING_ID);
        blob.push(0);
        blob.extend_from_slice(&4u16.to_le_bytes()); // wrong: should be 8
        blob.extend_from_slice(&[1u8, 2, 3, 4, 0, 0, 0, 0]);
        let mut value = drec_value(7, DT_REG);
        value.extend_from_slice(&blob);
        let err = decode_fs_record(100, 0, &key, &value)
            .expect_err("wrong DREC_EXT_TYPE_SIBLING_ID size rejected");
        assert!(matches!(err, ScanError::InvalidObject(r)
            if r.contains("DREC_EXT_TYPE_SIBLING_ID") && r.contains("expected 8")));
    }

    #[test]
    fn hard_stops_on_xattr_short_value() {
        let key = xattr_key(42, "com.apple.fs.symlink");
        let value = vec![0u8; XATTR_VALUE_HEADER_SIZE - 1];
        let err = decode_fs_record(100, 0, &key, &value).expect_err("short xattr rejected");
        assert!(
            matches!(err, ScanError::InvalidObject(r) if r.contains("shorter than \n             j_xattr_val_t") || r.contains("j_xattr_val_t"))
        );
    }

    #[test]
    fn hard_stops_on_xattr_both_flags() {
        let key = xattr_key(42, "com.apple.fs.symlink");
        let value = xattr_value(XATTR_DATA_EMBEDDED | XATTR_DATA_STREAM, b"x");
        let err = decode_fs_record(100, 0, &key, &value).expect_err("both flags rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("both or neither")));
    }

    #[test]
    fn hard_stops_on_xattr_neither_flag() {
        let key = xattr_key(42, "com.apple.fs.symlink");
        let value = xattr_value(0, b"x");
        let err = decode_fs_record(100, 0, &key, &value).expect_err("no flag rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("both or neither")));
    }

    #[test]
    fn hard_stops_on_xattr_unknown_flag_bits() {
        let key = xattr_key(42, "com.apple.fs.symlink");
        let value = xattr_value(XATTR_DATA_EMBEDDED | 0x8000, b"x");
        let err = decode_fs_record(100, 0, &key, &value).expect_err("unknown flag bits rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("unknown flag bits")));
    }

    #[test]
    fn hard_stops_on_xattr_xdata_len_mismatch() {
        let key = xattr_key(42, "com.apple.fs.symlink");
        let mut value = Vec::new();
        value.extend_from_slice(&XATTR_DATA_EMBEDDED.to_le_bytes());
        value.extend_from_slice(&7u16.to_le_bytes()); // claim 7 bytes
        value.extend_from_slice(b"only5"); // only 5
        let err = decode_fs_record(100, 0, &key, &value).expect_err("xdata_len mismatch rejected");
        assert!(
            matches!(err, ScanError::InvalidObject(r) if r.contains("disagrees with xdata_len"))
        );
    }

    #[test]
    fn hard_stops_on_xattr_stream_too_short() {
        let key = xattr_key(42, "com.apple.metadata.foo");
        // stream form needs at least 48 bytes; supply 16.
        let payload = vec![0u8; 16];
        let value = xattr_value(XATTR_DATA_STREAM, &payload);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("short stream rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("j_xattr_dstream_t")));
    }

    #[test]
    fn hard_stops_on_sibling_link_name_len_overflow() {
        let mut key = vec![0u8; 16];
        let key_word =
            (5u64 & FS_OBJECT_ID_MASK) | ((RAW_TYPE_SIBLING_LINK as u64) << FS_RECORD_TYPE_SHIFT);
        put_u64(&mut key, 0, key_word);
        put_u64(&mut key, 8, 1);
        let mut value = Vec::new();
        value.extend_from_slice(&0u64.to_le_bytes()); // parent_id
        value.extend_from_slice(&200u16.to_le_bytes()); // name_len lie
        value.extend_from_slice(b"only short bytes");
        let err = decode_fs_record(100, 0, &key, &value)
            .expect_err("sibling_link name_len overflow rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("name_len")));
    }

    #[test]
    fn hard_stops_on_sibling_map_short_value() {
        let mut key = vec![0u8; 8];
        let key_word =
            (5u64 & FS_OBJECT_ID_MASK) | ((RAW_TYPE_SIBLING_MAP as u64) << FS_RECORD_TYPE_SHIFT);
        put_u64(&mut key, 0, key_word);
        let value = vec![0u8; 4];
        let err = decode_fs_record(100, 0, &key, &value).expect_err("short sibling_map rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("shorter than 8")));
    }

    #[test]
    fn hard_stops_on_drec_xfield_metadata_out_of_bounds() {
        let key = drec_hashed_key(2, "alpha.txt");
        let mut blob = Vec::new();
        blob.extend_from_slice(&50u16.to_le_bytes()); // 50 xfields claimed
        blob.extend_from_slice(&0u16.to_le_bytes());
        let mut value = drec_value(1, DT_REG);
        value.extend_from_slice(&blob);
        let err = decode_fs_record(100, 0, &key, &value).expect_err("xfield metadata oob rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("runs past blob length")));
    }

    #[test]
    fn hard_stops_on_xfield_blob_shorter_than_header() {
        let key = drec_hashed_key(2, "alpha.txt");
        let mut value = drec_value(1, DT_REG);
        value.push(0x01); // 1 stray byte after the fixed body
        let err =
            decode_fs_record(100, 0, &key, &value).expect_err("xfield blob < 4 bytes rejected");
        assert!(matches!(err, ScanError::InvalidObject(r) if r.contains("xf_blob_t")));
    }

    #[test]
    fn fs_tree_internal_value_size_round_up() {
        assert_eq!(round_up_8(0), 0);
        assert_eq!(round_up_8(1), 8);
        assert_eq!(round_up_8(7), 8);
        assert_eq!(round_up_8(8), 8);
        assert_eq!(round_up_8(9), 16);
        assert_eq!(round_up_8(40), 40);
        assert_eq!(round_up_8(41), 48);
    }
}
