// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Word-wrap runaround: flow text around floating tiles by treating the free
//! space as a stream of slots (design note §3.5).
//!
//! ## The slot stream
//!
//! For each visible row, subtracting every floating child's rectangle (plus a
//! gutter) from the row's width leaves 1..n free intervals — "left of tile",
//! "right of tile", or both ([`crate::float::free_intervals_in_rows`]). Flattened
//! top-to-bottom (and left-to-right within a row), those intervals form an ordered
//! stream of [`Slot`]s. Wrapped tokens flow into *slots* instead of full-width
//! lines ([`crate::text::wrap_into_slots`]), so a tile carves a hole that the text
//! reads around.
//!
//! The obstacle-free case is simply "every row is one full-width slot", which
//! flows through the *same* path as flat wrapping — so [`flow`] with no obstacles
//! reproduces [`crate::text::wrap`] line for line. Reflow on a tile drag is bounded
//! by the rows you ask about, never the whole document.
//!
//! ## BiDi × runaround
//!
//! Per §3.5 this is a feature-multiplication zone, landed in two stages. Stage A
//! was LTR: slots within a row flow left-to-right. Stage B makes the within-row
//! slot order **direction-aware**: under an RTL base the reader starts at the
//! right-of-tile slot and moves left, so [`flow`] reverses the slot order within
//! each row (the rows themselves still run top-to-bottom). The bidi reordering
//! *inside* each slot is the Phase 2 machinery, unchanged — only the order in
//! which slots are filled flips.

use std::ops::Range;

use crate::buffer::Buffer;
use crate::float::free_intervals_in_rows;
use crate::geometry::Rect;
use crate::style::Style;
use crate::text::{render_line, wrap_into_slots, BaseDirection, VisualLine};

// ── Slot ─────────────────────────────────────────────────────────────────────

/// One free interval on a single row, used as a flow target: text placed here is
/// one visual line at `(col, row)`, at most `width` columns wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Absolute row.
    pub row: u16,
    /// Absolute column of the slot's left edge.
    pub col: u16,
    /// Slot width in columns (always ≥ 1).
    pub width: u16,
}

// ── PlacedLine ───────────────────────────────────────────────────────────────

/// A wrapped line positioned at the slot it was flowed into.
#[derive(Debug, Clone)]
pub struct PlacedLine {
    /// Absolute row to draw on.
    pub row: u16,
    /// Absolute column of the line's left edge.
    pub col: u16,
    /// Width of the slot (the line's content is never wider than this).
    pub width: u16,
    /// The visual line (cells in visual order), possibly empty.
    pub line: VisualLine,
}

// ── Slot stream ──────────────────────────────────────────────────────────────

/// Build the ordered slot stream for `rows` of `parent`, after subtracting the
/// `obstacles` (already-solved floating-child rects) grown by `gutter`.
///
/// Slots are returned in geometric order: top-to-bottom, then **left-to-right**
/// within a row. This is the LTR flow order; [`flow`] reverses the within-row
/// order itself for an RTL base. The query is viewport-bounded by `rows`: pass the
/// visible row range and the cost is paid only for those rows.
pub fn slots_in(
    parent: Rect,
    obstacles: &[Rect],
    gutter: u16,
    rows: Range<u16>,
) -> Vec<Slot> {
    free_intervals_in_rows(parent, obstacles, gutter, rows)
        .into_iter()
        .map(|iv| Slot { row: iv.row, col: iv.start, width: iv.end - iv.start })
        .collect()
}

// ── Flow ─────────────────────────────────────────────────────────────────────

/// Flow `text` around the `obstacles` within `parent`, over the rows in `rows`,
/// returning one [`PlacedLine`] per slot (in flow order).
///
/// Builds the slot stream ([`slots_in`]) and flows the wrapped text into it
/// ([`crate::text::wrap_into_slots`]); each resulting visual line is tagged with
/// its slot's position. Text that outlasts the visible slots is dropped (it would
/// fall below the rows asked for). With no obstacles every row is one full-width
/// slot, so the result matches flat wrapping.
///
/// `base` selects the bidi base direction. It governs both the reordering
/// *within* each slot (Phase 2) and, under an RTL base, the *order of the slots*
/// within a row: the right-of-tile slot is filled before the left-of-tile slot
/// (§3.5). The rows themselves always run top-to-bottom.
pub fn flow(
    text: &str,
    parent: Rect,
    obstacles: &[Rect],
    gutter: u16,
    base: BaseDirection,
    rows: Range<u16>,
) -> Vec<PlacedLine> {
    let mut slots = slots_in(parent, obstacles, gutter, rows);
    // RTL reading order reverses the slots within each row so text flows into the
    // right-of-tile slot first. A single-slot (obstacle-free) row is unaffected.
    if matches!(base, BaseDirection::Rtl) {
        reverse_within_rows(&mut slots);
    }
    let widths: Vec<u16> = slots.iter().map(|s| s.width).collect();
    let lines = wrap_into_slots(text, &widths, base);
    slots
        .into_iter()
        .zip(lines)
        .map(|(s, line)| PlacedLine { row: s.row, col: s.col, width: s.width, line })
        .collect()
}

