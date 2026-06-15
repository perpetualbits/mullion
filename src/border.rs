//! Border drawing: glyphs, [`draw_box`], and per-tile framing.
//!
//! ## Glyph vocabulary
//!
//! Three line weights are supported, each with a horizontal line, a vertical
//! line, and four corner glyphs.  Rounded corners are only available for
//! `Light` weight; requesting them with `Heavy` or `Double` silently falls back
//! to square corners of the same weight.
//!
//! ```text
//! Light  (square):  ─ │   ┌ ┐ └ ┘
//! Light  (rounded): ─ │   ╭ ╮ ╰ ╯
//! Heavy:            ━ ┃   ┏ ┓ ┗ ┛
//! Double:           ═ ║   ╔ ╗ ╚ ╝
//! ```
//!
//! Tee/cross glyphs (`├ ┤ ┬ ┴ ┼` and mixed-weight variants) are Phase 2b.
//!
//! ## Per-tile vs. shared-border mode
//!
//! [`frame_tiles`] is the **per-tile mode**: every tile gets its own box.
//! Adjacent tiles produce a doubled gutter (`┐┌` / `││` / `┘└`).  A single
//! shared line with junction glyphs is Phase 2b.
//!
//! To draw one box around a group of tiles, call [`draw_box`] on the group's
//! bounding rect directly.
//!
//! ## Note for Phase 3
//!
//! Phase 3 focus highlighting will want to draw a tile's box in a different
//! [`BorderStyle`] (e.g. heavier weight or accent colour) than its neighbours.
//! Because [`draw_box`] and [`frame_tiles`] already accept the style per call,
//! a focus pass can re-draw the focused tile's box over the base frame without
//! any API change.

use bitflags::bitflags;

use crate::{
    buffer::Buffer,
    geometry::Rect,
    layout::TileId,
    style::Style,
};

// ── LineWeight ────────────────────────────────────────────────────────────────

/// Thickness of the lines used in a border.
///
/// Rounded corners are only available for [`Light`](LineWeight::Light).  When
/// [`CornerStyle::Rounded`] is paired with [`Heavy`](LineWeight::Heavy) or
/// [`Double`](LineWeight::Double), the corner glyphs fall back to the square
/// variant of the same weight.  Line glyphs are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineWeight {
    /// Single thin lines (`─` `│`).
    Light,
    /// Single thick lines (`━` `┃`).
    Heavy,
    /// Double thin lines (`═` `║`).
    Double,
}

// ── CornerStyle ───────────────────────────────────────────────────────────────

/// Whether to use curved or right-angle corners.
///
/// Only honoured for [`LineWeight::Light`].  Other weights fall back to square
/// corners of the same weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CornerStyle {
    /// Right-angle corners (`┌ ┐ └ ┘` / `┏ ┓ ┗ ┛` / `╔ ╗ ╚ ╝`).
    Square,
    /// Curved corners (`╭ ╮ ╰ ╯`).  Falls back to `Square` for non-`Light` weights.
    Rounded,
}

// ── Borders ───────────────────────────────────────────────────────────────────

bitflags! {
    /// Which sides of a box to draw.
    ///
    /// Combine flags with `|` — e.g. `Borders::TOP | Borders::LEFT`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Borders: u8 {
        const TOP    = 0b0001;
        const BOTTOM = 0b0010;
        const LEFT   = 0b0100;
        const RIGHT  = 0b1000;
        /// Shorthand for all four sides.
        const ALL    = Self::TOP.bits() | Self::BOTTOM.bits()
                     | Self::LEFT.bits() | Self::RIGHT.bits();
    }
}

// ── BorderStyle ───────────────────────────────────────────────────────────────

/// Combined description of how a border looks.
pub struct BorderStyle {
    /// Thickness of the lines.
    pub weight: LineWeight,
    /// Square or rounded corners.
    pub corners: CornerStyle,
    /// Colour and text attributes applied to every border glyph.
    pub style: Style,
}

// ── Glyph lookup ──────────────────────────────────────────────────────────────

