# SR-020 Snapshot API and Mount Lifecycle

Status: Complete (entitlement-blocked for create-path)
Date: 2026-05-16
Type: Source Review
Related RLs:
- RL-06
- RL-07
- RL-08
- RL-10
- RL-11
- RL-13

## Bottom line

R2-B's "scan a snapshot of a live volume" plan is realisable on macOS
13–14 but the API surface is split sharply across a privilege /
entitlement boundary:

1. **Read-only enumeration is free.** `fs_snapshot_list(2)`,
   `diskutil apfs listSnapshots [-plist] <volume>`, and
   `tmutil listlocalsnapshots <path>` all run as an unprivileged user
   on a volume the user can read. Names of every snapshot on the
   volume (Time Machine local snapshots in
   `com.apple.TimeMachine.YYYY-MM-DD-HHMMSS.local` form, plus any
   APFS-native named snapshot) are returned without `sudo` and
   without an entitlement.
2. **Creating a named snapshot is gated.** `fs_snapshot_create(2)`
   requires both root *and* the DTS-issued private entitlement
   `com.apple.developer.vfs.snapshot`. The same gate applies to
   `_delete`, `_rename`, `_mount`, `_revert`. There is **no
   public-SDK Objective-C wrapper.** A normal user app cannot call
   these even on a volume the user owns RW.
3. **`tmutil localsnapshot` is the only unprivileged create path.**
   It runs over XPC against the system-provided `backupd`, which
   holds the entitlement. It accepts no caller-supplied name and
   operates only on volumes included in the Time Machine backup set
   (it is silently a no-op for volumes that are not TM-included).
4. **`mount_apfs -s <snapshot-name> <mountpoint>` requires root**
   (and Full Disk Access for the calling process in TCC). The
   mountpoint is always read-only. Lifecycle: the mount survives
   even if the underlying snapshot is deleted, until the caller
   unmounts.
5. **`diskutil apfs deleteSnapshot` requires root** (auth prompt);
   `listSnapshots` does not.

Implication for R2-B: the scanner cannot create snapshots on its
own. The realistic probe is *scan an existing snapshot the system
or a privileged user has already produced*. EX-23 must therefore
operate against either (a) a pre-existing TM local snapshot
mounted by the user (root), or (b) a snapshot the user
deliberately creates for the test via `tmutil localsnapshot` or
`sudo mount_apfs -s` on a generated fixture.

The shape-parity oracle remains unchanged: live-directory walk vs
snapshot-mount walk of the unchanged data should produce
byte-identical `NamespaceEntry` + `DirectoryAggregate` rows. The
only thing the entitlement gate changes is *who runs the snapshot
create step*.

## Scope

This review answers one question:

- What is the minimum API surface a scanner needs to (a) take or
  identify, (b) mount, (c) walk, (d) unmount, and (e) release an
  APFS snapshot on macOS 13–14, and what privilege class does each
  step require?

Out of scope:

- snapshot-retained byte accounting (which blocks are held only
  because a snapshot still references them — that's a future EX-22b
  / EX-23b once R2-A converges)
- cross-snapshot diff workflows
- sealed-system content access via user snapshots
- snapshot creation from a UI-class app distributed outside
  Apple's MDM channel (would require the entitlement to ship in the
  signed bundle)

## Sources reviewed

All retrieved 2026-05-16.

- xnu syscall table:
  <https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/syscalls.master>
  (syscall 518, `AUE_NULL`, `NO_SYSCALL_STUB`).
- xnu public header:
  <https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/snapshot.h>
  (declarations for `fs_snapshot_create`, `_list`, `_delete`,
  `_rename`, `_mount`, `_revert`, `_root`, with `__OSX_AVAILABLE`
  guards).
- xnu manpage for `fs_snapshot_create.2`:
  <https://github.com/apple/darwin-xnu/blob/main/bsd/man/man2/fs_snapshot_create.2>
  ("All snapshot functions require superuser privileges and also
  require an additional entitlement").
- `man 8 mount_apfs`:
  <https://keith.github.io/xcode-man-pages/mount_apfs.8.html>
- `man 8 tmutil`:
  <https://keith.github.io/xcode-man-pages/tmutil.8.html>
- `man 8 diskutil`:
  <https://keith.github.io/xcode-man-pages/diskutil.8.html>
- Apple HT102154 *About Time Machine local snapshots*:
  <https://support.apple.com/en-us/102154>
- Apple Disk Utility guide *View APFS snapshots*:
  <https://support.apple.com/guide/disk-utility/view-apfs-snapshots-dskuf82354dc/mac>
- Apple Developer Forum thread 89635 (`fs_snapshot_create` entitlement
  discussion):
  <https://developer.apple.com/forums/thread/89635>
- Apple Developer Forum thread 786595 (creating filesystem
  snapshots from user-space):
  <https://developer.apple.com/forums/thread/786595>
- Howard Oakley *eclecticlight* APFS snapshots overview
  (2024-04-08):
  <https://eclecticlight.co/2024/04/08/apfs-snapshots/>
- Rich Trouton *derflounder* mounting TM local snapshots
  (2019-02-23):
  <https://derflounder.wordpress.com/2019/02/23/mounting-time-machine-local-snapshots-as-read-only-volumes/>

## Spec

- `fs_snapshot` is multiplexed syscall 518 (xnu
  `syscalls.master`). The libc wrappers in `bsd/sys/snapshot.h`
  select an `op` (`SNAPSHOT_OP_CREATE = 0x01` through
  `SNAPSHOT_OP_ROOT = 0x06`). Available since macOS 10.12; `_root`
  is `#ifdef PRIVATE` and macOS 10.12.4+.
- `fs_snapshot_create.2` manpage: *"All snapshot functions
  require superuser privileges and also require an additional
  entitlement."* The "additional entitlement" is identified as
  `com.apple.developer.vfs.snapshot` (create/list/delete/rename/
  mount) and `com.apple.private.apfs.revert-to-snapshot` (revert)
  in Apple Developer Forum thread 89635.
- `man 8 mount_apfs`: `-s <snapshot-name>` takes the snapshot's
  literal name (the form Time Machine uses is
  `com.apple.TimeMachine.YYYY-MM-DD-HHMMSS.local`); the
  positional argument is the *base volume's* current mount point;
  the mount itself is implicitly read-only.
- `man 8 tmutil`: `localsnapshot` "Create new local Time Machine
  snapshots of **all APFS volumes included in the Time Machine
  backup**." The created snapshots use the
  `com.apple.TimeMachine.YYYY-MM-DD-HHMMSS.local` naming.
- `man 8 diskutil`: `listSnapshots [-plist] <volume>` returns
  "all APFS Snapshots currently associated with the given APFS
  Volume" including "Snapshot UUID, Snapshot Name, numeric XID
  identifier, and possibly other fields." `deleteSnapshot
  <volume> -uuid <uuid> | -xid <xid> | -name <name> [-wait]`
  removes a snapshot in the background unless `-wait`.
- Apple HT102154: TM local snapshots are taken approximately
  every hour, retained ~24 h, and "automatically delete[d]…as
  they age or as space is needed."

## Observation

- Names alone do not distinguish APFS-native named snapshots from
  TM local snapshots on disk. They are the same on-disk object
  class (an `j_snap_metadata_t` plus a `j_snap_name_t` in the
  volume's snapshot meta tree); they differ only by the name
  string the creator chose. `fs_snapshot_list` and `diskutil apfs
  listSnapshots` return both in the same listing; `tmutil
  listlocalsnapshots` filters to the TM-prefixed subset only
  (eclecticlight 2024-04-08; verified against `diskutil apfs
  listSnapshots /` on macOS 13–14).
- The mount lifetime after delete is not documented in the
  `mount_apfs` manpage. Community reports
  (derflounder 2019-02-23) and Apple's own design notes converge
  on the kernel keeping the snapshot's blocks alive until every
  active mount-handle on it has been released, regardless of
  whether `deleteSnapshot` has been issued. Practical consequence
  for EX-23: the probe must unmount the snapshot it touched
  before declaring success; leaving a mounted snapshot pins
  blocks the volume otherwise would have freed.
- The mount path on macOS is `/Volumes/<mountpoint>` by
  convention, but `mount_apfs -s` accepts any directory the caller
  has rwx on. EX-23 should pick a `tempfile.mkdtemp()` directory
  so the test cleans up after itself.
- For an *existing* TM snapshot the mount-path equivalent that
  Apple itself uses inside its TM UI is
  `/Volumes/com.apple.TimeMachine.localsnapshots/Backups.backupdb/...`
  but those mountpoints are not auto-created; they appear only
  when something (the TM UI, `mount_apfs`) materialises them.
  EX-23 cannot count on them existing without action.
- `fs_snapshot_list` works for unprivileged callers (Apple
  Developer Forum thread 89635). It is the only public API that
  returns the *complete* snapshot inventory of a volume from
  user-space without an authorization prompt.
- `tmutil localsnapshot` invoked as a normal user succeeds when
  Time Machine is configured and the target volume is in the
  backup set; it is silently a no-op otherwise. It accepts an
  optional `<mount_point>` argument that selects which TM-included
  volume to snapshot. The caller does *not* hold the snapshot
  entitlement; it is `backupd` (running as root with the
  entitlement) that actually does the syscall over XPC. The
  side-effect appears in `tmutil listlocalsnapshots /` with the
  canonical `com.apple.TimeMachine.*.local` name.

## Hypothesis

- The R2-B shape-parity probe can be implemented as: identify any
  existing snapshot on a chosen volume (via `tmutil
  listlocalsnapshots` or `diskutil apfs listSnapshots -plist`);
  mount it read-only at a temporary directory via `sudo
  mount_apfs -s <name> <mountpoint>`; walk that mountpoint with
  the existing fallback walker; walk the live mountpoint of the
  same volume with the same walker, in the same run; diff the
  `NamespaceEntry` + `DirectoryAggregate` shape on paths that
  appear in both. Paths that exist only in the live walk
  (post-snapshot creations) and paths that exist only in the
  snapshot walk (post-snapshot deletions) are expected and not
  failures.
- The probe is **best-effort runnable without sudo** iff a
  snapshot is already mounted, which is the rare case (the user
  has been investigating TM snapshots themselves). The probe is
  **fully runnable with sudo** because `mount_apfs -s` then
  works. The probe is **not runnable** if neither a snapshot nor
  sudo is available; the right artifact in that case is a
  `blocked` summary that names the missing privilege and exits 0
  (because the lane is intentionally entitlement-gated, and a
  fail-loud exit would confuse the rest of the pipeline).
- The R2-B Rust integration, when it lands, exposes a
  `--snapshot <name-or-uuid>` flag on `apfs-fastindex-scan`
  that defers to an already-mounted snapshot path (the scanner
  itself does not call `mount_apfs`). This keeps the privilege
  decision in the user's hands and lets the binary stay an
  unprivileged tool.

## Open Limits

- **Entitlement gate.** Without the
  `com.apple.developer.vfs.snapshot` entitlement, the scanner
  cannot create or delete named APFS snapshots from inside its
  own process. The product story is therefore either "user
  brings the snapshot" or "wrapper script invokes `tmutil
  localsnapshot` and `mount_apfs -s` with appropriate
  privileges." The current branch picks the first; the second is
  a UX decision for the eventual native app shell.
- **TM-volume scoping.** `tmutil localsnapshot` is a no-op on
  volumes not included in the TM backup set. EX-23 cannot use
  `tmutil` to take a snapshot of a fresh `.dmg` fixture; it
  would have to use `sudo mount_apfs -s` against a snapshot
  created via the entitlement-gated API. The realistic
  fixture-based probe path is therefore: build a fixture image,
  mount it, take a snapshot via `sudo fs_snapshot_create` (which
  requires the entitlement and so fails outside Apple-internal
  builds) OR use `sudo diskutil apfs snapshotRestore` /
  `snapshotMount` equivalents (which we have not verified
  unprivileged). For this reason EX-23 starts with the
  existing-snapshot path; a fixture-snapshot follow-up is
  scoped separately.
- **Mount-after-delete semantics** are documented only by
  community sources, not by Apple. EX-23 should not rely on
  them: it must unmount before exiting.
- **Sealed-system snapshots** (the macOS Signed System Volume
  snapshot, e.g. the one named `com.apple.os.update-...`) are a
  separate access class and explicitly out of R2-B scope. The
  probe should detect them by name prefix and skip them rather
  than attempting to mount.

## Decision impact

- `RL-11`: snapshot-assisted scanning is API-level realisable
  but operationally split across an entitlement boundary. The
  R2-B Rust integration takes the **mount-name in, walk-out**
  shape: `--snapshot <mountpoint>` accepts an already-mounted
  snapshot directory and runs the existing fallback against it.
  The scanner does not assume snapshot-create privileges.
- `RL-08`: the read-path support matrix gains a *new column* —
  "mounted-snapshot directory" — as a fallback-mode cell that
  uses the same walker as "live mounted directory" with one
  extra invariant (the path's volume's snap-list contains a
  snapshot whose mount is at the path). The cell is gated on
  the user having mounted the snapshot themselves; nothing the
  scanner does requires root.
- `RL-13`: format-drift policy is unchanged for snapshot
  scanning; the snapshot mount is just a read-only view of an
  APFS volume that the fallback walker already supports.
- `RL-10`: the EX-23 oracle is `(live walk shape) ∩ (snapshot
  walk shape) == identity on the intersection of unchanged
  paths`. Differences outside that intersection are expected
  (creation / deletion between snapshot moment and live walk).
- The future "scanner takes its own snapshot" path is parked
  behind a separate SR / EX that pins the entitlement story or
  picks a `tmutil` shell-out wrapper as the operational
  realisation. SR-020 explicitly does **not** make a product
  claim for that path.
