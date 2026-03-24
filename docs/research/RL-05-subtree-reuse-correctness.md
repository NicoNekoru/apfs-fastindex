# RL-05 Subtree Reuse Correctness

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

## Core Question
- Under what exact conditions does "unchanged node => unchanged subtree" hold in APFS in a way safe for indexing?

## Why This Matters
- This is the central optimization premise of the entire design.
- If this premise is wrong or incomplete, repeat-scan speedups may be invalid.

## Current Assumptions
- APFS copy-on-write means changes rewrite the leaf and ancestors up to the root.
- Unchanged subtrees should therefore remain structurally identical.

## Known Facts
- Persistent-tree logic suggests subtree reuse should be possible.
- But APFS-specific details still need proof at the implementation level.

## Unknowns / Open Questions
- Is subtree identity determined solely by parent child-pointers and node identity?
- Can balancing, splitting, or merging affect reuse granularity?
- Can a logically unchanged subtree become physically relocated?
- Can metadata outside the subtree affect subtree summaries we intend to reuse?
- What is the smallest safe unit of reuse:
  - node
  - child pointer target
  - parsed record set
  - computed summary

## Risks if We Get This Wrong
- Skipping modified data.
- Reusing stale aggregate sizes.
- Incorrect incremental results that look plausible.

## Planned Experiments / Demos
1. Rename a file within a directory and across directories; inspect affected nodes.
2. Append to one large file; determine how far structural rewrites propagate.
3. Cause B-tree rebalance conditions on small synthetic volumes.
4. Compare subtree summaries before/after targeted mutations.

## Evidence Log
- [TBD] Rename propagation notes.
- [TBD] File growth propagation notes.
- [TBD] Rebalance observations.

## Interim Decisions
- Reuse should be proven at the node/subtree level before being trusted in production.

## Exit Criteria
- A precise reuse theorem for our implementation.
- A list of structural changes that preserve reuse vs force descent.
- A validated algorithm for skipping unchanged subtrees.

## Related Logs
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-07 Size and Space Accounting
- RL-09 Cache Persistence and Invalidation