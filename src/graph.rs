// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Graph canvas: a tile whose floating children are *nodes*, placed by hand
//! (design note §5.4 manual half, §5.7 canvas concept).
//!
//! A [`GraphCanvas`] owns a set of nodes — each a stable [`TileId`] paired with a
//! position and size in a **logical canvas coordinate space** — and the operations
//! to move them: drag (via the existing hit-test), keyboard nudge, and grid snap.
//! It is a thin manager over the Phase 1 floating-tile foundation
//! ([`FloatChild`]/[`FloatLayer`]): nodes are floating children, so they carry
//! stable ids across re-solves and their positions are part of the canvas state.
//!
//! ## Coordinates
//!
//! Node positions are **canvas-local**, in the range that keeps each node fully
//! inside the canvas (clamping is enforced on every move). The canvas may be
//! larger than the on-screen window that shows it; [`solve`](GraphCanvas::solve)
//! maps canvas-local rects to absolute screen rects against a window and clips to
//! it. This phase uses a **fixed origin** — panning a window over a larger canvas
//! and culling off-window nodes is Phase 10.
//!
//! ## Hit-testing
//!
//! There is no bespoke hit-test: [`solve`](GraphCanvas::solve) yields the same
//! `(TileId, Rect)` shape the tiling solver produces, so the existing
//! [`mouse::tile_at`](crate::mouse::tile_at) finds the node under a screen cell —
//! the basis for click-to-select and drag.

use crate::float::{FloatChild, FloatLayer, FloatRect};
use crate::geometry::Rect;
use crate::layout::TileId;

// ── GraphCanvas ──────────────────────────────────────────────────────────────

/// A canvas of hand-placed nodes (floating children with stable ids).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphCanvas {
    /// Nodes in back-to-front order; each is an id + canvas-local rect.
    nodes: Vec<FloatChild>,
    /// Logical canvas extent in cells. Node positions are clamped to keep every
    /// node fully inside `width × height`.
    width: u16,
    height: u16,
    /// Grid step for [`snap_to_grid`](GraphCanvas::snap_to_grid) (always ≥ 1).
    grid: u16,
}

impl GraphCanvas {
    /// Create an empty canvas of logical size `width × height` (grid step 1).
    pub fn new(width: u16, height: u16) -> Self {
        Self { nodes: Vec::new(), width, height, grid: 1 }
    }

    /// Set the grid step used by [`snap_to_grid`](GraphCanvas::snap_to_grid).
    pub fn with_grid(mut self, grid: u16) -> Self {
        self.grid = grid.max(1);
        self
    }

    /// The logical canvas size `(width, height)`.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Resize the logical canvas, re-clamping every node to stay inside it.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        for n in &mut self.nodes {
            n.place = clamp_in(n.place, width, height);
        }
    }

    /// The grid step.
    pub fn grid(&self) -> u16 {
        self.grid
    }

    /// The nodes, in back-to-front order.
    pub fn nodes(&self) -> &[FloatChild] {
        &self.nodes
    }

    /// Add a node with id `id` at canvas-local `place`, clamped inside the canvas.
    ///
    /// If a node with `id` already exists it is replaced (moved). New nodes are
    /// added on top (drawn last, hit-tested first).
    pub fn add(&mut self, id: TileId, place: FloatRect) {
        let place = self.clamp(place);
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == id) {
            node.place = place;
        } else {
            self.nodes.push(FloatChild::new(id, place));
        }
    }

    /// Remove the node with `id`. Returns `true` if it was present.
    pub fn remove(&mut self, id: TileId) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|n| n.id != id);
        self.nodes.len() != before
    }

    /// The canvas-local rect of node `id`, or `None` if absent.
    pub fn place(&self, id: TileId) -> Option<FloatRect> {
        self.nodes.iter().find(|n| n.id == id).map(|n| n.place)
    }

    /// Move node `id` to canvas-local position `(x, y)`, clamped inside the canvas.
    /// No-op if the node is absent.
    pub fn move_to(&mut self, id: TileId, x: u16, y: u16) {
        let (w, h) = (self.width, self.height);
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == id) {
            node.place = clamp_in(FloatRect { x, y, ..node.place }, w, h);
        }
    }

    /// Nudge node `id` by `(dx, dy)` cells (negative = left/up), clamped inside the
    /// canvas. No-op if the node is absent.
    pub fn nudge(&mut self, id: TileId, dx: i32, dy: i32) {
        if let Some(place) = self.place(id) {
            let x = (place.x as i32 + dx).max(0) as u16;
            let y = (place.y as i32 + dy).max(0) as u16;
            self.move_to(id, x, y);
        }
    }

    /// Snap node `id`'s position to the nearest grid multiple, then clamp.
    pub fn snap_to_grid(&mut self, id: TileId) {
        let g = self.grid;
        if let Some(place) = self.place(id) {
            // Round to the nearest multiple of `g` (half rounds up).
            let snap = |v: u16| ((v + g / 2) / g) * g;
            self.move_to(id, snap(place.x), snap(place.y));
        }
    }

    /// Clamp a placement so the node sits fully inside the canvas.
    fn clamp(&self, place: FloatRect) -> FloatRect {
        clamp_in(place, self.width, self.height)
    }

    /// Solve every node to an absolute screen [`Rect`] against the on-screen
    /// `window`, clipping to it.
    ///
    /// Node positions are canvas-local with a fixed origin, so a node lands at
    /// `window.origin + place`. Reuses [`FloatLayer::solve`], so a node that falls
    /// outside `window` (only possible when the canvas is larger than the window)
    /// is clipped to empty — full culling is Phase 10.
    pub fn solve(&self, window: Rect) -> Vec<(TileId, Rect)> {
        FloatLayer { children: self.nodes.clone() }.solve(window)
    }
}

