//! Per-extension aggregate for the ext-list panel.
//!
//! Walks the subtree rooted at a given node, groups every leaf
//! by its file extension, and emits one row per unique extension
//! with `(value_logical, value_allocated, file_count)`. Sorted
//! descending by the active metric so the largest contributors
//! land at the top of the panel.
//!
//! Doing the walk + group in Rust avoids ~N FFI round trips
//! that a Swift-side aggregator would pay (one `children(of:)`
//! call per dir, one `kind(of:)` + `name(of:)` per leaf — for a
//! /Applications scan that's ~150 k crossings). The Rust walk
//! processes each node from local memory and is bounded by the
//! `HashMap<String, ...>` insertion cost.

use std::collections::HashMap;

use crate::render::Metric;
use crate::tree::Tree;
use crate::EntryKind;

/// One row of the ext-list. Sorted into `ExtSummary.rows` by
/// the metric used to build it.
#[derive(Debug, Clone)]
pub struct ExtRow {
    /// Lowercase extension including the leading dot
    /// (e.g. `.pdf`), or one of the synthetic markers
    /// `"(no ext)"`, `"(symlink)"`, `"(other)"`.
    pub ext: String,
    pub value_logical: u64,
    /// `None` when at least one contributing file's
    /// `allocated_size` was SR-019 None-collapsed.
    pub value_allocated: Option<u64>,
    pub file_count: u32,
}

/// Snapshot of every extension contributing to a subtree.
#[derive(Debug)]
pub struct ExtSummary {
    pub rows: Vec<ExtRow>,
    /// Sum of the active-metric values across rows, used by the
    /// Swift panel for the per-row percent column.
    pub total_value: u64,
    /// True iff any contributing file's allocated value was
    /// None (SR-019 None-collapse). Surface in the panel
    /// subtitle so the user knows "allocated unclaimed" might
    /// appear in individual rows.
    pub any_unclaimed: bool,
}

impl ExtSummary {
    /// Build the summary for the subtree rooted at `node_idx`.
    /// Iterative descent — no recursion — so deeply-nested
    /// filesystems don't blow the stack.
    pub fn build(tree: &Tree, node_idx: u32, metric: Metric) -> Self {
        let mut by_ext: HashMap<String, (u64, Option<u64>, u32)> = HashMap::new();
        let mut any_unclaimed = false;
        if (node_idx as usize) >= tree.nodes.len() {
            return ExtSummary {
                rows: Vec::new(),
                total_value: 0,
                any_unclaimed: false,
            };
        }
        let mut stack: Vec<u32> = vec![node_idx];
        while let Some(idx) = stack.pop() {
            let n = &tree.nodes[idx as usize];
            if matches!(n.kind, EntryKind::Dir) {
                for &c in &n.children {
                    stack.push(c);
                }
                continue;
            }
            let ext = extension_of(&n.name, n.kind);
            let slot = by_ext.entry(ext).or_insert((0, Some(0), 0));
            slot.0 = slot.0.saturating_add(n.logical_size);
            slot.1 = match (slot.1, n.allocated_size) {
                (Some(a), Some(v)) => Some(a.saturating_add(v)),
                _ => {
                    if n.allocated_size.is_none() {
                        any_unclaimed = true;
                    }
                    None
                }
            };
            slot.2 = slot.2.saturating_add(1);
        }
        let mut rows: Vec<ExtRow> = by_ext
            .into_iter()
            .map(|(ext, (l, a, c))| ExtRow {
                ext,
                value_logical: l,
                value_allocated: a,
                file_count: c,
            })
            .collect();
        // Sort descending by the active metric. Stable on the
        // extension name so the panel's order is reproducible
        // when two extensions tie.
        rows.sort_by(|a, b| {
            let av = match metric {
                Metric::Logical => a.value_logical,
                Metric::Allocated => a.value_allocated.unwrap_or(0),
            };
            let bv = match metric {
                Metric::Logical => b.value_logical,
                Metric::Allocated => b.value_allocated.unwrap_or(0),
            };
            bv.cmp(&av).then_with(|| a.ext.cmp(&b.ext))
        });
        let total_value: u64 = rows
            .iter()
            .map(|r| match metric {
                Metric::Logical => r.value_logical,
                Metric::Allocated => r.value_allocated.unwrap_or(0),
            })
            .sum();
        ExtSummary {
            rows,
            total_value,
            any_unclaimed,
        }
    }
}

/// Derive the lowercase extension (with leading dot) from a
/// filename. Mirrors the JS canvas-era `extensionOf`:
///   - symlink → `(symlink)`
///   - other / dir → `(other)` / `(dir)` markers
///   - file with no usable dot → `(no ext)`
///   - otherwise the segment after the last dot, lowercased,
///     prefixed with `.`.
fn extension_of(name: &str, kind: EntryKind) -> String {
    match kind {
        EntryKind::Symlink => "(symlink)".to_string(),
        EntryKind::Other => "(other)".to_string(),
        EntryKind::Dir => "(dir)".to_string(),
        EntryKind::File => {
            if let Some(idx) = name.rfind('.') {
                // Reject leading-dot (hidden files: `.gitignore`)
                // and trailing-dot ("file.") cases — they don't
                // carry a usable extension.
                if idx > 0 && idx < name.len() - 1 {
                    return name[idx..].to_ascii_lowercase();
                }
            }
            "(no ext)".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Tree;
    use crate::{EntryKind, NamespaceEntry};

    fn entry(path: &str, kind: EntryKind, logical: u64) -> NamespaceEntry {
        NamespaceEntry {
            path: path.into(),
            entry_kind: kind,
            file_id: 0,
            logical_size: logical,
            symlink_target: None,
            allocated_size: Some(logical),
        }
    }

    #[test]
    fn ext_summary_groups_by_lowercase_extension() {
        let entries = vec![
            entry("a", EntryKind::Dir, 0),
            entry("a/Photo.JPG", EntryKind::File, 1000),
            entry("a/another.jpg", EntryKind::File, 2000),
            entry("a/code.rs", EntryKind::File, 500),
            entry("a/no-ext", EntryKind::File, 250),
            entry("a/.hidden", EntryKind::File, 100),
        ];
        let tree = Tree::build(&entries);
        let summary = ExtSummary::build(&tree, 0, Metric::Logical);
        // .jpg (3000) > .rs (500) > (no ext) (250 + 100 = 350)
        // — `.hidden` is treated as no-ext because the dot is
        // at index 0.
        assert_eq!(summary.rows.len(), 3);
        assert_eq!(summary.rows[0].ext, ".jpg");
        assert_eq!(summary.rows[0].value_logical, 3000);
        assert_eq!(summary.rows[0].file_count, 2);
        assert_eq!(summary.rows[1].ext, ".rs");
        assert_eq!(summary.rows[2].ext, "(no ext)");
        assert_eq!(summary.total_value, 1000 + 2000 + 500 + 250 + 100);
    }
}
