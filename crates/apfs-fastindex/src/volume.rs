//! APFS volume superblock decoder.
//!
//! `SR-008`/`SR-010`/`SR-012` together say the v1 native parser must record
//! per-volume role and feature bits before claiming any volume is in scope.
//! This module decodes one volume superblock that the container OMAP resolved
//! and surfaces the fields the FS-tree dumper needs (`apfs_omap_oid`,
//! `apfs_root_tree_oid`) plus the metadata fields the source gate uses to
//! decide whether the volume is supported at all.

use serde::Serialize;

use crate::block_io::{le_u16, le_u32, le_u64};
use crate::object::{
    flags_summary, validate_object_block, ObjectExpectation, ObjectHeader, APFS_MAGIC,
    OBJECT_TYPE_FS,
};
use crate::ScanError;

const APFS_FS_UNENCRYPTED: u64 = 0x1;
const APFS_FS_ONEKEY: u64 = 0x8;
const APFS_FS_SPILLEDOVER: u64 = 0x10;
const APFS_FS_RUN_SPILLOVER_CLEANER: u64 = 0x20;
const APFS_FS_ALWAYS_CHECK_EXTENTREF: u64 = 0x40;

const APFS_INCOMPAT_CASE_INSENSITIVE: u64 = 0x1;
const APFS_INCOMPAT_DATALESS_SNAPS: u64 = 0x2;
const APFS_INCOMPAT_ENC_ROLLED: u64 = 0x4;
const APFS_INCOMPAT_NORMALIZATION_INSENSITIVE: u64 = 0x8;
const APFS_INCOMPAT_INCOMPLETE_RESTORE: u64 = 0x10;
const APFS_INCOMPAT_SEALED_VOLUME: u64 = 0x20;

const APFS_VOL_ROLE_NONE: u16 = 0x0000;
const APFS_VOL_ROLE_SYSTEM: u16 = 0x0001;
const APFS_VOL_ROLE_USER: u16 = 0x0002;
const APFS_VOL_ROLE_RECOVERY: u16 = 0x0004;
const APFS_VOL_ROLE_VM: u16 = 0x0008;
const APFS_VOL_ROLE_PREBOOT: u16 = 0x0010;
const APFS_VOL_ROLE_INSTALLER: u16 = 0x0020;
const APFS_VOL_ROLE_DATA: u16 = 0x0040;
const APFS_VOL_ROLE_BASEBAND: u16 = 0x0080;