/// Reverse the order of slots **within each row**, leaving the top-to-bottom row
/// order intact — the within-row flip RTL runaround needs (§3.5).
///
/// Slots arrive grouped by ascending row (from [`slots_in`]); each maximal run of
/// same-row slots is reversed in place, so a row's `[left, right]` becomes
/// `[right, left]` while rows stay in order.
fn reverse_within_rows(slots: &mut [Slot]) {
    let mut i = 0;
    while i < slots.len() {
        let row = slots[i].row;
        let mut j = i;
        while j < slots.len() && slots[j].row == row {
            j += 1;
        }
        slots[i..j].reverse();
        i = j;
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Draw flowed lines into `buf`, each at its slot position, clipped to the slot
/// width. Empty lines draw nothing.
pub fn render_flow(buf: &mut Buffer, placed: &[PlacedLine], style: Style) {
    for p in placed {
        render_line(buf, p.col, p.row, &p.line, p.width, style);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::wrap;

    fn visual_string(line: &VisualLine) -> String {
        line.cells.iter().map(|c| c.symbol.as_str()).collect()
    }

    #[test]
    fn no_obstacle_one_slot_per_row() {
        let parent = Rect::new(0, 0, 10, 4);
        let placed = flow("the quick brown fox", parent, &[], 0, BaseDirection::Ltr, 0..4);
        // One slot per row, full width.
        assert!(placed.iter().all(|p| p.col == 0 && p.width == 10));
        // Row indices ascend.
        let rows: Vec<u16> = placed.iter().map(|p| p.row).collect();
        assert_eq!(rows, vec![0, 1, 2, 3]);
    }

    #[test]
    fn obstacle_splits_row_into_two_slots() {
        // A tile occupying columns [4, 8) on rows 0..2 splits those rows.
        let parent = Rect::new(0, 0, 12, 3);
        let tile = Rect::new(4, 0, 4, 2);
        let slots = slots_in(parent, &[tile], 0, 0..3);
        // Rows 0 and 1: left [0,4) and right [8,12); row 2: full [0,12).
        let row0: Vec<_> = slots.iter().filter(|s| s.row == 0).collect();
        assert_eq!(row0.len(), 2);
        assert_eq!((row0[0].col, row0[0].width), (0, 4)); // left of tile
        assert_eq!((row0[1].col, row0[1].width), (8, 4)); // right of tile
        let row2: Vec<_> = slots.iter().filter(|s| s.row == 2).collect();
        assert_eq!(row2.len(), 1);
        assert_eq!(row2[0].width, 12);
    }

    #[test]
    fn text_flows_around_tile() {
        // Text wraps into the left and right slots around a centered tile.
        let parent = Rect::new(0, 0, 12, 3);
        let tile = Rect::new(5, 0, 3, 2); // blocks cols [5,8) on rows 0,1
        let placed = flow("aaa bbb ccc ddd eee", parent, &[tile], 0, BaseDirection::Ltr, 0..3);
        // Every placed line fits inside its slot.
        for p in &placed {
            assert!(p.line.width() <= p.width, "line wider than slot");
        }
        // The first slot is the left-of-tile slot on row 0 (cols [0,5)).
        assert_eq!((placed[0].col, placed[0].width), (0, 5));
    }

    // ── Stage B: BiDi × runaround ─────────────────────────────────────────

    /// Find the placed line whose slot left edge is `col`, on row 0.
    fn at_col(placed: &[PlacedLine], col: u16) -> &PlacedLine {
        placed.iter().find(|p| p.row == 0 && p.col == col).expect("slot at col")
    }

    #[test]
    fn rtl_reverses_within_row_slot_order() {
        // A tile at cols [5,8) on row 0 splits it into left [0,5) and right [8,12).
        // Tokens "111 222 ..." flow left-first under LTR, right-first under RTL —
        // the §3.5 within-row slot-order flip. (ASCII digits isolate the slot
        // ordering from within-slot bidi, which Phase 2 already proves.)
        let parent = Rect::new(0, 0, 12, 1);
        let tile = Rect::new(5, 0, 3, 1);
        let text = "111 222 333 444";

        let ltr = flow(text, parent, &[tile], 0, BaseDirection::Ltr, 0..1);
        // LTR: first token in the LEFT slot, second in the RIGHT slot.
        assert_eq!(visual_string(&at_col(&ltr, 0).line), "111");
        assert_eq!(visual_string(&at_col(&ltr, 8).line), "222");

        let rtl = flow(text, parent, &[tile], 0, BaseDirection::Rtl, 0..1);
        // RTL: first token in the RIGHT slot, second in the LEFT slot.
        assert_eq!(visual_string(&at_col(&rtl, 8).line), "111");
        assert_eq!(visual_string(&at_col(&rtl, 0).line), "222");
    }

    #[test]
    fn rtl_flow_order_is_right_to_left() {
        // In flow (fill) order, row 0's first slot is the rightmost under RTL.
        let parent = Rect::new(0, 0, 12, 1);
        let tile = Rect::new(5, 0, 3, 1);
        let rtl = flow("aaa bbb ccc", parent, &[tile], 0, BaseDirection::Rtl, 0..1);
        let row0: Vec<u16> = rtl.iter().filter(|p| p.row == 0).map(|p| p.col).collect();
        assert_eq!(row0, vec![8, 0], "right slot precedes left slot under RTL");
    }

    #[test]
    fn rtl_obstacle_free_unchanged() {
        // With no tile, each row is one slot, so RTL reversal is a no-op on order;
        // the result still matches an RTL flat wrap.
        let parent = Rect::new(0, 0, 10, 3);
        let placed = flow("aaa bbb ccc ddd", parent, &[], 0, BaseDirection::Rtl, 0..3);
        let flat = wrap("aaa bbb ccc ddd", 10, BaseDirection::Rtl);
        for (i, p) in placed.iter().take(flat.line_count()).enumerate() {
            assert_eq!(visual_string(&p.line), visual_string(&flat.lines()[i]));
        }
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    fn obstacle_strategy() -> impl Strategy<Value = (Rect, Vec<Rect>)> {
        (8u16..24, 4u16..12).prop_flat_map(|(w, h)| {
            let parent = Rect::new(0, 0, w, h);
            let tile = (0u16..w, 0u16..h, 1u16..w, 1u16..h)
                .prop_map(move |(x, y, tw, th)| Rect::new(x, y, tw, th));
            proptest::collection::vec(tile, 0..3).prop_map(move |ts| (parent, ts))
        })
    }

    fn body() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![Just('a'), Just('b'), Just('c'), Just(' ')],
            0..60,
        )
        .prop_map(|cs| cs.into_iter().collect())
    }

    proptest! {
        /// No glyph is placed outside its slot, and the total flowed width on a
        /// row never exceeds the sum of that row's slot widths.
        #[test]
        fn prop_glyphs_stay_within_slots(
            (parent, tiles) in obstacle_strategy(),
            text in body(),
        ) {
            let placed = flow(&text, parent, &tiles, 0, BaseDirection::Ltr,
                parent.y..parent.bottom());
            // Per-line: content fits its slot and lands within the parent.
            for p in &placed {
                prop_assert!(p.line.width() <= p.width);
                prop_assert!(p.col + p.width <= parent.right());
                prop_assert!(p.row < parent.bottom());
            }
            // Per-row: flowed width ≤ summed slot width.
            for row in parent.y..parent.bottom() {
                let flowed: u32 = placed.iter().filter(|p| p.row == row)
                    .map(|p| p.line.width() as u32).sum();
                let slotted: u32 = placed.iter().filter(|p| p.row == row)
                    .map(|p| p.width as u32).sum();
                prop_assert!(flowed <= slotted);
            }
        }

        /// Regression guard: with zero obstacles, runaround reproduces flat
        /// wrapping line for line (within the visible rows).
        #[test]
        fn prop_no_obstacle_matches_flat_wrap(width in 2u16..20, height in 1u16..12, text in body()) {
            let parent = Rect::new(0, 0, width, height);
            let placed = flow(&text, parent, &[], 0, BaseDirection::Ltr, 0..height);
            let flat = wrap(&text, width, BaseDirection::Ltr);
            // Compare the visible window (the first `height` flat lines).
            let take = (height as usize).min(flat.line_count());
            for (p, flat_line) in placed.iter().zip(flat.lines()).take(take) {
                prop_assert_eq!(visual_string(&p.line), visual_string(flat_line));
            }
        }
    }
}
