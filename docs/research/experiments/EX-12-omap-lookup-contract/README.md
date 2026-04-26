# EX-12 OMAP lookup contract

ID: EX-12
Title: OMAP lookup contract
Date: 2026-04-26
Owner: GPT-5.5
Status: Blocked
Result: Blocked by missing replayable raw media for identity oracles
Related RLs:
- RL-02 OMAP and Object Resolution
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation
- RL-10 Validation Corpus and Oracle
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

This experiment defines the first native OMAP proof gate. It must verify that
lookup is `(omap context, oid, selected_xid) -> mapping with greatest xid <=
selected_xid`, and that deleted, encrypted, no-header, crypto-generation,
wrong-domain, and failed object-validation cases fail closed before FS-record
parsing begins.

`EX-11` produced a validated checkpoint context for a generated proof fixture,
but that context is not tied to the `EX-06`/`EX-07` identity artifacts. Because
the raw images for those identity oracles were not preserved, `EX-12` is blocked
until the identity corpus is regenerated or future runs preserve replayable raw
media.

## Question

- For a pinned APFS state, can the resolver choose the correct OMAP domain and
  mapping for an object ID at `selected_xid`, and can it reject the important
  failure cases without reading wrong objects?

## Hypothesis

- Hypothesis A: In detached proof images, a native OMAP cursor can reproduce the
  paddr/object-XID identities already observed by `EX-06` and `EX-07` when it
  seeks `(oid, selected_xid)` and accepts the greatest key with matching `oid`
  and `xid <= selected_xid`.
- Hypothesis B: Native OMAP lookup exposes cursor, deletion, flag, snapshot, or
  domain behavior that requires narrowing the resolver contract before root
  discovery can proceed.

## Environment

Recommended first environment:

- detached unencrypted image-backed APFS sources from `EX-06` and `EX-07`
- validated checkpoint context from `EX-11` or an equivalent checkpoint-map
  validator
- no live mounted raw scans
- no encrypted source support
- no snapshot source support in the first run

Record:

- source ID and selected checkpoint XID
- OMAP domain: `container` or selected `volume`
- OMAP object header and flags
- OMAP tree root object identity
- lookup key `(oid, selected_xid)`
- returned OMAP key/value
- resolved object header/type/subtype/checksum

## Oracle

Use existing pinned identity artifacts:

- `EX-06` root-tree identity sequence for OID, paddr, object XID, checksum, and
  block hash across mutations.
- `EX-07` node identity and summary artifacts for multi-node FS-tree states.

This oracle is valid because the experiment is comparing native resolver output
to already captured pinned raw identity facts, not yet proving namespace output.

## Setup

- Add a native or probe-only OMAP dumper after `EX-11` is executable.
- Save one JSON artifact per pinned state under `artifacts/generated/`.
- Include a synthetic OMAP fixture for deleted and flag-failure cases if real
  artifacts do not contain those flags.

## Probe Steps

1. Load a validated checkpoint context.
2. Resolve the container OMAP object from the selected checkpoint context.
3. For each volume superblock OID, resolve through the container OMAP using
   `(container, volume_oid, selected_xid)`.
4. Resolve the selected volume OMAP and FS root tree through their proper
   domains.
5. For each target object from `EX-06`/`EX-07`, perform lookup by
   `(omap domain, oid, selected_xid)`.
6. Reject a candidate if:
   - no key with matching `oid` and `xid <= selected_xid` exists
   - the returned value has `OMAP_VAL_DELETED`
   - the value has unsupported encrypted/no-header/crypto-generation flags
   - the OMAP object has unsupported encrypting/decrypting/keyrolling flags
   - the resolved object has unexpected type/subtype
   - the resolved object checksum fails
   - the resolved object XID is newer than `selected_xid`
7. Compare native lookup output to pinned identity artifacts.

## Expected Observations

### If Hypothesis A is true

- Native lookup returns the same paddr/object-XID identities recorded by `EX-06`
  and `EX-07`.
- Wrong-domain lookups fail or produce expected type/subtype rejection.
- Synthetic deleted/unsupported-flag cases produce explicit hard-stop verdicts.

### If Hypothesis B is true

- Native lookup disagrees with existing pinned identity artifacts.
- The discrepancy identifies a narrower resolver rule or a missing checkpoint,
  B-tree, or OMAP flag condition.

## Observed Results

- Blocked before execution.
- `EX-11` prerequisite status:
  - validated checkpoint context available: yes
  - context source: generated proof fixture
  - limitation: context does not belong to the `EX-06`/`EX-07` identity corpus
- Identity oracle status:
  - `EX-06`/`EX-07` JSON identity artifacts available: yes
  - corresponding detached raw media available in repo: no
  - stale raw device paths available in JSON: yes, but not replayable
- Blocked verdict: `blocked_missing_raw_identity_media`

## Artifacts Saved

- `README.md`
- `artifacts/generated/oracle-contract.json`
- `artifacts/generated/blocker.json`
- `artifacts/generated/summary.json`

Future execution should save:

- `artifacts/probe_ex12.py` or equivalent native probe runner
- `artifacts/generated/environment.json`
- `artifacts/generated/<state-id>-omap-lookups.json`
- `artifacts/generated/<failure-case>.json`
- `artifacts/generated/summary.json`

## Interpretation

- FS-record parsing must wait until this OMAP lookup contract is either executed
  or replaced by an equivalent resolver proof.
- The experiment deliberately separates object resolution correctness from
  namespace reconstruction.
- Checkpoint-map validation alone is insufficient for OMAP lookup validation.
- A future run must keep the raw image and identity JSON paired, or the native
  lookup output cannot be compared to the oracle.

## What This Rules Out

- It rules out a global `oid -> paddr` resolver.
- It rules out using bare OID, latest-only lookup, or cross-domain lookup as a
  native parser shortcut.
- It rules out comparing native lookup on a fresh generated fixture to stale
  identities from a different image.

## Impact on RLs

- RL-02: turns the resolver contract into an executable proof gate.
- RL-04/RL-05/RL-09: keeps cache and subtree identity downstream of validated
  OMAP lookup.
- RL-10: reuses pinned identity artifacts as feature-specific oracles.
- RL-13: names unsupported OMAP/value flags as hard-stop verdicts.

## Next Exact Step

- Regenerate or preserve the `EX-06`/`EX-07` raw image/state corpus, then execute
  native OMAP lookup against the same image/state that produced the identity
  oracle.
