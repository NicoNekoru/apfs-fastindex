//! Generic APFS object header validation.
//!
//! `SR-007` says every native parser step must validate `obj_phys_t` before
//! trusting the body of an object. This module is the single chokepoint:
//! every other module reads a candidate block through here so we have one
//! checksum, type, OID, and scan-state-XID gate.

use serde::Serialize;

use crate::block_io::{checksum_matches, le_u32, le_u64};
use crate::ScanError;

/// Mask for the object kind bits of `o_type`.
pub(crate) const OBJECT_TYPE_MASK: u32 = 0x0000_ffff;
/// Mask for the storage-class flags of `o_type`. The high 16 bits encode
/// `OBJ_VIRTUAL`, `OBJ_EPHEMERAL`, `OBJ_PHYSICAL`, `OBJ_NOHEADER`,
/// `OBJ_ENCRYPTED`, and `OBJ_NONPERSISTENT`.
pub(crate) const OBJECT_TYPE_FLAGS_MASK: u32 = 0xffff_0000;

pub(crate) const OBJ_VIRTUAL: u32 = 0x0000_0000;
pub(crate) const OBJ_EPHEMERAL: u32 = 0x8000_0000;
pub(crate) const OBJ_PHYSICAL: u32 = 0x4000_0000;
pub(crate) const OBJ_NOHEADER: u32 = 0x2000_0000;
pub(crate) const OBJ_ENCRYPTED: u32 = 0x1000_0000;

pub(crate) const OBJECT_TYPE_NX_SUPERBLOCK: u32 = 0x0001;
pub(crate) const OBJECT_TYPE_BTREE: u32 = 0x0002;
pub(crate) const OBJECT_TYPE_BTREE_NODE: u32 = 0x0003;
pub(crate) const OBJECT_TYPE_OMAP: u32 = 0x000b;
pub(crate) const OBJECT_TYPE_CHECKPOINT_MAP: u32 = 0x000c;
pub(crate) const OBJECT_TYPE_FS: u32 = 0x000d;
pub(crate) const OBJECT_TYPE_FSTREE: u32 = 0x000e;

pub(crate) const APFS_MAGIC: u32 = 0x4253_5041;

pub(crate) const OBJ_HEADER_SIZE: usize = 32;

/// What the native parser knows about an APFS object before it reads the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObjectHeader {
    pub block_address: u64,
    pub checksum: u64,
    pub oid: u64,
    pub xid: u64,
    pub object_type_raw: u32,
    pub object_type: u32,
    pub object_type_flags: u32,
    pub object_subtype: u32,
}

impl ObjectHeader {
    pub fn from_block(block: &[u8], block_address: u64) -> Self {
        let object_type_raw = le_u32(block, 0x18);
        Self {
            block_address,
            checksum: le_u64(block, 0x00),
            oid: le_u64(block, 0x08),
            xid: le_u64(block, 0x10),
            object_type_raw,
            object_type: object_type_raw & OBJECT_TYPE_MASK,
            object_type_flags: object_type_raw & OBJECT_TYPE_FLAGS_MASK,
            object_subtype: le_u32(block, 0x1c),
        }
    }

    pub fn is_physical(&self) -> bool {
        self.object_type_flags & OBJ_PHYSICAL != 0
    }

    pub fn is_ephemeral(&self) -> bool {
        self.object_type_flags & OBJ_EPHEMERAL != 0
    }

    pub fn is_encrypted(&self) -> bool {
        self.object_type_flags & OBJ_ENCRYPTED != 0
    }

