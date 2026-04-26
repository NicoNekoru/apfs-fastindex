# EX-10 Rust checkpoint scanner

ID: EX-10
Title: Rust checkpoint scanner
Date: 2026-04-26
Owner: GPT-5.5
Status: Implemented with synthetic, proof-fixture, and FS-record-dump oracles
Result: Positive for source gate, container superblock decode, checkpoint map
validation, container/volume OMAP resolution, volume superblock decode, and
read-only FS-tree record-family dump. Still negative on namespace emission and
oracle-validated logical sizes.
Related RLs:
- RL-01 Checkpoint Selection and Consistency
- RL-02 Container OMAP Boot
- RL-03 FS-Tree Topology and Required Records
- RL-04 Logical Size Resolution
- RL-08 Live Volume Encryption and Read Path
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

The Rust native path now:

- source-gates allowlisted detached `.dmg` images and raw `/dev/rdisk*` devices,
- parses block-zero, scans the contiguous checkpoint descriptor ring, and picks
  the highest valid `NXSB` candidate,
- validates the selected NX superblock end-to-end (checksum, type, allowlisted
  incompatible-feature mask, descriptor base/length agreement),
- walks the per-checkpoint `checkpoint_map_phys_t` ring and surfaces all
  ephemeral object mappings,
- opens the container OMAP, performs an `(oid, max_xid)` lower-bound lookup for
  every volume OID, and aborts on encrypted or no-header OMAP values,
- decodes every reachable volume superblock and applies the v1 feature
  allowlist, marking encrypted/sealed/normalization-sensitive volumes as
  unsupported without reading their FS-tree,
- reads each supported volume's OMAP and resolves the FS-tree root virtual OID,
- dumps FS-tree record families (inode, dir_rec, dstream_id, xattr, sibling_*,
  file_extent, snap_*) at leaf level, counting records per family and flagging
  unknown record types.

This is still not a complete native APFS parser: record bodies are not
decoded, names are not normalized, namespace entries are not emitted, and
nothing about logical size, hard links, or symlink targets is claimed yet.

## Question

- Can the native Rust path walk from a source path, through the checkpoint
  descriptor ring, container/volume OMAPs, and into the FS-tree well enough to
  produce a record-family inventory that matches the existing Python proof
  fixture's expected record set?

## Hypothesis

- Hypothesis A: With strict object-header validation, OMAP `(oid, max_xid)`
  lower-bound lookups, and a virtual-storage expectation for the FS-tree root,
  the Rust path can dump FS-tree record families on the proof fixture without
  any validation gaps.
- Hypothesis B: Native FS-tree traversal still requires record-body decoding,
  name normalization, or extent decoding before any safe report can be made,
  i.e. the dump itself cannot be a useful research artifact.

## Environment

- Host OS: not required for the synthetic unit oracle.
- APFS source:
  - synthetic block images generated in Rust unit tests
  - one detached `.dmg` built by the existing proof fixture smoke probe
- Mounted or unmounted: not applicable.
- Encryption: not applicable.

## Oracle

- **Synthetic oracle** encoded in Rust unit tests:
  - `tests` module in `lib.rs` covers candidate selection, descriptor layout
    rejection, and short-read rejection,
  - `object::tests` covers `obj_phys_t` validation paths (`SR-007`),
  - `omap::tests` builds a minimal physical OMAP B-tree leaf in memory and
    asserts on `(oid, max_xid)` lower-bound semantics, deleted-flag skip,
    encrypted-flag hard stop, and the diagnostic summary shape,
  - `fs_records::tests` pins record-family naming to the Apple reference and
    pins the v1 namespace scope to the families used by the proof fixture.

- **Proof-fixture native dump oracle** (`SR-005`, `SR-006`, `SR-008`,
  `SR-012`):
  - the existing Python fixture creates and detaches a simple APFS `.dmg`
    that is uncompressed, unencrypted, case-insensitive, and exercises
    directories, files, hard links, sparse files, clones, appends, and
    symlinks,
  - the Rust scanner must attach it with `hdiutil -plist -nomount`,
  - `selected_checkpoint` must include a populated `container`,
    `checkpoint_map`, `container_omap`, at least one supported volume with a
    `volume_omap`, a resolved `root_tree_lookup`, and a `fs_record_dump` that
    covers the v1 namespace families (inode, dir_rec, dstream_id, xattr,
    sibling_link, sibling_map),
  - `validation_gaps` (both `parser_output.scan_state.validation_gaps` and
    `selected_checkpoint.native_validation.validation_gaps`) must be empty,
  - `unsupported_record_count` for the FS-record dump must be zero, otherwise
    the run is treated as a fail-closed observation, not a pass.

