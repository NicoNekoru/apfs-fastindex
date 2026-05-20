//! EX-28 shape-parity comparator.
//!
//! Two `NamespaceEntry` slices walked from the same APFS volume in the
//! same scan window should produce nearly-identical shapes. "Nearly"
//! because:
//!
//! - On a live mounted volume, files can come and go in the gap between
//!   the two scans (typical churn: `~/Library/Caches/*`, `/private/
//!   var/folders/*`, `.fseventsd`).
//! - The raw and fallback backends can disagree on `file_id` for the
//!   same path: the fallback walker emits the POSIX inode number from
//!   `lstat`, while the raw walker emits the APFS virtual OID. These
//!   happen to coincide on fresh fixtures but the spec allows
//!   divergence.
//!
//! `compare_namespace_shapes` returns a structured diff so the caller
//! (an integration test, a CLI flag, or future tooling) can decide
//! whether the diff is within tolerance. The EX-28 acceptance bound is
//! `< 100` symmetric-difference paths on an idle machine and any
//! per-row metric mismatch on a path present in both scans.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::NamespaceEntry;
#[cfg(test)]
use crate::EntryKind;

/// One row in a per-path metric comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PerPathDelta {
    pub path: String,
    pub field: &'static str,
    pub left: serde_json::Value,
    pub right: serde_json::Value,
}

/// Result of comparing two `NamespaceEntry` slices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct ShapeDiff {
    /// Paths present in `left` but not `right` (e.g. files that
    /// disappeared between successive scans).
    pub only_in_left: Vec<String>,
    /// Paths present in `right` but not `left`.
    pub only_in_right: Vec<String>,
    /// Per-row field mismatches on paths present in both. Common
    /// fields: `entry_kind`, `logical_size`, `allocated_size`,
    /// `real_size`, `symlink_target`. `file_id` is intentionally
    /// excluded — the raw/fallback contract permits divergence.
    pub mismatches: Vec<PerPathDelta>,
    pub left_count: u32,
    pub right_count: u32,
}

impl ShapeDiff {
    /// Symmetric difference: the count of paths present in exactly one
    /// side. The EX-28 acceptance bound is on this number.
    pub fn symmetric_difference(&self) -> usize {
        self.only_in_left.len() + self.only_in_right.len()
    }

    /// True iff every path matches on both sides and every field
    /// agrees. Strict; integration tests typically check
    /// `symmetric_difference()` and `mismatches.is_empty()` against
    /// experiment-specific tolerances.
    pub fn is_identical(&self) -> bool {
        self.only_in_left.is_empty()
            && self.only_in_right.is_empty()
            && self.mismatches.is_empty()
    }
}

/// Compare two `NamespaceEntry` slices for shape parity.
///
/// The comparison is path-keyed: every path in either slice appears
/// in exactly one of `only_in_left`, `only_in_right`, or as part of
/// the matched set. For matched paths, every field except `file_id`
/// (which raw/fallback may disagree on by design) is compared and
/// emitted to `mismatches` when divergent.
///
/// Paths in either slice must be unique within that slice; this is
/// always true for `NamespaceEntry` output (SR-018 keys are unique on
/// a well-formed APFS volume).
pub fn compare_namespace_shapes(left: &[NamespaceEntry], right: &[NamespaceEntry]) -> ShapeDiff {
    // Build path maps. NamespaceEntry.path is a Box<str>; we key by
    // its borrowed slice.
    let left_by_path: BTreeMap<&str, &NamespaceEntry> = left.iter().map(|e| (&*e.path, e)).collect();
    let right_by_path: BTreeMap<&str, &NamespaceEntry> =
        right.iter().map(|e| (&*e.path, e)).collect();

    let mut only_in_left: Vec<String> = left_by_path
        .keys()
        .filter(|p| !right_by_path.contains_key(*p))
        .map(|p| p.to_string())
        .collect();
    only_in_left.sort_unstable();
    let mut only_in_right: Vec<String> = right_by_path
        .keys()
        .filter(|p| !left_by_path.contains_key(*p))
        .map(|p| p.to_string())
        .collect();
    only_in_right.sort_unstable();

    let mut mismatches: Vec<PerPathDelta> = Vec::new();
    for (path, l) in &left_by_path {
        if let Some(r) = right_by_path.get(path) {
            push_mismatches(path, l, r, &mut mismatches);
        }
    }

    ShapeDiff {
        only_in_left,
        only_in_right,
        mismatches,
        left_count: left.len() as u32,
        right_count: right.len() as u32,
    }
}

fn push_mismatches(
    path: &str,
    l: &NamespaceEntry,
    r: &NamespaceEntry,
    out: &mut Vec<PerPathDelta>,
) {
    if l.entry_kind != r.entry_kind {
        out.push(PerPathDelta {
            path: path.to_string(),
            field: "entry_kind",
            left: serde_json::to_value(l.entry_kind).unwrap_or(serde_json::Value::Null),
            right: serde_json::to_value(r.entry_kind).unwrap_or(serde_json::Value::Null),
        });
    }
    if l.logical_size != r.logical_size {
        out.push(PerPathDelta {
            path: path.to_string(),
            field: "logical_size",
            left: serde_json::Value::from(l.logical_size),
            right: serde_json::Value::from(r.logical_size),
        });
    }
    if l.allocated_size != r.allocated_size {
        out.push(PerPathDelta {
            path: path.to_string(),
            field: "allocated_size",
            left: option_u64_value(l.allocated_size),
            right: option_u64_value(r.allocated_size),
        });
    }
    if l.real_size != r.real_size {
        out.push(PerPathDelta {
            path: path.to_string(),
            field: "real_size",
            left: option_u64_value(l.real_size),
            right: option_u64_value(r.real_size),
        });
    }
    let l_target = l.symlink_target.as_deref();
    let r_target = r.symlink_target.as_deref();
    if l_target != r_target {
        out.push(PerPathDelta {
            path: path.to_string(),
            field: "symlink_target",
            left: l_target.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
            right: r_target.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
        });
    }
}

fn option_u64_value(v: Option<u64>) -> serde_json::Value {
    match v {
        Some(n) => serde_json::Value::from(n),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(
        path: &str,
        kind: EntryKind,
        logical: u64,
        allocated: Option<u64>,
        real: Option<u64>,
    ) -> NamespaceEntry {
        NamespaceEntry {
            path: path.into(),
            entry_kind: kind,
            file_id: 0,
            logical_size: logical,
            symlink_target: None,
            allocated_size: allocated,
            real_size: real,
        }
    }

    #[test]
    fn ex28_identical_slices_diff_clean() {
        let a = vec![
            ent("a.txt", EntryKind::File, 100, Some(4096), Some(4096)),
            ent("b.txt", EntryKind::File, 200, Some(8192), Some(8192)),
        ];
        let diff = compare_namespace_shapes(&a, &a);
        assert!(diff.is_identical());
        assert_eq!(diff.symmetric_difference(), 0);
        assert_eq!(diff.left_count, 2);
        assert_eq!(diff.right_count, 2);
    }

    #[test]
    fn ex28_symmetric_difference_lists_extra_paths_on_each_side() {
        let a = vec![
            ent("ordinary.txt", EntryKind::File, 100, Some(4096), Some(4096)),
            ent("only_in_a.txt", EntryKind::File, 200, Some(8192), Some(8192)),
        ];
        let b = vec![
            ent("ordinary.txt", EntryKind::File, 100, Some(4096), Some(4096)),
            ent("only_in_b.txt", EntryKind::File, 300, Some(12288), Some(12288)),
        ];
        let diff = compare_namespace_shapes(&a, &b);
        assert!(!diff.is_identical());
        assert_eq!(diff.only_in_left, vec!["only_in_a.txt".to_string()]);
        assert_eq!(diff.only_in_right, vec!["only_in_b.txt".to_string()]);
        assert_eq!(diff.symmetric_difference(), 2);
        assert!(diff.mismatches.is_empty());
    }

    #[test]
    fn ex28_logical_size_disagreement_surfaces_as_mismatch() {
        let a = vec![ent("file.txt", EntryKind::File, 100, Some(4096), Some(4096))];
        let b = vec![ent("file.txt", EntryKind::File, 200, Some(4096), Some(4096))];
        let diff = compare_namespace_shapes(&a, &b);
        assert!(!diff.is_identical());
        assert_eq!(diff.mismatches.len(), 1);
        let m = &diff.mismatches[0];
        assert_eq!(m.path, "file.txt");
        assert_eq!(m.field, "logical_size");
    }

    #[test]
    fn ex28_allocated_and_real_diverge_independently() {
        let a = vec![ent("file.txt", EntryKind::File, 100, Some(4096), Some(4096))];
        let b = vec![ent("file.txt", EntryKind::File, 100, Some(8192), Some(4096))];
        let diff = compare_namespace_shapes(&a, &b);
        assert_eq!(diff.mismatches.len(), 1);
        assert_eq!(diff.mismatches[0].field, "allocated_size");
        // real_size is the same on both sides — no mismatch row.
    }

    #[test]
    fn ex28_symlink_target_compared() {
        let mut a_entry = ent("link", EntryKind::Symlink, 5, Some(0), Some(0));
        a_entry.symlink_target = Some("target_a".into());
        let mut b_entry = ent("link", EntryKind::Symlink, 5, Some(0), Some(0));
        b_entry.symlink_target = Some("target_b".into());
        let diff = compare_namespace_shapes(&[a_entry], &[b_entry]);
        assert_eq!(diff.mismatches.len(), 1);
        assert_eq!(diff.mismatches[0].field, "symlink_target");
    }

    #[test]
    fn ex28_file_id_divergence_is_intentionally_ignored() {
        // The raw/fallback contract permits file_id divergence (raw =
        // virtual OID, fallback = POSIX inode). compare_namespace_shapes
        // does NOT surface file_id mismatches.
        let mut a = ent("file.txt", EntryKind::File, 100, Some(4096), Some(4096));
        let mut b = ent("file.txt", EntryKind::File, 100, Some(4096), Some(4096));
        a.file_id = 42;
        b.file_id = 99;
        let diff = compare_namespace_shapes(&[a], &[b]);
        assert!(diff.is_identical());
    }

    #[test]
    fn ex28_empty_slices_diff_clean() {
        let diff = compare_namespace_shapes(&[], &[]);
        assert!(diff.is_identical());
        assert_eq!(diff.left_count, 0);
        assert_eq!(diff.right_count, 0);
    }
}
