# EX-18 Rust body-field dump

ID: EX-18
Title: Rust body-field dump diff against EX-13/EX-16 Python output
Date: 2026-05-14
Owner: Claude
Status: Executed
Result: `field_level_parity`
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

The Rust crate now decodes `DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`,
`SIBLING_MAP`, and dstream xfields under the SR-015 single cursor rule plus
SR-016 fail-closed gates. EX-18 proves field-level parity against the
Python EX-13 + SR-015-replay (EX-16) parser on the same fixture: every
record's `(node_paddr, entry_index, object_id, raw_type, family, key,
value)` field set must match between Rust and Python with zero divergent
fields.

Output stays as `FsRecordDump.records` (a list of structured `FsRecordRow`
entries). Product `NamespaceEntry` rows remain blocked by the next gate
(SR-017 logical-size precedence + EX-19) and `DirectoryAggregate` rows are
blocked further behind that.

## Question

- For the EX-13 proof fixture (rebuilt deterministically), does the Rust
  body decoder produce a record-by-record field set identical to the Python
  parser running EX-13's body decode + EX-16's SR-015 xfield replay, with
  zero divergent fields?

## Hypotheses

- Hypothesis A `field_level_parity`: yes. Every Rust `FsRecordRow` matches
  the corresponding Python record on `(node_paddr, entry_index, object_id,
  raw_type, family, key, value)` after the EX-13 candidate-scoring fields
  are dropped from the Python side.
- Hypothesis B `field_divergence`: at least one record has a field that
  differs. The probe emits the per-record divergence so the decoder can be
  patched (Rust side) or the Python decoder explained (Python side).

## Environment

- macOS version captured in `artifacts/generated/environment.json`.
- APFS source: rebuilt EX-13 proof fixture (same `build_proof_fixture`
  operations: rename, move, hard link, sparse 1 MiB, clone, append,
  symlink).
- Out of scope: live volumes, encryption, snapshots, merged boot-root
  semantics, compression precedence beyond ordinary dstream size, physical
  accounting.

## Oracle

- The Python parser (EX-13 record decode + EX-16 SR-015 xfield replay)
  carries the validated body contract from EX-13's
  `validated_native_record_body_contract` and EX-16's
  `validated_sr_015_cursor_rule`. Field-level parity against it is the
  reusable cross-tool check for Rust body decoding.

## Setup

1. Capture environment manifest.
2. Build the EX-13 proof fixture.
3. Capture the mounted POSIX oracle (for cross-validation; not the primary
   oracle here).
4. Detach and reattach `-nomount -readonly`.
5. Run `apfs-fastindex-scan` (Rust) and parse the `FsRecordDump.records`.
6. Run the Python EX-13/EX-16 parser on the same raw container.

## Probe Steps

1. Build the fixture, capture the POSIX tree.
2. Re-attach `-nomount -readonly`.
3. Run Rust → JSON with `FsRecordDump.records` per volume.
4. Walk the FS-tree in Python (EX-13 helpers) capturing raw key/value
   bytes, then apply EX-16's SR-015 xfield decoder to override the EX-13
   candidate-scored xfields.
5. Normalize both sides to a comparable dict per record (drop EX-13
   candidate fields like `xfield_layout`, `xfield_layout_score`,
   `xfield_layout_candidates`, `xfield_layout_ambiguous`, plus EX-13 fields
   the Rust dump does not emit, e.g. timestamps that we deliberately skip
   in v1).
6. Key both lists by `(node_paddr, entry_index)`, diff every field, record
   per-record divergences.

## Expected Observations

### If Hypothesis A is true

- Same record count, same `(node_paddr, entry_index)` set, zero divergent
  fields. Verdict `field_level_parity`.

### If Hypothesis B is true

- At least one record with a divergent field. The probe records the per-
  record mismatches so we can decide whether to patch Rust or the Python
  decoder.

## Observed Results

- Rebuilt the EX-13 proof fixture deterministically.
- Rust scanner published `selected_checkpoint` and emitted 53 decoded
  records in `FsRecordDump.records` (matching the EX-13 / EX-16 family
  counts: 12 inode, 9 xattr, 2 sibling_link, 6 dstream_id, 9 file_extent,
  13 dir_rec, 2 sibling_map).
- Python parser produced 53 records on the same raw container under the
  EX-13 body decoder + EX-16 SR-015 xfield replay.
- `(node_paddr, entry_index)` keys: identical sets (53 in each), zero
  missing in Rust, zero extra in Rust.
- Per-record field comparison: `0` mismatches; every `key`, `value`,
  `xfields[*]`, `xfield_used_data`, `xfield_padded_total`,
  `xfield_unused_trailing_bytes`, `validation_notes`, `key_len`,
  `value_len`, `object_id`, `raw_type`, `family` field matched exactly.
- Verdict: `field_level_parity`.

## Artifacts Saved

- `artifacts/probe_ex18.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/ex18-rust-records.json`
- `artifacts/generated/ex18-python-records.json`
- `artifacts/generated/ex18-comparison.json`
- `artifacts/generated/summary.json`

## Interpretation

- The Rust body decoder is now field-identical to the Python parser for
  every record family in scope on the EX-13 proof fixture. The two
  decoders share the SR-015 cursor rule and the SR-016 hard-stop set; this
  parity is the first cross-tool oracle the Rust path has cleared at the
  record-body level.
- The Rust decoder's surface (DIR_REC, INODE, XATTR, SIBLING_LINK,
  SIBLING_MAP, dstream xfields) is now sufficient to feed a namespace +
  logical-size emitter; the remaining gates are SR-017 (logical-size
  precedence) and SR-018 (name/case behavior), both of which sit above
  body decoding.

## What This Rules Out

- Rules out hypothesis B `field_divergence` for the EX-13 proof fixture
  shape on this commit.
- Does not rule out divergence on larger fixtures with denser xfields,
  more drec entry types, or different volume features. Those will exercise
  the same decoder against future fixtures (EX-19/EX-20).

## Impact on RLs

- RL-03: a positive verdict promotes Rust body-field decoding (DIR_REC,
  INODE, XATTR, SIBLING_LINK, SIBLING_MAP, dstream xfields) from "planned"
  to "validated against the proof fixture." Product rows still require
  SR-017 + EX-19 (logical-size precedence) and SR-018 + EX-20 (name/case).
- RL-10: adds the field-level cross-tool oracle as the new validation unit
  before any Rust namespace row emission.
- RL-13: the SR-016 hard-stop unit tests (EX-17) plus this field-level
  parity test together close the body-parser-promotion gate.

## Next Exact Step

- After parity: proceed to EX-19 (SR-017 logical-size precedence fixture)
  and EX-20 (SR-018 name/case fixture). Only after both, wire
  `NamespaceEntry` and `DirectoryAggregate` emission per the Rust MWP gate.
