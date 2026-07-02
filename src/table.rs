// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Column-grid layout and drawing helpers for structured data views.
//!
//! A [`ColumnGrid`] declares a set of columns with [`Size`]/[`Constraint`]
//! sizing (the same types used for tile layout) and resolves them to concrete
//! [`Rect`]s for a given area — exactly as [`layout::solve`] distributes space
//! among tiles, but along one axis of a data row.
//!
//! # Quick start
//!
//! ```no_run
//! use mullion::{Buffer, Rect};
//! use mullion::table::{ColumnDef, ColumnGrid, ColumnKind};
//! use mullion::layout::Size;
//! use mullion::label::Align;
//! use mullion::style::Style;
//!
//! // Define columns: name (flexible 8–24), spacer, value (fixed 9), bar (fill).
//! let grid = ColumnGrid::new(vec![
//!     ColumnDef::fill(1, ColumnKind::Text).with_min(8).with_max(24),
//!     ColumnDef::fixed(1, ColumnKind::Custom),
//!     ColumnDef::fixed(9, ColumnKind::Number { unit_cols: 1 }),
//!     ColumnDef::fill(1, ColumnKind::Bar),
//! ]);
//!
//! // In your render function, resolve once per frame:
//! # let buf: &mut Buffer = unimplemented!();
//! # let area: Rect = unimplemented!();
//! let col_rects = grid.resolve(area);
//!
//! // For each data row at y:
//! # let y = 0u16;
//! # let dim = Style::default();
//! ColumnGrid::write_text(buf, col_rects[0], y, "my-process", Align::Start, dim);
//! ColumnGrid::write_number(buf, col_rects[2], y, "42.3", dim, "%", dim, 1);
//! ColumnGrid::write_bar(buf, col_rects[3], y, 0.423, '█', dim, '░', dim, None);
//! ```

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::label::Align;
use crate::layout::{Constraint, Node, Orientation, Size};
use crate::layout::TileId;
use crate::render::render_carousel;
use crate::style::Style;
use crate::text::{elide, render_line, shape_line, TextCtx};

// ── ColumnKind ────────────────────────────────────────────────────────────────

/// The semantic type of a column.
///
/// Used by the `write_*` helpers to understand the column's role.  Callers
/// that write to columns directly can use [`ColumnKind::Custom`].
#[derive(Debug, Clone, Copy)]
pub enum ColumnKind {
    /// Arbitrary text; use [`ColumnGrid::write_text`].
    Text,
    /// A numeric value with a fixed-width unit suffix right-anchored within
    /// the column.  Use [`ColumnGrid::write_number`].
    ///
    /// For example, `unit_cols = 1` for a `"%"` suffix: the rightmost cell
    /// is always the unit; the rest hold the right-aligned number string.
    Number {
        /// Number of cells reserved at the right edge for the unit suffix.
        unit_cols: u16,
    },
    /// A horizontal bar that fills the entire column.  Use [`ColumnGrid::write_bar`].
    Bar,
    /// No assumed semantics; caller writes into the resolved rect directly.
    Custom,
}

// ── ColumnDef ─────────────────────────────────────────────────────────────────

/// Definition of one column in a [`ColumnGrid`].
#[derive(Debug, Clone)]
pub struct ColumnDef {
    /// How much horizontal space this column requests.
    ///
    /// Uses the same [`Constraint`] type as tile layout, so `Fixed`,
    /// `Percent`, `Fill`, `min`, and `max` all behave identically.
    pub size: Constraint,
    /// Semantic type of the column.
    pub kind: ColumnKind,
    /// Default text alignment for [`ColumnGrid::write_text`].
    pub align: Align,
}

impl ColumnDef {
    /// A fixed-width column.
    pub fn fixed(width: u16, kind: ColumnKind) -> Self {
        Self {
            size:  Constraint::new(Size::Fixed(width)),
            kind,
            align: Align::Start,
        }
    }

    /// A fill column that takes a proportional share of leftover space.
    pub fn fill(weight: u16, kind: ColumnKind) -> Self {
        Self {
            size:  Constraint::new(Size::Fill(weight)),
            kind,
            align: Align::Start,
        }
    }

