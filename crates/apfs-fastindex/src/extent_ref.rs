//! Read-only extent-reference-tree walker.
//!
//! The extent-reference tree lives at `volume_superblock.extentref_tree_oid`
//! and stores one `j_phys_ext_*` record per physically-allocated extent on
//! the volume. The record's `refcnt` field is the count of `file_extent`
//! records that reference the physical extent — the on-disk authority for
//! clone-deduplicated allocation.
//!
//! Storage class varies: on hdiutil-created `.dmg` fixtures the tree is
//! `OBJECT_TYPE_PHYSICAL` (the "OID" is a paddr; no OMAP lookup needed for
//! internal nodes). Live boot volumes may use `OBJECT_TYPE_VIRTUAL`
//! (internal-node values are virtual OIDs requiring resolution through the
//! volume OMAP at the selected XID). The walker handles both.
//!
//! Validated against the EX-27 fixture: walking this tree and summing
//! `length_blocks × block_size` over every leaf record reproduces the
//! on-disk deduplicated allocated total exactly, modulo container overhead.

use std::collections::HashSet;
use std::io::{Read, Seek};

use serde::Serialize;

use crate::block_io::le_u64;
use crate::btree::{read_btree_node, BtreeNode};
use crate::object::ExpectedStorage;
use crate::omap::OmapResolver;
use crate::ScanError;

const FS_OBJECT_ID_MASK: u64 = (1u64 << 60) - 1;
const FS_RECORD_TYPE_SHIFT: u32 = 60;

const RAW_TYPE_EXTENT_REFERENCE: u8 = 0x2;

/// `j_phys_ext_key_t` is 8 bytes: a single `obj_id_and_type` word where
/// the high 4 bits are the type (must be `0x2 == APFS_TYPE_EXTENT`) and
/// the low 60 bits are the **paddr of the first block** of the physical
/// extent.
const PHYS_EXT_KEY_SIZE: usize = 8;

/// `j_phys_ext_val_t` is 20 bytes: `len_and_kind` (8) + `owning_obj_id`
/// (8) + `refcnt` (4). High 4 bits of `len_and_kind` are the kind; low
/// 60 bits are the length in *blocks* (note: file_extent length is in
/// bytes, phys_ext length is in blocks).
const PHYS_EXT_VAL_SIZE: usize = 20;

/// Internal-node values are 8-byte child OIDs (paddrs for physical
/// trees, virtual OIDs for virtual trees).
const INTERNAL_VAL_SIZE: usize = 8;

/// Storage class of the extent-reference tree, decoded from
/// `extentref_tree_type_raw` (the high byte of the type field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentRefStorage {
    /// `OBJ_PHYSICAL` (0x40): the "OID" is a paddr; internal-node child
    /// values are paddrs directly.
    Physical,
    /// `OBJ_VIRTUAL` (0x00): the OID resolves through the volume OMAP;
    /// internal-node child values are child virtual OIDs that also
    /// require OMAP resolution.
    Virtual,
}

impl ExtentRefStorage {
    /// Decode from the high byte of `extentref_tree_type_raw`.
    pub fn from_type_raw(type_raw: u32) -> Result<Self, ScanError> {
        let storage_class = (type_raw >> 24) & 0xFF;
        match storage_class {
            0x00 => Ok(Self::Virtual),
            0x40 => Ok(Self::Physical),
            other => Err(ScanError::InvalidObject(format!(
                "extent-reference tree storage class {other:#x} is not supported \
                 (expected 0x00 virtual or 0x40 physical)"
            ))),
        }
    }

    fn expected_storage(self) -> ExpectedStorage {
        match self {
            Self::Physical => ExpectedStorage::Physical,
            Self::Virtual => ExpectedStorage::Virtual,
        }
    }
}

