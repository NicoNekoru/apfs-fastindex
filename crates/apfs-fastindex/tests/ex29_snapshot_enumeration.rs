//! EX-29 snapshot-enumeration harness.
//!
//! Two-test design that mirrors EX-28's pattern:
//!
//! 1. `ex29_enumerate_host_snapshots`: runs unconditionally. Calls
//!    `apfs_fastindex::snapshots::enumerate_mount(/)` and classifies
//!    the host's snapshot state. Asserts the classifier returned a
//!    well-formed verdict (any of the three is acceptable — what
//!    matters is that the enumeration didn't panic and the parser
//!    didn't disagree with `classify`'s output).
//!
//! 2. `ex29_mount_apfs_extent_diff`: gated on
//!    `APFS_FASTINDEX_EX29_SNAPSHOT_DEVICE` (the `/dev/diskNsM` of
//!    a mounted snapshot from `mount_apfs -s`). Runs the raw parser
//!    against the snapshot device and the live volume device named
//!    by `APFS_FASTINDEX_EX29_LIVE_DEVICE`, then diffs their extent
//!    sets via the existing `parity` comparator. Without those env
//!    vars, the test is a clean no-op. Designed for a future host
//!    where the raw fast path on snapshot devices is unblocked
//!    (EX-28 Hypothesis C is the current verdict on Apple silicon
//!    under SIP, so this test no-ops in practice; it's wired for
//!    when the world changes).

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use apfs_fastindex::parity::compare_namespace_shapes;
use apfs_fastindex::snapshots::{classify, enumerate_mount, SnapshotVerdict};
use apfs_fastindex::{checkpoint_scan_source, CheckpointScanOutput, ParserError, ScanError};

const SNAPSHOT_DEVICE_ENV: &str = "APFS_FASTINDEX_EX29_SNAPSHOT_DEVICE";
const LIVE_DEVICE_ENV: &str = "APFS_FASTINDEX_EX29_LIVE_DEVICE";
const RAW_PARITY_BUDGET: usize = 2000;

enum LiveScanOutcome {
    Ok(CheckpointScanOutput),
    BlockedByKernel,
    NotPrivileged,
    Other(ParserError),
}

fn classify_live_scan(device: &Path) -> LiveScanOutcome {
    match checkpoint_scan_source(device) {
        Ok(output) => LiveScanOutcome::Ok(output),
        Err(ParserError::Scan(ScanError::Io(err))) => match err.kind() {
            io::ErrorKind::PermissionDenied => {
                if err.raw_os_error() == Some(1) {
                    LiveScanOutcome::BlockedByKernel
                } else {
                    LiveScanOutcome::NotPrivileged
                }
            }
            _ => LiveScanOutcome::Other(ParserError::Scan(ScanError::Io(err))),
        },
        Err(other) => LiveScanOutcome::Other(other),
    }
}

/// EX-29 unprivileged enumeration. Always runs; just verifies the
/// classifier produced a well-formed verdict on the host. The
/// specific verdict varies by host: `Enumerated` on a host with TM
/// local snapshots, `NoUserSnapshots` on a host with only sealed-
/// system or no snapshots.
#[test]
fn ex29_enumerate_host_snapshots() {
    let root_mount = PathBuf::from("/");
    let data_mount = PathBuf::from("/System/Volumes/Data");
    let enums = [enumerate_mount(&root_mount), enumerate_mount(&data_mount)];

    // The classifier never errors; it returns one of three variants.
    let verdict = classify(&enums);
    match &verdict {
        SnapshotVerdict::Enumerated {
            user_visible_count,
            sealed_system_excluded_count,
        } => {
            eprintln!(
                "EX-29: enumerated {user_visible_count} user-visible TM local \
                 snapshot(s); {sealed_system_excluded_count} sealed-system snapshot(s) \
                 excluded per SR-020."
            );
        }
        SnapshotVerdict::NoUserSnapshots {
            sealed_system_excluded_count,
        } => {
            eprintln!(
                "EX-29: no user-visible TM local snapshots on this host; \
                 {sealed_system_excluded_count} sealed-system snapshot(s) \
                 excluded per SR-020."
            );
        }
        SnapshotVerdict::ToolingUnavailable { reason } => {
            eprintln!("EX-29: tmutil/diskutil unavailable: {reason}");
        }
    }

    // Cross-check: every entry that the parser marked
    // `user_visible = true` must NOT match the sealed-system
    // prefix.
    for e in &enums {
        for entry in e.tmutil_entries.iter().chain(e.diskutil_entries.iter()) {
            assert_eq!(
                entry.user_visible,
                !entry.name.starts_with("com.apple.os.update-"),
                "classifier disagreed with the SR-020 sealed-system prefix \
                 rule on entry {:?}",
                entry,
            );
        }
    }
}

