# SR-019 Allocated-size Source Precedence

Status: Complete
Date: 2026-05-16
Type: Source Review
Related RLs:
- RL-03
- RL-06
- RL-07
- RL-10
- RL-13

## Bottom line

For R2-A's per-file *allocated bytes* product mode the only oracle-able
source-of-truth per inode is `j_dstream_t.alloced_size`. The candidate
precedence is:

1. regular file with `INO_EXT_TYPE_DSTREAM`: pick
   `j_dstream_t.alloced_size`. This is what the Linux kernel publishes
   as `inode->i_blocks << 9` and what The Sleuth Kit returns as
   `_size_on_disk`. It is per-stream and *clone-agnostic*: two clones
   of the same content each carry the same `alloced_size`; the sum
   over a clone family over-counts shared extents by `(refcnt - 1) *
   shared_bytes`. Apple's spec leaves this behaviour intentional.
2. regular file with no dstream xfield and a `com.apple.decmpfs`
   xattr: **fail closed** for v1. The actual on-disk footprint is
   distributed between the inline decmpfs payload and a possible
   `com.apple.ResourceFork` dstream. No public reader (libfsapfs, TSK,
   apfs-fuse, go-apfs, dissect.apfs, linux-apfs-rw) exposes a
   `st_blocks`-equivalent value for decmpfs files that has been
   oracle-validated against macOS; the most aggressive reader
   (linux-apfs-rw) actively *misreports* `i_blocks` as the rounded
   *uncompressed* size, which is not what macOS publishes.
3. symlink: zero. Targets are stored in `com.apple.fs.symlink` (or
   `com.apple.fs.symlink-name` on some macOS versions) as embedded
   xattr bytes; there is no per-inode dstream, no extent, and macOS
   reports `st_blocks = 0` for symlinks.
4. directory: zero. Directories own no data extents in v1's product
   mode; the FS-tree blocks that hold dir_rec entries are accounted to
   the volume's B-tree, not to any inode's `j_dstream_t`.
5. anything else: fail closed.

The sum-of-`j_file_extent_val_t`-lengths candidate is *not* picked. It
is recoverable from the FS tree, but no open-source reader uses it as
a published per-file allocated metric; Apple does not equate it to
`alloced_size`; and `apfsck` enforces it only as an internal
consistency check (sum-of-extents == alloced_size for the same
dstream). The extent-reference tree
(`OBJECT_TYPE_BLOCKREFTREE` / `j_phys_ext_*`) is *also* not picked.
It is the per-volume authority on physical sharing (clones,
snapshots) and is the prerequisite for an exclusive-bytes or
shared-bytes metric, neither of which is part of R2-A.

## Scope

This review answers one question:

- Which on-disk APFS field should the parser emit as a per-file
  allocated-bytes column, and which cases should be reported as
  unclaimed?

Out of scope:

- exclusive-bytes / shared-bytes accounting across clones, hard
  links, and snapshots
- snapshot-retained bytes (bytes held only because a snapshot still
  references them)
- decompressed-on-disk reconciliation for `com.apple.decmpfs` and
  `com.apple.ResourceFork`
- container-level used/free accounting

The candidate that *would* settle clone attribution
(`j_phys_ext_val_t.refcnt` join across `j_file_extent_val_t`) is
listed in the source map below but is explicitly deferred. R2-A
emits a per-stream number; the exclusive/shared metrics get their
own SR / EX when their oracle exists.

## Sources reviewed

- Apple File System Reference PDF, retrieved 2026-05-16
  (document footer date 2020-06-22, the latest publicly published
  revision):
  <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf>
- `linux-apfs/linux-apfs-rw`, retrieved 2026-05-16, commit
  `628b6810e46bcdd423189d2c66295258e10090dc` (tag 0.3.20):
  <https://github.com/linux-apfs/linux-apfs-rw>
