//! Read-only APFS B-tree node reader.
//!
//! `SR-006` says the minimum safe resolver key for any APFS object is
//! `(omap context, oid, max_xid)`, so the v1 native B-tree code only walks the
//! tree structure and surfaces raw key/value byte slices. Tree-specific
//! interpretation lives in `omap.rs` and `fs_records.rs`.
//!
//! The same node layout is used by every APFS B-tree, so internal-node
//! traversal does not depend on whether the tree is the OMAP, FS, or extent
//! reference tree. Internal nodes always carry an 8-byte child block address
//! as the value associated with each key.

use std::io::{Read, Seek};

use crate::block_io::{le_u16, read_block};
use crate::object::{
    validate_object_block, ExpectedStorage, ObjectExpectation, ObjectHeader, OBJECT_TYPE_BTREE,
    OBJECT_TYPE_BTREE_NODE, OBJ_HEADER_SIZE,
};
use crate::ScanError;

const BTNODE_ROOT: u16 = 0x0001;
const BTNODE_LEAF: u16 = 0x0002;
const BTNODE_FIXED_KV_SIZE: u16 = 0x0004;
const BTREE_INFO_SIZE: usize = 40;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct BtreeNode<'a> {
    /// Whole block backing this node; key/value byte slices borrow from this.
    pub block: &'a [u8],
    /// Block size for this APFS container, used when computing trailing
    /// `btree_info_t` for root nodes.
    pub block_size: usize,
    pub flags: u16,
    pub level: u16,
    pub nkeys: u32,
    pub data_offset: usize,
    pub toc_offset: usize,
    pub toc_len: u16,
    pub key_area_offset: usize,
    pub value_area_end: usize,
    pub fixed_kv_size: bool,
    pub is_root: bool,
    pub is_leaf: bool,
}

impl<'a> BtreeNode<'a> {
    pub fn parse(block: &'a [u8], block_size: usize) -> Result<Self, ScanError> {
        if block.len() < block_size {
            return Err(ScanError::InvalidObject(
                "btree node block shorter than block_size".to_string(),
            ));
        }
        let flags = le_u16(block, 0x20);
        let level = le_u16(block, 0x22);
        let nkeys = u32::from_le_bytes(block[0x24..0x28].try_into().expect("u32 nkeys field"));
        let toc_off_rel = le_u16(block, 0x28);
        let toc_len = le_u16(block, 0x2a);
        let data_offset = OBJ_HEADER_SIZE + 24;
        let toc_offset = data_offset
            .checked_add(toc_off_rel as usize)
            .ok_or_else(|| ScanError::InvalidObject("btree TOC offset overflow".to_string()))?;
        let key_area_offset = toc_offset.checked_add(toc_len as usize).ok_or_else(|| {
            ScanError::InvalidObject("btree key-area offset overflow".to_string())
        })?;
        let is_root = flags & BTNODE_ROOT != 0;
        let value_area_end = if is_root {
            block_size
                .checked_sub(BTREE_INFO_SIZE)
                .ok_or_else(|| ScanError::InvalidObject("root btree node too small".to_string()))?
        } else {
            block_size
        };
        if key_area_offset > value_area_end {
            return Err(ScanError::InvalidObject(
                "btree key area overlaps value area".to_string(),
            ));
        }
        Ok(Self {
            block,
            block_size,
            flags,
            level,
            nkeys,
            data_offset,
            toc_offset,
            toc_len,
            key_area_offset,
            value_area_end,
            fixed_kv_size: flags & BTNODE_FIXED_KV_SIZE != 0,
            is_root,
            is_leaf: flags & BTNODE_LEAF != 0,
        })
    }

    pub fn entry(&self, index: u32) -> Result<BtreeEntry, ScanError> {
        if index >= self.nkeys {
            return Err(ScanError::InvalidObject(format!(
                "btree entry index {} out of range (nkeys={})",
                index, self.nkeys
            )));
        }
        if self.fixed_kv_size {
            self.fixed_entry(index)
        } else {
            self.variable_entry(index)
        }
    }

