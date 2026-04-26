# EX-07 Subtree reuse proof probe

ID: EX-07
Title: Subtree reuse proof probe
Date: 2026-04-25
Owner: TBD
Status: Planned
Result: Inconclusive
Related RLs:
- RL-05 Subtree Reuse Correctness
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-07 Size and Space Accounting
- RL-09 Cache Persistence and Invalidation

## Bottom line

`EX-06` ruled out bare OID cache identity and produced the first multi-node FS
tree states for this repo. `EX-07` should use that finding to test a narrower
and falsifiable reuse rule:

- a subtree summary may be reused only when the child node identity observed
  through the parent pointer is unchanged under the same OMAP domain and scan
  state
- node identity must include at least OID, object XID, paddr, checksum/hash, and
  expected type/subtype
- any changed ancestor, changed child pointer, changed child identity, changed
  parser version, or unsupported side metadata forces descent

The goal is not to prove a general APFS theorem. The goal is to decide whether a
conservative subtree-skipping algorithm is worth designing after the narrow full
parser exists.

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

- Not run yet.

## Interpretation

- Pending execution.

## What This Rules Out

- Pending execution.

## Impact on RLs

- RL-05: turns the broad subtree-reuse question into a falsifiable identity and
  summary-reuse test.
- RL-04: consumes the candidate identity tuple from `EX-06`.
- RL-07: keeps reuse scoped to namespace plus logical size; physical/shared
  accounting would need a different theorem.
- RL-09: defines the first simulated incremental oracle: fresh full raw summary
  of the same pinned state.

## Next Exact Step

- Implement the mutation and identity-summary harness by extending the
  `EX-06` identity dumper with per-node namespace/logical-size summaries and a
  simulated reuse decision report.
