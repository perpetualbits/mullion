// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Border label drawing: text overlaid on tile border edges after junctions are
//! resolved.
//!
//! [`draw_label`] is a **post-pass**: it overwrites only the border-line cells
//! in the *run* (the edge between the two corner cells), so corner glyphs and
//! junction glyphs produced by [`render_shared`](crate::border::render_shared)
//! or [`frame_tiles`](crate::border::frame_tiles) are never touched.  Call it
//! once per label, after the frame pass.
//!
//! ## Horizontal labels (`Top` / `Bottom`)
//!
//! Text is measured in **display columns** (grapheme clusters +
//! `unicode-width`; wide graphemes count as 2).  When the text fits the run it
//! is positioned by [`Align`]; when it overflows a **marquee** window scrolls
//! left as the caller advances [`Label::offset`] each render tick.  A wide
//! grapheme that straddles the window edge is replaced with spaces rather than
//! rendered as a half-glyph.
//!
//! ## Vertical labels (`Left` / `Right`)
//!
//! One grapheme per row, reading **top → bottom** — upright-stacked, not
//! rotated.  Unicode has no 90°-rotated Latin letters, and terminals do not
//! implement UAX #50 text orientation.  Only **width-1** graphemes are drawn;
//! a width-2 grapheme is **skipped** (the row is not consumed) because a
//! 1-cell-wide border column cannot hold a 2-column glyph.  Vertical labels
//! are intended for short ASCII identifiers such as `MB/s` or `CPU`.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{buffer::Buffer, geometry::Rect, style::Style};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Blank columns/rows inserted between the end of marquee text and its next
/// repetition, giving a visible pause between cycles.
const MARQUEE_GAP: u16 = 3;

// ── Public types ──────────────────────────────────────────────────────────────

/// Which edge of a tile's outer box rect to draw the label on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Top border row.
    Top,
    /// Bottom border row.
    Bottom,
    /// Left border column; label reads top → bottom.
    Left,
    /// Right border column; label reads top → bottom.
    Right,
}

/// Alignment of the label within the run when the text fits without scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Flush with the near end of the run (left for H, top for V).
    Start,
    /// Centred in the run; odd remainders round toward `Start`.
    Center,
    /// Flush with the far end of the run (right for H, bottom for V).
    End,
}

/// A *physical* horizontal anchor, after resolving a logical [`Align`] against a
/// base direction (§round-2 A5). `Start`/`End` are leading/trailing edges that
/// flip under RTL; `Anchor` is the concrete left/centre/right they resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Left,
    Center,
    Right,
}

impl Align {
    /// Resolve this logical alignment to a physical [`Anchor`] for `base`.
    ///
    /// Under LTR: `Start → Left`, `End → Right`. Under RTL these flip
    /// (`Start → Right`, `End → Left`). `Center` is invariant. `Auto` is treated
    /// as LTR (a caller that has resolved auto-direction passes the concrete base).
    pub fn resolve(self, base: crate::text::BaseDirection) -> Anchor {
        use crate::text::BaseDirection::Rtl;
        match (self, base) {
            (Align::Center, _)      => Anchor::Center,
            (Align::Start, Rtl)     => Anchor::Right,
            (Align::Start, _)       => Anchor::Left,
            (Align::End,   Rtl)     => Anchor::Left,
            (Align::End,   _)       => Anchor::Right,
        }
    }
}

/// A text label to draw on a tile's border edge.
pub struct Label {
    /// The text to display.  For vertical labels only width-1 graphemes are
    /// rendered; width-2 graphemes are silently skipped.
    pub text: String,
    /// Which border edge to place the label on.
    pub side: Side,
    /// How to align the text when it fits the run; ignored in marquee mode.
    pub align: Align,
    /// Marquee scroll position: number of display columns (H) or rows (V)
    /// scrolled from the text start.  Advance by 1 each render tick and let
    /// [`draw_label`] wrap it via modulo — or pre-compute `offset % period`
    /// from [`label_period`].  `0` is the unscrolled position.
    pub offset: u16,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Return the marquee period for `text` on a run of `run_len` cells, or `None`
/// when the text fits and no scrolling is needed.
///
/// For horizontal sides the extent is measured in display columns; for vertical
/// sides it is the count of width-1 graphemes (wide graphemes are skipped, so
/// they contribute 0 to the count).  When the result is `Some(p)` the caller
/// should advance `Label::offset` by 1 each tick; [`draw_label`] wraps it via
/// `offset % p` internally.
///
/// # Returns
/// `None` if `text_extent ≤ run_len`; otherwise `Some(text_extent +
/// MARQUEE_GAP)` where `MARQUEE_GAP = 3`.
pub fn label_period(text: &str, run_len: u16, side: Side) -> Option<u16> {
    let extent = match side {
        Side::Top | Side::Bottom => h_display_cols(text),
        Side::Left | Side::Right => narrow_grapheme_count(text),
    };
    if extent <= run_len { None } else { Some(extent + MARQUEE_GAP) }
}

/// Draw `label` over the border of `rect` (the tile's full outer box rect).
///
/// The label occupies only the **run** of the target edge — the cells strictly
/// between the two corner cells.  For `Top`/`Bottom` the run spans columns
/// `rect.x+1 .. rect.x+rect.width-1` on the top/bottom row; for `Left`/`Right`
/// it spans rows `rect.y+1 .. rect.y+rect.height-1` on the left/right column.
/// Corner cells and all cells on other edges are untouched.
///
/// When the text fits the run it is placed according to [`Label::align`].  When
/// it overflows, a marquee window of width/height `run_len` is shown starting at
/// `label.offset % period`.  Wide graphemes that straddle a horizontal window
/// edge are blanked; width-2 graphemes on vertical edges are skipped (no row
/// consumed).
///
/// This function is a no-op when the text is empty or the run is empty (rect
/// smaller than 3 in the relevant dimension).  It never panics.
///
/// # Parameters
/// - `rect`: the tile's *outer* rect (including the border row/column), as
///   provided by the solve pass, not the deflated content rect.
pub fn draw_label(buf: &mut Buffer, rect: Rect, label: &Label, style: &Style) {
    if label.text.is_empty() {
        return;
    }
    match label.side {
        Side::Top | Side::Bottom => draw_h_label(buf, rect, label, style),
        Side::Left | Side::Right => draw_v_label(buf, rect, label, style),
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Total display columns occupied by `text` (sum of per-grapheme widths).
fn h_display_cols(text: &str) -> u16 {
    text.graphemes(true)
        .map(|g| UnicodeWidthStr::width(g) as u16)
        .sum()
}

/// Count of width-1 graphemes in `text`; the rendering unit for vertical labels.
fn narrow_grapheme_count(text: &str) -> u16 {
    text.graphemes(true)
        .filter(|g| UnicodeWidthStr::width(*g) == 1)
        .count() as u16
}

/// Render a `Top` or `Bottom` label into `buf`.
///
/// Computes the run coordinates, measures the text, then either aligns it (fits
/// case) or delegates to [`draw_h_window`] (marquee case).
fn draw_h_label(buf: &mut Buffer, rect: Rect, label: &Label, style: &Style) {
    if rect.width < 3 {
        // Run length ≤ 0 — nothing to draw.
        return;
    }
    let run_len = rect.width - 2;
    let run_x   = rect.x + 1; // skip left corner
    let run_y   = match label.side {
        Side::Top => rect.y,
        _         => rect.y + rect.height - 1,
    };

    // Collect grapheme clusters with display widths; discard zero-width clusters
    // (combining marks that arrived without a base character) since they would
    // produce no visible output and must not consume run space.
    let gws: Vec<(&str, u16)> = label.text
        .graphemes(true)
        .map(|g| (g, UnicodeWidthStr::width(g) as u16))
        .filter(|(_, w)| *w > 0)
        .collect();

    let text_cols: u16 = gws.iter().map(|(_, w)| w).sum();
    if text_cols == 0 {
        return;
    }

    if text_cols <= run_len {
        // Text fits: position by align, then write graphemes left to right.
        let label_x = run_x + match label.align {
            Align::Start  => 0,
            Align::End    => run_len - text_cols,
            Align::Center => (run_len - text_cols) / 2,
        };
        let mut x = label_x;
        for &(g, w) in &gws {
            if x + w > run_x + run_len {
                break; // guard: shouldn't trigger with correct alignment math
            }
            buf.set_grapheme(x, run_y, g, *style);
            x += w;
        }
    } else {
        // Text overflows: marquee window.
        let period = text_cols + MARQUEE_GAP;
        let start  = label.offset % period; // wrap large offsets
        draw_h_window(buf, run_x, run_y, run_len, &gws, start, style);
    }
}

/// Render a `Left` or `Right` label into `buf`.
///
/// Only width-1 graphemes are drawn, one per row.  Wide graphemes are skipped
/// without consuming a row, consistent with the spec's documented rule (a
/// 1-cell-wide border cannot hold a 2-column glyph).
fn draw_v_label(buf: &mut Buffer, rect: Rect, label: &Label, style: &Style) {
    if rect.height < 3 {
        // Run length ≤ 0 — nothing to draw.
        return;
    }
    let run_len = rect.height - 2;
    let run_y   = rect.y + 1; // skip top corner row
    let run_x   = match label.side {
        Side::Left => rect.x,
        _          => rect.x + rect.width - 1,
    };

    // Collect only width-1 graphemes; wide graphemes are skipped.
    let narrow: Vec<&str> = label.text
        .graphemes(true)
        .filter(|g| UnicodeWidthStr::width(*g) == 1)
        .collect();

    let count = narrow.len() as u16;
    if count == 0 {
        return;
    }

    if count <= run_len {
        // Text fits: position by align, then write one grapheme per row.
        let label_y = run_y + match label.align {
            Align::Start  => 0,
            Align::End    => run_len - count,
            Align::Center => (run_len - count) / 2,
        };
        for (k, &g) in narrow.iter().enumerate() {
            let y = label_y + k as u16;
            if y >= run_y + run_len {
                break; // guard
            }
            buf.set_grapheme(run_x, y, g, *style);
        }
    } else {
        // Marquee: virtual sequence is narrow graphemes followed by MARQUEE_GAP
        // blank rows, repeating.  Each row maps to one virtual slot.
        let period = count + MARQUEE_GAP;
        let start  = label.offset % period;

        // Build the per-slot virtual sequence for one period.
        let mut virtual_rows: Vec<&str> = narrow;
        // Append MARQUEE_GAP blank slots as the pause between repetitions.
        virtual_rows.extend(std::iter::repeat_n(" ", MARQUEE_GAP as usize));

        for k in 0..run_len {
            let slot = ((start + k) % period) as usize;
            buf.set_grapheme(run_x, run_y + k, virtual_rows[slot], *style);
        }
    }
}

/// Write a horizontal marquee window of `run_len` columns into `buf`.
///
/// The virtual content is `gws` (grapheme clusters from the label text)
/// followed by `MARQUEE_GAP` space characters, for a total width of `period`
/// columns.  The window starts at virtual column `start` (already reduced
/// modulo `period` by the caller) and wraps around as needed.
///
/// **Wide-grapheme edge rule:** when a width-2 grapheme straddles the left or
/// right edge of the window, the visible column(s) are filled with spaces
/// rather than rendering a fractional glyph.  This matches the rule used by
/// [`Buffer::blit`](crate::buffer::Buffer::blit).
///
/// # Parameters
/// - `gws`: grapheme–width pairs from the label text (gap spaces are added
///   internally; do not include them here).
/// - `start`: first virtual column to display; must be `< period` where
///   `period = sum(gws widths) + MARQUEE_GAP`.
fn draw_h_window(
    buf:     &mut Buffer,
    run_x:   u16,
    run_y:   u16,
    run_len: u16,
    gws:     &[(&str, u16)],
    start:   u16,
    style:   &Style,
) {
    // Build the complete item list for one period: text graphemes + gap spaces.
    let mut all: Vec<(&str, u16)> = gws.to_vec();
    for _ in 0..MARQUEE_GAP {
        all.push((" ", 1));
    }
    // Invariant: ∑ all widths == period (= ∑ gws widths + MARQUEE_GAP).

    // Find the item whose column range [col, col+w) contains `start`, and
    // record how many leading columns of that item are before the window.
    let mut col      = 0u16;
    let mut item_idx = 0usize;
    let mut skip     = 0u16; // columns of the first item that precede the window
    for (i, &(_, w)) in all.iter().enumerate() {
        if col + w > start {
            item_idx = i;
            skip = start - col; // 0 for width-1 items; 0 or 1 for width-2 items
            break;
        }
        col += w;
    }
    // Unreachable: `start < period` and `∑ widths == period` guarantee we break.

    let screen_end = run_x + run_len;
    let mut screen_x = run_x;
    let mut i        = item_idx;
    let mut first    = true; // the first item may be entered mid-grapheme

    while screen_x < screen_end {
        let &(g, w) = &all[i % all.len()];

        let s = if first { first = false; skip } else { 0 };

        if s > 0 {
            // The window starts inside this grapheme (can only happen for a
            // width-2 grapheme with s=1).  Blank the visible right portion.
            let visible = (w - s).min(screen_end - screen_x);
            for _ in 0..visible {
                buf.set_grapheme(screen_x, run_y, " ", *style);
                screen_x += 1;
            }
        } else if w <= screen_end - screen_x {
            // Entire grapheme fits within the remaining window.
            buf.set_grapheme(screen_x, run_y, g, *style);
            screen_x += w;
        } else {
            // Wide grapheme straddles the right edge: blank the columns that
            // would be occupied so no half-glyph appears.
            while screen_x < screen_end {
                buf.set_grapheme(screen_x, run_y, " ", *style);
                screen_x += 1;
            }
        }

        i += 1; // advance to next item; wraps via modulo on next access
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        border::{draw_box, render_shared, BorderStyle, Borders, CornerStyle, LineWeight},
        buffer::Buffer,
        geometry::Rect,
        layout::{Constraint, Node, Orientation, Size},
        style::Style,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn light_box_style() -> BorderStyle {
        BorderStyle {
            weight:  LineWeight::Light,
            corners: CornerStyle::Square,
            style:   Style::default(),
        }
    }

    /// Draw a light square box around `rect` in `buf`.
    fn frame(buf: &mut Buffer, rect: Rect) {
        draw_box(buf, rect, Borders::ALL, &light_box_style());
    }

    fn label(text: &str, side: Side, align: Align, offset: u16) -> Label {
        Label { text: text.into(), side, align, offset }
    }

    // ── Horizontal fit + alignment ────────────────────────────────────────────

    #[test]
    fn h_label_fit_start() {
        // Rect 8 wide: run = x=1..6 (6 cells).  "Hi" (2 cols) → x=1,2.
        let rect = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(&mut buf, rect, &label("Hi", Side::Top, Align::Start, 0), &Style::default());

        assert_eq!(buf.get(0, 0).symbol, "┌", "left corner untouched");
        assert_eq!(buf.get(1, 0).symbol, "H");
        assert_eq!(buf.get(2, 0).symbol, "i");
        assert_eq!(buf.get(3, 0).symbol, "─", "border glyph preserved after text");
        assert_eq!(buf.get(7, 0).symbol, "┐", "right corner untouched");
    }

    #[test]
    fn h_label_fit_center() {
        // Rect 10 wide: run = 8 cells.  "Hi" (2 cols), center → offset = (8-2)/2 = 3
        // → x=4, x=5.
        let rect = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(&mut buf, rect, &label("Hi", Side::Top, Align::Center, 0), &Style::default());

        assert_eq!(buf.get(0, 0).symbol, "┌");
        assert_eq!(buf.get(1, 0).symbol, "─"); // before label
        assert_eq!(buf.get(4, 0).symbol, "H");
        assert_eq!(buf.get(5, 0).symbol, "i");
        assert_eq!(buf.get(6, 0).symbol, "─"); // after label
        assert_eq!(buf.get(9, 0).symbol, "┐");
    }

    #[test]
    fn h_label_fit_end() {
        // Bottom edge, End align. Rect 8×3: run_len=6. "Hi" → x=5,6 on y=2.
        let rect = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(&mut buf, rect, &label("Hi", Side::Bottom, Align::End, 0), &Style::default());

        assert_eq!(buf.get(0, 2).symbol, "└");
        assert_eq!(buf.get(1, 2).symbol, "─");
        assert_eq!(buf.get(5, 2).symbol, "H");
        assert_eq!(buf.get(6, 2).symbol, "i");
        assert_eq!(buf.get(7, 2).symbol, "┘");
    }

    // ── Horizontal marquee ────────────────────────────────────────────────────

    #[test]
    fn h_label_marquee_windows() {
        // "ABCDE" (5 cols) on a run of 3 → marquee, period = 5+3 = 8.
        // Virtual: A B C D E _ _ _   (indices 0-7, _ = gap space)
        let rect = Rect::new(0, 0, 5, 3); // run_len = 3
        let do_draw = |offset: u16| -> [String; 3] {
            let mut buf = Buffer::empty(rect);
            frame(&mut buf, rect);
            draw_label(&mut buf, rect, &label("ABCDE", Side::Top, Align::Start, offset), &Style::default());
            [
                buf.get(1, 0).symbol.clone(),
                buf.get(2, 0).symbol.clone(),
                buf.get(3, 0).symbol.clone(),
            ]
        };

        assert_eq!(do_draw(0), ["A", "B", "C"], "offset 0: first three chars");
        assert_eq!(do_draw(1), ["B", "C", "D"], "offset 1: slide by one");
        assert_eq!(do_draw(4), ["E", " ", " "], "offset 4: last char + gap");
        assert_eq!(do_draw(5), [" ", " ", " "], "offset 5: all gap");
        // offset=8 wraps to 0: same as offset 0
        assert_eq!(do_draw(8), ["A", "B", "C"], "offset 8 wraps to 0");
    }

    #[test]
    fn h_label_marquee_wrap_boundary() {
        // "ABCDE" period=8, run=3. At offset=6 the window is [6,9) mod 8.
        // Virtual slots 6,7,0 → ' ',' ','A'.
        let rect = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(&mut buf, rect, &label("ABCDE", Side::Top, Align::Start, 6), &Style::default());

        assert_eq!(buf.get(1, 0).symbol, " ");
        assert_eq!(buf.get(2, 0).symbol, " ");
        assert_eq!(buf.get(3, 0).symbol, "A", "wraps from gap back to text start");
    }

    // ── label_period ─────────────────────────────────────────────────────────

    #[test]
    fn label_period_values() {
        // Fits → None.
        assert_eq!(label_period("Hi",    5, Side::Top),  None);   // 2 ≤ 5
        assert_eq!(label_period("Hello", 5, Side::Top),  None);   // 5 ≤ 5
        // Overflows → Some(cols + 3).
        assert_eq!(label_period("Hello!", 5, Side::Top),  Some(9)); // 6+3
        assert_eq!(label_period("ABCDE",  3, Side::Top),  Some(8)); // 5+3
        // Vertical: counts only narrow graphemes.
        assert_eq!(label_period("A\u{4e16}B", 3, Side::Left), None);    // 2 narrow ≤ 3
        assert_eq!(label_period("ABCD",        3, Side::Left), Some(7)); // 4+3
    }

    // ── Vertical upright stack ────────────────────────────────────────────────

    #[test]
    fn v_label_upright_stack() {
        // "ABC" on Left edge of a 3×5 rect: run = rows 1..3 (3 rows).
        // Start align: A@(0,1), B@(0,2), C@(0,3).  Corners and row 4 unchanged.
        let rect = Rect::new(0, 0, 3, 5);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(&mut buf, rect, &label("ABC", Side::Left, Align::Start, 0), &Style::default());

        assert_eq!(buf.get(0, 0).symbol, "┌", "top-left corner untouched");
        assert_eq!(buf.get(0, 1).symbol, "A", "upright A on row 1");
        assert_eq!(buf.get(0, 2).symbol, "B", "upright B on row 2");
        assert_eq!(buf.get(0, 3).symbol, "C", "upright C on row 3");
        assert_eq!(buf.get(0, 4).symbol, "└", "bottom-left corner untouched");
    }

    #[test]
    fn v_label_fit_center() {
        // "AB" on Right edge of a 3×6 rect: run_len=4, text_count=2.
        // Center: (4-2)/2 = 1 → rows 2,3 of the run (run starts at row 1).
        let rect = Rect::new(0, 0, 3, 6);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(&mut buf, rect, &label("AB", Side::Right, Align::Center, 0), &Style::default());

        let rx = rect.width - 1; // right column = 2
        assert_eq!(buf.get(rx, 0).symbol, "┐");
        assert_eq!(buf.get(rx, 1).symbol, "│"); // border preserved before label
        assert_eq!(buf.get(rx, 2).symbol, "A");
        assert_eq!(buf.get(rx, 3).symbol, "B");
        assert_eq!(buf.get(rx, 4).symbol, "│"); // border preserved after label
        assert_eq!(buf.get(rx, 5).symbol, "┘");
    }

    #[test]
    fn v_label_fit_end() {
        // "AB" on Left edge, End align, 3×6 rect: run_len=4.
        // End: (4-2) = 2 offset → rows 3,4.
        let rect = Rect::new(0, 0, 3, 6);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(&mut buf, rect, &label("AB", Side::Left, Align::End, 0), &Style::default());

        assert_eq!(buf.get(0, 1).symbol, "│"); // border before label
        assert_eq!(buf.get(0, 2).symbol, "│"); // border before label
        assert_eq!(buf.get(0, 3).symbol, "A");
        assert_eq!(buf.get(0, 4).symbol, "B");
        assert_eq!(buf.get(0, 5).symbol, "└");
    }

    // ── Vertical marquee ──────────────────────────────────────────────────────

    #[test]
    fn v_label_marquee() {
        // "ABCDE" (5 narrow chars) on Left, run_len=3 → marquee, period=8.
        // Virtual: A B C D E _ _ _  (rows 0-7, _ = blank)
        let rect = Rect::new(0, 0, 3, 5); // run_len = 3
        let do_draw = |offset: u16| -> [String; 3] {
            let mut buf = Buffer::empty(rect);
            frame(&mut buf, rect);
            draw_label(&mut buf, rect, &label("ABCDE", Side::Left, Align::Start, offset), &Style::default());
            [
                buf.get(0, 1).symbol.clone(),
                buf.get(0, 2).symbol.clone(),
                buf.get(0, 3).symbol.clone(),
            ]
        };

        assert_eq!(do_draw(0), ["A", "B", "C"]);
        assert_eq!(do_draw(2), ["C", "D", "E"]);
        assert_eq!(do_draw(5), [" ", " ", " "], "all gap");
        assert_eq!(do_draw(6), [" ", " ", "A"], "wraps: slots 6,7,0");
        assert_eq!(do_draw(8), ["A", "B", "C"], "period=8 wraps to 0");
    }

    // ── Wide-grapheme handling ────────────────────────────────────────────────

    #[test]
    fn wide_grapheme_h_right_edge_blanked() {
        // "A世" is 3 display cols.  run_len=2 → marquee (period=3+3=6).
        // At offset=0: window [0,2).  A(1) fits; 世(2) straddles right → blank.
        let rect = Rect::new(0, 0, 4, 3); // run_len = 2
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(
            &mut buf, rect,
            &label("A\u{4e16}", Side::Top, Align::Start, 0),
            &Style::default(),
        );

        assert_eq!(buf.get(1, 0).symbol, "A");
        assert_eq!(buf.get(2, 0).symbol, " ", "right half of 世 blanked, not rendered");
        assert_eq!(buf.get(0, 0).symbol, "┌");
        assert_eq!(buf.get(3, 0).symbol, "┐");
    }

    #[test]
    fn wide_grapheme_h_left_edge_blanked() {
        // "A世B" = 4 cols, run_len=3 → marquee (period=7).
        // At offset=2: window [2,5).  世 occupies cols 1-2; col 2 is the right
        // half → blank it.  Then B and a gap space fill the rest.
        let rect = Rect::new(0, 0, 5, 3); // run_len = 3
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(
            &mut buf, rect,
            &label("A\u{4e16}B", Side::Top, Align::Start, 2),
            &Style::default(),
        );

        assert_eq!(buf.get(1, 0).symbol, " ", "right half of 世 at left edge blanked");
        assert_eq!(buf.get(2, 0).symbol, "B");
        assert_eq!(buf.get(3, 0).symbol, " ", "gap space");
    }

    #[test]
    fn wide_grapheme_v_skipped() {
        // "A世B": 世 is width-2, skipped on vertical label → A, B on rows 1,2.
        // "AB" fits in run_len=3, Start align.
        let rect = Rect::new(0, 0, 3, 5);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(
            &mut buf, rect,
            &label("A\u{4e16}B", Side::Left, Align::Start, 0),
            &Style::default(),
        );

        assert_eq!(buf.get(0, 0).symbol, "┌");
        assert_eq!(buf.get(0, 1).symbol, "A", "A drawn at row 1");
        assert_eq!(buf.get(0, 2).symbol, "B", "B drawn at row 2 — 世 consumed no row");
        assert_eq!(buf.get(0, 3).symbol, "│", "border preserved where no text");
        assert_eq!(buf.get(0, 4).symbol, "└");
    }

    // ── Degenerate inputs ─────────────────────────────────────────────────────

    #[test]
    fn degenerate_rect_too_small_no_panic() {
        // Width/height < 3 → run is empty → no-op; must not panic.
        let tiny = Rect::new(0, 0, 2, 2);
        let mut buf = Buffer::empty(tiny);
        draw_label(&mut buf, tiny, &label("Hello", Side::Top,  Align::Start, 0), &Style::default());
        draw_label(&mut buf, tiny, &label("Hello", Side::Left, Align::Start, 0), &Style::default());
        // Buffer untouched (all spaces).
        assert_eq!(buf.get(0, 0).symbol, " ");
    }

    #[test]
    fn degenerate_empty_text_no_op() {
        let rect = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        let snapshot: Vec<String> = (0..10).map(|x| buf.get(x, 0).symbol.clone()).collect();
        draw_label(&mut buf, rect, &label("", Side::Top, Align::Start, 0), &Style::default());
        let after: Vec<String> = (0..10).map(|x| buf.get(x, 0).symbol.clone()).collect();
        assert_eq!(snapshot, after, "empty text must not modify the buffer");
    }

    #[test]
    fn degenerate_large_offset_no_panic() {
        // offset=99999 → wraps via modulo; must not panic or index out of bounds.
        let rect = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::empty(rect);
        frame(&mut buf, rect);
        draw_label(
            &mut buf, rect,
            &label("ABCDE", Side::Top, Align::Start, 9999),
            &Style::default(),
        );
        // 9999 % 8 = 7 → window starts at slot 7 (last gap space).
        // Slots 7, 0, 1 → ' ', 'A', 'B'.
        assert_eq!(buf.get(1, 0).symbol, " ");
        assert_eq!(buf.get(2, 0).symbol, "A");
        assert_eq!(buf.get(3, 0).symbol, "B");
    }

    // ── Composition: junctions and corners survive a label pass ───────────────

    #[test]
    fn composition_shared_border_junction_intact() {
        // 2×2 shared-border grid (4 tiles) in a 13×7 buffer.
        // Inner area: 11×5.  H-split divider at x=6, V-split divider at y=3.
        // ┼ junction at (6, 3).
        //
        // Draw a Top label on the outer rect of tile 0 (top-left).
        // The label runs on row 0, x=1..5 — far from the ┼ — so the junction
        // must be unchanged, proving draw_label never touches interior cells.
        let area = Rect::new(0, 0, 13, 7);
        let mut buf = Buffer::empty(area);

        let mut root = Node::Split {
            orientation: Orientation::Vertical,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Split {
                    orientation: Orientation::Horizontal,
                    children: vec![
                        (Constraint::new(Size::Fill(1)), Node::Tile(0)),
                        (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                    ],
                }),
                (Constraint::new(Size::Fill(1)), Node::Split {
                    orientation: Orientation::Horizontal,
                    children: vec![
                        (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                        (Constraint::new(Size::Fill(1)), Node::Tile(3)),
                    ],
                }),
            ],
        };
        render_shared(&mut buf, &mut root, area, &light_box_style(), &[]);

        // Verify the ┼ is where expected before the label pass.
        assert_eq!(buf.get(6, 3).symbol, "┼", "┼ must be present after render_shared");

        // Tile 0 occupies the top-left; its outer box in shared-border mode
        // runs from (0,0) to (6,3) inclusive → Rect::new(0, 0, 7, 4).
        let tile0_rect = Rect::new(0, 0, 7, 4);
        draw_label(
            &mut buf, tile0_rect,
            &label("Tile-0", Side::Top, Align::Center, 0),
            &Style::default(),
        );

        // Junction and corners must be unchanged.
        assert_eq!(buf.get(6, 3).symbol, "┼", "┼ untouched by label");
        assert_eq!(buf.get(0, 0).symbol, "┌", "top-left outer corner untouched");
        assert_eq!(buf.get(6, 0).symbol, "┬", "top ┬ junction untouched");
        assert_eq!(buf.get(0, 3).symbol, "├", "left ├ junction untouched");

        // The run cells at row 0, x=1..5 must have been written by the label.
        // "Tile-0" = 6 cols, run_len = 5 → marquee, period=9.
        // offset=0 → 'T','i','l','e','-' at x=1..5.
        assert_eq!(buf.get(1, 0).symbol, "T");
        assert_eq!(buf.get(2, 0).symbol, "i");
        assert_eq!(buf.get(3, 0).symbol, "l");
        assert_eq!(buf.get(4, 0).symbol, "e");
        assert_eq!(buf.get(5, 0).symbol, "-");
    }
}
