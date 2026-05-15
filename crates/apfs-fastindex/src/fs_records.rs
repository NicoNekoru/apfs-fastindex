//! Read-only FS-tree record-family dumper.
//!
//! `SR-008` says native FS parsing should begin as a record dumper: walk the
//! FS-tree root, decode the key family from each leaf entry, count records by
//! family, and surface unsupported counts before any product namespace claim
//! is made. This module does exactly that and nothing more.
//!
//! The dumper does not interpret record bodies, does not normalize names, and
//! does not emit `NamespaceEntry` rows. Those steps require oracle parity
//! probes that have not been run against Rust output yet.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use serde::Serialize;

use crate::btree::{read_btree_node, BtreeNode};
use crate::fs_record_body::{decode_fs_record, FsRecordRow};
use crate::object::ExpectedStorage;
use crate::omap::OmapResolver;
use crate::ScanError;

/// FS-tree blocks are stored as virtual objects: the root is reached through
/// the volume OMAP, and **each internal-node value is itself a child virtual
/// OID** that must be resolved through the same OMAP at the selected XID
/// before the child block can be read. Internal values are NOT direct paddrs.
/// See EX-15 for the proof on the EX-14 fixture (block 1031 was a child OID,
/// not a paddr).
const FS_TREE_STORAGE: ExpectedStorage = ExpectedStorage::Virtual;