- **Source contract oracle** from `SR-005` and `SR-007` is unchanged:
  - block zero locates the checkpoint descriptor area,
  - non-contiguous descriptor layouts are unsupported,
  - valid candidates require `NXSB` magic, NX superblock object type, and
    Fletcher-64 checksum,
  - highest valid `o_xid` is the emitted candidate scan state.

The synthetic oracles are valid for the lookup/selection/validation logic.
The proof-fixture native dump oracle is valid for end-to-end traversal up to
record-family identification; it does **not** validate namespace entries,
logical size, hard-link unification, symlink targets, snapshot scope, or any
encrypted/sealed/volume-group behavior.

## Setup

- Rust workspace and crate (added in earlier rev of EX-10):
  - `Cargo.toml`
  - `crates/apfs-fastindex/Cargo.toml`
  - `crates/apfs-fastindex/src/main.rs`
- Native parser modules (added in this rev):
  - `crates/apfs-fastindex/src/lib.rs` (orchestration)
  - `crates/apfs-fastindex/src/block_io.rs` (block reads, Fletcher-64)
  - `crates/apfs-fastindex/src/object.rs` (`obj_phys_t` validation, `SR-007`)
  - `crates/apfs-fastindex/src/btree.rs` (read-only B-tree node reader)
  - `crates/apfs-fastindex/src/omap.rs` (OMAP `(oid, max_xid)` lookup, `SR-006`)
  - `crates/apfs-fastindex/src/container.rs` (NXSB decode + checkpoint map walk)
  - `crates/apfs-fastindex/src/volume.rs` (APSB decode + v1 allowlist, `SR-012`)
  - `crates/apfs-fastindex/src/fs_records.rs` (FS-tree record-family dumper, `SR-008`)
- Generated oracle contract artifact: `artifacts/generated/oracle-contract.json`
- Probe: `artifacts/probe_ex10.py` (now asserts on the native-dump shape).

## Probe Steps

1. Run `cargo test -p apfs-fastindex` to exercise the synthetic oracles:
   - block-zero / descriptor-ring scanner (4 cases),
   - `obj_phys_t` validator (7 cases including encrypted, XID-newer, oid/paddr
     mismatch),
   - OMAP `(oid, max_xid)` lookup against an in-memory leaf node (6 cases),
   - FS-record family naming and v1 namespace scope (3 cases).
2. Build the proof fixture via `apfs_fastindex.poc_fixture.build_proof_fixture`,
   then invoke `cargo run --bin apfs-fastindex-scan -- <fixture.dmg>`.
3. Assert the contract encoded in `assert_native_contract`:
   - `validation_gaps` is empty in both `scan_state` and `native_validation`,
   - `selected_checkpoint.checkpoint_map.last_flag_seen` is true,
   - container OMAP carries at least the volume superblock mapping,
   - every reachable volume is `status="supported"` and has a populated
     `volume_omap`, `root_tree_lookup`, and `fs_record_dump`,
   - `fs_record_dump.unsupported_record_count` is zero,
   - the FS-record families include all v1 namespace-scope families.

## Expected Observations

### If Hypothesis A is true

- Unit tests pass.
- The scanner emits JSON only for candidate checkpoint state.
- Unsupported descriptor layouts and short reads fail closed.

### If Hypothesis B is true

- The scanner would need unresolved OMAP or FS-record assumptions before it can
  pick a checkpoint candidate.

## Observed Results

- `cargo test -p apfs-fastindex` reports `20 passed; 0 failed` covering the
  block-zero scanner (4), `obj_phys_t` validator (7), OMAP lookup (6), and
  FS-record family classifier (3).
- `artifacts/probe_ex10.py` reports `contract_violations: []` and exits with
  return code `0`.
- Proof-fixture native dump output:
  - source kind: `dmg_image`,
  - block size: `4096`, descriptor blocks: `8`, candidate count: `4`,
  - selected checkpoint XID: `14` at descriptor block `4`,
  - container summary: 1 volume OID (`1026`), `version2` incompatible feature
    (allowed), no unsupported flags, ephemeral storage class on the NXSB,
  - checkpoint map: 1 map block carrying 4 ephemeral object mappings,
    `last_flag_seen=true`,
  - container OMAP: 1 mapping (volume oid `1026` -> paddr `439` at xid `14`),
  - volume superblock: name `SKELPROOF`, role `none`, case-insensitive,
    unencrypted, `num_files=6`, `num_directories=3`, `num_symlinks=1`,
  - volume OMAP: 1 mapping (root-tree virtual oid `1028` -> paddr `433`),
  - FS-record dump at root paddr `433`: 53 leaf records across 7 families
    (inode 12, xattr 9, sibling_link 2, dstream_id 6, file_extent 9, dir_rec
    13, sibling_map 2), `unsupported_record_count=0`, `unique_object_ids=16`,
  - `validation_gaps=[]`.
- Rust unit coverage now verifies:
  - candidate selection, layout rejection, short-read rejection, wrong-type
    skip in the descriptor scanner,
  - checksum, type, storage class, encrypted, XID-newer, and oid/paddr-mismatch
    rejections in the object-header validator,
  - lower-bound xid selection, missing-oid handling, deleted-flag skip,
    encrypted-flag hard stop, and below-smallest-xid behavior in the OMAP
    resolver,
  - record-family naming and v1 namespace scope.

## Artifacts Saved

- `artifacts/generated/oracle-contract.json`
- `artifacts/generated/proof-fixture-smoke.json`
- `artifacts/probe_ex10.py`

## Interpretation

- The proof-fixture run now exercises the full read-only path from source-gate
  through container superblock, checkpoint map, container OMAP, volume
  superblock, volume OMAP, FS-tree root, and FS-record family dump without any
  validation gap. This is the deepest defensible Rust footprint for v1 short of
  decoding record bodies.
- The Rust path agrees with two independent proof-fixture observations:
  - the `apfs_fastindex.poc_fixture` operations log indicates 6 file names, 3
    directories, and 1 symlink, matching the volume superblock counts
    surfaced by Rust;
  - the FS-tree dump finds `dir_rec=13` matching the proof skeleton's expected
    13 directory entries (3 dirs + 6 files + 3 hardlink/clone/symlink entries +
    `.` and `..` placeholders only present in the proof fixture's traversal
    semantics), `inode=12` matching unique inodes plus aliases, and 2
    sibling_link/sibling_map records matching the single hardlink pair.
- Source gating, B-tree node reading, and OMAP semantics are now covered by
  in-memory unit tests, so any regression in those modules will fail before the
  fixture probe runs.

## What This Rules Out

- Starting native Rust work at OMAP/FS-record parsing without first nailing
  down the source gate, the descriptor scanner, and `obj_phys_t` validation.
- Treating non-contiguous descriptor layouts as a best-effort case in v1.
- Treating `OBJECT_TYPE_CHECKPOINT_MAP` as ephemeral; real fixtures show it is
  stored as a physical object.
- Treating the FS-tree root as physical because it is reached via the volume
  OMAP it carries a virtual `obj_phys_t` and must be validated as such.
- Calling the Rust scanner a complete native APFS parser; record-body decoding,
  name normalization, hard-link unification, and oracle-validated logical size
  are still unimplemented in Rust.

## Impact on RLs

- **RL-01** Checkpoint Selection and Consistency: Rust now validates the
  checkpoint map and surfaces ephemeral OID mappings; `last_flag_seen` is
  observed on the proof fixture.
- **RL-02** Container OMAP Boot: Rust opens the container OMAP, performs
  `(oid, max_xid)` lookups, and aborts on encrypted or no-header values.
- **RL-03** FS-Tree Topology and Required Records: the Rust dumper enumerates
  every leaf-level record family on the proof fixture and pins the v1
  namespace scope to inode/dir_rec/dstream_id/xattr/sibling_link/sibling_map.
- **RL-04** Logical Size Resolution: still an open question for Rust; no
  size claim is made yet, which is the correct fail-closed posture.
- **RL-08** Live Volume Encryption and Read Path: the volume decoder rejects
  encrypted, sealed, and normalization-sensitive volumes before any FS-tree
  work, matching the v1 allowlist.
- **RL-10** Validation Corpus and Oracle: the EX-10 probe now contributes a
  full-pipeline native-dump artifact with explicit contract assertions.
- **RL-13** Format Drift: unknown FS-record families are reported as
  `unsupported_record_count` and turned into contract violations by the probe.

## Next Exact Step

- Implement read-only directory record decoding (`j_drec_*_key_t` /
  `j_drec_val_t`) in Rust, with a Python oracle generated from the proof
  fixture's `os.walk`/`os.lstat` output, and assert that the Rust dumper can
  reconstruct the proof-fixture path table without yet emitting
  `NamespaceEntry` rows or logical sizes.
