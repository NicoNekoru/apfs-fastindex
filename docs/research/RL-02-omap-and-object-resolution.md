# RL-02 OMAP and Object Resolution

Status: Open
Priority: P0
Owner: TBD
Last Updated: TBD

## Core Question
- How do OID-to-physical-address mappings behave across XIDs?
- What exact object-resolution contract do we rely on during full and incremental scans?

## Why This Matters
- Every raw APFS traversal depends on correct object resolution.
- Incremental caching assumes object identity can be tracked across scans.

## Current Assumptions
- OMAP resolves logical object identity to physical storage for a chosen transaction view.
- Changes in objects should appear as changed mappings and/or changed object contents.
- Container and volume OMAPs should be treated as separate resolution domains.

## Known Facts
- APFS uses OMAP structures to resolve object identifiers.
- Object graph traversal requires object resolution rather than direct linear metadata walking.
- Resolver correctness depends on both transaction context and the correct OMAP
  ownership domain.

## Unknowns / Open Questions
- Is object resolution strictly XID-scoped in the way we need?
- Can an OID remain stable while its physical address changes?
- Can physical blocks be reused in ways that break simplistic cache assumptions?
- What object header/type checks must be performed after resolution?
- Do we need per-XID caching of OMAP-derived lookups?
- What is the minimal resolver contract we can encode into the first parser
  without overcommitting to future cache design?

## Risks if We Get This Wrong
- Reading wrong objects.
- False positives for "unchanged" nodes.
- Cache poisoning across scans.

## Planned Experiments / Demos
1. Track a known object across multiple transactions after metadata-only updates.
2. Track the same object after content growth/shrink and observe mapping changes.
3. Delete objects and create new ones to test OID and block reuse behavior.
4. Diff OMAP state across low-churn and high-churn workloads.

## Evidence Log
- [TBD] OMAP lookup behavior notes.
- [TBD] OID stability observations.
- [TBD] Physical block reuse observations.
- [2026-04-24] `SR-002` summarized the current resolver contract as
  `OMAP context + oid + scan-state context`, and tied root discovery to
  container-then-volume OMAP resolution rather than a global `oid -> paddr`
  model.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` turned that into an
  implementation rule set: resolver input must include OMAP context and chosen
  scan state, and resolved objects must pass checksum, header, and type/subtype
  validation before use.
- [2026-04-25] `EX-06` captured raw identity traces across eight pinned mutation
  states. The FS root tree OID stayed stable while resolved paddr, object XID,
  checksum, and block hash changed after every mutation, reinforcing that OMAP
  resolution output must carry version/content identity, not just OID.

## Interim Decisions
- Do not assume `OID` alone is a sufficient cache identity until proven.
- The first parser should define resolver inputs explicitly before any persistent
  cache design is attempted.
- V1 resolver validation must fail closed on checksum, OID, XID, or expected
  type mismatches.
- Incremental identity observations should record OMAP domain, OID, object XID,
  paddr, checksum/hash, and scan-state context until a cheaper tuple is proven.

## Exit Criteria
- Documented resolver contract: input, output, validation steps.
- Proven rules for detecting whether a resolved object is unchanged.
- Decision on whether cache keys require OID only, OID+block, or a stronger tuple.
- Mode-specific rules for when additional trees or resolution domains are
  required.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness