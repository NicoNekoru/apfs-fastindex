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

## Known Facts
- APFS uses OMAP structures to resolve object identifiers.
- Object graph traversal requires object resolution rather than direct linear metadata walking.

## Unknowns / Open Questions
- Is object resolution strictly XID-scoped in the way we need?
- Can an OID remain stable while its physical address changes?
- Can physical blocks be reused in ways that break simplistic cache assumptions?
- What object header/type checks must be performed after resolution?
- Do we need per-XID caching of OMAP-derived lookups?

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

## Interim Decisions
- Do not assume `OID` alone is a sufficient cache identity until proven.

## Exit Criteria
- Documented resolver contract: input, output, validation steps.
- Proven rules for detecting whether a resolved object is unchanged.
- Decision on whether cache keys require OID only, OID+block, or a stronger tuple.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness