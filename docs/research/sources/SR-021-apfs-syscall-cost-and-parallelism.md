# SR-021 APFS Syscall Cost Map and Parallelism Ceiling

Status: Complete
Date: 2026-05-16
Type: Source Review
Related RLs:
- RL-06
- RL-08
- RL-12
- RL-13

## Bottom line

The post-r2c-fallback-perf scanner is at ~200k entries/sec on
`/Applications` (~0.816 s wall on 164k entries, ~63% sys-CPU). The
remaining time is structurally **kernel-side, not user-side**: each
entry that asks for inode-required attributes (`size`, `mode`,
`mtime`, `flags`) causes APFS to create a vnode that the VFS layer
*immediately rage-ages* via `UT_KERN_RAGE_VNODES`
(`vfs_attrlist.c:4436` + `vfs_subr.c:7233-7256`). The 64 KiB
`getattrlistbulk` buffer and the syscall framing are **not** the
load-bearing cost.

Two distinct levers are open:

1. **Parallelism at the directory level** (one worker per CPU,
   workers pull dirs off a shared queue, each worker holds its own
   `BulkReader`). Convergent across three independent fast macOS
   scanners (`dumac`, `macdirstat`, `jwalk`). Per-open-file `FV_LOCK`
   means two threads on two different dirfds don't contend at the
   VFS layer. Expected envelope on Apple silicon, single APFS
   container: **1.6-1.8× @ T=2, 2.5-3.2× @ T=4, 3.5-4.5× @ T=8**,
   then plateaus or regresses. Catastrophic at T > physical cores
   (Szorc 2018 measured 18 procs as *worse* than 12, and ripgrep
   `fd#1131` saw a 1212× *slowdown* on WSL2 under parallel mode).
2. **Attribute mask shrink** — drec-only attributes (name,
   `file_id`, `obj_type`, `parent_id`) are answered straight from
   the FS-tree drec leaf and skip vnode creation entirely. A
   two-phase scan that defers `size`/`mode` to a second pass on
   files of interest would cut Phase-1 sys-CPU dramatically. This
   is a major refactor (changes the product contract because the
   v1 column set requires `logical_size`), so it is **scoped out
   of this SR** and recorded as a future direction in §"Open
   Limits".

Two avenues are ruled out:

3. **Spotlight / `mdfind`** as a metadata oracle: dispositively
   unsuitable — by-design incomplete (`/System`, dotfiles,
   `~/Library` exclusions), no public scoped-subtree streaming
   API for third-party apps, no row-formatted `(path, size, kind)`
   output (would need a second `mdls` pass that erases the
   index speedup), and undocumented store format. May still be
   useful as opportunistic *post-walk* UTI enrichment; not on
   the scanner hot path.
4. **`getattrlistbulk` buffer size**: at ~2k syscalls/sec the
   per-syscall floor of ~100 ns is <0.1% of the budget. Larger
   buffers (256 KiB, 1 MiB) do not move the needle.

A third avenue is recorded as **worth A/B testing**:

5. **`fts(3)`** instead of `getattrlistbulk` — Tempel (2019)
   measured `fts` ≥ `getattrlistbulk` on single-threaded APFS
   metadata walks because the per-`fstatat` vnode is reused via
   the namecache instead of being raged. If our parallel walker
   plateaus, an EX-26 fts probe is the next experiment.

## Scope

This review answers one question:

- Given the post-r2c-fallback-perf walker, where is the remaining
  ~500 ms / scan going inside the kernel, and which optimisation
  avenues are structurally viable on macOS 14+ for a single-APFS-
  container live-volume scan?

Out of scope:

- two-phase scan with deferred attribute fetch (changes the
  product contract; future work behind a Phase-2 plan)
- raw-mode parsing of a live boot disk (R1 explicitly excluded
  this; remains gated on a Gate-2 oracle)
- snapshot-assisted scanning (R2-B; entitlement-blocked per SR-020)
- compression-aware accounting (R2-A follow-up territory)
- iCloud `FileProvider` materialisation control
  (`vfs.nspace.prevent_materialization` is interesting but lives
  in a separate spec and only matters for `du`-class tools)

## Sources reviewed

All retrieved 2026-05-16.

### xnu kernel source (`xnu-12377.101.15`)

- `bsd/vfs/vfs_attrlist.c:4253-4484` (`getattrlistbulk()` syscall
  entry) — <https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.101.15/bsd/vfs/vfs_attrlist.c>
- `bsd/vfs/vfs_attrlist.c:4436` (`UT_KERN_RAGE_VNODES` set before
  dispatch)
- `bsd/vfs/vfs_attrlist.c:2244-2350` (`attr_pack_file` with
  `is_bulk=1`)
- `bsd/vfs/kpi_vfs.c:5587-5610` (`VNOP_GETATTRLISTBULK` dispatch)
- `bsd/vfs/vfs_subr.c:7233-7256` (`vnode_create_internal` rage path)
- `bsd/sys/user.h:401` (`UT_KERN_RAGE_VNODES` definition)

### Open-source APFS readers (for the per-entry cost model)

- `sgan81/apfs-fuse`, `ApfsLib/ApfsDir.cpp::ListDirectory` and
  `GetInode` — <https://github.com/sgan81/apfs-fuse>
- `linux-apfs/linux-apfs-rw`, `dir.c::apfs_readdir` —
  <https://github.com/linux-apfs/linux-apfs-rw>
- `libyal/libfsapfs` —
  <https://github.com/libyal/libfsapfs>

### Convergent fast-macOS-scanner practice

- Andrew Healey, `dumac` (Rust + Rayon + `getattrlistbulk`) —
  <https://healeycodes.com/maybe-the-fastest-disk-usage-program-on-macos>,
  <https://healeycodes.com/optimizing-my-disk-usage-program>,
  <https://github.com/healeycodes/dumac>
- Michael Stromberg, `macdirstat` (Rust + Rayon +
  `getattrlistbulk` + `openat`) —
  <https://github.com/MichaelStromberg/macdirstat>
- `jwalk` (per-directory rayon tasks, `process_read_dir` API) —
  <https://docs.rs/jwalk/>, <https://github.com/Byron/jwalk>
- BurntSushi `ignore::WalkParallel` (used by `rg`/`fd`;
  `crossbeam_deque` work-stealing) —
  <https://docs.rs/ignore/>, ripgrep#2854 (default-2-threads bug)

### APFS concurrency evidence

- Gregory Szorc, *Global Kernel Locks in APFS* (2018-10-29) —
  <https://gregoryszorc.com/blog/2018/10/29/global-kernel-locks-in-apfs/>
- rdar://45648013 (Szorc filed; 10.14 partially fixed) —
  <https://openradar.appspot.com/45648013>
- Apple DTS engineer Kevin Elliott, Apple Developer Forums
  #800906 (2025) —
  <https://developer.apple.com/forums/thread/800906>
- Eugene Petrenko, *Listing Files on macOS* (2020-08-12) —
  <https://jonnyzzz.com/blog/2020/08/12/listing-files/>
- Thomas Tempelmann, *Performance Considerations Reading
  Directories on macOS* (2019-04) —
  <http://blog.tempel.org/2019/04/dir-read-performance.html>
- sharkdp/fd#1131 (1212× slowdown on WSL2 under parallel) —
  <https://github.com/sharkdp/fd/issues/1131>

### Spotlight (for the ruled-out branch)

- libyal Spotlight store format docs —
  <https://github.com/libyal/dtformats>
- Howard Oakley's *eclecticlight* APFS / Spotlight series
  (multiple posts, retrieval-dated above)
- Apple Developer Forums #121187 (no public API for system Core
  Spotlight)

### Syscall floor

- Cloudflare ebpf_exporter benchmark on M1
- Arkanis 2017 syscall performance measurements

## Spec

- **xnu** (`vfs_attrlist.c:4253-4484`): `getattrlistbulk(2)` first
  resolves the dirfd via `fp_getfvp`, takes `vnode_getwithref(dvp)`,
  MAC-checks, runs `vnode_authorize(dvp, …, LIST_DIRECTORY |
  SEARCH, …)`, then takes `FV_LOCK(fvdata)` — the lock is
  **per-open-file**, on `struct fd_vn_data`. Two threads operating
  on two different dirfds do not contend at this layer even if the
  dirfds are inside the same APFS container.
