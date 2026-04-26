# RL-07 Size and Space Accounting

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-04-26

## Core Question
- What size metrics will the product report, and how can those metrics be computed correctly on APFS?

## Why This Matters
- "Size" is not a single concept on APFS.
- A WizTree-like tool lives or dies by whether its size numbers make sense.

## Current Assumptions
- Logical file size is easier than physical allocated size.
- Physical accounting may require extent-level analysis.
- Clones, sparse files, and compression complicate "size on disk."

## Known Facts
- APFS supports copy-on-write sharing and modern allocation features.
- Shared storage means naive subtree summation may overcount.

## Unknowns / Open Questions
- Which metrics should v1 support?
  - logical size
  - physical allocated size
  - exclusive size
  - shared size
- How do clones affect per-file and per-directory totals?
- How are sparse files represented?
- How is compression represented?
- Do snapshots retain blocks that should or should not be attributed?
- How should we explain differences from Finder/du if semantics differ?

## Risks if We Get This Wrong
- Totals that are internally inconsistent.
- Overcounting clone/shared data.
- User confusion due to mismatch with macOS tooling.

## Planned Experiments / Demos
1. Sparse file demo: create large sparse files and compare logical vs physical.
2. Clone demo: clone large files and inspect accounting changes.
3. Compression demo: compare compressible and incompressible files.
4. Directory aggregate demo: verify parent totals under mixed file types.

## Evidence Log
- [TBD] Logical vs physical comparison notes.
- [TBD] Clone accounting notes.
- [TBD] Compression notes.
- [2026-04-24] `EX-02` reinforced `logical size` as the correct v1 metric:
  sparse files diverged sharply between logical and allocated bytes, while hard
  links showed that naive path-summed logical totals and unique-inode logical
  totals already differ.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` set the current v1 aggregate
  rule: canonical directory `logical size` is unique-inode logical total within
  the aggregate root, even though sibling sums may then be non-additive in the
  presence of hard links.
- [2026-04-24] `EX-03` matched raw and oracle `logical size` output exactly for
  the tested corpus, including sparse-file size, cloned-file size, and hard-link
  aggregate summaries.
- [2026-04-25] `SR-003` confirmed the source-backed v1 boundary: inode dstream
  or equivalent size-bearing fields are enough for the logical-size target,
  while file extents, extent-reference records, clone/shared interpretation, and
  snapshot-retained bytes belong to later accounting modes.
- [2026-04-25] `EX-04` matched raw and oracle logical-size output across a
  broader corpus, including a 2 MiB sparse file, a cloned file whose source was
  later mutated, cross-directory hard links, and symlink target size. The
  compression candidate did not prove compressed storage semantics and remains a
  future accounting probe.
- [2026-04-26] `EX-09` was designed as the focused accounting probe for sparse,
  clone, compression, hard-link, and optional snapshot-retained fixtures. It
  explicitly separates logical-size oracles from allocated/shared/exclusive
  observations.
- [2026-04-26] `SR-009` narrowed compression handling: v1 may report logical
  size for compressed files only when inode/dstream or uncompressed-size metadata
  agrees with the public logical-size oracle; compressed storage savings remain
  a later accounting mode.
- [2026-04-26] `SR-009` and `EX-09` were tightened around compressed logical-size
  precedence. Ordinary dstream size, inode `uncompressed_size`, and decmpfs
  header uncompressed size must be compared separately to public `st_size`.

## Interim Decisions
- v1 may need to distinguish "logical size" mode from "physical accounting" mode.
- V1 should standardize on `logical size` first and treat hard-link aggregation
  policy as an explicit design question rather than an accidental implementation
  detail.
- The current v1 decision is to avoid obvious hard-link overcounting in
  directory aggregates, even at the cost of strict additive child sums.
- Compressed-file logical size still needs a focused corpus check before the
  parser encodes broad field-precedence rules beyond the current allowlist.
- Do not add physical, shared, exclusive, or snapshot-retained accounting to
  implementation specs until `EX-09` or a successor produces metric-specific
  formulas and mismatch cases.
- Unsupported compression algorithms should not be productized as best-effort
  reads. If compression metadata affects requested logical-size output and lacks
  an oracle-backed precedence rule, raw mode should fail closed for that source
  class.
- If ordinary dstream size is zero or inconsistent for a compressed file, v1
  must use an oracle-backed uncompressed-size source or reject the file/source
  class for raw output.

## Exit Criteria
- Defined product-facing size semantics.
- Formula/algorithm for each reported metric.
- A list of known mismatch cases versus other tools.

## Related Logs
- RL-03 FS Tree Topology and Required Records
- RL-05 Subtree Reuse Correctness
- RL-11 Snapshots, Volume Groups, and Firmlinks