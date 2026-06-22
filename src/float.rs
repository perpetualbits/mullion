// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Floating tiles and the free space around them — the shared foundation for
//! runaround text and node-graph routing (design note §2).
//!
//! ## What a floating tile is
//!
//! Every [`Node`](crate::layout::Node) in `layout` *partitions* its parent: a
//! split consumes the whole area with no leftover.  A **floating** child is the
//! opposite primitive — it occupies a sub-rectangle *inset* from the parent's
//! borders, leaving free space around it on one or more sides, without
//! partitioning the parent.
//!
//! This module is deliberately a **separate, composable layer** rather than a
//! new `Node` variant: the layout solver and its consumers (`tree`, downstream
//! apps) match `Node` exhaustively, and the project's additive-only rule forbids
//! breaking those matches.  Floating tiles therefore compose the same way a
//! carousel does — solve the tiling tree, take a tile's rect, then solve floats
//! *within* that rect:
//!
//! ```text
//! solve(tree) ──▶ region_of(tile) ──▶ FloatLayer::solve(tile_rect)
//! ```
//!
//! ## The two outputs
//!
//! A parent holding floating children exposes exactly the two things §2 calls
//! load-bearing:
//!
//! 1. The **placed rectangles** of the floats ([`FloatLayer::solve`]) — for
//!    drawing and hit-testing.
//! 2. The **free space** around them, as an *ordered, queryable structure* read
//!    two ways from the same geometry so the two consumers can never disagree
//!    about where the obstacles are:
//!    - [`free_intervals_in_rows`] — per-row free intervals, for the future text
//!      engine's slot stream (§3.5).
//!    - [`free_cells_in_window`] — free cells over a window, for the future
//!      orthogonal-connector router (§5.2).
//!
//! Both queries are **viewport-bounded**: the caller passes the visible row
//! range or window and the cost is paid only for what is on screen, never for an
//! unbounded document or canvas.
//!
//! ## Coordinate model
//!
//! A [`FloatRect`] is expressed in **parent-local** cells — `(0, 0)` is the
//! parent's top-left.  Storing placements relative to the parent (rather than in
//! absolute screen coordinates) is what lets a float's position survive a
//! re-solve in which the parent itself moves: the same `FloatChild` re-solves to
//! the correct absolute rect wherever the parent lands, satisfying the stable-
//! identity obligation of §2.

use std::ops::Range;

use crate::geometry::Rect;
use crate::layout::TileId;

// ── FloatRect ──────────────────────────────────────────────────────────────────

/// A floating child's placement, in **parent-local** cell coordinates.
///
/// `x`/`y` are offsets from the parent's top-left corner; `width`/`height` are
/// the child's extent.  The placement is resolved to an absolute, parent-clipped
/// [`Rect`] by [`FloatLayer::solve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FloatRect {
    /// Columns from the parent's left edge to the child's left edge.
    pub x: u16,
    /// Rows from the parent's top edge to the child's top edge.
    pub y: u16,
    /// Child width in columns.
    pub width: u16,
    /// Child height in rows.
    pub height: u16,
}

impl FloatRect {
    /// Construct a parent-local placement from a corner and a size.
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }
}

// ── FloatChild ─────────────────────────────────────────────────────────────────

/// One floating child: a stable [`TileId`] paired with its parent-local
/// placement.
///
/// The `id` is caller-assigned and durable exactly like a tiling-leaf id — it is
/// the same `id` across re-solves, so focus/scroll/zoom state keyed on it
/// survives layout churn (§2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatChild {
    /// Stable identifier for this float, chosen by the caller.
    pub id: TileId,
    /// Where the float sits inside its parent, in parent-local cells.
    pub place: FloatRect,
}

impl FloatChild {
    /// Construct a floating child from its id and placement.
    pub fn new(id: TileId, place: FloatRect) -> Self {
        Self { id, place }
    }
}

// ── FloatLayer ─────────────────────────────────────────────────────────────────

/// An ordered set of floating children belonging to one parent rect.
///
/// The layer owns only *placements*; absolute rects are computed on demand by
/// [`solve`](FloatLayer::solve) against whatever parent rect the tiling solver
/// hands back this frame.  Re-solving never mutates the placements, so the
/// declared layout is stable across frames.
///
/// Order is preserved: earlier children are drawn first (lower in the stack) and
/// the free-space queries subtract every child regardless of order, so order
/// matters only for paint/hit-test priority, not for free space.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FloatLayer {
    /// The floating children, in back-to-front paint order.
    pub children: Vec<FloatChild>,
}