const APFS_VOLNAME_OFFSET: usize = 0x2c0;
const APFS_VOLNAME_LEN: usize = 256;
const APFS_ROLE_OFFSET: usize = 0x3c4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeSummary {
    pub block_address: u64,
    pub virtual_oid: u64,
    pub xid: u64,
    pub fs_index: u32,
    pub features_raw: u64,
    pub readonly_compatible_features_raw: u64,
    pub incompatible_features_raw: u64,
    pub fs_flags_raw: u64,
    pub incompatible_features: Vec<&'static str>,
    pub fs_flags: Vec<&'static str>,
    pub unsupported_incompatible_features: u64,
    pub volume_name: String,
    pub volume_uuid_hex: String,
    pub role_raw: u16,
    pub role_names: Vec<&'static str>,
    pub root_tree_type_raw: u32,
    pub extentref_tree_type_raw: u32,
    pub snap_meta_tree_type_raw: u32,
    pub omap_oid: u64,
    pub root_tree_virtual_oid: u64,
    pub extentref_tree_oid: u64,
    pub snap_meta_tree_oid: u64,
    pub num_files: u64,
    pub num_directories: u64,
    pub num_symlinks: u64,
    pub num_other_fsobjects: u64,
    pub num_snapshots: u64,
    pub object_header: ObjectHeader,
    pub object_storage_summary: Vec<&'static str>,
    pub case_insensitive: bool,
    pub normalization_insensitive: bool,
    pub encrypted_runtime: bool,
    pub support_status: VolumeSupportStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeSupportStatus {
    /// Volume passes the v1 source-gate allowlist; we may walk its OMAP and
    /// FS-tree for diagnostic counting.
    Supported,
    /// Volume is decoded but cannot be walked end-to-end (e.g. encryption,
    /// unsupported incompatible feature). The caller must skip OMAP/FS-tree
    /// traversal and surface the reason.
    Unsupported(String),
}

pub(crate) fn decode_volume_summary(
    block: &[u8],
    block_address: u64,
    expected_oid: u64,
    max_xid: u64,
) -> Result<VolumeSummary, ScanError> {
    let header = validate_object_block(
        block,
        block_address,
        ObjectExpectation::virtual_object(OBJECT_TYPE_FS, Some(max_xid)),
    )?;
    if header.oid != expected_oid {
        return Err(ScanError::InvalidObject(format!(
            "volume superblock at {block_address} has o_oid={} (expected virtual oid {})",
            header.oid, expected_oid
        )));
    }
    if le_u32(block, 0x20) != APFS_MAGIC {
        return Err(ScanError::InvalidObject(format!(
            "volume superblock at {block_address} missing APSB magic"
        )));
    }

    let fs_index = le_u32(block, 0x24);
    let features_raw = le_u64(block, 0x28);
    let ro_compat_raw = le_u64(block, 0x30);
    let incompat_raw = le_u64(block, 0x38);
    let root_tree_type_raw = le_u32(block, 0x74);
    let extentref_tree_type_raw = le_u32(block, 0x78);
    let snap_meta_tree_type_raw = le_u32(block, 0x7c);
    let omap_oid = le_u64(block, 0x80);
    let root_tree_virtual_oid = le_u64(block, 0x88);
    let extentref_tree_oid = le_u64(block, 0x90);
    let snap_meta_tree_oid = le_u64(block, 0x98);
    let num_files = le_u64(block, 0xb8);
    let num_directories = le_u64(block, 0xc0);
    let num_symlinks = le_u64(block, 0xc8);
    let num_other_fsobjects = le_u64(block, 0xd0);
    let num_snapshots = le_u64(block, 0xd8);
    let volume_uuid_hex = format_uuid(&block[0xf0..0x100]);
    let fs_flags_raw = le_u64(block, 0x108);
    let role_raw = if block.len() > APFS_ROLE_OFFSET + 2 {
        le_u16(block, APFS_ROLE_OFFSET)
    } else {
        0
    };

    let volume_name = if block.len() >= APFS_VOLNAME_OFFSET + APFS_VOLNAME_LEN {
        decode_volname(&block[APFS_VOLNAME_OFFSET..APFS_VOLNAME_OFFSET + APFS_VOLNAME_LEN])
    } else {
        String::new()
    };

    let mut incompatible_features = Vec::new();
    if incompat_raw & APFS_INCOMPAT_CASE_INSENSITIVE != 0 {
        incompatible_features.push("case_insensitive");
    }
    if incompat_raw & APFS_INCOMPAT_DATALESS_SNAPS != 0 {
        incompatible_features.push("dataless_snapshots");
    }
    if incompat_raw & APFS_INCOMPAT_ENC_ROLLED != 0 {
        incompatible_features.push("enc_rolled");
    }
    if incompat_raw & APFS_INCOMPAT_NORMALIZATION_INSENSITIVE != 0 {
        incompatible_features.push("normalization_insensitive");
    }
    if incompat_raw & APFS_INCOMPAT_INCOMPLETE_RESTORE != 0 {
        incompatible_features.push("incomplete_restore");
    }
    if incompat_raw & APFS_INCOMPAT_SEALED_VOLUME != 0 {
        incompatible_features.push("sealed_volume");
    }

    let known_incompat_mask = APFS_INCOMPAT_CASE_INSENSITIVE
        | APFS_INCOMPAT_DATALESS_SNAPS
        | APFS_INCOMPAT_ENC_ROLLED
        | APFS_INCOMPAT_NORMALIZATION_INSENSITIVE
        | APFS_INCOMPAT_INCOMPLETE_RESTORE
        | APFS_INCOMPAT_SEALED_VOLUME;
    let unsupported_incompatible_features = incompat_raw & !known_incompat_mask;

    let mut fs_flags = Vec::new();
    if fs_flags_raw & APFS_FS_UNENCRYPTED != 0 {
        fs_flags.push("unencrypted");
    }
    if fs_flags_raw & APFS_FS_ONEKEY != 0 {
        fs_flags.push("onekey");
    }
    if fs_flags_raw & APFS_FS_SPILLEDOVER != 0 {
        fs_flags.push("spilledover");
    }
    if fs_flags_raw & APFS_FS_RUN_SPILLOVER_CLEANER != 0 {
        fs_flags.push("run_spillover_cleaner");
    }
    if fs_flags_raw & APFS_FS_ALWAYS_CHECK_EXTENTREF != 0 {
        fs_flags.push("always_check_extentref");
    }

    let role_names = decode_role(role_raw);
    let case_insensitive = incompat_raw & APFS_INCOMPAT_CASE_INSENSITIVE != 0;
    let normalization_insensitive = incompat_raw & APFS_INCOMPAT_NORMALIZATION_INSENSITIVE != 0;
    let encrypted_runtime = fs_flags_raw & APFS_FS_UNENCRYPTED == 0;

    let mut support_status = VolumeSupportStatus::Supported;
    if encrypted_runtime {
        support_status =
            VolumeSupportStatus::Unsupported("volume is encrypted at runtime".to_string());
    } else if unsupported_incompatible_features != 0 {
        support_status = VolumeSupportStatus::Unsupported(format!(
            "volume sets unknown incompatible features {unsupported_incompatible_features:#x}"
        ));
    } else if incompat_raw & APFS_INCOMPAT_ENC_ROLLED != 0 {
        support_status =
            VolumeSupportStatus::Unsupported("volume in encryption-rolling state".to_string());
    } else if incompat_raw & APFS_INCOMPAT_DATALESS_SNAPS != 0 {
        support_status = VolumeSupportStatus::Unsupported(
            "volume uses dataless snapshots, namespace semantics undefined for v1".to_string(),
        );
    } else if incompat_raw & APFS_INCOMPAT_INCOMPLETE_RESTORE != 0 {
        support_status = VolumeSupportStatus::Unsupported(
            "volume restore is incomplete, walk would mix old and new state".to_string(),
        );
    } else if incompat_raw & APFS_INCOMPAT_SEALED_VOLUME != 0 {
        support_status = VolumeSupportStatus::Unsupported(
            "sealed system volumes are out of v1 raw scope".to_string(),
        );
    }

    Ok(VolumeSummary {
        block_address,
        virtual_oid: header.oid,
        xid: header.xid,
        fs_index,
        features_raw,
        readonly_compatible_features_raw: ro_compat_raw,
        incompatible_features_raw: incompat_raw,
        fs_flags_raw,
        incompatible_features,
        fs_flags,
        unsupported_incompatible_features,
        volume_name,
        volume_uuid_hex,
        role_raw,
        role_names,
        root_tree_type_raw,
        extentref_tree_type_raw,
        snap_meta_tree_type_raw,
        omap_oid,
        root_tree_virtual_oid,
        extentref_tree_oid,
        snap_meta_tree_oid,
        num_files,
        num_directories,
        num_symlinks,
        num_other_fsobjects,
        num_snapshots,
        object_storage_summary: flags_summary(&header),
        object_header: header,
        case_insensitive,
        normalization_insensitive,
        encrypted_runtime,
        support_status,
    })
}

fn decode_volname(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn decode_role(role: u16) -> Vec<&'static str> {
    if role == APFS_VOL_ROLE_NONE {
        return vec!["none"];
    }
    let mut roles = Vec::new();
    if role & APFS_VOL_ROLE_SYSTEM != 0 {
        roles.push("system");
    }
    if role & APFS_VOL_ROLE_USER != 0 {
        roles.push("user");
    }
    if role & APFS_VOL_ROLE_RECOVERY != 0 {
        roles.push("recovery");
    }
    if role & APFS_VOL_ROLE_VM != 0 {
        roles.push("vm");
    }
    if role & APFS_VOL_ROLE_PREBOOT != 0 {
        roles.push("preboot");
    }
    if role & APFS_VOL_ROLE_INSTALLER != 0 {
        roles.push("installer");
    }
    if role & APFS_VOL_ROLE_DATA != 0 {
        roles.push("data");
    }
    if role & APFS_VOL_ROLE_BASEBAND != 0 {
        roles.push("baseband");
    }
    if roles.is_empty() {
        roles.push("unknown");
    }
    roles
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
