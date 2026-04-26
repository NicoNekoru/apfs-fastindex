# EX-07 Subtree reuse proof probe

ID: EX-07
Title: Subtree reuse proof probe
Date: 2026-04-25
Owner: GPT-5.5
Status: Complete
Result: Positive within lab corpus
Related RLs:
- RL-05 Subtree Reuse Correctness
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-07 Size and Space Accounting
- RL-09 Cache Persistence and Invalidation

## Bottom line

`EX-06` ruled out bare OID cache identity and produced the first multi-node FS
tree states for this repo. `EX-07` used that finding to test a narrower and
falsifiable reuse rule:

- a subtree summary may be reused only when the child node identity observed
  through the parent pointer is unchanged under the same OMAP domain and scan
  state
- node identity must include at least OID, object XID, paddr, checksum/hash, and
  expected type/subtype
- any changed ancestor, changed child pointer, changed child identity, changed
  parser version, or unsupported side metadata forces descent

The first execution produced a positive result in a detached image-backed lab
corpus:

- exact node-identity reuse produced zero false reuse across six adjacent
  transitions
- low-churn hot-directory mutations still left `65.9%` to `90.3%` of current
  FS-tree nodes reusable
- `stable-a/` and `stable-b/` raw path digests stayed unchanged throughout all
  hot-directory mutations

The goal is still not to prove a general APFS theorem. The result is strong
enough to keep subtree summary reuse as a viable post-v1 architecture track, but
not enough to encode it as production cache behavior before a native parser and
larger corpus exist.

## Question

- When a directory/file subtree appears unchanged between two pinned APFS states,
  which raw identity conditions are sufficient to reuse its parsed namespace and
  logical-size summary without descending into it?

## Hypothesis

- Hypothesis A: If a parent's child pointer resolves to the same child node tuple
  `(omap domain, oid, object_xid, paddr, checksum/hash, type/subtype)` in both
  states, then that child node's parsed record set and derived namespace/logical
  summary are unchanged for the narrow v1 metric.
- Hypothesis B: APFS balancing, relocation, side metadata, or summary dependencies
  can make the same-looking child identity insufficient, so subtree reuse must be
  more conservative or abandoned until stronger fingerprints exist.

## Environment

Recommended first environment:

- fresh unencrypted APFS image
- detached raw reads only for identity and summary comparison
- mounted mutations only between pinned raw states
- case-insensitive first, then case-sensitive if the first run is promising

The first execution should begin from the `EX-06` pattern:

- mount for mutation
- capture mounted oracle
- detach
- reattach `-nomount`
- dump raw tree/node identities and summaries
- detach

## Oracle

Use two oracles, each with a narrow purpose:

- Namespace/logical-size oracle:
  POSIX/API traversal of the mounted volume before detach.
- Incremental-correctness oracle:
  fresh full raw summary of the same pinned state, compared against the simulated
  reuse decision.

The experiment must not validate incremental output against a live drifting view.
Every comparison needs a named pinned state.

## Setup

Create a corpus that forces a multi-node FS tree and has visibly separable
subtrees:

- `stable-a/` with enough files to occupy one or more FS tree leaves
- `stable-b/` with enough files to occupy a different range
- `hot/` where mutations will occur
- `moved/` for cross-directory rename and move tests

Recommended fanout:

- start with `200` small files per top-level directory
- increase to `1000` only if the FS tree does not split into enough nodes
- record node count after setup before proceeding

Actual first-run setup:

- fresh unencrypted 384 MiB APFS image
- case-insensitive
- detached raw reads only
- mounted mutation/oracle phase before every detach
- `200` files each in `stable-a/`, `stable-b/`, and `hot/`
- `20` seed files in `moved/`
- `200` extra files added during the split/fanout transition

## Probe Steps

1. Build the baseline volume with `stable-a/`, `stable-b/`, `hot/`, and
   `moved/`.
2. Capture baseline full raw node identities, record groups, path entries, and
   per-directory logical summaries.
3. Mutate one file inside `hot/` by appending data.
4. Capture the next pinned state and compare:
   - changed node identities
   - unchanged node identities
   - per-directory summary changes
   - whether stable subtrees remained reusable
5. Rename one file within `hot/`.
6. Move one file from `hot/` to `moved/`.
7. Delete and recreate a file in `hot/`.
8. Add enough files to trigger split or rebalance if it has not happened yet.
9. Delete enough files to trigger merge or compaction if practical.
10. For every adjacent state, simulate an incremental scan:
    - reuse summaries only for nodes whose identity tuple is unchanged
    - force descent for changed or missing tuples
    - compare simulated result to fresh full raw summary

## Expected Observations

### If Hypothesis A is true

- mutations in `hot/` change the leaf/ancestor path that contains `hot/`
- child nodes covering `stable-a/` and `stable-b/` remain byte-identical or have
  stable identity tuples when their records are untouched
- reused subtree summaries match fresh full raw summaries exactly
- split/merge events reduce reuse granularity but do not create false reuse

### If Hypothesis B is true

- a reused node produces a namespace or logical-size mismatch against the fresh
  full raw summary
- a logically unchanged subtree is relocated or rewritten enough that identity
  reuse is ineffective
- metadata outside the child node changes a summary the algorithm would have
  reused
- the safe rule collapses to full descent for common low-churn mutations

## Required Artifacts

Save at least:

- setup manifest with file counts, volume type, block size, and APFS feature
  flags
- mutation script
- mounted oracle JSON per state
- full raw identity dump per state
- per-node summary JSON per state
- simulated reuse decision per state transition
- fresh full raw summary per state
- comparison report for simulated incremental output versus full raw output
- a final `summary.json` with pass/fail for each transition

Saved artifacts:

- `artifacts/probe_ex07.py`
- `artifacts/generated/environment.json`
- `artifacts/generated/00-baseline.json`
- `artifacts/generated/01-append-hot.json`
- `artifacts/generated/02-rename-hot.json`
- `artifacts/generated/03-move-hot.json`
- `artifacts/generated/04-delete-recreate-hot.json`
- `artifacts/generated/05-add-hot-fanout.json`
- `artifacts/generated/06-delete-hot-fanout.json`
- `artifacts/generated/summary.json`
- `artifacts/generated/run.json`

## Documentation Requirements

The completed `README.md` must answer:

- Which node identity tuple was tested?
- Which transitions allowed reuse?
- Which transitions forced descent?
- Did any reused summary disagree with a full raw summary?
- How much of the tree was reusable under low churn?
- Did split/merge or relocation make reuse too weak to matter?
- What exact reuse theorem, if any, is now permitted?

## Candidate Reuse Theorem To Test

Do not encode this as a design until the experiment passes:

- For raw single-volume namespace plus logical size, a parsed node summary can be
  reused between pinned states only when:
  - source volume identity is the same
  - parser version and summary schema are the same
  - OMAP domain is the same
  - parent child pointer still targets the same logical child OID
  - OMAP lookup for that child yields the same object XID and paddr
  - child object checksum/hash and type/subtype match
  - no requested output metric depends on side metadata outside that subtree

If any condition fails, descend or fully reparse.

## Observed Results

- The state sequence covered `7` pinned raw states.
- Highest visible checkpoint XID advanced from `6` to `30`.
- The corpus started with:
  - `624` raw entries
  - `61` FS-tree nodes
  - `60` leaf node summaries
- The add-fanout transition grew the tree to:
  - `824` raw entries
  - `82` FS-tree nodes
  - `81` leaf node summaries
- The delete-fanout transition reduced it to:
  - `724` raw entries
  - `75` FS-tree nodes
  - `74` leaf node summaries
- Across all adjacent transitions, exact node-identity reuse produced:
  - `false_reuse_count = 0`
  - no reused node summary hash mismatch
  - unchanged `stable-a/` raw path digest
  - unchanged `stable-b/` raw path digest

Reuse by transition:

- `00-baseline -> 01-append-hot`: `52 / 61` current nodes reusable (`85.2%`)
- `01-append-hot -> 02-rename-hot`: `53 / 62` current nodes reusable (`85.5%`)
- `02-rename-hot -> 03-move-hot`: `56 / 62` current nodes reusable (`90.3%`)
- `03-move-hot -> 04-delete-recreate-hot`: `55 / 63` current nodes reusable
  (`87.3%`)
- `04-delete-recreate-hot -> 05-add-hot-fanout`: `54 / 82` current nodes
  reusable (`65.9%`)
- `05-add-hot-fanout -> 06-delete-hot-fanout`: `63 / 75` current nodes
  reusable (`84.0%`)

## Interpretation

- The tested identity tuple is sufficient in this corpus to avoid false reuse
  for node-local namespace/logical-size summaries.
- Stable subtrees remained reusable even when mutations targeted `hot/`,
  including append, rename, move, delete/recreate, fanout growth, and fanout
  deletion.
- Tree growth reduced reuse granularity but did not collapse the model. The
  add-fanout transition had the lowest reuse fraction because it created many
  new hot-directory nodes.
- The result supports a future conservative incremental design where unchanged
  exact node identities can reuse parsed summaries and every changed/missing
  identity forces descent or full parse.
- This is not a production cache theorem yet. The tool still uses `go-apfs`, the
  corpus is image-backed and single-volume, and no physical/shared accounting or
  live mounted raw scan semantics were requested.

## What This Rules Out

- It rules out the fear that every small mutation necessarily rewrites the whole
  FS tree in this lab image-backed corpus.
- It rules out abandoning subtree summary reuse before the native parser exists.
- It does not rule out APFS layouts or product modes where relocation, snapshots,
  compression side metadata, volume groups, or live reads make reuse ineffective
  or unsafe.

## Impact on RLs

- RL-05: turns the broad subtree-reuse question into a falsifiable identity and
  summary-reuse test.
- RL-04: consumes the candidate identity tuple from `EX-06`.
- RL-07: keeps reuse scoped to namespace plus logical size; physical/shared
  accounting would need a different theorem.
- RL-09: defines the first simulated incremental oracle: fresh full raw summary
  of the same pinned state.

## Next Exact Step

- Keep the exact node-identity rule as the only permitted candidate for future
  subtree summary reuse:
  `(omap domain, oid, object_xid, paddr, checksum/hash, type/subtype, parser
  version, summary schema)`.
- Do not implement persistent incremental caching yet. First replace the
  `go-apfs` proof backend with native root/FS-record parsing and rerun this
  corpus through native summaries.
- Add a larger follow-up corpus only after native parsing exists, with
  case-sensitive images, larger fanout, clone/sparse/compressed candidates, and
  repeated churn across multiple top-level directories.