- **xnu** (`kpi_vfs.c:5587-5610`): `VNOP_GETATTRLISTBULK` is a thin
  vnode-op trampoline into the per-FS vector — directly into APFS
  with no FSEvents fan-out, no namecache invalidation, no
  intermediate VFS-layer work.
- **xnu** (`vfs_attrlist.c:4436` + `vfs_subr.c:7233-7256`): before
  dispatching to APFS, the kernel sets
  `ut->uu_flag |= UT_KERN_RAGE_VNODES;`. When APFS subsequently
  calls `vnode_create_internal`, the new vnode is OR'd with
  `VRAGE` and placed at the front of the recycle list. The vnode
  is created, used to fill `vnode_attr`, and immediately reclaimed
  — bulk callers do not pollute the long-term namecache, but they
  *do* pay the create/destroy cost per entry.
- **xnu** (`vfs_attrlist.c:2244-2350`): per-entry attribute pack
  is a branch + 4/8-byte memcpy + bitmask-or. Adding
  `ATTR_FILE_ALLOCSIZE` next to `ATTR_FILE_TOTALSIZE` is one extra
  `ATTR_PACK8` per entry — sub-1% overhead — **as long as the
  attribute is already filled in `vnode_attr` by APFS** and does
  not require a separate xattr / btree fetch (resource-fork sizes,
  `ATTR_CMNEXT_LINKID`, and anything in `forkattr` cross this
  boundary).
- **xnu** (`vfs_attrlist.c:4454-4465`): if APFS returned `ENOTSUP`
  the kernel falls back to `readdir + per-entry getattrlist`
  internally. APFS does not return ENOTSUP for our attribute set;
  this code path is not exercised.
- **Apple File System Reference** (p. 102-106): a directory listing
  is a single B-tree range scan over `j_drec_hashed_key_t` records;
  name, `file_id`, and POSIX `obj_type` are emitted directly from
  the drec leaf with no per-entry lookup. Any other attribute
  requires a second point-lookup keyed on `(oid=ino, type=INODE)`.

## Observation

### Per-entry cost decomposition (cross-reader synthesis)

Cited from `apfs-fuse::ApfsDir::ListDirectory` and `linux-apfs-rw::
apfs_readdir`, both of which mirror Apple's on-disk shape:

- **Drec-only attributes** (name, `file_id`, `obj_type`,
  `parent_id`): one B-tree range scan per directory; emit each leaf
  directly. **No vnode creation.**
- **Inode-required attributes** (size, mode, mtime, uid/gid,
  bsd_flags, link count, generation): one extra B-tree point
  lookup per entry. The kernel allocates a vnode, fills
  `vnode_attr`, and immediately rage-ages it
  (`vfs_attrlist.c:4436`, `vfs_subr.c:7233-7256`).

Our current attribute mask
(`ATTR_CMN_NAME | ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE |
ATTR_CMN_FILEID | ATTR_CMN_ERROR | ATTR_FILE_TOTALSIZE |
ATTR_FILE_ALLOCSIZE`) crosses the boundary at `ATTR_FILE_TOTALSIZE`:
that field needs `va_total_size` filled, which requires APFS to
load the inode → vnode create + rage path fires.

This is the credible source of the 63% sys-CPU we observe. The
load-bearing implication is that **shrinking the attribute mask
to drec-only would substantially cut sys-CPU**, at the cost of
losing `logical_size` (and therefore the product's namespace+size
v1 contract). A two-phase scan that splits enumeration from
attribute fetch would let Phase 1 stay drec-only; that refactor
is out of SR-021 scope but is the highest-impact future direction
if the parallelism lever plateaus before we are satisfied.

### Convergent practice: parallel `getattrlistbulk`

Three independent fast macOS scanners use the same architecture:

- **`dumac`** (Healey, 2023): Rayon worker pool, one task per
  directory, each task holds its own `getattrlistbulk` 64 KiB
  buffer. Reports **563 ms vs `du -sh` 3.186 s on M1 Pro**
  (5.7×). The Tokio → Rayon migration alone (same syscall
  strategy, different async runtime) cut context switches
  1.2M → 235k (-80%), halved syscalls observed in `dtrace`, and
  improved wall time 901 ms → 731 ms.
- **`macdirstat`** (Stromberg): same pattern, with `openat` for
  relative resolution.
