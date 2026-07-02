// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
/// A rectangle in terminal cell coordinates.
///
/// The coordinate system places the origin `(0, 0)` at the top-left corner of
/// the terminal.  Both axes increase downward and rightward.  All values are in
/// **terminal cell units** (columns for x/width, rows for y/height).
///
/// `Rect` is used as both a region descriptor (the area a buffer covers) and a
/// clipping/intersection primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// Column of the left edge (inclusive).
    pub x: u16,
    /// Row of the top edge (inclusive).
    pub y: u16,
    /// Number of columns.
    pub width: u16,
    /// Number of rows.
    pub height: u16,
}

impl Rect {
    /// Construct a `Rect` from its top-left corner and dimensions.
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    /// Return the total number of cells in the rectangle.
    ///
    /// Widened to `u32` to avoid overflow for large terminals (a 65535×65535
    /// rect would overflow `u16`).
    pub fn area(self) -> u32 {
        u32::from(self.width) * u32::from(self.height)
    }

    /// Return the column one past the right edge (exclusive right bound).
    ///
    /// `saturating_add` is used so that a rect at the maximum `u16` position
    /// never wraps around to 0.
    pub fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Return the row one past the bottom edge (exclusive bottom bound).
    ///
    /// `saturating_add` is used for the same overflow-safety reason as `right`.
    pub fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Return `true` if the rectangle has no cells.
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Return `true` if the cell at `(x, y)` lies within this rectangle.
    ///
    /// The bounds are `[self.x, self.right())` and `[self.y, self.bottom())`,
    /// i.e. the right and bottom edges are exclusive.
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Return the number of cells on the border perimeter.
    ///
    /// The border visits every cell on the outer ring of the rectangle exactly
    /// once, clockwise from the top-left corner.  For a rectangle with `width`
    /// W and `height` H, the count is `2*(W+H) - 4`; the four corner cells
    /// are shared between two edges but counted only once in the clockwise
    /// walk.
    ///
    /// Returns `self.area()` for rectangles smaller than 2×2 (where there is
    /// no distinct interior to distinguish from the border).
    pub fn border_len(self) -> u32 {
        if self.width < 2 || self.height < 2 {
            return self.area();
        }
        2 * (u32::from(self.width) + u32::from(self.height)) - 4
    }

    /// Return the normalised position `s ∈ [0, 1)` of cell `(x, y)` on the
    /// clockwise border perimeter, starting from the top-left corner.
    ///
    /// The walk order is:
    /// - **Top edge** (`y == self.y`): left → right, s = 0 … W/(2W+2H-4).
    /// - **Right edge** (`x == self.right()-1`): top → bottom.
    /// - **Bottom edge** (`y == self.bottom()-1`): right → left.
    /// - **Left edge** (`x == self.x`): bottom → top.
    ///
    /// Each corner belongs to the edge that first reaches it in the clockwise
    /// walk (so the top-left corner is at `s = 0`, top-right is on the top
    /// edge, bottom-right is on the right edge, and bottom-left is on the
    /// bottom edge).  Interior cells, and cells outside the rectangle, return
    /// `0.0` (same as the top-left corner — callers that need to distinguish
    /// them should check [`Rect::contains`] first).
    ///
    /// # Use case
    ///
    /// Feed the result into [`ease::gaussian`](crate::ease::gaussian) or a
    /// sinusoid to animate a smooth, wrap-around effect on a box border — for
    /// example a colour bump that travels continuously around the rectangle
    /// without a visible seam at the starting corner.
    ///
    /// ```
    /// use mullion::Rect;
    ///
    /// let r = Rect::new(0, 0, 5, 4); // 5 wide, 4 tall; border_len = 14
    /// assert_eq!(r.border_pos(0, 0), 0.0 / 14.0);  // top-left  (top edge, s=0)
    /// assert_eq!(r.border_pos(4, 0), 4.0 / 14.0);  // top-right (top edge, s=4)
    /// assert_eq!(r.border_pos(4, 3), 7.0 / 14.0);  // bot-right (right edge, s=4+3=7)
    /// assert_eq!(r.border_pos(0, 3), 11.0 / 14.0); // bot-left  (bottom edge, s=4+3+4=11)
    /// ```
    pub fn border_pos(self, x: u16, y: u16) -> f32 {
        if self.width < 2 || self.height < 2 {
            return 0.0;
        }
        let bx0 = self.x;
        let by0 = self.y;
        let bx1 = self.x + self.width - 1;
        let by1 = self.y + self.height - 1;
        let w = u32::from(self.width - 1);
        let h = u32::from(self.height - 1);
        let perim = (2 * (w + h)) as f32;

        let s: u32 = if y == by0 {
            u32::from(x.saturating_sub(bx0))          // top →
        } else if x == bx1 {
            w + u32::from(y.saturating_sub(by0))      // right ↓
        } else if y == by1 {
            w + h + u32::from(bx1.saturating_sub(x)) // bottom ←
        } else if x == bx0 {
            2 * w + h + u32::from(by1.saturating_sub(y)) // left ↑
        } else {
            return 0.0; // interior or outside
        };

        s as f32 / perim
    }

