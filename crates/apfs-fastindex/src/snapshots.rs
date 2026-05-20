//! Local-snapshot enumeration (EX-29).
//!
//! macOS exposes two unprivileged read-only surfaces for snapshot
//! listing:
//!
//! - `tmutil listlocalsnapshots <mount>` — Time Machine local
//!   snapshots created by Apple's Spotlight/TimeMachine framework.
//!   This is what users mean by "local snapshots" in Disk Utility's
//!   space-usage UI.
//! - `diskutil apfs listSnapshots <mount>` — every APFS snapshot on
//!   the disk's APFS volume, including the sealed-system OS-update
//!   boot snapshot.
//!
//! Neither surface reports per-snapshot reclaimable bytes; the only
//! macOS tool that does (`tmutil thinlocalsnapshots`) is destructive
//! (it deletes the snapshot to report what was reclaimed). EX-29's
//! Rust contribution is the enumeration; bytes are explicitly
//! unclaimed.
//!
//! SR-020 sealed-system filter: snapshots whose name matches
//! `com.apple.os.update-*` are the boot snapshot pinning the read-
//! only system volume's state. They cannot be deleted by users and
//! their bytes are not user-reclaimable; they are excluded from the
//! user-visible count.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

/// Sealed-system OS-update snapshot name prefix. See SR-020.
const SEALED_SYSTEM_PREFIX: &str = "com.apple.os.update-";

/// One snapshot from either `tmutil` or `diskutil`. `user_visible`
/// is `false` for sealed-system OS-update snapshots (SR-020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotEntry {
    pub name: String,
    pub user_visible: bool,
    /// Optional UUID, present when sourced from
    /// `diskutil apfs listSnapshots`; `None` for tmutil entries.
    pub uuid: Option<String>,
    /// Optional XID, present in `diskutil` output.
    pub xid: Option<u64>,
}

impl SnapshotEntry {
    /// EX-29 user-visibility filter (SR-020): excludes
    /// `com.apple.os.update-*` snapshots.
    pub fn classify_user_visibility(name: &str) -> bool {
        !name.starts_with(SEALED_SYSTEM_PREFIX)
    }
}

/// `tmutil listlocalsnapshots <mount>` output, parsed.
///
/// macOS shape:
///
/// ```text
/// Snapshots for disk <mount>:
/// com.apple.TimeMachine.2026-05-20-100000.local
/// com.apple.TimeMachine.2026-05-20-110000.local
/// ```
///
/// When no snapshots exist, only the header line appears. Returns
/// the snapshot list in document order. Public so callers (the FFI
/// surface, the CLI summary mode, the EX-29 integration harness)
/// can run their own subprocess and parse it.
pub fn parse_tmutil_output(stdout: &str) -> Vec<SnapshotEntry> {
    let mut out: Vec<SnapshotEntry> = Vec::new();
    for line in stdout.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with("Snapshots for disk") {
            continue;
        }
        if stripped.starts_with("NOTE:") {
            continue;
        }
        out.push(SnapshotEntry {
            user_visible: SnapshotEntry::classify_user_visibility(stripped),
            name: stripped.to_string(),
            uuid: None,
            xid: None,
        });
    }
    out
}

/// `diskutil apfs listSnapshots <mount>` output, parsed.
///
/// macOS shape:
///
/// ```text
/// Snapshot for disk3s1s1 (1 found)
/// |
/// +-- 3E1AC922-F4EC-433E-B4D0-0052ADD81E03
///     Name:        com.apple.os.update-...
///     XID:         2293965
///     Purgeable:   No
///     NOTE:        This snapshot limits the minimum size of APFS Container disk3
/// ```
///
/// Each `+--` line introduces one snapshot. Returns the list in
/// document order.
pub fn parse_diskutil_output(stdout: &str) -> Vec<SnapshotEntry> {
    let mut out: Vec<SnapshotEntry> = Vec::new();
    let mut current_uuid: Option<String> = None;
    let mut current_name: Option<String> = None;
    let mut current_xid: Option<u64> = None;

    let flush = |out: &mut Vec<SnapshotEntry>,
                 uuid: Option<String>,
                 name: Option<String>,
                 xid: Option<u64>| {
        if let Some(n) = name {
            out.push(SnapshotEntry {
                user_visible: SnapshotEntry::classify_user_visibility(&n),
                name: n,
                uuid,
                xid,
            });
        }
    };

    for line in stdout.lines() {
        let stripped = line.trim();
        if let Some(rest) = stripped.strip_prefix("+--") {
            // Starting a new snapshot block — flush the previous.
            flush(
                &mut out,
                current_uuid.take(),
                current_name.take(),
                current_xid.take(),
            );
            current_uuid = Some(rest.trim().to_string());
            continue;
        }
        if let Some(value) = stripped.strip_prefix("Name:") {
            current_name = Some(value.trim().to_string());
        } else if let Some(value) = stripped.strip_prefix("XID:") {
            current_xid = value.trim().parse::<u64>().ok();
        }
    }
    flush(&mut out, current_uuid, current_name, current_xid);
    out
}

/// EX-29 verdict for a host's snapshot state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotVerdict {
    /// At least one user-visible TM local snapshot. The count +
    /// names are surfaceable in the UI; bytes stay unclaimed.
    Enumerated {
        user_visible_count: u32,
        sealed_system_excluded_count: u32,
    },
    /// No user-visible snapshots (only sealed-system or none).
    /// Same shape EX-23 found on this host class.
    NoUserSnapshots {
        sealed_system_excluded_count: u32,
    },
    /// `tmutil` / `diskutil` exited non-zero or are not present.
    /// Probably an unsupported host (non-macOS, sandboxed
    /// subprocess, etc.).
    ToolingUnavailable {
        reason: String,
    },
}

/// One mount-point's enumeration: raw tmutil + diskutil parses,
/// pre-classified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotEnumeration {
    pub mount_point: String,
    pub tmutil_entries: Vec<SnapshotEntry>,
    pub diskutil_entries: Vec<SnapshotEntry>,
}

/// Run `tmutil listlocalsnapshots <mount>` and parse. Returns
/// `Err(reason)` if the binary is not present or exits non-zero.
pub fn list_tmutil_snapshots(mount: &Path) -> Result<Vec<SnapshotEntry>, String> {
    let output = Command::new("tmutil")
        .args(["listlocalsnapshots", &mount.to_string_lossy()])
        .output()
        .map_err(|e| format!("tmutil invocation failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "tmutil exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_tmutil_output(&stdout))
}

/// Run `diskutil apfs listSnapshots <mount>` and parse.
pub fn list_diskutil_snapshots(mount: &Path) -> Result<Vec<SnapshotEntry>, String> {
    let output = Command::new("diskutil")
        .args(["apfs", "listSnapshots", &mount.to_string_lossy()])
        .output()
        .map_err(|e| format!("diskutil invocation failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "diskutil exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_diskutil_output(&stdout))
}

/// Enumerate snapshots for one mount point. Returns
/// `SnapshotEnumeration` populated with both tmutil and diskutil
/// results; either may be empty if the underlying tool fails.
pub fn enumerate_mount(mount: &Path) -> SnapshotEnumeration {
    let tmutil_entries = list_tmutil_snapshots(mount).unwrap_or_default();
    let diskutil_entries = list_diskutil_snapshots(mount).unwrap_or_default();
    SnapshotEnumeration {
        mount_point: mount.to_string_lossy().into_owned(),
        tmutil_entries,
        diskutil_entries,
    }
}

/// Classify the host's snapshot state from one or more
/// per-mount-point enumerations. Used by the CLI summary path and
/// the integration harness; structural pattern mirrors EX-28's
/// `LiveScanOutcome` so the two harnesses look the same.
pub fn classify(enumerations: &[SnapshotEnumeration]) -> SnapshotVerdict {
    let mut user_visible: u32 = 0;
    let mut sealed_excluded: u32 = 0;
    for e in enumerations {
        for entry in e.tmutil_entries.iter().chain(e.diskutil_entries.iter()) {
            if entry.user_visible {
                user_visible += 1;
            } else {
                sealed_excluded += 1;
            }
        }
    }
    if user_visible > 0 {
        SnapshotVerdict::Enumerated {
            user_visible_count: user_visible,
            sealed_system_excluded_count: sealed_excluded,
        }
    } else {
        SnapshotVerdict::NoUserSnapshots {
            sealed_system_excluded_count: sealed_excluded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ex29_tmutil_no_snapshots_returns_empty() {
        let output = "Snapshots for disk /:\n";
        let parsed = parse_tmutil_output(output);
        assert!(parsed.is_empty());
    }

    #[test]
    fn ex29_tmutil_parses_user_snapshots() {
        let output = "Snapshots for disk /:\n\
                      com.apple.TimeMachine.2026-05-20-100000.local\n\
                      com.apple.TimeMachine.2026-05-20-110000.local\n";
        let parsed = parse_tmutil_output(output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "com.apple.TimeMachine.2026-05-20-100000.local");
        assert!(parsed[0].user_visible);
        assert_eq!(parsed[1].name, "com.apple.TimeMachine.2026-05-20-110000.local");
        assert!(parsed[1].user_visible);
    }

    #[test]
    fn ex29_tmutil_filters_sealed_system_snapshot() {
        let output = "Snapshots for disk /:\n\
                      com.apple.os.update-DEADBEEF\n";
        let parsed = parse_tmutil_output(output);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "com.apple.os.update-DEADBEEF");
        assert!(!parsed[0].user_visible);
    }

    /// The actual fixture string `diskutil` printed on this host
    /// today — one sealed-system snapshot, no user snapshots.
    /// Locks the parser against the real on-host output, not just
    /// a curated example.
    #[test]
    fn ex29_diskutil_parses_actual_host_fixture() {
        let output = "Snapshot for disk3s1s1 (1 found)\n\
                      |\n\
                      +-- 3E1AC922-F4EC-433E-B4D0-0052ADD81E03\n    \
                              Name:        com.apple.os.update-5514FF97DEE9C60C7FBF462B06A418D5FC4A882D5AF41D2BAF0A1419FB9B9F86\n    \
                              XID:         2293965\n    \
                              Purgeable:   No\n    \
                              NOTE:        This snapshot limits the minimum size of APFS Container disk3\n";
        let parsed = parse_diskutil_output(output);
        assert_eq!(parsed.len(), 1);
        let s = &parsed[0];
        assert_eq!(s.uuid.as_deref(), Some("3E1AC922-F4EC-433E-B4D0-0052ADD81E03"));
        assert_eq!(s.xid, Some(2293965));
        assert!(s.name.starts_with("com.apple.os.update-"));
        assert!(!s.user_visible);
    }

    #[test]
    fn ex29_diskutil_parses_multiple_snapshots() {
        let output = "Snapshot for disk3s5 (2 found)\n\
                      |\n\
                      +-- AAAA-BBBB-CCCC-DDDD-EEEE\n    \
                              Name:        com.apple.TimeMachine.2026-05-20-100000.local\n    \
                              XID:         100\n\
                      |\n\
                      +-- 1111-2222-3333-4444-5555\n    \
                              Name:        com.apple.TimeMachine.2026-05-20-110000.local\n    \
                              XID:         200\n";
        let parsed = parse_diskutil_output(output);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].user_visible);
        assert!(parsed[1].user_visible);
        assert_eq!(parsed[0].xid, Some(100));
        assert_eq!(parsed[1].xid, Some(200));
    }

    #[test]
    fn ex29_classify_no_user_snapshots_on_sealed_only_host() {
        // Matches the project owner's 2026-05-20 host state.
        let enums = vec![SnapshotEnumeration {
            mount_point: "/".into(),
            tmutil_entries: vec![],
            diskutil_entries: vec![SnapshotEntry {
                name: "com.apple.os.update-DEAD".into(),
                user_visible: false,
                uuid: Some("ABC".into()),
                xid: Some(1),
            }],
        }];
        let v = classify(&enums);
        assert!(matches!(
            v,
            SnapshotVerdict::NoUserSnapshots {
                sealed_system_excluded_count: 1
            }
        ));
    }

    #[test]
    fn ex29_classify_enumerated_when_user_snapshots_present() {
        let enums = vec![SnapshotEnumeration {
            mount_point: "/".into(),
            tmutil_entries: vec![SnapshotEntry {
                name: "com.apple.TimeMachine.2026-05-20-100000.local".into(),
                user_visible: true,
                uuid: None,
                xid: None,
            }],
            diskutil_entries: vec![],
        }];
        let v = classify(&enums);
        match v {
            SnapshotVerdict::Enumerated {
                user_visible_count,
                sealed_system_excluded_count,
            } => {
                assert_eq!(user_visible_count, 1);
                assert_eq!(sealed_system_excluded_count, 0);
            }
            other => panic!("expected Enumerated, got {other:?}"),
        }
    }

    #[test]
    fn ex29_classify_empty_enumeration_returns_no_snapshots() {
        let v = classify(&[]);
        assert!(matches!(
            v,
            SnapshotVerdict::NoUserSnapshots {
                sealed_system_excluded_count: 0
            }
        ));
    }
}
