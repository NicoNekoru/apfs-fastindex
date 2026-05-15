# EX-20 SR-018 name/case fixture

ID: EX-20
Title: SR-018 name preservation across case and normalization variants
Date: 2026-05-14
Owner: Claude
Status: Executed
Result: `validated_sr_018_name_preservation`
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

SR-018 says Rust v1 should preserve visible APFS names exactly as stored in
directory keys and must not normalize or case-fold output paths; case and
normalization affect lookup/key comparison only, not row rendering. EX-20
builds two same-run APFS images — one case-insensitive (`APFS`) and one
case-sensitive (`Case-sensitive APFS`) — each containing ASCII case
variants, precomposed NFC vs decomposed NFD Unicode forms, and explicit
collision attempts. The probe captures which `os.O_CREAT | os.O_EXCL`
operations APFS accepted vs rejected (this is the APFS lookup oracle), and
asserts that Rust's emitted UTF-8 paths from `FsRecordDump.records` match
mounted POSIX traversal byte-for-byte.

The Rust crate makes no lookup-by-name claim. Row enumeration only.

## Question

- For both `APFS` (case-insensitive, normalization-insensitive) and
  `Case-sensitive APFS` (case-sensitive, normalization-insensitive)
  volumes, does Rust preserve the stored UTF-8 bytes of each directory
  entry exactly, and do its enumerated path strings match mounted POSIX
  paths byte-for-byte?

## Hypotheses

- Hypothesis A `validated_sr_018_name_preservation`: yes. Rust paths
  match POSIX paths byte-for-byte on both volumes, including precomposed
  Unicode names, and the volume's `case_insensitive` /
  `normalization_insensitive` flags are exposed in `VolumeSummary` so
  downstream code can record them as metadata.
- Hypothesis B `name_preservation_gap`: at least one path differs (e.g.,
  Rust normalized, case-folded, or dropped a byte).

## Environment

- macOS version captured in `artifacts/generated/environment.json`.
- APFS sources: one detached `.dmg` per volume variant.
- Out of scope: Finder display, UI sorting, lookup-by-name semantics
  (those depend on the APFS name hash + Unicode normalization tables and
  are deferred until a dedicated hash fixture).

## Oracle

- Mounted POSIX traversal: paths returned by `os.walk(root)`. APFS's
  acceptance/rejection of duplicate creates under `O_EXCL` is its own
  lookup oracle.
- The Rust crate's `FsRecordDump.records` (EX-18 parity) is the raw
  parser. Path reconstruction here climbs `parent_id` chains from inode
  bodies + reads names from `dir_rec` keys, with no normalization or
  case folding.

## Setup

1. Capture environment manifest.
2. For each of `APFS` and `Case-sensitive APFS`:
   - Build an image and mount it.
   - Create entries:
     - `CaseName.txt` (then attempt to create `casename.txt` with
       `O_EXCL`).
     - Precomposed NFC `café.txt` (`café.txt`).
     - Decomposed NFD form `café.txt` (then attempt to create with
       `O_EXCL`).
     - An ASCII control case `plain.txt`.
   - Capture which creates succeeded and which raised `FileExistsError`.
   - Snapshot the mounted POSIX traversal (paths as Python returns them).
3. Detach and reattach `-nomount -readonly`.
4. Run the Rust scanner, collect `FsRecordDump.records`.
5. Reconstruct paths from Rust output (dir_rec key.name + parent chain via
   inode `parent_id`).
6. Diff Rust paths against mounted POSIX paths byte-for-byte (Python
   `str.encode("utf-8")`).
7. Verify each `VolumeSummary` carries `case_insensitive` and
   `normalization_insensitive` flags.

## Probe Steps

Same as setup; this experiment is mechanical and deterministic.

## Expected Observations

### If Hypothesis A is true

- For both volumes: mounted path set == Rust-emitted path set (UTF-8 bytes
  identical). Volume flags carry the right case/normalization mode.
- Case-insensitive volume rejects the `casename.txt` duplicate; both
  volumes (because both are normalization-insensitive on macOS HS+)
  reject the NFD duplicate after the NFC creation.

### If Hypothesis B is true

- At least one Rust path differs from POSIX. The probe records which.

## Observed Results

Two same-run images built and validated:

