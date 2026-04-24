# RL-06 Namespace Reconstruction

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

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
- [2026-04-24] `EX-03` showed that rename and move preserve inode identity while
  changing path placement, that hard links immediately separate path identity
  from inode identity, and that case-sensitive versus case-insensitive APFS
  volumes diverge in visible name behavior.

## Interim Decisions
- Keep namespace reconstruction separate from storage traversal logic.
- Hard-link handling and case semantics belong in the core namespace design, not
  in a later polish pass.

## Exit Criteria
- Exact reconstruction algorithm for paths and parent-child graph.
- Rules for case and Unicode handling.
- A clear policy for hard links and any non-tree relationships.

## Related Logs
- RL-03 FS Tree Topology and Required Records
- RL-07 Size and Space Accounting
- RL-11 Snapshots, Volume Groups, and Firmlinks