/// One leaf record from the extent-reference tree: a physically-
/// allocated extent and the count of `file_extent` records that point
/// at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysExtRecord {
    pub paddr_first: u64,
    pub length_blocks: u64,
    pub kind: u8,
    pub owning_obj_id: u64,
    pub refcnt: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtentRefDump {
    pub root_paddr: u64,
    pub storage_class: &'static str,
    pub leaf_node_count: u32,
    pub index_node_count: u32,
    pub leaf_record_count: u32,
    pub unsupported_record_count: u32,
    pub max_xid: u64,
    pub records: Vec<PhysExtRecord>,
    pub validation_notes: Vec<String>,
}

/// Walk the extent-reference tree at `root_paddr`.
///
/// For physical trees, `volume_omap` is unused (internal-node values are
/// paddrs directly). For virtual trees, `volume_omap` is required to
/// resolve child OIDs at `max_xid`.
pub(crate) fn dump_extent_refs<R: Read + Seek>(
    reader: &mut R,
    block_size: usize,
    root_paddr: u64,
    storage: ExtentRefStorage,
    max_xid: u64,
    volume_omap: &OmapResolver,
) -> Result<ExtentRefDump, ScanError> {
    let mut state = DumpState {
        block_size,
        storage,
        max_xid,
        leaf_node_count: 0,
        index_node_count: 0,
        leaf_record_count: 0,
        unsupported_record_count: 0,
        records: Vec::new(),
        validation_notes: Vec::new(),
    };
    let mut visited: HashSet<u64> = HashSet::new();
    walk_node(
        &mut state,
        reader,
        root_paddr,
        true,
        volume_omap,
        0,
        &mut visited,
    )?;
    Ok(ExtentRefDump {
        root_paddr,
        storage_class: match storage {
            ExtentRefStorage::Physical => "physical",
            ExtentRefStorage::Virtual => "virtual",
        },
        leaf_node_count: state.leaf_node_count,
        index_node_count: state.index_node_count,
        leaf_record_count: state.leaf_record_count,
        unsupported_record_count: state.unsupported_record_count,
        max_xid,
        records: state.records,
        validation_notes: state.validation_notes,
    })
}

struct DumpState {
    block_size: usize,
    storage: ExtentRefStorage,
    max_xid: u64,
    leaf_node_count: u32,
    index_node_count: u32,
    leaf_record_count: u32,
    unsupported_record_count: u32,
    records: Vec<PhysExtRecord>,
    validation_notes: Vec<String>,
}