| volume    | fs                         | `case_insensitive` | `normalization_insensitive` | mounted entries | rust entries | byte-for-byte match |
| --------- | -------------------------- | ------------------ | --------------------------- | --------------- | ------------ | ------------------- |
| `EX20CI`  | `APFS`                     | `true`             | `false`                     | 4               | 4            | yes                 |
| `EX20CS`  | `Case-sensitive APFS`      | `false`            | `true`                      | 5               | 5            | yes                 |

APFS lookup-oracle outcomes per fixture step (matches expectations):

| step                                            | CI volume   | CS volume   |
| ----------------------------------------------- | ----------- | ----------- |
| create `plain.txt`                              | accepted    | accepted    |
| create `CaseName.txt`                           | accepted    | accepted    |
| attempt `casename.txt` after `CaseName.txt`     | EEXIST      | accepted    |
| create NFC `café.txt`                           | accepted    | accepted    |
| attempt NFD `café.txt` after NFC sibling        | EEXIST      | EEXIST      |

Notes:

- `EX20CI` reports `case_insensitive=true` but `normalization_insensitive=false`
  — APFS sets only the case bit on CI volumes; the volume is still
  normalization-insensitive for lookup, but the explicit incompat bit is
  reserved for the CS variant.
- `EX20CS` reports `case_insensitive=false` and
  `normalization_insensitive=true`. This is consistent with SR-018's
  observation that CS APFS volumes on macOS still treat NFC and NFD as
  the same name for lookup.
- Rust's reconstructed paths come from `FsRecordDump.records` dir_rec
  keys (stored UTF-8 bytes), with no normalization or case folding on
  the Rust side. The byte-level hex comparison
  (`path.encode("utf-8").hex()`) is identical to the mounted POSIX
  traversal for every entry on both volumes.

Verdict: `validated_sr_018_name_preservation`.

## Artifacts Saved

- `artifacts/probe_ex20.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex20-fixture-operations.json`
- `artifacts/generated/ex20-ci-mounted-posix-oracle.json`
- `artifacts/generated/ex20-cs-mounted-posix-oracle.json`
- `artifacts/generated/ex20-ci-rust-records.json`
- `artifacts/generated/ex20-cs-rust-records.json`
- `artifacts/generated/ex20-ci-comparison.json`
- `artifacts/generated/ex20-cs-comparison.json`
- `artifacts/generated/summary.json`

## Interpretation

- SR-018's stored-spelling-preserved policy is correct for v1 row
  enumeration. Rust does not normalize and the mounted POSIX oracle does
  not normalize: stored UTF-8 bytes pass through identically on both
  CI and CS volumes.
- The volume superblock's `case_insensitive` and
  `normalization_insensitive` flags are sufficient context to record
  lookup mode without performing lookup-by-name in Rust.
- Lookup-by-name semantics are still NOT claimed by Rust. A future
  dedicated APFS-name-hash fixture (CRC32C over UTF-32LE normalized
  codepoints, with optional case fold) is needed before any "find this
  path" feature can be implemented; this probe explicitly does not open
  that gate.

## What This Rules Out

- Rules out hypothesis B `name_preservation_gap` on the proof fixture.
- Does not rule out failure modes that need lookup-by-name semantics
  (collision resolution under hashed keys, NFC/NFD equivalence queries,
  case-fold lookups). Those remain explicitly out of v1.

## Impact on RLs

- RL-06: row enumeration is safe for both case-insensitive and
  case-sensitive APFS volumes without any normalization on the Rust
  side. Volume metadata (`case_insensitive`,
  `normalization_insensitive`) is sufficient context for downstream
  code that wants to claim lookup-by-name later.
- RL-10: adds the across-case-and-normalization parity oracle as a
  regression.
- RL-13: a Rust path mismatch on either volume would be a fail-closed
  signal pointing to either name decoding (SR-016) or a stored-spelling
  drift that needs investigation.

## Next Exact Step

- Positive verdict: wire `NamespaceEntry` and `DirectoryAggregate`
  emission in Rust per the MWP gate. Names emitted verbatim, sizes per
  SR-017, aggregates per SR-009 unique-inode policy.
- Negative verdict: identify which volume + which entry diverges, decide
  whether the bug is in Rust's name decoding (likely an SR-016 bug) or
  in the Python POSIX traversal expectation.