impl FloatLayer {
    /// Construct an empty layer.
    pub fn new() -> Self {
        Self { children: Vec::new() }
    }

    /// Append a floating child and return `self` for builder-style chaining.
    pub fn with_child(mut self, child: FloatChild) -> Self {
        self.children.push(child);
        self
    }

    /// Resolve every floating child to an absolute, parent-clipped [`Rect`].
    ///
    /// Each parent-local [`FloatRect`] is translated by the parent's origin and
    /// then intersected with the parent, so a float that pokes past the parent's
    /// border is clipped to the visible portion and a float entirely outside the
    /// parent yields an empty rect.  Translation uses saturating arithmetic so a
    /// placement near `u16::MAX` never wraps.
    ///
    /// # Returns
    /// `(TileId, Rect)` pairs in declaration (back-to-front) order — the same
    /// shape [`solve`](crate::layout::solve) returns for tiling leaves, so the
    /// two can be concatenated and drawn through one path.
    ///
    /// # Invariants
    /// Every returned rect is contained within `parent`.
    pub fn solve(&self, parent: Rect) -> Vec<(TileId, Rect)> {
        self.children
            .iter()
            .map(|c| {
                // Parent-local → absolute, saturating so positions near u16::MAX
                // do not wrap, then clip to the parent so off-edge floats are
                // trimmed to what is actually visible.
                let abs = Rect::new(
                    parent.x.saturating_add(c.place.x),
                    parent.y.saturating_add(c.place.y),
                    c.place.width,
                    c.place.height,
                );
                (c.id, abs.intersection(parent))
            })
            .collect()
    }
}

// ── FreeInterval ───────────────────────────────────────────────────────────────

/// A maximal run of free cells on a single row: the half-open column span
/// `[start, end)` at row `row`, in absolute coordinates.
///
/// Produced by [`free_intervals_in_rows`].  "Free" means inside the parent and
/// outside every floating child (after the child is grown by the query's
/// gutter).  Intervals on a row are returned left-to-right and never touch or
/// overlap; an empty span is never emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeInterval {
    /// Absolute row this interval lies on.
    pub row: u16,
    /// First free column (inclusive).
    pub start: u16,
    /// One past the last free column (exclusive).  Always `> start`.
    pub end: u16,
}

impl FreeInterval {
    /// Width of the interval in columns (`end - start`).
    pub fn width(self) -> u16 {
        self.end - self.start
    }
}

// ── Free-space queries ─────────────────────────────────────────────────────────

