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
use crate::object::ExpectedStorage;
use crate::ScanError;

/// FS-tree blocks are stored as virtual objects: the root is reached through
/// the volume OMAP and child paddrs in internal nodes still address virtual
/// data even though the address itself is physical. We do not enforce
/// `o_oid == paddr` here because virtual objects carry virtual OIDs.
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FamilyCount {
    pub raw_type: u8,
    pub name: &'static str,
    pub count: u32,
    pub in_v1_namespace_scope: bool,
}

/// Walk the FS-tree at `root_paddr` and count records by family.
pub(crate) fn dump_fs_records<R: Read + Seek>(
    reader: &mut R,
    block_size: usize,
    volume_index: u32,
    volume_oid: u64,
    root_paddr: u64,
    max_xid: u64,
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
    };
    walk_fs_node(&mut state, reader, root_paddr, true)?;
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
}

fn walk_fs_node<R: Read + Seek>(
    state: &mut DumpState,
    reader: &mut R,
    paddr: u64,
    is_root: bool,
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
            }
        }
        return Ok(());
    }

    state.index_node_count += 1;
    for index in 0..node.nkeys {
        let entry = node.entry(index)?;
        let value = node.value_bytes(&entry, FS_INTERNAL_VAL_SIZE);
        if value.len() < FS_INTERNAL_VAL_SIZE {
            return Err(ScanError::InvalidObject(format!(
                "FS-tree internal value at {paddr} entry {index} shorter than child paddr"
            )));
        }
        let child_paddr =
            u64::from_le_bytes(value[..FS_INTERNAL_VAL_SIZE].try_into().expect("paddr u64"));
        walk_fs_node(state, reader, child_paddr, false)?;
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