    /// Border cells, clockwise from the top-left corner, with no duplicates — the
    /// discrete-cell companion to [`border_pos`](Rect::border_pos)/[`border_len`](Rect::border_len).
    ///
    /// The walk is the exact order [`border_pos`](Rect::border_pos) parameterises:
    /// top edge left→right, right edge top→bottom, bottom edge right→left, left edge
    /// bottom→top, visiting each of the `2·(W+H) − 4` ring cells once.  Feeding each
    /// cell back through [`border_pos`](Rect::border_pos) recovers its position on the
    /// perimeter, so the canonical travelling-glow loop is:
    ///
    /// ```
    /// use mullion::Rect;
    /// let r = Rect::new(0, 0, 6, 4);
    /// for (x, y) in r.border_cells() {
    ///     let s = r.border_pos(x, y); // 0.0 ..< 1.0 around the ring
    ///     let _ = s;
    /// }
    /// ```
    ///
    /// The yielded count equals [`border_len`](Rect::border_len): for a degenerate
    /// rect (`width < 2` or `height < 2`, which has no distinct interior) the iterator
    /// yields the rect's straight run of cells in row-major order rather than an empty
    /// sequence, so `border_cells().count() == border_len()` holds for every rect.
    pub fn border_cells(self) -> impl Iterator<Item = (u16, u16)> {
        let mut cells = Vec::new();
        if self.width == 0 || self.height == 0 {
            return cells.into_iter();
        }
        let (x0, y0) = (self.x, self.y);
        let (x1, y1) = (self.right() - 1, self.bottom() - 1); // inclusive far corners
        if self.width == 1 || self.height == 1 {
            // Degenerate: no corners — just the straight run, matching `border_len`.
            for y in y0..=y1 {
                for x in x0..=x1 {
                    cells.push((x, y));
                }
            }
            return cells.into_iter();
        }
        for x in x0..=x1 {
            cells.push((x, y0)); // top edge, left → right
        }
        for y in (y0 + 1)..=y1 {
            cells.push((x1, y)); // right edge, top → bottom
        }
        for x in (x0..x1).rev() {
            cells.push((x, y1)); // bottom edge, right → left
        }
        for y in ((y0 + 1)..y1).rev() {
            cells.push((x0, y)); // left edge, bottom → top
        }
        cells.into_iter()
    }

    /// Return the largest `Rect` that fits within both `self` and `other`.
    ///
    /// Computes the overlap by taking the maximum of the two left/top edges and
    /// the minimum of the two right/bottom edges.  Returns `Rect::default()`
    /// (zero-sized, at the origin) when the two rectangles are adjacent or
    /// non-overlapping — callers should check `is_empty()` before using the
    /// result.
    pub fn intersection(self, other: Rect) -> Rect {
        // The inner corners of the potential overlap region.
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = self.right().min(other.right());
        let y2 = self.bottom().min(other.bottom());
        // If the right edge did not extend past the left edge (or bottom past
        // top), the rectangles do not overlap.
        if x2 <= x1 || y2 <= y1 {
            Rect::default()
        } else {
            Rect { x: x1, y: y1, width: x2 - x1, height: y2 - y1 }
        }
    }
}

