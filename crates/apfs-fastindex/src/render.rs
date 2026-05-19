//! Treemap layout + cell flattening.
//!
//! Port of d3-hierarchy's `treemapSquarify` to Rust, paired with
//! the cell-flatten + colour logic the JS canvas renderer used to
//! own. The output is a flat `Vec<ApfsCell>` Swift reads directly
//! via `UnsafeBufferPointer<ApfsCell>` — no per-cell FFI calls.
//!
//! Layout flow:
//!
//! 1. Walk the subtree rooted at `node_idx` pre-order.
//! 2. For each interior node, run `squarify` on its children
//!    (sorted descending by the active metric) inside the node's
//!    rect minus any padding (paddingOuter on every side,
//!    paddingTop reserved for the label strip when the dir is
//!    big enough to fit one).
//! 3. Emit one `ApfsCell` per visible node — drop anything below
//!    `MIN_PIXEL_AREA` and stop recursing past `max_depth`.
//!
//! Squarify reference: Bruls, Huijsen, van Wijk (2000)
//! "Squarified Treemaps". d3-hierarchy's implementation is the
//! direct source.

use crate::tree::Tree;
use crate::EntryKind;

/// Per-cell record exposed across the FFI. `#[repr(C)]` so Swift
/// can read the struct via `UnsafeBufferPointer<ApfsCell>`
/// without any per-field unwrapping. 32 bytes/cell; ~6.4 MB for
/// a 200 k-cell `/`-scan view.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct ApfsCell {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub depth: u32,
    pub node_index: u32,
    /// Bit 0: is_dir. Bit 1: is_symlink. Bit 2: paddingTop
    /// reserved for label (only set on dir cells big enough to
    /// host the label strip).
    pub flags: u32,
    /// `0x00RRGGBB`. Pre-computed by the layout pass so the
    /// renderer (whether Swift CG or a future Metal pipeline)
    /// reads the colour directly without re-running the
    /// extension lookup per draw.
    pub fill_rgb: u32,
}

pub const CELL_FLAG_DIR: u32 = 1 << 0;
pub const CELL_FLAG_SYMLINK: u32 = 1 << 1;
pub const CELL_FLAG_PADDING_TOP: u32 = 1 << 2;

/// `0 = logical`, `1 = allocated`. Matches the picker on the
/// Swift side.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Metric {
    Logical,
    Allocated,
}

const MIN_PIXEL_AREA: f64 = 1.0;
const DIR_LABEL_HEIGHT: f64 = 14.0;
const MIN_DIR_LABEL_W: f64 = 48.0;
const PADDING_OUTER: f64 = 1.0;
const SQUARIFY_RATIO: f64 = 1.4;

/// Top-level entry point. Lays out the subtree rooted at
/// `root_idx` into a viewport of `(width, height)` and returns
/// the flattened cell list. `max_depth = 0` is the
/// no-truncation sentinel (matches the JS depth picker).
pub fn render_cells(
    tree: &Tree,
    root_idx: u32,
    max_depth: u32,
    metric: Metric,
    width: f64,
    height: f64,
) -> Vec<ApfsCell> {
    let mut cells: Vec<ApfsCell> = Vec::new();
    if width <= 0.0 || height <= 0.0 {
        return cells;
    }
    if (root_idx as usize) >= tree.nodes.len() {
        return cells;
    }
    let rect = Rect { x0: 0.0, y0: 0.0, x1: width, y1: height };
    layout_subtree(tree, root_idx, rect, 0, max_depth, metric, &mut cells);
    cells
}

#[derive(Copy, Clone)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}
impl Rect {
    fn area(&self) -> f64 {
        let w = (self.x1 - self.x0).max(0.0);
        let h = (self.y1 - self.y0).max(0.0);
        w * h
    }
}

fn metric_value(node: &crate::tree::TreeNode, metric: Metric) -> u64 {
    match metric {
        Metric::Logical => node.value_logical,
        Metric::Allocated => node.value_allocated.unwrap_or(0),
    }
}

/// Recursively lay out `node_idx` inside `rect` at `depth`. Emits
/// cells in pre-order (parents before children) so the canvas
/// paint order is correct — d3's `eachBefore` semantics.
fn layout_subtree(
    tree: &Tree,
    node_idx: u32,
    rect: Rect,
    depth: u32,
    max_depth: u32,
    metric: Metric,
    cells: &mut Vec<ApfsCell>,
) {
    if rect.area() < MIN_PIXEL_AREA {
        return;
    }
    let node = &tree.nodes[node_idx as usize];
    let is_dir = matches!(node.kind, EntryKind::Dir);
    let is_symlink = matches!(node.kind, EntryKind::Symlink);

    // We carve out paddingTop for the dir's label strip only when
    // the cell is big enough to host the label. Compute now so
    // we can both flag the cell and shrink the child rect.
    let mut child_rect = rect;
    // paddingOuter: shrink by 1 px on every side at depth > 0 so
    // sibling cells don't share a hairline edge. Root rect skips
    // this — the root fills the whole viewport.
    if depth > 0 {
        child_rect.x0 += PADDING_OUTER;
        child_rect.y0 += PADDING_OUTER;
        child_rect.x1 -= PADDING_OUTER;
        child_rect.y1 -= PADDING_OUTER;
    }
    let mut label_reserved = false;
    if depth > 0 && is_dir {
        let dx = child_rect.x1 - child_rect.x0;
        let dy = child_rect.y1 - child_rect.y0;
        if dx >= MIN_DIR_LABEL_W && dy >= DIR_LABEL_HEIGHT + 4.0 {
            child_rect.y0 += DIR_LABEL_HEIGHT;
            label_reserved = true;
        }
    }

    // Emit a cell for this node — but only at depth > 0. Depth-0
    // is the root, which has no visible cell (the breadcrumb is
    // its label).
    if depth > 0 {
        let mut flags: u32 = 0;
        if is_dir { flags |= CELL_FLAG_DIR; }
        if is_symlink { flags |= CELL_FLAG_SYMLINK; }
        if label_reserved { flags |= CELL_FLAG_PADDING_TOP; }
        cells.push(ApfsCell {
            x0: rect.x0 as f32,
            y0: rect.y0 as f32,
            x1: rect.x1 as f32,
            y1: rect.y1 as f32,
            depth,
            node_index: node_idx,
            flags,
            fill_rgb: compute_fill(node, metric),
        });
    }

    // Stop recursing on leaves or at the depth cap (max_depth = 0
    // disables the cap — it's the user-facing "unlimited" mode).
    if node.children.is_empty() {
        return;
    }
    if max_depth != 0 && depth >= max_depth {
        return;
    }
    if child_rect.area() < MIN_PIXEL_AREA {
        return;
    }

    // Build the squarify input: (node_index, value), sorted
    // descending. Zero-value children are skipped — squarify's
    // ratio math degenerates on zero.
    let mut items: Vec<(u32, f64)> = node
        .children
        .iter()
        .filter_map(|&ci| {
            let v = metric_value(&tree.nodes[ci as usize], metric);
            if v == 0 { None } else { Some((ci, v as f64)) }
        })
        .collect();
    if items.is_empty() {
        return;
    }
    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let total: f64 = items.iter().map(|(_, v)| v).sum();
    if total <= 0.0 {
        return;
    }

    // Run squarify, recurse into each child's allocated rect.
    let mut child_rects = vec![Rect { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0 }; items.len()];
    squarify(child_rect, &items, total, SQUARIFY_RATIO, &mut child_rects);
    for (i, &(child_idx, _)) in items.iter().enumerate() {
        layout_subtree(
            tree,
            child_idx,
            child_rects[i],
            depth + 1,
            max_depth,
            metric,
            cells,
        );
    }
}

/// Squarified treemap layout. Greedy row-packing on the shorter
/// side of the current rect; each row adds children until the
/// max aspect ratio would worsen, then lays the row out via
/// dice (horizontal) or slice (vertical) and recurses on the
/// remaining strip.
///
/// Ported from d3-hierarchy's `treemapSquarify`; same names and
/// the same `alpha = max(dy/dx, dx/dy) / (value * ratio)` /
/// `beta = sumValue² * alpha` aspect-ratio math.
fn squarify(
    initial_rect: Rect,
    items: &[(u32, f64)],
    total: f64,
    ratio: f64,
    out: &mut [Rect],
) {
    let mut rect = initial_rect;
    let mut value = total;
    let n = items.len();
    let mut i0 = 0;
    while i0 < n {
        let dx = rect.x1 - rect.x0;
        let dy = rect.y1 - rect.y0;
        if dx <= 0.0 || dy <= 0.0 {
            break;
        }
        // Find the first positive-value item from `i0`.
        let mut i1 = i0;
        let mut sum_value = items[i1].1;
        while sum_value <= 0.0 && i1 + 1 < n {
            i1 += 1;
            sum_value = items[i1].1;
        }
        if sum_value <= 0.0 {
            break;
        }
        let mut min_value = sum_value;
        let mut max_value = sum_value;
        // `alpha` and `beta` from the Bruls et al. paper.
        let alpha = (dy / dx).max(dx / dy) / (value * ratio);
        let mut beta = sum_value * sum_value * alpha;
        let mut min_ratio = (max_value / beta).max(beta / min_value);
        // Greedily grow the row while the aspect ratio improves.
        let mut i = i1 + 1;
        while i < n {
            let v = items[i].1;
            if v <= 0.0 {
                break;
            }
            sum_value += v;
            if v < min_value { min_value = v; }
            if v > max_value { max_value = v; }
            beta = sum_value * sum_value * alpha;
            let new_ratio = (max_value / beta).max(beta / min_value);
            if new_ratio > min_ratio {
                sum_value -= v;
                break;
            }
            min_ratio = new_ratio;
            i += 1;
        }
        let row_end = i; // exclusive

        // Lay out the row. `dice` = horizontal slice (the row
        // spans the full width of `rect`); otherwise we take a
        // vertical strip on the left and let it span the full
        // height.
        let dice = dx < dy;
        if dice {
            let row_y1 = if value > 0.0 {
                rect.y0 + dy * sum_value / value
            } else {
                rect.y1
            };
            let mut x = rect.x0;
            let k = if sum_value > 0.0 { dx / sum_value } else { 0.0 };
            for j in i0..row_end {
                let v = items[j].1;
                let next_x = if v > 0.0 { x + v * k } else { x };
                out[j] = Rect { x0: x, y0: rect.y0, x1: next_x, y1: row_y1 };
                x = next_x;
            }
            rect.y0 = row_y1;
        } else {
            let row_x1 = if value > 0.0 {
                rect.x0 + dx * sum_value / value
            } else {
                rect.x1
            };
            let mut y = rect.y0;
            let k = if sum_value > 0.0 { dy / sum_value } else { 0.0 };
            for j in i0..row_end {
                let v = items[j].1;
                let next_y = if v > 0.0 { y + v * k } else { y };
                out[j] = Rect { x0: rect.x0, y0: y, x1: row_x1, y1: next_y };
                y = next_y;
            }
            rect.x0 = row_x1;
        }
        value -= sum_value;
        i0 = row_end;
    }
}

/// Per-node fill colour. Mirrors the JS canvas renderer's
/// `leafColor`:
///   - Allocated metric + None on this row → muted grey (the
///     SR-019 / EX-22 unclaimed swatch).
///   - Symlink → blue-grey.
///   - Anything not-a-file → mid-grey.
///   - File → extension lookup, then hash-to-HSL for unknowns.
/// Dirs are drawn with a uniform dir-bg fill on the Swift side;
/// the per-cell `fill_rgb` is still computed so a future
/// per-dir colouring (e.g. by mount point) is a one-field
/// swap.
fn compute_fill(node: &crate::tree::TreeNode, metric: Metric) -> u32 {
    // SR-019 unclaimed (allocated metric, value_allocated None).
    if metric == Metric::Allocated && node.value_allocated.is_none() {
        return 0x3b3f4a;
    }
    match node.kind {
        EntryKind::Dir => 0x1e2330, // matches `dir-bg` rgba(30,35,45,0.55) blended on the body bg
        EntryKind::Symlink => 0x7d8a99,
        EntryKind::Other => 0x5b6779,
        EntryKind::File => fill_for_file(&node.name),
    }
}

fn fill_for_file(name: &str) -> u32 {
    let bytes = name.as_bytes();
    // Find the last '.'; everything before it must be non-empty.
    let mut dot: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate().rev() {
        if b == b'.' {
            if i == 0 || i == bytes.len() - 1 {
                break;
            }
            dot = Some(i);
            break;
        }
    }
    let ext = match dot {
        Some(idx) => &bytes[idx + 1..],
        None => return 0x9aa3b2,
    };
    if let Some(rgb) = palette_lookup(ext) {
        return rgb;
    }
    hash_hsl(ext)
}

/// Fixed extension palette. Subset of the JS `EXT_COLORS` —
/// extensions the user is most likely to see on a Mac. Anything
/// not in here falls through to `hash_hsl`.
fn palette_lookup(ext_bytes: &[u8]) -> Option<u32> {
    // Lowercase comparison, ASCII-only fast path (every
    // realistic file extension is ASCII).
    let mut lower = [0u8; 8];
    if ext_bytes.len() > lower.len() {
        return None;
    }
    for (i, &b) in ext_bytes.iter().enumerate() {
        lower[i] = if b.is_ascii_uppercase() { b + 32 } else { b };
    }
    let key = &lower[..ext_bytes.len()];
    match key {
        // text / code
        b"txt" | b"md" => Some(0xa0c4ff),
        b"rs" => Some(0xffc09f),
        b"py" => Some(0xffd6a5),
        b"js" | b"ts" | b"tsx" | b"jsx" => Some(0xffe066),
        b"json" => Some(0xf4d35e),
        b"html" => Some(0xff8fab),
        b"css" => Some(0xcaffbf),
        b"c" | b"cpp" | b"h" | b"hpp" => Some(0xbdb2ff),
        b"swift" => Some(0xfdb5a5),
        b"go" => Some(0x9bf6ff),
        b"rb" => Some(0xffb3c1),
        // images
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"webp" | b"heic" | b"svg" | b"icns" => Some(0x8ecae6),
        // av
        b"mp4" | b"mov" | b"mp3" | b"wav" | b"m4a" | b"flac" => Some(0xb388eb),
        // documents
        b"pdf" | b"doc" | b"docx" | b"pages" => Some(0xef476f),
        // archives / binaries
        b"zip" | b"tar" | b"gz" | b"bz2" | b"dmg" | b"iso" => Some(0xadb5bd),
        // app / system
        b"app" | b"framework" | b"dylib" | b"so" => Some(0xffafcc),
        _ => None,
    }
}

/// 64×64 axis-aligned spatial hash for sub-millisecond
/// hit-testing on a laid-out cell array. Each cell is inserted
/// into every bucket it overlaps; buckets are then sorted
/// depth-descending so the first containing match the
/// `hit_test` loop sees is always the deepest cell at that
/// point.
pub struct HitGrid {
    /// `HIT_GRID_N * HIT_GRID_N` buckets in row-major order,
    /// each holding indices into the cell array the grid was
    /// built against.
    buckets: Vec<Vec<u32>>,
    cell_w: f32,
    cell_h: f32,
}

const HIT_GRID_N: usize = 64;

impl HitGrid {
    /// Build a grid over `cells` covering the rect `(0, 0) →
    /// (width, height)`. Pass the same dimensions the layout
    /// was constructed with.
    pub fn build(cells: &[ApfsCell], width: f32, height: f32) -> Self {
        let cell_w = (width / HIT_GRID_N as f32).max(f32::EPSILON);
        let cell_h = (height / HIT_GRID_N as f32).max(f32::EPSILON);
        let mut buckets: Vec<Vec<u32>> = (0..(HIT_GRID_N * HIT_GRID_N))
            .map(|_| Vec::new())
            .collect();
        for (idx, c) in cells.iter().enumerate() {
            let gx0 = ((c.x0 / cell_w) as i32).max(0).min((HIT_GRID_N - 1) as i32);
            let gy0 = ((c.y0 / cell_h) as i32).max(0).min((HIT_GRID_N - 1) as i32);
            // Subtract epsilon from the high edge so a cell that
            // exactly aligns with a grid boundary doesn't fan
            // into the next bucket unnecessarily.
            let gx1 = (((c.x1 - 0.0001) / cell_w) as i32)
                .max(0)
                .min((HIT_GRID_N - 1) as i32);
            let gy1 = (((c.y1 - 0.0001) / cell_h) as i32)
                .max(0)
                .min((HIT_GRID_N - 1) as i32);
            for gy in gy0..=gy1 {
                let row = (gy as usize) * HIT_GRID_N;
                for gx in gx0..=gx1 {
                    buckets[row + gx as usize].push(idx as u32);
                }
            }
        }
        for bucket in &mut buckets {
            bucket.sort_by(|&a, &b| {
                cells[b as usize].depth.cmp(&cells[a as usize].depth)
            });
        }
        HitGrid { buckets, cell_w, cell_h }
    }

    /// Find the deepest cell containing `(x, y)`. Returns
    /// `None` if no cell does or `(x, y)` is outside the
    /// laid-out rect.
    pub fn hit_test(&self, x: f32, y: f32, cells: &[ApfsCell]) -> Option<u32> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let gx = ((x / self.cell_w) as usize).min(HIT_GRID_N - 1);
        let gy = ((y / self.cell_h) as usize).min(HIT_GRID_N - 1);
        let bucket = &self.buckets[gy * HIT_GRID_N + gx];
        for &idx in bucket {
            let c = &cells[idx as usize];
            if x >= c.x0 && x < c.x1 && y >= c.y0 && y < c.y1 {
                return Some(idx);
            }
        }
        None
    }
}