/// Return the `(h_line, v_line, top_left, top_right, bot_left, bot_right)` glyph
/// set for the given weight and corner style.
///
/// `Rounded` corners are silently coerced to square for `Heavy` and `Double`
/// weight, because no curved variants exist for those weights.
pub(crate) fn border_glyphs(
    weight: &LineWeight,
    corners: &CornerStyle,
) -> (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str) {
    match (weight, corners) {
        // Light + square corners
        (LineWeight::Light, CornerStyle::Square)  => ("─", "│", "┌", "┐", "└", "┘"),
        // Light + rounded corners (╭ ╮ ╰ ╯ are only defined for light weight)
        (LineWeight::Light, CornerStyle::Rounded) => ("─", "│", "╭", "╮", "╰", "╯"),
        // Heavy: Rounded falls back to square heavy (no curved heavy corners exist)
        (LineWeight::Heavy, _)                    => ("━", "┃", "┏", "┓", "┗", "┛"),
        // Double: Rounded falls back to square double (no curved double corners exist)
        (LineWeight::Double, _)                   => ("═", "║", "╔", "╗", "╚", "╝"),
    }
}

// ── draw_box ──────────────────────────────────────────────────────────────────

/// Draw the requested border sides of `area` into `buf` using `style`.
///
/// Only the border cells are written; the interior is left untouched.  All box
/// glyphs occupy exactly one terminal column.
///
/// ## Corner logic
///
/// At each corner cell the glyph depends on which of the two meeting sides are
/// requested:
/// - Both sides → corner glyph (`┌`, `┐`, `└`, `┘`, …).
/// - Horizontal side only → `h_line` (`─` / `━` / `═`).
/// - Vertical side only → `v_line` (`│` / `┃` / `║`).
/// - Neither → nothing written.
///
/// ## Degenerate areas
///
/// - `width == 0` or `height == 0`: nothing drawn.
/// - `width == 1`: only vertical glyphs are possible; a full-height `v_line`
///   is drawn if `LEFT` or `RIGHT` is set (corners are impossible).
/// - `height == 1`: only horizontal glyphs are possible; a full-width `h_line`
///   is drawn if `TOP` or `BOTTOM` is set.
/// - `area` is clipped to `buf.area` before drawing, so oversized rects are
///   safe.
pub fn draw_box(buf: &mut Buffer, area: Rect, borders: Borders, style: &BorderStyle) {
    // Clip to buffer; a zero-area intersection means nothing to draw.
    let area = buf.area.intersection(area);
    if area.width == 0 || area.height == 0 {
        return;
    }

    let (h_line, v_line, tl, tr, bl, br) = border_glyphs(&style.weight, &style.corners);
    let st = style.style;

    let top    = borders.contains(Borders::TOP);
    let bottom = borders.contains(Borders::BOTTOM);
    let left   = borders.contains(Borders::LEFT);
    let right  = borders.contains(Borders::RIGHT);

    // Inclusive column/row indices of the four edges.
    let x0 = area.x;
    let x1 = area.x + area.width - 1;  // equals x0 when width == 1
    let y0 = area.y;
    let y1 = area.y + area.height - 1; // equals y0 when height == 1

    // ── Degenerate: single column ────────────────────────────────────────────
    if area.width == 1 {
        // No horizontal span is available; draw only a vertical line.
        if left || right {
            for y in y0..=y1 {
                buf.set_grapheme(x0, y, v_line, st);
            }
        }
        return;
    }

    // ── Degenerate: single row ───────────────────────────────────────────────
    if area.height == 1 {
        // No vertical span is available; draw only a horizontal line.
        if top || bottom {
            for x in x0..=x1 {
                buf.set_grapheme(x, y0, h_line, st);
            }
        }
        return;
    }

    // ── General case: width >= 2 and height >= 2 ─────────────────────────────

    // Select the glyph for a corner cell given the two meeting sides.
    // `h_side` is the horizontal border (top/bottom), `v_side` the vertical
    // one (left/right).  Both present → corner; one → its line; neither → skip.
    macro_rules! corner_glyph {
        ($h_side:expr, $v_side:expr, $corner:expr) => {
            if $h_side && $v_side {
                Some($corner)
            } else if $h_side {
                Some(h_line)
            } else if $v_side {
                Some(v_line)
            } else {
                None
            }
        };
    }

    // Top-left corner
    if let Some(g) = corner_glyph!(top, left, tl) {
        buf.set_grapheme(x0, y0, g, st);
    }
    // Top row middle (columns between the two corners)
    if top {
        for x in (x0 + 1)..x1 {
            buf.set_grapheme(x, y0, h_line, st);
        }
    }
    // Top-right corner
    if let Some(g) = corner_glyph!(top, right, tr) {
        buf.set_grapheme(x1, y0, g, st);
    }

    // Left and right column middles (rows between the two corner rows)
    for y in (y0 + 1)..y1 {
        if left  { buf.set_grapheme(x0, y, v_line, st); }
        if right { buf.set_grapheme(x1, y, v_line, st); }
    }

    // Bottom-left corner
    if let Some(g) = corner_glyph!(bottom, left, bl) {
        buf.set_grapheme(x0, y1, g, st);
    }
    // Bottom row middle
    if bottom {
        for x in (x0 + 1)..x1 {
            buf.set_grapheme(x, y1, h_line, st);
        }
    }
    // Bottom-right corner
    if let Some(g) = corner_glyph!(bottom, right, br) {
        buf.set_grapheme(x1, y1, g, st);
    }
}

