//! EX-28 live-volume shape-parity validation harness.
//!
//! The actual EX-28 probe runs against a live mounted system volume's
//! `/dev/diskNsM` device node and validates that:
//!
//! 1. The raw parser's `selected_checkpoint` returns successfully (no
//!    SR-014 fail-closure under live concurrent writes).
//! 2. Three successive scans produce identical or
//!    symmetric-difference-bounded shapes.
//! 3. The raw walk agrees with the fallback walker (mounted-volume
//!    backend) on the same paths within the live-churn budget.
//!
//! This requires:
//! - root privileges (non-removable disks aren't world-readable);
//! - an explicit opt-in via `APFS_FASTINDEX_EX28_LIVE_DEVICE` env
//!   var pointing at a `/dev/diskNsM` device node;
//! - optionally `APFS_FASTINDEX_EX28_MOUNT_POINT` for the fallback
//!   comparison side.
//!
//! Without those, every test in this file is a no-op (returns
//! `Ok(())` immediately). `cargo test --release` therefore runs them
//! as harness-tracked but exercises zero code; the suite stays
//! green. The tests only do real work when the developer explicitly
//! sets the env vars, typically via:
//!
//! ```sh
//! sudo APFS_FASTINDEX_EX28_LIVE_DEVICE=/dev/disk3s1 \
//!     cargo test --release --test ex28_live_parity -- --nocapture
//! ```
//!
//! This is the privileged-subprocess shape EX-28 documents for the
//! app's "Scan as administrator…" command.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use apfs_fastindex::parity::compare_namespace_shapes;
use apfs_fastindex::{
    checkpoint_scan_source, fallback_scan_path_with_options, CheckpointScanOutput,
    FallbackOptions, ParserError, ScanError,
};

const LIVE_DEVICE_ENV: &str = "APFS_FASTINDEX_EX28_LIVE_DEVICE";
const MOUNT_POINT_ENV: &str = "APFS_FASTINDEX_EX28_MOUNT_POINT";

/// Classifies why a `checkpoint_scan_source` call against a live
/// device failed, so the test surfaces the right EX-28 verdict.
#[allow(clippy::large_enum_variant)] // test-only; not on a hot path
enum LiveScanOutcome {
    /// Scan completed; the parity assertions can run.
    Ok(CheckpointScanOutput),
    /// macOS refused to read the raw device with `EPERM` even
    /// under root — this is EX-28 Hypothesis C, "live raw
    /// unreliable on this host class". The harness records the
    /// verdict and exits cleanly (the test passes; what failed is
    /// the operating system, not the parser).
    BlockedByKernel,
    /// macOS refused to read the raw device with `EACCES` — the
    /// caller is not root. Record and exit cleanly; the user
    /// needs to re-run under sudo.
    NotPrivileged,
    /// Any other error is a real bug; propagate it as a panic.
    Other(ParserError),
}