/// Opaque handle representing a treemap layout. Owns both the
/// laid-out cells and the spatial-hash index used for
/// hit-testing. Constructed via `apfs_layout_new`, freed via
/// `apfs_layout_free`. The FFI consumer (Swift) holds the
/// handle for as long as it needs to render or hit-test against
/// the layout.
pub struct ApfsLayout {
    pub(crate) cells: Box<[ApfsCell]>,
    pub(crate) hit_grid: HitGrid,
}

/// FNV-1a hash → HSL → RGB. Same shape as the JS `hashColor`
/// helper so an unknown extension renders in the same hue across
/// both renderers (until the standalone HTML viz is dropped in
/// phase 6).
fn hash_hsl(text: &[u8]) -> u32 {
    let mut h: u32 = 2166136261;
    for &b in text {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    let hue = (h % 360) as f64;
    hsl_to_rgb(hue, 0.45, 0.70)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> u32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let r = ((r1 + m) * 255.0).clamp(0.0, 255.0) as u32;
    let g = ((g1 + m) * 255.0).clamp(0.0, 255.0) as u32;
    let b = ((b1 + m) * 255.0).clamp(0.0, 255.0) as u32;
    (r << 16) | (g << 8) | b
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
    fn render_emits_cells_for_a_small_tree() {
        let entries = vec![
            entry("a", EntryKind::Dir, 0),
            entry("a/b.txt", EntryKind::File, 100),
            entry("a/c.txt", EntryKind::File, 50),
            entry("d.png", EntryKind::File, 200),
        ];
        let tree = Tree::build(&entries);
        let cells = render_cells(&tree, 0, 0, Metric::Logical, 1000.0, 1000.0);
        // root has 2 top-level children (`a` and `d.png`); `a`
        // has 2 of its own. Without depth limit we expect 4
        // visible cells. The root itself doesn't emit a cell
        // (depth 0).
        assert_eq!(cells.len(), 4, "expected 4 cells, got {}", cells.len());
        // Every emitted cell has positive area.
        for c in &cells {
            assert!(c.x1 > c.x0, "cell width must be positive");
            assert!(c.y1 > c.y0, "cell height must be positive");
        }
        // Cells emitted in pre-order: depth 1 before any depth 2
        // child of theirs.
        let depths: Vec<u32> = cells.iter().map(|c| c.depth).collect();
        // 3 depth-1 cells (a, d.png) + 2 depth-2 cells (b, c)
        // — but pre-order means [a, b.txt, c.txt, d.png] or
        // [d.png, a, b.txt, c.txt] depending on sort order.
        let max_depth = *depths.iter().max().unwrap();
        assert_eq!(max_depth, 2);
    }

    #[test]
    fn render_respects_depth_cap() {
        let entries = vec![
            entry("a", EntryKind::Dir, 0),
            entry("a/b", EntryKind::Dir, 0),
            entry("a/b/c.txt", EntryKind::File, 100),
        ];
        let tree = Tree::build(&entries);
        // max_depth = 1 keeps only top-level cells; `a` is the
        // sole top-level dir.
        let cells = render_cells(&tree, 0, 1, Metric::Logical, 1000.0, 1000.0);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].depth, 1);
        assert_eq!(cells[0].flags & CELL_FLAG_DIR, CELL_FLAG_DIR);
    }

    #[test]
    fn hit_grid_finds_deepest_cell_under_point() {
        let entries = vec![
            entry("a", EntryKind::Dir, 0),
            entry("a/b.txt", EntryKind::File, 100),
        ];
        let tree = Tree::build(&entries);
        let cells = render_cells(&tree, 0, 0, Metric::Logical, 1000.0, 1000.0);
        let grid = HitGrid::build(&cells, 1000.0, 1000.0);
        // Pick a point well inside `a` (the only top-level
        // dir). Hit-test should return the deeper `b.txt`
        // cell — not `a` — because the grid sorts each
        // bucket depth-descending.
        let b_idx = cells.iter().position(|c| c.depth == 2).unwrap() as u32;
        let b = cells[b_idx as usize];
        let x = (b.x0 + b.x1) * 0.5;
        let y = (b.y0 + b.y1) * 0.5;
        let hit = grid.hit_test(x, y, &cells);
        assert_eq!(hit, Some(b_idx));
    }
}
