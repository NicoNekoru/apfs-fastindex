# EX-06 OID, paddr, XID, checksum identity tracking

ID: EX-06
Title: OID, paddr, XID, checksum identity tracking
Date: 2026-04-25
Owner: GPT-5.5
Status: Complete
Result: Positive
Related RLs:
- RL-02 OMAP and Object Resolution
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-09 Cache Persistence and Invalidation

## Bottom line

This probe gives the first repo-owned identity trace across multiple APFS
mutations. It confirms the core cache-risk warning:

- the FS root tree's logical OID stayed stable at `1028`
- its resolved physical address changed after every mutation
- its object XID changed after every mutation
- its checksum and SHA-256 block hash changed after every mutation

In this corpus, `oid` alone is therefore not a safe cache key for parsed object
contents. A minimally defensible raw object identity must include at least OMAP
domain, logical OID, resolved physical address, object XID, and content
checksum/hash. The probe is intentionally not enough to define the final
incremental cache key; it narrows the next subtree-reuse experiment.

## Question

- What identity fields change across small create, append, rename, delete,
  recreate, and truncate mutations, and what does that rule out for cache keys?

## Hypothesis

- Hypothesis A: APFS copy-on-write updates keep logical OIDs stable while moving
  object contents to new physical addresses and XIDs, so bare `oid` is unsafe
  but a stronger tuple can distinguish changed objects.
- Hypothesis B: the observed identity fields are too noisy or insufficient to
  distinguish changed state, requiring whole-tree reparsing or stronger content
  fingerprints before any incremental work.

## Environment

- Host OS: recorded in `artifacts/generated/environment.json`
- Probe volume:
  - fresh 192 MiB APFS image
  - case-insensitive
  - unencrypted
  - mounted only for mutation and oracle capture
  - detached and reattached with `-nomount` for each raw identity dump
- Raw identity harness:
  - `artifacts/identitydump/`
  - uses `go-apfs` to resolve the volume OMAP, root FS tree, FS tree nodes, path
    entries, and record groups

## Oracle

The mounted oracle for each state is a POSIX/API walk of the same image before
detach. The raw identity dump is then captured from the detached image-backed
container.

This oracle is valid for the question because the experiment is not proving a
cache algorithm. It is checking whether raw identity observations line up with
the mounted namespace and mutation sequence for one pinned state at a time.

## Setup

- Added `artifacts/probe_ex06.py`.
- Added `artifacts/identitydump/` as a small Go raw identity dumper.
- Each mutation state is recorded as a separate JSON artifact.

## Probe Steps

1. Create a fresh APFS image.
2. For each mutation step:
   - mount image
   - apply one mutation
   - capture mounted oracle
   - detach image
   - reattach with `-nomount`
   - capture checkpoint candidates and raw identity dump
   - detach raw image
3. Mutations:
   - initial empty `work/` directory
   - create `work/alpha.txt`
   - append to `work/alpha.txt`
   - rename it to `work/renamed.txt`
   - create `work/beta.txt`
   - delete `work/beta.txt`
   - recreate `work/beta.txt`
   - truncate `work/renamed.txt`

## Expected Observations

### If Hypothesis A is true

- the FS root tree OID may remain stable
- physical address, object XID, checksum, or content hash should change when
  tree contents change
- file IDs should persist across rename and content mutation
- delete/recreate should not be assumed to reuse file IDs

### If Hypothesis B is true

- changed filesystem state might not produce detectable raw identity changes in
  the recorded tuple
- mounted namespace changes might appear without corresponding raw object or
  record-group changes
- the probe would force a stronger fingerprint or full reparse as the only safe
  near-term model

## Observed Results

- The state sequence covered `8` pinned raw states.
- Highest visible checkpoint XID advanced from `6` to `34`.
- The FS root tree logical OID stayed `1028` throughout.
- Across every adjacent state transition:
  - same OID: `true`
  - same paddr: `false`
  - same object XID: `false`
  - same checksum: `false`
  - same SHA-256 block hash: `false`
- The root tree resolved paddr changed:
  - `370 -> 396 -> 420 -> 444 -> 478 -> 507 -> 539 -> 565`
- The root tree object XID changed:
  - `4 -> 8 -> 12 -> 16 -> 20 -> 24 -> 28 -> 32`
- The file identity behavior matched namespace expectations:
  - `work/alpha.txt` kept file id `21` after append
  - the same file id `21` appeared at `work/renamed.txt` after rename
  - truncating `work/renamed.txt` kept file id `21` while changing logical size
    from `11` to `3`
  - deleted `work/beta.txt` had file id `28`
  - recreated `work/beta.txt` received file id `33`
- The FS tree grew from one node to three nodes after the rename step, giving
  the next subtree-reuse probe a small multi-node case to inspect.

## Artifacts Saved

- `artifacts/probe_ex06.py`
- `artifacts/identitydump/go.mod`
- `artifacts/identitydump/go.sum`
- `artifacts/identitydump/main.go`
- `artifacts/generated/environment.json`
- `artifacts/generated/00-initial-empty.json`
- `artifacts/generated/01-create-alpha.json`
- `artifacts/generated/02-append-alpha.json`
- `artifacts/generated/03-rename-alpha.json`
- `artifacts/generated/04-create-beta.json`
- `artifacts/generated/05-delete-beta.json`
- `artifacts/generated/06-recreate-beta.json`
- `artifacts/generated/07-truncate-renamed.json`
- `artifacts/generated/summary.json`
- `artifacts/generated/run.json`

## Interpretation

- Bare `oid` is ruled out as a content cache key. The root tree OID stayed stable
  while every content-bearing identity field changed.
- `(omap domain, oid, object_xid, paddr, checksum/hash)` is a better candidate
  for raw object identity, but still only a candidate. It needs subtree-level
  validation before it becomes an incremental cache contract.
- File identity and raw tree-node identity are different layers. File id `21`
  persisted across append, rename, and truncate, while FS tree object identity
  changed each time.
- This probe did not observe file-id reuse after delete/recreate; it observed a
  new file id. That is not proof that reuse never happens.
- The multi-node states created after the rename step are the right input for a
  focused subtree-reuse probe.

## What This Rules Out

- It rules out `node_cache[oid]` as a safe cache model for parsed APFS object
  contents.
- It rules out treating file-id stability as equivalent to unchanged metadata.
- It rules out jumping directly from copy-on-write theory to subtree skipping
  without checking paddr, XID, checksum/hash, and child-node behavior.

## Impact on RLs

- RL-02: object resolution must keep transaction and OMAP context attached to
  identity observations.
- RL-04: the cheapest plausible cache identity is stronger than bare OID and
  must include resolved object version/content fields.
- RL-09: persistent cache invalidation should treat any uncertainty in
  checkpoint continuity, object XID, paddr, checksum/hash, or parser version as
  a reason to avoid reuse.

## Next Exact Step

- Use the `03-rename-alpha` through `07-truncate-renamed` multi-node artifacts
  to design `EX-07`, which should test whether unchanged child-node identities
  correspond to unchanged subtree summaries.
