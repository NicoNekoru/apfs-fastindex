use std::fmt;
use std::fs::File;
use std::io;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::Command;

use plist::Value;
use serde::Serialize;

mod block_io;
mod btree;
mod container;
pub mod fallback;
mod fallback_bulk;
mod fs_record_body;
mod fs_records;
mod namespace;
mod object;
mod omap;
mod volume;
/// C ABI surface for the native (Swift) renderer. See
/// `docs/implementation/viz-perf-study.md` for the architecture
/// sketch. The `#[no_mangle] extern "C"` symbols defined here are
/// always exported by the cdylib regardless of whether the rlib
/// path is used; cost is zero when no caller references them.
pub mod ffi;

use block_io::{checksum_matches, le_u32, le_u64, open_block_source, read_block};
use object::{
    validate_object_block, ExpectedStorage, ObjectExpectation, OBJECT_TYPE_FSTREE,
    OBJECT_TYPE_NX_SUPERBLOCK,
};

pub use container::{
    CheckpointMapBlock, CheckpointMapSummary, CheckpointMapping, ContainerSummary,
};
pub use fallback::{
    fallback_scan_path, fallback_scan_path_with_options, FallbackError, FallbackOptions,
    FallbackScanOutput, ProgressEvent,
};
pub use fs_record_body::{
    DirRecBody, DstreamFields, FsRecordKey, FsRecordRow, FsRecordValue, InodeBody, SiblingLinkBody,
    XattrBody, XfieldEntry, XfieldInterpreted,
};
pub use fs_records::{FamilyCount, FsRecordDump};
pub use object::ObjectHeader;
pub use omap::{OmapDumpEntry, OmapPhysSummary, OmapSummary, OmapValue};
pub use volume::{VolumeSummary, VolumeSupportStatus};

const APFS_CONTAINER_HINT: &str = "EF57347C-0000-11AA-AA11-00306543ECAC";
const NX_MAGIC: u32 = 0x4253_584e;
const NX_SUPERBLOCK_OID: u64 = 1;
const OBJECT_TYPE_MASK: u32 = 0x0000_ffff;
const MIN_BLOCK_SIZE: u32 = 4096;
const MAX_BLOCK_SIZE: u32 = 64 * 1024;
const MAX_DESCRIPTOR_BLOCKS: u32 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceDescriptor {
    pub requested_path: PathBuf,
    pub raw_container_path: String,
    pub source_kind: String,
    pub allowlist_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanState {
    pub block_size: u32,
    pub descriptor_blocks: u32,
    pub descriptor_base: u64,
    pub descriptor_base_non_contiguous: bool,
    pub highest_xid: u64,
    pub candidate_count: usize,
    pub validation_gaps: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamespaceEntry {
    pub path: String,
    pub entry_kind: EntryKind,
    pub file_id: u64,
    pub logical_size: u64,
    pub symlink_target: Option<String>,
    /// Per-inode allocated bytes under SR-019 + EX-22 precedence:
    ///
    /// - regular + dstream + no `INO_EXT_TYPE_SPARSE_BYTES` xfield →
    ///   `Some(j_dstream_t.alloced_size)`
    /// - symlink, directory → `Some(0)`
    /// - regular + dstream + `INO_EXT_TYPE_SPARSE_BYTES` present →
    ///   `None` (sparse divergence; see EX-22)
    /// - regular + `com.apple.decmpfs` xattr → `None`
    /// - any other case → `None`
    ///
    /// The fallback backend's truth is the kernel's stat output, so it
    /// emits `Some(st_blocks * 512)` for regular files (the public
    /// oracle directly) and `Some(0)` for symlinks and directories so
    /// the shape parity with raw mode holds.
    #[serde(default)]
    pub allocated_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectoryAggregate {
    pub path: String,
    pub unique_inode_logical_total: u64,
    pub contributing_file_ids: Vec<u64>,
    /// Per-directory unique-inode allocated-bytes total. `None` if any
    /// contributing file inode has `allocated_size == None`; a partial
    /// total cannot be authoritative.
    #[serde(default)]
    pub unique_inode_allocated_total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParserOutput {
    pub source: SourceDescriptor,
    pub scan_state: ScanState,
    pub backend_name: String,
    pub entries: Vec<NamespaceEntry>,
    pub aggregates: Vec<DirectoryAggregate>,
    /// Per-path skip notes recorded while walking. Empty for raw mode
    /// (which fails closed on every malformed-source signal). The
    /// fallback walker populates this with `permission_denied`,
    /// `not_found` (raced between readdir and lstat),
    /// `mount_boundary` (cross-device child without `--cross-mounts`),
    /// and `non_utf8_name` notes so a user-facing tool can report how
    /// much of the requested subtree it actually saw.
    #[serde(default)]
    pub walk_skips: Vec<WalkSkip>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WalkSkip {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OracleMismatch {
    pub path: String,
    pub expected: serde_json::Value,
    pub actual: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OracleDiff {
    pub matched: bool,
    pub missing_paths: Vec<String>,
    pub unexpected_paths: Vec<String>,
    pub mismatches: Vec<OracleMismatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointScanOutput {
    pub parser_output: ParserOutput,
    pub checkpoint_candidates: Vec<NxsbCandidate>,
    pub skipped_descriptors: Vec<SkippedDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_checkpoint: Option<SelectedCheckpoint>,
    pub correctness_claim: String,
    pub not_claimed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectedCheckpoint {
    pub block_address: u64,
    pub xid: u64,
    pub container: ContainerSummary,
    pub checkpoint_map: CheckpointMapSummary,
    pub container_omap: OmapSummary,
    pub volumes: Vec<VolumeReport>,
    pub native_validation: NativeValidationReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeReport {
    pub fs_oid_index: u32,
    pub volume_oid: u64,
    pub container_omap_lookup: OmapValue,
    pub summary: VolumeSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_omap: Option<OmapSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_tree_lookup: Option<OmapValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs_record_dump: Option<FsRecordDump>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeValidationReport {
    pub checkpoint_map_validated: bool,
    pub container_omap_loaded: bool,
    pub container_feature_allowlist_ok: bool,
    pub volume_count: u32,
    pub volume_supported_count: u32,
    pub fs_records_dumped_count: u32,
    pub validation_gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub block_size: u32,
    pub descriptor_blocks: u32,
    pub descriptor_base: u64,
    pub descriptor_base_non_contiguous: bool,
    pub candidates: Vec<NxsbCandidate>,
    pub skipped_descriptors: Vec<SkippedDescriptor>,
}

impl ScanReport {
    pub fn highest_candidate(&self) -> Option<&NxsbCandidate> {
        self.candidates.iter().max_by_key(|candidate| candidate.xid)
    }

    pub(crate) fn to_scan_state(&self, gaps: Vec<String>) -> ScanState {
        ScanState {
            block_size: self.block_size,
            descriptor_blocks: self.descriptor_blocks,
            descriptor_base: self.descriptor_base,
            descriptor_base_non_contiguous: self.descriptor_base_non_contiguous,
            highest_xid: self
                .highest_candidate()
                .map(|candidate| candidate.xid)
                .unwrap_or_default(),
            candidate_count: self.candidates.len(),
            validation_gaps: gaps,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NxsbCandidate {
    pub descriptor_index: u32,
    pub block_address: u64,
    pub oid: u64,
    pub xid: u64,
    pub object_type_raw: u32,
    pub object_subtype: u32,
    pub checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedDescriptor {
    pub descriptor_index: u32,
    pub block_address: u64,
    pub reason: String,
}

#[derive(Debug)]
pub enum ScanError {
    Io(io::Error),
    ShortRead {
        block_address: u64,
        expected: usize,
        actual: usize,
    },
    InvalidBlockZero(String),
    UnsupportedDescriptorLayout(String),
    NoCheckpointCandidates,
    InvalidObject(String),
    UnsupportedContainerFeature(String),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::ShortRead {
                block_address,
                expected,
                actual,
            } => write!(
                f,
                "short read at block {block_address}: expected {expected} bytes, read {actual}"
            ),
            Self::InvalidBlockZero(reason) => write!(f, "invalid APFS block zero: {reason}"),
            Self::UnsupportedDescriptorLayout(reason) => {
                write!(f, "unsupported checkpoint descriptor layout: {reason}")
            }
            Self::NoCheckpointCandidates => write!(f, "no valid checkpoint NXSB candidates found"),
            Self::InvalidObject(reason) => write!(f, "APFS object validation failed: {reason}"),
            Self::UnsupportedContainerFeature(reason) => {
                write!(f, "unsupported container feature: {reason}")
            }
        }
    }
}

impl std::error::Error for ScanError {}

impl From<io::Error> for ScanError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
pub enum SourceGateError {
    Io(io::Error),
    Plist(plist::Error),
    CommandFailed {
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    UnsupportedSource(String),
    MissingApfsContainer,
}

impl fmt::Display for SourceGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Plist(err) => write!(f, "{err}"),
            Self::CommandFailed {
                command,
                status,
                stderr,
            } => write!(
                f,
                "command failed (status {:?}): {command}\nstderr:\n{stderr}",
                status
            ),
            Self::UnsupportedSource(reason) => write!(f, "unsupported source: {reason}"),
            Self::MissingApfsContainer => {
                write!(f, "image does not expose a simple APFS container")
            }
        }
    }
}

impl std::error::Error for SourceGateError {}

impl From<io::Error> for SourceGateError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<plist::Error> for SourceGateError {
    fn from(value: plist::Error) -> Self {
        Self::Plist(value)
    }
}

#[derive(Debug)]
pub enum ParserError {
    Source(SourceGateError),
    Scan(ScanError),
}

impl fmt::Display for ParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(err) => write!(f, "{err}"),
            Self::Scan(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ParserError {}

impl From<SourceGateError> for ParserError {
    fn from(value: SourceGateError) -> Self {
        Self::Source(value)
    }
}

impl From<ScanError> for ParserError {
    fn from(value: ScanError) -> Self {
        Self::Scan(value)
    }
}

#[derive(Debug)]
pub struct ValidatedSource {
    pub descriptor: SourceDescriptor,
    detach_device: Option<String>,
}

impl Drop for ValidatedSource {
    fn drop(&mut self) {
        if let Some(device) = &self.detach_device {
            let _ = Command::new("hdiutil").args(["detach", device]).output();
        }
    }
}

pub fn open_validated_source<P: AsRef<Path>>(
    source_path: P,
) -> Result<ValidatedSource, SourceGateError> {
    let requested_path = source_path.as_ref().to_path_buf();
    let requested_str = requested_path.to_string_lossy();

    if requested_str.starts_with("/dev/") {
        let raw_container_path = normalize_raw_device(&requested_str)?;
        if !Path::new(&raw_container_path).exists() {
            return Err(SourceGateError::UnsupportedSource(format!(
                "raw device does not exist: {raw_container_path}"
            )));
        }
        return Ok(ValidatedSource {
            descriptor: SourceDescriptor {
                requested_path,
                raw_container_path,
                source_kind: "raw_device".to_string(),
                allowlist_reason: "caller-supplied raw APFS container device".to_string(),
            },
            detach_device: None,
        });
    }

    if !requested_path.exists() {
        return Err(SourceGateError::UnsupportedSource(format!(
            "source path does not exist: {}",
            requested_path.display()
        )));
    }

    if requested_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| !extension.eq_ignore_ascii_case("dmg"))
        .unwrap_or(true)
    {
        return Err(SourceGateError::UnsupportedSource(
            "only detached APFS .dmg images or raw APFS container devices are in the current allowlist"
                .to_string(),
        ));
    }

    attach_dmg_source(requested_path)
}

pub fn checkpoint_scan_source<P: AsRef<Path>>(
    source_path: P,
) -> Result<CheckpointScanOutput, ParserError> {
    let source = open_validated_source(source_path)?;
    let mut reader =
        open_block_source(&source.descriptor.raw_container_path).map_err(ScanError::from)?;
    let report = scan_reader(&mut reader)?;
    let highest = report
        .highest_candidate()
        .ok_or(ScanError::NoCheckpointCandidates)?
        .clone();

    let (selected, mut validation_gaps) = match attempt_native_dump(&mut reader, &report, &highest)
    {
        Ok((selected, gaps)) => (Some(selected), gaps),
        Err(error) => (None, vec![format!("native dump aborted: {error}")]),
    };

    if selected.is_none() {
        validation_gaps.extend([
            "checkpoint map validation not completed".to_string(),
            "container OMAP resolution not completed".to_string(),
            "volume superblock decoding not completed".to_string(),
            "FS-tree record dumping not completed".to_string(),
        ]);
    }

    // Build the v1 namespace + per-directory aggregate output from the
    // first supported volume's FS-record dump. EX-18 / EX-19 / EX-20 have
    // validated the body decoder, SR-017 logical-size precedence, and
    // SR-018 stored-name preservation on the proof fixture; gating
    // entry emission on `selected_checkpoint` + the volume's
    // `fs_record_dump` keeps the emission off when any earlier
    // fail-closed gate trips.
    let (entries, aggregates) = match selected
        .as_ref()
        .and_then(|sel| sel.volumes.first())
        .and_then(|vol| vol.fs_record_dump.as_ref())
    {
        Some(dump) => namespace::build_namespace(dump),
        None => (Vec::new(), Vec::new()),
    };
    let namespace_emitted = !entries.is_empty();

    let parser_output = ParserOutput {
        source: source.descriptor.clone(),
        scan_state: report.to_scan_state(validation_gaps.clone()),
        backend_name: "rust-checkpoint-scan".to_string(),
        entries,
        aggregates,
        walk_skips: Vec::new(),
    };

    let correctness_claim = if namespace_emitted {
        "Rust path emits one APFS volume's NamespaceEntry + DirectoryAggregate rows under SR-017 logical-size precedence, SR-018 stored-name preservation, and SR-019+EX-22 allocated-size precedence (regular+dstream+no-sparse-bytes -> alloced_size; sparse / decmpfs -> fail closed; symlink/dir -> 0), gated on EX-18 body-field parity, EX-19 size precedence, EX-20 name preservation, and EX-22 case-class verdict"
            .to_string()
    } else if selected.is_some() {
        "Rust path validates the source gate, candidate scan, container superblock, checkpoint map, container OMAP, decoded volume superblocks, and a read-only FS-tree record-body dump (no namespace rows)"
            .to_string()
    } else {
        "Rust path validates the source gate and preliminary checkpoint descriptor scan only"
            .to_string()
    };

    let mut not_claimed = vec![
        "live mounted raw-scan correctness".to_string(),
        "per-file allocated_size for sparse regular files (INO_EXT_TYPE_SPARSE_BYTES present; EX-22 saw alloced_size overstate st_blocks*512 by exactly sparse_bytes)".to_string(),
        "per-file allocated_size for decmpfs-compressed regular files (no oracle-validated rule yet)".to_string(),
        "exclusive / shared / snapshot-retained byte accounting".to_string(),
        "incremental cache reuse".to_string(),
        "encryption decryption or keybag handling".to_string(),
        "snapshot, sealed-volume, or volume-group merged semantics".to_string(),
        "APFS lookup-by-name (hash + normalization + case fold)".to_string(),
        "boot-root or Finder-visible merged namespace".to_string(),
    ];
    if !namespace_emitted {
        not_claimed.insert(
            0,
            "namespace entry emission and oracle-validated logical-size / allocated-size output from Rust"
                .to_string(),
        );
    }

    Ok(CheckpointScanOutput {
        parser_output,
        checkpoint_candidates: report.candidates,
        skipped_descriptors: report.skipped_descriptors,
        selected_checkpoint: selected,
        correctness_claim,
        not_claimed,
    })
}

pub fn scan_source<P: AsRef<Path>>(source_path: P) -> Result<ParserOutput, ParserError> {
    checkpoint_scan_source(source_path).map(|output| output.parser_output)
}

fn attempt_native_dump<R: Read + Seek>(
    reader: &mut R,
    report: &ScanReport,
    highest: &NxsbCandidate,
) -> Result<(SelectedCheckpoint, Vec<String>), ScanError> {
    let block_size = report.block_size;
    let block_size_usize = block_size as usize;

    // Re-read selected NXSB and decode it. Unlike the raw scan loop the
    // decoder validates checksum, type, OID, and (because virtual NXSB do
    // exist in `nx_xp_desc_data`) accepts any storage class that the spec
    // allows for the container superblock.
    let block = read_block(reader, highest.block_address, block_size_usize)?;
    let container = container::decode_container_summary(&block, highest.block_address)?;

    if container.unsupported_incompatible_features != 0 {
        return Err(ScanError::UnsupportedContainerFeature(format!(
            "container sets unknown incompatible features {:#x}",
            container.unsupported_incompatible_features
        )));
    }
    if container.block_size != block_size {
        return Err(ScanError::InvalidObject(format!(
            "selected NXSB block_size {} does not match block-zero block_size {}",
            container.block_size, block_size
        )));
    }
    if container.xp_desc_base != report.descriptor_base {
        return Err(ScanError::InvalidObject(format!(
            "selected NXSB descriptor base {} does not match block-zero base {}",
            container.xp_desc_base, report.descriptor_base
        )));
    }

    let checkpoint_map =
        container::walk_checkpoint_maps(reader, block_size, &container, highest.block_address)?;
    let mut validation_gaps = Vec::new();
    if !checkpoint_map.last_flag_seen && !checkpoint_map.map_blocks.is_empty() {
        validation_gaps.push(
            "no checkpoint-map block carried CHECKPOINT_MAP_LAST; relying on length only"
                .to_string(),
        );
    }

    let container_omap_resolver =
        omap::OmapResolver::open(reader, block_size_usize, container.omap_oid)?;
    let container_omap_summary =
        container_omap_resolver.summarize(reader, block_size_usize, container.xid, 8)?;

    let mut volume_reports = Vec::new();
    let mut fs_records_dumped = 0u32;
    let mut volume_supported_count = 0u32;
    for (index, volume_oid) in container.volume_oids.iter().enumerate() {
        let lookup =
            container_omap_resolver.lookup(reader, block_size_usize, *volume_oid, container.xid)?;
        let Some(volume_value) = lookup else {
            volume_reports.push(VolumeReport {
                fs_oid_index: index as u32,
                volume_oid: *volume_oid,
                container_omap_lookup: OmapValue {
                    oid: *volume_oid,
                    xid: 0,
                    paddr: 0,
                    flags: 0,
                    size: 0,
                },
                summary: empty_volume_summary(*volume_oid),
                volume_omap: None,
                root_tree_lookup: None,
                fs_record_dump: None,
                status: "missing_in_container_omap".to_string(),
                status_reason: Some(
                    "container OMAP did not return a mapping at the selected scan XID".to_string(),
                ),
            });
            continue;
        };

        let volume_block = read_block(reader, volume_value.paddr, block_size_usize)?;
        let summary = match volume::decode_volume_summary(
            &volume_block,
            volume_value.paddr,
            *volume_oid,
            container.xid,
        ) {
            Ok(summary) => summary,
            Err(err) => {
                volume_reports.push(VolumeReport {
                    fs_oid_index: index as u32,
                    volume_oid: *volume_oid,
                    container_omap_lookup: volume_value,
                    summary: empty_volume_summary(*volume_oid),
                    volume_omap: None,
                    root_tree_lookup: None,
                    fs_record_dump: None,
                    status: "volume_decode_failed".to_string(),
                    status_reason: Some(err.to_string()),
                });
                continue;
            }
        };

        if matches!(summary.support_status, VolumeSupportStatus::Unsupported(_)) {
            let reason = match &summary.support_status {
                VolumeSupportStatus::Unsupported(reason) => reason.clone(),
                _ => "unsupported".to_string(),
            };
            volume_reports.push(VolumeReport {
                fs_oid_index: index as u32,
                volume_oid: *volume_oid,
                container_omap_lookup: volume_value,
                summary,
                volume_omap: None,
                root_tree_lookup: None,
                fs_record_dump: None,
                status: "volume_unsupported".to_string(),
                status_reason: Some(reason),
            });
            continue;
        }

        volume_supported_count += 1;
        let volume_omap_resolver =
            omap::OmapResolver::open(reader, block_size_usize, summary.omap_oid)?;
        let volume_omap_summary =
            volume_omap_resolver.summarize(reader, block_size_usize, container.xid, 8)?;
        let root_tree_lookup = volume_omap_resolver.lookup(
            reader,
            block_size_usize,
            summary.root_tree_virtual_oid,
            container.xid,
        )?;

        let fs_record_dump = match &root_tree_lookup {
            Some(root_value) => {
                match validate_fs_root_header(reader, block_size_usize, root_value, container.xid) {
                    Ok(()) => Some(fs_records::dump_fs_records(
                        reader,
                        block_size_usize,
                        index as u32,
                        *volume_oid,
                        root_value.paddr,
                        container.xid,
                        &volume_omap_resolver,
                    )?),
                    Err(err) => {
                        validation_gaps.push(format!(
                            "FS-tree root for volume oid {volume_oid} did not validate: {err}"
                        ));
                        None
                    }
                }
            }
            None => None,
        };
        if fs_record_dump.is_some() {
            fs_records_dumped += 1;
        }

        volume_reports.push(VolumeReport {
            fs_oid_index: index as u32,
            volume_oid: *volume_oid,
            container_omap_lookup: volume_value,
            summary,
            volume_omap: Some(volume_omap_summary),
            root_tree_lookup,
            fs_record_dump,
            status: "supported".to_string(),
            status_reason: None,
        });
    }

    if volume_reports.is_empty() {
        validation_gaps.push("container exposed zero volume superblock OIDs".to_string());
    }

    let selected = SelectedCheckpoint {
        block_address: highest.block_address,
        xid: highest.xid,
        container,
        checkpoint_map,
        container_omap: container_omap_summary,
        volumes: volume_reports.clone(),
        native_validation: NativeValidationReport {
            checkpoint_map_validated: true,
            container_omap_loaded: true,
            container_feature_allowlist_ok: true,
            volume_count: volume_reports.len() as u32,
            volume_supported_count,
            fs_records_dumped_count: fs_records_dumped,
            validation_gaps: validation_gaps.clone(),
        },
    };
    Ok((selected, validation_gaps))
}

fn validate_fs_root_header<R: Read + Seek>(
    reader: &mut R,
    block_size: usize,
    root_value: &OmapValue,
    max_xid: u64,
) -> Result<(), ScanError> {
    let block = read_block(reader, root_value.paddr, block_size)?;
    // The FS-tree root is reached through the volume OMAP, so the on-disk
    // block carries a *virtual* object header (storage flags 0x0000_0000).
    // We validate checksum, type, max-XID, and subtype, but we cannot
    // require `o_oid == paddr` here because virtual OIDs are independent
    // of the physical block address.
    let header = validate_object_block(
        &block,
        root_value.paddr,
        ObjectExpectation {
            object_type: object::OBJECT_TYPE_BTREE,
            storage: ExpectedStorage::Virtual,
            max_xid: Some(max_xid),
            require_oid_eq_paddr: false,
        },
    )?;
    if header.oid != root_value.oid {
        return Err(ScanError::InvalidObject(format!(
            "FS-tree root at {} carries virtual oid {:#x}, expected {:#x} from volume OMAP",
            root_value.paddr, header.oid, root_value.oid
        )));
    }
    if header.object_subtype != OBJECT_TYPE_FSTREE {
        return Err(ScanError::InvalidObject(format!(
            "FS-tree root at {} has subtype {:#x}, expected fstree ({:#x})",
            root_value.paddr, header.object_subtype, OBJECT_TYPE_FSTREE
        )));
    }
    Ok(())
}

fn empty_volume_summary(volume_oid: u64) -> VolumeSummary {
    VolumeSummary {
        block_address: 0,
        virtual_oid: volume_oid,
        xid: 0,
        fs_index: 0,
        features_raw: 0,
        readonly_compatible_features_raw: 0,
        incompatible_features_raw: 0,
        fs_flags_raw: 0,
        incompatible_features: Vec::new(),
        fs_flags: Vec::new(),
        unsupported_incompatible_features: 0,
        volume_name: String::new(),
        volume_uuid_hex: String::new(),
        role_raw: 0,
        role_names: Vec::new(),
        root_tree_type_raw: 0,
        extentref_tree_type_raw: 0,
        snap_meta_tree_type_raw: 0,
        omap_oid: 0,
        root_tree_virtual_oid: 0,
        extentref_tree_oid: 0,
        snap_meta_tree_oid: 0,
        num_files: 0,
        num_directories: 0,
        num_symlinks: 0,
        num_other_fsobjects: 0,
        num_snapshots: 0,
        object_header: ObjectHeader {
            block_address: 0,
            checksum: 0,
            oid: volume_oid,
            xid: 0,
            object_type_raw: 0,
            object_type: 0,
            object_type_flags: 0,
            object_subtype: 0,
        },
        object_storage_summary: Vec::new(),
        case_insensitive: false,
        normalization_insensitive: false,
        encrypted_runtime: false,
        support_status: VolumeSupportStatus::Unsupported("not decoded".to_string()),
    }
}

fn normalize_raw_device(device: &str) -> Result<String, SourceGateError> {
    if device.starts_with("/dev/rdisk") {
        return Ok(device.to_string());
    }
    if let Some(suffix) = device.strip_prefix("/dev/disk") {
        return Ok(format!("/dev/rdisk{suffix}"));
    }
    Err(SourceGateError::UnsupportedSource(format!(
        "unsupported raw device path: {device}"
    )))
}

fn attach_dmg_source(requested_path: PathBuf) -> Result<ValidatedSource, SourceGateError> {
    let output = Command::new("hdiutil")
        .args(["attach", "-plist", "-nomount"])
        .arg(&requested_path)
        .output()?;
    if !output.status.success() {
        return Err(SourceGateError::CommandFailed {
            command: format!(
                "hdiutil attach -plist -nomount {}",
                requested_path.display()
            ),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let attach_info = Value::from_reader_xml(output.stdout.as_slice())?;
    let entities = attach_info
        .as_dictionary()
        .and_then(|dict| dict.get("system-entities"))
        .and_then(Value::as_array)
        .ok_or(SourceGateError::MissingApfsContainer)?;

    let detach_device = entities
        .iter()
        .find_map(|entity| entity_string(entity, "dev-entry"))
        .ok_or(SourceGateError::MissingApfsContainer)?;
    let container_device = entities
        .iter()
        .find(|entity| {
            entity_string(entity, "content-hint").as_deref() == Some(APFS_CONTAINER_HINT)
        })
        .and_then(|entity| entity_string(entity, "dev-entry"))
        .ok_or(SourceGateError::MissingApfsContainer)?;

    Ok(ValidatedSource {
        descriptor: SourceDescriptor {
            requested_path,
            raw_container_path: normalize_raw_device(&container_device)?,
            source_kind: "dmg_image".to_string(),
            allowlist_reason: "detached image-backed APFS container".to_string(),
        },
        detach_device: Some(detach_device),
    })
}

fn entity_string(entity: &Value, key: &str) -> Option<String> {
    entity
        .as_dictionary()
        .and_then(|dict| dict.get(key))
        .and_then(Value::as_string)
        .map(ToString::to_string)
}

pub fn scan_path(path: &str) -> Result<ScanReport, ScanError> {
    let mut file = File::open(path)?;
    scan_reader(&mut file)
}

pub fn scan_reader<R: Read + Seek>(reader: &mut R) -> Result<ScanReport, ScanError> {
    let initial = read_block(reader, 0, MIN_BLOCK_SIZE as usize)?;
    let block_size = le_u32(&initial, 0x24);
    validate_block_size(block_size)?;

    let block0 = if block_size == MIN_BLOCK_SIZE {
        initial
    } else {
        read_block(reader, 0, block_size as usize)?
    };

    validate_nxsb_block0(&block0)?;

    let descriptor_blocks = le_u32(&block0, 0x68);
    if descriptor_blocks == 0 {
        return Err(ScanError::UnsupportedDescriptorLayout(
            "descriptor block count is zero".to_string(),
        ));
    }
    if descriptor_blocks > MAX_DESCRIPTOR_BLOCKS {
        return Err(ScanError::UnsupportedDescriptorLayout(format!(
            "descriptor block count {descriptor_blocks} exceeds fail-closed limit {MAX_DESCRIPTOR_BLOCKS}"
        )));
    }

    let descriptor_base_raw = le_u64(&block0, 0x70);
    let descriptor_base_non_contiguous = (descriptor_base_raw >> 63) != 0;
    let descriptor_base = descriptor_base_raw & !(1u64 << 63);
    if descriptor_base_non_contiguous {
        return Err(ScanError::UnsupportedDescriptorLayout(
            "non-contiguous checkpoint descriptor areas require checkpoint mapping-tree support"
                .to_string(),
        ));
    }

    let mut candidates = Vec::new();
    let mut skipped_descriptors = Vec::new();
    for descriptor_index in 0..descriptor_blocks {
        let block_address = descriptor_base + u64::from(descriptor_index);
        let block = read_block(reader, block_address, block_size as usize)?;
        match parse_nxsb_candidate(&block, descriptor_index, block_address) {
            Ok(Some(candidate)) => candidates.push(candidate),
            Ok(None) => {}
            Err(reason) => skipped_descriptors.push(SkippedDescriptor {
                descriptor_index,
                block_address,
                reason,
            }),
        }
    }

    if candidates.is_empty() {
        return Err(ScanError::NoCheckpointCandidates);
    }

    Ok(ScanReport {
        block_size,
        descriptor_blocks,
        descriptor_base,
        descriptor_base_non_contiguous,
        candidates,
        skipped_descriptors,
    })
}

fn validate_block_size(block_size: u32) -> Result<(), ScanError> {
    if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) {
        return Err(ScanError::InvalidBlockZero(format!(
            "block size {block_size} is outside the current allowlist"
        )));
    }
    if !block_size.is_power_of_two() {
        return Err(ScanError::InvalidBlockZero(format!(
            "block size {block_size} is not a power of two"
        )));
    }
    Ok(())
}

fn validate_nxsb_block0(block: &[u8]) -> Result<(), ScanError> {
    if le_u32(block, 0x20) != NX_MAGIC {
        return Err(ScanError::InvalidBlockZero(
            "missing NXSB magic at block zero".to_string(),
        ));
    }
    if !is_nxsb_type(le_u32(block, 0x18)) {
        return Err(ScanError::InvalidBlockZero(
            "block-zero object type is not NX_SUPERBLOCK".to_string(),
        ));
    }
    if le_u64(block, 0x08) != NX_SUPERBLOCK_OID {
        return Err(ScanError::InvalidBlockZero(
            "block-zero object id is not the NX superblock object id".to_string(),
        ));
    }
    if !checksum_matches(block) {
        return Err(ScanError::InvalidBlockZero(
            "block-zero object checksum is invalid".to_string(),
        ));
    }
    Ok(())
}

fn parse_nxsb_candidate(
    block: &[u8],
    descriptor_index: u32,
    block_address: u64,
) -> Result<Option<NxsbCandidate>, String> {
    if le_u32(block, 0x20) != NX_MAGIC {
        return Ok(None);
    }

    let object_type_raw = le_u32(block, 0x18);
    if !is_nxsb_type(object_type_raw) {
        return Err(format!(
            "NXSB magic with non-NX-superblock object type {object_type_raw:#x}"
        ));
    }
    if !checksum_matches(block) {
        return Err("NXSB checksum mismatch".to_string());
    }

    Ok(Some(NxsbCandidate {
        descriptor_index,
        block_address,
        oid: le_u64(block, 0x08),
        xid: le_u64(block, 0x10),
        object_type_raw,
        object_subtype: le_u32(block, 0x1c),
        checksum: le_u64(block, 0x00),
    }))
}

fn is_nxsb_type(object_type_raw: u32) -> bool {
    object_type_raw & OBJECT_TYPE_MASK == OBJECT_TYPE_NX_SUPERBLOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_io::{put_u32, put_u64, resign_block};
    use std::io::Cursor;

    const BLOCK_SIZE: usize = 4096;
    const NXSB_TYPE: u32 = OBJECT_TYPE_NX_SUPERBLOCK;

    #[test]
    fn chooses_highest_valid_nxsb_and_skips_bad_checksum() {
        let mut image = vec![0u8; BLOCK_SIZE * 6];
        write_nxsb(&mut image, 0, 2, 2, 3, true, NXSB_TYPE);
        write_nxsb(&mut image, 2, 30, 2, 2, true, NXSB_TYPE);
        write_nxsb(&mut image, 3, 99, 3, 3, false, NXSB_TYPE);
        write_nxsb(&mut image, 4, 42, 4, 4, true, NXSB_TYPE);

        let report = scan_reader(&mut Cursor::new(image)).expect("scan succeeds");
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.skipped_descriptors.len(), 1);
        assert_eq!(report.highest_candidate().unwrap().xid, 42);
    }

    #[test]
    fn rejects_non_contiguous_descriptor_area() {
        let mut image = vec![0u8; BLOCK_SIZE * 2];
        write_nxsb(&mut image, 0, 2, 2, 1, true, NXSB_TYPE);
        let base = le_u64(&image[..BLOCK_SIZE], 0x70) | (1u64 << 63);
        put_u64(&mut image[..BLOCK_SIZE], 0x70, base);
        resign_block(&mut image[..BLOCK_SIZE]);

        let err = scan_reader(&mut Cursor::new(image)).expect_err("layout rejected");
        assert!(matches!(err, ScanError::UnsupportedDescriptorLayout(_)));
    }

    #[test]
    fn rejects_short_descriptor_read() {
        let mut image = vec![0u8; BLOCK_SIZE * 3 + 100];
        write_nxsb(&mut image, 0, 2, 2, 2, true, NXSB_TYPE);
        write_nxsb(&mut image, 2, 10, 2, 2, true, NXSB_TYPE);

        let err = scan_reader(&mut Cursor::new(image)).expect_err("short read rejected");
        assert!(matches!(
            err,
            ScanError::ShortRead {
                block_address: 3,
                ..
            }
        ));
    }

    #[test]
    fn skips_nxsb_magic_with_wrong_object_type() {
        let mut image = vec![0u8; BLOCK_SIZE * 4];
        write_nxsb(&mut image, 0, 2, 2, 2, true, NXSB_TYPE);
        write_nxsb(&mut image, 2, 10, 2, 2, true, 0x0000_000b);
        write_nxsb(&mut image, 3, 11, 3, 3, true, NXSB_TYPE);

        let report = scan_reader(&mut Cursor::new(image)).expect("scan succeeds");
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.highest_candidate().unwrap().xid, 11);
        assert_eq!(report.skipped_descriptors.len(), 1);
        assert!(report.skipped_descriptors[0]
            .reason
            .contains("non-NX-superblock"));
    }

    fn write_nxsb(
        image: &mut [u8],
        block_address: usize,
        xid: u64,
        descriptor_base: u64,
        descriptor_blocks: u32,
        valid_checksum: bool,
        object_type: u32,
    ) {
        let start = block_address * BLOCK_SIZE;
        let end = start + BLOCK_SIZE;
        let block = &mut image[start..end];
        block.fill(0);
        let oid = if block_address == 0 {
            NX_SUPERBLOCK_OID
        } else {
            block_address as u64
        };
        put_u64(block, 0x08, oid);
        put_u64(block, 0x10, xid);
        put_u32(block, 0x18, object_type);
        put_u32(block, 0x20, NX_MAGIC);
        put_u32(block, 0x24, BLOCK_SIZE as u32);
        put_u32(block, 0x68, descriptor_blocks);
        put_u64(block, 0x70, descriptor_base);
        resign_block(block);
        if !valid_checksum {
            block[100] ^= 0xff;
        }
    }
}
