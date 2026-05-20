import Foundation
import CApfsFastindex

/// Detects whether a filesystem path lives on an APFS snapshot.
///
/// The Swift Darwin overlay collides on `statfs` (both struct
/// and function name), making a direct call awkward. We
/// instead delegate to the Rust crate's `apfs_is_snapshot_path`
/// FFI, which wraps `libc::statfs(2)` + the MNT_SNAPSHOT flag
/// check inside the panic-guard. Same result, cleaner Swift.
///
/// EX-29 follow-up: this is the "implicit opt-in" path — when
/// the user types a snapshot path directly we surface a
/// "Snapshot" status chip in the UI without requiring the
/// Settings "Expand local snapshots" toggle. They asked for
/// the data by virtue of typing the path; we just label it
/// accordingly.
enum SnapshotDetect {
    /// Returns `true` iff `path` resolves to a filesystem
    /// mounted as a snapshot. Returns `false` on any error
    /// (`statfs` failed, path doesn't exist, etc.) — the UI
    /// treats a non-snapshot as the safe default and the user
    /// can always opt in via Settings.
    static func isOnSnapshot(_ path: String) -> Bool {
        path.withCString { cPath in
            apfs_is_snapshot_path(cPath) != 0
        }
    }
}
