# SR-006 OMAP Lookup Semantics

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-02
- RL-05
- RL-13

## Bottom line

APFS object lookup is not `oid -> paddr`. The minimum safe resolver key is
`(omap context, oid, max_xid or snapshot_xid)`. Lookup must return the mapping
for the requested `oid` with the highest `xid <= selected_xid`, and then reject
deleted mappings, unsupported OMAP/OMAP-value flags, ambiguous domain selection,
and resolved-object validation failures.

This review answers one question: what OMAP lookup contract may the native
parser encode without inventing semantics?

## Evidence

### Spec

- Apple File System Reference defines object maps as the structures used to map
  virtual object identifiers to physical storage, with OMAP keys containing
  object identity and transaction identity.
- The APFS mount path locates the container OMAP from the selected container
  superblock, then resolves volume superblocks through the container OMAP.
- `omap_val_t` stores flags, mapped object size, and physical address. Those
  flags are part of the resolver contract, not optional metadata.

### Observation

- JT Sylve's OMAP writeup states that the container and each volume maintain
  independent OMAPs with independent virtual address spaces, and that OMAP keys
  are ordered by `oid` then `xid`.
- The same writeup says active-state lookup chooses the highest available XID
  for the object, while snapshot parsing ignores keys newer than the snapshot
  XID.
- JT Sylve, `apfs-fuse`, `dissect.apfs`, and `linux-apfs-rw` converge on the
  OMAP value flags: `DELETED`, `SAVED`, `ENCRYPTED`, `NOHEADER`, and
  `CRYPTO_GENERATION`.
- `linux-apfs-rw` documents `apfs_omap_lookup_block_with_xid` as searching for
  the most recent matching object with transaction ID below the requested XID;
  its mounted-XID helper uses the snapshot XID when a snapshot is mounted.
- `go-apfs` resolves the FS root tree through the volume OMAP using the volume
  superblock object's XID, showing that root discovery already depends on
  XID-aware OMAP lookup.
- `EX-06` showed the FS root tree OID can remain stable while paddr, object XID,
  checksum, and block hash change after each mutation.
- `EX-12` executed a self-paired proof fixture and validated the native
  `(omap context, oid, selected_xid)` lower-bound lookup contract against
  on-disk object-header replay, Python replay of Rust-published OMAP samples,
  and cross-tool `root_tree.oid` agreement.

### Hypothesis

- The native resolver should first implement active-state `max_xid` lookup
  against one selected OMAP domain. A lookup that lands on `OMAP_VAL_DELETED`
  means "not present at this scan state", not "try an older mapping" unless a
  recovery mode is explicitly specified.
- Snapshot-aware lookup is the same shape but must not be treated as supported
  until snapshot root selection and oracle semantics are probed.

## Open Limits

- The exact B-tree cursor rule has proof-fixture coverage from `EX-12`: seek to
  `(oid, selected_xid)` and accept only the greatest key with matching `oid` and
  `xid <= selected_xid`.
- OMAP snapshot trees and pending revert ranges remain outside v1.
- `OMAP_VAL_ENCRYPTED`, `OMAP_VAL_NOHEADER`, `OMAP_VAL_CRYPTO_GENERATION`, OMAP
  `ENCRYPTING`/`DECRYPTING`/`KEYROLLING`, and unknown flag bits are hard stops
  until the caller has type-specific support.
- The probe still needs direct evidence for deleted mappings under delete/reuse
  churn.

## Decision impact

- `RL-02`: resolver APIs must name the OMAP domain and scan-state XID.
- `RL-05`: subtree reuse can only consume resolved identities after OMAP lookup,
  never bare object IDs.
- `RL-13`: unsupported OMAP flags and value flags are fail-closed parser gates.
- Exact next step: carry the `EX-12` selected-XID discipline into `EX-13`
  FS-record body decoding; separately design a churn probe for repeated
  delete/reuse histories and direct deleted-mapping evidence.