/// Slide a viewport of `viewport` slots over `len` items so that `cursor` stays
/// visible, returning the visible half-open index range `[offset, end)`.
///
/// `offset` is **caller-owned** scroll state, mutated in place: the app keeps the
/// cursor and the offset, mullion only does the keep-in-view arithmetic every list
/// screen and horizontally-scrolling field repeats.  The offset moves the minimum
/// amount needed — down to `cursor` when the cursor sits above the window, up so the
/// cursor is the last visible slot when it sits below — then is clamped so the
/// window never scrolls past the end (`offset ≤ len.saturating_sub(viewport)`).
///
/// Indices are abstract slots: rows for a list, display columns for a line editor
/// (pass `len = columns + 1` so the cursor can rest one past the last column).
///
/// Returns an empty range when `viewport == 0` or `len == 0`.  Never panics.
///
/// ```
/// use mullion::visible_window;
/// let mut offset = 0;
/// // 100 rows, a 10-row viewport, cursor on row 42 → window slides to show it.
/// let win = visible_window(42, &mut offset, 100, 10);
/// assert_eq!(offset, 33);
/// assert_eq!(win, 33..43);
/// ```
pub fn visible_window(
    cursor: usize,
    offset: &mut usize,
    len: usize,
    viewport: usize,
) -> std::ops::Range<usize> {
    if viewport == 0 || len == 0 {
        *offset = 0;
        return 0..0;
    }
    // Bring the cursor into view with the smallest move.
    if cursor < *offset {
        *offset = cursor;
    } else if cursor >= *offset + viewport {
        *offset = cursor + 1 - viewport;
    }
    // Never leave blank space at the end when there is content to show it.
    let max_offset = len.saturating_sub(viewport);
    if *offset > max_offset {
        *offset = max_offset;
    }
    let start = *offset;
    let end = (start + viewport).min(len);
    start..end
}