/// EX-29 raw-extent diff between a mounted snapshot's device node
/// and the live volume's device node. Gated on
/// `APFS_FASTINDEX_EX29_SNAPSHOT_DEVICE` and
/// `APFS_FASTINDEX_EX29_LIVE_DEVICE`. Without those, the test is
/// a clean no-op.
///
/// On a host where EX-28's Hypothesis C holds (Apple silicon under
/// SIP), this test will hit `BlockedByKernel` on the first scan
/// and record the same verdict EX-28 closed with — the harness
/// stays wired for when the world changes (e.g. an external
/// non-sealed APFS disk that does permit raw reads).
#[test]
fn ex29_mount_apfs_extent_diff() {
    let Some(snap_device) = env::var_os(SNAPSHOT_DEVICE_ENV).map(PathBuf::from) else {
        eprintln!(
            "EX-29: {SNAPSHOT_DEVICE_ENV} not set — skipping snapshot extent diff. \
             Mount a snapshot read-only via `mount_apfs -s <snap-name> /dev/diskNsM \
             /Volumes/apfs-ex29-snap`, then set this env var to its device node."
        );
        return;
    };
    let Some(live_device) = env::var_os(LIVE_DEVICE_ENV).map(PathBuf::from) else {
        eprintln!(
            "EX-29: {LIVE_DEVICE_ENV} not set — skipping snapshot extent diff."
        );
        return;
    };

    let snap_scan = match classify_live_scan(&snap_device) {
        LiveScanOutcome::Ok(s) => s,
        LiveScanOutcome::BlockedByKernel => {
            eprintln!(
                "EX-29: EX-28 Hypothesis C reproduces on the snapshot device \
                 {}; macOS returned EPERM on raw read. Snapshot extent diff is \
                 not measurable on this host class; the classifier stays the \
                 only available oracle.",
                snap_device.display()
            );
            return;
        }
        LiveScanOutcome::NotPrivileged => {
            eprintln!("EX-29: EACCES on {}. Re-run under sudo.", snap_device.display());
            return;
        }
        LiveScanOutcome::Other(err) => panic!("unexpected snapshot raw scan error: {err:?}"),
    };
    let live_scan = match classify_live_scan(&live_device) {
        LiveScanOutcome::Ok(s) => s,
        LiveScanOutcome::BlockedByKernel => {
            eprintln!(
                "EX-29: live device {} also EPERM-blocked. Snapshot extent diff \
                 needs both sides readable.",
                live_device.display()
            );
            return;
        }
        LiveScanOutcome::NotPrivileged => {
            eprintln!("EX-29: EACCES on {}. Re-run under sudo.", live_device.display());
            return;
        }
        LiveScanOutcome::Other(err) => panic!("unexpected live raw scan error: {err:?}"),
    };

    let diff = compare_namespace_shapes(
        &snap_scan.parser_output.entries,
        &live_scan.parser_output.entries,
    );
    eprintln!(
        "EX-29: snapshot vs live shape diff: symmetric_difference = {}, \
         mismatches = {} (snapshot entries = {}, live entries = {}).",
        diff.symmetric_difference(),
        diff.mismatches.len(),
        snap_scan.parser_output.entries.len(),
        live_scan.parser_output.entries.len(),
    );
    assert!(
        diff.symmetric_difference() <= RAW_PARITY_BUDGET,
        "snapshot vs live symmetric difference {} exceeds EX-29 budget {}",
        diff.symmetric_difference(),
        RAW_PARITY_BUDGET,
    );
}
