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
use crate::vlist::ScrollMetrics;

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

// ── Viewport ───────────────────────────────────────────────────────────────

/// A window onto a larger canvas: a `(dx, dy)` **pan** offset plus the on-screen
/// window rect (design note §5.7). The canvas cell at `pan` is shown at the
/// window's top-left; panning slides the window over the canvas.
///
/// It projects canvas-space rects (nodes, and — via [`visible`](Viewport::visible)
/// — connector routing regions) to screen and culls whatever falls outside, and
/// reports **exact** scroll metrics on each axis (the canvas bounding box is known,
/// unlike an estimated row scrollbar). Panning is a camera move only: it never
/// touches canvas coordinates, so connector routes computed in canvas space stay
/// put as you scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    window: Rect,
    canvas: (u16, u16),
    pan: (u16, u16),
}

impl Viewport {
    /// A viewport showing a `canvas_w × canvas_h` canvas through `window`, panned to
    /// the origin.
    pub fn new(window: Rect, canvas_w: u16, canvas_h: u16) -> Self {
        let mut v = Self { window, canvas: (canvas_w, canvas_h), pan: (0, 0) };
        v.clamp();
        v
    }

    /// The on-screen window rect.
    pub fn window(&self) -> Rect {
        self.window
    }
    /// The logical canvas size `(width, height)`.
    pub fn canvas(&self) -> (u16, u16) {
        self.canvas
    }
    /// The current pan offset `(x, y)` in canvas coordinates.
    pub fn pan(&self) -> (u16, u16) {
        self.pan
    }
    /// The largest pan on each axis — `canvas − window`, saturating (zero when the
    /// canvas fits the window, i.e. nothing to scroll).
    pub fn max_pan(&self) -> (u16, u16) {
        (
            self.canvas.0.saturating_sub(self.window.width),
            self.canvas.1.saturating_sub(self.window.height),
        )
    }

    fn clamp(&mut self) {
        let (mx, my) = self.max_pan();
        self.pan = (self.pan.0.min(mx), self.pan.1.min(my));
    }

    /// Replace the window rect (e.g. on resize), re-clamping the pan.
    pub fn set_window(&mut self, window: Rect) {
        self.window = window;
        self.clamp();
    }
    /// Replace the canvas size, re-clamping the pan.
    pub fn set_canvas(&mut self, width: u16, height: u16) {
        self.canvas = (width, height);
        self.clamp();
    }
    /// Pan to an absolute offset, clamped to `[0, max_pan]`.
    pub fn set_pan(&mut self, x: u16, y: u16) {
        self.pan = (x, y);
        self.clamp();
    }
    /// Pan by `(dx, dy)` cells (negative = left/up), clamped to the canvas.
    pub fn pan_by(&mut self, dx: i32, dy: i32) {
        let x = (self.pan.0 as i32 + dx).max(0) as u16;
        let y = (self.pan.1 as i32 + dy).max(0) as u16;
        self.set_pan(x, y);
    }

    /// The canvas sub-rect currently visible through the window. Pass this as the
    /// `canvas` region (with [`origin`](Viewport::origin)) to connector rendering.
    pub fn visible(&self) -> Rect {
        let w = self.window.width.min(self.canvas.0.saturating_sub(self.pan.0));
        let h = self.window.height.min(self.canvas.1.saturating_sub(self.pan.1));
        Rect::new(self.pan.0, self.pan.1, w, h)
    }

    /// The screen origin the canvas's visible top-left draws at (the window origin).
    pub fn origin(&self) -> (u16, u16) {
        (self.window.x, self.window.y)
    }

    /// Whether canvas rect `c` intersects the visible region grown by `margin` — the
    /// cull test. Drawing every node for which this is true never omits one that
    /// touches the window.
    pub fn is_visible(&self, c: Rect, margin: u16) -> bool {
        let v = self.visible();
        let grown = Rect::new(
            v.x.saturating_sub(margin),
            v.y.saturating_sub(margin),
            v.width + 2 * margin,
            v.height + 2 * margin,
        );
        !c.intersection(grown).is_empty()
    }

    /// Project canvas rect `c` to its on-screen rect, **clipped to the window**;
    /// `None` if `c` is not currently visible. The clip means a node straddling an
    /// edge is drawn as its visible portion.
    pub fn project(&self, c: Rect) -> Option<Rect> {
        let inter = c.intersection(self.visible());
        if inter.is_empty() {
            return None;
        }
        Some(Rect::new(
            self.window.x + (inter.x - self.pan.0),
            self.window.y + (inter.y - self.pan.1),
            inter.width,
            inter.height,
        ))
    }

