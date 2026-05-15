# RL-10 Validation Corpus and Oracle

Status: Open
Priority: P0
Owner: TBD
Last Updated: 2026-05-14

## Core Question
- How do we prove the parser and incremental engine are correct?

## Why This Matters
- Reverse-engineered raw parsing needs a disciplined correctness process.
- Performance claims are irrelevant if correctness is not measurable.

## Current Assumptions
- We need both:
  - a golden test corpus
  - an oracle for comparison
- The oracle may vary by feature:
  - POSIX traversal for namespace
  - system tools for sizes
  - snapshots for stable comparison
- Negative and inconclusive results should be preserved as first-class evidence,
  not discarded.

## Known Facts
- Full correctness and incremental correctness are separate problems.
- Many edge cases will only appear under targeted demos.
- "The oracle" is not one thing; validation must be feature-specific and scoped
  to the exact product mode under test.

## Unknowns / Open Questions
- What is the best oracle for each output metric?
- How do we compare against user-visible namespace on modern macOS?
- What test corpus is needed to cover APFS edge behavior?
- How do we detect silent incremental bugs?
- What minimum artifact set should every experiment save so that future work can
  reuse it?

## Risks if We Get This Wrong
- Shipping a parser that appears to work on happy-path volumes only.
- Regression blindness as reverse engineering progresses.

## Planned Experiments / Demos
1. Build a corpus matrix covering:
   - create/delete
   - rename/move
   - hard links
   - sparse files
   - clones
   - compression
   - snapshots
   - case-sensitive names
   - Unicode edge cases
2. Compare raw parser output to POSIX/API traversal on stable snapshots.
3. Run incremental scans after each mutation and diff against a fresh full scan.
4. Add fuzz-style small-volume mutation sequences and compare results.

## Evidence Log
- [TBD] Initial corpus definition.
- [TBD] Oracle comparison method.
- [TBD] Incremental diffing framework notes.
- [2026-04-24] The research documentation schema was split into `RL-*`, `SR-*`,
  and `EX-*` artifacts, with experiments required to record environment, oracle,
  expected outcomes, observed results, and what the result rules out.
- [2026-04-24] `EX-01` combined a mounted-view oracle with direct raw checkpoint
  observation, which is the pattern future live-state probes should follow.
- [2026-04-24] `EX-02` established a reusable mutation corpus covering rename,
  move, hard link, sparse file, clone, symlink, and case behavior across both
  case-insensitive and case-sensitive APFS volumes.
- [2026-04-24] `contracts/narrow-v1-parser-contract.md` fixed the first proof target:
  compare raw output and mounted-view oracle for one chosen state on one volume,
  matching path, entry type, stable file identity, and `logical size`.
- [2026-04-24] `EX-03` implemented that proof target directly with a reusable
  loop: mounted oracle -> detach to pin state -> raw walk -> normalized diff.
- [2026-04-25] `EX-04` extended that proof loop to two image-backed APFS
  variants and a broader corpus. Both case-insensitive and case-sensitive
  images matched the mounted oracle exactly for path, type, file identity,
  logical size, and symlink target fields.
- [2026-04-26] `EX-05` demonstrated a negative live-state oracle case: baseline
  and final mounted oracles were not enough to validate a raw walk that resolved
  latest state during write churn. The live raw walk needs either true XID
  pinning or a stable snapshot/API oracle before correctness can be asserted.
- [2026-04-26] `EX-09` accounting design split size validation into per-metric
  oracles. Logical size, allocated size, clone/shared behavior, compression, and
  snapshot-retained bytes must not share a single pass/fail oracle.
- [2026-04-26] `EX-10` added a synthetic unit oracle for the Rust checkpoint
  scanner. The oracle validates fail-closed selection mechanics only; real APFS
  support still requires detached-image comparison against existing pinned-state
  artifacts and mounted oracles.
- [2026-04-26] `EX-11` defined a checkpoint-map integrity oracle: detached proof
  images for positive checkpoint context and synthetic malformed descriptor/data
  rings for negative verdicts.
- [2026-04-26] `EX-12` defined an OMAP lookup oracle using pinned identity
  artifacts from `EX-06` and `EX-07`, keeping object-resolution correctness
  separate from namespace reconstruction.
- [2026-04-26] `EX-11` executed its positive checkpoint-map oracle on a generated
  detached proof fixture and matched all synthetic malformed-case expectations.
- [2026-04-26] Observation: the first `EX-12` route was marked blocked because
  `EX-06`/`EX-07` preserved identity JSON but not the raw images needed to
  replay native OMAP lookup against those identities.