- **`jwalk`** (Grosjean / Byron): the canonical Rust API for
  per-directory parallel walks. Uses `std::fs::read_dir` rather
  than `getattrlistbulk` (a fork would be needed for our use),
  but the parallelism architecture transfers directly. Author
  benchmark: ~4× over `walkdir` on metadata workloads.

### Concurrency ceiling: the headline disagreement and its resolution

Two pieces of evidence at first appear contradictory:

- **Szorc (2018, macOS 10.13)**: parallel readdir on 12 procs
  consumed **23 m 42 s sys vs 3 m 50 s @ 1 proc** — sys-time grew
  6× super-linearly. 18 procs got *worse* (210 s wall vs 172 s @
  12). Concluded an apparent global mutex in `apfs_vnop_readdir`
  / `lck_mtx_lock_grab_mutex`.
- **Apple DTS engineer (Kevin Elliott, 2025)**: the original
  analysis had "significant flaws" and was "largely addressed in
  macOS 10.14 (Mojave)."

The reconciling reading consistent with both data points and with
modern (2023) measurements:

1. Pre-10.14 had a genuine APFS-wide bottleneck (Szorc reproduced
   it).
2. 10.14 reduced its scope. The surviving contention is at the
   **B-tree / object-cache layer per APFS *container***, not
   strictly global and not strictly per-volume.
3. On macOS 14+ (Sonoma/Sequoia/Tahoe), no reproducible
   global-lock report exists, and Healey's 2023 measurements show
   real super-1× scaling from a parallel `getattrlistbulk` walker
   on a single container.

