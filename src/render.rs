// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Smooth-scroll carousel renderer.
//!
//! [`render_carousel`] renders a [`Node::Carousel`] into an on-screen buffer
//! using a **temp-buffer + blit** strategy: visible children are painted at
//! their full size into a temporary buffer, then the scrolled window is blitted
//! into the on-screen viewport.  Children that straddle the viewport edge are
//! genuinely **cut off** (their top or bottom border is missing), not shrunk.

use crate::{
    buffer::Buffer,
    geometry::Rect,
    layout::{Axis, Node, TileId, carousel_visible_entries, solve},
};

/// Render a `Carousel` node into `buf` at `viewport` with smooth scrolling.
///
/// ## Algorithm
///
/// 1. Resolve orientation (updating `Adaptive`'s `last` field for hysteresis).
/// 2. Clamp `scroll` in place and find the children that intersect the viewport.
/// 3. Allocate a **temp `Buffer`** whose main-axis extent spans from the first
///    visible child's start to the last visible child's end (in content space),
///    and whose cross extent equals the viewport's cross dimension.
/// 4. For each visible child: solve its layout within its full-size rect in the
///    temp buffer, then call `draw_child(temp, id, rect)` for every resulting
///    leaf.
/// 5. **Blit** the window `[scroll − first, scroll − first + main_extent)` from
///    temp into `buf` at the viewport origin.  Because children are painted at
///    full size, the cut at either viewport edge is produced by the blit —
///    borders are absent where the content was cropped, not shrunk.
///
/// ## Parameters
/// - `carousel`: must be a `Node::Carousel`; any other variant is a no-op.
/// - `viewport`: the on-screen region to fill; must be within `buf.area`.
/// - `draw_child`: called once per leaf with `(buf, id, rect)` in temp-buffer
///   coordinates.  It should paint the tile's border and interior into `buf`
///   at `rect`.
///
/// ## Degenerate inputs
/// A zero-size viewport, an empty carousel, or all children off-screen never
/// panics and leaves `buf` unchanged.
pub fn render_carousel(
    buf: &mut Buffer,
    carousel: &mut Node,
    viewport: Rect,
    draw_child: &mut dyn FnMut(&mut Buffer, TileId, Rect),
) {
    let (orientation, scroll, children) = match carousel {
        Node::Carousel { orientation, scroll, children, .. } => (orientation, scroll, children),
        _ => return,
    };

    if viewport.is_empty() || children.is_empty() {
        return;
    }

    let axis = orientation.resolve(viewport);
    let (main_extent, cross_extent) = match axis {
        Axis::Horizontal => (viewport.width, viewport.height),
        Axis::Vertical => (viewport.height, viewport.width),
    };
    if main_extent == 0 {
        return;
    }

    // Collect extents as a plain slice so the shared helper can borrow them
    // independently from `children` (allowing the later mutable child access).
    let extents: Vec<u16> = children.iter().map(|(e, _)| *e).collect();
    let (clamped, entries) = carousel_visible_entries(&extents, *scroll, main_extent);
    *scroll = clamped;

    if entries.is_empty() {
        return;
    }

    // Content-space span of the visible children.
    let first_v: u32 = entries[0].1;
    let (_, last_v_start, last_ext) = *entries.last().unwrap();
    let last_end_v: u32 = last_v_start + last_ext as u32;
    // Cap temp_main at u16::MAX (pathological; in practice ≈ viewport + 2 items).
    let temp_main = (last_end_v - first_v).min(u16::MAX as u32) as u16;

    let temp_area = match axis {
        Axis::Horizontal => Rect::new(0, 0, temp_main, cross_extent),
        Axis::Vertical   => Rect::new(0, 0, cross_extent, temp_main),
    };
    let mut temp = Buffer::empty(temp_area);

    // Paint every visible child into the temp buffer at its full main-axis extent.
    for (child_idx, v_start, ext) in &entries {
        // The child's origin within the temp buffer (measured from first_v).
        let origin_in_temp = (v_start - first_v) as u16;
        let child_rect = match axis {
            Axis::Horizontal => Rect::new(origin_in_temp, 0, *ext, cross_extent),
            Axis::Vertical   => Rect::new(0, origin_in_temp, cross_extent, *ext),
        };
        // Solve the child's internal layout and invoke draw_child for each leaf.
        let leaves = solve(&mut children[*child_idx].1, child_rect);
        for (id, leaf_rect) in leaves {
            draw_child(&mut temp, id, leaf_rect);
        }
    }

    // Blit the viewport window from temp into buf.
    // scroll_in_temp: offset within temp where the visible window starts.
    let scroll_in_temp = (clamped as u32 - first_v) as u16; // safe: first_v <= clamped
    // When content is shorter than the viewport, temp_main < main_extent; only
    // blit the rows that exist — the remainder of the viewport is left as-is.
    let blit_main = main_extent.min(temp_main.saturating_sub(scroll_in_temp));
    if blit_main == 0 {
        return;
    }
    let (src_x, src_y, src_w, src_h) = match axis {
        Axis::Horizontal => (scroll_in_temp, 0u16, blit_main, cross_extent),
        Axis::Vertical   => (0u16, scroll_in_temp, cross_extent, blit_main),
    };
    buf.blit(&temp, Rect::new(src_x, src_y, src_w, src_h), viewport.x, viewport.y);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        border::{draw_box, BorderStyle, Borders, CornerStyle, LineWeight},
        layout::Orientation,
        style::Style,
    };

    /// Shared draw_child: draw a full box border around each leaf's rect.
    fn box_draw_child(buf: &mut Buffer, _id: TileId, rect: Rect) {
        let style = BorderStyle {
            weight: LineWeight::Light,
            corners: CornerStyle::Square,
            style: Style::default(),
        };
        draw_box(buf, rect, Borders::ALL, &style);
    }

    /// 3 children × 5 rows each; `Vertical` carousel.
    fn vert_carousel(scroll: u16) -> Node {
        Node::Carousel {
            id: 0,
            orientation: Orientation::Vertical,
            scroll,
            children: (0u64..3).map(|i| (5u16, Node::Tile(i))).collect(),
        }
    }

    #[test]
    fn render_carousel_scroll0_top_is_border() {
        // At scroll=0 the viewport's first row must be the first child's top border.
        let viewport = Rect::new(0, 0, 10, 7);
        let mut carousel = vert_carousel(0);
        let mut buf = Buffer::empty(viewport);
        render_carousel(&mut buf, &mut carousel, viewport, &mut box_draw_child);
        // Column 0 of row 0 is the top-left corner of child 0's box.
        assert_eq!(buf.get(0, 0).symbol, "┌",
            "at scroll=0 the top border must be visible");
    }

    #[test]
    fn render_carousel_scroll2_top_border_cut() {
        // At scroll=2 the first child's top 2 rows are scrolled past —
        // the viewport's first row shows the child's interior, not its border.
        //
        // A 10×5 box with ALL borders has:
        //   row 0: ┌──────────┐  (top border)
        //   row 1: │          │  (interior)
        //   row 2: │          │  (interior)
        //   ...
        //   row 4: └──────────┘  (bottom border)
        //
        // At scroll=2, viewport row 0 = child row 2 = left-border cell │.
        let viewport = Rect::new(0, 0, 10, 7);
        let mut carousel = vert_carousel(2);
        let mut buf = Buffer::empty(viewport);
        render_carousel(&mut buf, &mut carousel, viewport, &mut box_draw_child);
        let top_left = buf.get(0, 0).symbol.clone();
        assert_ne!(top_left, "┌", "at scroll=2 the top border must be cut");
        assert_eq!(top_left, "│", "row 2 of the first child is an interior row");
    }

    #[test]
    fn render_carousel_bottom_partial_child_cut() {
        // viewport height=7; 3 children × 5 rows = 15 total.
        // At scroll=0: content [0,7). Child 0 [0,5) fully visible; child 1 [5,10)
        // partially visible (rows 0-1 only → top border + one interior row).
        // The viewport's last row (row 6) must show child 1's interior, not its
        // bottom border (which is at row 4 of child 1 — off screen).
        let viewport = Rect::new(0, 0, 10, 7);
        let mut carousel = vert_carousel(0);
        let mut buf = Buffer::empty(viewport);
        render_carousel(&mut buf, &mut carousel, viewport, &mut box_draw_child);
        // Row 5 = child 1 row 0 = top border (┌).
        assert_eq!(buf.get(0, 5).symbol, "┌");
        // Row 6 = child 1 row 1 = interior left border (│), not bottom border.
        assert_eq!(buf.get(0, 6).symbol, "│",
            "partial bottom child must be cut, not show its bottom border");
    }

    #[test]
    fn render_carousel_scroll_past_end_clamped() {
        // total=15, viewport_h=7, max_scroll=8. scroll=9999 → clamped to 8.
        // At scroll=8: content [8,15). All 7 rows must be covered (no blank gap).
        let viewport = Rect::new(0, 0, 10, 7);
        let mut carousel = vert_carousel(9999);
        let mut buf = Buffer::empty(viewport);
        render_carousel(&mut buf, &mut carousel, viewport, &mut box_draw_child);
        // Every row in the viewport must be non-default (a border or interior cell).
        for row in viewport.y..viewport.bottom() {
            let sym = &buf.get(0, row).symbol;
            assert_ne!(sym, " ",
                "row {row} must not be blank after clamping");
        }
        // scroll field must have been clamped.
        if let Node::Carousel { scroll, .. } = &carousel {
            assert_eq!(*scroll, 8, "scroll must be clamped to max_scroll=8");
        }
    }

    #[test]
    fn render_carousel_empty_no_panic() {
        let mut carousel = Node::Carousel {
            id: 0,
            orientation: Orientation::Vertical,
            scroll: 0,
            children: vec![],
        };
        let viewport = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(viewport);
        render_carousel(&mut buf, &mut carousel, viewport, &mut box_draw_child);
        // Buffer untouched.
        assert_eq!(buf.get(0, 0).symbol, " ");
    }

    #[test]
    fn render_carousel_non_carousel_node_is_noop() {
        let mut node = Node::Tile(42);
        let viewport = Rect::new(0, 0, 5, 5);
        let mut buf = Buffer::empty(viewport);
        render_carousel(&mut buf, &mut node, viewport, &mut box_draw_child);
        assert_eq!(buf.get(0, 0).symbol, " ");
    }
}