/// Reflect each rect horizontally about the vertical centre axis of `area`, in
/// place — mirroring column order (a [`ColumnGrid`](crate::table::ColumnGrid)) or
/// pane order (a solved `Split` row) for a right-to-left layout (§round-2 A5).
///
/// Each rect keeps its width and vertical extent; only `x` is flipped, so a rect
/// flush to `area`'s left edge lands flush to its right edge. Rects are assumed to
/// lie within `area`'s horizontal span; anything outside is clamped by saturation.
pub fn mirror_rects_in(area: Rect, rects: &mut [Rect]) {
    for r in rects.iter_mut() {
        let offset_from_left = r.x.saturating_sub(area.x);
        let inner_right = offset_from_left.saturating_add(r.width);
        r.x = area.x + area.width.saturating_sub(inner_right);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_rects_flips_x_and_preserves_width() {
        let area = Rect::new(0, 0, 30, 1);
        // Three 10-wide columns at x=0,10,20.
        let mut rects = [Rect::new(0, 0, 10, 1), Rect::new(10, 0, 10, 1), Rect::new(20, 0, 10, 1)];
        mirror_rects_in(area, &mut rects);
        assert_eq!(rects[0].x, 20); // leftmost → rightmost
        assert_eq!(rects[1].x, 10); // centre stays
        assert_eq!(rects[2].x, 0);  // rightmost → leftmost
        assert!(rects.iter().all(|r| r.width == 10));
        // Non-zero area origin is respected.
        let area2 = Rect::new(5, 0, 20, 1);
        let mut r = [Rect::new(5, 0, 6, 1)];
        mirror_rects_in(area2, &mut r);
        assert_eq!(r[0].x, 5 + 20 - 6); // flush-left → flush-right
    }

    #[test]
    fn area() {
        assert_eq!(Rect::new(0, 0, 10, 5).area(), 50);
        assert_eq!(Rect::new(0, 0, 0, 5).area(), 0);
    }

    #[test]
    fn contains() {
        let r = Rect::new(2, 3, 4, 5);
        assert!(r.contains(2, 3));
        assert!(r.contains(5, 7));
        assert!(!r.contains(6, 7)); // x == right()
        assert!(!r.contains(5, 8)); // y == bottom()
        assert!(!r.contains(1, 3));
    }

    #[test]
    fn intersection_overlap() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersection(b), Rect::new(5, 5, 5, 5));
    }

    #[test]
    fn intersection_no_overlap() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(10, 10, 5, 5);
        assert!(a.intersection(b).is_empty());
    }

    #[test]
    fn intersection_adjacent() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(5, 0, 5, 5);
        assert!(a.intersection(b).is_empty());
    }

    #[test]
    fn border_cells_walk_clockwise_without_repeats() {
        // 4×3 box → 2·(4+3) − 4 = 10 ring cells, clockwise from the top-left.
        let r = Rect::new(0, 0, 4, 3);
        let cells: Vec<_> = r.border_cells().collect();
        assert_eq!(
            cells,
            vec![
                (0, 0), (1, 0), (2, 0), (3, 0), // top → right
                (3, 1), (3, 2),                 // right edge down
                (2, 2), (1, 2), (0, 2),         // bottom ← left
                (0, 1),                         // left edge up
            ]
        );
        // Count matches border_len, and every cell is distinct.
        assert_eq!(cells.len() as u32, r.border_len());
        let mut uniq = cells.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), cells.len());
    }

    #[test]
    fn border_cells_agree_with_border_pos_order() {
        // The k-th yielded cell sits at perimeter position k / border_len.
        let r = Rect::new(2, 1, 6, 5);
        let len = r.border_len() as f32;
        for (k, (x, y)) in r.border_cells().enumerate() {
            assert!((r.border_pos(x, y) - k as f32 / len).abs() < 1e-6);
        }
    }

    #[test]
    fn border_cells_degenerate_is_straight_run() {
        // A single row/column has no corners: yield its cells, count == border_len.
        let row = Rect::new(2, 5, 4, 1);
        assert_eq!(row.border_cells().collect::<Vec<_>>(), vec![(2, 5), (3, 5), (4, 5), (5, 5)]);
        assert_eq!(row.border_cells().count() as u32, row.border_len());
        let col = Rect::new(2, 5, 1, 3);
        assert_eq!(col.border_cells().collect::<Vec<_>>(), vec![(2, 5), (2, 6), (2, 7)]);
        // Empty rect yields nothing.
        assert_eq!(Rect::new(0, 0, 0, 4).border_cells().count(), 0);
    }

    #[test]
    fn visible_window_keeps_cursor_in_view() {
        let mut off = 0;
        // Cursor within the first window — no scroll.
        assert_eq!(visible_window(3, &mut off, 100, 10), 0..10);
        assert_eq!(off, 0);
        // Cursor past the window bottom — slide so cursor is the last visible slot.
        assert_eq!(visible_window(12, &mut off, 100, 10), 3..13);
        assert_eq!(off, 3);
        // Cursor above the window top — slide up to the cursor.
        assert_eq!(visible_window(1, &mut off, 100, 10), 1..11);
        assert_eq!(off, 1);
    }

    #[test]
    fn visible_window_clamps_to_end_and_degenerates() {
        let mut off = 90;
        // Few items, large stale offset → clamp so no blank tail.
        assert_eq!(visible_window(2, &mut off, 5, 10), 0..5);
        assert_eq!(off, 0);
        // Zero viewport or empty length → empty range, offset reset.
        let mut o2 = 7;
        assert_eq!(visible_window(0, &mut o2, 10, 0), 0..0);
        assert_eq!(o2, 0);
        assert_eq!(visible_window(0, &mut o2, 0, 5), 0..0);
    }
}