    /// A percent-of-available-width column.
    pub fn percent(pct: u16, kind: ColumnKind) -> Self {
        Self {
            size:  Constraint::new(Size::Percent(pct)),
            kind,
            align: Align::Start,
        }
    }

    /// Override the default alignment.
    pub fn with_align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Set a minimum width (cells).
    pub fn with_min(mut self, min: u16) -> Self {
        self.size.min = min;
        self
    }

    /// Set a maximum width (cells).
    pub fn with_max(mut self, max: u16) -> Self {
        self.size.max = max;
        self
    }
}

// ── ColumnGrid ────────────────────────────────────────────────────────────────

/// A row-oriented column layout grid.
///
/// Holds an ordered list of [`ColumnDef`]s and resolves them to concrete
/// [`Rect`]s for a given area.  Column widths are computed by [`layout::solve`],
/// so `Fill` water-filling, `Percent`, `Fixed`, and `min`/`max` clamps all
/// behave identically to tile layouts.
///
/// The grid is stateless after construction; call [`resolve`](Self::resolve)
/// every frame or whenever the area changes.
pub struct ColumnGrid {
    columns: Vec<ColumnDef>,
    /// When set, [`resolve`](ColumnGrid::resolve) mirrors the column rects for a
    /// right-to-left layout (§round-2 A5), so column 0 sits at the right edge.
    mirror: bool,
}

impl ColumnGrid {
    /// Construct a new grid from a list of column definitions.
    pub fn new(columns: Vec<ColumnDef>) -> Self {
        Self { columns, mirror: false }
    }

    /// Mirror the resolved column order for `base` (RTL reverses it; LTR is a
    /// no-op). Column *indices* are preserved — `cols[0]` is still the first
    /// declared column, only positioned from the right — so callers index the
    /// same way regardless of direction.
    pub fn mirrored(mut self, base: crate::text::BaseDirection) -> Self {
        self.mirror = matches!(base, crate::text::BaseDirection::Rtl);
        self
    }

    /// Resolve all column widths for `area` and return one `Rect` per column.
    ///
    /// All returned rects share `area.y` and `area.height`; their `x` and
    /// `width` tile exactly across `area.width` with no gaps or overlaps.
    ///
    /// Calls [`layout::solve`] internally, so the sizing behaviour is
    /// identical to tile layout: fixed columns are satisfied first, then
    /// percent columns, then fill columns share the remainder proportionally.
    ///
    /// Returns `vec![Rect::default(); n]` when the grid is empty or the area
    /// has zero width.
    ///
    /// ```
    /// use mullion::table::{ColumnDef, ColumnGrid, ColumnKind};
    /// use mullion::layout::Size;
    /// use mullion::Rect;
    ///
    /// let grid = ColumnGrid::new(vec![
    ///     ColumnDef::fixed(10, ColumnKind::Text),
    ///     ColumnDef::fill(1,  ColumnKind::Bar),
    /// ]);
    /// let area  = Rect::new(0, 0, 30, 1);
    /// let rects = grid.resolve(area);
    /// assert_eq!(rects[0].width, 10);
    /// assert_eq!(rects[1].width, 20);
    /// ```
    pub fn resolve(&self, area: Rect) -> Vec<Rect> {
        if self.columns.is_empty() || area.width == 0 {
            return vec![Rect::default(); self.columns.len()];
        }
        let mut root = Node::Split {
            orientation: Orientation::Horizontal,
            children: self.columns.iter().enumerate()
                .map(|(i, c)| (c.size, Node::Tile(i as u64)))
                .collect(),
        };
        let tiles = crate::layout::solve(&mut root, area);
        let mut rects = vec![Rect::default(); self.columns.len()];
        for (id, rect) in tiles {
            let idx = id as usize;
            if idx < rects.len() {
                rects[idx] = rect;
            }
        }
        if self.mirror {
            crate::geometry::mirror_rects_in(area, &mut rects);
        }
        rects
    }

    /// Resolve columns for a single row `y` within `area`.
    ///
    /// Equivalent to `resolve(Rect::new(area.x, y, area.width, 1))`.
    pub fn row_rects(&self, area: Rect, y: u16) -> Vec<Rect> {
        let row = Rect::new(area.x, y, area.width, 1);
        self.resolve(row)
    }