    pub fn is_noheader(&self) -> bool {
        self.object_type_flags & OBJ_NOHEADER != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ExpectedStorage {
    Virtual,
    Ephemeral,
    Physical,
    Any,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ObjectExpectation {
    pub object_type: u32,
    pub storage: ExpectedStorage,
    pub max_xid: Option<u64>,
    pub require_oid_eq_paddr: bool,
}

impl ObjectExpectation {
    pub fn physical(object_type: u32) -> Self {
        Self {
            object_type,
            storage: ExpectedStorage::Physical,
            max_xid: None,
            require_oid_eq_paddr: true,
        }
    }

    pub fn virtual_object(object_type: u32, max_xid: Option<u64>) -> Self {
        Self {
            object_type,
            storage: ExpectedStorage::Virtual,
            max_xid,
            require_oid_eq_paddr: false,
        }
    }

    #[allow(dead_code)]
    pub fn ephemeral_object(object_type: u32) -> Self {
        Self {
            object_type,
            storage: ExpectedStorage::Ephemeral,
            max_xid: None,
            require_oid_eq_paddr: false,
        }
    }

    pub fn any_storage(object_type: u32) -> Self {
        Self {
            object_type,
            storage: ExpectedStorage::Any,
            max_xid: None,
            require_oid_eq_paddr: false,
        }
    }
}

/// Parse and validate an `obj_phys_t` header against expectations.
///
/// This is intentionally strict because of `SR-007`:
/// * checksum must match unless the caller explicitly bypassed validation,
/// * the kind bits of `o_type` must equal the expected kind,
/// * the flag bits must agree with the expected storage class,
/// * encrypted and no-header objects are hard stops in the v1 parser,
/// * for physical objects we record (and optionally enforce) `o_oid == paddr`,
/// * if a scan-state XID is supplied, an object newer than that XID is a hard
///   stop because we would otherwise be reading a partially-flushed checkpoint.
pub(crate) fn validate_object_block(
    block: &[u8],
    block_address: u64,
    expectation: ObjectExpectation,
) -> Result<ObjectHeader, ScanError> {
    if !checksum_matches(block) {
        return Err(ScanError::InvalidObject(format!(
            "checksum mismatch at block {block_address}"
        )));
    }

    let header = ObjectHeader::from_block(block, block_address);

    if header.object_type != expectation.object_type {
        return Err(ScanError::InvalidObject(format!(
            "block {block_address} has object type {:#06x}, expected {:#06x}",
            header.object_type, expectation.object_type
        )));
    }

    match expectation.storage {
        ExpectedStorage::Virtual => {
            if header.is_physical() || header.is_ephemeral() {
                return Err(ScanError::InvalidObject(format!(
                    "block {block_address} has storage flags {:#010x}, expected virtual",
                    header.object_type_flags
                )));
            }
        }
        ExpectedStorage::Physical => {
            if !header.is_physical() {
                return Err(ScanError::InvalidObject(format!(
                    "block {block_address} has storage flags {:#010x}, expected physical",
                    header.object_type_flags
                )));
            }
        }
        ExpectedStorage::Ephemeral => {
            if !header.is_ephemeral() {
                return Err(ScanError::InvalidObject(format!(
                    "block {block_address} has storage flags {:#010x}, expected ephemeral",
                    header.object_type_flags
                )));
            }
        }
        ExpectedStorage::Any => {}
    }

    if header.is_encrypted() {
        return Err(ScanError::InvalidObject(format!(
            "block {block_address} has OBJ_ENCRYPTED set; encrypted objects are unsupported"
        )));
    }

    if header.is_noheader() {
        return Err(ScanError::InvalidObject(format!(
            "block {block_address} has OBJ_NOHEADER set; zero-header objects are unsupported"
        )));
    }

    if expectation.require_oid_eq_paddr && header.oid != block_address {
        return Err(ScanError::InvalidObject(format!(
            "physical object at block {block_address} has o_oid={} (expected o_oid==paddr)",
            header.oid
        )));
    }

    if let Some(max_xid) = expectation.max_xid {
        if header.xid > max_xid {
            return Err(ScanError::InvalidObject(format!(
                "object at block {block_address} has o_xid={} newer than scan state {max_xid}",
                header.xid
            )));
        }
    }

    Ok(header)
}

pub(crate) fn flags_summary(header: &ObjectHeader) -> Vec<&'static str> {
    let mut summary = Vec::new();
    if header.is_physical() {
        summary.push("physical");
    } else if header.is_ephemeral() {
        summary.push("ephemeral");
    } else if header.object_type_flags == OBJ_VIRTUAL {
        summary.push("virtual");
    }
    if header.is_encrypted() {
        summary.push("encrypted");
    }
    if header.is_noheader() {
        summary.push("noheader");
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_io::{put_u32, put_u64, resign_block};

    const BLOCK_SIZE: usize = 4096;

    fn make_header_block(
        oid: u64,
        xid: u64,
        object_type_raw: u32,
        object_subtype: u32,
        block_address: u64,
    ) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        put_u64(&mut block, 0x08, oid);
        put_u64(&mut block, 0x10, xid);
        put_u32(&mut block, 0x18, object_type_raw);
        put_u32(&mut block, 0x1c, object_subtype);
        resign_block(&mut block);
        let _ = block_address;
        block
    }

    #[test]
    fn validates_physical_object_with_oid_eq_paddr() {
        let block = make_header_block(7, 14, OBJ_PHYSICAL | OBJECT_TYPE_OMAP, 0, 7);
        let header =
            validate_object_block(&block, 7, ObjectExpectation::physical(OBJECT_TYPE_OMAP))
                .expect("physical OMAP header is accepted");
        assert_eq!(header.oid, 7);
        assert_eq!(header.object_type, OBJECT_TYPE_OMAP);
        assert!(header.is_physical());
    }

    #[test]
    fn rejects_physical_when_oid_differs_from_paddr() {
        let block = make_header_block(8, 14, OBJ_PHYSICAL | OBJECT_TYPE_OMAP, 0, 7);
        let err = validate_object_block(&block, 7, ObjectExpectation::physical(OBJECT_TYPE_OMAP))
            .expect_err("oid != paddr is rejected for physical objects");
        assert!(matches!(err, ScanError::InvalidObject(reason) if reason.contains("o_oid")));
    }

    #[test]
    fn rejects_encrypted_object_unconditionally() {
        let block = make_header_block(7, 14, OBJ_PHYSICAL | OBJ_ENCRYPTED | OBJECT_TYPE_OMAP, 0, 7);
        let err = validate_object_block(&block, 7, ObjectExpectation::physical(OBJECT_TYPE_OMAP))
            .expect_err("encrypted objects are rejected");
        assert!(
            matches!(err, ScanError::InvalidObject(reason) if reason.contains("OBJ_ENCRYPTED"))
        );
    }

    #[test]
    fn rejects_object_newer_than_scan_xid() {
        let block = make_header_block(7, 99, OBJ_PHYSICAL | OBJECT_TYPE_OMAP, 0, 7);
        let expectation = ObjectExpectation {
            object_type: OBJECT_TYPE_OMAP,
            storage: ExpectedStorage::Physical,
            max_xid: Some(50),
            require_oid_eq_paddr: true,
        };
        let err = validate_object_block(&block, 7, expectation)
            .expect_err("object newer than scan_xid is rejected");
        assert!(matches!(err, ScanError::InvalidObject(reason) if reason.contains("newer")));
    }

    #[test]
    fn rejects_storage_class_mismatch() {
        let block = make_header_block(7, 14, OBJ_PHYSICAL | OBJECT_TYPE_BTREE, 0, 7);
        let expectation = ObjectExpectation::virtual_object(OBJECT_TYPE_BTREE, None);
        let err = validate_object_block(&block, 7, expectation)
            .expect_err("physical object is rejected when virtual is expected");
        assert!(
            matches!(err, ScanError::InvalidObject(reason) if reason.contains("storage flags"))
        );
    }

    #[test]
    fn rejects_checksum_mismatch() {
        let mut block = make_header_block(7, 14, OBJ_PHYSICAL | OBJECT_TYPE_OMAP, 0, 7);
        block[200] ^= 0xff;
        let err = validate_object_block(&block, 7, ObjectExpectation::physical(OBJECT_TYPE_OMAP))
            .expect_err("checksum mismatch is rejected");
        assert!(matches!(err, ScanError::InvalidObject(reason) if reason.contains("checksum")));
    }

    #[test]
    fn accepts_virtual_object_when_virtual_expected() {
        let block = make_header_block(1028, 13, OBJECT_TYPE_BTREE, OBJECT_TYPE_FSTREE, 433);
        let expectation = ObjectExpectation::virtual_object(OBJECT_TYPE_BTREE, Some(14));
        let header = validate_object_block(&block, 433, expectation)
            .expect("virtual FS-tree root header is accepted");
        assert_eq!(header.object_subtype, OBJECT_TYPE_FSTREE);
        assert!(!header.is_physical());
        assert!(!header.is_ephemeral());
    }
}
