//! APFS Object Map (OMAP) reader for the v1 native parser.
//!
//! `SR-006` says the resolver key is `(omap context, oid, max_xid)` and that
//! ambiguous mappings, encrypted values, and zero-header values are hard
//! stops. This module implements the lookup primitive plus a diagnostic
//! traversal that lets later stages report what the OMAP carries without
//! claiming any specific mapping is correct.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use serde::Serialize;

use crate::block_io::{le_u32, le_u64, read_block};
use crate::btree::{read_btree_node, BtreeNode};
use crate::object::{validate_object_block, ExpectedStorage, ObjectExpectation, OBJECT_TYPE_OMAP};
use crate::ScanError;

const OMAP_TREE_STORAGE: ExpectedStorage = ExpectedStorage::Physical;

/// `omap_key_t` size as defined by Apple's APFS reference.
pub(crate) const OMAP_KEY_SIZE: usize = 16;
/// `omap_val_t` size as defined by Apple's APFS reference.
pub(crate) const OMAP_VAL_SIZE: usize = 16;
/// Internal-node values are physical block addresses (`u64`).
const OMAP_INTERNAL_VAL_SIZE: usize = 8;

const OMAP_VAL_DELETED: u32 = 0x0000_0001;
const OMAP_VAL_SAVED: u32 = 0x0000_0002;
const OMAP_VAL_ENCRYPTED: u32 = 0x0000_0004;
const OMAP_VAL_NOHEADER: u32 = 0x0000_0008;
const OMAP_VAL_CRYPTO_GENERATION: u32 = 0x0000_0010;

/// Value-flag bits the v1 resolver knows how to interpret. Anything outside
/// this allowlist is treated as a hard stop per `SR-006` ("unknown flag bits
/// are hard stops until the caller has type-specific support"). DELETED is
/// recognized but is translated into a negative lookup result rather than an
/// error.
const OMAP_VAL_KNOWN_BITS: u32 = OMAP_VAL_DELETED
    | OMAP_VAL_SAVED
    | OMAP_VAL_ENCRYPTED
    | OMAP_VAL_NOHEADER
    | OMAP_VAL_CRYPTO_GENERATION;

/// `omap_phys_t.om_flags` bit positions per the APFS reference (`omap.h`).
const OMAP_MANUALLY_MANAGED: u32 = 0x0000_0001;
const OMAP_PHYS_ENCRYPTING: u32 = 0x0000_0002;
const OMAP_PHYS_DECRYPTING: u32 = 0x0000_0004;
const OMAP_PHYS_KEYROLLING: u32 = 0x0000_0008;
const OMAP_PHYS_CRYPTO_GENERATION_FLAG: u32 = 0x0000_0010;

/// Phys-flag bits the v1 resolver accepts. `OMAP_MANUALLY_MANAGED` is
/// expected on volume OMAPs. Encryption/key-rolling state and any unknown
/// bits are hard stops.
const OMAP_PHYS_ALLOWED_BITS: u32 = OMAP_MANUALLY_MANAGED;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmapPhysSummary {
    pub block_address: u64,
    pub flags: u32,
    pub snapshot_count: u32,
    pub tree_type_raw: u32,
    pub snapshot_tree_type_raw: u32,
    pub tree_oid: u64,
    pub snapshot_tree_oid: u64,
    pub most_recent_snap: u64,
    pub pending_revert_min: u64,
    pub pending_revert_max: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmapValue {
    pub oid: u64,
    pub xid: u64,
    pub paddr: u64,
    pub flags: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmapDumpEntry {
    pub oid: u64,
    pub xid: u64,
    pub paddr: u64,
    pub flags: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OmapSummary {
    pub phys: OmapPhysSummary,
    pub leaf_node_count: u32,
    pub index_node_count: u32,
    pub mapping_count: u32,
    pub max_observed_xid: u64,
    pub flagged_values: Vec<String>,
    pub sample_mappings: Vec<OmapDumpEntry>,
}

pub(crate) struct OmapResolver {
    pub phys: OmapPhysSummary,
    pub tree_root_paddr: u64,
}

impl OmapResolver {
    pub fn open<R: Read + Seek>(
        reader: &mut R,
        block_size: usize,
        omap_paddr: u64,
    ) -> Result<Self, ScanError> {
        let block = read_block(reader, omap_paddr, block_size)?;
        let _header = validate_object_block(
            &block,
            omap_paddr,
            ObjectExpectation::physical(OBJECT_TYPE_OMAP),
        )?;
        let flags = le_u32(&block, 0x20);
        if let Some(reason) = phys_flag_hard_stop_reason(flags) {
            return Err(ScanError::InvalidObject(format!(
                "OMAP at paddr {omap_paddr} has unsupported om_flags {flags:#010x}: {reason}"
            )));
        }
        let snapshot_count = le_u32(&block, 0x24);
        let tree_type_raw = le_u32(&block, 0x28);
        let snapshot_tree_type_raw = le_u32(&block, 0x2c);
        let tree_oid = le_u64(&block, 0x30);
        let snapshot_tree_oid = le_u64(&block, 0x38);
        let most_recent_snap = le_u64(&block, 0x40);
        let pending_revert_min = le_u64(&block, 0x48);
        let pending_revert_max = le_u64(&block, 0x50);
        Ok(Self {
            phys: OmapPhysSummary {
                block_address: omap_paddr,
                flags,
                snapshot_count,
                tree_type_raw,
                snapshot_tree_type_raw,
                tree_oid,
                snapshot_tree_oid,
                most_recent_snap,
                pending_revert_min,
                pending_revert_max,
            },
            tree_root_paddr: tree_oid,
        })
    }

    /// `(oid, max_xid)` lower-bound lookup as required by `SR-006`.
    ///
    /// Walks from the tree root down the rightmost child whose smallest key is
    /// `<= (oid, max_xid)`, then picks the largest leaf entry with the same
    /// invariant. If the resulting entry's `oid` does not equal the requested
    /// `oid`, the lookup returns `Ok(None)`.
    pub fn lookup<R: Read + Seek>(
        &self,
        reader: &mut R,
        block_size: usize,
        oid: u64,
        max_xid: u64,
    ) -> Result<Option<OmapValue>, ScanError> {
        let mut current_paddr = self.tree_root_paddr;
        let mut is_root = true;
        loop {
            let (block, _header) = read_btree_node(
                reader,
                block_size,
                current_paddr,
                is_root,
                OMAP_TREE_STORAGE,
                None,
            )?;
            let node = BtreeNode::parse(&block, block_size)?;
            if node.nkeys == 0 {
                return Ok(None);
            }
            if !node.fixed_kv_size {
                return Err(ScanError::InvalidObject(
                    "OMAP B-tree node is not fixed-kv-size".to_string(),
                ));
            }

            if node.is_leaf {
                return select_leaf_lower_bound(&node, oid, max_xid);
            }

            let child_index = select_internal_lower_bound(&node, oid, max_xid)?;
            let entry = node.entry(child_index)?;
            let value = node.value_bytes(&entry, OMAP_INTERNAL_VAL_SIZE);
            current_paddr = u64::from_le_bytes(value.try_into().expect("internal paddr u64"));
            is_root = false;
        }
    }

    /// Walk the entire OMAP tree and report a diagnostic summary.
    ///
    /// Used by the native dumper to record what the OMAP carries without
    /// claiming any specific mapping is in scope yet. Encrypted, no-header,
    /// or pending-revert flagged entries are surfaced as
    /// [`OmapSummary::flagged_values`].
    pub fn summarize<R: Read + Seek>(
        &self,
        reader: &mut R,
        block_size: usize,
        max_xid: u64,
        sample_limit: usize,
    ) -> Result<OmapSummary, ScanError> {
        let mut state = SummarizeState {
            block_size,
            max_xid,
            sample_limit,
            leaf_node_count: 0,
            index_node_count: 0,
            mapping_count: 0,
            max_observed_xid: 0,
            flagged_values: BTreeMap::new(),
            samples: Vec::new(),
        };
        walk_node(&mut state, reader, self.tree_root_paddr, true)?;
        Ok(OmapSummary {
            phys: self.phys.clone(),
            leaf_node_count: state.leaf_node_count,
            index_node_count: state.index_node_count,
            mapping_count: state.mapping_count,
            max_observed_xid: state.max_observed_xid,
            flagged_values: state
                .flagged_values
                .into_iter()
                .map(|(name, count)| format!("{name}: {count}"))
                .collect(),
            sample_mappings: state.samples,
        })
    }
}

struct SummarizeState {
    block_size: usize,
    max_xid: u64,
    sample_limit: usize,
    leaf_node_count: u32,
    index_node_count: u32,
    mapping_count: u32,
    max_observed_xid: u64,
    flagged_values: BTreeMap<&'static str, u32>,
    samples: Vec<OmapDumpEntry>,
}

fn walk_node<R: Read + Seek>(
    state: &mut SummarizeState,
    reader: &mut R,
    paddr: u64,
    is_root: bool,
) -> Result<(), ScanError> {
    let (block, _header) = read_btree_node(
        reader,
        state.block_size,
        paddr,
        is_root,
        OMAP_TREE_STORAGE,
        None,
    )?;
    let node = BtreeNode::parse(&block, state.block_size)?;
    if !node.fixed_kv_size {
        return Err(ScanError::InvalidObject(
            "OMAP B-tree node is not fixed-kv-size".to_string(),
        ));
    }

    if node.is_leaf {
        state.leaf_node_count += 1;
        for index in 0..node.nkeys {
            let entry = node.entry(index)?;
            let key = node.key_bytes(&entry, OMAP_KEY_SIZE);
            let value = node.value_bytes(&entry, OMAP_VAL_SIZE);
            let oid = u64::from_le_bytes(key[0..8].try_into().unwrap());
            let xid = u64::from_le_bytes(key[8..16].try_into().unwrap());
            let flags = u32::from_le_bytes(value[0..4].try_into().unwrap());
            let size = u32::from_le_bytes(value[4..8].try_into().unwrap());
            let val_paddr = u64::from_le_bytes(value[8..16].try_into().unwrap());
            classify_flags(flags, &mut state.flagged_values);
            if xid <= state.max_xid {
                state.mapping_count += 1;
                if xid > state.max_observed_xid {
                    state.max_observed_xid = xid;
                }
                if state.samples.len() < state.sample_limit {
                    state.samples.push(OmapDumpEntry {
                        oid,
                        xid,
                        paddr: val_paddr,
                        flags,
                        size,
                    });
                }
            }
        }
        return Ok(());
    }

    state.index_node_count += 1;
    for index in 0..node.nkeys {
        let entry = node.entry(index)?;
        let value = node.value_bytes(&entry, OMAP_INTERNAL_VAL_SIZE);
        let child_paddr = u64::from_le_bytes(value.try_into().expect("internal paddr u64"));
        walk_node(state, reader, child_paddr, false)?;
    }
    Ok(())
}

fn classify_flags(flags: u32, counts: &mut BTreeMap<&'static str, u32>) {
    if flags & OMAP_VAL_DELETED != 0 {
        *counts.entry("deleted").or_insert(0) += 1;
    }
    if flags & OMAP_VAL_SAVED != 0 {
        *counts.entry("saved").or_insert(0) += 1;
    }
    if flags & OMAP_VAL_ENCRYPTED != 0 {
        *counts.entry("encrypted").or_insert(0) += 1;
    }
    if flags & OMAP_VAL_NOHEADER != 0 {
        *counts.entry("noheader").or_insert(0) += 1;
    }
    if flags & OMAP_VAL_CRYPTO_GENERATION != 0 {
        *counts.entry("crypto_generation").or_insert(0) += 1;
    }
}

fn select_internal_lower_bound(
    node: &BtreeNode<'_>,
    oid: u64,
    max_xid: u64,
) -> Result<u32, ScanError> {
    if node.nkeys == 0 {
        return Err(ScanError::InvalidObject(
            "internal OMAP node has zero keys".to_string(),
        ));
    }
    let mut chosen: Option<u32> = None;
    for index in 0..node.nkeys {
        let entry = node.entry(index)?;
        let key = node.key_bytes(&entry, OMAP_KEY_SIZE);
        let entry_oid = u64::from_le_bytes(key[0..8].try_into().unwrap());
        let entry_xid = u64::from_le_bytes(key[8..16].try_into().unwrap());
        if (entry_oid, entry_xid) <= (oid, max_xid) {
            chosen = Some(index);
        } else {
            break;
        }
    }
    Ok(chosen.unwrap_or(0))
}

fn select_leaf_lower_bound(
    node: &BtreeNode<'_>,
    oid: u64,
    max_xid: u64,
) -> Result<Option<OmapValue>, ScanError> {
    let mut best: Option<(u64, u64, u32, u32, u64)> = None;
    for index in 0..node.nkeys {
        let entry = node.entry(index)?;
        let key = node.key_bytes(&entry, OMAP_KEY_SIZE);
        let entry_oid = u64::from_le_bytes(key[0..8].try_into().unwrap());
        let entry_xid = u64::from_le_bytes(key[8..16].try_into().unwrap());
        if (entry_oid, entry_xid) > (oid, max_xid) {
            break;
        }
        let value = node.value_bytes(&entry, OMAP_VAL_SIZE);
        let flags = u32::from_le_bytes(value[0..4].try_into().unwrap());
        let size = u32::from_le_bytes(value[4..8].try_into().unwrap());
        let paddr = u64::from_le_bytes(value[8..16].try_into().unwrap());
        best = Some((entry_oid, entry_xid, flags, size, paddr));
    }
    let Some((entry_oid, entry_xid, flags, size, paddr)) = best else {
        return Ok(None);
    };
    if entry_oid != oid {
        return Ok(None);
    }
    if flags & OMAP_VAL_DELETED != 0 {
        return Ok(None);
    }
    if let Some(reason) = value_flag_hard_stop_reason(flags) {
        return Err(ScanError::InvalidObject(format!(
            "OMAP value for oid {entry_oid} has unsupported flags {flags:#010x}: {reason}"
        )));
    }
    Ok(Some(OmapValue {
        oid: entry_oid,
        xid: entry_xid,
        paddr,
        flags,
        size,
    }))
}

/// Returns a human-readable reason if `flags` contains any
/// `omap_val_t.ov_flags` bit that the v1 resolver must hard-stop on. The
/// caller is expected to short-circuit `OMAP_VAL_DELETED` separately because
/// it is a negative result, not an error.
fn value_flag_hard_stop_reason(flags: u32) -> Option<&'static str> {
    if flags & OMAP_VAL_ENCRYPTED != 0 {
        return Some("OMAP_VAL_ENCRYPTED");
    }
    if flags & OMAP_VAL_NOHEADER != 0 {
        return Some("OMAP_VAL_NOHEADER");
    }
    if flags & OMAP_VAL_CRYPTO_GENERATION != 0 {
        return Some("OMAP_VAL_CRYPTO_GENERATION");
    }
    let unknown = flags & !OMAP_VAL_KNOWN_BITS;
    if unknown != 0 {
        return Some("unknown OMAP value flag bits");
    }
    None
}

/// Returns a human-readable reason if `flags` contains any
/// `omap_phys_t.om_flags` bit that the v1 resolver must hard-stop on at OMAP
/// open time. `OMAP_MANUALLY_MANAGED` is the only supported bit.
fn phys_flag_hard_stop_reason(flags: u32) -> Option<&'static str> {
    if flags & OMAP_PHYS_ENCRYPTING != 0 {
        return Some("OMAP_ENCRYPTING");
    }
    if flags & OMAP_PHYS_DECRYPTING != 0 {
        return Some("OMAP_DECRYPTING");
    }
    if flags & OMAP_PHYS_KEYROLLING != 0 {
        return Some("OMAP_KEYROLLING");
    }
    if flags & OMAP_PHYS_CRYPTO_GENERATION_FLAG != 0 {
        return Some("OMAP_CRYPTO_GENERATION_FLAG");
    }
    let unknown = flags & !OMAP_PHYS_ALLOWED_BITS;
    if unknown != 0 {
        return Some("unknown OMAP phys flag bits");
    }
    None
}

#[allow(dead_code)]
pub(crate) fn omap_storage_class() -> ExpectedStorage {
    ExpectedStorage::Physical
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_io::{put_u32, put_u64, resign_block};
    use crate::object::{OBJECT_TYPE_BTREE, OBJ_HEADER_SIZE, OBJ_PHYSICAL};
    use std::io::Cursor;

    const BLOCK_SIZE: usize = 4096;

    /// Layout-compatible single-leaf OMAP B-tree node:
    /// * fixed-kv-size + leaf + root (info trailer at end of block),
    /// * keys are `omap_key_t` (16 bytes), values are `omap_val_t` (16 bytes).
    fn make_leaf_omap_root(paddr: u64, mappings: &[(u64, u64, u32, u32, u64)]) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        let nkeys = mappings.len() as u32;
        // obj_phys_t header
        put_u64(&mut block, 0x08, paddr);
        put_u64(&mut block, 0x10, 14);
        put_u32(&mut block, 0x18, OBJ_PHYSICAL | OBJECT_TYPE_BTREE);
        put_u32(&mut block, 0x1c, OBJECT_TYPE_OMAP);
        // btree_node_phys_t starting at OBJ_HEADER_SIZE (32)
        let bt_flags: u16 = 0x0001 | 0x0002 | 0x0004; // ROOT | LEAF | FIXED_KV_SIZE
        let level: u16 = 0;
        block[0x20..0x22].copy_from_slice(&bt_flags.to_le_bytes());
        block[0x22..0x24].copy_from_slice(&level.to_le_bytes());
        block[0x24..0x28].copy_from_slice(&nkeys.to_le_bytes());
        // nloc_t toc: relative offset 0, length covers 4 bytes per fixed entry.
        let toc_len: u16 = (nkeys as u16) * 4;
        block[0x28..0x2a].copy_from_slice(&0u16.to_le_bytes());
        block[0x2a..0x2c].copy_from_slice(&toc_len.to_le_bytes());
        // Free, key_free, val_free are unused by the read path; keep zero.
        let data_offset = OBJ_HEADER_SIZE + 24;
        let toc_offset = data_offset;
        let key_area_offset = toc_offset + toc_len as usize;
        let value_area_end = BLOCK_SIZE - 40; // root nodes leave a trailing btree_info_t (40 bytes)
                                              // Place keys densely at start of the key area, values densely at end of
                                              // value area (so v_off counts back from value_area_end to value start).
        for (index, (oid, xid, _flags, _size, _paddr)) in mappings.iter().enumerate() {
            let entry_off = toc_offset + 4 * index;
            let k_off = (index * OMAP_KEY_SIZE) as u16;
            // Values are stored back-to-back at the end of the value area.
            // First entry sits at the largest offset (closest to the end).
            let v_off = ((index + 1) * OMAP_VAL_SIZE) as u16;
            block[entry_off..entry_off + 2].copy_from_slice(&k_off.to_le_bytes());
            block[entry_off + 2..entry_off + 4].copy_from_slice(&v_off.to_le_bytes());
            let key_start = key_area_offset + k_off as usize;
            block[key_start..key_start + 8].copy_from_slice(&oid.to_le_bytes());
            block[key_start + 8..key_start + 16].copy_from_slice(&xid.to_le_bytes());
            let value_start = value_area_end - v_off as usize;
            let (_, _, flags, size, value_paddr) = mappings[index];
            block[value_start..value_start + 4].copy_from_slice(&flags.to_le_bytes());
            block[value_start + 4..value_start + 8].copy_from_slice(&size.to_le_bytes());
            block[value_start + 8..value_start + 16].copy_from_slice(&value_paddr.to_le_bytes());
        }
        // btree_info_t trailer: only the fixed key/value sizes are read.
        let info_off = BLOCK_SIZE - 40;
        // btree_info_fixed_t starts at info_off, and we only need
        // bt_fixed.bt_key_size at +0x10 and bt_fixed.bt_val_size at +0x14
        // (relative to info_off). We populate them defensively.
        block[info_off + 0x10..info_off + 0x14]
            .copy_from_slice(&(OMAP_KEY_SIZE as u32).to_le_bytes());
        block[info_off + 0x14..info_off + 0x18]
            .copy_from_slice(&(OMAP_VAL_SIZE as u32).to_le_bytes());
        resign_block(&mut block);
        block
    }

    fn make_omap_phys(omap_paddr: u64, tree_root_paddr: u64) -> Vec<u8> {
        make_omap_phys_with_flags(omap_paddr, tree_root_paddr, OMAP_MANUALLY_MANAGED)
    }

    fn make_omap_phys_with_flags(
        omap_paddr: u64,
        tree_root_paddr: u64,
        phys_flags: u32,
    ) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_SIZE];
        put_u64(&mut block, 0x08, omap_paddr);
        put_u64(&mut block, 0x10, 14);
        put_u32(&mut block, 0x18, OBJ_PHYSICAL | OBJECT_TYPE_OMAP);
        put_u32(&mut block, 0x1c, 0);
        put_u32(&mut block, 0x20, phys_flags);
        put_u32(&mut block, 0x24, 0);
        put_u32(&mut block, 0x28, OBJ_PHYSICAL | OBJECT_TYPE_BTREE);
        put_u32(&mut block, 0x2c, OBJ_PHYSICAL | OBJECT_TYPE_BTREE);
        put_u64(&mut block, 0x30, tree_root_paddr);
        put_u64(&mut block, 0x38, 0);
        put_u64(&mut block, 0x40, 0);
        put_u64(&mut block, 0x48, 0);
        put_u64(&mut block, 0x50, 0);
        resign_block(&mut block);
        block
    }

    fn build_image_with_phys_flags(
        omap_paddr: u64,
        tree_root_paddr: u64,
        mappings: &[(u64, u64, u32, u32, u64)],
        phys_flags: u32,
    ) -> Vec<u8> {
        let total_blocks = (omap_paddr.max(tree_root_paddr) as usize) + 1;
        let mut image = vec![0u8; total_blocks * BLOCK_SIZE];
        let omap = make_omap_phys_with_flags(omap_paddr, tree_root_paddr, phys_flags);
        let start = (omap_paddr as usize) * BLOCK_SIZE;
        image[start..start + BLOCK_SIZE].copy_from_slice(&omap);
        let leaf = make_leaf_omap_root(tree_root_paddr, mappings);
        let start = (tree_root_paddr as usize) * BLOCK_SIZE;
        image[start..start + BLOCK_SIZE].copy_from_slice(&leaf);
        image
    }

    fn build_image(
        omap_paddr: u64,
        tree_root_paddr: u64,
        mappings: &[(u64, u64, u32, u32, u64)],
    ) -> Vec<u8> {
        let total_blocks = (omap_paddr.max(tree_root_paddr) as usize) + 1;
        let mut image = vec![0u8; total_blocks * BLOCK_SIZE];
        let omap = make_omap_phys(omap_paddr, tree_root_paddr);
        let start = (omap_paddr as usize) * BLOCK_SIZE;
        image[start..start + BLOCK_SIZE].copy_from_slice(&omap);
        let leaf = make_leaf_omap_root(tree_root_paddr, mappings);
        let start = (tree_root_paddr as usize) * BLOCK_SIZE;
        image[start..start + BLOCK_SIZE].copy_from_slice(&leaf);
        image
    }

    #[test]
    fn lookup_returns_largest_xid_at_or_below_max() {
        // Three mappings for oid 100 at xids 5, 10, 20. A lookup with
        // max_xid=15 must return the xid=10 entry (lower-bound semantics).
        let mappings = vec![
            (100u64, 5u64, 0u32, BLOCK_SIZE as u32, 1000u64),
            (100, 10, 0, BLOCK_SIZE as u32, 2000),
            (100, 20, 0, BLOCK_SIZE as u32, 3000),
        ];
        let image = build_image(2, 3, &mappings);
        let mut cursor = Cursor::new(image);
        let resolver = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let value = resolver
            .lookup(&mut cursor, BLOCK_SIZE, 100, 15)
            .expect("lookup ok")
            .expect("entry returned");
        assert_eq!(value.oid, 100);
        assert_eq!(value.xid, 10);
        assert_eq!(value.paddr, 2000);
    }

    #[test]
    fn lookup_returns_none_when_oid_missing() {
        let mappings = vec![(100u64, 5u64, 0u32, BLOCK_SIZE as u32, 1000u64)];
        let image = build_image(2, 3, &mappings);
        let mut cursor = Cursor::new(image);
        let resolver = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let value = resolver
            .lookup(&mut cursor, BLOCK_SIZE, 200, 100)
            .expect("lookup ok");
        assert!(value.is_none());
    }

    #[test]
    fn lookup_rejects_encrypted_value() {
        let mappings = vec![(100u64, 5u64, OMAP_VAL_ENCRYPTED, BLOCK_SIZE as u32, 1000u64)];
        let image = build_image(2, 3, &mappings);
        let mut cursor = Cursor::new(image);
        let resolver = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let err = resolver
            .lookup(&mut cursor, BLOCK_SIZE, 100, 100)
            .expect_err("encrypted value forces a hard stop");
        assert!(
            matches!(err, ScanError::InvalidObject(reason) if reason.contains("OMAP_VAL_ENCRYPTED"))
        );
    }

    #[test]
    fn lookup_skips_deleted_value() {
        let mappings = vec![(100u64, 5u64, OMAP_VAL_DELETED, BLOCK_SIZE as u32, 1000u64)];
        let image = build_image(2, 3, &mappings);
        let mut cursor = Cursor::new(image);
        let resolver = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let value = resolver
            .lookup(&mut cursor, BLOCK_SIZE, 100, 100)
            .expect("lookup ok");
        assert!(value.is_none(), "deleted entries are filtered from lookup");
    }

    #[test]
    fn lookup_returns_none_when_max_xid_below_smallest() {
        let mappings = vec![(100u64, 50u64, 0u32, BLOCK_SIZE as u32, 1000u64)];
        let image = build_image(2, 3, &mappings);
        let mut cursor = Cursor::new(image);
        let resolver = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let value = resolver
            .lookup(&mut cursor, BLOCK_SIZE, 100, 10)
            .expect("lookup ok");
        assert!(
            value.is_none(),
            "max_xid below smallest mapping must not match"
        );
    }

    #[test]
    fn summarize_records_flagged_values() {
        let mappings = vec![
            (100u64, 5u64, OMAP_VAL_NOHEADER, BLOCK_SIZE as u32, 1000u64),
            (101, 5, OMAP_VAL_DELETED, BLOCK_SIZE as u32, 1001),
        ];
        let image = build_image(2, 3, &mappings);
        let mut cursor = Cursor::new(image);
        let resolver = OmapResolver::open(&mut cursor, BLOCK_SIZE, 2).expect("omap opens");
        let summary = resolver
            .summarize(&mut cursor, BLOCK_SIZE, 100, 8)
            .expect("summary ok");
        assert_eq!(summary.mapping_count, 2);
        assert!(summary
            .flagged_values
            .iter()
            .any(|line| line.starts_with("noheader: ")));
        assert!(summary
            .flagged_values
            .iter()
            .any(|line| line.starts_with("deleted: ")));
    }
}
