// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    geometry::Rect,
    style::Style,
};

/// A single terminal cell holding one grapheme cluster and its visual style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme cluster rendered in this cell (may be multi-byte UTF-8).
    ///
    /// **Empty-string convention:** an empty `symbol` marks a *continuation
    /// cell* — the right-hand column of a 2-wide grapheme (e.g. `世`).
    /// Continuation cells are skipped by the renderer; the terminal advances
    /// the cursor automatically when it draws the wide glyph in the left cell.
    /// The invariant is: if `symbol.is_empty()` then the cell at `(x-1, y)`
    /// holds a 2-wide grapheme.
    pub symbol: String,
    /// Visual style (colors + text attributes) applied to this grapheme.
    pub style: Style,
}

impl Default for Cell {
    /// Return a blank cell: a single space with the default style.
    fn default() -> Self {
        Self { symbol: " ".into(), style: Style::default() }
    }
}

impl Cell {
    /// Construct a cell with the given symbol string and style.
    pub fn new(symbol: impl Into<String>, style: Style) -> Self {
        Self { symbol: symbol.into(), style }
    }

    /// Return `true` if this cell is the right-hand placeholder of a 2-wide grapheme.
    pub fn is_continuation(&self) -> bool {
        self.symbol.is_empty()
    }
}

/// A 2-D grid of [`Cell`]s that widgets draw into.
///
/// Storage is **row-major**: cell `(x, y)` is at index
/// `(y - area.y) * area.width + (x - area.x)`.
///
/// ## Wide-grapheme rule
///
/// When a 2-wide grapheme (e.g. `世`, emoji) is written at `(x, y)`:
/// - `(x, y)` holds the cluster.
/// - `(x+1, y)` becomes a **continuation cell** (empty symbol).
///
/// Overwriting either half of a wide pair blanks the partner so no
/// half-glyph is ever visible.
pub struct Buffer {
    /// The rectangular region in terminal space that this buffer covers.
    pub area: Rect,
    /// Flat backing store, row-major.  Private so callers must go through the
    /// validated accessors that maintain the wide-grapheme invariant.
    cells: Vec<Cell>,
}

impl Buffer {
    /// Allocate a buffer covering `area`, filled with default (space) cells.
    pub fn empty(area: Rect) -> Self {
        let len = area.area() as usize;
        Self { area, cells: vec![Cell::default(); len] }
    }

    /// Compute the flat index of cell `(x, y)`.
    ///
    /// Uses `debug_assert!` to catch out-of-bounds accesses in debug builds;
    /// in release builds the assertion is elided for performance.
    ///
    /// # Panics
    /// Panics (in debug builds) if `(x, y)` is outside `self.area`.
    fn idx(&self, x: u16, y: u16) -> usize {
        debug_assert!(self.area.contains(x, y), "({x},{y}) outside {:?}", self.area);
        (y - self.area.y) as usize * self.area.width as usize
            + (x - self.area.x) as usize
    }

    /// Return a shared reference to the cell at `(x, y)`.
    pub fn get(&self, x: u16, y: u16) -> &Cell {
        let i = self.idx(x, y);
        &self.cells[i]
    }

    /// Return a mutable reference to the cell at `(x, y)`.
    pub fn get_mut(&mut self, x: u16, y: u16) -> &mut Cell {
        let i = self.idx(x, y);
        &mut self.cells[i]
    }

    /// Write `cell` at `(x, y)`, maintaining the wide-grapheme invariant.
    ///
    /// Steps:
    /// 1. Unlink any existing wide-grapheme pair at `(x, y)` so no stranded
    ///    half-glyph remains.
    /// 2. Write the new cell.
    /// 3. If the new cell's symbol is 2 display columns wide, write a
    ///    continuation cell at `(x+1, y)` (after unlinking any pair that was
    ///    already there).
    ///
    /// # Invariants
    /// After this call, every wide-grapheme in the buffer has a matching
    /// continuation cell immediately to its right, and no continuation cell
    /// exists without a wide-grapheme to its left.
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        self.unlink_wide_at(x, y);