    fn fixed_entry(&self, index: u32) -> Result<BtreeEntry, ScanError> {
        let entry_off = self
            .toc_offset
            .checked_add(4 * index as usize)
            .ok_or_else(|| ScanError::InvalidObject("btree TOC entry overflow".to_string()))?;
        if entry_off + 4 > self.toc_offset + self.toc_len as usize {
            return Err(ScanError::InvalidObject(
                "btree fixed TOC entry past TOC length".to_string(),
            ));
        }
        let k_off = le_u16(self.block, entry_off) as usize;
        let v_off = le_u16(self.block, entry_off + 2) as usize;
        let key_start = self.key_area_offset.checked_add(k_off).ok_or_else(|| {
            ScanError::InvalidObject("btree fixed key offset overflow".to_string())
        })?;
        // `v_off` from the TOC measures distance from the end of the value
        // area to the **start** of the value. Values then run forward for
        // `fixed_value_size` bytes (or the variable `v_len`).
        let value_start = self.value_area_end.checked_sub(v_off).ok_or_else(|| {
            ScanError::InvalidObject("btree fixed value offset underflow".to_string())
        })?;
        Ok(BtreeEntry {
            key_offset: key_start,
            key_len: None,
            value_start,
            value_len: None,
        })
    }

    fn variable_entry(&self, index: u32) -> Result<BtreeEntry, ScanError> {
        let entry_off = self
            .toc_offset
            .checked_add(8 * index as usize)
            .ok_or_else(|| ScanError::InvalidObject("btree TOC entry overflow".to_string()))?;
        if entry_off + 8 > self.toc_offset + self.toc_len as usize {
            return Err(ScanError::InvalidObject(
                "btree variable TOC entry past TOC length".to_string(),
            ));
        }
        let k_off = le_u16(self.block, entry_off) as usize;
        let k_len = le_u16(self.block, entry_off + 2) as usize;
        let v_off = le_u16(self.block, entry_off + 4) as usize;
        let v_len = le_u16(self.block, entry_off + 6) as usize;
        let key_start = self.key_area_offset.checked_add(k_off).ok_or_else(|| {
            ScanError::InvalidObject("btree variable key offset overflow".to_string())
        })?;
        let value_start = self.value_area_end.checked_sub(v_off).ok_or_else(|| {
            ScanError::InvalidObject("btree variable value offset underflow".to_string())
        })?;
        Ok(BtreeEntry {
            key_offset: key_start,
            key_len: Some(k_len),
            value_start,
            value_len: Some(v_len),
        })
    }

    pub fn key_bytes(&self, entry: &BtreeEntry, fixed_key_size: usize) -> &'a [u8] {
        let len = entry.key_len.unwrap_or(fixed_key_size);
        &self.block[entry.key_offset..entry.key_offset + len]
    }

    pub fn value_bytes(&self, entry: &BtreeEntry, fixed_value_size: usize) -> &'a [u8] {
        let len = entry.value_len.unwrap_or(fixed_value_size);
        let end = entry.value_start.checked_add(len).expect("btree value end");
        &self.block[entry.value_start..end]
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BtreeEntry {
    pub key_offset: usize,
    pub key_len: Option<usize>,
    pub value_start: usize,
    pub value_len: Option<usize>,
}

/// Read a B-tree node block at `paddr` and verify its object header.
///
/// The first node read is the tree root and must have type `OBJECT_TYPE_BTREE`;
/// every other node has type `OBJECT_TYPE_BTREE_NODE`. Both share the same
/// physical layout; only the type byte differs. Storage class is supplied by
/// the caller because OMAP trees use physical roots while FS-trees are
/// referenced via a volume OMAP and are therefore virtual.
pub(crate) fn read_btree_node<R: Read + Seek>(
    reader: &mut R,
    block_size: usize,
    paddr: u64,
    is_root: bool,
    storage: ExpectedStorage,
    max_xid: Option<u64>,
) -> Result<(Vec<u8>, ObjectHeader), ScanError> {
    let block = read_block(reader, paddr, block_size)?;
    let expected_type = if is_root {
        OBJECT_TYPE_BTREE
    } else {
        OBJECT_TYPE_BTREE_NODE
    };
    let header = validate_object_block(
        &block,
        paddr,
        ObjectExpectation {
            object_type: expected_type,
            storage,
            max_xid,
            require_oid_eq_paddr: matches!(storage, ExpectedStorage::Physical),
        },
    )?;
    Ok((block, header))
}
