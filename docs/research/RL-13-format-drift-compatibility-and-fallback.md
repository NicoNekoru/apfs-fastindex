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
- Some APFS internals may vary or be insufficiently documented.
- A hybrid strategy may be needed to remain reliable across environments.

## Known Facts
- The spec surface we rely on is complex and not equivalent to stable public APIs.
- Reverse-engineered details may drift or vary by OS release.

## Unknowns / Open Questions
- Which parser assumptions are version-sensitive?
- Which environments are likely to break raw parsing first?
- What runtime checks can detect unsupported states?
- When should the tool fall back to:
  - POSIX traversal
  - bulk attribute APIs
  - snapshot-assisted scanning
- How do we communicate degraded mode to users?

## Risks if We Get This Wrong
- Brittle behavior after OS updates.
- Support burden from edge-case machines.
- Incorrect results on unsupported variants.

## Planned Experiments / Demos
1. Test across multiple macOS/APFS versions.
2. Test on case-sensitive and case-insensitive volumes.
3. Test on encrypted and unencrypted volumes.
4. Record parser assumptions that differ across versions.

## Evidence Log
- [TBD] Version compatibility notes.
- [TBD] Unsupported-state detection notes.
- [TBD] Fallback design notes.

## Interim Decisions
- Compatibility boundaries must be explicit, not implied.

## Exit Criteria
- Supported-version matrix exists.
- Fallback triggers are defined.
- User-facing degraded-mode behavior is specified.

## Related Logs
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-12 Performance Model and Optimization