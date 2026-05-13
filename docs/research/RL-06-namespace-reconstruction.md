# RL-06 Namespace Reconstruction

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-05-13

## Core Question
- How do we reconstruct an exact directory tree and stable parent/child relationships from APFS metadata?

## Why This Matters
- The product output is not just a flat inode list; it is a browsable directory tree with aggregates.
- Namespace mistakes are highly visible to users.

## Current Assumptions
- Directory entry records and inode records are sufficient to reconstruct hierarchy.
- A post-processing pass can build the logical tree after storage-level traversal.

## Known Facts
- APFS stores metadata in B-trees rather than as direct directory walking output.
- Directory hierarchy reconstruction is a separate concern from raw tree traversal.

## Unknowns / Open Questions
- What exact record relationships define parent-child links?
- How are names encoded and normalized?
- How do case-sensitive vs case-insensitive volumes change key semantics?
- How should hard links appear in the directory tree?
- What happens with orphaned or inconsistent records?
- Are there special namespace constructs that alter user-visible layout?

## Risks if We Get This Wrong
- Missing or duplicate files.
- Incorrect paths.
- Mismatches against Finder or POSIX traversal.
- Broken aggregate rollups.

## Planned Experiments / Demos
1. Create directories/files with Unicode-normalization edge cases.
2. Compare behavior on case-sensitive and case-insensitive APFS volumes.
3. Create hard-linked files and observe representation in metadata.
4. Rename/move directories and validate parent linkage changes.

## Evidence Log
- [TBD] Directory-entry schema notes.
- [TBD] Unicode/name handling notes.
- [TBD] Hard-link observations.
- [2026-04-24] `EX-02` showed that rename and move preserve inode identity while
  changing path placement, that hard links immediately separate path identity
  from inode identity, and that case-sensitive versus case-insensitive APFS
  volumes diverge in visible name behavior.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` fixed the current v1 namespace
  policy: directory membership comes from directory records, hard links surface
  shared file identity, symlinks are emitted as symlink nodes and are not
  traversed, and case behavior follows the volume mode.
- [2026-04-24] `EX-03` proved that this namespace policy is achievable on a
  pinned raw state in the tested allowlist: raw output matched the mounted
  oracle for path set, entry type, shared file identity across hard links, and
  symlink target fidelity.
- [2026-04-25] `SR-003` reinforced the namespace contract from outside sources:
  directory records define hierarchy placement and visible names, inode records
  define file identity and link count, and hard links require keeping path
  graph and file identity as separate concepts.
- [2026-04-25] `EX-04` broadened the raw-vs-oracle namespace proof to
  case-insensitive and case-sensitive images with cross-directory hard links,
  symlinks, Unicode names, case-collision behavior, and a FIFO; both images
  matched the mounted oracle with zero path or field mismatches.
- [2026-04-26] Spec/Observation: `SR-014` defines the native field inputs for
  namespace rows. Directory placement comes from `DIR_REC` key parent identity,
  key name, and `j_drec_val.file_id`; entry type comes from `DIR_REC.flags` and
  inode `mode`; symlink target comes from the `com.apple.fs.symlink` xattr; hard
  links require `DREC_EXT_TYPE_SIBLING_ID`, `SIBLING_LINK`, and `SIBLING_MAP` so
  path identity and file identity are not collapsed.
- [2026-04-26] Hypothesis: `EX-13` should compare native field dumps to a
  same-run mounted/POSIX namespace oracle before any Rust output is promoted to
  product namespace rows.
- [2026-04-26] Observation: Python `EX-13` reconstructed the proof fixture path
  set from raw `DIR_REC`/`INODE`/`XATTR`/sibling records with `0` missing paths,
  `0` unexpected paths, and `0` path/type/file-identity mismatches across `8`
  mounted/POSIX entries. After xfield layout candidates were recorded and scored,
  the same field dump also matched logical-size rows, so `EX-13` validates the
  proof-fixture namespace+ordinary-logical-size body contract.
- [2026-05-13] Observation: `EX-14` did not produce a namespace comparison for
  the xfield-layout variant. The mounted/POSIX oracle was saved for `16` entries,
  but Rust did not publish `selected_checkpoint` or FS-tree root context because
  native validation aborted with `checksum mismatch at block 1031`. Namespace
  reconstruction for the variant remains untested rather than failed.
- [2026-05-13] Spec/Observation: `SR-018` fixes v1 name policy: emit stored
  UTF-8 spelling exactly, record case/normalization volume modes, and defer
  APFS lookup/hash claims until a dedicated name-hash fixture exists.
- [2026-05-13] Spec/Observation: `SR-016` makes malformed names, missing
  embedded symlink targets, sibling-link/map inconsistency, and drec/inode type
  mismatch fail-closed namespace gates.

## Interim Decisions
- Keep namespace reconstruction separate from storage traversal logic.
- Hard-link handling and case semantics belong in the core namespace design, not
  in a later polish pass.
- V1 namespace correctness requires both a path graph and stable file identity;
  the parser must not collapse those into one concept.
- Symlink target extraction belongs in the active v1 namespace surface for the
  tested allowlist, but future environments that break the known xattr path
  should fail closed until a targeted probe explains them.
- Native namespace reconstruction must remain a two-step proof: first validate
  record-body fields in `EX-13`, then assemble product rows and aggregates. A
  family-count dump alone is not namespace evidence.
- Path reconstruction evidence should continue in Python fixture variants before
  Rust product rows are added, especially around xfield layout and hard-link
  record combinations.
- Product `NamespaceEntry` emission remains behind two gates: first isolate the
  `EX-14` checkpoint-context failure, then rerun the Python xfield-layout
  variant and require a same-run namespace/logical-size diff.
- Product paths must not be normalized or case-folded during row emission; any
  normalization/case behavior belongs to a later lookup/search gate.

## Exit Criteria
- Exact reconstruction algorithm for paths and parent-child graph.
- Rules for case and Unicode handling.
- A clear policy for hard links and any non-tree relationships.

## Related Logs
- RL-03 FS Tree Topology and Required Records
- RL-07 Size and Space Accounting
- RL-11 Snapshots, Volume Groups, and Firmlinks
