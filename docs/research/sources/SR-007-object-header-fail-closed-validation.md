# SR-007 Object Header Fail-Closed Validation

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-01
- RL-02
- RL-13

## Bottom line

Every native parser step must validate the APFS object header before trusting
object body fields. The first hard gates are checksum, object type, subtype when
the caller expects one, and object XID not newer than the selected scan state.

This review answers one question: which object-header checks are mandatory for
fail-closed parsing?

## Evidence

### Spec

- Apple File System Reference defines `obj_phys_t` with `o_cksum`, `o_oid`,
  `o_xid`, `o_type`, and `o_subtype`, and describes `o_cksum` as the Fletcher-64
  checksum of the object.
- The reference's boot and mount flows explicitly say to verify object checksum
  and magic before using superblock-like objects.

### Observation

- JT Sylve's object anatomy writeup says most APFS on-disk objects start with
  `obj_phys_t`, that checksum mismatch indicates partial flush or corruption,
  and that type/subtype distinguish structures and B-tree roles.
- `linux-apfs-rw` exposes mount options to verify metadata node checksums and
  skips corrupted checkpoint superblocks during mount.
- `go-apfs` surfaces object header checksum, oid, xid, type, subtype, and flags
  in its debug path and returns checksum errors during object reads.

### Hypothesis

- The native parser should have one object validation function used by every
  resolver call. Ad hoc per-object validation invites accidental best-effort
  parsing.

## Open Limits

- Some APFS objects are headerless or zero-header OMAP values; those need
  explicit type-specific support before parsing.
- Physical-object `o_oid` equality to block address should be recorded first and
  promoted to a hard check only after real-image probes confirm no exception in
  the supported matrix.
- Multi-block object checksum handling is not yet implemented.

## Decision impact

- `RL-02`: resolved objects must carry validation status, not just paddr.
- `RL-13`: unexpected type/subtype, checksum mismatch, newer-than-scan XID, and
  unsupported zero-header objects are fallback triggers.
- Exact next step: wire checksum/type validation into the Rust checkpoint
  scanner now; add OID-vs-paddr and multi-block validation probes before making
  those checks universal.