    /// Exact horizontal scroll metrics (pan over canvas width) for
    /// [`render_scrollbar`](crate::vlist::render_scrollbar).
    pub fn h_metrics(&self) -> ScrollMetrics {
        axis_metrics(self.pan.0, self.window.width, self.canvas.0)
    }
    /// Exact vertical scroll metrics (pan over canvas height).
    pub fn v_metrics(&self) -> ScrollMetrics {
        axis_metrics(self.pan.1, self.window.height, self.canvas.1)
    }
}

/// Exact [`ScrollMetrics`] for one axis: `offset` of a `window`-long view over a
/// `total`-long canvas. Always `exact` — the canvas length is known.
fn axis_metrics(offset: u16, window: u16, total: u16) -> ScrollMetrics {
    let total_f = total.max(1) as f32;
    ScrollMetrics {
        position: offset as f32 / total_f,
        extent: window.min(total) as f32 / total_f,
        exact: true,
    }
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

    // ── Viewport (Phase 10) ───────────────────────────────────────────────

    #[test]
    fn pan_clamps_to_canvas() {
        // 100×60 canvas in a 40×20 window → max pan (60, 40).
        let mut vp = Viewport::new(Rect::new(0, 0, 40, 20), 100, 60);
        assert_eq!(vp.max_pan(), (60, 40));
        vp.pan_by(999, 999);
        assert_eq!(vp.pan(), (60, 40), "clamped to max");
        vp.pan_by(-999, -5);
        assert_eq!(vp.pan(), (0, 35));
        // A canvas no larger than the window cannot scroll.
        let small = Viewport::new(Rect::new(0, 0, 40, 20), 30, 10);
        assert_eq!(small.max_pan(), (0, 0));
    }

    #[test]
    fn project_maps_and_clips() {
        let mut vp = Viewport::new(Rect::new(5, 2, 20, 10), 100, 60);
        vp.set_pan(10, 4);
        // A node fully inside: canvas (12,6,4,3) → screen (5+12-10, 2+6-4) = (7,4).
        assert_eq!(vp.project(Rect::new(12, 6, 4, 3)), Some(Rect::new(7, 4, 4, 3)));
        // A node straddling the left edge is clipped to its visible part.
        // canvas (8,6,4,3): visible part x∈[10,12) → screen (5, 4), width 2.
        assert_eq!(vp.project(Rect::new(8, 6, 4, 3)), Some(Rect::new(5, 4, 2, 3)));
        // Fully off-window → culled.
        assert_eq!(vp.project(Rect::new(0, 0, 4, 3)), None);
    }

    #[test]
    fn scroll_metrics_are_exact() {
        let mut vp = Viewport::new(Rect::new(0, 0, 20, 10), 100, 50);
        vp.set_pan(40, 20);
        let h = vp.h_metrics();
        assert!(h.exact);
        assert!((h.position - 0.4).abs() < 1e-6); // 40 / 100
        assert!((h.extent - 0.2).abs() < 1e-6); //   20 / 100
        let v = vp.v_metrics();
        assert!((v.position - 0.4).abs() < 1e-6); // 20 / 50
        assert!((v.extent - 0.2).abs() < 1e-6); //   10 / 50
    }

    #[test]
    fn routes_are_invariant_under_pan() {
        use crate::route::{route_all, RouteRequest};
        use crate::tree::Direction;
        use std::collections::HashSet;
        // A scene in canvas space; routing depends only on the canvas, not the camera.
        let nodes = [Rect::new(2, 2, 6, 4), Rect::new(30, 14, 6, 4)];
        let canvas = Rect::new(0, 0, 60, 30);
        let free: HashSet<(u16, u16)> =
            crate::float::free_cells_in_window(canvas, &nodes, 0, canvas).into_iter().collect();
        let reqs = [RouteRequest::new((9, 4), (29, 16), Direction::Right, Direction::Left)];
        let before = route_all(&free, &reqs, 4, 4);
        // Panning the viewport does not touch the canvas rects or free cells…
        let mut vp = Viewport::new(Rect::new(0, 0, 20, 12), 60, 30);
        vp.pan_by(15, 8);
        let after = route_all(&free, &reqs, 4, 4);
        assert_eq!(before, after, "routes are stable under pan (canvas-space routing)");
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

        /// Culling never omits a node that touches the window: `is_visible` agrees
        /// exactly with a ground-truth canvas-space intersection, and a visible node
        /// always projects to a non-empty screen rect.
        #[test]
        fn prop_cull_never_omits_intersecting_node(
            px in 0u16..80, py in 0u16..40,
            nx in 0u16..120, ny in 0u16..60, nw in 1u16..12, nh in 1u16..8,
        ) {
            let mut vp = Viewport::new(Rect::new(3, 1, 30, 16), 120, 60);
            vp.set_pan(px, py);
            let node = Rect::new(nx, ny, nw, nh);
            let touches = !node.intersection(vp.visible()).is_empty();
            prop_assert_eq!(vp.is_visible(node, 0), touches);
            prop_assert_eq!(vp.project(node).is_some(), touches);
        }
    }
}
