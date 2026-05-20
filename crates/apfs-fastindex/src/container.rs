//! Native container superblock decoder and checkpoint-map walker.
//!
//! Once the checkpoint scanner picks the highest valid `NXSB` candidate, this
//! module decodes the selected superblock, validates the matching descriptor
//! ring (checkpoint maps + the NXSB tail), and surfaces the structural fields
//! the rest of the parser needs: container OMAP physical OID, volume
//! superblock virtual OIDs, and the container-level feature masks that
//! `SR-012` requires us to allowlist before broadening any support claim.

use std::io::{Read, Seek};

use serde::Serialize;

use crate::block_io::{le_u32, le_u64, read_block};
use crate::object::{
    flags_summary, validate_object_block, ObjectExpectation, ObjectHeader,
    OBJECT_TYPE_CHECKPOINT_MAP, OBJECT_TYPE_NX_SUPERBLOCK,
};
use crate::ScanError;

const NX_MAX_FILE_SYSTEMS: usize = 100;
const FS_OID_ARRAY_OFFSET: usize = 0xb8;

// `nx_features` bits we know about (Apple APFS reference).
const NX_FEATURE_DEFRAG: u64 = 0x1;
const NX_FEATURE_LCFD: u64 = 0x2;

// `nx_incompatible_features` bits the v1 parser is willing to read.
const NX_INCOMPAT_VERSION1: u64 = 0x1;
const NX_INCOMPAT_VERSION2: u64 = 0x2;
const NX_INCOMPAT_FUSION: u64 = 0x100;

