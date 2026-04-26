# EX-12 OMAP lookup contract

ID: EX-12
Title: OMAP lookup contract
Date: 2026-04-26
Owner: Claude Opus 4.7
Status: Executed
Result: `validated_omap_lookup_contract` against a paired raw image + identity oracle generated in the same run
Related RLs:
- RL-02 OMAP and Object Resolution
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

This experiment is the first native OMAP proof gate. It verifies that
lookup is `(omap context, oid, selected_xid) -> mapping with greatest xid <=
selected_xid`, that deleted, encrypted, no-header, crypto-generation,
OMAP-phys ENCRYPTING/DECRYPTING/KEYROLLING/CRYPTO_GENERATION_FLAG, and
unknown-bit cases fail closed before FS-record parsing begins, and that
the resolver-output volume superblock, volume OMAP, and FS root tree pass
SR-007 obj-header validation when re-read by an independent observer.

The earlier blocker - that `EX-06`/`EX-07` preserved identity JSON without
the raw images that produced it - is replaced here with a self-contained
probe. The new probe builds a fresh APFS proof fixture, keeps the `.dmg`
alive while two independent readers execute against it, and pairs the
native Rust scanner output with a `go-apfs` identitydump captured from
the same on-disk bytes. Both tools therefore witness identical raw media,
and divergences are recorded as evidence rather than ignored.

## Question

- For a pinned APFS state, can the resolver choose the correct OMAP domain
  and mapping for an object ID at `selected_xid`, and can it reject the
  important failure cases without reading wrong objects?

## Hypothesis

- Hypothesis A: The native Rust resolver returns `(oid, paddr, xid)` tuples
  that pass independent obj-header validation at every returned paddr,
  satisfy SR-006 lower-bound semantics over the OMAP samples Rust observed,
  and agree with the `go-apfs` identitydump on the FS root tree object ID.
- Hypothesis B: Native OMAP lookup exposes cursor, deletion, flag, snapshot,
  or domain behavior that requires narrowing the resolver contract before
  root discovery can proceed.

## Environment

Executed environment:

- generated detached unencrypted image-backed APFS source
  (`build_proof_fixture`) attached `-nomount`
- validated checkpoint context produced by the native Rust scanner itself
  (NXSB candidate scan, container superblock decode, checkpoint-map walk,
  feature allowlist) on the same source
- no live mounted raw scans
- no encrypted source support
- no snapshot source support

Recorded for each volume:

- source ID and selected checkpoint XID
- OMAP domain (container vs selected volume)
- OMAP `obj_phys_t` header read independently from disk
- OMAP tree root identity
- lookup key `(oid, selected_xid)`
- returned OMAP key/value
- resolved object header (type/subtype/checksum/oid/xid/storage class)

## Oracle

Three independent observers cross-check the resolver:

1. **On-disk obj-header replay.** The probe re-reads each paddr the Rust
   resolver returned and recomputes Fletcher-64. It confirms type/subtype
   base bits, storage class, oid, and `xid <= selected_xid`. This is the
   primary correctness oracle because it does not depend on any other
   parser.
2. **SR-006 lower-bound replay.** The probe extracts Rust's published OMAP
   sample lists (`container_omap.sample_mappings`,
   `volume_omap.sample_mappings`) and re-runs the lower-bound algorithm in
   Python. The Rust-returned mapping must equal the largest sample with
   matching `oid` and `xid <= selected_xid`. This is degenerate (single
   mapping) on the proof fixture, so the multi-version proof comes from the
   Rust unit tests instead (see "Synthetic failure cases").
3. **`go-apfs` identitydump.** The existing EX-06 identitydump tool runs
   against the same raw container the Rust scanner saw. It exposes
   `root_tree.{oid, paddr, object_xid, checksum, content_hash}` after its
   own pipeline navigates from `apfs.Open` to the FS root. Cross-tool
   agreement on `oid` is required; `(paddr, xid)` divergence is recorded
   but does not fail the experiment - go-apfs's `apfs.Open` selects its own
   active-state checkpoint, which can differ from the Rust scanner's choice.

## Setup

- `artifacts/probe_ex12.py` (this run) is the canonical executable.
- Each run writes JSON artifacts under `artifacts/generated/`.
- The previous "blocked" verdict is preserved as
  `artifacts/generated/blocker.json` and
  `artifacts/generated/blocked-summary.json` for history.

## Probe Steps

1. Run the SR-006 hard-stop unit tests in `crates/apfs-fastindex` (covers
   the `OMAP_VAL_ENCRYPTED`, `OMAP_VAL_NOHEADER`, `OMAP_VAL_DELETED`,
   `OMAP_VAL_CRYPTO_GENERATION`, unknown-value-bit, missing-OID, and
   below-smallest-XID cases plus the OMAP-phys
   `ENCRYPTING`/`DECRYPTING`/`KEYROLLING`/`CRYPTO_GENERATION_FLAG`/
   unknown-bit cases on a fully synthetic OMAP image).