- `linux-apfs/apfsprogs`, retrieved 2026-05-16, commit
  `3721463ba7f539e532907bc1d10ed5b9a97d0449` (tag 0.2.1):
  <https://github.com/linux-apfs/apfsprogs>
- `libyal/libfsapfs`, retrieved 2026-05-16, commit
  `62ca6b2a8f7b48abe35d533c2ef97893184bf389`:
  <https://github.com/libyal/libfsapfs>
- `fox-it/dissect.apfs`, retrieved 2026-05-16, commit
  `8d8dbd2545ebb1d65c1cda144097ee15f783e233`:
  <https://github.com/fox-it/dissect.apfs>
- `sleuthkit/sleuthkit`, retrieved 2026-05-16, commit
  `463bbece6702dd5486f910ddc8a0216dc3640970`:
  <https://github.com/sleuthkit/sleuthkit>
- EX-13 / EX-16 / EX-19 local artifacts in this repository.

## Spec

- Apple defines `j_dstream_t` (Apple File System Reference, p. 106) as
  `{ uint64_t size; uint64_t alloced_size; uint64_t
  default_crypto_id; uint64_t total_bytes_written; uint64_t
  total_bytes_read; }`. `alloced_size` is described verbatim as
  *"The total space allocated for the data stream, including any
  unused space."* This is the entire normative description; Apple
  does **not** equate it to a sum of extent lengths and does not
  define an apportioning rule across clones.
- Apple defines `j_file_extent_val_t` (p. 104) as
  `{ uint64_t len_and_flags; uint64_t phys_block_num; uint64_t
  crypto_id; }` with `J_FILE_EXTENT_LEN_MASK = 0x00ffffffffffffff`
  (p. 105). The flag bits are reserved; Apple states *"There are
  currently no flags defined."* Sparse holes are represented as
  extents with `phys_block_num == 0`.
- Apple defines the extent-reference tree (p. 102–103) as
  `j_phys_ext_key_t` (key OID = the physical block address of the
  start of the extent, raw_type `APFS_TYPE_EXTENT` = 0x2) →
  `j_phys_ext_val_t { uint64_t len_and_kind; uint64_t
  owning_obj_id; int32_t refcnt; }`. The tree's purpose is per-block
  reference counting; *"The extent can be deleted when its
  reference count reaches zero."* Per Apple p. 55, every snapshot
  rotation moves the live extent-reference tree to the snapshot and
  starts a new one.
- Apple defines `INODE_WAS_EVER_CLONED = 0x00000400`
  (p. 81): *"If this flag is set, the blocks on disk that store
  this inode might also be in use with another inode. For example,
  when deleting this inode, you need to check reference counts
  before deallocating storage."* This is the only Apple-stated
  hook for cross-inode sharing; it does not redefine `alloced_size`.
- Apple defines `INO_EXT_TYPE_SPARSE_BYTES` (p. 112) as a separate
  per-inode xfield reporting how many bytes of the inode's logical
  extent have not been committed to disk. SR-017 already pinned this
  as an allocation hint, not a logical size; it is also not the
  allocated-size answer.

## Observation

### How each reader maps the on-disk fields

