# RL-13 Format Drift, Compatibility, and Fallback

Status: Open
Priority: P1
Owner: TBD
Last Updated: TBD

## Core Question
- How stable is this raw-parser approach across APFS/macOS versions, and when should the product fall back to supported APIs?

## Why This Matters
- Raw parsing can be fast and powerful, but it carries long-term maintenance risk.
- A commercial or broadly distributed tool needs clear support boundaries.

## Current Assumptions
- Some APFS internals vary enough that raw mode must be gated by an explicit
  support matrix rather than optimistic best effort.
- A hybrid strategy is likely necessary for broad deployment.
- Compatibility boundaries should be narrow first and expanded only after
  evidence, not declared in advance.

## Known Facts
- The spec surface we rely on is complex and not equivalent to stable public APIs.
- Reverse-engineered details may drift or vary by OS release.
- Third-party APFS parsers openly document feature gaps and version ceilings.
- Sealed-volume, snapshot, compression, block-size, and special-tree handling
  are recurring compatibility fault lines.

## Unknowns / Open Questions
- Which parser assumptions are version-sensitive?
- Which environments are likely to break raw parsing first?
- What runtime checks can detect unsupported states?
- When should the tool fall back to:
  - POSIX traversal
  - bulk attribute APIs
  - snapshot-assisted scanning
- How do we communicate degraded mode to users?
- Which unsupported states should be treated as immediate hard-stop conditions in
  v1 instead of soft warnings?

## Risks if We Get This Wrong
- Brittle behavior after OS updates.
- Support burden from edge-case machines.
- Incorrect results on unsupported variants.
- False user confidence because raw mode appears available in environments that
  are outside the tested matrix.

## Planned Experiments / Demos
1. Test across multiple macOS/APFS versions.
2. Test on case-sensitive and case-insensitive volumes.
3. Test on encrypted and unencrypted volumes.
4. Record parser assumptions that differ across versions.

## Evidence Log
- [TBD] Version compatibility notes.
- [TBD] Unsupported-state detection notes.
- [TBD] Fallback design notes.
- [2026-04-24] `SR-001` established the initial direction: narrow raw-mode
  allowlist, fail closed on unsupported states, and treat live startup-disk raw
  parsing as unsupported until proven.
- [2026-04-24] `SR-002` identified root-discovery and resolver-level hard-stop
  conditions such as malformed checkpoints, unsupported checkpoint layouts, and
  unexpected object type/subtype results during OMAP and root traversal.
- [2026-04-24] `EX-01` added a runtime hard-stop data point: the live startup
  container on this host was not raw-readable without elevated privilege, while
  a mounted APFS image-backed lab container was probeable and showed a moving
  latest checkpoint under write churn.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` carried those source and probe
  results into the implementation boundary: raw mode stops on unsupported
  checkpoint layout, ambiguous OMAP context, unexpected object typing, or
  environments that require unsupported encryption, snapshot, or boot-root
  semantics.
- [2026-04-24] `EX-03` confirmed that a detached image-backed APFS container is
  inside the current allowlist: in that environment the raw walk matched the
  mounted oracle exactly for the narrow v1 fields.
- [2026-04-25] `EX-04` kept detached image-backed APFS inside the allowlist
  across both case-insensitive and case-sensitive variants, but did not broaden
  that support claim to live startup disks, encrypted media, snapshots, or
  unsupported feature sets.
- [2026-04-25] `SR-004` defined the current support-matrix draft: raw mode is
  allowlisted for detached, unencrypted, image-backed or otherwise explicitly
  stable sources; unsupported feature bits, encryption requirements, Fusion,
  snapshot/sealed-volume semantics, merged-root requests, and unvalidated APFS
  variants are hard stops or fallback triggers.

## Interim Decisions
- Compatibility boundaries must be explicit, not implied.
- V1 raw mode should prefer an allowlist over a broad claim of general APFS
  support.
- Unsupported states should trigger fallback, not degraded best-effort parsing.
- The first parser prototype should encode its hard-stop list directly from the
  narrow parser contract rather than infer it ad hoc at runtime.
- Feature-bit and environment checks belong at the source gate before traversal,
  not after partial parser output has already been produced.

## Exit Criteria
- Supported-version matrix exists.
- Fallback triggers are defined.
- User-facing degraded-mode behavior is specified.
- The repo contains a concrete raw-mode allowlist and a concrete hard-stop list.

## Related Logs
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-12 Performance Model and Optimization