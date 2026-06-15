// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Focus model and layout-tree owner.
//!
//! [`Tree`] wraps a [`Node`] root plus interaction state.  Today that state is
//! just the focused tile; later phases extend this (scroll offsets in Phase 4,
//! a zoom stack in Phase 5).
//!
//! ## DFS order
//!
//! Leaf tiles are enumerated in **depth-first pre-order**, children visited in
//! declaration order.  This is the linear `Tab` / `Shift-Tab` traversal order.
//! Geometric/directional focus (`hjkl`) requires solved rects and is deferred
//! to a later phase; this module is restricted to id-based traversal and
//! structural edits (`flip`, `swap`).

use crate::border::LineWeight;
use crate::layout::{Axis, Node, Orientation, TileId};

// ── Free functions ────────────────────────────────────────────────────────────

/// The `TileId` of a leaf node, or `None` for a `Split`.
pub fn tile_id_of(node: &Node) -> Option<TileId> {
    match node {
        Node::Tile(id) => Some(*id),
        Node::Split { .. } => None,
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

// ── Tree helpers ──────────────────────────────────────────────────────────────

/// Navigate `root` by following `path` (each element is a `children` index) and
/// return a mutable reference to the node at that position.
fn node_at_path_mut<'a>(root: &'a mut Node, path: &[usize]) -> Option<&'a mut Node> {
    let mut cur = root;
    for &idx in path {
        match cur {
            Node::Split { children, .. } if idx < children.len() => {
                cur = &mut children[idx].1;
            }
            _ => return None,
        }
    }
    Some(cur)
}

// ── Tree ──────────────────────────────────────────────────────────────────────

/// Owns a layout tree plus interaction state.  Today: the root node and which
/// leaf is focused.  Later phases extend this (scroll offsets, zoom stack).
pub struct Tree {
    root: Node,
    focus: Option<TileId>,
}

impl Tree {
    /// Wrap a root node.  Focus initialises to the first leaf in DFS order,
    /// or `None` if the tree has no `Tile` leaves.
    pub fn new(root: Node) -> Self {
        let focus = leaves(&root).into_iter().next();
        Self { root, focus }
    }

    /// The root node.
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Mutable access for `solve` / `render_shared` and for structural edits.
    ///
    /// After editing the tree call [`ensure_focus_valid`](Tree::ensure_focus_valid).
    pub fn root_mut(&mut self) -> &mut Node {
        &mut self.root
    }

    /// The currently focused tile id, or `None` if the tree has no leaves.
    pub fn focus(&self) -> Option<TileId> {
        self.focus
    }

    /// Focus a specific leaf.  Returns `false` (and leaves focus unchanged)
    /// if `id` is not a `Tile` leaf currently in the tree.
    pub fn focus_set(&mut self, id: TileId) -> bool {
        if leaves(&self.root).contains(&id) {
            self.focus = Some(id);
            true
        } else {
            false
        }
    }

    /// Move focus to the next leaf in DFS order, wrapping at the end.
    ///
    /// If focus is `None` but leaves exist, selects the first leaf.
    /// No-op if the tree has no leaves.
    pub fn focus_next(&mut self) {
        let ls = leaves(&self.root);
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

    /// Move focus to the previous leaf in DFS order, wrapping at the start.
    ///
    /// If focus is `None` but leaves exist, selects the last leaf.
    /// No-op if the tree has no leaves.
    pub fn focus_prev(&mut self) {
        let ls = leaves(&self.root);
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

    /// Move focus to the first leaf in DFS order.
    ///
    /// No-op if the tree has no leaves.
    pub fn focus_first(&mut self) {
        self.focus = leaves(&self.root).into_iter().next();
    }

    /// Move focus to the last leaf in DFS order.
    ///
    /// No-op if the tree has no leaves.
    pub fn focus_last(&mut self) {
        self.focus = leaves(&self.root).into_iter().last();
    }

    /// Re-validate focus after a structural edit.
    ///
    /// If focus is `None` or points to an id no longer present in the tree,
    /// reset it to the first leaf (or `None` if the tree is now empty).
    /// A focus id that still exists is left untouched — focus follows the *id*,
    /// not a position, so adding/removing/reordering *other* leaves never moves it.
    pub fn ensure_focus_valid(&mut self) {
        let ls = leaves(&self.root);
        match self.focus {
            Some(id) if ls.contains(&id) => {}
            _ => self.focus = ls.into_iter().next(),
        }
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

    // ── tile_id_of ────────────────────────────────────────────────────────

    #[test]
    fn tile_id_of_leaf() {
        assert_eq!(tile_id_of(&Node::Tile(7)), Some(7));
    }

    #[test]
    fn tile_id_of_split_is_none() {
        assert_eq!(tile_id_of(&h_split(vec![])), None);
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
}
