# Rust Checkpoint Scanner

Status: Native read-only path implemented through FS-tree record-family dump
Date: 2026-04-26
Scope: `crates/apfs-fastindex`

## Bottom line

The Rust crate is the native implementation slice for the v1 contract. It now
covers the full read-only path from source-gate to FS-record family inventory,
but **does not** decode FS record bodies, emit `NamespaceEntry` rows, compute
logical sizes, or claim live-mounted-source correctness.

What the Rust path currently proves on the proof fixture:

- accepted input is a detached APFS `.dmg` or caller-supplied raw
  `/dev/disk*` / `/dev/rdisk*` APFS container path,
- `.dmg` inputs are attached with `hdiutil attach -plist -nomount`, parsed
  with a plist parser, normalized to the raw APFS container device, and
  detached on drop,
- block zero is validated as an APFS container locator (block size, `NXSB`
  magic, object type, object ID, Fletcher-64 checksum),
- contiguous checkpoint descriptor areas are scanned for checksum-valid
  `NXSB` candidates and the highest candidate XID is selected,
- the selected NX superblock is decoded end-to-end (block_size, descriptor
  base/length, OMAP OID, volume OID array, feature/incompatible/RO masks,
  reaper/spaceman OIDs, UUID),
- the v1 incompatible-feature allowlist is enforced: any unknown
  incompatible bit is a hard stop,
- the per-checkpoint `checkpoint_map_phys_t` ring is walked, ephemeral OID
  mappings are reported, and `CHECKPOINT_MAP_LAST` is required,
- the container OMAP is opened, traversed, and queried with the
  `(oid, max_xid)` lower-bound semantics required by `SR-006`. At OMAP
  open time the resolver hard-stops on
  `OMAP_PHYS_ENCRYPTING`/`OMAP_PHYS_DECRYPTING`/`OMAP_PHYS_KEYROLLING`/
  `OMAP_PHYS_CRYPTO_GENERATION_FLAG` and any unknown bits in
  `omap_phys.flags`. At lookup time `OMAP_VAL_DELETED` is a negative
  result, while `OMAP_VAL_ENCRYPTED`, `OMAP_VAL_NOHEADER`,
  `OMAP_VAL_CRYPTO_GENERATION`, and any unknown bits in
  `omap_val_t.flags` force a hard stop. Coverage is exercised by 14 unit
  tests on synthetic OMAPs and confirmed end-to-end by `EX-12`,
- every reachable volume superblock is decoded, its v1 allowlist is
  enforced (encryption, sealed volumes, normalization sensitivity, unknown
  incompatible features all flip the volume to `Unsupported(reason)`), and
  its OMAP / FS-tree root virtual OID are recorded,
- supported volumes have their volume OMAP opened and the FS-tree root
  resolved through it,
- the FS-tree is walked read-only and a record-family dump is emitted
  (`inode`, `xattr`, `sibling_link`, `dstream_id`, `file_extent`, `dir_rec`,
  `sibling_map`, snapshot families, and an `unsupported` bucket). FS-tree
  internal-node values are resolved as **virtual child OIDs** through the
  volume OMAP at `(child_oid, scan_xid)` before each child block is read
  (EX-15 root cause; prior code read the value as a paddr and tripped a
  Fletcher-64 gate on the bare OID).
- each in-scope leaf entry is decoded into a structured `FsRecordRow`
  (`crates/apfs-fastindex/src/fs_record_body.rs`) under the SR-015 single
  xfield cursor rule plus the SR-016 fail-closed gates. The decoder
  surfaces: `key.kind` (`plain` / `named` / `sibling_link`), `value.kind`
  per family, fixed inode/drec/xattr/sibling fields, `xfields[]` with
  per-field `x_type`, `x_flags`, `x_size`, `padded_length`, `value_hex`,
  and interpreted `u64` / `utf8` / `dstream` value when applicable. EX-17
  and EX-18 prove the decoder against synthetic SR-016 negative blocks and
  against the Python EX-13/EX-16 parser respectively.

What it still does not prove:

- name normalization (UTF-8, NFD, case folding) and path reconstruction,
- logical-size precedence across compression / sparse / clone (SR-017 is
  source-reviewed but not yet executed against the corpus),
- hard-link aggregation policy in product output,
- snapshot, sealed-volume, volume-group, or firmlink scope,
- live mounted raw-scan correctness,
- physical/shared/exclusive accounting,
- incremental cache reuse.

The Python proof skeleton remains the authoritative oracle for
namespace+logical-size parity until those gaps close.

## Module layout

```
crates/apfs-fastindex/src/
├── lib.rs          orchestration, parser shapes, source gate
├── main.rs         apfs-fastindex-scan binary (JSON output)
├── block_io.rs     block reads, le_u*, Fletcher-64, test helpers
├── object.rs       obj_phys_t validation (SR-007 single chokepoint)
├── btree.rs        read-only B-tree node reader (fixed and variable kv)
├── omap.rs         OMAP resolver: (oid, max_xid) lower-bound + summary
├── container.rs    NXSB decode + checkpoint map ring walk
├── volume.rs       APSB decode + v1 feature allowlist (SR-012)
├── fs_records.rs   FS-tree walk + record-family dumper (SR-008)
└── fs_record_body.rs  Body decoders for v1 (SR-015 / SR-016 / EX-17 / EX-18)
```

