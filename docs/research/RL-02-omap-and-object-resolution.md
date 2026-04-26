# RL-02 OMAP and Object Resolution

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-04-26

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
- [2026-04-26] `SR-005` reinforced that object resolution is both
  OMAP-domain-specific and XID-aware. Container and volume OMAPs have separate
  virtual address spaces, and active-state lookup must choose the highest usable
  mapping for the requested object within the selected scan XID rather than a
  bare latest mapping.
- [2026-04-26] `SR-006` narrowed the native resolver contract to
  `(omap context, oid, max_xid or snapshot_xid)` and identified domain ambiguity,
  newer-than-scan mappings, encrypted OMAP values, and resolved-object
  validation failures as hard stops.
- [2026-04-26] `SR-007` made object-header validation a resolver prerequisite:
  checksum, expected type/subtype, and object XID relative to the selected scan
  state must be checked before an object body is trusted.
- [2026-04-26] `SR-013` clarified that native OMAP/root resolution also depends
  on checkpoint-map validation. A highest-candidate `NXSB` is not enough input
  for a resolver until the selected checkpoint's ephemeral-object context is
  validated.
- [2026-04-26] `SR-006` was tightened further: lookup should seek
  `(oid, selected_xid)` and accept the greatest matching key with
  `xid <= selected_xid`; `OMAP_VAL_DELETED` is a negative lookup result, while
  encrypted/no-header/crypto-generation flags are unsupported hard stops.
- [2026-04-26] `EX-12` was designed to compare native OMAP lookup output against
  pinned identity artifacts from `EX-06` and `EX-07`, after `EX-11` supplies a
  validated checkpoint context.
- [2026-04-26] `EX-11` executed and produced a validated checkpoint context for a
  generated detached proof fixture, but `EX-12` was blocked because the
  `EX-06`/`EX-07` raw images that produced the pinned identity oracles were not
  preserved.
- [2026-04-26] `EX-10` extended the Rust path with a native OMAP resolver
  (`crates/apfs-fastindex/src/omap.rs`). The resolver opens an `omap_phys_t`,
  walks the embedded B-tree, and performs the `(oid, max_xid)` lower-bound
  lookup that `SR-006` requires. On the proof fixture it resolves volume OID
  `1026` to paddr `439` at xid `14` from the container OMAP, then opens the
  volume OMAP and resolves FS-tree root virtual OID `1028` to paddr `433` at
  xid `13`. `OMAP_VAL_ENCRYPTED` and `OMAP_VAL_NOHEADER` values force a hard
  stop and `OMAP_VAL_DELETED` returns a negative lookup, all covered by
  in-memory unit tests against a synthetic OMAP leaf. An empirical correction
  was distilled into the code: the value-area offset stored in B-tree TOC
  entries is the distance from the **end** of the value area to the
  **start** of the value (values run forward), which fixed an early misread of
  `flags=1114111` on real fixture data.

## Interim Decisions
- Do not assume `OID` alone is a sufficient cache identity until proven.
- The first parser should define resolver inputs explicitly before any persistent
  cache design is attempted.
- V1 resolver validation must fail closed on checksum, OID, XID, or expected
  type mismatches.
- Incremental identity observations should record OMAP domain, OID, object XID,
  paddr, checksum/hash, and scan-state context until a cheaper tuple is proven.
- Native parser work must not wire namespace traversal to candidate checkpoint
  discovery alone. It needs checkpoint-map validation plus container OMAP,
  volume OMAP, and expected object type/subtype validation before resolved
  roots can be trusted.
- Native OMAP parsing should not begin as a global lookup table. The resolver API
  must require the owning container or volume OMAP domain and selected scan-state
  XID.
- OMAP snapshot lookup and encrypted OMAP values are deferred until their source
  and oracle semantics are probed.
- The next resolver experiment should consume a `validated_checkpoint_context`
  verdict from `EX-11`, not the preliminary `EX-10` checkpoint-candidate output.
- Deleted mappings should be treated as absent at the selected scan state. Do not
  walk backward to an older mapping unless a separate recovery mode and oracle
  define that behavior.
- OMAP validation requires the raw media and identity oracle to be paired. Stale
  JSON identities cannot validate native lookup on a different generated image.

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