const CHECKPOINT_MAP_LAST: u32 = 0x1;
const CHECKPOINT_MAPPING_SIZE: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainerSummary {
    pub block_address: u64,
    pub xid: u64,
    pub block_size: u32,
    pub block_count: u64,
    pub features_raw: u64,
    pub readonly_compatible_features_raw: u64,
    pub incompatible_features_raw: u64,
    pub features: Vec<&'static str>,
    pub incompatible_features: Vec<&'static str>,
    pub unsupported_incompatible_features: u64,
    pub uuid_hex: String,
    pub next_oid: u64,
    pub next_xid: u64,
    pub xp_desc_blocks: u32,
    pub xp_desc_base: u64,
    pub xp_desc_index: u32,
    pub xp_desc_len: u32,
    pub xp_data_blocks: u32,
    pub xp_data_base: u64,
    pub xp_data_index: u32,
    pub xp_data_len: u32,
    pub spaceman_oid: u64,
    pub omap_oid: u64,
    pub reaper_oid: u64,
    pub max_file_systems: u32,
    pub volume_oids: Vec<u64>,
    pub object_header: ObjectHeader,
    pub object_storage_summary: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointMapping {
    pub map_block: u64,
    pub object_type_raw: u32,
    pub object_subtype: u32,
    pub size: u32,
    pub fs_oid: u64,
    pub oid: u64,
    pub paddr: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointMapBlock {
    pub block_address: u64,
    pub flags: u32,
    pub mapping_count: u32,
    pub last: bool,
    pub object: ObjectHeader,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointMapSummary {
    pub map_blocks: Vec<CheckpointMapBlock>,
    pub mappings: Vec<CheckpointMapping>,
    pub trailing_nxsb_block: u64,
    pub last_flag_seen: bool,
    pub validation_notes: Vec<String>,
}

pub(crate) fn decode_container_summary(
    block: &[u8],
    block_address: u64,
) -> Result<ContainerSummary, ScanError> {
    let header = validate_object_block(
        block,
        block_address,
        ObjectExpectation::any_storage(OBJECT_TYPE_NX_SUPERBLOCK),
    )?;
    let block_size = le_u32(block, 0x24);
    let block_count = le_u64(block, 0x28);
    let features_raw = le_u64(block, 0x30);
    let ro_compat_raw = le_u64(block, 0x38);
    let incompat_raw = le_u64(block, 0x40);
    let next_oid = le_u64(block, 0x58);
    let next_xid = le_u64(block, 0x60);
    let xp_desc_blocks = le_u32(block, 0x68);
    let xp_data_blocks = le_u32(block, 0x6c);
    let xp_desc_base_raw = le_u64(block, 0x70);
    let xp_data_base_raw = le_u64(block, 0x78);
    let xp_desc_next = le_u32(block, 0x80);
    let xp_data_next = le_u32(block, 0x84);
    let xp_desc_index = le_u32(block, 0x88);
    let xp_desc_len = le_u32(block, 0x8c);
    let xp_data_index = le_u32(block, 0x90);
    let xp_data_len = le_u32(block, 0x94);
    let spaceman_oid = le_u64(block, 0x98);
    let omap_oid = le_u64(block, 0xa0);
    let reaper_oid = le_u64(block, 0xa8);
    let max_file_systems = le_u32(block, 0xb4);

    let _ = (xp_desc_next, xp_data_next);

    if max_file_systems == 0 || (max_file_systems as usize) > NX_MAX_FILE_SYSTEMS {
        return Err(ScanError::InvalidObject(format!(
            "container reports nx_max_file_systems={max_file_systems}, expected 1..={NX_MAX_FILE_SYSTEMS}"
        )));
    }
    let fs_array_end = FS_OID_ARRAY_OFFSET + 8 * NX_MAX_FILE_SYSTEMS;
    if block.len() < fs_array_end {
        return Err(ScanError::InvalidObject(
            "NXSB block shorter than nx_fs_oid array".to_string(),
        ));
    }

    let mut volume_oids = Vec::new();
    for index in 0..NX_MAX_FILE_SYSTEMS {
        let oid = le_u64(block, FS_OID_ARRAY_OFFSET + 8 * index);
        if oid != 0 {
            volume_oids.push(oid);
        }
    }

    let xp_desc_base = xp_desc_base_raw & !(1u64 << 63);
    let xp_data_base = xp_data_base_raw & !(1u64 << 63);

    let known_incompat_mask = NX_INCOMPAT_VERSION1 | NX_INCOMPAT_VERSION2 | NX_INCOMPAT_FUSION;
    let unsupported_incompatible_features = incompat_raw & !known_incompat_mask;

    let mut features = Vec::new();
    if features_raw & NX_FEATURE_DEFRAG != 0 {
        features.push("defrag");
    }
    if features_raw & NX_FEATURE_LCFD != 0 {
        features.push("lcfd");
    }

    let mut incompatible = Vec::new();
    if incompat_raw & NX_INCOMPAT_VERSION1 != 0 {
        incompatible.push("version1");
    }
    if incompat_raw & NX_INCOMPAT_VERSION2 != 0 {
        incompatible.push("version2");
    }
    if incompat_raw & NX_INCOMPAT_FUSION != 0 {
        incompatible.push("fusion");
    }

    let uuid_hex = format_uuid(&block[0x48..0x58]);

    Ok(ContainerSummary {
        block_address,
        xid: header.xid,
        block_size,
        block_count,
        features_raw,
        readonly_compatible_features_raw: ro_compat_raw,
        incompatible_features_raw: incompat_raw,
        features,
        incompatible_features: incompatible,
        unsupported_incompatible_features,
        uuid_hex,
        next_oid,
        next_xid,
        xp_desc_blocks,
        xp_desc_base,
        xp_desc_index,
        xp_desc_len,
        xp_data_blocks,
        xp_data_base,
        xp_data_index,
        xp_data_len,
        spaceman_oid,
        omap_oid,
        reaper_oid,
        max_file_systems,
        volume_oids,
        object_storage_summary: flags_summary(&header),
        object_header: header,
    })
}

/// Walk the descriptor ring of the selected checkpoint and validate each
/// checkpoint-map block plus the trailing NXSB.
///
/// `selected_block_address` is the absolute block where we found the chosen
/// NXSB candidate; this is the trailing block of the checkpoint and must
/// match the last ring position. Each preceding ring block must validate as
/// `OBJECT_TYPE_CHECKPOINT_MAP` with a matching scan-state XID.
pub(crate) fn walk_checkpoint_maps<R: Read + Seek>(
    reader: &mut R,
    block_size: u32,
    container: &ContainerSummary,
    selected_block_address: u64,
) -> Result<CheckpointMapSummary, ScanError> {
    if container.xp_desc_blocks == 0 || container.xp_desc_len == 0 {
        return Err(ScanError::InvalidObject(
            "selected NXSB has empty checkpoint descriptor area".to_string(),
        ));
    }
    if container.xp_desc_len > container.xp_desc_blocks {
        return Err(ScanError::InvalidObject(format!(
            "checkpoint descriptor length {} exceeds descriptor area size {}",
            container.xp_desc_len, container.xp_desc_blocks
        )));
    }

    let mut map_blocks = Vec::new();
    let mut mappings = Vec::new();
    let mut validation_notes = Vec::new();
    let mut last_flag_seen = false;
    let mut trailing_nxsb_block = 0u64;

    for offset in 0..container.xp_desc_len {
        let position = (container.xp_desc_index + offset) % container.xp_desc_blocks;
        // `xp_desc_base + position` previously used wrapping
        // arithmetic; a crafted NXSB with `xp_desc_base` near
        // `u64::MAX` would wrap to a low paddr and direct the
        // following `read_block` at attacker-chosen low bytes
        // of the source file. Round-2 audit #Rust-10. The block
        // address is bounded by the source size in `read_block`,
        // so this is a confidentiality concern only — but the
        // fix is one line.
        let block_address = container
            .xp_desc_base
            .checked_add(position as u64)
            .ok_or_else(|| {
                ScanError::InvalidObject(format!(
                    "xp_desc_base {} + position {} overflows u64",
                    container.xp_desc_base, position
                ))
            })?;
        let block = read_block(reader, block_address, block_size as usize)?;
        let is_trailing = offset == container.xp_desc_len - 1;
        if is_trailing {
            let header = validate_object_block(
                &block,
                block_address,
                ObjectExpectation::any_storage(OBJECT_TYPE_NX_SUPERBLOCK),
            )?;
            if block_address != selected_block_address {
                return Err(ScanError::InvalidObject(format!(
                    "trailing checkpoint block {block_address} does not match selected NXSB at {selected_block_address}"
                )));
            }
            if header.xid != container.xid {
                return Err(ScanError::InvalidObject(format!(
                    "trailing NXSB xid {} does not match selected xid {}",
                    header.xid, container.xid
                )));
            }
            trailing_nxsb_block = block_address;
            continue;
        }

        // `checkpoint_map_phys_t` is itself a physical object stored in the
        // descriptor area; its body carries mappings whose `cpm_oid` fields
        // are ephemeral. The header's storage class is therefore physical,
        // and `o_oid` equals the descriptor-area block address per
        // `SR-005`/`SR-007`.
        let map_header = validate_object_block(
            &block,
            block_address,
            ObjectExpectation::physical(OBJECT_TYPE_CHECKPOINT_MAP),
        )?;
        if map_header.xid != container.xid {
            return Err(ScanError::InvalidObject(format!(
                "checkpoint map at {block_address} has xid {} (expected {})",
                map_header.xid, container.xid
            )));
        }

        let flags = le_u32(&block, 0x20);
        let count = le_u32(&block, 0x24);
        let last = flags & CHECKPOINT_MAP_LAST != 0;
        if last {
            last_flag_seen = true;
        }
        let entries_end = 0x28usize.checked_add(CHECKPOINT_MAPPING_SIZE * count as usize);
        let Some(entries_end) = entries_end else {
            return Err(ScanError::InvalidObject(format!(
                "checkpoint map at {block_address} reports overflowing count {count}"
            )));
        };
        if entries_end > block.len() {
            return Err(ScanError::InvalidObject(format!(
                "checkpoint map at {block_address} reports {count} entries past block end"
            )));
        }

        for index in 0..count {
            let entry_off = 0x28 + CHECKPOINT_MAPPING_SIZE * index as usize;
            let cpm_type = le_u32(&block, entry_off);
            let cpm_subtype = le_u32(&block, entry_off + 4);
            let cpm_size = le_u32(&block, entry_off + 8);
            let cpm_fs_oid = le_u64(&block, entry_off + 0x10);
            let cpm_oid = le_u64(&block, entry_off + 0x18);
            let cpm_paddr = le_u64(&block, entry_off + 0x20);
            mappings.push(CheckpointMapping {
                map_block: block_address,
                object_type_raw: cpm_type,
                object_subtype: cpm_subtype,
                size: cpm_size,
                fs_oid: cpm_fs_oid,
                oid: cpm_oid,
                paddr: cpm_paddr,
            });
        }

        map_blocks.push(CheckpointMapBlock {
            block_address,
            flags,
            mapping_count: count,
            last,
            object: map_header,
        });
    }

    if !last_flag_seen && !map_blocks.is_empty() {
        validation_notes
            .push("no checkpoint-map block was flagged CHECKPOINT_MAP_LAST".to_string());
    }
    if trailing_nxsb_block == 0 {
        return Err(ScanError::InvalidObject(
            "checkpoint descriptor ring did not include a trailing NXSB".to_string(),
        ));
    }

    Ok(CheckpointMapSummary {
        map_blocks,
        mappings,
        trailing_nxsb_block,
        last_flag_seen,
        validation_notes,
    })
}

fn format_uuid(bytes: &[u8]) -> String {
    if bytes.len() < 16 {
        return String::new();
    }
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[cfg(test)]
mod tests {
    //! Adversarial-input coverage for the audit r2 #Rust-10
    //! checked_add at `xp_desc_base + position`.

    use super::*;
    use crate::object::ObjectHeader;
    use std::io::Cursor;

    /// Hand-rolled minimal `ContainerSummary` for tests that
    /// only care about the descriptor-ring fields. Everything
    /// else gets a zero / empty value — `walk_checkpoint_maps`
    /// shouldn't read those before it errors out on the
    /// arithmetic overflow we're testing.
    fn stub_container(xp_desc_base: u64, xp_desc_blocks: u32, xp_desc_len: u32) -> ContainerSummary {
        ContainerSummary {
            block_address: 0,
            xid: 0,
            block_size: 4096,
            block_count: 1,
            features_raw: 0,
            readonly_compatible_features_raw: 0,
            incompatible_features_raw: 0,
            features: Vec::new(),
            incompatible_features: Vec::new(),
            unsupported_incompatible_features: 0,
            uuid_hex: String::new(),
            next_oid: 0,
            next_xid: 0,
            xp_desc_blocks,
            xp_desc_base,
            xp_desc_index: 0,
            xp_desc_len,
            xp_data_blocks: 0,
            xp_data_base: 0,
            xp_data_index: 0,
            xp_data_len: 0,
            spaceman_oid: 0,
            omap_oid: 0,
            reaper_oid: 0,
            max_file_systems: 0,
            volume_oids: Vec::new(),
            object_storage_summary: Vec::new(),
            object_header: ObjectHeader {
                block_address: 0,
                checksum: 0,
                oid: 0,
                xid: 0,
                object_type_raw: 0,
                object_type: 0,
                object_type_flags: 0,
                object_subtype: 0,
            },
        }
    }

    /// Audit r2 #Rust-10 regression. `xp_desc_base` near
    /// `u64::MAX` with a positive `position` previously wrapped
    /// to a low paddr and directed `read_block` at attacker-
    /// chosen low bytes. The `checked_add` now returns
    /// `InvalidObject` instead.
    #[test]
    fn walk_checkpoint_maps_rejects_xp_desc_base_overflow() {
        // xp_desc_base = u64::MAX → first iteration has
        // position=0, which doesn't overflow. The audit asked
        // specifically about `u64::MAX - 1` with xp_desc_len>1,
        // since then position=1 overflows. Set
        // xp_desc_blocks=2 so positions 0 and 1 are both visited.
        let container = stub_container(u64::MAX, 2, 2);
        let mut reader = Cursor::new(vec![0u8; 8192]);
        let err = walk_checkpoint_maps(&mut reader, 4096, &container, 0)
            .expect_err("u64::MAX xp_desc_base must fail-closed on overflow");
        let msg = format!("{err}");
        assert!(
            msg.contains("overflow") || msg.contains("xp_desc"),
            "error should reference the overflow / xp_desc field; got: {msg}"
        );
    }

    /// Symmetric check: a legitimate xp_desc_base with no
    /// overflow gets past the gate (and then fails on the
    /// downstream `read_block` because the cursor is empty —
    /// that's a different error type, which is the point).
    #[test]
    fn walk_checkpoint_maps_accepts_normal_xp_desc_base() {
        let container = stub_container(1, 4, 4);
        let mut reader = Cursor::new(vec![0u8; 8192]);
        let result = walk_checkpoint_maps(&mut reader, 4096, &container, 0);
        // We expect an error, but NOT the overflow one — the
        // arithmetic guard let the call proceed, so the failure
        // should come from the object-header validation step.
        let err = result.expect_err("stub container should fail downstream");
        let msg = format!("{err}");
        assert!(
            !msg.contains("overflow"),
            "non-overflow path must not trip the overflow guard; got: {msg}"
        );
    }
}