// ── frame_tiles ───────────────────────────────────────────────────────────────

/// Frame each solved leaf rect with a box and return the interior content rect.
///
/// For each `(TileId, Rect)` pair in `tiles`:
/// 1. Calls [`draw_box`] to draw the border into `buf`.
/// 2. Computes the *interior* rect by deflating the original rect by 1 on each
///    bordered side (saturating, so an under-sized tile yields a zero-area
///    interior).
///
/// Returns a `Vec` of `(TileId, interior_rect)` in the same order as `tiles`.
///
/// ## Degenerate tiles
///
/// A tile too small to have a usable interior (e.g. a 1×1 tile framed with
/// `Borders::ALL`) yields a content rect with zero width or height.  The caller
/// should check [`Rect::is_empty`] before rendering into the content rect.
///
/// ## Phase 2 border mode
///
/// This is the **per-tile mode**: each tile gets its own box.  Adjacent tiles
/// produce a doubled gutter (`┐┌` / `││` / `┘└`).  A single shared border line
/// with junction glyphs (`├ ┤ ┬ ┴ ┼`) is Phase 2b.
///
/// To draw one box around a *group* of tiles, call [`draw_box`] on the bounding
/// rect of the group instead.
///
/// // NOTE: Phase 3 focus highlighting will draw the focused tile's box in a
/// // different [`BorderStyle`] (e.g. [`LineWeight::Heavy`] or an accent colour).
/// // Because [`draw_box`] and [`frame_tiles`] already accept the style per call,
/// // the focus pass re-draws that one box over the base frame — no API change
/// // anticipated.
pub fn frame_tiles(
    buf: &mut Buffer,
    tiles: &[(TileId, Rect)],
    borders: Borders,
    style: &BorderStyle,
) -> Vec<(TileId, Rect)> {
    // Deflation amounts: 1 when the corresponding border is active, else 0.
    // Cast bool → u16 (true=1, false=0) avoids branches.
    let dl = borders.contains(Borders::LEFT)   as u16;
    let dt = borders.contains(Borders::TOP)    as u16;
    let dr = borders.contains(Borders::RIGHT)  as u16;
    let db = borders.contains(Borders::BOTTOM) as u16;

    tiles.iter().map(|&(id, rect)| {
        draw_box(buf, rect, borders, style);

        // Shrink the rect inward on each bordered side to get the interior.
        // Saturating arithmetic ensures we never underflow for tiny tiles.
        let x = rect.x.saturating_add(dl);
        let y = rect.y.saturating_add(dt);
        let w = rect.width.saturating_sub(dl + dr);
        let h = rect.height.saturating_sub(dt + db);

        (id, Rect::new(x, y, w, h))
    }).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_sets_are_correct() {
        // Light/Square
        assert_eq!(
            border_glyphs(&LineWeight::Light, &CornerStyle::Square),
            ("─", "│", "┌", "┐", "└", "┘")
        );
        // Light/Rounded
        assert_eq!(
            border_glyphs(&LineWeight::Light, &CornerStyle::Rounded),
            ("─", "│", "╭", "╮", "╰", "╯")
        );
        // Heavy/Square
        assert_eq!(
            border_glyphs(&LineWeight::Heavy, &CornerStyle::Square),
            ("━", "┃", "┏", "┓", "┗", "┛")
        );
        // Heavy/Rounded → square heavy fallback (no curved heavy corners exist)
        assert_eq!(
            border_glyphs(&LineWeight::Heavy, &CornerStyle::Rounded),
            ("━", "┃", "┏", "┓", "┗", "┛"),
            "Rounded+Heavy must fall back to square heavy corners"
        );
        // Double/Square
        assert_eq!(
            border_glyphs(&LineWeight::Double, &CornerStyle::Square),
            ("═", "║", "╔", "╗", "╚", "╝")
        );
        // Double/Rounded → square double fallback
        assert_eq!(
            border_glyphs(&LineWeight::Double, &CornerStyle::Rounded),
            ("═", "║", "╔", "╗", "╚", "╝"),
            "Rounded+Double must fall back to square double corners"
        );
    }
}