fn walk_node<R: Read + Seek>(
    state: &mut DumpState,
    reader: &mut R,
    paddr: u64,
    is_root: bool,
    volume_omap: &OmapResolver,
    depth: usize,
    visited: &mut HashSet<u64>,
) -> Result<(), ScanError> {
    if depth >= crate::MAX_TREE_DEPTH {
        return Err(ScanError::InvalidObject(format!(
            "extent-reference tree depth exceeded {} at paddr {paddr}",
            crate::MAX_TREE_DEPTH
        )));
    }
    if !visited.insert(paddr) {
        return Err(ScanError::InvalidObject(format!(
            "extent-reference tree cycle detected at paddr {paddr}"
        )));
    }
    let (block, header) = read_btree_node(
        reader,
        state.block_size,
        paddr,
        is_root,
        state.storage.expected_storage(),
        Some(state.max_xid),
    )?;
    if header.xid > state.max_xid {
        state.validation_notes.push(format!(
            "extent-reference tree node at {paddr} has xid {} newer than scan state {}",
            header.xid, state.max_xid
        ));
    }
    let node = BtreeNode::parse(&block, state.block_size)?;
    if node.fixed_kv_size {
        return Err(ScanError::InvalidObject(
            "extent-reference tree node should not be fixed-kv-size".to_string(),
        ));
    }
    if node.is_leaf {
        state.leaf_node_count += 1;
        for index in 0..node.nkeys {
            let entry = node.entry(index)?;
            let key = node.key_bytes(&entry, 0)?;
            let value = node.value_bytes(&entry, 0)?;
            if key.len() < PHYS_EXT_KEY_SIZE {
                return Err(ScanError::InvalidObject(format!(
                    "extent-reference leaf key at node {paddr} entry {index} \
                     shorter than j_phys_ext_key_t ({PHYS_EXT_KEY_SIZE} bytes)"
                )));
            }
            let key_word = le_u64(key, 0);
            let paddr_first = key_word & FS_OBJECT_ID_MASK;
            let raw_type = (key_word >> FS_RECORD_TYPE_SHIFT) as u8;
            if raw_type != RAW_TYPE_EXTENT_REFERENCE {
                state.unsupported_record_count += 1;
                state.validation_notes.push(format!(
                    "extent-reference leaf at node {paddr} entry {index} has raw_type {raw_type:#x} \
                     (expected {RAW_TYPE_EXTENT_REFERENCE:#x})"
                ));
                continue;
            }
            if value.len() < PHYS_EXT_VAL_SIZE {
                return Err(ScanError::InvalidObject(format!(
                    "extent-reference leaf value at node {paddr} entry {index} \
                     shorter than j_phys_ext_val_t ({PHYS_EXT_VAL_SIZE} bytes), got {}",
                    value.len()
                )));
            }
            let len_and_kind = le_u64(value, 0);
            let length_blocks = len_and_kind & FS_OBJECT_ID_MASK;
            let kind = ((len_and_kind >> FS_RECORD_TYPE_SHIFT) as u8) & 0xF;
            let owning_obj_id = le_u64(value, 8);
            let refcnt =
                i32::from_le_bytes(value[16..20].try_into().expect("i32 refcnt"));
            state.leaf_record_count += 1;
            state.records.push(PhysExtRecord {
                paddr_first,
                length_blocks,
                kind,
                owning_obj_id,
                refcnt,
            });
        }
        return Ok(());
    }

    state.index_node_count += 1;
    let mut children = Vec::with_capacity(node.nkeys as usize);
    for index in 0..node.nkeys {
        let entry = node.entry(index)?;
        let value = node.value_bytes(&entry, INTERNAL_VAL_SIZE)?;
        if value.len() < INTERNAL_VAL_SIZE {
            return Err(ScanError::InvalidObject(format!(
                "extent-reference internal value at {paddr} entry {index} shorter than child oid"
            )));
        }
        let child_oid = le_u64(value, 0);
        children.push((index, child_oid));
    }
    for (index, child_oid) in children {
        let child_paddr = match state.storage {
            ExtentRefStorage::Physical => child_oid,
            ExtentRefStorage::Virtual => {
                let mapping = volume_omap
                    .lookup(reader, state.block_size, child_oid, state.max_xid)?
                    .ok_or_else(|| {
                        ScanError::InvalidObject(format!(
                            "extent-reference internal entry at {paddr}/{index} child oid \
                             {child_oid} not resolvable in volume OMAP at max_xid {}",
                            state.max_xid
                        ))
                    })?;
                mapping.paddr
            }
        };
        walk_node(
            state,
            reader,
            child_paddr,
            false,
            volume_omap,
            depth + 1,
            visited,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_class_decodes_physical_and_virtual() {
        assert_eq!(
            ExtentRefStorage::from_type_raw(0x4000_0002).unwrap(),
            ExtentRefStorage::Physical
        );
        assert_eq!(
            ExtentRefStorage::from_type_raw(0x0000_0002).unwrap(),
            ExtentRefStorage::Virtual
        );
    }

    #[test]
    fn storage_class_rejects_ephemeral_or_unknown() {
        // 0x80 is OBJ_EPHEMERAL — not a tree storage class.
        assert!(ExtentRefStorage::from_type_raw(0x8000_0002).is_err());
        assert!(ExtentRefStorage::from_type_raw(0x1234_0002).is_err());
    }
}