    // ── Drawing helpers ───────────────────────────────────────────────────────

    /// Write a number and unit string into a column rect at row `y`.
    ///
    /// The rightmost `unit_cols` cells always hold `unit` (left-aligned within
    /// that sub-rect).  The remaining cells to the left hold `value`,
    /// right-aligned.  Both portions may carry independent styles.
    ///
    /// If the rect is too narrow to show the unit at all, only the value is
    /// written (truncated if necessary).
    ///
    /// ```
    /// use mullion::{Buffer, Rect};
    /// use mullion::table::ColumnGrid;
    /// use mullion::style::Style;
    ///
    /// let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
    /// ColumnGrid::write_number(&mut buf, Rect::new(0, 0, 10, 1), 0,
    ///     " 42.3", Style::default(), "%", Style::default(), 1);
    /// // Cells 0..8 → right-aligned " 42.3", cell 9 → "%".
    /// ```
    pub fn write_number(
        buf:         &mut Buffer,
        rect:        Rect,
        y:           u16,
        value:       &str,
        value_style: Style,
        unit:        &str,
        unit_style:  Style,
        unit_cols:   u16,
    ) {
        if rect.width == 0 { return; }
        let unit_w = unit_cols.min(rect.width);
        let val_w  = rect.width - unit_w;
        if val_w > 0 {
            Self::write_text(buf, Rect::new(rect.x, y, val_w, 1), y,
                value, Align::End, value_style);
        }
        if unit_w > 0 {
            Self::write_text(buf, Rect::new(rect.x + val_w, y, unit_w, 1), y,
                unit, Align::Start, unit_style);
        }
    }

    /// Write text into a column rect with alignment and `…` truncation (LTR).
    ///
    /// A convenience wrapper over [`write_text_ctx`](ColumnGrid::write_text_ctx) with
    /// [`TextCtx::LTR`]. Prefer the `_ctx` form when a base direction is in play.
    pub fn write_text(buf: &mut Buffer, rect: Rect, y: u16, text: &str, align: Align, style: Style) {
        Self::write_text_ctx(buf, rect, y, text, align, style, TextCtx::LTR);
    }

    /// Write text into a column rect, grapheme/width/bidi correct, per `ctx`.
    ///
    /// - `Align::Start`  — flush leading (no padding)
    /// - `Align::End`    — flush trailing (leading spaces)
    /// - `Align::Center` — centred; odd remainder biases toward `Start`
    ///
    /// The text is shaped through [`shape_line`](crate::text::shape_line) so RTL and
    /// mixed content read correctly; text wider than `rect.width` is truncated with a
    /// direction-aware `…` via [`elide`](crate::text::elide) (grapheme-cluster and
    /// width correct — never a split cluster or half-glyph). Zero-width rects are
    /// ignored.
    pub fn write_text_ctx(
        buf:   &mut Buffer,
        rect:  Rect,
        y:     u16,
        text:  &str,
        align: Align,
        style: Style,
        ctx:   TextCtx,
    ) {
        if rect.width == 0 { return; }
        let w = rect.width;
        let line = shape_line(text, 0, ctx.base);
        let dw = line.width();
        if dw <= w {
            let x_offset = match align {
                Align::Start  => 0,
                Align::Center => (w - dw) / 2,
                Align::End    => w - dw,
            };
            render_line(buf, rect.x + x_offset, y, &line, w, style);
        } else {
            let clipped = elide(text, w, ctx);
            render_line(buf, rect.x, y, &clipped, w, style);
        }
    }

