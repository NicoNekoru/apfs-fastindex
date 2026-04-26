# EX-13 Native FS-record body oracle

ID: EX-13
Title: Native FS-record body oracle
Date: 2026-04-26
Owner: GPT-5.5
Status: Designed
Result: Not executed
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

`EX-12` validates native OMAP/root lookup for a paired detached fixture. The
next correctness gate is not product namespace emission; it is a native
FS-record body oracle that proves the parser can decode the required
`DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`, `SIBLING_MAP`, and dstream fields
from the same selected scan state.

## Question

- Can native record-body field dumps reproduce the mounted/POSIX namespace and
  ordinary logical-size facts for the same generated APFS proof fixture, under a
  declared `selected_xid`?

## Hypotheses

- Hypothesis A: On a detached unencrypted image-backed proof fixture, native
  field dumps for directory records, inodes, xattrs, sibling records, and inode
  dstreams are sufficient to reconstruct oracle-checkable namespace rows and
  ordinary logical file sizes.
- Hypothesis B: At least one required body field, variable-length encoding,
  xfield case, or cross-tool selected-XID mismatch prevents a trustworthy
  namespace/logical-size diff; the result should identify the blocker rather
  than emit product rows.

## Environment

Recommended first environment:

- generated unencrypted APFS `.dmg`
- mounted setup phase for fixture creation and POSIX oracle capture
- detached or `-nomount` raw phase for native scanning
- same-run `go-apfs identitydump` or equivalent third-party record-group dump
- no live startup source, encryption, snapshot, sealed-volume, or volume-group
  semantic support claim

Record:

- macOS version and APFS tool versions
- fixture `.dmg` path and retained raw-media artifact policy
- mounted device and raw `/dev/rdiskN`
- selected native `NXSB` XID and FS-root identity
- third-party parser active-state/XID if available
- volume feature and incompatibility bits
- case sensitivity / normalization flags
- record-family counts before and after body decoding

## Oracle

Use three scoped observers:

- Mounted/POSIX namespace oracle:
  collect file path, entry type, stable file identity, link count where public
  APIs expose it, symlink target, and logical byte size while the generated image
  is mounted and quiescent.
- Native raw field-dump oracle:
  after detach or `-nomount`, run the Rust path against the same image and dump
  body fields without product aggregation.
- Cross-tool structural observer:
  run `go-apfs identitydump` or another APFS parser against the same raw device
  in the same execution, preserving selected-XID caveats from `EX-12`.

This oracle is valid because the question is field decoding for a stable
single-volume fixture. POSIX owns visible namespace and logical size for the
chosen mounted state; native raw output owns selected-XID field extraction; the
third-party observer helps separate APFS record-shape misunderstandings from
native implementation mistakes. If the observers do not name the same state, the
comparison must be marked inconclusive instead of coerced.

## Required Native Dump Fields

- Source and scan state:
  selected checkpoint XID, volume OID, volume UUID, volume feature bits, root
  tree OID/paddr/xid, block size, and validation notes.
- `DIR_REC`:
  parent directory object ID from the key, raw key form (hashed or unhashed),
  decoded name bytes/string, `file_id`, `date_added`, low-bit entry type,
  reserved flag bits, and optional `DREC_EXT_TYPE_SIBLING_ID`.
- `INODE`:
  inode ID, `parent_id`, `private_id`, `mode`, `nchildren`/`nlink`,
  `internal_flags`, `bsd_flags`, `owner`, `group`, `uncompressed_size` plus
  whether `INODE_HAS_UNCOMPRESSED_SIZE` is set, and parsed inode xfields.
- dstream metadata:
  `INO_EXT_TYPE_DSTREAM` as `j_dstream_t.size`, `alloced_size`,
  `default_crypto_id`, `total_bytes_written`, `total_bytes_read`;
  `DSTREAM_ID.refcnt` where present.
- `XATTR`:
  object ID, xattr name, `flags`, `xdata_len`, embedded payload summary, stream
  object ID if stream-backed, and explicit recognition of
  `com.apple.fs.symlink`, `com.apple.fs.firmlink`, and `com.apple.decmpfs`.
- `SIBLING_LINK`:
  target inode ID from key, `sibling_id`, `parent_id`, and sibling name.
- `SIBLING_MAP`:
  sibling ID from key and target `file_id`.

