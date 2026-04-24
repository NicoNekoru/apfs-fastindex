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

## Interim Decisions
- Compatibility boundaries must be explicit, not implied.
- V1 raw mode should prefer an allowlist over a broad claim of general APFS
  support.
- Unsupported states should trigger fallback, not degraded best-effort parsing.

## Exit Criteria
- Supported-version matrix exists.
- Fallback triggers are defined.
- User-facing degraded-mode behavior is specified.
- The repo contains a concrete raw-mode allowlist and a concrete hard-stop list.

## Related Logs
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-12 Performance Model and Optimization