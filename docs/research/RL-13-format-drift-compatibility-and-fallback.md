# RL-13 Format Drift, Compatibility, and Fallback

Status: Open
Priority: P1
Owner: TBD
Last Updated: 2026-04-26

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
- [2026-04-24] `EX-01` added a runtime hard-stop data point: the live startup
  container on this host was not raw-readable without elevated privilege, while
  a mounted APFS image-backed lab container was probeable and showed a moving
  latest checkpoint under write churn.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` carried those source and probe
  results into the implementation boundary: raw mode stops on unsupported
  checkpoint layout, ambiguous OMAP context, unexpected object typing, or
  environments that require unsupported encryption, snapshot, or boot-root
  semantics.
- [2026-04-24] `EX-03` confirmed that a detached image-backed APFS container is
  inside the current allowlist: in that environment the raw walk matched the
  mounted oracle exactly for the narrow v1 fields.
- [2026-04-25] `EX-04` kept detached image-backed APFS inside the allowlist
  across both case-insensitive and case-sensitive variants, but did not broaden
  that support claim to live startup disks, encrypted media, snapshots, or
  unsupported feature sets.
- [2026-04-25] `SR-004` defined the current support-matrix draft: raw mode is
  allowlisted for detached, unencrypted, image-backed or otherwise explicitly
  stable sources; unsupported feature bits, encryption requirements, Fusion,
  snapshot/sealed-volume semantics, merged-root requests, and unvalidated APFS
  variants are hard stops or fallback triggers.
- [2026-04-26] `EX-05` added a support-boundary distinction: mounted image-backed
  APFS can be raw-readable during churn, but the current latest-state raw walk
  is not raw-supported because it did not validate against a named stable oracle.
- [2026-04-26] `EX-08` converted the support-boundary draft into an executable
  matrix design. Each source class must record raw readability, checkpoint/root
  discovery, oracle availability, comparison result, and support verdict.
- [2026-04-26] `SR-005` and `EX-10` moved `.dmg`/raw-device source gating and
  contiguous checkpoint descriptor scanning into native Rust while keeping
  non-contiguous descriptor layouts, short reads, and missing valid NXSB
  candidates as hard stops.
- [2026-04-26] `SR-007` and `SR-012` clarified the parser's fail-closed posture:
  validate object headers before use and gate APFS feature/layout drift through
  allowlists rather than best-effort parsing.
- [2026-04-26] `SR-011` kept FileVault, hardware-backed encryption, encrypted
  OMAP values, and key rolling outside raw mode until a read-path matrix cell is
  executed.
- [2026-04-26] First `EX-08` safe-host execution produced concrete verdict
  artifacts. Detached image-backed APFS matched oracle and stayed `supported`
  for narrow v1 proof work. Mounted image-backed APFS was raw-readable and
  parsable but mismatched the mounted oracle, so the correct verdict is
  `readable_not_supported`. Startup-container raw read failed unprivileged and
  is `blocked_privilege`.
- [2026-04-26] `SR-005` clarified immediate native hard stops for the checkpoint
  layer: unsupported non-contiguous descriptor layouts, invalid block-zero
  locator, checksum-failing checkpoint candidates, out-of-range checkpoint
  indexes, and missing checkpoint-map validation before downstream traversal.
- [2026-04-26] `SR-013` and `EX-11` promoted checkpoint-map integrity to an
  explicit compatibility gate. Invalid checkpoint-map chains, missing terminal
  flags, impossible mapping counts, unsupported data-ring wrapping, or bad
  ephemeral-object checksums require fallback.
- [2026-04-26] `SR-012` and `EX-08` converted feature drift into recorded
  allowlist fields: source-gate facts, container facts, volume facts, and
  requested-mode blockers. They also split checkpoint-scanner-safe,
  checkpoint-context-safe, OMAP-root-safe, namespace-logical-size-safe, and
  product-supported verdicts.
- [2026-04-26] `SR-009` and `EX-09` clarified compression as a compatibility
  gate for v1 logical-size output: if compressed metadata cannot be reconciled
  with public logical size, raw mode should fail closed rather than report a
  guessed size.
- [2026-04-26] `EX-11` executed checkpoint-map validation on a generated
  detached proof fixture and matched synthetic hard-stop verdicts for malformed
  map chains, invalid mapping counts/sizes, bad ephemeral checksums, and
  non-contiguous descriptors.
- [2026-04-26] Observation: the first `EX-12` route was blocked because the
  identity-oracle raw media was not preserved. This remains a
  compatibility/oracle rule for replaying old identities, but it is no longer
  the current `EX-12` status.
- [2026-04-26] `EX-12` was unblocked by a self-paired probe that builds a fresh
  proof fixture, attaches it `-nomount`, and runs the native Rust scanner and
  `go-apfs identitydump` against the same `/dev/rdiskN` in the same execution.
  Verdict: `validated_omap_lookup_contract`. Oracle evidence: on-disk
  obj-header replay at every paddr Rust returned passes type/subtype/storage/
  oid/xid/Fletcher-64 checks; Python re-runs SR-006 lower-bound on Rust's
  published OMAP samples and gets the same mapping; identitydump and Rust
  agree on `root_tree.oid = 1028`. Cross-tool `(paddr, object_xid)`
  divergence (go-apfs xid 12 / paddr 427 vs Rust xid 13 / paddr 433) is
  recorded as a `go_apfs_active_state_observation` and constitutes a
  compatibility caveat: third-party APFS parsers can resolve a different
  active-state checkpoint than this scanner, so cross-tool oracles must
  declare the same `selected_xid` to be comparable.
- [2026-04-26] OMAP feature-allowlist enforcement was extended to fully
  cover SR-006: the resolver now hard-stops on
  `OMAP_VAL_CRYPTO_GENERATION` values, on unknown bits in
  `omap_val_t.flags`, and on OMAP-phys flags
  `OMAP_PHYS_ENCRYPTING`/`OMAP_PHYS_DECRYPTING`/`OMAP_PHYS_KEYROLLING`/
  `OMAP_PHYS_CRYPTO_GENERATION_FLAG` plus any unknown `omap_phys.flags`
  bit. `OMAP_MANUALLY_MANAGED` remains an allowed phys-flag bit. All
  paths covered by Rust unit tests on synthetic OMAPs.
- [2026-04-26] `EX-10` extended the Rust path with explicit feature-allowlist
  enforcement. The container decoder rejects any `incompatible_features` bit
  outside the v1 allowlist before checkpoint-map validation begins. The volume
  decoder marks volumes with `OBJ_ENCRYPTED` set, the
  `APFS_INCOMPAT_SEALED_VOLUME` bit, normalization sensitivity, or unknown
  `incompatible_features` bits as `Unsupported(reason)` and skips their
  FS-tree. `OMAP_VAL_ENCRYPTED` and `OMAP_VAL_NOHEADER` are hard stops in the
  resolver. Unknown FS-record families are recorded as
  `unsupported_record_count` rather than silently ignored. The EX-10 probe
  asserts that all of these counters are zero for the proof fixture.
- [2026-04-26] Spec/Observation: `SR-014` extends fail-closed compatibility
  rules from record-family discovery into record-body decoding. Malformed
  variable-length names, duplicate or impossible xfields, invalid UTF-8 where a
  namespace string is required, unsupported xattr stream forms, unknown
  directory or xattr flag bits, and mode-incompatible record groups must block
  namespace/logical-size output until `EX-13` or a successor proves a narrower
  behavior.
- [2026-04-26] Hypothesis: `EX-13` should classify record-body failures as
  `body_field_mismatch`, `selected_xid_mismatch`, `unsupported_record_body`,
  `malformed_record_body`, or `oracle_inconclusive` instead of letting product
  rows proceed with partial metadata.
- [2026-04-26] Observation: Python `EX-13` produced exactly this kind of gate and
  then resolved the first negative case in Python: the verdict is now
  `validated_native_record_body_contract` for the proof fixture after xfield
  layout candidates are recorded and scored. Rust support should still wait for
  a Python fixture-variant pass because the successful decoder relies on
  observed layout selection rather than a single settled rule.
- [2026-04-26] Observation: Extended `EX-13` layout diagnostics found `4`
  non-row-critical xfield records with true top-score ambiguity. This does not
  invalidate the proof-fixture namespace/logical-size comparison, but it does
  prevent treating the current scoring heuristic as a compatibility rule.

## Interim Decisions
- Compatibility boundaries must be explicit, not implied.
- V1 raw mode should prefer an allowlist over a broad claim of general APFS
  support.
- Unsupported states should trigger fallback, not degraded best-effort parsing.
- The first parser prototype should encode its hard-stop list directly from the
  narrow parser contract rather than infer it ad hoc at runtime.
- Feature-bit and environment checks belong at the source gate before traversal,
  not after partial parser output has already been produced.
- Compatibility/fallback language should report both access and correctness:
  `readable` means bytes can be read; `supported` means the output is validated
  against the selected product semantics.
- Runtime support should be represented as a matrix artifact, not prose alone,
  so untested hardware and blocked privilege cases remain visible.
- Native Rust code should land as source-gate or parser-gate slices. Each slice
  must state exactly which unsupported APFS layouts become hard errors.
- The source gate should preserve the distinction between access, parsing, and
  support. A source that reaches checkpoint discovery or proof raw walk can
  still be outside the allowlist when oracle alignment or coherent-state
  pinning is missing.
- Candidate checkpoint discovery is a parser-gate slice, not support. Support
  for native traversal starts only after checkpoint-map, OMAP, root, and
  requested record validation all pass for the selected product mode.
- Feature masks, volume roles, volume-group UUIDs, snapshot/revert fields,
  encryption state, OMAP flags, and root-tree layout facts must be recorded
  before a source is promoted from one gate to the next.
- Support promotion requires replayable evidence at the same gate. Do not use
  raw identities from one generated image as an oracle for another image.
- Namespace/logical-size support promotion requires body-field evidence, not
  just FS-record family counts. Sources with unsupported body encodings or
  unaligned oracle state must stop before product rows are emitted.
- Rust parser support should not broaden until the Python body oracle exercises
  xfield layout selection across additional fixture variants. The immediate
  compatibility gate moved from sparse dstream mismatch to deterministic
  xfield-layout policy.

## Exit Criteria
- Supported-version matrix exists.
- Fallback triggers are defined.
- User-facing degraded-mode behavior is specified.
- The repo contains a concrete raw-mode allowlist and a concrete hard-stop list.

## Related Logs
- RL-08 Live Volume, Encryption, and Read Path
- RL-10 Validation Corpus and Oracle
- RL-12 Performance Model and Optimization