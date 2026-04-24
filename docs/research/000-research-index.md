# APFS Indexer Research Index

Purpose:
Track unresolved technical questions for a high-performance APFS indexing
engine, and make every research step durable enough that future humans and
agents do not need to reconstruct context from scratch.

## Research Rules

- Treat every claim as one of:
  - `Spec`: backed by public documentation
  - `Observation`: confirmed empirically or by converging implementations
  - `Hypothesis`: plausible but not yet proven
- Do not let raw notes become the canonical record. Distill them back into the
  appropriate `RL-*` logs.
- Every artifact must change at least one of:
  - what we believe
  - what we rule out
  - what we do next
- If an artifact does not update an `RL-*` log or define the next exact step, it
  is probably too vague.

## Artifact Types

### `RL-*` Research Logs

Use `RL-*` files for durable question-led synthesis:

- the core question
- why it matters
- current assumptions
- known facts
- open unknowns
- risks
- planned probes
- evidence log
- interim decisions
- exit criteria

`RL-*` files are living synthesis, not raw evidence dumps.

### `SR-*` Source Reviews

Use `sources/SR-###-slug.md` for compact reviews of external evidence on one
coherent topic.

Every `SR-*` file must:

- open with `Bottom line`
- declare `Related RLs`
- separate `Spec`, `Observation`, and `Hypothesis`
- end with `Decision impact`

Current source reviews:

- `SR-001` V1 support boundary
- `SR-002` checkpoint, OMAP, and root-discovery contract

### `EX-*` Experiment Notes

Use `experiments/EX-###-slug/README.md` for one controlled probe or mutation
program.

Each experiment directory may contain:

- `README.md` for distilled results
- `artifacts/` for manifests, scripts, raw outputs, and diff snapshots

Every `EX-*` note must record:

- environment
- oracle
- exact setup
- exact steps
- expected outcomes for competing hypotheses
- observed results
- interpretation
- what the experiment rules out
- impact on related `RL-*` logs

Negative or inconclusive results are first-class artifacts.

## Oracle Policy

Validation is feature-specific.
Do not speak of "the oracle" as if one tool answers everything.

Current oracle policy:

- namespace oracle:
  POSIX/API traversal of the chosen volume or stable view
- logical-size oracle:
  public file metadata APIs and tools that report logical size
- allocated-size oracle:
  public file metadata APIs only for explicitly supported cases
- incremental oracle:
  fresh full reparse compared against the incremental path
- boot-root semantics oracle:
  user-visible macOS namespace only when the product mode explicitly targets it

Every experiment must state which oracle it uses and why that oracle is valid
for the exact question being tested.

## Documentation Layout

- `RL-*` files: distilled rolling synthesis
- `sources/`: external source reviews
- `experiments/`: controlled probes and their artifacts
- templates:
  - `001-research-template.md`
  - `002-source-review-template.md`
  - `003-experiment-template.md`

Narrative-tightness rule:

- each new artifact should answer one primary question
- keep raw notes in `artifacts/`
- keep durable conclusions in `README.md`
- summarize the implication back into the relevant `RL-*` logs

## Staged Gates

The repo should think in staged gates, not a flat P0/P1 list.

Gate A: minimally correct v1 parser

- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-03 FS Tree Topology and Required Records
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-10 Validation Corpus and Oracle

Gate B: support boundary

- RL-08 Live Volume, Encryption, and Read Path
- RL-13 Format Drift, Compatibility, and Fallback

Gate C: safe incremental scanning

- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness
- RL-09 Cache Persistence and Invalidation

Gate D: broader product semantics and optimization

- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-12 Performance Model and Optimization

## Current Experiment Tracks

- `EX-01` live checkpoint consistency and runtime boundary
- `EX-03` required-record taxonomy for narrow v1

## Research Tracks

- RL-01 Checkpoint Selection and Consistency
- RL-02 OMAP and Object Resolution
- RL-03 FS Tree Topology and Required Records
- RL-04 Node Identity, Cache Keys, and OID Reuse
- RL-05 Subtree Reuse Correctness
- RL-06 Namespace Reconstruction
- RL-07 Size and Space Accounting
- RL-08 Live Volume, Encryption, and Read Path
- RL-09 Cache Persistence and Invalidation
- RL-10 Validation Corpus and Oracle
- RL-11 Snapshots, Volume Groups, and Firmlinks
- RL-12 Performance Model and Optimization
- RL-13 Format Drift, Compatibility, and Fallback