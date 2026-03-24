# APFS Indexer Research Index

Purpose:
Track unresolved technical questions for a high-performance APFS indexing engine.

Research rules:
- Treat every claim as one of:
  - Spec: backed by public documentation
  - Observation: confirmed empirically from disk images / live systems
  - Hypothesis: plausible but not yet proven
- Every experiment entry should record:
  - macOS version
  - APFS volume/container type
  - encrypted or unencrypted
  - case-sensitive or case-insensitive
  - mounted/unmounted state
  - sample image or device identifier
- Every document is a living log, not a final design doc.
- Final architecture decisions should not be made until exit criteria are satisfied.

Research tracks:
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

Priority order:
P0:
- RL-01
- RL-02
- RL-03
- RL-04
- RL-05
- RL-06
- RL-07
- RL-08
- RL-09
- RL-10

P1:
- RL-11
- RL-12
- RL-13