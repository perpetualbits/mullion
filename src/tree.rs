// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Focus model, zoom stack, and layout-tree owner.
//!
//! [`Tree`] wraps a [`Node`] root plus interaction state:
//!
//! - **Focus** — which leaf tile is currently active (Phase 3).
//! - **Carousel scroll** — embedded in each [`Node::Carousel`] (Phase 4).
//! - **Zoom stack** — a `Vec<TileId>` that re-roots the *view* at a subtree
//!   without touching the underlying tree (Phase 5).  While zoomed, focus
//!   navigation stays inside the effective subtree; the full tree is intact.
//!
//! ## Effective root
//!
//! [`Tree::effective_root`] returns the node the app should solve and render:
//! the deepest zoom target, or the real root when not zoomed.  All focus
//! methods operate on this subtree.
//!
//! ## DFS order
//!
//! Leaf tiles are enumerated in **depth-first pre-order**, children visited in
//! declaration order.  This is the linear `Tab` / `Shift-Tab` traversal order.
//! Geometric/directional focus (`hjkl`) requires solved rects and is deferred
//! to a later phase; this module is restricted to id-based traversal and
//! structural edits (`flip`, `swap`).

use crate::border::LineWeight;
use crate::geometry::Rect;
use crate::layout::{Axis, Node, Orientation, TileId, partition, solve};

// ── Free functions ────────────────────────────────────────────────────────────

/// The `TileId` of a leaf node, or `None` for a container (`Split` or `Carousel`).
///
/// A `Carousel`'s own `id` field is not a focusable leaf id — address it via
/// [`node_by_id`] instead.
pub fn tile_id_of(node: &Node) -> Option<TileId> {
    match node {
        Node::Tile(id) => Some(*id),
        Node::Split { .. } => None,
        Node::Carousel { .. } => None, // container, not a focusable leaf
    }
}

/// Leaf ids in DFS pre-order (children visited in vector order).
///
/// This is the linear `Tab` traversal order.  The returned `Vec` is never
/// de-duplicated; if the caller assigns the same [`TileId`] to multiple leaves
/// those ids appear multiple times.
pub fn leaves(root: &Node) -> Vec<TileId> {
    let mut out = Vec::new();
    collect_leaves(root, &mut out);
    out
}

fn collect_leaves(node: &Node, out: &mut Vec<TileId>) {
    match node {
        Node::Tile(id) => out.push(*id),
        Node::Split { children, .. } => {
            for (_, child) in children {
                collect_leaves(child, out);
            }
        }
        // All carousel children are logical leaves regardless of scroll position.
        Node::Carousel { children, .. } => {
            for (_, child) in children {
                collect_leaves(child, out);
            }
        }
    }
}

/// The child-index path from `root` down to the leaf with `id`, or `None` if
/// no such leaf exists.
///
/// Each element is the index into that level's `children` vector.  For a
/// `Tile` root that matches `id` the path is the empty `vec![]`.
///
/// Used in tests; Phase 3b and Phase 4 reuse this for directional focus and zoom.
pub fn focus_path(root: &Node, id: TileId) -> Option<Vec<usize>> {
    let mut path = Vec::new();
    if find_path(root, id, &mut path) { Some(path) } else { None }
}

