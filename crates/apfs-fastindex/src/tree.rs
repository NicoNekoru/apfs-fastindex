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

use rustc_hash::FxBuildHasher;

use crate::{EntryKind, NamespaceEntry};

/// Per-dir child-lookup map used during `Tree::build`. The keys
/// are filenames (short, non-adversarial UTF-8), so the
/// DoS-resistant SipHash backing std's default `HashMap` is
/// overkill — fxhash is ~3× faster on this workload at the cost
/// of weaker collision resistance, which is irrelevant for
/// filesystem names. Bounded to build time only; the map is
/// discarded post-build.
type FxChildMap = HashMap<Box<str>, u32, FxBuildHasher>;

/// Sentinel value used by FFI callers for "no such node" /
/// "metric isn't a sum I can give you". `u32::MAX` matches the
/// `APFS_NODE_INVALID` constant in the C header.
pub const NODE_INVALID: u32 = u32::MAX;

/// One tree node. Holds the data the renderer needs in
/// directly-indexable form. `parent == None` only at the root
/// (index 0); every other node has a parent.
/// `name`, `path`, and `symlink_target` are stored as `Box<str>`
/// (16 bytes vs 24 for `String`) since `TreeNode` is built once
/// at scan-finalize and never mutated afterwards. On a /-scale
/// scan with ~3M nodes that's a ~24 MiB drop in the tree vec
/// alone, plus tighter cache lines per node walk.
#[derive(Debug)]
pub struct TreeNode {
    pub name: Box<str>,
    pub path: Box<str>,
    pub kind: EntryKind,
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    pub logical_size: u64,
    pub allocated_size: Option<u64>,
    pub symlink_target: Option<Box<str>>,
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
        // Pre-size the major buffers. `entries.len() + 1` is the
        // upper bound on node count (one node per entry plus
        // root); on real scans synthesised dirs make the actual
        // count a bit larger but the over-allocation cost is
        // tiny vs. the savings from skipping Vec growth.
        let cap = entries.len() + 1;
        let mut nodes: Vec<TreeNode> = Vec::with_capacity(cap);
        // Sparse: only directories need a child_map for the
        // duplicate-detection during build. Indexed by node_idx,
        // `None` for leaves. Saves ~5× the HashMap allocations
        // on a file-heavy scan (most rows are files). The key is
        // `Box<str>` rather than `String` so the map and the
        // owning `TreeNode.name` can share the same allocation
        // — we clone the box (one alloc) instead of allocating
        // the name string twice.
        let mut child_maps: Vec<Option<FxChildMap>> = Vec::with_capacity(cap);

        // Index 0 = root. Empty path; the renderer displays it
        // as "/" in the breadcrumb.
        nodes.push(TreeNode {
            name: Box::<str>::from(""),
            path: Box::<str>::from(""),
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
        child_maps.push(Some(FxChildMap::default()));

        for entry in entries {
            let path = &*entry.path;
            let bytes = path.as_bytes();
            let path_len = bytes.len();

            // Walk path components. At each separator the
            // current segment is either an existing dir we
            // descend into or a new node we create.
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
                            // Dirs may be referenced as parents
                            // by later entries; look up the
                            // parent's child_map to dedupe.
                            let cm = child_maps[cursor as usize]
                                .as_ref()
                                .expect("dir cursor must have a child_map");
                            if !cm.contains_key(name) {
                                let new_idx = nodes.len() as u32;
                                let parent_path = nodes[cursor as usize].path.as_ref();
                                let full_path = join_path(parent_path, name);
                                // Allocate `name` as Box<str>
                                // once; we share the allocation
                                // between the TreeNode and the
                                // child_map via Box::clone (one
                                // additional alloc, same as the
                                // old `name.to_string()` clone).
                                let name_box: Box<str> = Box::from(name);
                                nodes.push(TreeNode {
                                    name: name_box.clone(),
                                    path: full_path,
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
                                child_maps.push(Some(FxChildMap::default()));
                                nodes[cursor as usize].children.push(new_idx);
                                if let Some(cm_mut) = &mut child_maps[cursor as usize] {
                                    cm_mut.insert(name_box, new_idx);
                                }
                            }
                        }
                        _ => {
                            // Leaf: no child_map needed (we
                            // never look up files during build,
                            // and they have no children of
                            // their own).
                            let new_idx = nodes.len() as u32;
                            let parent_path = nodes[cursor as usize].path.as_ref();
                            let full_path = join_path(parent_path, name);
                            nodes.push(TreeNode {
                                name: Box::from(name),
                                path: full_path,
                                kind: entry.entry_kind,
                                parent: Some(cursor),
                                children: Vec::new(),
                                logical_size: entry.logical_size,
                                allocated_size: entry.allocated_size,
                                symlink_target: entry
                                    .symlink_target
                                    .as_deref()
                                    .map(Box::from),
                                value_logical: entry.logical_size,
                                value_allocated: entry.allocated_size,
                                item_count: 1,
                            });
                            child_maps.push(None);
                            nodes[cursor as usize].children.push(new_idx);
                        }
                    }
                } else {
                    let existing = child_maps[cursor as usize]
                        .as_ref()
                        .and_then(|cm| cm.get(name).copied());
                    let child = match existing {
                        Some(idx) => idx,
                        None => {
                            let new_idx = nodes.len() as u32;
                            let parent_path = nodes[cursor as usize].path.as_ref();
                            let full_path = join_path(parent_path, name);
                            let name_box: Box<str> = Box::from(name);
                            nodes.push(TreeNode {
                                name: name_box.clone(),
                                path: full_path,
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
                            child_maps.push(Some(FxChildMap::default()));
                            nodes[cursor as usize].children.push(new_idx);
                            if let Some(cm_mut) = &mut child_maps[cursor as usize] {
                                cm_mut.insert(name_box, new_idx);
                            }
                            new_idx
                        }
                    };
                    cursor = child;
                }

                seg_start = i + 1;
                i += 1;
            }
        }

        // No global `path_index` HashMap any more. On a
        // file-heavy /-scan the eager build was the largest
        // single share of Tree::build cost (500 k+ HashMap
        // inserts of strings nobody ever queries). The few
        // callers that *do* need path → node lookups (the
        // breadcrumb's back-nav, programmatic seek) walk the
        // tree via `node_index_for_path` instead — O(depth) +
        // O(siblings) per lookup, which is fine at the rates
        // those callers run.
        let mut tree = Tree { nodes };
        tree.finalize();
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

    /// Look up a node by its absolute logical path. The empty
    /// string and `"/"` both map to the root (index 0).
    /// Returns `NODE_INVALID` if no such node exists.
    ///
    /// Walks the tree component-by-component (linear scan of
    /// each parent's children at every level). On a directory
    /// with ~100 children that's ~100 string comparisons per
    /// step — fine at typical UI rates. The eager
    /// `HashMap<String, u32>` we used to maintain cost ~600 ms
    /// on a /Library-scale scan and went unused on every
    /// scan that didn't actually issue a path lookup.
    pub fn node_index_for_path(&self, path: &str) -> u32 {
        let key = if path == "/" { "" } else { path };
        if key.is_empty() {
            return 0;
        }
        let bytes = key.as_bytes();
        let len = bytes.len();
        let mut cursor: u32 = 0;
        let mut seg_start: usize = 0;
        let mut i: usize = 0;
        while i <= len {
            let at_end = i == len;
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
            // SAFETY: caller passes a UTF-8 path; ASCII '/'
            // boundaries leave each segment UTF-8-valid.
            let name = unsafe { std::str::from_utf8_unchecked(&bytes[seg_start..i]) };
            let children = &self.nodes[cursor as usize].children;
            let mut found: Option<u32> = None;
            for &child_idx in children {
                if &*self.nodes[child_idx as usize].name == name {
                    found = Some(child_idx);
                    break;
                }
            }
            match found {
                Some(c) => cursor = c,
                None => return NODE_INVALID,
            }
            seg_start = i + 1;
            i += 1;
        }
        cursor
    }
}

/// Build `parent/name` (or just `name` if parent is empty) into
/// a `Box<str>` with exactly the right capacity reserved. The
/// returned box is built from a `String` with a sized buffer (no
/// realloc, no growth slack) so we never carry unused capacity
/// past the build phase.
fn join_path(parent: &str, name: &str) -> Box<str> {
    if parent.is_empty() {
        return Box::from(name);
    }
    let mut s = String::with_capacity(parent.len() + 1 + name.len());
    s.push_str(parent);
    s.push('/');
    s.push_str(name);
    s.into_boxed_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntryKind;

    fn entry(path: &str, kind: EntryKind, logical: u64, allocated: Option<u64>) -> NamespaceEntry {
        NamespaceEntry {
            path: path.into(),
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
