# RL-04 Node Identity, Cache Keys, and OID Reuse

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-04-26

## Core Question
- What constitutes a stable identity for a B-tree node or parsed object in cache?
- Is `node_cache[oid]` sufficient, or do we need a stronger key?

## Why This Matters
- The proposed incremental strategy currently assumes node identity can be keyed simply and safely.
- This is one of the highest-risk assumptions in the spec.

## Current Assumptions
- Unchanged APFS nodes keep stable identity under copy-on-write.
- A changed node will have detectable identity or location changes.

## Known Facts
- APFS is copy-on-write.
- Physical location may be relevant, but stability and reuse rules are not yet fully proven.

## Unknowns / Open Questions
- Are OIDs ever reused after deletion?
- Can a node’s OID remain the same while content changes?
- Is physical block address stable enough to use as part of identity?
- Do we need object header fields, checksum, type, or XID in the cache key?
- What is the cheapest identity that is still safe?

## Risks if We Get This Wrong
- Reusing stale data as if current.
- Missing changed subtrees.
- Persistent false correctness in repeat scans.

## Planned Experiments / Demos
1. Repeated create/delete cycles to probe OID reuse.
2. Small edits causing known node rewrites; track OID vs block changes.
3. Long-run churn test to observe physical block reuse.
4. Validate whether a checksum or subtree fingerprint is needed.

## Evidence Log
- [TBD] Node identity observations.
- [TBD] OID reuse observations.
- [TBD] Block reuse observations.
- [2026-04-25] `EX-06` ruled out bare `oid` as a safe parsed-object cache key
  in the tested corpus: the FS root tree OID remained `1028` across all
  mutations, while paddr, object XID, checksum, and block hash changed at every
  step. Delete/recreate of `work/beta.txt` produced a new file id in this short
  run, but that does not prove file-id reuse impossible.
- [2026-04-26] `EX-07` tested the stronger identity tuple against per-node
  summary hashes. Exact node keys had zero false reuse in the detached lab
  corpus; weaker keys were not promoted. The observed safe candidate remains an
  exact tuple, not a reduced or performance-optimized key.
- [2026-04-26] `SR-006` and `EX-12` moved native identity evidence behind an
  explicit OMAP lookup contract. Cache identity must consume the mapping actually
  returned for `(omap domain, oid, selected_xid)`, including OMAP value flags,
  not a guessed latest mapping.
- [2026-04-26] `EX-12` was blocked because the raw images corresponding to
  `EX-06`/`EX-07` identity JSON were not preserved. Native cache identity proof
  therefore still needs regenerated or preserved raw media.

## Interim Decisions
- Cache identity should remain conservative until reuse safety is demonstrated.
- A candidate raw object identity for future probes is at least:
  `omap domain + oid + object_xid + paddr + checksum/hash + scan state`.
- File identity and B-tree node identity must remain separate concepts.
- For future subtree summary reuse, include parser version, summary schema, and
  expected type/subtype with the raw identity tuple. Do not collapse the key to
  OID, paddr, or object XID alone for performance.
- Do not promote any identity tuple from `EX-06`/`EX-07` into native cache code
  until `EX-12` proves the native resolver returns the same identities.
- Future identity experiments must preserve the raw source or a reproducible
  fixture recipe that can regenerate the exact compared state.

## Exit Criteria
- Final cache-key definition for node cache.
- Documented invalidation rules.
- Proof that chosen key does not permit unsafe reuse under tested conditions.

## Related Logs
- RL-02 OMAP and Object Resolution
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation