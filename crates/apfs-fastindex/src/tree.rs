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
/// `name` and `symlink_target` are stored as `Box<str>` (16
/// bytes vs 24 for `String`) since `TreeNode` is built once at
/// scan-finalize and never mutated afterwards.
///
/// `path` is deliberately *not* stored — the full absolute path
/// is `parent.path + "/" + self.name`, and the only consumer
/// that materialises it is the FFI (`apfs_scan_node_path`),
/// which is called O(visible-cells) per UI session, not
/// O(nodes). Computing on demand and caching the result on
/// `ApfsScan` saves 16 bytes per node (~50 MiB on a /-scale
/// scan) and eliminates the `join_path` allocation that
/// happened once per node during build (~3M allocations).
/// Callers that need the path call `Tree::compute_path`.
#[derive(Debug)]
pub struct TreeNode {
    pub name: Box<str>,
    pub kind: EntryKind,
    pub parent: Option<u32>,
    /// Children of this node live contiguously in
    /// `Tree::children_arena[children_start..children_start +
    /// children_count]`. The flat-arena layout replaces a
    /// `Vec<u32>` per node (24 B) with two `u32`s (8 B) — saves
    /// 16 B / node, ~53 MiB on a /-scale tree — plus eliminates
    /// the per-dir Vec heap allocations (one alloc per non-leaf
    /// directory, ~200k on /). Use `Tree::children_of(idx)` to
    /// get a borrowed slice; the FFI exposes the same pair as
    /// `apfs_scan_node_children`.
    pub children_start: u32,
    pub children_count: u32,
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
    /// Flat children arena — `Tree::children_of` slices into
    /// this. Stored on `Tree` rather than per-node so the bytes
    /// are contiguous and dense (one big allocation instead of
    /// one Vec per dir).
    pub children_arena: Vec<u32>,
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
        // Build directly into the final `TreeNode` shape with
        // `(children_start, children_count) = (0, 0)` as a
        // placeholder; a parallel `child_lists: Vec<Vec<u32>>`
        // captures the per-dir children-as-we-find-them. Once
        // the entries loop is done we flatten `child_lists` into
        // `Tree::children_arena` and patch the placeholders.
        //
        // This keeps the peak working set down: there's never a
        // moment where two full-sized node Vecs coexist. The
        // side-car `Vec<Vec<u32>>` is freed at flatten time.
        let mut nodes: Vec<TreeNode> = Vec::with_capacity(cap);
        let mut child_lists: Vec<Vec<u32>> = Vec::with_capacity(cap);
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
            kind: EntryKind::Dir,
            parent: None,
            children_start: 0,
            children_count: 0,
            logical_size: 0,
            allocated_size: None,
            symlink_target: None,
            value_logical: 0,
            value_allocated: None,
            item_count: 0,
        });
        // Pre-size dir child Vecs to 8 — most dirs on a macOS
        // volume have 1-32 children (median ~8). Skips the first
        // three Vec doublings (1→2→4→8) for the median, trimming
        // allocator pressure during the entries loop. Leaves get
        // a `Vec::new()` so they don't allocate (see below).
        child_lists.push(Vec::with_capacity(8));
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
                                // Allocate `name` as Box<str>
                                // once; we share the allocation
                                // between the TreeNode and the
                                // child_map via Box::clone.
                                let name_box: Box<str> = Box::from(name);
                                nodes.push(TreeNode {
                                    name: name_box.clone(),
                                    kind: EntryKind::Dir,
                                    parent: Some(cursor),
                                    children_start: 0,
                                    children_count: 0,
                                    logical_size: 0,
                                    allocated_size: None,
                                    symlink_target: None,
                                    value_logical: 0,
                                    value_allocated: None,
                                    item_count: 0,
                                });
                                // Pre-sized dir child Vec (see
                                // root-push comment for rationale).
                                child_lists.push(Vec::with_capacity(8));
                                child_maps.push(Some(FxChildMap::default()));
                                child_lists[cursor as usize].push(new_idx);
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
                            nodes.push(TreeNode {
                                name: Box::from(name),
                                kind: entry.entry_kind,
                                parent: Some(cursor),
                                children_start: 0,
                                children_count: 0,
                                logical_size: entry.logical_size,
                                allocated_size: entry.allocated_size,
                                symlink_target: entry.symlink_target.as_deref().map(Box::from),
                                value_logical: entry.logical_size,
                                value_allocated: entry.allocated_size,
                                item_count: 1,
                            });
                            // Leaves never push into their own
                            // child slot — keep the Vec empty
                            // so it doesn't allocate a heap
                            // backing. Pre-sizing here would
                            // waste 32 B × ~3M leaves on /.
                            child_lists.push(Vec::new());
                            child_maps.push(None);
                            child_lists[cursor as usize].push(new_idx);
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
                            let name_box: Box<str> = Box::from(name);
                            nodes.push(TreeNode {
                                name: name_box.clone(),
                                kind: EntryKind::Dir,
                                parent: Some(cursor),
                                children_start: 0,
                                children_count: 0,
                                logical_size: 0,
                                allocated_size: None,
                                symlink_target: None,
                                value_logical: 0,
                                value_allocated: None,
                                item_count: 0,
                            });
                            // Pre-sized dir child Vec (see
                            // root-push comment for rationale).
                            child_lists.push(Vec::with_capacity(8));
                            child_maps.push(Some(FxChildMap::default()));
                            child_lists[cursor as usize].push(new_idx);
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
        drop(child_maps);

        // Phase 2 — flatten the side-car `child_lists` into one
        // contiguous `children_arena` and patch each TreeNode's
        // placeholder `(children_start, children_count)` in
        // place. Exactly `nodes.len() - 1` u32s total (every
        // non-root node is a child of exactly one parent).
        let total_children = nodes.len().saturating_sub(1);
        let mut children_arena: Vec<u32> = Vec::with_capacity(total_children);
        for (i, cl) in child_lists.iter().enumerate() {
            nodes[i].children_start = children_arena.len() as u32;
            nodes[i].children_count = cl.len() as u32;
            children_arena.extend_from_slice(cl);
        }
        drop(child_lists);

        let mut tree = Tree {
            nodes,
            children_arena,
        };
        tree.finalize();
        tree
    }

    /// Borrow this node's immediate-children indices. O(1) — a
    /// pointer + length pair into `children_arena`. Returns an
    /// empty slice for leaves and for out-of-range indices.
    #[inline]
    pub fn children_of(&self, idx: u32) -> &[u32] {
        let Some(node) = self.nodes.get(idx as usize) else {
            return &[];
        };
        let start = node.children_start as usize;
        let end = start + node.children_count as usize;
        &self.children_arena[start..end]
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
            let node = &self.nodes[idx as usize];
            let start = node.children_start as usize;
            let end = start + node.children_count as usize;
            for k in (start..end).rev() {
                stack.push((self.children_arena[k], false));
            }
        }

        for idx in post_order {
            let ni = idx as usize;
            let is_dir = matches!(self.nodes[ni].kind, EntryKind::Dir);

            // Seed with own contribution. For dirs, sums start at
            // zero — only leaves carry real `logical_size` etc.;
            // dirs' own `allocated_size` is `Some(0)` in the
            // fallback walker by SR-019 contract.
            let mut sum_logical: u64 = if is_dir {
                0
            } else {
                self.nodes[ni].logical_size
            };
            let mut sum_allocated: Option<u64> = if is_dir {
                Some(0)
            } else {
                self.nodes[ni].allocated_size
            };
            let mut sum_items: u64 = if is_dir { 0 } else { 1 };

            // Sum children. Walk the arena slice directly —
            // contiguous u32s are cache-friendly, and we already
            // know the range from the parent node's metadata.
            let start = self.nodes[ni].children_start as usize;
            let end = start + self.nodes[ni].children_count as usize;
            for k in start..end {
                let child_idx = self.children_arena[k] as usize;
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
            let mut found: Option<u32> = None;
            for &child_idx in self.children_of(cursor) {
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

impl Tree {
    /// Materialise the absolute path for `idx` by walking the
    /// parent chain. Returns `""` for the root (idx 0) and for
    /// out-of-range indices.
    ///
    /// O(depth) in tree depth + path bytes. Typical paths on a
    /// macOS volume are < 50 bytes and depth < 12, so each call
    /// is a few hundred cycles. The FFI wraps this with a cache
    /// keyed on `idx` so repeated queries (hover, click,
    /// breadcrumb) don't repeat the walk.
    pub fn compute_path(&self, idx: u32) -> Box<str> {
        if (idx as usize) >= self.nodes.len() || idx == 0 {
            return Box::from("");
        }
        // First pass: walk to root, summing byte length.
        let mut total: usize = 0;
        let mut cur = idx;
        loop {
            let node = &self.nodes[cur as usize];
            total += node.name.len();
            match node.parent {
                // Add a separator for every step *between* names.
                // Skip the leading slash for direct root children
                // (parent == 0): the root's path is "", not "/".
                Some(p) if p != 0 => total += 1,
                _ => break,
            }
            cur = node.parent.unwrap();
        }
        // Second pass: build front-to-back into a sized String.
        let mut s = String::with_capacity(total);
        write_path_into(&self.nodes, idx, &mut s);
        s.into_boxed_str()
    }
}

/// Recursively appends `nodes[idx]`'s ancestor path (excluding
/// the synthetic root at index 0) into `out`. Recursion depth
/// is bounded by the filesystem path depth (< 32 on real
/// volumes), so stack use is fine.
fn write_path_into(nodes: &[TreeNode], idx: u32, out: &mut String) {
    if idx == 0 {
        return;
    }
    let node = &nodes[idx as usize];
    if let Some(p) = node.parent {
        if p != 0 {
            write_path_into(nodes, p, out);
            out.push('/');
        }
    }
    out.push_str(&node.name);
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