/// Clamp `place` so its rect lies fully within `width × height` (origin 0,0).
///
/// The top-left is pulled toward the origin only as far as needed: a node wider or
/// taller than the canvas is pinned to `0` on that axis.
fn clamp_in(place: FloatRect, width: u16, height: u16) -> FloatRect {
    let x = place.x.min(width.saturating_sub(place.width));
    let y = place.y.min(height.saturating_sub(place.height));
    FloatRect { x, y, width: place.width, height: place.height }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mouse::tile_at;

    fn canvas() -> GraphCanvas {
        let mut c = GraphCanvas::new(40, 20).with_grid(4);
        c.add(1, FloatRect::new(2, 2, 8, 4));
        c.add(2, FloatRect::new(20, 10, 10, 5));
        c
    }

    #[test]
    fn add_place_remove() {
        let mut c = canvas();
        assert_eq!(c.place(1), Some(FloatRect::new(2, 2, 8, 4)));
        assert_eq!(c.nodes().len(), 2);
        // Re-adding an id moves it rather than duplicating.
        c.add(1, FloatRect::new(5, 5, 8, 4));
        assert_eq!(c.nodes().len(), 2);
        assert_eq!(c.place(1), Some(FloatRect::new(5, 5, 8, 4)));
        assert!(c.remove(2));
        assert!(!c.remove(2));
        assert_eq!(c.nodes().len(), 1);
    }

    #[test]
    fn move_clamps_into_canvas() {
        let mut c = canvas();
        // Way past the right/bottom edge → clamped so the node still fits.
        c.move_to(1, 999, 999);
        let p = c.place(1).unwrap();
        assert_eq!(p, FloatRect::new(40 - 8, 20 - 4, 8, 4));
    }

    #[test]
    fn nudge_then_inverse_returns() {
        let mut c = canvas();
        let before = c.place(1).unwrap();
        c.nudge(1, 3, -1);
        c.nudge(1, -3, 1);
        assert_eq!(c.place(1).unwrap(), before);
    }

    #[test]
    fn resize_reclamps_nodes() {
        let mut c = canvas(); // 40×20, node 2 at (20,10,10,5)
        c.resize(22, 12); // node 2 (right edge 30) no longer fits → clamped
        let p = c.place(2).unwrap();
        assert!(p.x + p.width <= 22 && p.y + p.height <= 12);
        assert_eq!(p, FloatRect::new(22 - 10, 12 - 5, 10, 5));
    }

    #[test]
    fn snap_aligns_to_grid() {
        let mut c = canvas(); // grid 4
        c.move_to(1, 6, 9); // → snaps to (8, 8)? 6→8 (nearest 4), 9→8
        c.snap_to_grid(1);
        let p = c.place(1).unwrap();
        assert_eq!(p.x % 4, 0);
        assert_eq!(p.y % 4, 0);
        assert_eq!((p.x, p.y), (8, 8));
    }

    #[test]
    fn solve_and_hit_test() {
        let c = canvas();
        let window = Rect::new(100, 50, 40, 20); // canvas shown at (100,50)
        let rects = c.solve(window);
        // Node 1 (canvas-local 2,2) → absolute (102, 52).
        let r1 = rects.iter().find(|(id, _)| *id == 1).unwrap().1;
        assert_eq!(r1, Rect::new(102, 52, 8, 4));
        // The existing tile_at hit-tests the same shape.
        assert_eq!(tile_at(&rects, 103, 53), Some(1));
        assert_eq!(tile_at(&rects, 0, 0), None);
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// After any sequence of moves/nudges a node stays fully inside the canvas.
        #[test]
        fn prop_node_stays_in_canvas(
            w in 6u16..60, h in 6u16..40,
            nw in 1u16..10, nh in 1u16..8,
            ops in prop::collection::vec((-80i32..80, -80i32..80), 0..30),
        ) {
            let mut c = GraphCanvas::new(w, h);
            c.add(1, FloatRect::new(0, 0, nw.min(w), nh.min(h)));
            for (dx, dy) in ops {
                c.nudge(1, dx, dy);
                let p = c.place(1).unwrap();
                prop_assert!(p.x as u32 + p.width as u32 <= w as u32,
                    "node right {} > canvas {}", p.x + p.width, w);
                prop_assert!(p.y as u32 + p.height as u32 <= h as u32);
            }
        }

        /// Away from the edges, a nudge and its inverse cancel exactly.
        #[test]
        fn prop_nudge_inverse_identity(dx in -20i32..20, dy in -20i32..20) {
            // Canvas big enough that a 4×3 node near the centre never clamps.
            let mut c = GraphCanvas::new(200, 200);
            c.add(7, FloatRect::new(100, 100, 4, 3));
            let before = c.place(7).unwrap();
            c.nudge(7, dx, dy);
            c.nudge(7, -dx, -dy);
            prop_assert_eq!(c.place(7).unwrap(), before);
        }
    }
}
