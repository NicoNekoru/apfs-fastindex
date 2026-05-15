# EX-17 Synthetic fail-closed record bodies

ID: EX-17
Title: Synthetic fail-closed record bodies for SR-016
Date: 2026-05-14
Owner: Claude
Status: Executed
Result: `validated_sr_016_fail_closed_unit_tests`
Related RLs:
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

SR-016 lists 12+ malformed/edge cases that must be Rust hard stops before
namespace or logical-size rows can be emitted. EX-17 lands one synthetic
raw-block Rust unit test per case in
`crates/apfs-fastindex/src/fs_record_body.rs::tests`. Every test asserts
that `decode_fs_record(...)` returns
`Err(ScanError::InvalidObject(reason))` with `reason` containing the
expected substring; no synthetic body is silently skipped. All tests pass
green together with the existing 30 crate tests for a total of 55/55.

## Question

- Does the Rust body decoder hard-stop on every SR-016 malformed/edge
  record-body case with a typed `ScanError::InvalidObject`, instead of
  silently skipping or guessing?

## Hypotheses

- Hypothesis A `validated_sr_016_fail_closed_unit_tests`: yes, every
  enumerated case in SR-016 is covered by a Rust unit test that asserts a
  typed hard stop on a synthetic raw block.
- Hypothesis B `fail_closed_gap`: at least one SR-016 case is not yet
  enforced by Rust, or silently passes the decoder.

## Environment

- Rust unit tests against synthetic in-memory blocks; no `hdiutil`, no APFS
  image. The tests are deterministic and run with `cargo test
  -p apfs-fastindex`.

## Oracle

- SR-016 itself is the oracle: each test maps to one named SR-016 case and
  asserts the documented failure mode.

## Setup

The Rust crate's body decoder is `crates/apfs-fastindex/src/fs_record_body.rs`.
Synthetic block builders live in the test submodule of that file:
`drec_hashed_key`, `drec_value`, `inode_value`, `inode_value_with_xfields`,
`xattr_key`, `xattr_value`, `inode_key`. Each negative test composes one
malformation and feeds it through `decode_fs_record(...)`.

## Probe Steps

1. Build a synthetic key/value pair that contains exactly one SR-016
   malformation (and is otherwise well-formed).
2. Call `decode_fs_record(node_paddr, entry_index, &key, &value)`.
3. Assert the call returns `Err(ScanError::InvalidObject(reason))` and the
   reason string contains the SR-016 signature.

## SR-016 cases covered

Each case below is enforced by a named Rust unit test in
`fs_record_body::tests`.

| SR-016 case                                          | Unit test                                                     |
| ---------------------------------------------------- | -------------------------------------------------------------- |
| inode body shorter than fixed struct                  | `hard_stops_on_short_inode_value`                              |
| drec body shorter than fixed struct                   | `hard_stops_on_short_drec_value`                               |
| variable-length name longer than the bytes available  | `hard_stops_on_drec_name_too_long`                             |
| embedded NUL inside a required name                   | `hard_stops_on_embedded_nul_in_drec_name`                      |
| non-UTF-8 required name                               | `hard_stops_on_non_utf8_drec_name`                             |
| drec entry type outside the POSIX `DT_*` allowlist    | `hard_stops_on_unknown_drec_entry_type`                        |
| duplicate xfield types inside one blob                | `hard_stops_on_duplicate_xfield_types`                         |
| `xf_used_data != sum(round_up(x_size, 8))`            | `hard_stops_on_xf_used_data_mismatch`                          |
| xfield value cursor runs past the blob                | `hard_stops_on_xfield_value_out_of_bounds`                     |
| xfield metadata table runs past the blob              | `hard_stops_on_drec_xfield_metadata_out_of_bounds`             |
| xfield blob shorter than 4-byte `xf_blob_t` header    | `hard_stops_on_xfield_blob_shorter_than_header`                |
| required xfield with wrong `x_size` (dstream)         | `hard_stops_on_wrong_dstream_size`                             |
| required xfield with wrong `x_size` (sibling_id)      | `hard_stops_on_wrong_sibling_id_size`                          |
| xattr value shorter than fixed header                 | `hard_stops_on_xattr_short_value`                              |
| xattr sets both EMBEDDED and STREAM flags             | `hard_stops_on_xattr_both_flags`                               |
| xattr sets neither EMBEDDED nor STREAM flag           | `hard_stops_on_xattr_neither_flag`                             |
| xattr sets unknown flag bits                          | `hard_stops_on_xattr_unknown_flag_bits`                        |
| xattr body length disagrees with `xdata_len`          | `hard_stops_on_xattr_xdata_len_mismatch`                       |
| xattr stream body shorter than `j_xattr_dstream_t`    | `hard_stops_on_xattr_stream_too_short`                         |
| sibling_link `name_len` lie                           | `hard_stops_on_sibling_link_name_len_overflow`                 |
| sibling_map value shorter than 8 bytes                | `hard_stops_on_sibling_map_short_value`                        |

Plus four positive baseline tests
(`decode_inode_with_dstream_and_name_xfields`,
`decode_drec_with_sibling_id`, `decode_sibling_link_with_name`,
`fs_tree_internal_value_size_round_up`) that pin the happy path.

## Expected Observations

### If Hypothesis A is true

- Every named SR-016 case has a passing Rust unit test asserting a typed
  hard stop with a matching reason substring.

### If Hypothesis B is true

- At least one named case is missing or fails to hard-stop.

## Observed Results

- 21 negative Rust unit tests added in
  `crates/apfs-fastindex/src/fs_record_body.rs::tests`, plus 4 positive
  baseline tests. All 25 pass.
- Total crate test count: `55/55` passing under `cargo test
  -p apfs-fastindex` after the EX-17 + EX-18 patches.
- `cargo fmt --check` clean.
- `cargo clippy -p apfs-fastindex --all-targets -- -D warnings` clean.
- Verdict: `validated_sr_016_fail_closed_unit_tests`.

## Artifacts Saved

- Rust source: `crates/apfs-fastindex/src/fs_record_body.rs` (decoder +
  tests).
- No probe binary, no APFS image: this experiment is purely synthetic and
  deterministic.

## Interpretation

- The Rust body decoder enforces every SR-016 malformed/edge case as a
  typed hard stop with a substring identifying the rule. The decoder
  module's doc comment lists the same set as durable documentation
  alongside the tests.
- Three SR-016 cases that depend on cross-record state (drec entry-type
  versus inode mode disagreement, hard-link sibling records that do not
  close the path-to-inode mapping, compressed-size precedence conflicts)
  remain out of EX-17 scope: they need fixture-level cross-record probes
  rather than per-record synthetic blocks. Those land in EX-19 (logical
  size precedence) and the future EX-22-ish cross-record consistency probe.

## What This Rules Out

- Rules out hypothesis B for the per-record SR-016 cases the decoder is
  responsible for.
- Does not rule out the cross-record SR-016 cases (drec-type vs inode mode
  mismatch, missing sibling map for a drec carrying
  `DREC_EXT_TYPE_SIBLING_ID`, etc.). Those are out of EX-17 scope.

## Impact on RLs

- RL-03: SR-016 fail-closed boundary is enforced for every per-record case
  via Rust unit tests. Cross-record cases still pending.
- RL-10: synthetic negative body cases are now part of the regression
  suite. Re-run via `cargo test -p apfs-fastindex`.
- RL-13: the body-failure category vocabulary
  (`malformed_record_body`, `unsupported_record_body`, `body_field_mismatch`)
  collapses for v1 into typed `ScanError::InvalidObject` with the SR-016
  substring carrying the category.

## Next Exact Step

- Proceed to EX-19 (SR-017 logical-size precedence fixture) — once that
  lands, wire `NamespaceEntry` and `DirectoryAggregate` emission per the
  Rust MWP gate.
