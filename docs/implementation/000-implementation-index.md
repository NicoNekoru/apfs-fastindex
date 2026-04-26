# Implementation Index

Status: Active
Date: 2026-04-26

This directory contains implementation-facing specs only when the corresponding
research direction is resolved tightly enough to describe reproducible behavior
without turning hypotheses into design commitments.

## Specs

- `narrow-v1-proof-parser-skeleton.md`: current `src/apfs_fastindex`
  proof-backed parser skeleton. This documents the resolved runnable boundary,
  validation command, module contracts, current limitations, and replacement
  path toward native APFS parsing.

## Not Yet Specified

The following topics intentionally remain in research docs until experiments or
benchmarks close their proof gates:

- native OMAP/root/FS-record parser internals
- live mounted raw scanning
- subtree reuse and persistent incremental cache
- physical, shared, exclusive, compression, and snapshot-retained accounting
- Finder-visible merged boot-root semantics
- production performance optimizations