fn classify_live_scan(device: &Path) -> LiveScanOutcome {
    match checkpoint_scan_source(device) {
        Ok(output) => LiveScanOutcome::Ok(output),
        Err(ParserError::Scan(ScanError::Io(err))) => match err.kind() {
            io::ErrorKind::PermissionDenied => {
                // macOS distinguishes EPERM (operation not
                // permitted by the kernel security policy) from
                // EACCES (file permissions). Both map to
                // PermissionDenied in Rust's io::ErrorKind; we
                // disambiguate via the raw_os_error if present.
                if err.raw_os_error() == Some(libc_eperm()) {
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

/// `libc::EPERM = 1` on every Apple platform. We hardcode the value
/// to avoid the `libc` dependency in tests (already a dependency in
/// the crate proper, but the import shape varies between editions).
fn libc_eperm() -> i32 {
    1
}

/// EX-28's accepted symmetric-difference budget for successive raw
/// scans on an idle macOS host. Empirically calibrated: a 60-second
/// scan window typically sees `~/Library/Caches/*` and
/// `/private/var/folders/*` churn ≤ a few dozen entries. The bound
/// is intentionally generous; tightening it requires a quiescent
/// host.
const SUCCESSIVE_SCAN_BUDGET: usize = 200;

/// EX-28 acceptance budget for raw-vs-fallback parity. Looser than
/// successive-scan because raw and fallback also differ on whether
/// each backend can read every path (TCC restrictions on fallback,
/// encryption boundaries on raw, etc.).
const RAW_FALLBACK_BUDGET: usize = 1000;

fn live_device() -> Option<PathBuf> {
    env::var_os(LIVE_DEVICE_ENV).map(PathBuf::from)
}

fn mount_point() -> Option<PathBuf> {
    env::var_os(MOUNT_POINT_ENV).map(PathBuf::from)
}

/// EX-28 Hypothesis A (successive-scan stability): three successive
/// raw scans of a live boot volume produce shapes whose pairwise
/// symmetric difference is ≤ `SUCCESSIVE_SCAN_BUDGET`.
///
/// Without `APFS_FASTINDEX_EX28_LIVE_DEVICE`, this test is a no-op
/// and the harness reports it as a clean pass.
#[test]
fn ex28_successive_scans_stabilize() {
    let Some(device) = live_device() else {
        eprintln!(
            "EX-28: {LIVE_DEVICE_ENV} not set — skipping live-device test. \
             Set it to a /dev/diskNsM path under root to exercise."
        );
        return;
    };

    eprintln!("EX-28: scanning {} three times…", device.display());
    let scan_one = match classify_live_scan(&device) {
        LiveScanOutcome::Ok(s) => s,
        LiveScanOutcome::BlockedByKernel => {
            eprintln!(
                "EX-28 Hypothesis C verdict: macOS kernel returned EPERM on raw read of {}. \
                 Live raw mode is blocked on this host class even under root. The harness \
                 records the verdict and exits cleanly; the parser is unchanged.",
                device.display()
            );
            return;
        }
        LiveScanOutcome::NotPrivileged => {
            eprintln!(
                "EX-28: EACCES reading {}. Re-run under sudo: \
                 sudo {LIVE_DEVICE_ENV}={} cargo test --release --test ex28_live_parity",
                device.display(),
                device.display(),
            );
            return;
        }
        LiveScanOutcome::Other(err) => panic!("unexpected first raw scan error: {err:?}"),
    };
    let scan_two = checkpoint_scan_source(&device)
        .expect("second raw scan should succeed (first one already did)");
    let scan_three = checkpoint_scan_source(&device)
        .expect("third raw scan should succeed");

    for (label, scan) in [("first", &scan_one), ("second", &scan_two), ("third", &scan_three)] {
        assert!(
            scan.selected_checkpoint.is_some(),
            "{label} scan did not publish selected_checkpoint — SR-014 fail-closure under live writes"
        );
    }

    let diff_12 = compare_namespace_shapes(
        &scan_one.parser_output.entries,
        &scan_two.parser_output.entries,
    );
    let diff_23 = compare_namespace_shapes(
        &scan_two.parser_output.entries,
        &scan_three.parser_output.entries,
    );

    eprintln!(
        "EX-28: scan_one entries = {}, scan_two = {}, scan_three = {}",
        scan_one.parser_output.entries.len(),
        scan_two.parser_output.entries.len(),
        scan_three.parser_output.entries.len(),
    );
    eprintln!(
        "EX-28: diff(1,2) symmetric_difference = {}, mismatches = {}",
        diff_12.symmetric_difference(),
        diff_12.mismatches.len(),
    );
    eprintln!(
        "EX-28: diff(2,3) symmetric_difference = {}, mismatches = {}",
        diff_23.symmetric_difference(),
        diff_23.mismatches.len(),
    );

    assert!(
        diff_12.symmetric_difference() <= SUCCESSIVE_SCAN_BUDGET,
        "scan(1) vs scan(2) symmetric difference {} exceeds EX-28 budget {}; \
         see diff details",
        diff_12.symmetric_difference(),
        SUCCESSIVE_SCAN_BUDGET,
    );
    assert!(
        diff_23.symmetric_difference() <= SUCCESSIVE_SCAN_BUDGET,
        "scan(2) vs scan(3) symmetric difference {} exceeds EX-28 budget {}",
        diff_23.symmetric_difference(),
        SUCCESSIVE_SCAN_BUDGET,
    );
}

/// EX-28 Hypothesis A': the raw scan and the fallback walker on the
/// same mounted volume agree within the live-churn budget. The
/// fallback walker is the second oracle for whether the raw fast
/// path is doing its job on a live volume.
///
/// Without `APFS_FASTINDEX_EX28_LIVE_DEVICE` + `APFS_FASTINDEX_EX28_MOUNT_POINT`,
/// this test is a no-op.
#[test]
fn ex28_raw_vs_fallback_parity() {
    let Some(device) = live_device() else {
        eprintln!(
            "EX-28: {LIVE_DEVICE_ENV} not set — skipping raw-vs-fallback test."
        );
        return;
    };
    let Some(mount) = mount_point() else {
        eprintln!(
            "EX-28: {MOUNT_POINT_ENV} not set — skipping raw-vs-fallback test \
             (need both env vars to exercise both backends on the same volume)."
        );
        return;
    };

    let raw = match classify_live_scan(&device) {
        LiveScanOutcome::Ok(s) => s,
        LiveScanOutcome::BlockedByKernel => {
            eprintln!(
                "EX-28 Hypothesis C verdict: macOS kernel returned EPERM on raw read of {}. \
                 raw-vs-fallback parity is not measurable on this host. The fallback walker \
                 itself remains the only validated backend for live volumes here.",
                device.display()
            );
            return;
        }
        LiveScanOutcome::NotPrivileged => {
            eprintln!(
                "EX-28: EACCES reading {}. Re-run under sudo.",
                device.display()
            );
            return;
        }
        LiveScanOutcome::Other(err) => panic!("unexpected raw scan error: {err:?}"),
    };
    let fallback = fallback_scan_path_with_options(&mount, FallbackOptions::default())
        .expect("fallback scan should succeed");

    let diff = compare_namespace_shapes(
        &raw.parser_output.entries,
        &fallback.parser_output.entries,
    );

    eprintln!(
        "EX-28: raw entries = {}, fallback entries = {}",
        raw.parser_output.entries.len(),
        fallback.parser_output.entries.len(),
    );
    eprintln!(
        "EX-28: raw-vs-fallback symmetric_difference = {}, mismatches = {}",
        diff.symmetric_difference(),
        diff.mismatches.len(),
    );

    assert!(
        diff.symmetric_difference() <= RAW_FALLBACK_BUDGET,
        "raw-vs-fallback symmetric difference {} exceeds EX-28 budget {}",
        diff.symmetric_difference(),
        RAW_FALLBACK_BUDGET,
    );
}
