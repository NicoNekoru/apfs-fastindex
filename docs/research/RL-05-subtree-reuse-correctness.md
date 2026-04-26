# RL-05 Subtree Reuse Correctness

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-04-26

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
- [2026-04-25] `EX-07` was designed from `EX-06` identity results as a
  falsifiable subtree-reuse probe. It will test reuse only when a parent child
  pointer resolves to the same child tuple: OMAP domain, OID, object XID, paddr,
  checksum/hash, and type/subtype.
- [2026-04-26] `EX-07` executed on a detached 384 MiB image-backed corpus with
  separated `stable-a/`, `stable-b/`, `hot/`, and `moved/` subtrees. Exact
  node-identity reuse produced zero false reuse across append, rename, move,
  delete/recreate, fanout growth, and fanout deletion transitions. Reusable
  current-node fractions ranged from `65.9%` to `90.3%`.
- [2026-04-26] `EX-12` was designed to prove the native OMAP lookup step that
  produces those node identities. Subtree reuse must remain downstream of native
  `(omap context, oid, selected_xid)` validation.
- [2026-04-26] Observation: the first `EX-12` route was blocked by missing raw
  media for the `EX-06`/`EX-07` identity artifacts. That is now historical for
  `EX-12`, which executed on a self-paired fixture, but the subtree-reuse proof
  still remains a proof-backend result until the same mutation grid can be
  replayed through native OMAP lookup and native FS-record body parsing.
- [2026-04-26] `EX-12` executed end-to-end on a self-paired raw image plus
  identity oracle and produced verdict `validated_omap_lookup_contract`.
  The OMAP lookup step that supplies node identities (OMAP domain, OID,
  object XID, paddr) is therefore validated for the proof fixture under
  `selected_xid = container.xid`. The subtree-reuse theorem is still
  pending: it requires native FS-record body decoding and a multi-state
  rerun of the EX-07 mutation grid through the native parser before any
  reuse claim is allowed in production.

## Interim Decisions
- Reuse should be proven at the node/subtree level before being trusted in production.
- The candidate theorem is scoped only to raw single-volume namespace plus
  logical size. Physical/shared/snapshot accounting requires a separate theorem.
- Any changed identity field, unsupported side metadata, parser-version change,
  or unproven metric dependency should force descent or full reparse.
- Keep exact node identity reuse alive as a post-v1 architecture candidate. The
  only currently supported candidate identity includes OMAP domain, OID, object
  XID, paddr, checksum/hash, and type/subtype; weaker identity tuples remain
  rejected.
- Do not implement persistent subtree skipping yet. The positive `EX-07` result
  must be rerun after native root/FS-record parsing exists.
- Treat `EX-07` identities as proof-backend evidence until a regenerated
  subtree/identity experiment preserves raw media and native parsing reproduces
  the mutation states. `EX-12` validates the OMAP lookup contract on a
  self-paired fixture, not the `EX-07` reuse theorem.
- Preserve raw identity media in the next subtree/identity experiment so native
  resolver replay can be compared against the same states.

## Exit Criteria
- A precise reuse theorem for our implementation.
- A list of structural changes that preserve reuse vs force descent.
- A validated algorithm for skipping unchanged subtrees.

## Related Logs
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-07 Size and Space Accounting
- RL-09 Cache Persistence and Invalidation