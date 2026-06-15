use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    geometry::Rect,
    style::Style,
};

/// A single terminal cell holding one grapheme cluster and its visual style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme cluster rendered in this cell (may be multi-byte).
    ///
    /// An empty string marks a **continuation cell**: the right half of a
    /// 2-wide grapheme.  Continuation cells are skipped during rendering.
    pub symbol: String,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self { symbol: " ".into(), style: Style::default() }
    }
}

impl Cell {
    pub fn new(symbol: impl Into<String>, style: Style) -> Self {
        Self { symbol: symbol.into(), style }
    }

    /// True if this cell is the right-hand placeholder of a 2-wide grapheme.
    pub fn is_continuation(&self) -> bool {
        self.symbol.is_empty()
    }
}

/// A 2-D grid of [`Cell`]s that widgets draw into.
///
/// Indexed row-major: `cells[y * width + x]`.
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
    pub area: Rect,
    cells: Vec<Cell>,
}

impl Buffer {
    /// Allocate a buffer covering `area`, filled with default (space) cells.
    pub fn empty(area: Rect) -> Self {
        let len = area.area() as usize;
        Self { area, cells: vec![Cell::default(); len] }
    }

    fn idx(&self, x: u16, y: u16) -> usize {
        debug_assert!(self.area.contains(x, y), "({x},{y}) outside {:?}", self.area);
        (y - self.area.y) as usize * self.area.width as usize
            + (x - self.area.x) as usize
    }

    pub fn get(&self, x: u16, y: u16) -> &Cell {
        let i = self.idx(x, y);
        &self.cells[i]
    }

    pub fn get_mut(&mut self, x: u16, y: u16) -> &mut Cell {
        let i = self.idx(x, y);
        &mut self.cells[i]
    }

    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        // If cell at (x,y) is wide, blank the continuation to the right.
        // If cell at (x,y) is a continuation, blank the wide cell to the left.
        self.unlink_wide_at(x, y);

        let i = self.idx(x, y);
        self.cells[i] = cell;
    }

    /// Blank any wide-grapheme partner of the cell at `(x, y)` before overwriting it.
    fn unlink_wide_at(&mut self, x: u16, y: u16) {
        let i = self.idx(x, y);
        if self.cells[i].is_continuation() {
            // We're overwriting a continuation: blank the wide cell to our left.
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
                // Blank the continuation cell to the right, if in bounds.
                let rx = x + 1;
                if rx < self.area.right() {
                    let right = self.idx(rx, y);
                    self.cells[right] = Cell::default();
                }
            }
        }
    }

    /// Write a single grapheme cluster at `(x, y)`.
    ///
    /// If the cluster is 2 columns wide the next cell becomes a continuation.
    /// Returns the x position after this grapheme.
    pub fn set_grapheme(&mut self, x: u16, y: u16, grapheme: &str, style: Style) -> u16 {
        if x >= self.area.right() || y >= self.area.bottom() || y < self.area.y {
            return x;
        }
        let w = UnicodeWidthStr::width(grapheme) as u16;
        if w == 0 {
            // zero-width: append to the previous cell's symbol (combining mark)
            if x > self.area.x {
                let i = self.idx(x - 1, y);
                self.cells[i].symbol.push_str(grapheme);
            }
            return x;
        }
        // Would the grapheme spill past the right edge?
        if x + w > self.area.right() {
            // Fill with spaces rather than splitting.
            let space = Cell { symbol: " ".into(), style };
            let i = self.idx(x, y);
            self.cells[i] = space;
            return x + 1;
        }

        self.unlink_wide_at(x, y);
        let i = self.idx(x, y);
        self.cells[i] = Cell { symbol: grapheme.into(), style };

        if w == 2 {
            // Also clear the right neighbor that is now a continuation.
            if x + 1 < self.area.right() {
                // Also unlink whatever was at x+1 before.
                self.unlink_wide_at(x + 1, y);
                let j = self.idx(x + 1, y);
                self.cells[j] = Cell { symbol: String::new(), style };
            }
        }

        x + w
    }

    /// Write `s` left-to-right starting at `(x, y)`, clipping at the right edge.
    ///
    /// Returns the x position after the last written grapheme.
    pub fn set_string(&mut self, x: u16, y: u16, s: &str, style: Style) -> u16 {
        let mut cx = x;
        for g in s.graphemes(true) {
            if cx >= self.area.right() {
                break;
            }
            cx = self.set_grapheme(cx, y, g, style);
        }
        cx
    }

    /// Reset every cell to the default (space, default style).
    pub fn reset(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
    }

    /// Fill `rect` (clipped to `self.area`) with copies of `cell`.
    pub fn fill(&mut self, rect: Rect, cell: Cell) {
        let r = self.area.intersection(rect);
        for row in r.y..r.bottom() {
            for col in r.x..r.right() {
                let i = self.idx(col, row);
                self.cells[i] = cell.clone();
            }
        }
    }

    /// Reallocate for a new `area`, discarding all content.
    pub fn resize(&mut self, area: Rect) {
        self.area = area;
        let len = area.area() as usize;
        self.cells = vec![Cell::default(); len];
    }

    /// Return the minimal set of cells that differ from `prev`.
    ///
    /// Continuation cells are never included (they carry no independent
    /// render content).
    pub fn diff<'a>(&'a self, prev: &'a Buffer) -> Vec<(u16, u16, &'a Cell)> {
        let mut out = Vec::new();
        // Only diff over the intersection; both buffers may have different areas
        // when a resize just happened.
        let area = self.area.intersection(prev.area);
        for row in area.y..area.bottom() {
            for col in area.x..area.right() {
                let cell = self.get(col, row);
                if cell.is_continuation() {
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