/// Grow `r` by `gutter` cells on every side, then clamp to `bounds`.
///
/// Used to turn a floating child's solved rect into the obstacle region that
/// free space must avoid: a `gutter`-wide margin is kept clear around the child
/// (the runaround gutter of §3.5).  The grown rect is clamped to `bounds` so the
/// margin never reports cells outside the parent as blocked.  Saturating
/// arithmetic keeps growth safe at the `u16` extremes.  Returns an empty rect
/// when the child does not intersect `bounds` at all.
fn inflate_clamped(r: Rect, gutter: u16, bounds: Rect) -> Rect {
    let x0 = r.x.saturating_sub(gutter).max(bounds.x);
    let y0 = r.y.saturating_sub(gutter).max(bounds.y);
    let x1 = r.right().saturating_add(gutter).min(bounds.right());
    let y1 = r.bottom().saturating_add(gutter).min(bounds.bottom());
    if x1 <= x0 || y1 <= y0 {
        Rect::default()
    } else {
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// Compute, for a single row, the free column spans within `[lo, hi)` left after
/// removing the `blocked` spans.
///
/// `blocked` is consumed (sorted in place).  Overlapping or abutting blocked
/// spans merge naturally because the cursor only ever advances.  Each blocked
/// span is first clamped to `[lo, hi)` so obstacles wider than the row
/// contribute no spurious gap.  Returns the complement as left-to-right
/// `(start, end)` pairs, every one non-empty.
fn row_complement(lo: u16, hi: u16, blocked: &mut [(u16, u16)]) -> Vec<(u16, u16)> {
    blocked.sort_unstable();
    let mut out = Vec::new();
    // `cursor` is the first column not yet known to be blocked.
    let mut cursor = lo;
    for &(a, b) in blocked.iter() {
        // Clamp the obstacle to the row's own span before differencing.
        let a = a.max(lo);
        let b = b.min(hi);
        if b <= a {
            continue; // obstacle lies entirely outside [lo, hi)
        }
        if a > cursor {
            out.push((cursor, a)); // free gap before this obstacle
        }
        cursor = cursor.max(b); // jump past the obstacle (merges overlaps)
    }
    if cursor < hi {
        out.push((cursor, hi)); // trailing free gap after the last obstacle
    }
    out
}

/// Per-row free intervals — the **text-engine view** of free space (§3.5).
///
/// For every row in `rows` that also lies within `parent`, subtract each
/// floating child's rect (first grown by `gutter` via [`inflate_clamped`]) from
/// the parent's column span and emit the leftover intervals.  The result is the
/// ordered "slot" geometry the future word-wrap engine flows tokens into; the
/// obstacle-free row degenerates to a single full-width interval, so one code
/// path serves both flat text and runaround.
///
/// # Parameters
/// - `parent`: the area the floats live in (absolute coordinates).
/// - `children`: the floats' **already-solved** absolute rects (typically the
///   output of [`FloatLayer::solve`]); the query is agnostic to how they were
///   placed.
/// - `gutter`: cells of clearance kept around every child.  `0` means intervals
///   may butt directly against a child.
/// - `rows`: the **viewport-bounded** range of absolute rows to report on; rows
///   outside `parent` are skipped, so callers may pass any range cheaply.
///
/// # Returns
/// Intervals grouped by ascending row, left-to-right within a row.  A fully
/// blocked row contributes no interval.
///
/// # Invariants
/// Every emitted interval lies within `parent` and overlaps no child grown by
/// `gutter`; intervals on a row are disjoint and non-empty.
///
/// # Examples
/// ```
/// use mullion::{Rect, float::free_intervals_in_rows};
///
/// // 10×3 parent with a 4-wide float at columns [3, 7) on every row.
/// let parent = Rect::new(0, 0, 10, 3);
/// let child = Rect::new(3, 0, 4, 3);
/// let intervals = free_intervals_in_rows(parent, &[child], 0, 0..3);
/// // Each row splits into a left slot [0,3) and a right slot [7,10).
/// assert_eq!(intervals.len(), 6);
/// assert_eq!((intervals[0].start, intervals[0].end), (0, 3));
/// assert_eq!((intervals[1].start, intervals[1].end), (7, 10));
/// ```
pub fn free_intervals_in_rows(
    parent: Rect,
    children: &[Rect],
    gutter: u16,
    rows: Range<u16>,
) -> Vec<FreeInterval> {
    let mut out = Vec::new();
    if parent.is_empty() {
        return out;
    }

    // Grow obstacles once up front; their row coverage and column spans are then
    // reused for every scanned row.
    let obstacles: Vec<Rect> = children
        .iter()
        .map(|&c| inflate_clamped(c, gutter, parent))
        .filter(|r| !r.is_empty())
        .collect();

    // Restrict the requested rows to those actually inside the parent — this is
    // the viewport bound that keeps the scan cheap.
    let first = rows.start.max(parent.y);
    let last = rows.end.min(parent.bottom());

    let mut blocked: Vec<(u16, u16)> = Vec::new();
    for row in first..last {
        blocked.clear();
        // An obstacle blocks this row only if the row falls within its vertical
        // span; record just its column extent.
        for o in &obstacles {
            if row >= o.y && row < o.bottom() {
                blocked.push((o.x, o.right()));
            }
        }
        for (start, end) in row_complement(parent.x, parent.right(), &mut blocked) {
            out.push(FreeInterval { row, start, end });
        }
    }
    out
}

/// Free cells over a window — the **router view** of free space (§5.2).
///
/// Enumerates every free cell inside `window` (clipped to `parent`): a cell is
/// free when it lies in the parent and outside every floating child grown by
/// `gutter`.  This is the routing grid the future orthogonal-connector router
/// runs A\* over; routers typically pass `gutter = 0` so a connector may run
/// flush against a node.
///
/// The enumeration reuses [`free_intervals_in_rows`] internally so the two views
/// derive from identical geometry and can never disagree about obstacle extent.
///
/// # Parameters
/// - `parent`, `children`, `gutter`: as in [`free_intervals_in_rows`].
/// - `window`: the **viewport-bounded** region to enumerate; only the
///   intersection of `window` and `parent` is scanned, so the cost is bounded by
///   the window area regardless of how large the canvas is.
///
/// # Returns
/// `(column, row)` pairs in row-major order (top-to-bottom, left-to-right).
///
/// # Invariants
/// Every returned cell lies within both `parent` and `window` and inside no
/// child grown by `gutter`.
pub fn free_cells_in_window(
    parent: Rect,
    children: &[Rect],
    gutter: u16,
    window: Rect,
) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    // Clip the request to the parent up front; an empty intersection means there
    // is nothing to enumerate.
    let area = parent.intersection(window);
    if area.is_empty() {
        return out;
    }

    // Reuse the per-row interval view, then expand each interval — clipped to the
    // window's column span — into its individual cells.
    let intervals = free_intervals_in_rows(parent, children, gutter, area.y..area.bottom());
    for iv in intervals {
        let start = iv.start.max(area.x);
        let end = iv.end.min(area.right());
        for col in start..end {
            out.push((col, iv.row));
        }
    }
    out
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── FloatLayer::solve ─────────────────────────────────────────────────

    #[test]
    fn solve_translates_to_absolute() {
        // Parent offset to (10, 5); a float at parent-local (2, 1) lands at
        // absolute (12, 6).
        let parent = Rect::new(10, 5, 20, 10);
        let layer = FloatLayer::new().with_child(FloatChild::new(1, FloatRect::new(2, 1, 4, 3)));
        let rects = layer.solve(parent);
        assert_eq!(rects, vec![(1, Rect::new(12, 6, 4, 3))]);
    }

    #[test]
    fn solve_clips_to_parent() {
        // A float that pokes past the parent's right/bottom edge is clipped.
        let parent = Rect::new(0, 0, 10, 10);
        let layer = FloatLayer::new().with_child(FloatChild::new(7, FloatRect::new(8, 8, 5, 5)));
        let rects = layer.solve(parent);
        // [8,13)×[8,13) ∩ [0,10)×[0,10) = [8,10)×[8,10) → 2×2.
        assert_eq!(rects, vec![(7, Rect::new(8, 8, 2, 2))]);
    }

    #[test]
    fn solve_offscreen_float_is_empty() {
        let parent = Rect::new(0, 0, 10, 10);
        let layer = FloatLayer::new().with_child(FloatChild::new(1, FloatRect::new(20, 20, 4, 4)));
        let rects = layer.solve(parent);
        assert!(rects[0].1.is_empty());
    }

    // ── free_intervals_in_rows ────────────────────────────────────────────

    #[test]
    fn empty_children_yield_one_full_interval_per_row() {
        let parent = Rect::new(0, 0, 10, 3);
        let intervals = free_intervals_in_rows(parent, &[], 0, 0..3);
        assert_eq!(intervals.len(), 3);
        for (i, iv) in intervals.iter().enumerate() {
            assert_eq!(iv.row, i as u16);
            assert_eq!((iv.start, iv.end), (0, 10));
        }
    }

    #[test]
    fn single_float_splits_row_into_two_slots() {
        let parent = Rect::new(0, 0, 10, 1);
        let child = Rect::new(3, 0, 4, 1); // blocks columns [3, 7)
        let intervals = free_intervals_in_rows(parent, &[child], 0, 0..1);
        assert_eq!(intervals.len(), 2);
        assert_eq!((intervals[0].start, intervals[0].end), (0, 3));
        assert_eq!((intervals[1].start, intervals[1].end), (7, 10));
    }

    #[test]
    fn float_against_left_edge_yields_only_right_slot() {
        let parent = Rect::new(0, 0, 10, 1);
        let child = Rect::new(0, 0, 4, 1); // flush with the left edge
        let intervals = free_intervals_in_rows(parent, &[child], 0, 0..1);
        assert_eq!(intervals.len(), 1);
        assert_eq!((intervals[0].start, intervals[0].end), (4, 10));
    }

    #[test]
    fn gutter_widens_the_obstacle() {
        let parent = Rect::new(0, 0, 12, 1);
        let child = Rect::new(5, 0, 2, 1); // blocks [5, 7)
        // gutter=1 grows the obstacle to [4, 8).
        let intervals = free_intervals_in_rows(parent, &[child], 1, 0..1);
        assert_eq!(intervals.len(), 2);
        assert_eq!((intervals[0].start, intervals[0].end), (0, 4));
        assert_eq!((intervals[1].start, intervals[1].end), (8, 12));
    }

    #[test]
    fn overlapping_floats_merge() {
        let parent = Rect::new(0, 0, 10, 1);
        // Two overlapping obstacles [2,5) and [4,7) cover [2,7) together.
        let a = Rect::new(2, 0, 3, 1);
        let b = Rect::new(4, 0, 3, 1);
        let intervals = free_intervals_in_rows(parent, &[a, b], 0, 0..1);
        assert_eq!(intervals.len(), 2);
        assert_eq!((intervals[0].start, intervals[0].end), (0, 2));
        assert_eq!((intervals[1].start, intervals[1].end), (7, 10));
    }

    #[test]
    fn fully_blocked_row_yields_nothing() {
        let parent = Rect::new(0, 0, 6, 1);
        let child = Rect::new(0, 0, 6, 1); // covers the whole row
        let intervals = free_intervals_in_rows(parent, &[child], 0, 0..1);
        assert!(intervals.is_empty());
    }

    #[test]
    fn rows_outside_parent_are_skipped() {
        let parent = Rect::new(0, 5, 4, 2); // rows 5 and 6 only
        // Ask about rows 0..10; only 5 and 6 should appear.
        let intervals = free_intervals_in_rows(parent, &[], 0, 0..10);
        let got_rows: Vec<u16> = intervals.iter().map(|iv| iv.row).collect();
        assert_eq!(got_rows, vec![5, 6]);
    }

    #[test]
    fn float_blocks_only_its_own_rows() {
        let parent = Rect::new(0, 0, 8, 3);
        let child = Rect::new(2, 1, 3, 1); // only row 1 is blocked
        let intervals = free_intervals_in_rows(parent, &[child], 0, 0..3);
        // Row 0: full; row 1: split; row 2: full.
        let row0: Vec<_> = intervals.iter().filter(|iv| iv.row == 0).collect();
        let row1: Vec<_> = intervals.iter().filter(|iv| iv.row == 1).collect();
        assert_eq!(row0.len(), 1);
        assert_eq!(row1.len(), 2);
    }

    // ── free_cells_in_window ──────────────────────────────────────────────

    #[test]
    fn cells_empty_children_fill_window() {
        let parent = Rect::new(0, 0, 4, 2);
        let cells = free_cells_in_window(parent, &[], 0, parent);
        assert_eq!(cells.len(), 8); // 4×2 all free
        assert!(cells.contains(&(0, 0)));
        assert!(cells.contains(&(3, 1)));
    }

    #[test]
    fn cells_exclude_floats() {
        let parent = Rect::new(0, 0, 4, 2);
        let child = Rect::new(1, 0, 2, 2); // blocks columns 1 and 2
        let cells = free_cells_in_window(parent, &[child], 0, parent);
        // Only columns 0 and 3 survive, across both rows.
        assert_eq!(cells.len(), 4);
        for (x, _) in &cells {
            assert!(*x == 0 || *x == 3);
        }
    }

    #[test]
    fn cells_clipped_to_window() {
        let parent = Rect::new(0, 0, 10, 10);
        // Window is a 2×2 patch; only those four cells may be reported.
        let window = Rect::new(4, 4, 2, 2);
        let cells = free_cells_in_window(parent, &[], 0, window);
        assert_eq!(cells.len(), 4);
        for (x, y) in &cells {
            assert!((4..6).contains(x) && (4..6).contains(y));
        }
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    /// A strategy producing a parent rect and a handful of floats inside it.
    fn parent_and_floats() -> impl Strategy<Value = (Rect, Vec<Rect>)> {
        (4u16..40, 4u16..40).prop_flat_map(|(w, h)| {
            let parent = Rect::new(0, 0, w, h);
            // Each float is a rect somewhere in/around the parent.
            let float = (0u16..w, 0u16..h, 1u16..w, 1u16..h)
                .prop_map(move |(x, y, cw, ch)| Rect::new(x, y, cw, ch));
            proptest::collection::vec(float, 0..4).prop_map(move |floats| (parent, floats))
        })
    }

    proptest! {
        /// Free intervals always lie within the parent and never overlap any
        /// child grown by the gutter; intervals on a row are disjoint.
        #[test]
        fn prop_intervals_within_parent_and_clear_of_floats(
            (parent, floats) in parent_and_floats(),
            gutter in 0u16..3,
        ) {
            let intervals =
                free_intervals_in_rows(parent, &floats, gutter, parent.y..parent.bottom());

            // Pre-grow obstacles the same way the query does, to check clearance.
            let obstacles: Vec<Rect> = floats
                .iter()
                .map(|&c| inflate_clamped(c, gutter, parent))
                .collect();

            for iv in &intervals {
                // Containment: the interval lies on a parent row and within the
                // parent's column span.
                prop_assert!(iv.row >= parent.y && iv.row < parent.bottom());
                prop_assert!(iv.start >= parent.x && iv.end <= parent.right());
                prop_assert!(iv.end > iv.start);

                // Clearance: no cell of the interval is inside any grown float.
                for col in iv.start..iv.end {
                    for o in &obstacles {
                        prop_assert!(!o.contains(col, iv.row),
                            "free cell ({col},{}) overlaps obstacle {o:?}", iv.row);
                    }
                }
            }

            // Disjointness: per row, intervals are sorted and non-touching.
            for row in parent.y..parent.bottom() {
                let mut spans: Vec<_> =
                    intervals.iter().filter(|iv| iv.row == row).collect();
                spans.sort_by_key(|iv| iv.start);
                for w in spans.windows(2) {
                    prop_assert!(w[0].end < w[1].start,
                        "intervals {:?} and {:?} touch or overlap", w[0], w[1]);
                }
            }
        }

        /// With no floats, every parent row reports exactly one full-width
        /// interval.
        #[test]
        fn prop_empty_floats_one_full_interval(
            w in 1u16..50, h in 1u16..50, gutter in 0u16..3,
        ) {
            let parent = Rect::new(0, 0, w, h);
            let intervals =
                free_intervals_in_rows(parent, &[], gutter, parent.y..parent.bottom());
            prop_assert_eq!(intervals.len(), h as usize);
            for iv in &intervals {
                prop_assert_eq!((iv.start, iv.end), (0, w));
            }
        }

        /// Every enumerated free cell lies inside the parent and outside every
        /// grown float — the router never gets an occupied cell.
        #[test]
        fn prop_cells_clear_of_floats(
            (parent, floats) in parent_and_floats(),
            gutter in 0u16..3,
        ) {
            let cells = free_cells_in_window(parent, &floats, gutter, parent);
            let obstacles: Vec<Rect> = floats
                .iter()
                .map(|&c| inflate_clamped(c, gutter, parent))
                .collect();
            for (x, y) in &cells {
                prop_assert!(parent.contains(*x, *y));
                for o in &obstacles {
                    prop_assert!(!o.contains(*x, *y),
                        "router cell ({x},{y}) overlaps obstacle {o:?}");
                }
            }
        }

        /// `solve` always clips floats into the parent.
        #[test]
        fn prop_solve_within_parent(
            px in 0u16..30, py in 0u16..30, pw in 1u16..40, ph in 1u16..40,
            fx in 0u16..60, fy in 0u16..60, fw in 1u16..40, fh in 1u16..40,
        ) {
            let parent = Rect::new(px, py, pw, ph);
            let layer = FloatLayer::new()
                .with_child(FloatChild::new(1, FloatRect::new(fx, fy, fw, fh)));
            let rects = layer.solve(parent);
            let (_, r) = rects[0];
            // Either empty (fully off-parent) or fully contained.
            if !r.is_empty() {
                prop_assert!(r.x >= parent.x && r.right() <= parent.right());
                prop_assert!(r.y >= parent.y && r.bottom() <= parent.bottom());
            }
        }
    }
}