    /// Fill a column rect with a horizontal bar at row `y`.
    ///
    /// Cells `0 .. ceil(fraction * width)` receive `filled_ch`/`filled_style`;
    /// the rest receive `empty_ch`/`empty_style`.  `fraction` is clamped to
    /// `[0, 1]`.
    ///
    /// The optional `overlay` closure is called for each cell index
    /// `0 .. width`; when it returns `Some((ch, style))` the returned
    /// character and style overwrite that cell.  Use this for histogram dot
    /// overlays (e.g. `◻` in a [`planck_color`]-based style) without coupling
    /// the colour logic to this module.
    ///
    /// ```
    /// use mullion::{Buffer, Rect};
    /// use mullion::table::ColumnGrid;
    /// use mullion::style::Style;
    ///
    /// let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
    /// ColumnGrid::write_bar(&mut buf, Rect::new(0, 0, 10, 1), 0,
    ///     0.4, '█', Style::default(), '░', Style::default(), None);
    /// // First 4 cells → '█', remaining 6 → '░'.
    /// ```
    pub fn write_bar(
        buf:          &mut Buffer,
        rect:         Rect,
        y:            u16,
        fraction:     f32,
        filled_ch:    char,
        filled_style: Style,
        empty_ch:     char,
        empty_style:  Style,
        overlay:      Option<&dyn Fn(usize) -> Option<(char, Style)>>,
    ) {
        let w = rect.width as usize;
        if w == 0 { return; }
        let filled = ((fraction.clamp(0.0, 1.0) * w as f32).round() as usize).min(w);

        for i in 0..w {
            let x = rect.x + i as u16;
            let (ch, st) = if i < filled {
                (filled_ch, filled_style)
            } else {
                (empty_ch, empty_style)
            };
            buf.set_string(x, y, &ch.to_string(), st);
        }

        if let Some(f) = overlay {
            for i in 0..w {
                if let Some((ch, st)) = f(i) {
                    buf.set_string(rect.x + i as u16, y, &ch.to_string(), st);
                }
            }
        }
    }
}

// ── Table ─────────────────────────────────────────────────────────────────────

/// A structured table: an optional fixed header row, a scrollable carousel body,
/// and an optional fixed footer row — all sharing the same column widths.
///
/// Column widths are resolved once from the available area and passed to every
/// closure as a `&[Rect]` slice, so header, body, and footer columns are
/// guaranteed to align perfectly without any manual coordinate arithmetic.
///
/// # Usage
///
/// ```no_run
/// use mullion::{Buffer, Rect, Table};
/// use mullion::table::{ColumnDef, ColumnGrid, ColumnKind};
/// use mullion::layout::Node;
/// use mullion::style::Style;
///
/// # let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
/// # let area = Rect::new(0, 0, 80, 24);
/// # let mut carousel = Node::Carousel { id: 0, orientation: mullion::layout::Orientation::Vertical, scroll: 0, children: vec![] };
/// # let dim = Style::default();
/// let grid = ColumnGrid::new(vec![
///     ColumnDef::fill(1, ColumnKind::Text).with_min(8).with_max(28),
///     ColumnDef::fixed(9, ColumnKind::Number { unit_cols: 1 }),
///     ColumnDef::fill(2, ColumnKind::Bar),
/// ]);
/// let table = Table::new(grid);
///
/// // Before rendering, use body_area to call scroll_focus_into_view:
/// // tree.scroll_focus_into_view(table.body_area(area, true, false));
///
/// table.render(&mut buf, area, &mut carousel,
///     Some(|buf: &mut Buffer, cols: &[Rect]| {
///         // draw header using cols[0], cols[1], cols[2] …
///     }),
///     None::<fn(&mut Buffer, &[Rect])>,
///     |buf: &mut Buffer, id: u64, cols: &[Rect]| {
///         // draw one data row
///     },
/// );
/// ```
pub struct Table {
    grid: ColumnGrid,
}

impl Table {
    /// Create a new `Table` with the given column layout.
    pub fn new(grid: ColumnGrid) -> Self {
        Self { grid }
    }

    /// The rect the carousel body will occupy given the presence of a header/footer.
    ///
    /// Call this before `render` to feed `tree.scroll_focus_into_view` the
    /// correct carousel rect:
    ///
    /// ```no_run
    /// # use mullion::{Table, Rect};
    /// # use mullion::table::{ColumnGrid, ColumnDef, ColumnKind};
    /// # let table = Table::new(ColumnGrid::new(vec![]));
    /// # let area = Rect::new(0, 0, 80, 24);
    /// # let mut tree = mullion::tree::Tree::new(mullion::layout::Node::Tile(0));
    /// tree.scroll_focus_into_view(table.body_area(area, true, false));
    /// ```
    pub fn body_area(&self, area: Rect, has_header: bool, has_footer: bool) -> Rect {
        let top = area.y + if has_header { 1 } else { 0 };
        let trim = if has_header { 1u16 } else { 0 } + if has_footer { 1u16 } else { 0 };
        Rect::new(area.x, top, area.width, area.height.saturating_sub(trim))
    }