2. Build a fresh proof fixture via
   `apfs_fastindex.poc_fixture.build_proof_fixture()`.
3. Re-attach the fixture image with `hdiutil attach -plist -nomount` so the
   raw `/dev/rdiskN` device is exposed without mounting.
4. Run `cargo run --release --bin apfs-fastindex-scan -- /dev/rdiskN` and
   capture the `CheckpointScanOutput` JSON.
5. Run `go run .` in the EX-06 identitydump directory against the same
   `/dev/rdiskN` and capture its `output` JSON.
6. For every (volume_oid, root_tree_oid) the Rust scanner resolved:
   - validate the obj-header at the returned paddr
     (type/subtype/storage/oid/xid/checksum),
   - re-run SR-006 lower-bound on Rust's published sample list,
   - compare against identitydump (oid agreement required;
     paddr/xid divergence captured),
   - read the volume OMAP phys block as `OBJECT_TYPE_OMAP` physical and
     verify its checksum.
7. Detach the image.

Reject if any of the following happens:

- no key with matching `oid` and `xid <= selected_xid` in any sample list,
- an `OMAP_VAL_DELETED`/`OMAP_VAL_ENCRYPTED`/`OMAP_VAL_NOHEADER`/
  `OMAP_VAL_CRYPTO_GENERATION` or unknown-flag value reaches the lookup
  result (Rust unit tests),
- an `OMAP_ENCRYPTING`/`DECRYPTING`/`KEYROLLING`/`CRYPTO_GENERATION_FLAG`
  or unknown-bit OMAP phys flag is observed at OMAP open time
  (Rust unit tests),
- the resolved object header fails type, subtype, storage-class, oid,
  xid, or checksum validation,
- identitydump and Rust disagree on `root_tree.oid`.

## Expected Observations

### If Hypothesis A is true

- Native lookup paddrs validate as expected APFS object types with valid
  checksums.
- Rust's lookup result is the SR-006 lower-bound entry of its own samples.
- Rust and identitydump agree on `root_tree.oid`.
- All synthetic hard-stop unit tests pass.
- Any divergence between Rust and identitydump on `(paddr, xid)` reflects
  go-apfs's own active-state choice, not a contract violation.

### If Hypothesis B is true

- Native lookup disagrees with on-disk obj-header replay or SR-006
  lower-bound replay, indicating a narrower resolver rule or missing
  flag/domain handling.

## Observed Results

### 2026-04-26 execution

- Verdict: `validated_omap_lookup_contract`.
- Source: generated proof fixture (`skeleton-proof.dmg`,
  ~167 MB, container UUID surfaced in the artifact).
- `selected_xid = 14`, `block_size = 4096`, container OMAP at paddr 440,
  volume OMAP for volume oid `1026` at paddr 434.
- Container OMAP header validates as `OBJECT_TYPE_OMAP` physical, valid
  Fletcher-64, oid==paddr, xid<=selected_xid.
- Volume superblock at paddr 439 validates as `OBJECT_TYPE_FS` (`0x0D`)
  virtual, oid 1026, xid 14, valid checksum.
- Volume OMAP header validates as `OBJECT_TYPE_OMAP` physical, valid
  Fletcher-64.
- FS root tree at paddr 433 validates as `OBJECT_TYPE_BTREE` virtual with
  subtype `OBJECT_TYPE_FSTREE` (`0x0E`), oid 1028, xid 13, valid checksum.
- SR-006 lower-bound check on Rust's container OMAP sample list returns
  the same `(oid, paddr, xid)` tuple Rust returned for volume oid 1026
  (`(1026, 439, 14)`).
- SR-006 lower-bound check on Rust's volume OMAP sample list returns the
  same `(oid, paddr, xid)` tuple Rust returned for the FS root tree oid
  1028 (`(1028, 433, 13)`).
- 14 OMAP unit tests pass: the 5 lookup-shape tests (largest-xid,
  missing-oid, deleted, max_xid-below-smallest, encrypted) plus 8 new
  SR-006 hard-stop tests added in this run (CRYPTO_GENERATION value,
  NOHEADER value, unknown-value-bit; OMAP-phys ENCRYPTING, DECRYPTING,
  KEYROLLING, CRYPTO_GENERATION_FLAG, unknown-phys-bit) plus the existing
  summarize coverage.
