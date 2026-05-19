//! In-memory tree built from a `Vec<NamespaceEntry>`.
//!
//! The treemap renderer needs random-access reads by node — for
//! the breadcrumb, the tree-list, the layout-cache key, the
//! squarify recursion, hit-test → navigation. A flat Vec of
//! entries doesn't give us that without re-scanning, so this
//! module turns the entry list into an indexed tree once per
//! scan and exposes index-based access from there on.
//!
//! Data shape:
//!
//! ```text
//! Tree {
//!   nodes: Vec<TreeNode>,        // index 0 is the synthetic root
//!   path_index: HashMap<String, u32>,
//! }
//!
//! TreeNode {
//!   name, path, kind, symlink_target,
//!   parent: Option<u32>,
//!   children: Vec<u32>,
//!   logical_size, allocated_size,   // own contribution
//!   value_logical, value_allocated, item_count,  // aggregates
//! }
//! ```
//!
//! Build is a single pass over `entries`, with a per-dir
//! `HashMap<name, child_index>` used during construction. After
//! the build the per-dir maps are dropped; consumers use the
//! global `path_index` for `path → node` lookups.
//!
//! Aggregates are computed iteratively in post-order over the
//! built tree (no recursion → no stack-overflow on /-class
//! filesystems with paths 30+ deep).

use std::collections::HashMap;

use crate::{EntryKind, NamespaceEntry};

/// Sentinel value used by FFI callers for "no such node" /
/// "metric isn't a sum I can give you". `u32::MAX` matches the
/// `APFS_NODE_INVALID` constant in the C header.
pub const NODE_INVALID: u32 = u32::MAX;

/// One tree node. Holds the data the renderer needs in
/// directly-indexable form. `parent == None` only at the root
/// (index 0); every other node has a parent.
#[derive(Debug)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    pub logical_size: u64,
    pub allocated_size: Option<u64>,
    pub symlink_target: Option<String>,
    /// Sum of `logical_size` for this subtree (own + descendants).
    pub value_logical: u64,
    /// Sum of `allocated_size` for this subtree, or `None` if the
    /// SR-019 None-collapse fired in any descendant.
    pub value_allocated: Option<u64>,
    /// Number of non-directory descendants (matches the JS
    /// renderer's `itemCount`).
    pub item_count: u64,
}

#[derive(Debug)]
pub struct Tree {
    pub nodes: Vec<TreeNode>,
    pub path_index: HashMap<String, u32>,
}

impl Tree {
    /// Build the tree from a slice of namespace entries. The
    /// synthetic root (index 0) carries an empty `path`
    /// (consumers map it to `"/"` in display); every other node's
    /// `path` is its absolute logical path with `/` separators,
    /// matching the entry's `path` field.
    ///
    /// Per-dir lookups during construction use a local
    /// `HashMap<String, u32>`. The map is discarded once the dir
    /// is "settled" (no more entries can target it, conservatively
    /// approximated by emptying it at the end of construction).
    pub fn build(entries: &[NamespaceEntry]) -> Self {
        let mut nodes: Vec<TreeNode> = Vec::with_capacity(entries.len() + 1);
        let mut child_maps: Vec<HashMap<String, u32>> = Vec::with_capacity(entries.len() + 1);
        let mut path_index: HashMap<String, u32> = HashMap::with_capacity(entries.len() + 1);

        // Index 0 = root. Empty path; the renderer renders it as
        // "/" in the breadcrumb.
        nodes.push(TreeNode {
            name: String::new(),
            path: String::new(),
            kind: EntryKind::Dir,
            parent: None,
            children: Vec::new(),
            logical_size: 0,
            allocated_size: None,
            symlink_target: None,
            value_logical: 0,
            value_allocated: None,
            item_count: 0,
        });
        child_maps.push(HashMap::new());
        path_index.insert(String::new(), 0);

        for entry in entries {
            let path = entry.path.as_str();
            let bytes = path.as_bytes();
            let path_len = bytes.len();

            // Walk path components. Mirrors the JS hot loop:
            //   - Skip leading/duplicate '/'.
            //   - Each non-empty segment is either an existing dir
            //     we descend into or a new node we create.
            //   - The last segment is the entry itself (dir or leaf).
            let mut cursor: u32 = 0;
            let mut seg_start: usize = 0;
            let mut i: usize = 0;
            while i <= path_len {
                let at_end = i == path_len;
                let is_sep = at_end || bytes[i] == b'/';
                if !is_sep {
                    i += 1;
                    continue;
                }
                if i == seg_start {
                    seg_start = i + 1;
                    i += 1;
                    continue;
                }
                let is_last = at_end;
                // SAFETY: `path` is UTF-8; segment boundaries are
                // ASCII `/`, so slicing on byte indices is safe.
                let name = unsafe { std::str::from_utf8_unchecked(&bytes[seg_start..i]) };

                if is_last {
                    match entry.entry_kind {
                        EntryKind::Dir => {
                            let exists = child_maps[cursor as usize]
                                .get(name)
                                .copied();
                            if exists.is_none() {
                                let new_idx = nodes.len() as u32;
                                let full_path = if nodes[cursor as usize].path.is_empty() {
                                    name.to_string()
                                } else {
                                    format!("{}/{}", &nodes[cursor as usize].path, name)
                                };
                                nodes.push(TreeNode {
                                    name: name.to_string(),
                                    path: full_path.clone(),
                                    kind: EntryKind::Dir,
                                    parent: Some(cursor),
                                    children: Vec::new(),
                                    logical_size: 0,
                                    allocated_size: None,
                                    symlink_target: None,
                                    value_logical: 0,
                                    value_allocated: None,
                                    item_count: 0,
                                });
                                child_maps.push(HashMap::new());
                                nodes[cursor as usize].children.push(new_idx);
                                child_maps[cursor as usize]
                                    .insert(name.to_string(), new_idx);
                                path_index.insert(full_path, new_idx);
                            }
                        }
                        _ => {
                            let new_idx = nodes.len() as u32;
                            let full_path = if nodes[cursor as usize].path.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}/{}", &nodes[cursor as usize].path, name)
                            };
                            nodes.push(TreeNode {
                                name: name.to_string(),
                                path: full_path.clone(),
                                kind: entry.entry_kind,
                                parent: Some(cursor),
                                children: Vec::new(),
                                logical_size: entry.logical_size,
                                allocated_size: entry.allocated_size,
                                symlink_target: entry.symlink_target.clone(),
                                value_logical: entry.logical_size,
                                value_allocated: entry.allocated_size,
                                item_count: 1,
                            });
                            child_maps.push(HashMap::new());
                            nodes[cursor as usize].children.push(new_idx);
                            child_maps[cursor as usize]
                                .insert(name.to_string(), new_idx);
                            path_index.insert(full_path, new_idx);
                        }
                    }
                } else {
                    let existing = child_maps[cursor as usize].get(name).copied();
                    let child = match existing {
                        Some(idx) => idx,
                        None => {
                            let new_idx = nodes.len() as u32;
                            let full_path = if nodes[cursor as usize].path.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}/{}", &nodes[cursor as usize].path, name)
                            };
                            nodes.push(TreeNode {
                                name: name.to_string(),
                                path: full_path.clone(),
                                kind: EntryKind::Dir,
                                parent: Some(cursor),
                                children: Vec::new(),
                                logical_size: 0,
                                allocated_size: None,
                                symlink_target: None,
                                value_logical: 0,
                                value_allocated: None,
                                item_count: 0,
                            });
                            child_maps.push(HashMap::new());
                            nodes[cursor as usize].children.push(new_idx);
                            child_maps[cursor as usize]
                                .insert(name.to_string(), new_idx);
                            path_index.insert(full_path, new_idx);
                            new_idx
                        }
                    };
                    cursor = child;
                }

                seg_start = i + 1;
                i += 1;
            }
        }

        let mut tree = Tree { nodes, path_index };
        tree.finalize();
        // Drop the per-dir maps; they were construction-only.
        // `path_index` stays as the global lookup.
        drop(child_maps);
        tree
    }

    /// Compute `value_logical`, `value_allocated`, `item_count`
    /// for every node by walking post-order. Iterative to avoid
    /// blowing the stack on deep filesystems.
    fn finalize(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        // Compute post-order via a two-marker stack pass: each
        // node is visited twice (down then up); on the up-pass
        // we record the index, giving us post-order.
        let mut post_order: Vec<u32> = Vec::with_capacity(n);
        let mut stack: Vec<(u32, bool)> = vec![(0, false)];
        while let Some((idx, visited)) = stack.pop() {
            if visited {
                post_order.push(idx);
                continue;
            }
            stack.push((idx, true));
            // Children pushed in order — `Vec::pop` pulls the
            // last one first, so reverse-iterate to keep the
            // left-to-right traversal stable.
            let child_count = self.nodes[idx as usize].children.len();
            for k in (0..child_count).rev() {
                let c = self.nodes[idx as usize].children[k];
                stack.push((c, false));
            }
        }

        for idx in post_order {
            let ni = idx as usize;
            let is_dir = matches!(self.nodes[ni].kind, EntryKind::Dir);

            // Seed with own contribution. For dirs, sums start at
            // zero — only leaves carry real `logical_size` etc.;
            // dirs' own `allocated_size` is `Some(0)` in the
            // fallback walker by SR-019 contract.
            let mut sum_logical: u64 = if is_dir { 0 } else { self.nodes[ni].logical_size };
            let mut sum_allocated: Option<u64> =
                if is_dir { Some(0) } else { self.nodes[ni].allocated_size };
            let mut sum_items: u64 = if is_dir { 0 } else { 1 };

            // Sum children. Re-borrow per-child to keep the
            // borrow checker happy.
            let child_count = self.nodes[ni].children.len();
            for k in 0..child_count {
                let child_idx = self.nodes[ni].children[k] as usize;
                let c = &self.nodes[child_idx];
                sum_logical = sum_logical.saturating_add(c.value_logical);
                sum_allocated = match (sum_allocated, c.value_allocated) {
                    (Some(s), Some(cv)) => Some(s.saturating_add(cv)),
                    _ => None,
                };
                sum_items = sum_items.saturating_add(c.item_count);
            }

            let n = &mut self.nodes[ni];
            n.value_logical = sum_logical;
            n.value_allocated = sum_allocated;
            n.item_count = sum_items;
        }
    }

    /// Look up a node by its absolute logical path. Returns
    /// `NODE_INVALID` if no such node exists. The empty string
    /// maps to the root (index 0).
    pub fn node_index_for_path(&self, path: &str) -> u32 {
        // "/" is also a synonym for root, since the renderer
        // writes the root crumb that way.
        let key = if path == "/" { "" } else { path };
        match self.path_index.get(key) {
            Some(&idx) => idx,
            None => NODE_INVALID,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntryKind;

    fn entry(path: &str, kind: EntryKind, logical: u64, allocated: Option<u64>) -> NamespaceEntry {
        NamespaceEntry {
            path: path.to_string(),
            entry_kind: kind,
            file_id: 0,
            logical_size: logical,
            symlink_target: None,
            allocated_size: allocated,
        }
    }

    #[test]
    fn tree_build_handles_nested_paths() {
        let entries = vec![
            entry("a", EntryKind::Dir, 0, Some(0)),
            entry("a/b", EntryKind::Dir, 0, Some(0)),
            entry("a/b/file.txt", EntryKind::File, 100, Some(120)),
            entry("a/b/other.txt", EntryKind::File, 50, Some(64)),
            entry("a/c", EntryKind::File, 200, Some(256)),
        ];
        let tree = Tree::build(&entries);
        // root + a + b + file.txt + other.txt + c = 6 nodes
        assert_eq!(tree.nodes.len(), 6);
        let root = &tree.nodes[0];
        assert_eq!(root.value_logical, 100 + 50 + 200);
        assert_eq!(root.value_allocated, Some(120 + 64 + 256));
        assert_eq!(root.item_count, 3);
    }

    #[test]
    fn tree_allocated_collapses_on_none() {
        let entries = vec![
            entry("a", EntryKind::Dir, 0, Some(0)),
            entry("a/sparse.bin", EntryKind::File, 1_000_000, None),
            entry("a/normal.bin", EntryKind::File, 100, Some(120)),
        ];
        let tree = Tree::build(&entries);
        let a_idx = tree.node_index_for_path("a");
        assert_ne!(a_idx, NODE_INVALID);
        let a = &tree.nodes[a_idx as usize];
        // Sparse entry has None — collapses up the chain.
        assert_eq!(a.value_allocated, None);
        // Logical total unaffected.
        assert_eq!(a.value_logical, 1_000_100);
    }

    #[test]
    fn tree_root_path_lookup() {
        let entries = vec![entry("file.txt", EntryKind::File, 10, Some(16))];
        let tree = Tree::build(&entries);
        assert_eq!(tree.node_index_for_path(""), 0);
        assert_eq!(tree.node_index_for_path("/"), 0);
        assert_ne!(tree.node_index_for_path("file.txt"), NODE_INVALID);
        assert_eq!(tree.node_index_for_path("nonexistent"), NODE_INVALID);
    }
}
