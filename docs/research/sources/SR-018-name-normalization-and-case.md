# SR-018 Name Normalization And Case

Status: Complete
Date: 2026-05-13
Type: Source Review
Related RLs:
- RL-03
- RL-06
- RL-10
- RL-13

## Bottom line

Rust v1 must preserve visible APFS names exactly as stored in directory keys and
must not normalize or case-fold output paths. Case and normalization affect
lookup/key comparison, not row rendering.

V1 policy:

- decode directory, inode-name, xattr, sibling-link, and symlink-target strings
  as UTF-8 for product namespace output
- preserve original stored byte spelling and case in emitted paths
- record the volume's case-insensitive and normalization-insensitive flags
- treat hashed directory keys as APFS lookup/index keys, not as permission to
  normalize visible names
- do not claim full APFS lookup behavior until the Rust code validates the APFS
  name-hash function against case and normalization fixtures

This preserves APFS behavior without overclaiming Unicode equivalence semantics
that v1 does not yet need for raw enumeration.

## Scope

This review answers one question:

- What name normalization and case behavior must Rust preserve for v1 without
  overclaiming?

Out of scope:

- Finder display quirks
- locale-sensitive UI sorting
- boot-root merged namespace behavior
- forensic raw-byte path mode for invalid UTF-8

## Sources reviewed

- Apple File System FAQ, retrieved 2026-05-13:
  <https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html>
- Apple File System Reference, retrieved 2026-05-13:
  <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf>
- `linux-apfs-rw`, retrieved 2026-05-13, commit
  `628b6810e46bcdd423189d2c66295258e10090dc`:
  <https://github.com/linux-apfs/linux-apfs-rw/blob/628b6810e46bcdd423189d2c66295258e10090dc/key.c>,
  <https://github.com/linux-apfs/linux-apfs-rw/blob/628b6810e46bcdd423189d2c66295258e10090dc/apfs.h>
- `apfs-fuse`, retrieved 2026-05-13, commit
  `66b86bd525e8cb90f9012543be89b1f092b75cf3`:
  <https://github.com/sgan81/apfs-fuse/blob/66b86bd525e8cb90f9012543be89b1f092b75cf3/ApfsLib/Util.cpp>,
  <https://github.com/sgan81/apfs-fuse/blob/66b86bd525e8cb90f9012543be89b1f092b75cf3/ApfsLib/Unicode.cpp>
- `dissect.apfs`, retrieved 2026-05-13, commit
  `8d8dbd2545ebb1d65c1cda144097ee15f783e233`:
  <https://github.com/fox-it/dissect.apfs/blob/8d8dbd2545ebb1d65c1cda144097ee15f783e233/dissect/apfs/objects/fs.py>
- `libfsapfs`, retrieved 2026-05-13, commit
  `f179325e5405d3b09a314348646e9898b722759f`:
  <https://github.com/libyal/libfsapfs/blob/f179325e5405d3b09a314348646e9898b722759f/libfsapfs/libfsapfs_name_hash.c>
- EX-02, EX-04, and EX-13 local artifacts in this repository.

## Spec

- Apple states APFS accepts only valid UTF-8 filenames through public creation
  APIs.
- Apple states APFS preserves case and preserves normalization of filenames in
  all variants.
- Apple states APFS on macOS High Sierra and later is normalization-insensitive,
  and that `readdir(2)` order is hash order instead of lexicographic order.
- Apple defines case-insensitive and normalization-insensitive incompatible
  feature bits in the volume superblock.
- Apple defines hashed directory-entry keys where `name_len_and_hash` stores
  both the byte length and a name hash. The reference ties the hash to
  normalized Unicode name data and CRC32C.

## Observation

- `linux-apfs-rw` compares names byte-for-byte only when the volume is not
  normalization-insensitive. Otherwise it iterates normalized Unicode codepoints
  and optionally case-folds depending on the case-insensitive flag.
- `linux-apfs-rw` treats case-insensitive volumes as normalization-insensitive
  for lookup purposes, even if only the case-insensitive bit is being queried.
- `linux-apfs-rw` builds directory-record search keys by hashing normalized
  UTF-32 codepoints with optional case folding and notes that normalization-aware
  queries can only consider the hash.
- `apfs-fuse` converts UTF-8 to UTF-32, normalizes and optionally folds case,
  then computes the APFS directory hash.
- `dissect.apfs` normalizes lookup names to NFD, optionally applies Python
  `casefold()`, hashes the UTF-32LE result, and then verifies case-insensitive
  matches to handle hash collisions.
- `dissect.apfs` uses hashed directory keys when a volume is case-insensitive or
  normalization-insensitive and unhashed keys only when neither mode applies.
- `libfsapfs` carries a `use_case_folding` setting and implements extensive
  Unicode decomposition/case-folding tables for APFS name hashing.
- EX-04 observed that case-insensitive APFS rejected `casename.txt` after
  `CaseName.txt`, while case-sensitive APFS allowed both names. EX-04 also
  observed the decomposed Unicode sibling was rejected after the precomposed
  name in both generated variants, matching normalization-insensitive behavior.

## Hypothesis

- For raw enumeration, Rust does not need to perform APFS lookup matching to
  reconstruct paths from every `DIR_REC`. It does need to preserve names exactly
  and keep volume mode bits attached to the output so later lookup/search code
  does not apply the wrong equivalence relation.
- For v1 row emission, invalid UTF-8 in a required namespace name should be a
  `malformed_record_body` failure. A later forensic mode could expose raw bytes,
  but the current product contract is UTF-8 paths.
- Rust should not normalize directory-entry names before comparing to the POSIX
  oracle. Oracle comparison may need a separate normalized matching layer for
  diagnostics, but product rows must keep stored spelling.

## Open Limits

- Rust has not yet implemented or validated APFS name hashing. Any feature that
  depends on path lookup by user-supplied name should wait for a dedicated hash
  fixture.
- The exact Unicode version and special-case folding table used by macOS need a
  fixture if Rust implements lookup semantics itself.
- This review does not define UI sorting or display normalization.
- Invalid UTF-8 behavior is intentionally outside v1.

## Decision impact

- `RL-06`: namespace reconstruction should emit stored names verbatim and keep
  case/normalization mode as metadata rather than canonicalizing paths.
- `RL-10`: add a name-hash fixture before implementing lookup-by-name in Rust;
  row enumeration can proceed without that gate if it preserves directory-key
  names exactly.
- `RL-13`: normalization-sensitive, case-sensitive, and case-insensitive volumes
  can be recorded distinctly, but unsupported name-lookup claims should fail
  closed instead of falling back to ad hoc Unicode handling.