The practical envelope on Apple silicon, single APFS container,
cold cache (synthesised from §1's xnu evidence, Szorc's curve
shape post-10.14, and Healey's modern numbers):

- T = 2: ~1.6-1.8× of T=1 throughput
- T = 4: ~2.5-3.2×
- T = 8: ~3.5-4.5× (plateaus as B-tree page-cache contention
  rises)
- T > physical cores: marginal-to-negative

The Tempelmann (2019) and Petrenko (2020) results that "parallel
did not help" are explained by either (a) pre-10.14 contention,
(b) WSL2 / encrypted-APFS edge cases, or (c) a per-file rather
than per-directory parallelism granularity that pays the
task-spawn overhead without amortising the syscall framing.

### Spotlight: structurally unsuitable

Four independent disqualifiers, any one of which is dispositive:

- `mdfind(1)` returns paths only; getting `kMDItemFSSize` per row
  requires `xargs mdls -name kMDItemFSSize`, which spawns one
  `mdls` per file/batch and erases the index speedup.
- Coverage is by-design incomplete: `/System`, dotfile trees,
  `~/Library` (except `Application Support`), `.noindex`,
  `.metadata_never_index`, and any Privacy-listed volume are
  invisible (Howard Oakley, 2024 + 2025; Apple HT102154).
- `MDQuery` has no public way to restrict scope to an arbitrary
  subtree; third-party apps cannot query the system Core
  Spotlight index without Apple-internal entitlements (Apple
  DevForums #121187, #800906).
- Writes are visible in the index with a 1-2 s lag (Apple
  vendor-aligned) to 5-30 s under load (user reports); not
  acceptable for a fresh-state scanner contract.

The store format (libyal's reverse-engineered docs at `dtformats`)
is technically parseable but undocumented, requires root + SIP
carve-outs, and revs across macOS versions — fragile foundation
for production.

A salvageable use exists: **post-walk UTI enrichment**. After we
have the path list, a batched `mdls -name kMDItemContentTypeTree`
per ~10k paths might be substantially faster than per-file
`LSCopyKindStringForURL`. This is an *optional* future
enhancement, not part of the scanner hot path.

### What's not a lever

- **`getattrlistbulk` buffer size**: at ~2k syscalls/sec the
  per-syscall floor of ~100 ns (Cloudflare M1 benchmark + Arkanis
  ARM measurements) is <0.1% of the budget. Going from 64 KiB to
  256 KiB or 1 MiB does not move the needle.
- **`getdirentriesattr(2)`**: deprecated since macOS 10.10
  (`opensource.apple.com` man page). Routes through the same VFS
  path on APFS; no advantage.
- **Pure `rayon::par_iter` over a pre-collected directory list**:
  pays the collect cost + the same APFS contention. The
  per-directory work-queue pattern is strictly better.
- **Per-file tasks**: 5 M tasks at ~1 µs spawn overhead = 5 s of
  pure rayon overhead on a `/`-scan, dwarfing the directory case.

### Worth A/B testing if parallelism plateaus: `fts(3)`

Tempel (2019) measured `fts` ≥ `getattrlistbulk` on single-threaded
APFS metadata walks. The xnu agent's hypothesis explains why: per-
`fstatat` vnodes are reused via the namecache when traversing
depth-first, instead of being raged like the bulk-path vnodes.
This *could* mean an `fts`-based walker has a lower per-entry
floor than ours; whether it parallelises as well is unknown (no
public data). Recorded as EX-26 if EX-25 plateaus before our
target gain.

## Hypothesis

- A worker-per-CPU parallel walker (per-directory tasks, shared
  work queue, per-worker `BulkReader`) will reach **2.5-3.5× of
  the current single-threaded throughput on Apple silicon at
  T=4**, sub-linearly with degraded returns above T=8.
- The sys-CPU per thread will grow super-linearly *some* with T
  (residual btree contention) but not catastrophically; this is
  the EX-25 falsification target.
- The right default is `--threads min(num_physical_cores, 4)`
  with a `--threads N` flag for explicit user control. Default 1
  until EX-25 ships positive.
- Sort order: per-directory local sort inside each worker, then
  DFS-ordered emission to a sink. Equivalent in correctness to
  the current collect-then-sort but avoids re-sorting 5M entries.
- The 64 KiB bulk buffer stays at 64 KiB.

## Open Limits

- **Phase-2 attribute-deferred scan**: the biggest remaining
  win (per the xnu evidence) would be a two-phase scan where
  Phase 1 enumerates names with a drec-only attribute mask (no
  vnode creation), and Phase 2 fetches `size`/`mode` only for
  paths of interest. This breaks the current "single-pass scan
  returns the full namespace + size" contract; would need a
  separate SR + design discussion. Out of this lane.
- **Encrypted-APFS / `FileVault` runtime**: Petrenko (2020)
  observed zero parallelism benefit on encrypted APFS on a 2019
  i9. Modern hardware-backed encryption may not have the same
  cost; we cannot validate without running EX-25 on an encrypted
  volume. Recorded; the v1 support matrix already excludes
  encrypted runtime.
- **Apple silicon vs Intel scaling**: all the optimistic envelope
  numbers above are calibrated to Apple silicon. Intel Macs with
  pre-NVMe storage will likely show lower ceilings; our default
  thread cap of 4 is intentionally conservative for that reason.
- **APFS container-level contention**: SR-021 cannot resolve the
  exact location of the surviving B-tree contention from public
  evidence alone (Apple DTS' position contradicts Szorc's
  reproducer but the modern Healey numbers are consistent with
  *some* residual cost). EX-25's instrumentation (sys-time
  per-thread + `sample` mid-run for kernel symbol attribution)
  is the right way to settle this empirically on our own host.

## Decision impact

- `RL-08`: the read-path support matrix gains a new diagnostic
  column ("per-thread sys-CPU growth ratio") as the empirical
  signal for the APFS-container contention ceiling. A row that
  shows >1.5× sys-CPU per added thread should back off to T=1.
- `RL-12`: parallelism becomes the named next performance lane.
  The drec-only attribute mask refactor is recorded as the
  highest-impact deferred follow-up.
- `RL-13`: Spotlight is added to the explicit "ruled out as a
  metadata oracle" list with the four disqualifiers cited.
- The Rust slice that comes out of EX-25 ships behind a
  `--threads N` flag, default behaviour to be set by the EX-25
  verdict. The walker contract (sorted output, mount-boundary
  skipping, per-entry `WalkSkip` recording) is preserved by
  construction; only the traversal scheduler changes.
- EX-24 (microbench: pure syscall framing, drec-only attr mask,
  current attr mask, `fts(3)`) is the prerequisite — it
  establishes the empirical floor per-thread before we add
  threads. EX-25 (parallel walker at T ∈ {1,2,4,8,num_cpus}) is
  the actual perf experiment. EX-26 (`fts(3)`) is conditional on
  EX-25 plateauing.