        let w = UnicodeWidthStr::width(cell.symbol.as_str());
        let style = cell.style;
        let i = self.idx(x, y);
        self.cells[i] = cell;

        // Establish the continuation cell so `set` and `set_grapheme` agree on
        // the wide-grapheme invariant.
        if w == 2 {
            let rx = x + 1;
            if rx < self.area.right() {
                self.unlink_wide_at(rx, y); // clear any prior pair that occupied x+1
                let j = self.idx(rx, y);
                self.cells[j] = Cell { symbol: String::new(), style };
            }
        }
    }

    /// Blank any wide-grapheme partner of the cell at `(x, y)` before overwriting it.
    ///
    /// Wide graphemes occupy two adjacent columns and must be erased as a pair —
    /// leaving either half in place would render a half-glyph (a visual
    /// artifact).  This helper is called before any write so the invariant is
    /// re-established atomically.
    ///
    /// Two cases:
    /// - **Cell is a continuation:** the wide glyph is one column to the left.
    ///   Blank the left cell so it becomes a normal space.
    /// - **Cell is wide (display width 2):** its continuation is one column to
    ///   the right.  Blank that right cell.
    ///
    /// Narrow cells (width 1) and default (space) cells have no partner and
    /// require no action.
    fn unlink_wide_at(&mut self, x: u16, y: u16) {
        let i = self.idx(x, y);
        if self.cells[i].is_continuation() {
            // We are about to overwrite a continuation cell.  The wide glyph
            // that owns it (to the left) must be blanked so no half appears.
            if x > self.area.x {
                let left = self.idx(x - 1, y);
                let w = UnicodeWidthStr::width(self.cells[left].symbol.as_str());
                if w == 2 {
                    self.cells[left] = Cell::default();
                }
            }
        } else {
            let w = UnicodeWidthStr::width(self.cells[i].symbol.as_str());
            if w == 2 {
                // We are about to overwrite the left half of a wide grapheme.
                // Blank the continuation cell to the right so no orphan remains.
                let rx = x + 1;
                if rx < self.area.right() {
                    let right = self.idx(rx, y);
                    self.cells[right] = Cell::default();
                }
            }
        }
    }

    /// Write a single grapheme cluster at `(x, y)` and return the next x position.
    ///
    /// Handles four cases based on the cluster's Unicode display width:
    ///
    /// - **Width 0 (combining mark / zero-width joiner):** the cluster is
    ///   appended to the *previous* cell's symbol so it renders as part of the
    ///   same glyph.  The cursor position `x` is not advanced.
    /// - **Width 1 (narrow):** written normally; cursor advances by 1.
    /// - **Width 2 (wide / full-width / emoji):** written at `(x, y)`; the
    ///   cell at `(x+1, y)` becomes a continuation cell (empty symbol).
    ///   Cursor advances by 2.
    /// - **Does not fit (x + width > right edge):** a space placeholder is
    ///   written at `x` instead of splitting the wide glyph, and the cursor
    ///   advances by 1.
    ///
    /// In all cases, `unlink_wide_at` is called before any write to avoid
    /// stranded half-glyphs.
    ///
    /// # Returns
    /// The x position immediately after the grapheme (or `x` for zero-width).
    ///
    /// # Invariants
    /// `x` must be within `self.area`; `y` must be a valid row.  Out-of-bounds
    /// `x` or `y` returns immediately without writing.
    pub fn set_grapheme(&mut self, x: u16, y: u16, grapheme: &str, style: Style) -> u16 {
        // Guard: do nothing if the position is outside the buffer.
        if x >= self.area.right() || y >= self.area.bottom() || y < self.area.y {
            return x;
        }
        let w = UnicodeWidthStr::width(grapheme) as u16;
        if w == 0 {
            // Zero-width cluster (e.g. a combining diacritic or ZWJ): attach it
            // to the preceding cell's symbol so it renders visually attached.
            // If there is no preceding cell on this row (x == area.x) the mark
            // is silently dropped — there is nowhere to anchor it.
            if x > self.area.x {
                let i = self.idx(x - 1, y);
                self.cells[i].symbol.push_str(grapheme);
            }
            return x; // cursor does not advance for zero-width clusters
        }
        // Would the grapheme spill past the right edge?  A wide glyph must never
        // be split across the buffer boundary, so we write a space placeholder
        // instead and leave the following column untouched.
        if x + w > self.area.right() {
            // Unlink first: column x may be the continuation of a wide char that
            // started at x-1; without unlinking, the left half would be stranded
            // after we overwrite the continuation with a space.
            self.unlink_wide_at(x, y);
            let i = self.idx(x, y);
            self.cells[i] = Cell { symbol: " ".into(), style };
            return x + 1; // consumed column x with a space; next position is x+1
        }

        // Normal write: unlink any prior pair, then install the grapheme.
        self.unlink_wide_at(x, y);
        let i = self.idx(x, y);
        self.cells[i] = Cell { symbol: grapheme.into(), style };

        if w == 2 {
            // Mark the column to the right as a continuation so the renderer
            // knows to skip it — only the left cell is sent to the backend.
            if x + 1 < self.area.right() {
                self.unlink_wide_at(x + 1, y); // clear anything that was there
                let j = self.idx(x + 1, y);
                self.cells[j] = Cell { symbol: String::new(), style };
            }
        }

        x + w
    }

    /// Write every grapheme in `s` left-to-right starting at `(x, y)`.
    ///
    /// Uses `unicode-segmentation` to split `s` into grapheme clusters so that
    /// combining marks stay attached to their base character.  Wide graphemes
    /// consume 2 columns; narrow ones consume 1.  Writing stops when the next
    /// grapheme would begin at or past the right edge.
    ///
    /// # Returns
    /// The x position immediately after the last written grapheme.
    pub fn set_string(&mut self, x: u16, y: u16, s: &str, style: Style) -> u16 {
        let mut cx = x;
        for g in s.graphemes(true) {
            // Stop as soon as there is no room for even a narrow grapheme.
            if cx >= self.area.right() {
                break;
            }
            cx = self.set_grapheme(cx, y, g, style);
        }
        cx
    }

    /// Reset every cell to the default (a space with the default style).
    pub fn reset(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
    }

    /// Fill every cell in `rect` (clipped to `self.area`) with copies of `cell`.
    ///
    /// The intersection of `rect` and `self.area` is computed first, so callers
    /// may pass an oversized rect without risking an out-of-bounds write.
    pub fn fill(&mut self, rect: Rect, cell: Cell) {
        let r = self.area.intersection(rect);
        for row in r.y..r.bottom() {
            for col in r.x..r.right() {
                let i = self.idx(col, row);
                self.cells[i] = cell.clone();
            }
        }
    }

    /// Reallocate to cover `area`, discarding all existing content.
    ///
    /// Called by `Terminal::check_resize`.  The caller is responsible for
    /// triggering a full repaint after resizing (e.g. via `Terminal::draw`).
    pub fn resize(&mut self, area: Rect) {
        self.area = area;
        let len = area.area() as usize;
        self.cells = vec![Cell::default(); len];
    }

    /// Return the minimal set of cells that differ from `prev`.
    ///
    /// Iterates the **intersection** of `self.area` and `prev.area` — the
    /// region that exists in both buffers.  Cells outside the intersection
    /// cannot be meaningfully compared (one side has no cell there) and are
    /// excluded; `Terminal::draw` issues a `backend.clear()` on resize to erase
    /// content that lies outside the new area.
    ///
    /// **Continuation cells are always skipped** in the output: they carry no
    /// independent render content.  The backend moves the cursor explicitly for
    /// every entry it receives, so only the left (non-continuation) cell of a
    /// wide pair needs to appear in the diff — the terminal advances the cursor
    /// past the right column automatically after drawing the wide glyph.
    ///
    /// # Returns
    /// A `Vec` of `(col, row, &Cell)` triples for every cell that changed.
    /// Returns an empty vec if the buffers are identical.
    pub fn diff<'a>(&'a self, prev: &'a Buffer) -> Vec<(u16, u16, &'a Cell)> {
        let mut out = Vec::new();
        // Diff only the region that exists in both buffers.  After a resize the
        // two buffers may have different areas; comparing outside the intersection
        // would require out-of-bounds indexing into the smaller buffer.
        let area = self.area.intersection(prev.area);
        for row in area.y..area.bottom() {
            for col in area.x..area.right() {
                let cell = self.get(col, row);
                if cell.is_continuation() {
                    // Continuation cells have no renderable content of their own;
                    // the backend will skip past them when drawing the wide glyph.
                    continue;
                }
                let old = prev.get(col, row);
                if cell != old {
                    out.push((col, row, cell));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    #[test]
    fn set_string_clips_at_right_edge() {
        let mut b = buf(4, 1);
        b.set_string(0, 0, "Hello", Style::default());
        assert_eq!(b.get(0, 0).symbol, "H");
        assert_eq!(b.get(3, 0).symbol, "l");
    }

    #[test]
    fn wide_graphemes_write_continuation_cells() {
        let mut b = buf(6, 1);
        b.set_string(0, 0, "世界", Style::default());
        // col 0: '世', col 1: continuation, col 2: '界', col 3: continuation
        assert_eq!(b.get(0, 0).symbol, "世");
        assert!(b.get(1, 0).is_continuation(), "col 1 should be continuation");
        assert_eq!(b.get(2, 0).symbol, "界");
        assert!(b.get(3, 0).is_continuation(), "col 3 should be continuation");
    }

    #[test]
    fn overwrite_first_half_of_wide_grapheme_blanks_second() {
        let mut b = buf(4, 1);
        b.set_string(0, 0, "世", Style::default());
        assert!(b.get(1, 0).is_continuation());
        // Now overwrite col 0 with a narrow char.
        b.set_grapheme(0, 0, "A", Style::default());
        assert_eq!(b.get(0, 0).symbol, "A");
        // col 1 must no longer be a continuation.
        assert_eq!(b.get(1, 0).symbol, " ", "continuation should have been blanked");
    }

    #[test]
    fn overwrite_second_half_of_wide_grapheme_blanks_first() {
        let mut b = buf(4, 1);
        b.set_string(0, 0, "世", Style::default());
        // Overwrite the continuation at col 1.
        b.set_grapheme(1, 0, "B", Style::default());
        assert_eq!(b.get(1, 0).symbol, "B");
        // col 0 must have been blanked.
        assert_eq!(b.get(0, 0).symbol, " ", "wide cell should have been blanked");
    }

    #[test]
    fn combining_mark_stays_in_one_cell() {
        let mut b = buf(4, 1);
        // "e\u{0301}" = é as base + combining acute
        b.set_string(0, 0, "e\u{0301}", Style::default());
        // grapheme segmentation should keep them together as one cluster
        let sym = &b.get(0, 0).symbol;
        assert!(sym.contains('e') && sym.contains('\u{0301}'),
            "combining mark should be in same cell, got: {sym:?}");
        assert_eq!(b.get(1, 0).symbol, " ", "col 1 should be untouched");
    }

    #[test]
    fn diff_equal_buffers_empty() {
        let a = buf(4, 2);
        let b = buf(4, 2);
        assert!(a.diff(&b).is_empty());
    }

    #[test]
    fn diff_single_change() {
        let mut a = buf(4, 2);
        let b = buf(4, 2);
        a.get_mut(2, 1).symbol = "X".into();
        let d = a.diff(&b);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].0, d[0].1), (2, 1));
    }

    #[test]
    fn diff_continuation_cells_not_emitted() {
        let mut a = buf(4, 1);
        let b = buf(4, 1);
        a.set_string(0, 0, "世", Style::default());
        let d = a.diff(&b);
        // Only the wide cell itself (col 0) should appear, not the continuation (col 1).
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].0, 0);
    }

    #[test]
    fn resize_then_write_does_not_panic() {
        let mut b = buf(10, 10);
        b.resize(Rect::new(0, 0, 5, 5));
        b.set_string(0, 0, "Hi", Style::default());
        b.resize(Rect::new(0, 0, 20, 20));
        b.set_string(0, 0, "Hello world!", Style::default());
    }
}