- [2026-04-26] `EX-12` was unblocked by replacing the stale-oracle pattern
  with a self-paired probe: build a fresh proof fixture, attach it
  `-nomount`, and run the native Rust scanner and `go-apfs identitydump`
  against the same `/dev/rdiskN` in one execution. The probe also re-reads
  every Rust-returned paddr and re-runs SR-006 lower-bound on Rust's own
  OMAP sample lists, giving three independent observers for the same raw
  bytes (on-disk replay, Python SR-006 replay, third-party identitydump).
  This pattern is the new template for validating any future native
  parser slice that needs a third-party oracle: the raw image and the
  oracle must be produced from the same execution, and any cross-tool
  divergence must be recorded with the oracle's `selected_xid` declared.
- [2026-04-26] `EX-12` introduced an active-state-selection caveat for
  third-party APFS oracles: `go-apfs identitydump` resolved a different
  active-state checkpoint than the Rust scanner on the same proof
  fixture, so cross-tool comparison can require `root_tree.oid` agreement
  while documenting `(paddr, object_xid)` divergence as expected when
  `selected_xid` differs. Every future cross-tool oracle must declare or
  pin `selected_xid` before its results are diffed against the native
  scanner.
- [2026-04-26] `EX-09` was tightened so compressed logical-size precedence must
  compare public `st_size` with dstream size, inode uncompressed-size fields,
  and decmpfs metadata separately.
- [2026-04-26] `EX-10` also added a proof-fixture smoke artifact. That run is a
  valid oracle only for Rust `.dmg` source gating and checkpoint candidate
  discovery on the detached-image allowlist; it is not a namespace or
  logical-size oracle because Rust still emits no entries or aggregates.
- [2026-04-26] Spec/Observation: `SR-014` and `EX-13` define the next
  feature-specific oracle. A native record-body field dump must be compared
  against a same-run mounted/POSIX namespace and ordinary logical-size oracle,
  with any third-party APFS observer preserving the `selected_xid` caveat from
  `EX-12`.
- [2026-04-26] Observation: `EX-13` executed as a Python-first probe and produced
  `validated_native_record_body_contract`. The same-run mounted oracle and
  Python raw parser agreed on path set, identity, symlink target, hard-link file
  identity, and ordinary logical size after xfield layout candidates were
  preserved. This is reusable evidence because it saves the raw body dump,
  mounted oracle, cross-tool observer, comparison, xfield-layout summary, and
  summary under `EX-13` artifacts.
- [2026-05-13] Observation: `EX-14` is a negative/inconclusive oracle result for
  the body-parser gate. The retained variant fixture saved environment,
  operations, mounted/POSIX oracle, Rust context, comparison status, and xfield
  summary status, but the raw body oracle did not run because Rust returned no
  `selected_checkpoint` after `APFS object validation failed: checksum mismatch
  at block 1031`. A same-session rerun of `EX-13` still passed, separating the
  new fixture-context blocker from the original proof fixture.
- [2026-05-13] Spec/Observation: `SR-015` creates the next body-oracle replay
  gate: run the source-backed xfield cursor rule against the saved EX-13 media
  state, record `xf_used_data`, and require the same namespace/logical-size diff.
- [2026-05-13] Spec/Observation: `SR-016` through `SR-018` add required negative
  and edge fixtures for record-body malformation, compression-size conflicts,
  and APFS name-hash/case behavior before Rust broadens beyond row enumeration.
- [2026-05-14] Observation: `EX-16` adds a per-record structural oracle:
  `xf_used_data == sum(round_up(x_size, 8))` over an inode/dir_rec's xfields.
  This is independent of any product output and is therefore reusable across
  any future fixture-replay probe. The EX-13 proof fixture's `14` xfield
  records pass with no exceptions. Future body probes should record this
  equality per record and treat any mismatch as a fail-closed signature for
  SR-016. (EX-17 will exercise the negative side with synthetic malformed
  xfield blobs.)
- [2026-05-14] Observation: `EX-15` resolved the EX-14 context-provider blocker
  via a deterministic rebuild + tri-oracle replay pattern that is reusable for
  any future "Rust aborts at block N" investigation: rebuild the fixture,
  retain the `.dmg` image, run `fsck_apfs -n` and `go-apfs identitydump`
  unchanged, then replay every SR-005 / SR-006 / SR-007 validation in Python
  one candidate at a time and dump the raw bytes of any block the Rust path
  rejects. The pattern caught the EX-14 signature as a Rust FS-tree traversal
  bug (internal-node values are virtual OIDs, not paddrs) rather than a
  checkpoint-selection or malformed-image issue.

## Interim Decisions
- Every optimization must be validated against a fresh full-scan oracle.
- Namespace, size, incremental behavior, and boot-root semantics may require
  different oracles.
- Raw outputs belong in `artifacts/`, while distilled conclusions belong in the
  experiment `README.md` and then back in the relevant `RL-*` logs.
- The current narrow-v1 regression pattern is now explicit and runnable, not a
  future TODO.
- Broader corpus additions should keep using the same mounted oracle -> detach
  -> raw walk -> normalized diff pattern unless the question is specifically
  about live-state behavior.
