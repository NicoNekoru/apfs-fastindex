# RL-09 Cache Persistence and Invalidation

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-04-26

## Core Question
- How do we store index/cache state across runs, and when must that state be invalidated?

## Why This Matters
- Incremental speed depends on persistent cache reuse.
- Bad invalidation logic turns a fast indexer into a wrong indexer.

## Current Assumptions
- Cache should persist at least:
  - last scan XID
  - node cache
  - inode cache
  - directory cache
- Cache must be versioned and tied to a specific volume identity.

## Known Facts
- Repeated scans are the main target optimization.
- Any uncertainty in scan consistency or identity must invalidate reuse conservatively.

## Unknowns / Open Questions
- What is the canonical volume identity for cache binding?
- How should cache entries be versioned across parser changes?
- What invalidates cache:
  - unclean shutdown
  - incompatible macOS/APFS version
  - missing checkpoint continuity
  - block reuse uncertainty
- What on-disk cache format is best?
- How do we make cache writes crash-safe?

## Risks if We Get This Wrong
- Stale results survive across runs.
- Cache corruption after process crash.
- Hidden incompatibility after software updates.

## Planned Experiments / Demos
1. Kill process during cache write and verify recovery behavior.
2. Reboot between scans and verify continuity assumptions.
3. Resize/modify volume and test cache binding behavior.
4. Roll back to an earlier snapshot and test invalidation.

## Evidence Log
- [TBD] Cache key design notes.
- [TBD] Crash-safety notes.
- [TBD] Invalidation trigger notes.
- [2026-04-25] `EX-06` provided the first cache-key evidence: root tree OID was
  stable while object XID, paddr, checksum, and block hash changed across each
  mutation. Any persistent cache that cannot prove continuity for those fields
  should invalidate rather than reuse.
- [2026-04-25] `EX-07` defined the next incremental oracle: simulate reuse
  decisions between two pinned raw states and compare the result to a fresh full
  raw summary of the same state.
- [2026-04-26] `EX-07` executed that oracle at node-summary granularity. Exact
  node-key matches had zero summary-hash mismatches across six adjacent pinned
  states, while changed or missing node keys forced descent/reparse. This
  supports separating the reuse-decision proof from the later persistent cache
  storage format.
- [2026-04-26] `EX-12` was designed as a prerequisite for native cache identity:
  prove native OMAP lookup returns the same mapped object identities as the
  pinned proof artifacts before any persistent cache consumes those identities.
- [2026-04-26] `EX-12` was blocked because the raw media for the pinned identity
  artifacts was not preserved. Persistent cache identity remains unproven for
  native lookup.

## Interim Decisions
- Prefer conservative invalidation over aggressive reuse.
- Persistent cache design should wait for subtree-reuse proof, but current
  invalidation inputs must include parser version, volume/source identity,
  scan-state continuity, OMAP domain, object XID, paddr, and checksum/hash.
- Incremental correctness should be validated independently from cache storage
  format; first prove reuse decisions, then design persistence.
- A future persistent cache entry for parsed node summaries must include parser
  version and summary schema version in addition to the exact raw node identity.
  If any field is absent, changed, or unvalidated, reuse is forbidden.
- Cache writes remain out of scope until checkpoint-map validation and native
  OMAP lookup both have executed proof artifacts.
- Cache validation artifacts must pair identity JSON with replayable raw media;
  otherwise they can document behavior but cannot validate native reuse.

## Exit Criteria
- Cache schema defined.
- Invalidation rules documented and tested.
- Crash-safe persistence strategy chosen.

## Related Logs
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness
- RL-10 Validation Corpus and Oracle