The fail-closed contract is centralized in `object::validate_object_block`:
checksum, expected object type, expected storage class, encryption flag, no-
header flag, optional `oid == paddr` check, and optional `xid <= scan_xid`
guard. Every reader in the crate validates a block through this function
before trusting its body.

## Commands

Run unit tests:

```sh
cargo test -p apfs-fastindex
```

Build and run the scanner against a detached APFS `.dmg`:

```sh
cargo run --bin apfs-fastindex-scan -- /path/to/detached-apfs.dmg
```

Run the EX-10 probe (also asserts on the native dump structure):

```sh
python3 docs/research/experiments/EX-10-rust-checkpoint-scanner/artifacts/probe_ex10.py
```

Run the EX-12 OMAP lookup contract probe (builds a fresh paired raw image
plus identity oracle, runs the Rust scanner and `go-apfs identitydump`
against the same `/dev/rdiskN`, and asserts SR-006 lower-bound semantics
and obj-header replay at every Rust-returned paddr):

```sh
python3 docs/research/experiments/EX-12-omap-lookup-contract/artifacts/probe_ex12.py
```

The Python proof regression is still the authority for parity:

```sh
PYTHONPATH=src python3 -m apfs_fastindex.poc_regression
```

## CLI output shape

`apfs-fastindex-scan` prints a single JSON document. Top-level fields:

- `parser_output`: `SourceDescriptor`, `ScanState`, `backend_name`, empty
  `entries` and `aggregates` (no namespace claim).
- `checkpoint_candidates`: every checksum-valid `NXSB` candidate observed in
  the descriptor ring.
- `skipped_descriptors`: descriptors that carried `NXSB` magic but failed
  type, checksum, or shape validation.
- `selected_checkpoint`: optional, present only when the native path
  succeeds. Carries:
  - `block_address`, `xid`,
  - `container` (`ContainerSummary`) including `incompatible_features`
    decoded by name and `unsupported_incompatible_features` mask,
  - `checkpoint_map` (`CheckpointMapSummary`) with `map_blocks`,
    `mappings`, `last_flag_seen`, `validation_notes`,
  - `container_omap` (`OmapSummary`) with `phys`, `flagged_values`,
    `sample_mappings`,
  - `volumes`: array of `VolumeReport` carrying the volume superblock,
    its OMAP summary, the FS-tree root lookup, and the FS-record dump,
  - `native_validation`: a counters block including `validation_gaps`.
- `correctness_claim` and `not_claimed`: explicit, machine-readable text the
  research discipline requires the parser to print so callers cannot
  silently overgeneralize.

## Empirical corrections distilled into code

These are observations from running the Rust path against the real APFS
proof fixture; they each represent a fix landed in the crate. They are also
referenced in `RL-01`, `RL-02`, and `RL-03`.

1. `OBJECT_TYPE_CHECKPOINT_MAP` blocks carry `OBJ_PHYSICAL`, not
   `OBJ_EPHEMERAL`. The validator for the checkpoint map ring expects
   `ExpectedStorage::Physical`.
2. B-tree TOC `v_off` measures the distance from the **end** of the value
   area to the **start** of the value; values then run forward by
   `fixed_value_size` (or `v_len`). The earlier "value extends backward
   from `v_off`" assumption mis-decoded OMAP values as
   `flags=0x10ffff`.
3. The FS-tree root is reached through the volume OMAP, so on-disk it
   carries a virtual `obj_phys_t` (storage flags `0x0000_0000`). The FS-tree
   validator therefore must not require `o_oid == paddr`; it instead checks
   `header.oid == volume_omap_lookup.oid` and uses
   `ExpectedStorage::Virtual`.

## Replacement path

Remaining native work, in order:

1. Decode `j_drec_*_key_t` and `j_drec_val_t` to reconstruct the path table
   without yet emitting `NamespaceEntry` rows.
2. Decode `j_inode_val_t` flags, link count, and embedded fields needed for
   logical size selection (per `SR-009`).
3. Decode `j_dstream_id_val_t` and `j_dstream_t` to read logical size, and
   resolve the symlink-target `XATTR` for `RL-06`.
4. Resolve `j_sibling_link_val_t` and `j_sibling_map_val_t` for hard-link
   unification.
5. Wire the resolved namespace into the existing
   `ParserOutput`/`NamespaceEntry`/`DirectoryAggregate`/oracle-diff contract
   so the Rust path can be diffed against the Python proof oracle.
6. Re-run the existing Python proof fixture and expanded corpus after each
   replacement stage. Do not declare native parity until the regression
   suite passes.

Any support broadening must first update the related `SR-*`, `EX-*`, and
`RL-*` artifacts.
