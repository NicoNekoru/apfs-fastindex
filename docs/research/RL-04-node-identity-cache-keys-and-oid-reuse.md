# RL-04 Node Identity, Cache Keys, and OID Reuse

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

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

## Interim Decisions
- Cache identity should remain conservative until reuse safety is demonstrated.

## Exit Criteria
- Final cache-key definition for node cache.
- Documented invalidation rules.
- Proof that chosen key does not permit unsafe reuse under tested conditions.

## Related Logs
- RL-02 OMAP and Object Resolution
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation