# EX-08 Read path support matrix

ID: EX-08
Title: Read path support matrix
Date: 2026-04-26
Owner: GPT-5.5
Status: Partially executed
Result: Safe host cells reinforce narrow raw-mode allowlist
Related RLs:
- RL-08 Live Volume, Encryption, and Read Path
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-13 Format Drift, Compatibility, and Fallback

## Bottom line

This experiment defines the support-matrix probe that must happen before raw
mode is broadened beyond detached/stable APFS sources. The first safe-host run
executed three cells and reinforced the current support boundary:

- detached unencrypted APFS `.dmg`: `supported` for narrow v1 proof work
- mounted unencrypted APFS `.dmg`: `readable_not_supported`
- startup container unprivileged raw read: `blocked_privilege`

It deliberately separates four verdicts that are easy to conflate:

- `readable`: raw bytes can be opened and sampled
- `parsable`: checkpoint/root discovery and narrow raw walk can run
- `validatable`: output can be compared to a named oracle for the requested mode
- `supported`: the source is inside the product allowlist for that mode

Current evidence supports one tested cell:

- detached unencrypted image-backed APFS is `supported` for narrow v1 proof work
  after `EX-03` and `EX-04`

Mounted image-backed APFS is currently only `readable`/`probeable`; `EX-05`
showed that live latest-state raw output is not supported under churn, and the
first `EX-08` safe-host run found a quiescent mounted-image mismatch as well.
The current proof backend therefore remains unsuitable as a mounted-source
support claim.

## Question

- Which APFS source types can this project read, parse, validate, and support
  for raw single-volume namespace plus logical size?

## Hypothesis

- Hypothesis A: Several common APFS runtime sources can be raw-supported after
  simple source-gate checks.
- Hypothesis B: Raw support must remain narrow; many sources are readable or
  partially parsable but still require fallback because they cannot be pinned,
  decrypted, validated, or mapped to the requested product semantics.

## Environment

Each matrix run must record:

- host model and CPU family
- macOS version
- `diskutil apfs list` output
- source path and source type
- whether the source is mounted
- encryption state if visible
- volume role, volume group, snapshot/sealed flags where visible
- command user and privilege level
- parser/probe version and commit if available

## Oracle

Each cell needs an oracle appropriate to its requested mode:

- detached image-backed single volume:
  mounted oracle captured before detach, then raw walk after detach
- mounted non-startup volume:
  named stable snapshot/API oracle if available; otherwise cell remains
  `readable` or `parsable`, not `supported`
- startup/System/Data volume:
  user-visible namespace oracle only if the product mode is merged-root output;
  raw single-volume output must not be compared as if it were Finder-visible `/`
- boot-root / merged namespace:
  blocked until a separate product mode exists. A future oracle must record
  mounted System/Data/snapshot identities, volume roles, volume-group UUIDs,
  `/usr/share/firmlinks`, and user-visible POSIX/API output.
- encrypted volume:
  oracle is only valid after documenting unlock state and whether raw metadata
  bytes are actually decrypted/readable

## Setup

Create one `artifacts/generated/<source-id>.json` per tested source.

Minimum fields:

```json
{
  "source_id": "detached-unencrypted-dmg",
  "source_class": "detached_image",
  "requested_mode": "raw_single_volume_namespace_logical_size",
  "raw_readable": null,
  "checkpoint_discovery": null,
  "root_discovery": null,
  "raw_walk": null,
  "oracle_available": null,
  "comparison_matched": null,
  "support_verdict": "pending",
  "fallback_reason": null
}
```

Future matrix artifacts should also record these allowlist groups:

- `source_gate_facts`:
  source class, mounted state, privilege level, encryption/unlock visibility,
  source path shape, and oracle availability.
- `container_facts`:
  block size, container UUID, container flags/features, checkpoint descriptor
  layout, Fusion/tier indicators, and container OMAP object ID.
- `volume_facts`:
  volume OID/UUID, role, volume-group UUID, feature masks, incompatible masks,
  volume flags, metadata crypto state, root/OMAP/extent/snapshot tree fields,
  case/normalization mode, and snapshot/revert fields.
- `mode_gate_verdicts`:
  checkpoint-scanner-safe, checkpoint-context-safe, OMAP-root-safe,
  namespace-logical-size-safe, and product-supported.

## Probe Steps

For each available source:

1. Record host and APFS environment facts.
2. Attempt a minimal raw read of block zero or the selected raw container.
3. Attempt checkpoint descriptor discovery.
4. Attempt root discovery/raw walk if the source is inside the experiment safety
   boundary.
5. Capture the correct oracle for the requested mode.
6. Compare raw output to oracle when the oracle is valid.
7. Assign a verdict:
   - `supported`
   - `readable_not_supported`
   - `parsable_not_validated`
   - `fallback_required`
   - `blocked_unavailable_hardware`
   - `blocked_privilege`
   - `not_tested`

## Matrix Cells

| Source class | Current evidence | Required probe | Current verdict |
| --- | --- | --- | --- |
| Detached unencrypted APFS `.dmg` | `EX-03`, `EX-04` matched oracle | Repeat as control cell | `supported` for narrow v1 proof |
| Mounted unencrypted APFS `.dmg` | `EX-01`, `EX-05` raw-readable but latest state drifts | Test true pinned-XID resolver when available | `readable_not_supported` |
| External unencrypted APFS volume | Not executed in repo | Raw read, checkpoint/root discovery, mounted/stable oracle | `not_tested` |
| External encrypted APFS volume | `SR-004` says encryption is a hard gate until proven | Unlock-state and raw metadata readability probe | `not_tested` |
| Internal startup container | `EX-01` unprivileged raw read failed on this host | Privileged/non-privileged access matrix, no parser traversal by default | `blocked_privilege` for unprivileged raw mode |
| System/Data volume group | `SR-004` says merged root is distinct product semantics | API/firmlink comparison, not raw v1 | `fallback_required` for raw v1 |
| Signed/sealed system volume | `SR-004` identifies snapshot/seal semantics | Read-only facts only unless product mode is defined | `fallback_required` for raw v1 |
| Snapshot source | Source review only | Entitlement/API availability and oracle probe | `not_tested` |
| Fusion or multi-device APFS | Source review only | Hardware-specific source-gate probe | `blocked_unavailable_hardware` |

## Allowlist Gate Semantics

- `checkpoint_scanner_safe` means block-zero and checkpoint descriptor scanning
  can run and report candidates. It does not imply namespace output.
- `checkpoint_context_safe` means checkpoint-map and ephemeral-object validation
  has passed for the selected checkpoint.
- `omap_root_safe` means container OMAP, volume superblock, volume OMAP, and FS
  root discovery are validated for the selected XID.
- `namespace_logical_size_safe` means required FS records and logical-size fields
  are supported for the requested single-volume mode.
- `supported` means all required gates passed and the output was validated
  against the correct oracle for the requested product semantics.

## Expected Observations

### If Hypothesis A is true

- More cells reach `supported` with simple source-gate checks.
- Valid oracles are available for mounted/live modes without special entitlements.
- Raw output can be tied to one named state.

### If Hypothesis B is true

- Several cells are raw-readable but not validatable.
- Encryption, privilege, snapshot, volume-group, or live-state constraints force
  fallback.
- The support matrix must distinguish lab probes from product support.

## Observed Results

- First safe-host run completed on 2026-04-26 and wrote results under
  `artifacts/generated/`.
- Detached unencrypted APFS `.dmg`:
  - raw readable: `true`
  - checkpoint discovery: `true`
  - root discovery / proof raw walk: `true`
  - oracle available: `true`
  - comparison matched: `true`
  - support verdict: `supported`
  - observed fixture paths: `7`
  - highest checkpoint XID: `15`
- Mounted unencrypted APFS `.dmg`, quiescent control:
  - raw readable: `true`
  - checkpoint discovery: `true`
  - root discovery / proof raw walk: `true`
  - oracle available: `true`
  - comparison matched: `false`
  - support verdict: `readable_not_supported`
  - mismatch shape: raw output missed `dst/link.txt` and `dst/moved.txt`, and
    unexpectedly reported pre-move `src/base.txt`
  - highest checkpoint XID: `6`
- Startup container, unprivileged raw-read attempt:
  - raw readable: `false`
  - support verdict: `blocked_privilege`
  - error: `PermissionError: [Errno 1] Operation not permitted: '/dev/rdisk3'`
  - root facts included FileVault/encryption enabled, signed system snapshot,
    volume group ID, sealed root, and non-writable `/`
- Existing evidence imported into the matrix:
  - `EX-03` and `EX-04`: detached unencrypted image-backed APFS is supported for
    narrow v1 proof work.
  - `EX-05`: mounted unencrypted image-backed APFS is raw-readable but not
    supported by current latest-state raw traversal under churn.
  - `EX-01`: unprivileged raw read of this host's startup container failed with
    `Operation not permitted`.

## Artifacts Saved

- `README.md`
- `artifacts/generated/environment.json`
- `artifacts/generated/detached-unencrypted-dmg.json`
- `artifacts/generated/mounted-unencrypted-dmg-quiescent.json`
- `artifacts/generated/startup-container-unprivileged-raw-read.json`
- `artifacts/generated/summary.json`
- `artifacts/generated/diskutil-apfs-list.txt`
- `artifacts/probe_ex08.py`

Future execution should add one `artifacts/generated/<source-id>.json` per
additional source class and keep unavailable hardware/media as explicit pending
cells rather than omitting them.

## Interpretation

- The first support-boundary execution reinforces the current narrow allowlist.
- Detached image-backed APFS remains the only `supported` raw-mode cell in this
  run.
- Mounted image-backed APFS is not supported by the current proof backend even
  when raw bytes, checkpoint discovery, and a proof raw walk are operational.
- A cell can be useful even when negative: `blocked_privilege` and
  `fallback_required` are product-shaping results.
- Hardware-sensitive cells should stay pending until the actual media exists.

## What This Rules Out

- It rules out a binary "APFS raw mode works/does not work" framing.
- It rules out treating raw readability as equivalent to product support.
- It rules out using startup-disk behavior to validate raw single-volume v1
  without a separate product semantics definition.
- It rules out broadening mounted-image raw mode based on quiescent readability
  or current proof-backend traversal.

## Impact on RLs

- RL-08: defines the concrete environment matrix and verdict vocabulary.
- RL-11: keeps System/Data/firmlink/snapshot behavior separate from raw v1.
- RL-13: turns support boundaries into explicit runtime verdicts.
- RL-08/RL-13: first safe-host run adds concrete `supported`,
  `readable_not_supported`, and `blocked_privilege` cells.

## Next Exact Step

- Keep raw v1 support limited to detached/stable sources.
- For mounted-source support, implement resolver-level selected-XID enforcement
  or a valid stable snapshot/API oracle, then rerun the mounted image cell.
- Add external unencrypted and encrypted media cells only when matching media is
  available, and record absent media as pending rather than inferred support.
- Extend future cell JSON with the allowlist field groups and gate semantics
  above before using the matrix to broaden support claims.