## Fixture Matrix

Minimum first fixture:

- nested directories
- regular file
- renamed and moved file
- hard link across directories
- symlink with target payload
- sparse file with ordinary logical size
- clone whose source is later mutated
- file with an embedded user xattr
- optional compressed file recorded as accounting probe input, not a pass/fail
  requirement for ordinary logical-size success

## Probe Steps

1. Generate the fixture image and capture the mounted/POSIX oracle while the
   fixture is quiescent.
2. Record fixture operations and public logical-size observations separately
   from allocated/shared/exclusive observations.
3. Detach or attach `-nomount` and run the native scanner with body-dump mode.
4. Save native raw record-body dumps under `artifacts/generated/`.
5. Run the third-party observer against the same raw device and save its output.
6. Verify native selected checkpoint, checkpoint map, OMAP/root context, and
   FS-tree record-family counts before comparing body fields.
7. Build normalized comparison rows:
   `path`, `entry_type`, `file_identity`, `parent_identity`, `dir_entry_name`,
   `inode_mode`, `link_group`, `symlink_target`, and `logical_size`.
8. Emit one of:
   - `validated_native_record_body_contract`
   - `body_field_mismatch`
   - `selected_xid_mismatch`
   - `unsupported_record_body`
   - `malformed_record_body`
   - `oracle_inconclusive`
   - `not_executed`

## Expected Observations

### If Hypothesis A is true

- Native `DIR_REC` names and `file_id` links reproduce the mounted path set.
- Native `INODE.mode` and `DIR_REC.flags` reproduce entry types.
- Native sibling records preserve hard-link path identity versus shared file
  identity.
- Native symlink xattr decoding reproduces symlink targets.
- Native inode dstream `size` reproduces ordinary logical size for
  uncompressed files in the fixture.

### If Hypothesis B is true

- The probe reports the first mismatch category with enough raw field context to
  choose the next source review or parser hard stop.
- Cross-tool `(paddr, object_xid)` divergence is not treated as a field-decoding
  failure unless the compared tools declare the same `selected_xid`.
- Compression, sparse allocated-size, clone/shared accounting, and
  snapshot-retained bytes remain outside the pass/fail verdict.

## Artifacts To Save

- `README.md`
- `artifacts/generated/oracle-contract.json`
- `artifacts/generated/environment.json`
- `artifacts/generated/fixture-operations.json`
- `artifacts/generated/mounted-posix-oracle.json`
- `artifacts/generated/native-record-body-dump.json`
- `artifacts/generated/go-apfs-record-observer.json`
- `artifacts/generated/comparison.json`
- `artifacts/generated/summary.json`
- execution script, when implemented, as `artifacts/probe_ex13.py`

## Interpretation

- Observation: `EX-12` makes this experiment legal to design because native
  OMAP/root context is no longer the current blocker for the proof fixture.
- Spec: `SR-014` identifies the body fields that must be present before
  namespace/logical-size rows can be trusted.
- Hypothesis: Passing this experiment should promote record-body decoding from
  source-backed design to native parser contract, while still leaving product
  aggregation, `EX-09` accounting, and subtree reuse outside scope.

## What This Can Rule Out

- Treating record-family counts as sufficient evidence for namespace support.
- Emitting product namespace rows before native `DIR_REC`/`INODE`/`XATTR`/sibling
  bodies have an oracle diff.
- Collapsing logical size, allocated size, compressed size, clone sharing, and
  snapshot-retained bytes into one metric.
- Using `go-apfs` or any third-party parser as a paddr/XID oracle without
  declaring selected-XID alignment.

## Impact on RLs

- `RL-03`: defines the next FS-tree parser gate after record-family counts.
- `RL-06`: defines the first native namespace field oracle.
- `RL-07`: separates ordinary logical-size body fields from `EX-09` accounting.
- `RL-10`: adds the next feature-specific validation oracle.
- `RL-13`: turns malformed record bodies and selected-XID mismatch into explicit
  hard-stop or inconclusive verdicts.

## Next Exact Step

- Implement only the `EX-13` probe/dump path when returning to code: generate a
  paired fixture, dump native record bodies, save POSIX and cross-tool observers,
  and compare field rows. Do not emit product namespace rows until the verdict is
  `validated_native_record_body_contract`.