- **linux-apfs-rw** (kernel module that publishes `inode->i_blocks`):
  - `inode.c:866-868`: `inode->i_blocks =
    le64_to_cpu(dstream_raw->alloced_size) >> 9`. The dstream xfield
    is the single source of truth; no extent walk; the extent-
    reference tree is not consulted on the read path.
  - `inode.c:852-853`: if no DSTREAM xfield, both `i_size` and
    `i_blocks` default to 0. Symlinks fall through this branch
    because their target is stored as `com.apple.fs.symlink` xattr,
    not as a dstream (`dir.c:662-664`, `symlink.c:35-49`).
  - `inode.c:854-858` and `compress.c:461-478`: for inodes with
    `APFS_INOBSD_COMPRESSED`, the regular dstream xfield is skipped
    and `inode->i_blocks` is set to `round_up(uncompressed_size,
    512) >> 9` — i.e. Linux reports the *uncompressed* size as
    on-disk blocks for decmpfs files. This is a Linux-specific
    misreport (macOS does not publish this) and is the load-bearing
    reason v1 must fail closed on decmpfs.
  - `apfs.h:784-790` + `inode.c:1292, 1362`: on write,
    `alloced_size` is computed as
    `apfs_alloced_size(dstream) = round_up(ds_size, blocksize)`. The
    on-disk field is therefore a *coarse upper bound derived from
    logical size*, not a sum of extent lengths.
  - `extents.c:1816-1829`: `apfs_clone_file_range` copies
    `dst_ds->ds_size = src_ds->ds_size` and recomputes
    `alloced_size` from it. The clone destination reports the *full*
    logical size as if it owned the bytes; on-disk `alloced_size`
    does not shrink to reflect sharing.