    /// Render the table into `area`.
    ///
    /// - `header` — if `Some`, draws one fixed row at the top of `area`.
    /// - `footer` — if `Some`, draws one fixed row at the bottom of `area`.
    /// - `row` — called by [`render_carousel`] for each visible entry; receives
    ///   the `TileId` and column rects already positioned at that entry's `y`.
    /// - `carousel` — the [`Node::Carousel`] that supplies the scrollable body.
    ///
    /// All closures receive the same resolved column x-positions and widths,
    /// guaranteeing alignment across header, body, and footer.
    pub fn render<H, F, R>(
        &self,
        buf:      &mut Buffer,
        area:     Rect,
        carousel: &mut Node,
        mut header: Option<H>,
        mut footer: Option<F>,
        mut row:    R,
    ) where
        H: FnMut(&mut Buffer, &[Rect]),
        F: FnMut(&mut Buffer, &[Rect]),
        R: FnMut(&mut Buffer, TileId, &[Rect]),
    {
        if area.height == 0 { return; }

        // Resolve x-positions and widths once; y is irrelevant for column layout.
        let widths: Vec<(u16, u16)> = self.grid
            .resolve(Rect::new(area.x, 0, area.width, 1))
            .into_iter()
            .map(|r| (r.x, r.width))
            .collect();

        let rects_at = |y: u16| -> Vec<Rect> {
            widths.iter().map(|&(x, w)| Rect::new(x, y, w, 1)).collect()
        };

        let mut top = area.y;
        let mut bot = area.y + area.height;

        if let Some(ref mut h) = header {
            if top < bot {
                h(buf, &rects_at(top));
                top += 1;
            }
        }
        if let Some(ref mut f) = footer {
            if top < bot {
                bot -= 1;
                f(buf, &rects_at(bot));
            }
        }

        let body = Rect::new(area.x, top, area.width, bot.saturating_sub(top));
        if body.height == 0 { return; }

        render_carousel(buf, carousel, body, &mut |buf, id, rect| {
            row(buf, id, &rects_at(rect.y));
        });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16) -> Rect { Rect::new(0, 0, w, 1) }

    #[test]
    fn resolve_fixed_plus_fill() {
        let grid = ColumnGrid::new(vec![
            ColumnDef::fixed(10, ColumnKind::Text),
            ColumnDef::fill(1, ColumnKind::Bar),
        ]);
        let rects = grid.resolve(area(30));
        assert_eq!(rects[0], Rect::new(0, 0, 10, 1));
        assert_eq!(rects[1], Rect::new(10, 0, 20, 1));
    }

    #[test]
    fn resolve_two_fills() {
        let grid = ColumnGrid::new(vec![
            ColumnDef::fill(1, ColumnKind::Text),
            ColumnDef::fill(3, ColumnKind::Bar),
        ]);
        let rects = grid.resolve(area(40));
        assert_eq!(rects[0].width, 10);
        assert_eq!(rects[1].width, 30);
    }

    #[test]
    fn resolve_min_clamp() {
        let grid = ColumnGrid::new(vec![
            ColumnDef::fill(1, ColumnKind::Text).with_min(8).with_max(24),
            ColumnDef::fill(1, ColumnKind::Bar),
        ]);
        // At width 12: both Fill(1). Seeded: col0 at min=8, col1 at 0.
        // Leftover = 4, split 50/50: col0 = 8+2 = 10, col1 = 0+2 = 2.
        let rects = grid.resolve(area(12));
        assert_eq!(rects[0].width, 10);
        assert_eq!(rects[1].width, 2);
    }

    #[test]
    fn resolve_empty_grid() {
        let grid = ColumnGrid::new(vec![]);
        let rects = grid.resolve(area(80));
        assert!(rects.is_empty());
    }

    #[test]
    fn resolve_zero_width() {
        let grid = ColumnGrid::new(vec![ColumnDef::fill(1, ColumnKind::Text)]);
        let rects = grid.resolve(area(0));
        assert_eq!(rects[0], Rect::default());
    }