fn find_path(node: &Node, id: TileId, path: &mut Vec<usize>) -> bool {
    match node {
        Node::Tile(tid) => *tid == id,
        Node::Split { children, .. } => {
            for (i, (_, child)) in children.iter().enumerate() {
                path.push(i);
                if find_path(child, id, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
        // Carousel children are logical leaves; search all of them regardless of scroll.
        Node::Carousel { children, .. } => {
            for (i, (_, child)) in children.iter().enumerate() {
                path.push(i);
                if find_path(child, id, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
    }
}

// ── node_by_id ────────────────────────────────────────────────────────────────

/// Find a node by id — matches a `Tile(id)` **or** a `Carousel { id, .. }`.
///
/// Performs a depth-first pre-order search through the entire tree.  Returns a
/// shared reference to the first matching node, or `None` if `id` is not present.
///
/// Enables addressing a carousel to read its scroll offset or children without
/// it being a focusable leaf (Phase 4b will expose mutable scrolling).
pub fn node_by_id(root: &Node, id: TileId) -> Option<&Node> {
    match root {
        Node::Tile(tid) => {
            if *tid == id { Some(root) } else { None }
        }
        Node::Split { children, .. } => {
            children.iter().find_map(|(_, child)| node_by_id(child, id))
        }
        Node::Carousel { id: cid, children, .. } => {
            if *cid == id {
                return Some(root);
            }
            children.iter().find_map(|(_, child)| node_by_id(child, id))
        }
    }
}

/// Find a node by id — matches a `Tile(id)` **or** a `Carousel { id, .. }`.
///
/// Mutable variant of [`node_by_id`].  Returns a mutable reference to the
/// first matching node, enabling callers to update fields such as
/// `Carousel::scroll` (Phase 4b).
///
/// The implementation performs two sequential match passes on `root` to avoid
/// borrow-checker conflicts: a shared borrow to identify the target, then a
/// mutable borrow to return it or recurse.
pub fn node_by_id_mut(root: &mut Node, id: TileId) -> Option<&mut Node> {
    // Phase 1: check if this node itself is the target via a shared borrow.
    let is_target = match &*root {
        Node::Tile(tid) => *tid == id,
        Node::Carousel { id: cid, .. } => *cid == id,
        Node::Split { .. } => false,
    };
    if is_target {
        return Some(root);
    }
    // Phase 2: recurse into children with a mutable borrow.
    match root {
        Node::Tile(_) => None,
        Node::Split { children, .. } => {
            for (_, child) in children.iter_mut() {
                if let Some(found) = node_by_id_mut(child, id) {
                    return Some(found);
                }
            }
            None
        }
        Node::Carousel { children, .. } => {
            for (_, child) in children.iter_mut() {
                if let Some(found) = node_by_id_mut(child, id) {
                    return Some(found);
                }
            }
            None
        }
    }
}

// ── Dir ───────────────────────────────────────────────────────────────────────

/// Direction for a sibling swap within a [`Split`](Node::Split).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Swap with the next sibling (higher index, later in DFS order).
    Next,
    /// Swap with the previous sibling (lower index, earlier in DFS order).
    Prev,
}

// ── Direction ─────────────────────────────────────────────────────────────────

/// Spatial direction used by [`Tree::focus_dir`] and [`Tree::focus_dir_cross`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Towards the top of the screen.
    Up,
    /// Towards the bottom of the screen.
    Down,
    /// Towards the left edge of the screen.
    Left,
    /// Towards the right edge of the screen.
    Right,
}

// ── Direction helpers ─────────────────────────────────────────────────────────

/// Read the current axis without calling `resolve()` (which mutates
/// `Adaptive::last` and requires a viewport rect).  For `Adaptive` the
/// last-resolved axis is used; defaults to `Horizontal` before the first solve.
fn peek_axis(orientation: &Orientation) -> Axis {
    match orientation {
        Orientation::Horizontal => Axis::Horizontal,
        Orientation::Vertical   => Axis::Vertical,
        Orientation::Adaptive { last, .. } => last.unwrap_or(Axis::Horizontal),
    }
}

/// Walk `path` from `root` and return
/// `(carousel_id, axis, child_idx_in_carousel, n_children)` for the
/// **innermost** [`Carousel`](Node::Carousel) ancestor on the path.
///
/// `child_idx_in_carousel` is the index within that carousel's `children` vec
/// that is on the path toward the focused leaf.  Returns `None` when no
/// `Carousel` node lies on the path.
fn nearest_carousel_ancestor(root: &Node, path: &[usize]) -> Option<(TileId, Axis, usize, usize)> {
    let mut result = None;
    let mut cur = root;
    for &idx in path {
        match cur {
            Node::Carousel { id, orientation, children, .. } => {
                result = Some((*id, peek_axis(orientation), idx, children.len()));
                if idx < children.len() {
                    cur = &children[idx].1;
                } else {
                    break;
                }
            }
            Node::Split { children, .. } => {
                if idx < children.len() {
                    cur = &children[idx].1;
                } else {
                    break;
                }
            }
            Node::Tile(_) => break,
        }
    }
    result
}

/// Whether `candidate` lies strictly in `dir` from `focus`.
fn is_in_direction(candidate: &Rect, focus: &Rect, dir: Direction) -> bool {
    match dir {
        Direction::Right => candidate.x >= focus.right(),
        Direction::Left  => candidate.right() <= focus.x,
        Direction::Down  => candidate.y >= focus.bottom(),
        Direction::Up    => candidate.bottom() <= focus.y,
    }
}

/// Length of the overlap between `candidate` and `focus` on the axis
/// **perpendicular** to `dir`.
fn perp_overlap(candidate: &Rect, focus: &Rect, dir: Direction) -> u16 {
    match dir {
        Direction::Left | Direction::Right =>
            overlap_1d(candidate.y, candidate.bottom(), focus.y, focus.bottom()),
        Direction::Up | Direction::Down =>
            overlap_1d(candidate.x, candidate.right(), focus.x, focus.right()),
    }
}

/// Gap in cells between `candidate` and `focus` along the primary axis of
/// `dir`.  Zero when the rects are touching or overlapping.
fn gap_distance(candidate: &Rect, focus: &Rect, dir: Direction) -> u16 {
    match dir {
        Direction::Right => candidate.x.saturating_sub(focus.right()),
        Direction::Left  => focus.x.saturating_sub(candidate.right()),
        Direction::Down  => candidate.y.saturating_sub(focus.bottom()),
        Direction::Up    => focus.y.saturating_sub(candidate.bottom()),
    }
}

/// Length of the intersection of `[a_lo, a_hi)` and `[b_lo, b_hi)`.
/// Returns 0 for disjoint intervals.
fn overlap_1d(a_lo: u16, a_hi: u16, b_lo: u16, b_hi: u16) -> u16 {
    let lo = a_lo.max(b_lo);
    let hi = a_hi.min(b_hi);
    hi.saturating_sub(lo)
}

// ── Tree helpers ──────────────────────────────────────────────────────────────

/// Navigate `root` by following `path` (each element is a `children` index) and
/// return a mutable reference to the node at that position.
///
/// Works for both `Split` and `Carousel` nodes; the child tuple layout differs
/// (`(Constraint, Node)` vs `(u16, Node)`) but the child node is always `.1`.
fn node_at_path_mut<'a>(root: &'a mut Node, path: &[usize]) -> Option<&'a mut Node> {
    let mut cur = root;
    for &idx in path {
        match cur {
            Node::Split { children, .. } if idx < children.len() => {
                cur = &mut children[idx].1;
            }
            Node::Carousel { children, .. } if idx < children.len() => {
                cur = &mut children[idx].1;
            }
            _ => return None,
        }
    }
    Some(cur)
}

/// Walk `root` → leaf along `path`, adjusting `Carousel::scroll` at each
/// carousel level so the focused child is flush-visible within its viewport.
///
/// `rect` is the on-screen area that `node` occupies at this level, passed
/// down through splits and carousels the same way `solve_into` does, so that
/// nested carousels each receive the correct viewport size.
///
/// For `Split` nodes the axis is resolved (updating `Adaptive::last`) and
/// the child's rect is computed via [`partition`] — the same geometry as
/// `solve_into` but without border insets.  For `Carousel` nodes the scroll
/// is nudged the minimum amount to make the focused child flush-visible, then
/// the child's on-screen clipped rect is derived from the new scroll and
/// passed into the recursion.
fn reveal_focus_path(node: &mut Node, path: &[usize], rect: Rect) {
    if path.is_empty() {
        return;
    }
    let child_idx = path[0];
    let rest = &path[1..];
    match node {
        Node::Tile(_) => {}
        Node::Split { orientation, children } => {
            if child_idx >= children.len() { return; }
            let axis = orientation.resolve(rect);
            let total = match axis {
                Axis::Horizontal => rect.width,
                Axis::Vertical => rect.height,
            };
            let sizes = partition(children, total);
            // Advance to the focused child's main-axis origin.
            let mut pos = match axis {
                Axis::Horizontal => rect.x,
                Axis::Vertical => rect.y,
            };
            for i in 0..child_idx {
                pos = pos.saturating_add(sizes[i]);
            }
            let size = sizes[child_idx];
            let child_rect = match axis {
                Axis::Horizontal => Rect::new(pos, rect.y, size, rect.height),
                Axis::Vertical   => Rect::new(rect.x, pos, rect.width, size),
            };
            reveal_focus_path(&mut children[child_idx].1, rest, child_rect);
        }
        Node::Carousel { orientation, scroll, children, .. } => {
            if child_idx >= children.len() { return; }
            let axis = orientation.resolve(rect);
            let (main_extent, cross_extent) = match axis {
                Axis::Horizontal => (rect.width, rect.height),
                Axis::Vertical   => (rect.height, rect.width),
            };
            let vp_main_origin = match axis {
                Axis::Horizontal => rect.x,
                Axis::Vertical   => rect.y,
            };

            let child_start: u32 = children[..child_idx].iter().map(|(e, _)| *e as u32).sum();
            let ext = children[child_idx].0;
            let total: u32 = children.iter().map(|(e, _)| *e as u32).sum();
            let max_scroll = total.saturating_sub(main_extent as u32).min(u16::MAX as u32) as u16;

            // `rest` being empty means this carousel is the direct container of
            // the focused leaf.  The "too-tall tile reveals its top" rule only
            // applies at that level (a too-tall *container* child should instead
            // use the far-edge reveal so the descent into it reaches the right
            // inner position).
            let is_leaf_child = rest.is_empty();
            let is_too_tall   = (ext as u32) > (main_extent as u32);

            // Three cases checked in priority order:
            //
            //  1. Child's top is above the viewport → reveal near edge (always).
            //  2. Focused leaf is too-tall (doesn't fit) and its top is not yet
            //     at the viewport top → align its top.  Any other scroll would
            //     either show less of the tile or cut off its top.
            //  3. Anything else with its far edge past the viewport → reveal far
            //     edge.  Covers: normal leaf below viewport, container child of
            //     any size below viewport (including too-tall containers whose
            //     inner focused tile may be near the bottom of the container).
            let mut new = *scroll as u32;
            if child_start < new {
                new = child_start;                                    // case 1: pull up
            } else if is_leaf_child && is_too_tall && child_start > new {
                new = child_start;                                    // case 2: align top
            } else if !is_too_tall || !is_leaf_child {
                if child_start + ext as u32 > new + main_extent as u32 {
                    new = child_start + ext as u32 - main_extent as u32; // case 3: reveal far edge
                }
            }
            let new_scroll = (new.min(u16::MAX as u32) as u16).min(max_scroll);
            *scroll = new_scroll;

            // Derive the focused child's on-screen clipped rect after adjustment.
            let vis_start = child_start.max(new_scroll as u32);
            let vis_end = (child_start + ext as u32).min(new_scroll as u32 + main_extent as u32);
            if vis_start >= vis_end { return; }
            let vis_len = (vis_end - vis_start) as u16;
            let screen_start = vp_main_origin + (vis_start - new_scroll as u32) as u16;

            let child_rect = match axis {
                Axis::Horizontal => Rect::new(screen_start, rect.y, vis_len, cross_extent),
                Axis::Vertical   => Rect::new(rect.x, screen_start, cross_extent, vis_len),
            };
            reveal_focus_path(&mut children[child_idx].1, rest, child_rect);
        }
    }
}

// ── Tree ──────────────────────────────────────────────────────────────────────

/// Owns a layout tree plus interaction state.
///
/// The three state fields are:
/// - `root` — the full layout tree (never pruned by zoom).
/// - `focus` — the focused leaf tile id (or `None`).
/// - `zoom` — re-root stack, outermost to innermost; empty = full-tree view.
pub struct Tree {
    root: Node,
    focus: Option<TileId>,
    zoom: Vec<TileId>,
}

impl Tree {
    /// Wrap a root node.  Focus initialises to the first leaf in DFS order,
    /// or `None` if the tree has no `Tile` leaves.  Zoom starts empty.
    pub fn new(root: Node) -> Self {
        let focus = leaves(&root).into_iter().next();
        Self { root, focus, zoom: Vec::new() }
    }

    /// The root node.
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Mutable access for structural edits.
    ///
    /// Always the real root regardless of zoom.  After editing the tree call
    /// [`ensure_focus_valid`](Tree::ensure_focus_valid) (and
    /// [`ensure_zoom_valid`](Tree::ensure_zoom_valid) if the edit may have
    /// removed zoomed-into nodes).  For rendering use
    /// [`effective_root_mut`](Tree::effective_root_mut) instead.
    pub fn root_mut(&mut self) -> &mut Node {
        &mut self.root
    }

    /// The node the app should solve and render.
    ///
    /// Returns the deepest zoom target, or the real root when not zoomed.
    /// If the zoom stack contains a stale id that no longer resolves, falls
    /// back to the real root for this call; call
    /// [`ensure_zoom_valid`](Tree::ensure_zoom_valid) to clean up the stack.
    pub fn effective_root(&self) -> &Node {
        match self.zoom.last().copied() {
            None => &self.root,
            Some(id) => node_by_id(&self.root, id).unwrap_or(&self.root),
        }
    }

    /// Mutable version of [`effective_root`](Tree::effective_root) for passing
    /// to `solve` / `render_carousel`.
    pub fn effective_root_mut(&mut self) -> &mut Node {
        let id = match self.zoom.last().copied() {
            None => return &mut self.root,
            Some(id) => id,
        };
        // Two-phase borrow: shared check first so the mutable borrow can follow.
        if node_by_id(&self.root, id).is_some() {
            node_by_id_mut(&mut self.root, id).unwrap()
        } else {
            &mut self.root
        }
    }

    /// The currently focused tile id, or `None` if the tree has no leaves.
    pub fn focus(&self) -> Option<TileId> {
        self.focus
    }

    /// Focus a specific leaf.  Returns `false` (and leaves focus unchanged) if
    /// `id` is not a `Tile` leaf within the **effective subtree**.
    ///
    /// While zoomed, leaves outside the zoom target are rejected — use
    /// [`zoom_reset`](Tree::zoom_reset) first to escape the zoom and then set
    /// focus on the full tree.
    pub fn focus_set(&mut self, id: TileId) -> bool {
        if leaves(self.effective_root()).contains(&id) {
            self.focus = Some(id);
            true
        } else {
            false
        }
    }

    /// Move focus to the next leaf in DFS order within the **effective
    /// subtree**, wrapping at the end.
    ///
    /// If focus is `None` but leaves exist, selects the first leaf.
    /// No-op if the effective subtree has no leaves.
    pub fn focus_next(&mut self) {
        let ls = leaves(self.effective_root());
        if ls.is_empty() {
            return;
        }
        self.focus = Some(match self.focus {
            None => ls[0],
            Some(cur) => match ls.iter().position(|&id| id == cur) {
                None => ls[0],
                Some(i) => ls[(i + 1) % ls.len()],
            },
        });
    }

    /// Move focus to the previous leaf in DFS order within the **effective
    /// subtree**, wrapping at the start.
    ///
    /// If focus is `None` but leaves exist, selects the last leaf.
    /// No-op if the effective subtree has no leaves.
    pub fn focus_prev(&mut self) {
        let ls = leaves(self.effective_root());
        if ls.is_empty() {
            return;
        }
        self.focus = Some(match self.focus {
            None => *ls.last().unwrap(),
            Some(cur) => match ls.iter().position(|&id| id == cur) {
                None => *ls.last().unwrap(),
                Some(i) => ls[(i + ls.len() - 1) % ls.len()],
            },
        });
    }

    /// Move focus to the first leaf of the **effective subtree**.
    ///
    /// No-op if the effective subtree has no leaves.
    pub fn focus_first(&mut self) {
        self.focus = leaves(self.effective_root()).into_iter().next();
    }

    /// Move focus to the last leaf of the **effective subtree**.
    ///
    /// No-op if the effective subtree has no leaves.
    pub fn focus_last(&mut self) {
        self.focus = leaves(self.effective_root()).into_iter().last();
    }

    /// Re-validate focus after a structural edit.
    ///
    /// If focus is `None` or points to an id no longer present in the
    /// **effective subtree**, reset it to that subtree's first leaf (or `None`
    /// if it has no leaves).  A focus id that still exists is left untouched.
    ///
    /// Pair with [`ensure_zoom_valid`](Tree::ensure_zoom_valid) when the edit
    /// may have removed nodes that the zoom stack points into.
    pub fn ensure_focus_valid(&mut self) {
        let ls = leaves(self.effective_root());
        match self.focus {
            Some(id) if ls.contains(&id) => {}
            _ => self.focus = ls.into_iter().next(),
        }
    }

    // ── Zoom ──────────────────────────────────────────────────────────────

    /// Whether any zoom level is active.
    pub fn is_zoomed(&self) -> bool {
        !self.zoom.is_empty()
    }

    /// Number of active zoom levels (0 = not zoomed).
    pub fn zoom_depth(&self) -> usize {
        self.zoom.len()
    }

    /// Re-root the view at the addressable node `id` (a `Tile` or `Carousel`).
    ///
    /// `id` must be reachable within the **current effective subtree** — you can
    /// only zoom into something in view.  Returns `false` (no change) if `id` is
    /// absent from the effective subtree or is already the effective root.
    ///
    /// On success the zoom stack is pushed.  If the current focus is not a leaf
    /// of the new effective subtree, focus moves to that subtree's first leaf.
    ///
    /// ## Zooming into a `Split`
    ///
    /// `Split` nodes carry no id, so they cannot be addressed here.  To zoom
    /// into a split grouping, wrap it in a `Carousel` or assign ids via a
    /// future mechanism (noted for a later phase).
    pub fn zoom_to(&mut self, id: TileId) -> bool {
        // Reject if id is already the effective root.
        let is_current_root = match self.effective_root() {
            Node::Tile(tid)             => *tid == id,
            Node::Carousel { id: cid, .. } => *cid == id,
            Node::Split { .. }          => false,
        };
        if is_current_root { return false; }

        // Reject if id is not reachable from the current effective subtree.
        if node_by_id(self.effective_root(), id).is_none() { return false; }

        self.zoom.push(id);

        // Move focus into the new subtree when it fell outside.
        let focus_valid = self.focus
            .map_or(false, |fid| leaves(self.effective_root()).contains(&fid));
        if !focus_valid {
            self.focus = leaves(self.effective_root()).into_iter().next();
        }

        true
    }

    /// Zoom into the currently focused leaf (tmux-style fullscreen).
    ///
    /// Equivalent to `zoom_to(self.focus().unwrap())`.  No-op when there is no
    /// focus or the focus is already the effective root.
    pub fn zoom_focus(&mut self) -> bool {
        match self.focus {
            Some(id) => self.zoom_to(id),
            None => false,
        }
    }

    /// Pop one zoom level.  No-op when not zoomed.
    ///
    /// Focus is left unchanged; the previously zoomed-out-of subtree still
    /// contains the same focus id, so focus remains valid within the wider view.
    pub fn zoom_out(&mut self) {
        self.zoom.pop();
    }

    /// Pop all zoom levels, returning to the real root.
    pub fn zoom_reset(&mut self) {
        self.zoom.clear();
    }

    /// Re-validate the zoom stack after a structural edit.
    ///
    /// Walks outermost → innermost; truncates the stack at the first id that no
    /// longer resolves within its parent context.  A pruned subtree drops its
    /// entire inner chain.  Pair with [`ensure_focus_valid`](Tree::ensure_focus_valid).
    pub fn ensure_zoom_valid(&mut self) {
        let valid_len = {
            let mut current: &Node = &self.root;
            let mut len = 0usize;
            for &id in &self.zoom {
                match node_by_id(current, id) {
                    Some(n) => { current = n; len += 1; }
                    None    => break,
                }
            }
            len
        }; // shared borrow of self.root ends here
        self.zoom.truncate(valid_len);
    }

    /// Flip the orientation of the [`Split`](Node::Split) that is the focused
    /// leaf's parent.
    ///
    /// Always produces an explicit `Horizontal` or `Vertical` (disabling any
    /// `Adaptive` variant).  For an `Adaptive` parent, "current" is its
    /// `last`-resolved axis, defaulting to `Horizontal` when no solve has run
    /// yet.  The new orientation is the opposite of that effective axis.
    ///
    /// No-op if there is no focus or the focused leaf is the root (no parent).
    pub fn flip_focused_parent(&mut self) {
        let id = match self.focus { Some(id) => id, None => return };
        let path = match focus_path(&self.root, id) {
            Some(p) if !p.is_empty() => p,
            _ => return,
        };
        // `path` is an owned Vec; the immutable borrow of self.root is released.
        let parent = match node_at_path_mut(&mut self.root, &path[..path.len() - 1]) {
            Some(n) => n,
            None => return,
        };
        if let Node::Split { orientation, .. } = parent {
            *orientation = match orientation {
                Orientation::Horizontal => Orientation::Vertical,
                Orientation::Vertical => Orientation::Horizontal,
                Orientation::Adaptive { last, .. } => match last {
                    Some(Axis::Vertical) => Orientation::Horizontal,
                    _ => Orientation::Vertical,
                },
            };
        }
    }

    /// Swap the focused leaf with its next or previous sibling.
    ///
    /// Focus stays on the same [`TileId`] — the tile moves, the id does not
    /// change.  No-op at the boundary (no sibling in that direction), if the
    /// focused leaf is the root, or if there is no focus.
    pub fn swap_focused(&mut self, dir: Dir) {
        let id = match self.focus { Some(id) => id, None => return };
        let path = match focus_path(&self.root, id) {
            Some(p) if !p.is_empty() => p,
            _ => return,
        };
        let child_idx = path[path.len() - 1];
        let parent = match node_at_path_mut(&mut self.root, &path[..path.len() - 1]) {
            Some(n) => n,
            None => return,
        };
        if let Node::Split { children, .. } = parent {
            match dir {
                Dir::Next if child_idx + 1 < children.len() => {
                    children.swap(child_idx, child_idx + 1);
                }
                Dir::Prev if child_idx > 0 => {
                    children.swap(child_idx - 1, child_idx);
                }
                _ => {}
            }
        }
    }

    /// Carousel-scoped directional focus.
    ///
    /// Finds the **nearest enclosing [`Carousel`](Node::Carousel)** in the path
    /// from the effective root to the focused leaf and advances or retreats focus
    /// by one sibling inside it, **wrapping** at both ends.
    ///
    /// The step direction is derived from the carousel's orientation:
    /// - `Horizontal` carousel: `Left` → previous, `Right` → next.
    /// - `Vertical` carousel: `Up` → previous, `Down` → next.
    /// - A direction that crosses the carousel's axis is a no-op.
    ///
    /// No-op when there is no focus, no `Carousel` ancestor exists within the
    /// effective subtree, or `dir` does not align with the carousel's axis.
    pub fn focus_dir(&mut self, dir: Direction) {
        let focus_id = match self.focus { Some(id) => id, None => return };

        let path = {
            let root = self.effective_root();
            match focus_path(root, focus_id) {
                Some(p) => p,
                None => return,
            }
        };

        let (carousel_id, axis, child_idx, n_children) = {
            let root = self.effective_root();
            match nearest_carousel_ancestor(root, &path) {
                Some(info) => info,
                None => return,
            }
        };

        let step: i64 = match (axis, dir) {
            (Axis::Vertical, Direction::Down) | (Axis::Horizontal, Direction::Right) =>  1,
            (Axis::Vertical, Direction::Up)   | (Axis::Horizontal, Direction::Left)  => -1,
            _ => return,
        };

        let new_idx = (child_idx as i64 + step).rem_euclid(n_children as i64) as usize;

        let first_leaf = match node_by_id(&self.root, carousel_id) {
            Some(Node::Carousel { children, .. }) => {
                leaves(&children[new_idx].1).into_iter().next()
            }
            _ => return,
        };
        self.focus = first_leaf;
    }

    /// Geometric cross-boundary focus.
    ///
    /// Solves the **effective subtree** within `area` to obtain on-screen rects
    /// for all currently visible leaves, then moves focus to the best candidate
    /// that lies strictly in `dir` from the focused tile's rect.
    ///
    /// The best candidate is chosen by:
    /// 1. Maximum perpendicular-axis overlap with the focused rect (alignment).
    /// 2. Minimum gap along the primary axis (proximity tie-break).
    /// 3. Lowest [`TileId`] (stable final tie-break).
    ///
    /// Unlike [`Tree::focus_dir`] this method **never wraps** and only considers tiles
    /// visible in the effective subtree (zoom-aware).  No-op when there is no
    /// focus, the focused tile is not visible, or no tile lies in `dir`.
    pub fn focus_dir_cross(&mut self, dir: Direction, area: Rect) {
        let focus_id = match self.focus { Some(id) => id, None => return };
        let rects = solve(self.effective_root_mut(), area);

        let focus_rect = match rects.iter().find(|(id, _)| *id == focus_id) {
            Some((_, r)) => *r,
            None => return,
        };

        let best = rects.iter()
            .filter(|(id, r)| *id != focus_id && is_in_direction(r, &focus_rect, dir))
            .max_by_key(|(id, r)| {
                let overlap = perp_overlap(r, &focus_rect, dir);
                let gap     = gap_distance(r, &focus_rect, dir);
                (overlap, std::cmp::Reverse(gap), std::cmp::Reverse(*id))
            })
            .map(|(id, _)| *id);

        if let Some(new_id) = best {
            self.focus = Some(new_id);
        }
    }

    /// Add `delta` to the scroll offset of the carousel with the given `id`.
    ///
    /// Saturates at 0 on the low end; the upper bound is clamped at render /
    /// solve time against the actual viewport, so this method need not know it.
    /// No-op if `id` does not identify a `Carousel` in this tree.
    pub fn scroll_by(&mut self, id: TileId, delta: i32) {
        if let Some(Node::Carousel { scroll, .. }) = node_by_id_mut(&mut self.root, id) {
            if delta >= 0 {
                *scroll = scroll.saturating_add(delta as u16);
            } else {
                *scroll = scroll.saturating_sub((-delta) as u16);
            }
        }
    }

    /// Set the scroll offset of the carousel with the given `id`.
    ///
    /// Clamps to 0 on the low end; the upper bound is clamped at render /
    /// solve time.  No-op if `id` does not identify a `Carousel`.
    pub fn scroll_to(&mut self, id: TileId, offset: u16) {
        if let Some(Node::Carousel { scroll, .. }) = node_by_id_mut(&mut self.root, id) {
            *scroll = offset;
        }
    }

    /// Adjust the scroll of every `Carousel` on the path to the focused leaf
    /// so the focused tile is fully within each carousel's viewport.
    ///
    /// Reveals the minimum needed: flush to the near edge when the tile is
    /// before the window, flush to the far edge when after, untouched when
    /// already wholly visible.  A tile taller than the viewport has its **top**
    /// edge revealed (scroll = child_start) rather than its bottom, to avoid
    /// thrashing.  Nested carousels are adjusted outer-first so each inner
    /// carousel receives the correct viewport rect.
    ///
    /// No-op when there is no focus or when no carousel sits on the focus path.
    ///
    /// ## Geometry assumption
    ///
    /// Carousel viewports are computed with `solve`-style layout (no border
    /// inset).  If the layout skeleton is rendered with
    /// [`render_shared`](crate::border::render_shared) (which deflates by one
    /// cell for shared borders), the revealed position may be off by ~1 cell at
    /// carousel boundaries — an acceptable slack for most use cases.  Pass the
    /// un-inset `area` (same rect as `solve`) to avoid this discrepancy.
    ///
    /// ## Call site
    ///
    /// Call once per frame after a focus change and before rendering, passing
    /// the same `area` that will be passed to `solve` / `render_carousel`.
    /// [`InputRouter`](crate::input::InputRouter) does **not** call this — it
    /// has no viewport.
    pub fn scroll_focus_into_view(&mut self, area: Rect) {
        let focus_id = match self.focus { Some(id) => id, None => return };
        let path = match focus_path(&self.root, focus_id) {
            Some(p) => p,
            None => return,
        };
        reveal_focus_path(&mut self.root, &path, area);
    }
}

/// Build the `overrides` slice for [`render_shared`](crate::border::render_shared)
/// that highlights the focused tile in weight `w`.
///
/// Returns an empty vec when `tree.focus()` is `None`.
pub fn focus_override(tree: &Tree, w: LineWeight) -> Vec<(TileId, LineWeight)> {
    match tree.focus() {
        Some(id) => vec![(id, w)],
        None => vec![],
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::layout::{Constraint, Orientation, Size};

    fn tile(id: TileId) -> Node {
        Node::Tile(id)
    }

    fn h_split(kids: Vec<Node>) -> Node {
        Node::Split {
            orientation: Orientation::Horizontal,
            children: kids.into_iter()
                .map(|n| (Constraint::new(Size::Fill(1)), n))
                .collect(),
        }
    }

    fn v_split(kids: Vec<Node>) -> Node {
        Node::Split {
            orientation: Orientation::Vertical,
            children: kids.into_iter()
                .map(|n| (Constraint::new(Size::Fill(1)), n))
                .collect(),
        }
    }

    /// H-split of [Tile(0), V-split[Tile(1), Tile(2)], Tile(3)].
    /// DFS pre-order: 0, 1, 2, 3.
    fn sample_tree() -> Node {
        h_split(vec![tile(0), v_split(vec![tile(1), tile(2)]), tile(3)])
    }

    fn carousel(id: TileId, kids: Vec<Node>) -> Node {
        Node::Carousel {
            id,
            orientation: Orientation::Horizontal,
            scroll: 0,
            children: kids.into_iter().map(|n| (10u16, n)).collect(),
        }
    }

    // ── tile_id_of ────────────────────────────────────────────────────────

    #[test]
    fn tile_id_of_leaf() {
        assert_eq!(tile_id_of(&Node::Tile(7)), Some(7));
    }

    #[test]
    fn tile_id_of_split_is_none() {
        assert_eq!(tile_id_of(&h_split(vec![])), None);
    }

    #[test]
    fn tile_id_of_carousel_is_none() {
        // Carousel has its own id but is a container, not a focusable leaf.
        assert_eq!(tile_id_of(&carousel(42, vec![])), None);
    }

    // ── Carousel leaves ───────────────────────────────────────────────────

    #[test]
    fn carousel_leaves_includes_all_children() {
        // All 5 tiles are logical leaves regardless of which would be on-screen.
        let node = carousel(99, (0u64..5).map(Node::Tile).collect());
        assert_eq!(leaves(&node), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn carousel_leaves_nested_in_split() {
        // H-split[Tile(0), Carousel(id=99)[Tile(1), Tile(2)]]
        let root = h_split(vec![tile(0), carousel(99, vec![tile(1), tile(2)])]);
        assert_eq!(leaves(&root), vec![0, 1, 2]);
    }

    // ── node_by_id ────────────────────────────────────────────────────────

    #[test]
    fn node_by_id_finds_carousel_by_its_own_id() {
        let root = carousel(10, vec![tile(1), tile(2)]);
        let found = node_by_id(&root, 10).unwrap();
        assert!(matches!(found, Node::Carousel { id: 10, .. }));
    }

    #[test]
    fn node_by_id_finds_tile_inside_carousel() {
        let root = carousel(10, vec![tile(1), tile(2)]);
        assert!(matches!(node_by_id(&root, 1).unwrap(), Node::Tile(1)));
        assert!(matches!(node_by_id(&root, 2).unwrap(), Node::Tile(2)));
    }

    #[test]
    fn node_by_id_returns_none_for_missing() {
        let root = carousel(10, vec![tile(1)]);
        assert!(node_by_id(&root, 999).is_none());
    }

    #[test]
    fn node_by_id_finds_tile_in_split() {
        let root = h_split(vec![tile(5), tile(6)]);
        assert!(matches!(node_by_id(&root, 5).unwrap(), Node::Tile(5)));
        assert!(matches!(node_by_id(&root, 6).unwrap(), Node::Tile(6)));
        assert!(node_by_id(&root, 99).is_none());
    }

    #[test]
    fn node_by_id_mut_modifies_carousel_scroll() {
        let mut root = carousel(10, vec![tile(1)]);
        if let Some(Node::Carousel { scroll, .. }) = node_by_id_mut(&mut root, 10) {
            *scroll = 42;
        }
        assert!(matches!(&root, Node::Carousel { scroll: 42, .. }));
    }

    #[test]
    fn node_by_id_mut_returns_none_for_missing() {
        let mut root = carousel(10, vec![tile(1)]);
        assert!(node_by_id_mut(&mut root, 999).is_none());
    }

    // ── leaves ────────────────────────────────────────────────────────────

    #[test]
    fn leaves_dfs_order() {
        assert_eq!(leaves(&sample_tree()), vec![0, 1, 2, 3]);
    }

    #[test]
    fn leaves_single_tile() {
        assert_eq!(leaves(&Node::Tile(42)), vec![42]);
    }

    #[test]
    fn leaves_empty_split() {
        assert!(leaves(&h_split(vec![])).is_empty());
    }

    // ── focus_path ────────────────────────────────────────────────────────

    #[test]
    fn focus_path_root_tile() {
        assert_eq!(focus_path(&Node::Tile(5), 5), Some(vec![]));
    }

    #[test]
    fn focus_path_missing_id() {
        assert_eq!(focus_path(&sample_tree(), 99), None);
    }

    #[test]
    fn focus_path_nested_leaves() {
        // H-split[0→Tile(0), 1→V-split[0→Tile(1), 1→Tile(2)], 2→Tile(3)]
        assert_eq!(focus_path(&sample_tree(), 0), Some(vec![0]));
        assert_eq!(focus_path(&sample_tree(), 1), Some(vec![1, 0]));
        assert_eq!(focus_path(&sample_tree(), 2), Some(vec![1, 1]));
        assert_eq!(focus_path(&sample_tree(), 3), Some(vec![2]));
    }

    // ── Tree::new ─────────────────────────────────────────────────────────

    #[test]
    fn tree_new_focuses_first_leaf() {
        let tree = Tree::new(sample_tree());
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn tree_new_empty_focus_none() {
        let tree = Tree::new(h_split(vec![]));
        assert_eq!(tree.focus(), None);
    }

    // ── focus_set ─────────────────────────────────────────────────────────

    #[test]
    fn focus_set_real_leaf() {
        let mut tree = Tree::new(sample_tree());
        assert!(tree.focus_set(2));
        assert_eq!(tree.focus(), Some(2));
    }

    #[test]
    fn focus_set_missing_id_is_noop() {
        let mut tree = Tree::new(sample_tree());
        let before = tree.focus();
        assert!(!tree.focus_set(99));
        assert_eq!(tree.focus(), before);
    }

    // ── focus_next / focus_prev ───────────────────────────────────────────

    #[test]
    fn focus_next_cycles_forward_and_wraps() {
        let mut tree = Tree::new(sample_tree()); // starts at 0
        tree.focus_next(); assert_eq!(tree.focus(), Some(1));
        tree.focus_next(); assert_eq!(tree.focus(), Some(2));
        tree.focus_next(); assert_eq!(tree.focus(), Some(3));
        tree.focus_next(); assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn focus_prev_cycles_backward_and_wraps() {
        let mut tree = Tree::new(sample_tree()); // starts at 0
        tree.focus_prev(); assert_eq!(tree.focus(), Some(3)); // wraps
        tree.focus_prev(); assert_eq!(tree.focus(), Some(2));
        tree.focus_prev(); assert_eq!(tree.focus(), Some(1));
        tree.focus_prev(); assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn focus_next_from_none_picks_first() {
        let mut tree = Tree::new(sample_tree());
        tree.focus = None;
        tree.focus_next();
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn focus_prev_from_none_picks_last() {
        let mut tree = Tree::new(sample_tree());
        tree.focus = None;
        tree.focus_prev();
        assert_eq!(tree.focus(), Some(3));
    }

    #[test]
    fn focus_next_noop_on_empty_tree() {
        let mut tree = Tree::new(h_split(vec![]));
        tree.focus_next();
        assert_eq!(tree.focus(), None);
    }

    #[test]
    fn focus_prev_noop_on_empty_tree() {
        let mut tree = Tree::new(h_split(vec![]));
        tree.focus_prev();
        assert_eq!(tree.focus(), None);
    }

    // ── focus_first / focus_last ──────────────────────────────────────────

    #[test]
    fn focus_first_and_last() {
        let mut tree = Tree::new(sample_tree());
        tree.focus_set(2);
        tree.focus_first();
        assert_eq!(tree.focus(), Some(0));
        tree.focus_last();
        assert_eq!(tree.focus(), Some(3));
    }

    // ── ensure_focus_valid ────────────────────────────────────────────────

    #[test]
    fn ensure_focus_valid_resets_stale_focus() {
        let mut tree = Tree::new(sample_tree());
        tree.focus_set(2);
        // Replace tree with one that omits tile 2.
        *tree.root_mut() = h_split(vec![tile(0), tile(1), tile(3)]);
        tree.ensure_focus_valid();
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn ensure_focus_valid_preserves_present_focus() {
        let mut tree = Tree::new(sample_tree());
        tree.focus_set(1);
        // Add more leaves; tile 1 is still present.
        *tree.root_mut() = h_split(vec![tile(0), tile(1), tile(2), tile(3), tile(4)]);
        tree.ensure_focus_valid();
        assert_eq!(tree.focus(), Some(1));
    }

    #[test]
    fn ensure_focus_valid_empty_tree_gives_none() {
        let mut tree = Tree::new(sample_tree());
        *tree.root_mut() = h_split(vec![]);
        tree.ensure_focus_valid();
        assert_eq!(tree.focus(), None);
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    /// Generate a bounded random layout tree with leaf ids from 0..16.
    fn arb_node() -> impl Strategy<Value = Node> {
        let leaf = (0u64..16).prop_map(Node::Tile);
        leaf.prop_recursive(
            4,  // max depth
            32, // desired total size
            4,  // expected branch count
            |inner| {
                prop::collection::vec(inner, 2..=4usize).prop_map(|kids| Node::Split {
                    orientation: Orientation::Horizontal,
                    children: kids
                        .into_iter()
                        .map(|n| (Constraint::new(Size::Fill(1)), n))
                        .collect(),
                })
            },
        )
    }

    // ── flip_focused_parent ───────────────────────────────────────────────

    #[test]
    fn flip_h_to_v() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.flip_focused_parent();
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Vertical, .. }));
    }

    #[test]
    fn flip_v_to_h() {
        let mut tree = Tree::new(v_split(vec![tile(0), tile(1)]));
        tree.flip_focused_parent();
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Horizontal, .. }));
    }

    #[test]
    fn flip_twice_roundtrips() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.flip_focused_parent();
        tree.flip_focused_parent();
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Horizontal, .. }));
    }

    #[test]
    fn flip_adaptive_none_last_becomes_vertical() {
        let root = Node::Split {
            orientation: Orientation::Adaptive { margin_pct: 10, last: None },
            children: vec![
                (Constraint::new(Size::Fill(1)), tile(0)),
                (Constraint::new(Size::Fill(1)), tile(1)),
            ],
        };
        let mut tree = Tree::new(root);
        tree.flip_focused_parent();
        // None defaults to Horizontal → flip produces Vertical
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Vertical, .. }));
    }

    #[test]
    fn flip_adaptive_last_vertical_becomes_horizontal() {
        let root = Node::Split {
            orientation: Orientation::Adaptive { margin_pct: 10, last: Some(Axis::Vertical) },
            children: vec![
                (Constraint::new(Size::Fill(1)), tile(0)),
                (Constraint::new(Size::Fill(1)), tile(1)),
            ],
        };
        let mut tree = Tree::new(root);
        tree.flip_focused_parent();
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Horizontal, .. }));
    }

    #[test]
    fn flip_noop_at_root_tile() {
        let mut tree = Tree::new(tile(42));
        tree.flip_focused_parent(); // no parent → no-op
        assert_eq!(tree.focus(), Some(42));
    }

    #[test]
    fn flip_inner_parent_not_outer() {
        // H-split[Tile(0), V-split[Tile(1), Tile(2)], Tile(3)], focus=1
        let mut tree = Tree::new(sample_tree());
        tree.focus_set(1);
        tree.flip_focused_parent(); // flips the V-split parent of tile(1)
        // Root is still H-split; inner parent of tile(1) is now H-split
        assert!(matches!(tree.root(), Node::Split { orientation: Orientation::Horizontal, .. }));
        if let Node::Split { children, .. } = tree.root() {
            // child at idx 1 is the former V-split, now H-split
            assert!(matches!(&children[1].1, Node::Split { orientation: Orientation::Horizontal, .. }));
        }
    }

    // ── swap_focused ──────────────────────────────────────────────────────

    #[test]
    fn swap_next_moves_focused_right() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1), tile(2)]));
        // focus=0 at idx=0
        tree.swap_focused(Dir::Next);
        assert_eq!(leaves(tree.root()), vec![1, 0, 2]);
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn swap_prev_moves_focused_left() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1), tile(2)]));
        tree.focus_set(1); // idx=1
        tree.swap_focused(Dir::Prev);
        assert_eq!(leaves(tree.root()), vec![1, 0, 2]);
        assert_eq!(tree.focus(), Some(1));
    }

    #[test]
    fn swap_next_noop_at_last() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1), tile(2)]));
        tree.focus_set(2);
        let before = leaves(tree.root());
        tree.swap_focused(Dir::Next);
        assert_eq!(leaves(tree.root()), before);
    }

    #[test]
    fn swap_prev_noop_at_first() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1), tile(2)]));
        // focus=0 at idx=0; no previous sibling
        let before = leaves(tree.root());
        tree.swap_focused(Dir::Prev);
        assert_eq!(leaves(tree.root()), before);
    }

    #[test]
    fn swap_noop_at_root_tile() {
        let mut tree = Tree::new(tile(42));
        tree.swap_focused(Dir::Next);
        assert_eq!(tree.focus(), Some(42));
    }

    proptest! {
        /// `leaves(root).len()` equals the number of `Tile` nodes in the tree.
        #[test]
        fn prop_leaves_count_equals_tile_count(root in arb_node()) {
            fn tile_count(n: &Node) -> usize {
                match n {
                    Node::Tile(_) => 1,
                    Node::Split { children, .. } =>
                        children.iter().map(|(_, c)| tile_count(c)).sum(),
                    Node::Carousel { children, .. } =>
                        children.iter().map(|(_, c)| tile_count(c)).sum(),
                }
            }
            prop_assert_eq!(leaves(&root).len(), tile_count(&root));
        }

        /// `focus_next` applied exactly `leaves.len()` times from any valid
        /// starting position returns to the starting leaf (full cycle).
        ///
        /// This property requires distinct leaf ids; with duplicates the position-
        /// based cycle is ambiguous.
        #[test]
        fn prop_focus_next_full_cycle(root in arb_node()) {
            let ls = leaves(&root);
            prop_assume!(!ls.is_empty());
            // Skip trees with duplicate TileIds — the cycle property is only
            // meaningful when every leaf is unambiguously identifiable.
            let unique: std::collections::HashSet<TileId> = ls.iter().copied().collect();
            prop_assume!(unique.len() == ls.len());
            let mut tree = Tree::new(root);
            let start = tree.focus().unwrap();
            for _ in 0..ls.len() {
                tree.focus_next();
            }
            prop_assert_eq!(tree.focus(), Some(start));
        }

        /// Every id in `leaves(root)` is accepted by `focus_set`;
        /// id 1000 (outside the 0..16 range) is always rejected.
        #[test]
        fn prop_focus_set_accepts_leaves_rejects_others(root in arb_node()) {
            let mut tree = Tree::new(root);
            let ls: Vec<TileId> = leaves(tree.root()).clone();
            for &id in &ls {
                prop_assert!(tree.focus_set(id), "id {} should be accepted", id);
            }
            prop_assert!(!tree.focus_set(1000), "id 1000 must be rejected");
        }

        /// Focus stability: focus a leaf, replace another subtree so that the
        /// focused leaf is still present, call `ensure_focus_valid` → focus unchanged.
        #[test]
        fn prop_focus_stability_after_sibling_change(
            root in arb_node(),
            extra in arb_node(),
        ) {
            let ls = leaves(&root);
            prop_assume!(!ls.is_empty());
            let mut tree = Tree::new(root);
            let focused_id = ls[0];
            tree.focus_set(focused_id);

            // Build a new tree that keeps the focused leaf plus an extra subtree;
            // the focused id is guaranteed to still be present.
            *tree.root_mut() = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(focused_id)),
                    (Constraint::new(Size::Fill(1)), extra),
                ],
            };
            tree.ensure_focus_valid();
            prop_assert_eq!(
                tree.focus(),
                Some(focused_id),
                "focus should remain on {} after sibling change",
                focused_id,
            );
        }

        /// `swap_focused(Next)` followed by `swap_focused(Prev)` restores the
        /// original child order when `swap_next` was not a no-op (i.e. the
        /// focused leaf was not already at the last sibling position).
        #[test]
        fn prop_swap_roundtrip(root in arb_node()) {
            let ls = leaves(&root);
            prop_assume!(ls.len() >= 2);
            let unique: std::collections::HashSet<TileId> = ls.iter().copied().collect();
            prop_assume!(unique.len() == ls.len());

            let mut tree = Tree::new(root);
            let focused_id = tree.focus().unwrap();
            let initial = leaves(tree.root());

            tree.swap_focused(Dir::Next);
            let after_next = leaves(tree.root());

            if after_next != initial {
                // swap_next moved something; swap_prev must restore it.
                tree.swap_focused(Dir::Prev);
                prop_assert_eq!(leaves(tree.root()), initial,
                    "swap roundtrip must restore child order");
                prop_assert_eq!(tree.focus(), Some(focused_id),
                    "focus id must survive swap roundtrip");
            }
        }
    }

    // ── scroll_by / scroll_to ─────────────────────────────────────────────

    fn carousel_tree(id: TileId, scroll: u16) -> Tree {
        Tree::new(Node::Carousel {
            id,
            orientation: Orientation::Horizontal,
            scroll,
            children: (0u64..5).map(|i| (10u16, Node::Tile(i))).collect(),
        })
    }

    #[test]
    fn scroll_by_positive_increments_scroll() {
        let mut tree = carousel_tree(1, 0);
        tree.scroll_by(1, 5);
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 1) {
            assert_eq!(*scroll, 5);
        }
    }

    #[test]
    fn scroll_by_negative_decrements_saturates_at_zero() {
        let mut tree = carousel_tree(1, 3);
        tree.scroll_by(1, -10); // would go below 0 → saturate
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 1) {
            assert_eq!(*scroll, 0, "scroll must saturate at 0");
        }
    }

    #[test]
    fn scroll_to_sets_absolute_offset() {
        let mut tree = carousel_tree(1, 0);
        tree.scroll_to(1, 42);
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 1) {
            assert_eq!(*scroll, 42);
        }
    }

    #[test]
    fn scroll_by_noop_for_non_carousel_id() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.scroll_by(0, 5); // id 0 is a Tile, not a Carousel → no-op; no panic
        assert_eq!(leaves(tree.root()), vec![0, 1]);
    }

    #[test]
    fn scroll_to_noop_for_missing_id() {
        let mut tree = carousel_tree(1, 10);
        tree.scroll_to(999, 0); // id 999 does not exist → no-op
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 1) {
            assert_eq!(*scroll, 10, "scroll must be unchanged");
        }
    }

    // ── zoom ──────────────────────────────────────────────────────────────

    /// H-split[Tile(0), Carousel(id=99)[Tile(1), Tile(2)], Tile(3)]
    fn zoom_test_tree() -> Tree {
        Tree::new(h_split(vec![
            tile(0),
            carousel(99, vec![tile(1), tile(2)]),
            tile(3),
        ]))
    }

    #[test]
    fn zoom_to_carousel_succeeds() {
        let mut tree = zoom_test_tree();
        assert!(!tree.is_zoomed());
        let ok = tree.zoom_to(99);
        assert!(ok, "zoom to carousel id=99 must succeed");
        assert!(tree.is_zoomed());
        assert_eq!(tree.zoom_depth(), 1);
        assert!(matches!(tree.effective_root(), Node::Carousel { id: 99, .. }));
    }

    #[test]
    fn zoom_to_id_not_in_current_view_fails() {
        // First zoom into carousel 99, then try to zoom to tile 0 (outside it).
        let mut tree = zoom_test_tree();
        assert!(tree.zoom_to(99));
        let ok = tree.zoom_to(0); // tile 0 is outside carousel 99
        assert!(!ok, "zoom to an id outside the current view must fail");
        assert_eq!(tree.zoom_depth(), 1, "zoom depth must be unchanged");
    }

    #[test]
    fn zoom_to_current_effective_root_fails() {
        let mut tree = zoom_test_tree();
        tree.zoom_to(99);
        let ok = tree.zoom_to(99); // 99 is already the effective root
        assert!(!ok, "zoom_to the current effective root must return false");
        assert_eq!(tree.zoom_depth(), 1, "zoom depth must be unchanged");
    }

    #[test]
    fn nested_zoom_and_zoom_out() {
        let mut tree = zoom_test_tree();
        // Zoom into carousel(99), then into tile(1) inside it.
        assert!(tree.zoom_to(99));
        assert!(tree.zoom_to(1)); // tile 1 is inside carousel 99
        assert_eq!(tree.zoom_depth(), 2);
        assert!(matches!(tree.effective_root(), Node::Tile(1)));

        tree.zoom_out(); // pop innermost
        assert_eq!(tree.zoom_depth(), 1);
        assert!(matches!(tree.effective_root(), Node::Carousel { id: 99, .. }));

        tree.zoom_out(); // pop to real root
        assert_eq!(tree.zoom_depth(), 0);
        assert!(!tree.is_zoomed());

        tree.zoom_out(); // extra zoom_out — no-op
        assert_eq!(tree.zoom_depth(), 0);
    }

    #[test]
    fn zoom_focus_and_zoom_reset() {
        let mut tree = zoom_test_tree(); // focus = 0
        let ok = tree.zoom_focus();
        assert!(ok, "zoom_focus on tile 0 must succeed");
        assert!(tree.is_zoomed());
        assert!(matches!(tree.effective_root(), Node::Tile(0)));

        tree.zoom_reset();
        assert!(!tree.is_zoomed());
        assert_eq!(tree.zoom_depth(), 0);
        // Effective root is the real root again.
        assert!(matches!(tree.effective_root(), Node::Split { .. }));
    }

    #[test]
    fn focus_scoping_stays_inside_zoom() {
        let mut tree = zoom_test_tree(); // focus = 0
        assert!(tree.zoom_to(99)); // carousel contains tiles 1, 2

        // focus moved to first leaf of carousel.
        assert_eq!(tree.focus(), Some(1));

        tree.focus_next();
        assert_eq!(tree.focus(), Some(2), "must advance within carousel");
        tree.focus_next();
        assert_eq!(tree.focus(), Some(1), "must wrap within carousel, not escape");
        tree.focus_prev();
        assert_eq!(tree.focus(), Some(2), "prev wraps within carousel");
        tree.focus_first();
        assert_eq!(tree.focus(), Some(1), "first leaf of effective subtree");
        tree.focus_last();
        assert_eq!(tree.focus(), Some(2), "last leaf of effective subtree");
    }

    #[test]
    fn focus_set_rejects_leaf_outside_zoom() {
        let mut tree = zoom_test_tree();
        assert!(tree.zoom_to(99));
        assert!(!tree.focus_set(0), "tile 0 is outside zoomed carousel");
        assert!(!tree.focus_set(3), "tile 3 is outside zoomed carousel");
        assert!(tree.focus_set(1), "tile 1 is inside zoomed carousel");
    }

    #[test]
    fn zoom_adjusts_focus_when_outside_new_subtree() {
        // focus=0 (tile 0 is NOT in carousel 99); after zoom_to(99) focus must move.
        let mut tree = zoom_test_tree();
        assert_eq!(tree.focus(), Some(0));
        assert!(tree.zoom_to(99));
        assert_eq!(tree.focus(), Some(1), "focus must move to first leaf of carousel");
    }

    #[test]
    fn focus_unchanged_when_inside_new_subtree() {
        // focus=1, which IS in carousel(99); zoom_to(99) must not change focus.
        let mut tree = zoom_test_tree();
        tree.focus_set(1);
        assert!(tree.zoom_to(99));
        assert_eq!(tree.focus(), Some(1), "focus already inside new subtree — must be unchanged");
    }

    #[test]
    fn zoom_out_leaves_focus_unchanged() {
        let mut tree = zoom_test_tree();
        assert!(tree.zoom_to(99));
        assert_eq!(tree.focus(), Some(1)); // adjusted to first leaf of carousel
        tree.zoom_out();
        // Focus stays on tile 1, which is valid in the wider root.
        assert_eq!(tree.focus(), Some(1), "focus must be unchanged after zoom_out");
    }

    #[test]
    fn carousel_scroll_preserved_through_zoom_round_trip() {
        let mut tree = zoom_test_tree();
        tree.scroll_to(99, 7);
        assert!(tree.zoom_to(99));
        tree.zoom_out();
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), 99) {
            assert_eq!(*scroll, 7, "scroll must survive zoom round-trip");
        }
    }

    #[test]
    fn ensure_zoom_valid_prunes_dangling_zoom_level() {
        let mut tree = zoom_test_tree();
        assert!(tree.zoom_to(99)); // zoom into carousel
        // Structurally remove the carousel by replacing the tree root.
        *tree.root_mut() = h_split(vec![tile(0), tile(3)]); // carousel(99) gone
        tree.ensure_zoom_valid();
        assert_eq!(tree.zoom_depth(), 0, "dangling zoom level must be pruned");
        assert!(!tree.is_zoomed());
    }

    #[test]
    fn ensure_zoom_valid_keeps_valid_levels() {
        let mut tree = zoom_test_tree();
        assert!(tree.zoom_to(99));
        assert!(tree.zoom_to(1)); // depth=2
        // Remove tile(2) but keep carousel(99) and tile(1).
        if let Some(Node::Carousel { children, .. }) = node_by_id_mut(tree.root_mut(), 99) {
            children.retain(|(_, n)| matches!(n, Node::Tile(1)));
        }
        tree.ensure_zoom_valid();
        // Both zoom ids (99, 1) still resolve: no truncation.
        assert_eq!(tree.zoom_depth(), 2, "valid zoom levels must be preserved");
    }

    #[test]
    fn zoom_to_tile_id_succeeds() {
        // zoom_to works for Tile nodes too (tmux fullscreen a single pane).
        let mut tree = zoom_test_tree(); // focus = 0
        let ok = tree.zoom_to(0);
        assert!(ok, "zoom_to a Tile id must succeed");
        assert!(matches!(tree.effective_root(), Node::Tile(0)));
        assert_eq!(tree.zoom_depth(), 1);
    }

    // ── scroll_focus_into_view ────────────────────────────────────────────

    /// Return the current scroll of the carousel identified by `car_id`.
    fn get_scroll(tree: &Tree, car_id: TileId) -> u16 {
        if let Some(Node::Carousel { scroll, .. }) = node_by_id(tree.root(), car_id) {
            *scroll
        } else {
            panic!("carousel {car_id} not found in tree")
        }
    }

    /// Vertical carousel (car_id) with 5 children × 5 rows each, focused on
    /// `focus_id`.  `scroll` is the initial offset.
    fn vert_carousel_tree5(car_id: TileId, scroll: u16, focus_id: TileId) -> Tree {
        let root = Node::Carousel {
            id: car_id,
            orientation: Orientation::Vertical,
            scroll,
            children: (0u64..5).map(|i| (5u16, Node::Tile(i))).collect(),
        };
        let mut tree = Tree::new(root);
        tree.focus_set(focus_id);
        tree
    }

    #[test]
    fn reveal_tile_below_fold_flush_at_far_edge() {
        // 5 children × 5 rows = 25 total; viewport height = 10.
        // Focus tile 4: child_start=20, ext=5. scroll=0 → [0,10); tile below fold.
        // Bottom check: 20+5=25 > 0+10 → new = 25-10 = 15.
        let area = Rect::new(0, 0, 10, 10);
        let mut tree = vert_carousel_tree5(100, 0, 4);
        tree.scroll_focus_into_view(area);
        assert_eq!(get_scroll(&tree, 100), 15,
            "scroll must be 15 so tile 4 is flush at the bottom edge");
    }

    #[test]
    fn reveal_tile_above_fold_flush_at_near_edge() {
        // scroll=10 → viewport [10,20); tile 0 at [0,5) is above the fold.
        // Top check: child_start=0 < scroll=10 → new = 0.
        let area = Rect::new(0, 0, 10, 10);
        let mut tree = vert_carousel_tree5(100, 10, 0);
        tree.scroll_focus_into_view(area);
        assert_eq!(get_scroll(&tree, 100), 0,
            "scroll must be 0 so tile 0 is flush at the top edge");
    }

    #[test]
    fn reveal_already_visible_tile_unchanged() {
        // scroll=0 → viewport [0,10); tile 1 at [5,10) is fully visible → no change.
        let area = Rect::new(0, 0, 10, 10);
        let mut tree = vert_carousel_tree5(100, 0, 1);
        tree.scroll_focus_into_view(area);
        assert_eq!(get_scroll(&tree, 100), 0, "scroll must be unchanged");
    }

    #[test]
    fn reveal_taller_than_viewport_reveals_top_edge() {
        // Carousel: [Tile(0) extent=15, Tile(1) extent=5]; viewport height=10.
        // Focus tile 0: child_start=0, ext=15 > main_extent=10. scroll=5.
        // Top check: 0 < 5 → new = 0 (reveals top, not bottom).
        let root = Node::Carousel {
            id: 100,
            orientation: Orientation::Vertical,
            scroll: 5,
            children: vec![
                (15u16, Node::Tile(0)),
                (5u16, Node::Tile(1)),
            ],
        };
        let mut tree = Tree::new(root);
        tree.focus_set(0);
        let area = Rect::new(0, 0, 10, 10);
        tree.scroll_focus_into_view(area);
        assert_eq!(get_scroll(&tree, 100), 0,
            "too-tall child: scroll must reveal the top edge, not thrash to bottom");
    }

    #[test]
    fn reveal_nested_carousels_both_scroll() {
        // Outer vertical carousel (id=10): 3 children × 20 rows; viewport height=10.
        // Outer child 2 = inner horizontal carousel (id=20): 3 children × 5 cols; viewport width=8.
        // Focus inner tile 2 (child_idx=2 in inner).
        //
        // Outer: child_start=40, ext=20, scroll=0, main_extent=10.
        //   Bottom check: 40+20=60 > 0+10 → new = 60-10 = 50.
        //   max_scroll = 60-10 = 50.  outer scroll → 50.
        //   on-screen rect for outer child 2 = Rect::new(0, 0, 8, 10).
        //
        // Inner: child_start=10, ext=5, scroll=0, main_extent=8.
        //   Bottom check: 10+5=15 > 0+8 → new = 15-8 = 7.
        //   max_scroll = 15-8 = 7.  inner scroll → 7.
        let inner = Node::Carousel {
            id: 20,
            orientation: Orientation::Horizontal,
            scroll: 0,
            children: vec![
                (5u16, Node::Tile(0)),
                (5u16, Node::Tile(1)),
                (5u16, Node::Tile(2)),
            ],
        };
        let root = Node::Carousel {
            id: 10,
            orientation: Orientation::Vertical,
            scroll: 0,
            children: vec![
                (20u16, Node::Tile(10)),
                (20u16, Node::Tile(11)),
                (20u16, inner),
            ],
        };
        let area = Rect::new(0, 0, 8, 10);
        let mut tree = Tree::new(root);
        tree.focus_set(2); // inner tile 2
        tree.scroll_focus_into_view(area);
        assert_eq!(get_scroll(&tree, 10), 50,
            "outer carousel must scroll to expose inner carousel (child 2)");
        assert_eq!(get_scroll(&tree, 20), 7,
            "inner carousel must scroll to expose tile 2");
    }

    #[test]
    fn reveal_no_focus_is_noop() {
        let mut tree = Tree::new(h_split(vec![])); // no leaves → focus = None
        tree.scroll_focus_into_view(Rect::new(0, 0, 80, 24)); // must not panic
    }

    #[test]
    fn reveal_no_carousel_on_path_is_noop() {
        // Pure h_split tree — no carousel → no state to change, no panic.
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.focus_set(0);
        tree.scroll_focus_into_view(Rect::new(0, 0, 80, 24));
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn reveal_integration_focused_child_fully_framed() {
        // After scroll_focus_into_view the carousel is in the right position so
        // render_carousel shows the focused child with both borders intact.
        //
        // 3 children × 5 rows; viewport 10×5. Focus tile 2 (child_start=10, ext=5).
        // scroll=0 → bottom check: 10+5=15 > 0+5 → new = 15-5 = 10.
        // render at scroll=10: child 2 fills the entire viewport → full box visible.
        use crate::{
            border::{draw_box, BorderStyle, Borders, CornerStyle, LineWeight},
            buffer::Buffer,
            render::render_carousel,
            style::Style,
        };
        let root = Node::Carousel {
            id: 1,
            orientation: Orientation::Vertical,
            scroll: 0,
            children: (0u64..3).map(|i| (5u16, Node::Tile(i))).collect(),
        };
        let area = Rect::new(0, 0, 10, 5);
        let mut tree = Tree::new(root);
        tree.focus_set(2);
        tree.scroll_focus_into_view(area);
        assert_eq!(get_scroll(&tree, 1), 10, "pre-condition: scroll must be 10");

        let bstyle = BorderStyle {
            weight: LineWeight::Light,
            corners: CornerStyle::Square,
            style: Style::default(),
        };
        let mut buf = Buffer::empty(area);
        render_carousel(&mut buf, tree.root_mut(), area, &mut |b, _id, rect| {
            draw_box(b, rect, Borders::ALL, &bstyle);
        });
        assert_eq!(buf.get(0, 0).symbol, "┌", "top border of focused child must be present");
        assert_eq!(buf.get(0, 4).symbol, "└", "bottom border of focused child must be present");
    }

    proptest! {
        /// After `zoom_to(id)` then `zoom_out()`, the previous depth and
        /// effective-root identity (by address) are both restored.
        #[test]
        fn prop_zoom_roundtrip(tile_id in 0u64..3u64) {
            // Tree: Carousel(id=200)[Tile(0), Tile(1), Tile(2)]
            let root = Node::Carousel {
                id: 200,
                orientation: Orientation::Vertical,
                scroll: 0,
                children: (0u64..3).map(|i| (5u16, Node::Tile(i))).collect(),
            };
            let mut tree = Tree::new(root);
            let depth_before = tree.zoom_depth();

            // zoom into one of the inner tiles
            prop_assume!(tree.zoom_to(tile_id));
            prop_assert!(tree.is_zoomed());
            prop_assert_eq!(tree.zoom_depth(), depth_before + 1);
            prop_assert!(matches!(tree.effective_root(), Node::Tile(t) if *t == tile_id));

            tree.zoom_out();
            prop_assert_eq!(tree.zoom_depth(), depth_before, "depth must be restored");
            prop_assert!(!tree.is_zoomed(), "must no longer be zoomed");
            prop_assert!(
                matches!(tree.effective_root(), Node::Carousel { id: 200, .. }),
                "effective root must be the original carousel"
            );
        }

        /// After `scroll_focus_into_view`, the focused child's `[start, start+ext)`
        /// is contained in `[scroll, scroll+main_extent)`, or top-aligned when the
        /// child is taller than the viewport.
        #[test]
        fn prop_reveal_containment(
            initial_scroll in 0u16..=50u16,
            focus_child  in 0usize..5usize,
            viewport_h   in 3u16..=20u16,
        ) {
            let root = Node::Carousel {
                id: 1,
                orientation: Orientation::Vertical,
                scroll: initial_scroll,
                children: (0u64..5).map(|i| (5u16, Node::Tile(i))).collect(),
            };
            let area = Rect::new(0, 0, 10, viewport_h);
            let mut tree = Tree::new(root);
            tree.focus_set(focus_child as u64);
            tree.scroll_focus_into_view(area);

            let new_scroll   = get_scroll(&tree, 1) as u32;
            let child_start  = focus_child as u32 * 5;
            let ext: u32     = 5;
            let main: u32    = viewport_h as u32;

            if ext <= main {
                prop_assert!(child_start >= new_scroll,
                    "child start {child_start} < scroll {new_scroll}");
                prop_assert!(child_start + ext <= new_scroll + main,
                    "child end {} > scroll+vp {}", child_start + ext, new_scroll + main);
            } else {
                // Too-tall: top edge must be aligned.
                prop_assert_eq!(new_scroll, child_start,
                    "too-tall child: scroll must equal child_start");
            }
        }
    }

    // ── focus_dir ─────────────────────────────────────────────────────────

    fn v_carousel(id: TileId, kids: Vec<Node>) -> Node {
        Node::Carousel {
            id,
            orientation: Orientation::Vertical,
            scroll: 0,
            children: kids.into_iter().map(|n| (10u16, n)).collect(),
        }
    }

    #[test]
    fn focus_dir_down_in_vertical_carousel_advances_one_step() {
        let mut tree = Tree::new(v_carousel(1, vec![tile(0), tile(1), tile(2)]));
        assert_eq!(tree.focus(), Some(0));
        tree.focus_dir(Direction::Down);
        assert_eq!(tree.focus(), Some(1));
        tree.focus_dir(Direction::Down);
        assert_eq!(tree.focus(), Some(2));
    }

    #[test]
    fn focus_dir_down_wraps_past_last_child() {
        let mut tree = Tree::new(v_carousel(1, vec![tile(0), tile(1), tile(2)]));
        tree.focus = Some(2);
        tree.focus_dir(Direction::Down);
        assert_eq!(tree.focus(), Some(0), "must wrap to first child");
    }

    #[test]
    fn focus_dir_up_wraps_past_first_child() {
        let mut tree = Tree::new(v_carousel(1, vec![tile(0), tile(1), tile(2)]));
        tree.focus_dir(Direction::Up);
        assert_eq!(tree.focus(), Some(2), "must wrap to last child");
    }

    #[test]
    fn focus_dir_wrong_axis_is_noop_on_vertical_carousel() {
        let mut tree = Tree::new(v_carousel(1, vec![tile(0), tile(1), tile(2)]));
        tree.focus_dir(Direction::Left);
        assert_eq!(tree.focus(), Some(0), "Left on a vertical carousel must be a no-op");
        tree.focus_dir(Direction::Right);
        assert_eq!(tree.focus(), Some(0), "Right on a vertical carousel must be a no-op");
    }

    #[test]
    fn focus_dir_left_right_on_horizontal_carousel() {
        let mut tree = Tree::new(carousel(99, vec![tile(0), tile(1), tile(2)]));
        assert_eq!(tree.focus(), Some(0));
        tree.focus_dir(Direction::Right);
        assert_eq!(tree.focus(), Some(1));
        tree.focus_dir(Direction::Left);
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn focus_dir_noop_on_pure_split_tree() {
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1), tile(2)]));
        tree.focus_dir(Direction::Right);
        assert_eq!(tree.focus(), Some(0));
    }

    #[test]
    fn focus_dir_carousel_inside_split_moves_within_carousel() {
        let root = h_split(vec![
            tile(0),
            carousel(1, vec![tile(1), tile(2), tile(3)]),
        ]);
        let mut tree = Tree::new(root);
        assert_eq!(tree.focus(), Some(0));

        // Tile(0) has no carousel ancestor → no-op.
        tree.focus_dir(Direction::Right);
        assert_eq!(tree.focus(), Some(0));

        // Tile(1) is inside the carousel → advances.
        tree.focus = Some(1);
        tree.focus_dir(Direction::Right);
        assert_eq!(tree.focus(), Some(2));
    }

    #[test]
    fn focus_dir_noop_on_no_focus() {
        let mut tree = Tree::new(v_carousel(1, vec![tile(0), tile(1)]));
        tree.focus = None;
        tree.focus_dir(Direction::Down);
        assert_eq!(tree.focus(), None);
    }

    // ── focus_dir_cross ───────────────────────────────────────────────────

    /// 2×2 grid: H-split [V-split[Tile(0), Tile(1)] | V-split[Tile(2), Tile(3)]].
    /// In 40×20: Tile(0)=(0,0,20,10), Tile(1)=(0,10,20,10),
    ///           Tile(2)=(20,0,20,10), Tile(3)=(20,10,20,10).
    fn two_by_two() -> Node {
        h_split(vec![
            v_split(vec![tile(0), tile(1)]),
            v_split(vec![tile(2), tile(3)]),
        ])
    }

    #[test]
    fn focus_dir_cross_right_moves_to_best_right_neighbor() {
        let area = Rect::new(0, 0, 40, 20);
        let mut tree = Tree::new(two_by_two());
        assert_eq!(tree.focus(), Some(0)); // top-left
        tree.focus_dir_cross(Direction::Right, area);
        assert_eq!(tree.focus(), Some(2), "top-right has full Y-overlap and must win");
    }

    #[test]
    fn focus_dir_cross_down_moves_to_best_lower_neighbor() {
        let area = Rect::new(0, 0, 40, 20);
        let mut tree = Tree::new(two_by_two());
        tree.focus_dir_cross(Direction::Down, area);
        assert_eq!(tree.focus(), Some(1), "bottom-left has full X-overlap and must win");
    }

    #[test]
    fn focus_dir_cross_noop_at_screen_edge() {
        let area = Rect::new(0, 0, 40, 20);
        let mut tree = Tree::new(two_by_two()); // focus = tile(0) top-left
        tree.focus_dir_cross(Direction::Left, area);
        assert_eq!(tree.focus(), Some(0), "no tile to the left → no change");
        tree.focus_dir_cross(Direction::Up, area);
        assert_eq!(tree.focus(), Some(0), "no tile above → no change");
    }

    #[test]
    fn focus_dir_cross_picks_better_perpendicular_overlap() {
        // H-split [V-split[Focus(0), Padding(3)] | V-split[A(1), B(2)]].
        // In 40×10 each H-half is 20 wide, each V-half is 5 tall.
        // Focus(0): (0,0,20,5). A(1): (20,0,20,5). B(2): (20,5,20,5).
        // Moving Right from Focus(0): A has Y-overlap=5, B has Y-overlap=0 → A wins.
        let area = Rect::new(0, 0, 40, 10);
        let root = h_split(vec![
            v_split(vec![tile(0), tile(3)]),
            v_split(vec![tile(1), tile(2)]),
        ]);
        let mut tree = Tree::new(root);
        assert_eq!(tree.focus(), Some(0));
        tree.focus_dir_cross(Direction::Right, area);
        assert_eq!(tree.focus(), Some(1), "better Y-overlap must win");
    }

    #[test]
    fn focus_dir_cross_prefers_closer_tile() {
        // H-split [Focus(0) | Mid(1) | Far(2)].  30×10.
        // Focus at x=[0,10), Mid at x=[10,20), Far at x=[20,30).
        // Moving Right: both candidates are right of Focus; Mid is gap=0, Far is gap=10.
        let area = Rect::new(0, 0, 30, 10);
        let root = h_split(vec![tile(0), tile(1), tile(2)]);
        let mut tree = Tree::new(root);
        tree.focus_dir_cross(Direction::Right, area);
        assert_eq!(tree.focus(), Some(1), "closer tile must be preferred");
    }

    #[test]
    fn focus_dir_cross_is_zoom_aware() {
        // H-split [Tile(0) | Tile(1)]; zoom into Tile(0).
        // Effective root = Tile(0): no other tile visible → no-op.
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.zoom_focus();
        tree.focus_dir_cross(Direction::Right, area);
        assert_eq!(tree.focus(), Some(0), "Tile(1) outside zoom must not be reachable");
    }

    #[test]
    fn focus_dir_cross_noop_on_no_focus() {
        let area = Rect::new(0, 0, 40, 10);
        let mut tree = Tree::new(h_split(vec![tile(0), tile(1)]));
        tree.focus = None;
        tree.focus_dir_cross(Direction::Right, area);
        assert_eq!(tree.focus(), None);
    }
}