const FS_OBJECT_ID_MASK: u64 = (1u64 << 60) - 1;
const FS_RECORD_TYPE_SHIFT: u32 = 60;
const FS_INTERNAL_VAL_SIZE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsRecordDump {
    pub volume_index: u32,
    pub volume_oid: u64,
    pub root_paddr: u64,
    pub leaf_node_count: u32,
    pub index_node_count: u32,
    pub leaf_record_count: u32,
    pub family_counts: Vec<FamilyCount>,
    pub unsupported_record_count: u32,
    pub unique_object_ids: u32,
    pub max_xid: u64,
    pub validation_notes: Vec<String>,
    /// Decoded body rows (SR-008 / SR-014 / SR-015 / SR-016). Each leaf entry
    /// in scope of the v1 body decoder shows up here in walk order.
    pub records: Vec<FsRecordRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FamilyCount {
    pub raw_type: u8,
    pub name: &'static str,
    pub count: u32,
    pub in_v1_namespace_scope: bool,
}

/// Walk the FS-tree at `root_paddr` and count records by family.
///
/// `volume_omap` is the volume's OMAP resolver. FS-tree internal entries
/// carry **virtual** child OIDs (not paddrs); the walker resolves each one
/// through this OMAP at `max_xid` before reading the child node.
pub(crate) fn dump_fs_records<R: Read + Seek>(
    reader: &mut R,
    block_size: usize,
    volume_index: u32,
    volume_oid: u64,
    root_paddr: u64,
    max_xid: u64,
    volume_omap: &OmapResolver,
) -> Result<FsRecordDump, ScanError> {
    let mut state = DumpState {
        block_size,
        max_xid,
        leaf_node_count: 0,
        index_node_count: 0,
        leaf_record_count: 0,
        family_counts: BTreeMap::new(),
        unsupported_record_count: 0,
        unique_object_ids: BTreeMap::new(),
        validation_notes: Vec::new(),
        records: Vec::new(),
    };
    walk_fs_node(&mut state, reader, root_paddr, true, volume_omap)?;
    let mut family_counts: Vec<FamilyCount> = state
        .family_counts
        .iter()
        .map(|(raw_type, count)| {
            let name = record_family_name(*raw_type);
            FamilyCount {
                raw_type: *raw_type,
                name,
                count: *count,
                in_v1_namespace_scope: family_in_v1_namespace_scope(*raw_type),
            }
        })
        .collect();
    family_counts.sort_by_key(|family| family.raw_type);
    Ok(FsRecordDump {
        volume_index,
        volume_oid,
        root_paddr,
        leaf_node_count: state.leaf_node_count,
        index_node_count: state.index_node_count,
        leaf_record_count: state.leaf_record_count,
        family_counts,
        unsupported_record_count: state.unsupported_record_count,
        unique_object_ids: state.unique_object_ids.len() as u32,
        max_xid,
        validation_notes: state.validation_notes,
        records: state.records,
    })
}

struct DumpState {
    block_size: usize,
    max_xid: u64,
    leaf_node_count: u32,
    index_node_count: u32,
    leaf_record_count: u32,
    family_counts: BTreeMap<u8, u32>,
    unsupported_record_count: u32,
    unique_object_ids: BTreeMap<u64, ()>,
    validation_notes: Vec<String>,
    records: Vec<FsRecordRow>,
}

fn walk_fs_node<R: Read + Seek>(
    state: &mut DumpState,
    reader: &mut R,
    paddr: u64,
    is_root: bool,
    volume_omap: &OmapResolver,
) -> Result<(), ScanError> {
    let (block, header) = read_btree_node(
        reader,
        state.block_size,
        paddr,
        is_root,
        FS_TREE_STORAGE,
        Some(state.max_xid),
    )?;
    if header.xid > state.max_xid {
        state.validation_notes.push(format!(
            "FS-tree node at {paddr} has xid {} newer than scan state {}",
            header.xid, state.max_xid
        ));
    }
    let node = BtreeNode::parse(&block, state.block_size)?;
    if node.fixed_kv_size {
        return Err(ScanError::InvalidObject(
            "FS-tree node should not be fixed-kv-size".to_string(),
        ));
    }
    if node.is_leaf {
        state.leaf_node_count += 1;
        for index in 0..node.nkeys {
            let entry = node.entry(index)?;
            let key = node.key_bytes(&entry, 0);
            let value = node.value_bytes(&entry, 0);
            if key.len() < 8 {
                return Err(ScanError::InvalidObject(format!(
                    "FS-tree leaf key at node {paddr} entry {index} shorter than j_key_t"
                )));
            }
            let key_word = u64::from_le_bytes(key[0..8].try_into().unwrap());
            let object_id = key_word & FS_OBJECT_ID_MASK;
            let raw_type = (key_word >> FS_RECORD_TYPE_SHIFT) as u8;
            *state.family_counts.entry(raw_type).or_insert(0) += 1;
            state.leaf_record_count += 1;
            state.unique_object_ids.insert(object_id, ());
            if !is_known_record_family(raw_type) {
                state.unsupported_record_count += 1;
                state.validation_notes.push(format!(
                    "FS-tree node {paddr} entry {index} carries unknown record type {raw_type:#x}"
                ));
                continue;
            }
            let row = decode_fs_record(paddr, index, key, value)?;
            state.records.push(row);
        }
        return Ok(());
    }

    state.index_node_count += 1;
    let mut children = Vec::with_capacity(node.nkeys as usize);
    for index in 0..node.nkeys {
        let entry = node.entry(index)?;
        let value = node.value_bytes(&entry, FS_INTERNAL_VAL_SIZE);
        if value.len() < FS_INTERNAL_VAL_SIZE {
            return Err(ScanError::InvalidObject(format!(
                "FS-tree internal value at {paddr} entry {index} shorter than child oid"
            )));
        }
        // FS-trees are virtual: the 8-byte internal value is a child virtual
        // OID, not a paddr. Resolve through the volume OMAP at max_xid before
        // reading the child node.
        let child_oid =
            u64::from_le_bytes(value[..FS_INTERNAL_VAL_SIZE].try_into().expect("oid u64"));
        children.push((index, child_oid));
    }
    for (index, child_oid) in children {
        let mapping = volume_omap
            .lookup(reader, state.block_size, child_oid, state.max_xid)?
            .ok_or_else(|| {
                ScanError::InvalidObject(format!(
                    "FS-tree internal entry at {paddr}/{index} child oid {child_oid} \
                     not resolvable in volume OMAP at max_xid {}",
                    state.max_xid
                ))
            })?;
        walk_fs_node(state, reader, mapping.paddr, false, volume_omap)?;
    }
    Ok(())
}

fn is_known_record_family(raw_type: u8) -> bool {
    (0x1..=0xd).contains(&raw_type)
}

fn record_family_name(raw_type: u8) -> &'static str {
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

/// Whether the family is part of the v1 namespace + logical-size record set
/// per `SR-008`/`RL-03`: directory records, inodes, dstream IDs, xattrs (for
/// symlinks), and sibling records for hard links.
fn family_in_v1_namespace_scope(raw_type: u8) -> bool {
    matches!(raw_type, 0x3 | 0x4 | 0x5 | 0x6 | 0x9 | 0xc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_io::{put_u32, put_u64, resign_block};
    use crate::object::{
        OBJECT_TYPE_BTREE, OBJECT_TYPE_BTREE_NODE, OBJECT_TYPE_FSTREE, OBJECT_TYPE_OMAP,
        OBJ_HEADER_SIZE, OBJ_PHYSICAL,
    };
    use std::io::Cursor;

    const BLOCK_SIZE: usize = 4096;
    const BTNODE_ROOT: u16 = 0x0001;
    const BTNODE_LEAF: u16 = 0x0002;
    const BTNODE_FIXED_KV_SIZE: u16 = 0x0004;
    const BTREE_INFO_SIZE: usize = 40;
    const OMAP_MANUALLY_MANAGED: u32 = 0x0000_0001;

    /// Build a single-leaf fixed-kv OMAP root holding `mappings`.
    fn make_omap_leaf(paddr: u64, mappings: &[(u64, u64, u32, u32, u64)]) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        let nkeys = mappings.len() as u32;
        put_u64(&mut block, 0x08, paddr);
        put_u64(&mut block, 0x10, 14);
        put_u32(&mut block, 0x18, OBJ_PHYSICAL | OBJECT_TYPE_BTREE);
        put_u32(&mut block, 0x1c, OBJECT_TYPE_OMAP);
        let bt_flags: u16 = BTNODE_ROOT | BTNODE_LEAF | BTNODE_FIXED_KV_SIZE;
        block[0x20..0x22].copy_from_slice(&bt_flags.to_le_bytes());
        block[0x22..0x24].copy_from_slice(&0u16.to_le_bytes());
        block[0x24..0x28].copy_from_slice(&nkeys.to_le_bytes());
        let toc_len: u16 = (nkeys as u16) * 4;
        block[0x28..0x2a].copy_from_slice(&0u16.to_le_bytes());
        block[0x2a..0x2c].copy_from_slice(&toc_len.to_le_bytes());
        let data_offset = OBJ_HEADER_SIZE + 24;
        let toc_offset = data_offset;
        let key_area_offset = toc_offset + toc_len as usize;
        let value_area_end = BLOCK_SIZE - BTREE_INFO_SIZE;
        let omap_key_size = 16usize;
        let omap_val_size = 16usize;
        for (index, (oid, xid, flags, size, val_paddr)) in mappings.iter().enumerate() {
            let entry_off = toc_offset + 4 * index;
            let k_off = (index * omap_key_size) as u16;
            let v_off = ((index + 1) * omap_val_size) as u16;
            block[entry_off..entry_off + 2].copy_from_slice(&k_off.to_le_bytes());
            block[entry_off + 2..entry_off + 4].copy_from_slice(&v_off.to_le_bytes());
            let key_start = key_area_offset + k_off as usize;
            block[key_start..key_start + 8].copy_from_slice(&oid.to_le_bytes());
            block[key_start + 8..key_start + 16].copy_from_slice(&xid.to_le_bytes());
            let value_start = value_area_end - v_off as usize;
            block[value_start..value_start + 4].copy_from_slice(&flags.to_le_bytes());
            block[value_start + 4..value_start + 8].copy_from_slice(&size.to_le_bytes());
            block[value_start + 8..value_start + 16].copy_from_slice(&val_paddr.to_le_bytes());
        }
        let info_off = BLOCK_SIZE - BTREE_INFO_SIZE;
        block[info_off + 0x10..info_off + 0x14]
            .copy_from_slice(&(omap_key_size as u32).to_le_bytes());
        block[info_off + 0x14..info_off + 0x18]
            .copy_from_slice(&(omap_val_size as u32).to_le_bytes());
        resign_block(&mut block);
        block
    }

    fn make_omap_phys(omap_paddr: u64, tree_root_paddr: u64) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        put_u64(&mut block, 0x08, omap_paddr);
        put_u64(&mut block, 0x10, 14);
        put_u32(&mut block, 0x18, OBJ_PHYSICAL | OBJECT_TYPE_OMAP);
        put_u32(&mut block, 0x1c, 0);
        put_u32(&mut block, 0x20, OMAP_MANUALLY_MANAGED);
        put_u32(&mut block, 0x28, OBJ_PHYSICAL | OBJECT_TYPE_BTREE);
        put_u32(&mut block, 0x2c, OBJ_PHYSICAL | OBJECT_TYPE_BTREE);
        put_u64(&mut block, 0x30, tree_root_paddr);
        resign_block(&mut block);
        block
    }

    /// Build a single-key variable-kv FS-tree internal root whose only entry
    /// points to a child virtual OID.
    fn make_fs_internal_single_child_oid(
        _paddr: u64,
        xid: u64,
        oid: u64,
        child_oid: u64,
    ) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        // Virtual storage (flags=0).
        put_u64(&mut block, 0x08, oid);
        put_u64(&mut block, 0x10, xid);
        put_u32(&mut block, 0x18, OBJECT_TYPE_BTREE);
        put_u32(&mut block, 0x1c, OBJECT_TYPE_FSTREE);
        let bt_flags: u16 = BTNODE_ROOT; // not leaf, not fixed
        block[0x20..0x22].copy_from_slice(&bt_flags.to_le_bytes());
        block[0x22..0x24].copy_from_slice(&1u16.to_le_bytes()); // level=1
        block[0x24..0x28].copy_from_slice(&1u32.to_le_bytes()); // nkeys=1
        let toc_len: u16 = 8; // one variable entry = 8 bytes
        block[0x28..0x2a].copy_from_slice(&0u16.to_le_bytes());
        block[0x2a..0x2c].copy_from_slice(&toc_len.to_le_bytes());
        let data_offset = OBJ_HEADER_SIZE + 24;
        let toc_offset = data_offset;
        let key_area_offset = toc_offset + toc_len as usize;
        let value_area_end = BLOCK_SIZE - BTREE_INFO_SIZE;
        // One TOC entry pointing at an 8-byte key and an 8-byte value.
        let k_off: u16 = 0;
        let k_len: u16 = 8;
        let v_off: u16 = 8;
        let v_len: u16 = 8;
        block[toc_offset..toc_offset + 2].copy_from_slice(&k_off.to_le_bytes());
        block[toc_offset + 2..toc_offset + 4].copy_from_slice(&k_len.to_le_bytes());
        block[toc_offset + 4..toc_offset + 6].copy_from_slice(&v_off.to_le_bytes());
        block[toc_offset + 6..toc_offset + 8].copy_from_slice(&v_len.to_le_bytes());
        // Key: j_key_t with object_id=0, type=0 (we never decode internal keys).
        let key_start = key_area_offset + k_off as usize;
        block[key_start..key_start + 8].copy_from_slice(&0u64.to_le_bytes());
        // Value: 8-byte child OID.
        let value_start = value_area_end - v_off as usize;
        block[value_start..value_start + 8].copy_from_slice(&child_oid.to_le_bytes());
        // btree_info_t trailer.
        let info_off = BLOCK_SIZE - BTREE_INFO_SIZE;
        block[info_off + 0x10..info_off + 0x14].copy_from_slice(&0u32.to_le_bytes());
        block[info_off + 0x14..info_off + 0x18].copy_from_slice(&0u32.to_le_bytes());
        resign_block(&mut block);
        block
    }

    /// Build an FS-tree leaf with one j_dstream_id record (raw_type=0x6).
    /// Body decoding requires only a 4-byte refcnt; an 8-byte j_key_t header
    /// is sufficient for the key. Used to exercise the FS-tree internal-OID
    /// resolution path without exercising the larger DIR_REC / INODE body
    /// gates that have their own coverage.
    fn make_fs_leaf_single_dstream_id(_paddr: u64, xid: u64, oid: u64, object_id: u64) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        put_u64(&mut block, 0x08, oid);
        put_u64(&mut block, 0x10, xid);
        put_u32(&mut block, 0x18, OBJECT_TYPE_BTREE_NODE);
        put_u32(&mut block, 0x1c, OBJECT_TYPE_FSTREE);
        let bt_flags: u16 = BTNODE_LEAF;
        block[0x20..0x22].copy_from_slice(&bt_flags.to_le_bytes());
        block[0x22..0x24].copy_from_slice(&0u16.to_le_bytes());
        block[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        let toc_len: u16 = 8;
        block[0x28..0x2a].copy_from_slice(&0u16.to_le_bytes());
        block[0x2a..0x2c].copy_from_slice(&toc_len.to_le_bytes());
        let data_offset = OBJ_HEADER_SIZE + 24;
        let toc_offset = data_offset;
        let key_area_offset = toc_offset + toc_len as usize;
        let k_off: u16 = 0;
        let k_len: u16 = 8;
        // Value is 4 bytes (j_dstream_id_val_t.refcnt). v_off counts back from
        // value_area_end == BLOCK_SIZE (non-root) to value start.
        let v_off: u16 = 4;
        let v_len: u16 = 4;
        block[toc_offset..toc_offset + 2].copy_from_slice(&k_off.to_le_bytes());
        block[toc_offset + 2..toc_offset + 4].copy_from_slice(&k_len.to_le_bytes());
        block[toc_offset + 4..toc_offset + 6].copy_from_slice(&v_off.to_le_bytes());
        block[toc_offset + 6..toc_offset + 8].copy_from_slice(&v_len.to_le_bytes());
        // Key: j_key_t with object_id in low 60 bits and record type 0x6
        // (DSTREAM_ID) in high 4 bits.
        let key_word = (object_id & FS_OBJECT_ID_MASK) | (0x6u64 << FS_RECORD_TYPE_SHIFT);
        let key_start = key_area_offset + k_off as usize;
        block[key_start..key_start + 8].copy_from_slice(&key_word.to_le_bytes());
        let value_start = BLOCK_SIZE - v_off as usize;
        block[value_start..value_start + 4].copy_from_slice(&1u32.to_le_bytes());
        resign_block(&mut block);
        block
    }

    #[test]
    fn fs_tree_internal_value_is_virtual_oid_resolved_via_omap() {
        // Layout:
        //   - volume OMAP-phys at paddr 2, tree root at 3
        //   - OMAP leaf at 3 maps:
        //       oid 1028 (fs root tree) -> paddr 4 at xid 5
        //       oid 1030 (child leaf)   -> paddr 7 at xid 5
        //   - FS-tree root at paddr 4: internal level-1 root, one child entry
        //     whose 8-byte value is the virtual OID 1030. The block "1030"
        //     itself is left as zeros so the broken code path (treating the
        //     internal value as a paddr) would short-read or checksum-mismatch.
        //   - FS-tree leaf at paddr 7: one DIR_REC entry.
        let mut image = vec![0u8; 8 * BLOCK_SIZE];
        let omap_phys = make_omap_phys(2, 3);
        image[2 * BLOCK_SIZE..3 * BLOCK_SIZE].copy_from_slice(&omap_phys);
        let omap_leaf = make_omap_leaf(
            3,
            &[
                (1028u64, 5u64, 0u32, BLOCK_SIZE as u32, 4u64),
                (1030u64, 5u64, 0u32, BLOCK_SIZE as u32, 7u64),
            ],
        );
        image[3 * BLOCK_SIZE..4 * BLOCK_SIZE].copy_from_slice(&omap_leaf);
        let fs_root = make_fs_internal_single_child_oid(4, 5, 1028, 1030);
        image[4 * BLOCK_SIZE..5 * BLOCK_SIZE].copy_from_slice(&fs_root);
        let fs_leaf = make_fs_leaf_single_dstream_id(7, 5, 1030, 42);
        image[7 * BLOCK_SIZE..8 * BLOCK_SIZE].copy_from_slice(&fs_leaf);

        let mut cursor = Cursor::new(image);
        let omap = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let dump = dump_fs_records(&mut cursor, BLOCK_SIZE, 0, 1026, 4, 5, &omap)
            .expect("fs records dump succeeds");
        assert_eq!(dump.leaf_node_count, 1);
        assert_eq!(dump.index_node_count, 1);
        assert_eq!(dump.leaf_record_count, 1);
        assert_eq!(dump.unique_object_ids, 1);
        assert_eq!(dump.family_counts.len(), 1);
        let fc = &dump.family_counts[0];
        assert_eq!(fc.raw_type, 0x6);
        assert_eq!(fc.name, "dstream_id");
        assert_eq!(fc.count, 1);
        assert_eq!(dump.records.len(), 1);
    }

    #[test]
    fn fs_tree_internal_oid_missing_from_omap_is_hard_stop() {
        // Same layout as above but the OMAP only contains the root mapping; the
        // child OID 1030 has no mapping, so the walker must hard-stop instead
        // of falling back to treating the value as a paddr.
        let mut image = vec![0u8; 8 * BLOCK_SIZE];
        let omap_phys = make_omap_phys(2, 3);
        image[2 * BLOCK_SIZE..3 * BLOCK_SIZE].copy_from_slice(&omap_phys);
        let omap_leaf = make_omap_leaf(3, &[(1028u64, 5u64, 0u32, BLOCK_SIZE as u32, 4u64)]);
        image[3 * BLOCK_SIZE..4 * BLOCK_SIZE].copy_from_slice(&omap_leaf);
        let fs_root = make_fs_internal_single_child_oid(4, 5, 1028, 1030);
        image[4 * BLOCK_SIZE..5 * BLOCK_SIZE].copy_from_slice(&fs_root);

        let mut cursor = Cursor::new(image);
        let omap = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let err = dump_fs_records(&mut cursor, BLOCK_SIZE, 0, 1026, 4, 5, &omap)
            .expect_err("missing child OID is a hard stop");
        match err {
            ScanError::InvalidObject(reason) => {
                assert!(
                    reason.contains("not resolvable in volume OMAP"),
                    "unexpected error: {reason}"
                );
                assert!(
                    reason.contains("1030"),
                    "expected oid 1030 in error: {reason}"
                );
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn family_names_match_apple_reference() {
        // SR-008 lists the exact j_obj_kinds_t values; this guards against
        // accidental shuffling of the table when families are added.
        assert_eq!(record_family_name(0x3), "inode");
        assert_eq!(record_family_name(0x4), "xattr");
        assert_eq!(record_family_name(0x5), "sibling_link");
        assert_eq!(record_family_name(0x6), "dstream_id");
        assert_eq!(record_family_name(0x8), "file_extent");
        assert_eq!(record_family_name(0x9), "dir_rec");
        assert_eq!(record_family_name(0xc), "sibling_map");
        assert_eq!(record_family_name(0x42), "unknown");
    }

    #[test]
    fn v1_namespace_scope_matches_rl_03() {
        // Directory record, inode, dstream id, xattr, sibling link/map.
        for raw in [0x3u8, 0x4, 0x5, 0x6, 0x9, 0xc] {
            assert!(
                family_in_v1_namespace_scope(raw),
                "{raw:#x} should be in scope"
            );
        }
        for raw in [0x1u8, 0x2, 0x7, 0x8, 0xa, 0xb, 0xd] {
            assert!(
                !family_in_v1_namespace_scope(raw),
                "{raw:#x} must stay out of the v1 namespace scope"
            );
        }
    }

    #[test]
    fn known_family_range_is_one_through_thirteen() {
        for raw in 1u8..=13 {
            assert!(is_known_record_family(raw));
        }
        assert!(!is_known_record_family(0));
        assert!(!is_known_record_family(14));
        assert!(!is_known_record_family(0xff));
    }
}