- For live-state behavior, every oracle must name the exact state it validates.
  "Before" and "after" mounted walks are diagnostic artifacts, not proof that a
  concurrent latest raw walk is coherent.
- Synthetic unit oracles are acceptable for parser hard-stop mechanics, but they
  must not be treated as real-media APFS compatibility evidence.
- Native parser gates should be validated independently in this order:
  checkpoint candidate selection, checkpoint-map context, OMAP lookup, root
  discovery, FS-record decode, namespace/logical-size diff.
- Identity-oracle artifacts must preserve or regenerate matching raw media. JSON
  identities alone are not enough to validate a native resolver on a later image.
- Record-family counts are not a namespace oracle. The next validation unit is a
  raw FS-record body dump with enough fields to explain path, type, file
  identity, symlink target, hard-link grouping, and ordinary logical size.
- Negative body-field oracle results are sufficient to block Rust work. A Python
  probe that reconstructs paths but mismatches sparse logical size should be
  treated as an unresolved parser contract, not as a partial product success.
- Inconclusive body-field oracle results are also sufficient to block Rust body
  work when an earlier native context gate fails. `EX-14` should lead to a
  focused checkpoint-context replay that preserves the offending context before
  another xfield-layout fixture is attempted.
- Body-parser promotion now requires both the positive EX-13 replay under the
  source-backed xfield rule and synthetic negative fixtures for malformed
  variable-length fields, xfields, xattrs, and cross-record inconsistencies.

## Oracle Matrix

- `Raw single-volume namespace`:
  POSIX/API traversal of the same chosen volume or stable mounted view.
- `Logical size`:
  public file metadata APIs and tools that report logical size for the chosen
  files and directories.
- `Allocated size`:
  public metadata APIs only for explicitly scoped cases; do not generalize to
  clone-, compression-, or snapshot-heavy semantics without proof.
- `Physical/shared/exclusive accounting`:
  metric-specific comparison only after a probe defines whether public tools,
  raw extents, extent-reference records, or product policy owns the metric.
- `Incremental correctness`:
  compare incremental output against a fresh full reparse of the same selected
  state.
- `Boot-root or merged namespace semantics`:
  only use a user-visible macOS root oracle when that exact semantic mode is the
  question under test.
- `Checkpoint scanner unit boundary`:
  source-backed synthetic block images may validate magic/type/checksum,
  descriptor-layout rejection, short-read errors, and highest-XID selection, but
  not real-source support.
- `Checkpoint-map integrity`:
  detached proof images plus synthetic malformed rings validate
  checkpoint-map/ephemeral-object handling before OMAP lookup.
- `OMAP lookup`:
  same-run raw media plus identity evidence validate `(omap context, oid,
  selected_xid)` mapping before FS-record parsing; pinned identity artifacts are
  valid only when their raw media is also preserved or regenerated.
- `Historical OMAP lookup blocker`:
  if raw media that produced pinned identities is absent, that specific replay
  route must be blocked rather than compared against a different image. `EX-12`
  superseded its initial blocker by generating raw media and identity evidence
  in the same probe run.
- `FS-record body oracle`:
  same-run mounted/POSIX namespace and logical-size facts validate native
  `DIR_REC`, `INODE`, `XATTR`, `SIBLING_LINK`, `SIBLING_MAP`, and dstream field
  dumps only when the selected scan state is declared.
- `Python-first parser experiments`:
  body-field uncertainty should be resolved in Python artifacts before the Rust
  implementation surface is widened.
- `Context-provider blockers for body experiments`:
  if the Rust scanner cannot publish `selected_checkpoint` and FS-tree root
  context for a same-run body fixture, the body oracle is `oracle_inconclusive`
  and the next experiment should isolate checkpoint-map/OMAP/root validation
  before retrying body decoding.
- `Name and case behavior`:
  row enumeration validates stored-name preservation; lookup-by-name requires a
  separate APFS name-hash fixture before Rust can claim case/normalization
  equivalence.
- `Compression logical-size precedence`:
  public logical-size APIs must be compared to each raw candidate size source
  rather than collapsed into a global size pass/fail.
- `Checkpoint scanner detached-image smoke`:
  the existing proof fixture may validate Rust `.dmg` source gating and candidate
  checkpoint discovery against a real APFS image, but not namespace,
  logical-size, OMAP, or FS-record correctness.

## Artifact Policy

Every `EX-*` note should save at least:

- environment manifest
- oracle definition
- exact setup and probe steps
- expected A/B observations
- observed results
- artifact list
- interpretation
- what the result rules out
- impact on related `RL-*` logs
- next exact step

Negative and inconclusive results remain valid evidence if they narrow the
design space.

## Exit Criteria
- Automated regression suite exists.
- Golden corpus exists.
- Incremental engine is continuously compared against full reparse output.

## Related Logs
- RL-01 Checkpoint Selection and Consistency
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-09 Cache Persistence and Invalidation
