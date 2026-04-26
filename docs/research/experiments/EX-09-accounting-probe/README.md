# EX-09 Accounting probe

ID: EX-09
Title: Accounting probe
Date: 2026-04-26
Owner: GPT-5.5
Status: Designed
Result: Inconclusive until executed
Related RLs:
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-11 Snapshots, Volume Groups, and Firmlinks

## Bottom line

The project has enough evidence to keep `logical size` as the v1 metric, but not
enough evidence to specify physical, shared, exclusive, compression, or
snapshot-retained accounting. `EX-09` defines the focused probe needed before any
non-logical-size metric is implemented or documented as product behavior.

This experiment should answer two separate questions:

- Do inode/dstream logical-size fields keep matching public logical-size oracles
  for sparse, clone, compressed, and snapshot-retained fixtures?
- Where do public allocated-size tools diverge from each other and from raw
  extent/reference interpretations?
- For compressed files, which raw field should own v1 logical-size precedence
  when ordinary dstream size, inode uncompressed size, and decmpfs metadata
  disagree?

## Question

- Which size fields and public tools agree for logical size, and where do
  allocated/shared/exclusive metrics diverge across sparse files, clones,
  compression, and snapshots?

## Hypothesis

- Hypothesis A: V1 logical size remains extractable from inode/dstream or
  equivalent fields across these cases, while allocated/shared/exclusive metrics
  require separate extent/reference accounting and product semantics.
- Hypothesis B: Some compressed, sparse, clone, or snapshot cases expose logical
  size ambiguities that require narrowing the v1 contract or adding special
  precedence rules.

## Environment

Recommended first environment:

- fresh unencrypted APFS image
- detached raw reads for raw field capture
- mounted oracle capture immediately before detach
- case-insensitive first
- case-sensitive repeat only if logical-size behavior differs or the first run
  exposes a name/normalization dependency

Record:

- macOS version
- APFS image size
- case mode
- compression command versions/return codes
- whether snapshots can be created in the lab image
- raw walker/parser version

## Oracle

Use a matrix of feature-specific oracles:

- logical size:
  - `os.lstat().st_size`
  - `getattrlist` or equivalent public API where available
  - raw inode/dstream size
  - inode `uncompressed_size` when `HAS_UNCOMPRESSED_SIZE` is set
  - decmpfs header `uncompressed_size` when compression metadata is present
- allocated size:
  - `os.lstat().st_blocks * 512`
  - `du -k`/`du -A` variants, recorded as tool-specific observations
  - raw file extent totals, only when that parser path exists
- clone/shared semantics:
  - public tool observations only in this experiment unless raw extent-reference
    parsing is implemented
- snapshot-retained semantics:
  - public snapshot and space observations only; do not attribute retained bytes
    to v1 output
- compression precedence:
  - compare public `st_size` against dstream size, inode uncompressed size, and
    decmpfs uncompressed size separately
  - record the compression algorithm and whether metadata is inline xattr,
    xattr data stream, or resource fork when visible

No single oracle should be treated as authoritative for every metric.

## Setup

Create a mounted APFS image with these fixtures:

- `regular/random.bin`: incompressible regular file
- `regular/compressible.txt`: repeated text file
- `sparse/sparse-64m.bin`: sparse file with data at start and end
- `clone/source.bin`: regular file cloned with `cp -c`
- `clone/clone.bin`: clone mutated after copy-on-write divergence
- `hardlinks/original.bin` plus at least one hard link
- `compressed/ditto-compressed.txt`: `ditto --hfsCompression` candidate
- `compressed/large-compressible.txt`: large enough to force non-inline
  compression metadata when the OS chooses to compress it
- `compressed/precompressed.bin`: incompressible control that should not gain
  compression semantics
- optional dataless/cloud fixture if available without introducing iCloud account
  dependency; otherwise record as blocked
- optional snapshot retained file:
  - create large file
  - create snapshot if permitted
  - delete file
  - record free-space/tool behavior

## Probe Steps