- Cross-tool oracle: identitydump and Rust agree on `root_tree.oid = 1028`.
  identitydump reports `(paddr, object_xid) = (427, 12)` while Rust reports
  `(433, 13)`. The probe captures this as
  `go_apfs_active_state_observation`. Inspecting Rust's checkpoint
  candidates shows four valid NXSBs at xids `{11, 12, 13, 14}`. Rust's
  resolver is parameterized on the highest-XID NXSB (xid 14). go-apfs's
  `apfs.Open` independently selects an earlier NXSB, navigates a
  different volume superblock, and resolves a different volume OMAP -
  hence different physical addresses for the same OID. Both views are
  individually internally consistent: `root_tree.oid` is stable across
  versions, and each version's paddr produces a checksum-valid B-tree root
  with subtype `OBJECT_TYPE_FSTREE` and the same logical content
  (53 leaf records). This documents an active-state-selection caveat
  rather than a contract violation.

### Synthetic failure coverage (Rust unit tests)

- `omap::tests::lookup_returns_largest_xid_at_or_below_max`
- `omap::tests::lookup_returns_none_when_oid_missing`
- `omap::tests::lookup_returns_none_when_max_xid_below_smallest`
- `omap::tests::lookup_skips_deleted_value`
- `omap::tests::lookup_rejects_encrypted_value`
- `omap::tests::lookup_rejects_noheader_value`
- `omap::tests::lookup_rejects_crypto_generation_value`
- `omap::tests::lookup_rejects_unknown_value_flag_bits`
- `omap::tests::open_rejects_phys_encrypting_flag`
- `omap::tests::open_rejects_phys_decrypting_flag`
- `omap::tests::open_rejects_phys_keyrolling_flag`
- `omap::tests::open_rejects_phys_crypto_generation_flag`
- `omap::tests::open_rejects_unknown_phys_flag_bits`
- `omap::tests::summarize_records_flagged_values`

## Artifacts Saved

Current run:

- `README.md`
- `artifacts/probe_ex12.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/synthetic-omap-tests.json`
- `artifacts/generated/paired-fixture.json`
- `artifacts/generated/summary.json`

Historical (kept for context):

- `artifacts/generated/oracle-contract.json`
- `artifacts/generated/blocker.json`
- `artifacts/generated/blocked-summary.json`

## Interpretation

- The OMAP lookup contract holds for the native Rust resolver under
  `selected_xid = container.xid` for the proof fixture. Rust's lookup
  output is internally consistent with the OMAP it observed and is
  independently confirmed by on-disk obj-header replay and Rust's
  synthetic hard-stop unit tests.
- The cross-tool comparison adds a useful caveat for the support matrix:
  third-party APFS parsers can pick a different active-state checkpoint
  than this scanner. The contract is parameterized by `selected_xid`, so
  any oracle used to validate native lookup must declare the same
  `selected_xid`.
- FS-record body decoding (`DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`,
  `SIBLING_MAP`, dstream/logical-size fields) may now begin without
  invalidating the OMAP/root contract this experiment proves.
- Multi-version OMAP behavior across delete/reuse churn on a real APFS
  image remains future work; the current proof fixture has only one
  mapping per OID in each OMAP. The Rust unit tests prove the algorithm
  but not the empirical behavior of repeated mutation.

## What This Rules Out

- A global `oid -> paddr` resolver.
- Bare-OID, latest-only, or cross-domain shortcut lookup.
- Comparing native lookup on a fresh generated fixture to stale identities
  from a different image (the probe always pairs raw bytes with the
  oracle generated against those same raw bytes).
- Treating `apfs.Open` from any third-party parser as the canonical
  oracle without first naming the same `selected_xid`.

## Impact on RLs

- RL-02: turns the resolver contract into an executed proof gate; records
  the (paddr, xid) divergence between Rust and go-apfs as an
  active-state-selection caveat, not a violation.
- RL-04 / RL-05 / RL-09: keep cache and subtree identity downstream of a
  resolver result that has now been validated and recorded.
- RL-10: registers identitydump and the on-disk obj-header replay as the
  EX-12 oracle pair, plus the Rust synthetic OMAP unit tests for
  hard-stop coverage.
- RL-13: names the fully enumerated OMAP value-flag and OMAP-phys flag
  hard stops the resolver enforces today
  (DELETED is a negative result; ENCRYPTED, NOHEADER, CRYPTO_GENERATION,
  unknown value bits, ENCRYPTING, DECRYPTING, KEYROLLING,
  CRYPTO_GENERATION_FLAG, and unknown phys bits are hard stops).

## Next Exact Step

- Begin FS-record body decoding under the validated OMAP/root context:
  parse `DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`, `SIBLING_MAP`, and
  dstream/logical-size fields in Rust, then emit `NamespaceEntry` rows
  whose oracle-validation diff can be checked against identitydump's
  `entries`/`record_groups` in a future EX.
