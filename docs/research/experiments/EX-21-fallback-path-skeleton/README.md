# EX-21 Fallback path skeleton

ID: EX-21
Title: POSIX traversal fallback that emits the v1 namespace shape
Date: 2026-05-14
Owner: Claude
Status: Executed
Result: `validated_fallback_skeleton`
Related RLs:
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

The v1 spec requires that, when raw mode is rejected, the parser falls
back to a supported POSIX-API traversal rather than guessing. EX-21
lands a small Python skeleton in `src/apfs_fastindex/fallback_traversal.py`
that walks any mounted directory and emits the same `NamespaceEntry` and
`DirectoryAggregate` shape that the Rust MWP emits from raw mode. The
probe verifies that, for the EX-13 proof fixture mounted on macOS, the
fallback output exactly matches the Rust raw output (same paths, same
`entry_kind`, same `logical_size`, same `symlink_target`, same
per-directory aggregates).

The skeleton uses `os.walk` + `os.lstat` + `os.readlink` today. A future
optimization can swap in `getattrlistbulk` for performance without
changing the contract — the support-matrix cell this experiment covers is
identical either way.

Support-matrix cell covered:

- source class: locally mounted directory (e.g., detached APFS image)
- semantic mode: per-volume namespace + logical size
- correctness model: per-file `st_size` as logical size; symlink target
  byte length; directory aggregates via SR-009 unique-inode policy

Out of scope (deliberately, until Gate 2+ approval):

- live boot disk + sealed-volume + boot-root merged namespace
- encrypted runtime
- snapshot-assisted scanning
- physical / shared / exclusive accounting
- `getattrlistbulk` performance work (correctness skeleton first)
- non-macOS hosts

## Question

- For a mounted APFS directory, does a POSIX-API traversal emit the same
  `NamespaceEntry` and `DirectoryAggregate` rows the Rust raw scanner
  emits on the same volume's detached image?

## Hypotheses

- Hypothesis A `validated_fallback_skeleton`: yes. The shape contract
  is identical: same entry set, same per-file logical size (`st_size`),
  same symlink targets, same aggregates per SR-009.
- Hypothesis B `fallback_shape_drift`: the two outputs differ. The
  probe records the diff so the fallback contract can be tightened.

## Environment

- macOS host (probe runs as a normal user).
- One proof fixture (built via `apfs_fastindex.poc_fixture`), mounted at
  the standard POSIX path during fallback traversal, then detached and
  reattached `-nomount -readonly` for the Rust pass.

## Oracle

- Mounted POSIX traversal *is* the fallback; the oracle here is the
  Rust raw path's output. Both must agree on the shape contract.

## Setup

1. Build the proof fixture (image + mountpoint).
2. Take a `NamespaceEntry`/`DirectoryAggregate` snapshot via the fallback
   traversal while the image is still mounted.
3. Detach, reattach `-nomount -readonly`, run the Rust scanner.
4. Diff `NamespaceEntry` lists and `DirectoryAggregate` lists.

## Probe Steps

Implemented by `artifacts/probe_ex21.py`.

## Expected Observations

### If Hypothesis A is true

- Same entry count.
- Same set of `(path, entry_kind, file_id?, logical_size, symlink_target)`
  tuples (the fallback path uses POSIX inode numbers; the raw path uses
  APFS virtual OIDs — these happen to coincide for v1 fixtures but the
  contract only requires logical-size + name + entry_kind +
  symlink_target parity).
- Same aggregates.

### If Hypothesis B is true

- The diff lists the diverging fields.

## Observed Results

- Built the EX-13 proof fixture; ran the Python fallback traversal on
  the mounted image, then the Rust raw scanner against the detached
  `.dmg`.
- Same 7 entries, same paths (`dst`, `dst/clone.txt`, `dst/hard.txt`,
  `dst/link.txt`, `dst/moved.txt`, `dst/sparse.bin`, `src`).
- Same logical sizes including the sparse case (1 048 576 bytes) and
  the symlink target (`moved.txt`, length 9).
- Same 3 aggregates (`.`, `dst`, `src`); same totals (e.g.,
  `dst -> 1048595` summing the three unique inodes — moved.txt/hard.txt
  collapsed, sparse 1 MiB, symlink target length, and so on).
- **Rust port landed** as a follow-up:
  `crates/apfs-fastindex/src/fallback.rs` ports the same walker to
  Rust and ships via the `apfs-fastindex-scan` CLI. Auto-detect
  dispatches a directory path to fallback mode and a `.dmg` /
  `/dev/...` path to raw mode (`--mode raw|fallback|auto` overrides).
  Cross-check: same proof fixture, Rust raw vs Rust fallback both
  emit 7 entries + 3 aggregates with identical
  `(path, entry_kind, logical_size, symlink_target)` and identical
  `unique_inode_logical_total` per directory. Three new Rust unit
  tests in `fallback::tests` cover the happy path, non-directory
  rejection, and top-level `.fseventsd` skip.
- Verdict: `validated_fallback_skeleton`.

## Artifacts Saved

- `src/apfs_fastindex/fallback_traversal.py`: the fallback module.
- `artifacts/probe_ex21.py`: the cross-check probe.
- `artifacts/generated/environment.json`
- `artifacts/generated/ex21-fallback-entries.json`
- `artifacts/generated/ex21-rust-entries.json`
- `artifacts/generated/ex21-comparison.json`
- `artifacts/generated/summary.json`

## Interpretation

- The fallback path is a thin POSIX shim. It is correct enough to serve
  as the supported-source side of the v1 spec's fall-closed boundary
  ("if the environment falls outside the tested raw-mode allowlist, the
  product should fall back to safer supported APIs rather than
  guessing.").
- The current implementation uses `os.lstat` and `os.readlink`. A
  later performance pass can replace the walk with `getattrlistbulk`
  via `ctypes` without changing the public contract.
- The fallback path uses POSIX `inode` as `file_id`. For raw mode the
  `file_id` is the APFS virtual OID. These are the same number on
  freshly-built APFS images, but the contract permits them to differ;
  callers must not key cross-source comparisons on `file_id` alone.

## What This Rules Out

- Rules out hypothesis B on the proof fixture: the shape contract holds
  between raw mode and the fallback.
- Does not validate the fallback on Gate 2 sources (live boot,
  encryption, snapshot-assisted, etc.). Those still require fresh
  experiments and explicit support-matrix approval.

## Impact on RLs

- RL-06: the fallback path emits the same namespace shape, satisfying
  the spec's fail-closed fall-back requirement.
- RL-07: the fallback uses `st_size` directly, which on macOS is the
  same logical-size number SR-017 computes for the in-scope cases.
- RL-08: the fallback is the safe default whenever raw mode rejects a
  source; live / encrypted / boot-root sources continue to require
  explicit support-matrix coverage before they can be added.
- RL-10: the cross-tool oracle now has a "raw vs fallback" parity
  artifact in addition to the existing oracle diffs.
- RL-13: any future `unsupported_source` outcome from raw mode can hand
  off to the fallback with documented expectations.

## Next Exact Step

- R1 complete. Stop and ask the user before extending into Gate 2+ (live
  volumes, encryption, boot-root, incremental cache). EX-21 only covers
  the mounted-detached-image cell; further support-matrix cells need
  dedicated oracles.