1. Create the fixture image and mount it.
2. Create each fixture with deterministic content and sizes.
3. Force sync after each fixture family.
4. Capture public metadata:
   - `stat`
   - Python `os.lstat`
   - `du`
   - `ls -l`/`ls -ls`
   - any available compression/xattr metadata commands
   - xattr names and sizes, especially `com.apple.decmpfs` and
     `com.apple.ResourceFork`
5. Capture mounted oracle JSON.
6. Detach, attach `-nomount`, and capture raw walk plus raw inode/dstream fields.
7. Capture raw inode uncompressed-size fields, xattr records, and decmpfs headers
   when native or probe-only field extraction exists.
8. If raw extent parsing exists, capture file extents and extent-reference
   observations separately from logical-size output.
9. Write comparison JSON with one section per metric, not one global pass/fail.

## Expected Observations

### If Hypothesis A is true

- Logical size matches public `st_size` for regular, sparse, clone, hard-link,
  and compressed candidate files.
- Sparse allocated bytes are lower than logical size.
- Clones show logical duplication but allocated/shared attribution is ambiguous
  without extent-reference semantics.
- Compression observations are tool- and fixture-dependent; they do not affect
  v1 logical size unless raw logical fields disagree with public size.
- If ordinary dstream size is zero for a compressed file, inode
  uncompressed-size or decmpfs uncompressed-size metadata should explain public
  `st_size`.
- Snapshot-retained bytes are container/accounting semantics, not v1 per-file
  logical-size semantics.

### If Hypothesis B is true

- A compressed or dataless/snapshot-related fixture exposes a logical-size
  mismatch.
- Raw dstream/inode precedence differs from public APIs for a v1-visible file.
- The v1 contract needs a narrower allowlist or explicit field-precedence rule.

## Observed Results

- Designed but not executed.

Known prior evidence:

- `EX-02`, `EX-03`, and `EX-04` matched logical size for regular, sparse,
  cloned, hard-linked, and symlink entries in image-backed corpora.
- `EX-04` did not prove compressed storage semantics; its compression candidate
  did not create a meaningful allocated-byte divergence.
- `SR-009` added the compression-specific risk that ordinary metadata may report
  zero or otherwise insufficient logical size for compressed files; v1 needs a
  field-precedence proof before broad compressed-file support.

## Artifacts Saved

- `README.md`

Future execution should save:

- `artifacts/probe_ex09.py`
- `artifacts/generated/environment.json`
- public metadata command outputs
- mounted oracle JSON
- raw field dump JSON including dstream, inode uncompressed-size, xattr, and
  decmpfs header fields
- per-metric comparison JSON
- `summary.json`

## Interpretation

- V1 should continue to mean logical size until this experiment proves more.
- Physical/shared/exclusive accounting should remain out of implementation specs
  until the experiment supplies metric-specific formulas and mismatch cases.
- Compression deserves targeted evidence because APFS compression can involve
  xattrs, resource forks, and tool-specific behavior rather than a simple file
  extent count.
- If compressed logical size cannot be reconciled to public `st_size`, v1 should
  keep compressed files outside the raw allowlist rather than reporting a guessed
  size.

## What This Rules Out

- It rules out adding "size on disk" to v1 based only on sparse/clone logical-size
  success.
- It rules out treating `du`, `st_blocks`, raw file extents, and Finder-like
  values as interchangeable oracles.
- It rules out reporting ordinary dstream size for compressed files without
  checking uncompressed-size metadata once the compression fixture exists.

## Impact on RLs

- RL-07: defines the next accounting proof gate before physical/shared metrics.
- RL-10: reinforces feature-specific oracle policy for size metrics.
- RL-11: keeps snapshot-retained bytes out of raw v1 until product semantics are
  defined.
- RL-13: unsupported compression metadata is a raw-mode support gate when it
  affects requested logical-size output.

## Next Exact Step

- Execute `EX-13` first or share its native field-dump machinery, then implement
  `artifacts/probe_ex09.py` once inode dstream, inode uncompressed-size, xattr,
  and decmpfs metadata can be emitted. The first `EX-09` result should answer
  compressed logical-size precedence before any physical/shared accounting
  formula is attempted.
