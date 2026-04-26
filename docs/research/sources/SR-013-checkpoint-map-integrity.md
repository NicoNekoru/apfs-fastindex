# SR-013 Checkpoint Map Integrity

Status: Complete
Date: 2026-04-26
Type: Source Review
Related RLs:
- RL-01
- RL-02
- RL-13

## Bottom line

Checkpoint selection is not complete when the parser finds the highest
checksum-valid `NXSB`. Before that state can feed OMAP/root discovery, the parser
must walk the selected checkpoint's checkpoint-map chain, validate each
`checkpoint_map_phys_t`, enumerate every `checkpoint_mapping_t`, read mapped
ephemeral objects from the checkpoint data area, and validate those objects.

This review answers one question: what additional checkpoint-map validation is
required after candidate `NXSB` selection and before OMAP/root traversal?

## Evidence

### Spec

- `Spec | Apple File System Reference:` the latest valid checkpoint consists of
  the selected container superblock and its checkpoint-mapping blocks. The
  selected superblock's descriptor/data fields identify which mapping blocks and
  ephemeral objects belong to that checkpoint.
- `Spec | Apple File System Reference:` descriptor-area entries are
  `nx_superblock_t` or `checkpoint_map_phys_t`; checkpoint mappings describe
  ephemeral objects stored in the checkpoint data area.

### Observation

- `Observation | reverse engineering writeup:` JT Sylve describes
  `checkpoint_map_phys_t` as `obj_phys_t cpm_o`, `cpm_flags`, `cpm_count`, and
  an array of `checkpoint_mapping_t`. `CHECKPOINT_MAP_LAST` marks the final
  checkpoint-map object for the selected checkpoint.
- `Observation | reverse engineering writeup:` the selected checkpoint's
  `nx_xp_desc_index` is the first checkpoint-map object's zero-based index in
  the descriptor area. Additional map objects are found by advancing through the
  descriptor area as a circular buffer until `CHECKPOINT_MAP_LAST`.
- `Observation | reverse engineering writeup:` `checkpoint_mapping_t` records
  object type, subtype, size, filesystem OID, ephemeral object OID, and physical
  block address in the checkpoint data area.
- `Observation | open-source implementation:` `linux-apfs-rw` reads ephemeral
  objects from the checkpoint data ring, validates multiblock checksums on
  mount, rejects invalid object sizes, and imposes hard limits so descriptor and
  data rings cannot silently wrap forever.
- `Observation | open-source implementation:` both `linux-apfs-rw` and `go-apfs`
  reject unsupported non-contiguous checkpoint descriptor layouts rather than
  guessing.

### Hypothesis

- Candidate checkpoint discovery can remain a preliminary source-gate output,
  but native root discovery must require a stronger `ValidatedCheckpoint` object
  that includes validated checkpoint-map entries and loaded/validated ephemeral
  objects.
- Non-contiguous descriptor layouts require a separate range-map parser before
  checkpoint-map validation can be attempted safely.

## Open Limits

- The repo has not yet captured a real artifact with a non-contiguous checkpoint
  descriptor area.
- The exact set of ephemeral object types required before container OMAP
  traversal still needs a probe.
- Recovery behavior for a bad latest checkpoint, such as falling back to the
  next-highest valid checkpoint, needs a separate damaged-image oracle before it
  can become product behavior.
- Multiblock ephemeral object support needs real-media evidence before a limit is
  chosen.

## Decision impact

- `RL-01`: split checkpoint work into two gates: candidate `NXSB` selection and
  checkpoint-map/ephemeral-object validation.
- `RL-02`: native OMAP/root resolution must not consume a candidate `scan_xid`
  until checkpoint-map validation has produced the ephemeral object context for
  that selected checkpoint.
- `RL-13`: unsupported non-contiguous descriptor layouts, invalid checkpoint-map
  chains, missing `CHECKPOINT_MAP_LAST`, impossible `cpm_count`, invalid
  mapped-object size, or ephemeral-object checksum failure are fallback triggers.
- Exact next step: add `EX-11` to design a checkpoint-map integrity probe over
  detached proof images and synthetic malformed descriptor/data rings.