    #[test]
    fn write_bar_basic() {
        let mut buf = Buffer::empty(area(10));
        ColumnGrid::write_bar(&mut buf, area(10), 0, 0.4, '█', Style::default(), '░', Style::default(), None);
        let content: String = (0..10).map(|x| buf.get(x, 0).symbol.chars().next().unwrap_or(' ')).collect();
        assert_eq!(&content, "████░░░░░░");
    }

    #[test]
    fn write_bar_overlay() {
        let mut buf = Buffer::empty(area(10));
        ColumnGrid::write_bar(&mut buf, area(10), 0, 1.0, '█', Style::default(), '░', Style::default(),
            Some(&|i| if i == 3 { Some(('◻', Style::default())) } else { None }));
        assert_eq!(buf.get(3, 0).symbol, "◻");
        assert_eq!(buf.get(0, 0).symbol, "█");
    }

    #[test]
    fn write_text_right_align() {
        let mut buf = Buffer::empty(area(10));
        ColumnGrid::write_text(&mut buf, area(10), 0, "hi", Align::End, Style::default());
        // "hi" is 2 chars, right-aligned in 10 → offset 8.
        assert_eq!(buf.get(8, 0).symbol, "h");
        assert_eq!(buf.get(9, 0).symbol, "i");
    }

    #[test]
    fn write_text_truncation() {
        let mut buf = Buffer::empty(area(5));
        ColumnGrid::write_text(&mut buf, area(5), 0, "abcdefgh", Align::Start, Style::default());
        let content: String = (0..5).map(|x| buf.get(x, 0).symbol.chars().next().unwrap_or(' ')).collect();
        // 4 chars + ellipsis = 5
        assert_eq!(&content, "abcd…");
    }

    #[test]
    fn mirrored_grid_reverses_positions_keeping_indices() {
        use crate::text::BaseDirection;
        let grid = ColumnGrid::new(vec![
            ColumnDef::fixed(10, ColumnKind::Text),
            ColumnDef::fill(1, ColumnKind::Bar),
        ]);
        let area = Rect::new(0, 0, 30, 1);
        let ltr = grid.resolve(area);
        let grid2 = ColumnGrid::new(vec![
            ColumnDef::fixed(10, ColumnKind::Text),
            ColumnDef::fill(1, ColumnKind::Bar),
        ]);
        let rtl = grid2.mirrored(BaseDirection::Rtl).resolve(area);
        // Index 0 is still the 10-wide column, but now flush right (x=20) not left.
        assert_eq!(ltr[0].x, 0);
        assert_eq!(rtl[0].x, 20);
        assert_eq!(rtl[0].width, 10);
        assert_eq!(rtl[1].x, 0); // the fill column moved to the left
    }

    #[test]
    fn align_resolves_against_direction() {
        use crate::label::Anchor;
        use crate::text::BaseDirection::{Ltr, Rtl};
        assert_eq!(Align::Start.resolve(Ltr), Anchor::Left);
        assert_eq!(Align::Start.resolve(Rtl), Anchor::Right);
        assert_eq!(Align::End.resolve(Rtl), Anchor::Left);
        assert_eq!(Align::Center.resolve(Rtl), Anchor::Center);
    }

    #[test]
    fn write_text_ctx_renders_rtl_visually() {
        use crate::text::TextCtx;
        let mut buf = Buffer::empty(area(6));
        // Logical "אבג" displays right-to-left as גבא through the shaper.
        ColumnGrid::write_text_ctx(&mut buf, area(6), 0, "אבג", Align::Start, Style::default(), TextCtx::rtl());
        let content: Vec<String> = (0..3).map(|x| buf.get(x, 0).symbol.clone()).collect();
        assert_eq!(content, vec!["ג", "ב", "א"]);
    }

    #[test]
    fn write_number_splits_correctly() {
        let mut buf = Buffer::empty(area(10));
        ColumnGrid::write_number(&mut buf, area(10), 0,
            "42.3", Style::default(), "%", Style::default(), 1);
        // Unit at column 9.
        assert_eq!(buf.get(9, 0).symbol, "%");
        // Value right-aligned in columns 0..9 → "42.3" ends at col 8.
        assert_eq!(buf.get(8, 0).symbol, "3");
    }
}