- **apfsprogs / apfsck** (the same project's filesystem checker):
  - `apfsck/extents.c:240-241`: after walking every
    `j_file_extent_val_t` for a dstream, apfsck asserts
    `d_bytes == d_alloced_size`, where `d_bytes` is the sum of
    `len_and_flags & J_FILE_EXTENT_LEN_MASK` over all extents
    (`extents.c:527-533`). Mismatch is a hard error.
  - **Disagreement with the kernel module:** linux-apfs-rw writes
    `alloced_size = round_up(ds_size, blocksize)`, while apfsck
    enforces `alloced_size = Σ extent.len`. These agree only when
    every extent ends exactly at `ds_size` and the file has no
    truncate residue. The kernel can therefore *write* volumes that
    its companion checker flags as corrupt. Decision impact: the
    parser cannot trust either definition by itself; the oracle
    must be `st_blocks * 512`, not internal field consistency.
- **The Sleuth Kit** (`apfs_fs.cpp:119-125`, `apfs_fs.hpp:50,97`,
  `apfs_compat.cpp:785-788`):
  - `_size_on_disk = ds->alloced_size` verbatim, no refcount math,
    no extent walk. Used as TSK's `alloc_size` for both the
    `is_clone()` and non-clone branches — so summing TSK's
    `size_on_disk` across a clone family over-counts by `(refcnt -
    1) * shared_bytes`.
- **libfsapfs** (`libfsapfs_inode.c:719-779`,
  `libfsapfs_data_block_vector.c:163-205`,
  `libfsapfs_extent_reference_tree.c:140-357`,
  `libfsapfs_file_entry.c:4902-5088`):
  - Parses `alloced_size` into the in-memory dstream struct but
    drops it (debug-build-only). The public API
    (`include/libfsapfs.h.in:998`) exposes only
    `libfsapfs_file_entry_get_size`, which returns logical bytes.
    Extents are summed only into an I/O vector. The extent-
    reference tree is opened only under `HAVE_DEBUG_OUTPUT` and its
    entries are never parsed. For decmpfs the public size accessor
    returns the *uncompressed* logical size — same posture as
    linux-apfs-rw, same reason to fail closed in v1.
  - libfsapfs has no clone deduplication: two cloned inodes each
    carry the full per-extent record set; summing extents over both
    double-counts. `INODE_WAS_CLONED` / `INODE_WAS_EVER_CLONED` are
    parsed for debug labels only.
- **dissect.apfs** (`dissect/apfs/objects/fs.py:579-593,717-726`,
  `dissect/apfs/c_apfs.py:653-660`, `dissect/apfs/stream.py:39-84`,
  `dissect/apfs/c_apfs.py:610-620`):
  - `INode.size` returns logical bytes only:
    `INODE_HAS_UNCOMPRESSED_SIZE` -> inode `uncompressed_size`,
    else decmpfs header, else `dstream.size`. **Never reads
    `alloced_size`.** `j_phys_ext_val_t` is decoded into a struct
    but no code in `dissect/apfs/` joins it against extents for
    size attribution.
- **apfs-fuse** (`ApfsLib/ApfsDir.cpp:262-272`,
  `apfsfuse/ApfsFuse.cpp:187-194`): decodes `j_dstream_t.alloced_size`
  into `Inode::ds_alloced_size` but the FUSE `stat` binding has the
  `st_blocks = rec.sizes.size_on_disk / 512` line **commented out**.
  The field is decoded into the C++ struct, never surfaced through
  `stat(2)`. Clones, sparse, and decmpfs receive no special
  treatment for allocated bytes; symlinks fall through to
  `st_size = 0`. The extent-reference tree is opened for sealed-
  volume reads but never consulted for per-file accounting.
- **go-apfs** (`types/dstream.go:88-104`,
  `cmd/apfs/cmd/.../apfs.go:381,410,565`): decodes
  `JDstreamT.AllocedSize` and prints it from `String()` but no code
  path returns it to a caller. `RegFile.Size` is populated from
  `JDstreamT.Size`, never from `AllocedSize`.
  `JFileExtentValT.Length()` is consumed per-extent inside read I/O
  loops, never summed. `j_phys_ext_val_t.RefCount` is parsed but
  never read for byte attribution.

The net pattern across all six readers:
`j_dstream_t.alloced_size` is the only on-disk per-stream
"allocated bytes" field, every reader decodes it, but only TSK and
linux-apfs-rw surface it to callers, only linux-apfs-rw publishes
it as the kernel `st_blocks` source, and *none* of the readers
makes any per-inode allocated-bytes claim for clones, sparse, or
decmpfs that has been validated against macOS's `st_blocks`. The
candidate set is unambiguous (one field) and the unknowns are
case-class boundaries (compressed, snapshot), not field choice.

### How the on-disk fields relate to the public `st_blocks` oracle

- For ordinary, sparse, clone, and hard-linked regular files on
  macOS, `st_blocks * 512` reports the per-inode allocated bytes.
  `j_dstream_t.alloced_size` is the only candidate that publishes
  this number directly: linux-apfs-rw uses it for `inode->i_blocks`
  and TSK uses it for `_size_on_disk`, both unmodified. EX-22 must
  validate the macOS `st_blocks * 512` parity case by case because
  Apple does not document the relationship; the same-run fixture
  pattern from EX-19 is the right vehicle.
- For symlinks, both candidates collapse to zero (`alloced_size`
  defaults to 0 when no dstream xfield is present; macOS reports
  `st_blocks = 0` for symlinks). The rule emits 0 and the oracle
  matches by construction.
- For decmpfs-compressed regular files the readers disagree:
  - **linux-apfs-rw** publishes the *uncompressed* size as
    `i_blocks` (decompressed bytes / 512), which is wrong against
    macOS.
  - **macOS** publishes whatever the kernel allocated for the
    decmpfs xattr (typically one xattr block) plus the resource
    fork if present.
  - No reader exposes a `st_blocks`-equivalent that has been
    validated against macOS for the resource-fork path.
  v1 must therefore **fail closed** for decmpfs files — emit no
  allocated_size, list the column as `not_claimed` for compressed
  rows, and let the caller decide how to surface "compressed."

### The extent-reference tree's actual role

- Apple (p. 55) ties the extent-reference tree to snapshot
  rotation: every new snapshot becomes the owner of the previous
  live extent-ref tree, and a fresh empty one is started. This is
  the per-volume bookkeeping for "which physical blocks are held
  by a snapshot." It is *not* a per-inode allocated-bytes table.
- No reader uses the extent-reference tree for the per-file
  allocated metric. libfsapfs (`extent_reference_tree.c:140-357`)
  reads only the tree's root header to validate the volume; TSK
  parses `refcnt` into a struct but never consults it for
  `size_on_disk`; linux-apfs-rw and dissect.apfs likewise leave it
  off the read path. The tree is the necessary input for a
  *future* exclusive-bytes / shared-bytes metric — and the future
  snapshot-retained-bytes metric — but it is not on R2-A's
  emission path.

## Hypothesis

- For the EX-19 fixture shape (ordinary, sparse, clone, hard link,
  symlink, ditto-compressed), per-inode
  `st_blocks * 512 == j_dstream_t.alloced_size` for the
  ordinary / sparse / clone / hard-link cases. Symlink: both 0.
  Compressed: `st_blocks * 512` is the *actual* on-disk footprint
  (xattr block + possible resource-fork bytes); `alloced_size`
  from any present dstream is unrelated to it. The Rust slice
  should emit `Some(alloced_size)` only for cases (1) and (3) and
  emit `None` (with `allocated_size` listed in `not_claimed`) for
  case (2). The hard-link case is per-inode and needs no separate
  column.
- Aggregate policy: directory `unique_inode_allocated_total`
  should mirror the SR-009 unique-inode logical policy — each
  inode contributes its allocated_size to its containing directory
  exactly once, regardless of how many hard-link paths reference
  it. If any contributing inode in the subtree has
  `allocated_size == None` (decmpfs fail-closed), the aggregate
  emits `None` so the consumer cannot accidentally treat a
  partial total as authoritative.

## Open Limits

- Spec gap: Apple defines `alloced_size` only as "total space
  allocated for the data stream, including any unused space." It
  gives no closed-form equation, no clone-apportioning rule, and
  no cross-snapshot semantics. The R2-A oracle is therefore the
  external public number (`st_blocks * 512`), not any internal
  field equation.
- Reader disagreement: linux-apfs-rw writes
  `alloced_size = round_up(ds_size, blocksize)` while apfsck
  enforces `alloced_size = Σ extent.len`. A volume produced under
  one rule is "corrupt" under the other. R2-A does not need to
  resolve this — the macOS oracle is independent — but a future
  cross-platform mode will.
- decmpfs is genuinely undefined for a public oracle:
  `linux-apfs-rw` publishes uncompressed bytes as `i_blocks`,
  `apfs-fuse` reports 0, macOS reports the xattr+resource-fork
  footprint. v1 must fail closed; an EX-22b (or a future
  compression accounting probe) is the right place to chase this.
- Resource-fork compression (decmpfs methods 4 and 8): same fail-
  closed posture. The resource-fork stream's own
  `j_dstream_t.alloced_size` could in principle be summed against
  the decmpfs xattr block, but no reader does this and no oracle
  has been validated.
- Snapshot interaction: SR-019 makes no claim about
  snapshot-retained bytes. The extent-reference tree is the place
  that work begins (it tracks which blocks a snapshot still
  references); R2-A leaves it untouched.

## Decision impact

- `RL-07`: physical/allocated-bytes precedence is now specific
  enough to attempt a Rust emission gate. R2-A's product column is
  `allocated_size: Option<u64>`. Step 1 emits `Some(alloced_size)`;
  steps 2 and below emit `None` (and the source is listed in
  `not_claimed`).
- `RL-10`: the EX-22 fixture must capture per-inode
  `(st_blocks * 512, alloced_size, sum_of_file_extents)` plus
  per-row entry kind and decmpfs presence, then assert that
  candidate (1) equals the oracle for the non-compressed cases.
  Sum-of-file-extents is captured as a *diagnostic*, not as a
  product candidate, to record the apfsck / linux-apfs-rw
  disagreement for the next reviewer.
- `RL-13`: decmpfs presence on a regular file is a fail-closed
  gate for the allocated_size column in v1. The Rust crate must
  refuse to emit `Some(_)` for that case; the column status moves
  into `not_claimed`.
- `RL-11`: snapshot-retained bytes remain out of scope. The
  extent-reference tree is the dependency that work picks up;
  R2-A's emission does not depend on it.
