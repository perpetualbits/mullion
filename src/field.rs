// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The **field**: one surface for everything you paint sub-cell content into.
//!
//! A [`Field`] is an ordered set of screen cells with a logical `width × height`
//! grid laid over them. That one type covers the whole family:
//!
//! - a **rectangle** ([`Field::rect`]) — a video / effects panel;
//! - a **strip** ([`Field::strip`]) — a 1-row field over an arbitrary cell path: a
//!   border-perimeter interval (which may cross corners), a connecting wire, or a
//!   single line of text;
//! - (later) thick / multi-line edges — a path with height > 1.
//!
//! Each logical cell carries a **glyph and a colour independently** — paint text and
//! let a separate source (a cellular automaton, a waving field, an image) drive the
//! colour, or tie them together. [`paint`](Field::paint) is the general per-cell
//! hook; the **video unit** maps an image (any `intensity(u, v)` over the field) to
//! cells three ways — [`render_braille`](Field::render_braille) (2×4 dithered
//! sub-pixels), [`render_ramp`](Field::render_ramp) (one brightness glyph per cell),
//! and [`render_glyphs`](Field::render_glyphs) (structure-aware directional strokes).
//! Each has an `_xy` variant ([`render_braille_xy`](Field::render_braille_xy), …)
//! whose colour closure also receives the cell's position, so a single image can
//! carry a **multi-hue scene** — colour by *where* a cell is, not just how bright.

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::style::Style;

/// A default brightness ramp for [`render_ramp`](Field::render_ramp), dark → light.
pub const BLOCK_RAMP: [char; 5] = [' ', '░', '▒', '▓', '█'];
/// The classic ASCII brightness ramp, dark → light.
pub const ASCII_RAMP: [char; 10] = [' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// A surface of screen cells with a logical `width × height` grid over them.
///
/// Logical cell `(col, row)` (origin top-left) maps to the screen cell
/// `cells[row * width + col]`. For a [`rect`](Field::rect) that is the obvious
/// grid; for a [`strip`](Field::strip) the row is always 0 and the column walks the
/// path. Content sources work in logical coordinates and never need to know which
/// shape they are painting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Screen cells in logical row-major order; `len() == width * height`.
    cells: Vec<(u16, u16)>,
    width: u16,
    height: u16,
}

impl Field {
    /// A rectangular field over `area`, row-major.
    pub fn rect(area: Rect) -> Self {
        let mut cells = Vec::with_capacity(area.width as usize * area.height as usize);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                cells.push((x, y));
            }
        }
        Self { cells, width: area.width, height: area.height }
    }

    /// A 1-row field over an arbitrary ordered cell path (a strip): a wire, a
    /// border-perimeter interval, a line of text. `width = cells.len()`, `height = 1`.
    ///
    /// A routed connector is one directly: `Field::strip(connector.path.clone())`
    /// gives a strip running along the wire, so it can **carry content** (a label, a
    /// flowing animation, a video band) along its length.
    pub fn strip(cells: Vec<(u16, u16)>) -> Self {
        let width = cells.len() as u16;
        Self { cells, width, height: 1 }
    }

    /// A 1-row field tracing the **border perimeter** of `area`, clockwise from the
    /// top-left corner — a strip that **crosses all four corners**, so content runs
    /// continuously around the box and turns at the corners without a break. This is
    /// the strip behind *gaps that move across corners*: paint a sliding window of the
    /// perimeter and the gap (or a marquee, or a band) travels around the border,
    /// corners and all.
    ///
    /// `width` is the border-cell count `2·(w + h) − 4` for `w, h ≥ 2`; a degenerate
    /// single row or column (which has no corners) is simply its straight run of
    /// cells. `height = 1`. The cells are distinct — the loop is left *open* (it does
    /// not repeat the start), so a caller that wants to wrap takes the column index
    /// modulo [`width`](Field::width).
    pub fn perimeter(area: Rect) -> Self {
        let mut cells = Vec::new();
        if area.width == 0 || area.height == 0 {
            return Self::strip(cells);
        }
        let (x0, y0) = (area.x, area.y);
        let (x1, y1) = (area.right() - 1, area.bottom() - 1); // inclusive far corners
        if area.width == 1 || area.height == 1 {
            // Degenerate: a single row or column has no corners — just its cells.
            for y in y0..=y1 {
                for x in x0..=x1 {
                    cells.push((x, y));
                }
            }
            return Self::strip(cells);
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
        Self::strip(cells)
    }

    /// Logical width (columns).
    pub fn width(&self) -> u16 {
        self.width
    }
    /// Logical height (rows).
    pub fn height(&self) -> u16 {
        self.height
    }
    /// The screen cells, in logical row-major order.
    pub fn cells(&self) -> &[(u16, u16)] {
        &self.cells
    }

    /// The screen cell at logical `(col, row)`, or `None` if out of range.
    pub fn cell(&self, col: u16, row: u16) -> Option<(u16, u16)> {
        if col >= self.width || row >= self.height {
            return None;
        }
        self.cells.get(row as usize * self.width as usize + col as usize).copied()
    }

    /// Paint every logical cell. `f(col, row)` returns the glyph and style to draw,
    /// or `None` to leave the cell untouched — the general per-cell hook that the
    /// video encoders and any future content source (text, CA, wave) build on.
    pub fn paint(&self, buf: &mut Buffer, mut f: impl FnMut(u16, u16) -> Option<(String, Style)>) {
        for row in 0..self.height {
            for col in 0..self.width {
                if let (Some((x, y)), Some((g, s))) = (self.cell(col, row), f(col, row)) {
                    buf.set_grapheme(x, y, &g, s);
                }
            }
        }
    }

    /// Render an image into the field as **braille** (2×4 sub-pixels per cell),
    /// **ordered-dithered** so the lit-dot density tracks brightness.
    ///
    /// `intensity(u, v)` samples the image at normalised field coordinates
    /// `u, v ∈ [0, 1]` (`0` = dark, `1` = light). Each sub-pixel lights when its
    /// sample beats a position-dependent threshold from a 4×4 Bayer matrix, so a
    /// mid-grey cell lights about half its dots in a dispersed pattern — interior
    /// gradients get texture instead of going solid, and the dither breaks up the
    /// hard horizontal/vertical banding of a plain threshold. `style(mean)` colours
    /// the cell from its mean intensity (return a fixed [`Style`] to ignore it); for
    /// colour that varies by position too, see
    /// [`render_braille_xy`](Field::render_braille_xy).
    pub fn render_braille(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        style: impl Fn(f32) -> Style,
    ) {
        self.render_braille_xy(buf, intensity, move |m, _u, _v| style(m));
    }

    /// Like [`render_braille`](Field::render_braille), but the colour closure also
    /// receives the cell's **position**: `style(mean, u, v)`, where `(u, v)` is the
    /// cell's normalised centre `((col + 0.5)/width, (row + 0.5)/height)` — the same
    /// `[0, 1]` space `intensity` is sampled in.
    ///
    /// Positional colour lets one image carry a multi-hue **scene**: blue sky above,
    /// orange sand below, brown cliffs straddling the horizon — the hue chosen by
    /// *where* a cell sits, not only how bright it is. The glyph (dot pattern) still
    /// comes from `intensity`; only the colour gains position.
    pub fn render_braille_xy(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        style: impl Fn(f32, f32, f32) -> Style,
    ) {
        // 4×4 Bayer matrix → 16 ordered dither levels, tiled over the sub-pixel grid.
        const BAYER: [[u8; 4]; 4] =
            [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
        let (sw, sh) = (self.width as f32 * 2.0, self.height as f32 * 4.0);
        for row in 0..self.height {
            for col in 0..self.width {
                let Some((x, y)) = self.cell(col, row) else { continue };
                let (mut mask, mut sum) = (0u8, 0.0f32);
                for sy in 0..4u16 {
                    for sx in 0..2u16 {
                        let (gx, gy) = (col * 2 + sx, row * 4 + sy); // global sub-pixel
                        let u = (gx as f32 + 0.5) / sw;
                        let v = (gy as f32 + 0.5) / sh;
                        let i = intensity(u, v).clamp(0.0, 1.0);
                        sum += i;
                        let thr = (BAYER[(gy % 4) as usize][(gx % 4) as usize] as f32 + 0.5) / 16.0;
                        if i > thr {
                            mask |= braille_bit(sx, sy);
                        }
                    }
                }
                let g = char::from_u32(0x2800 + mask as u32).unwrap_or(' ');
                let (cu, cv) = self.cell_centre(col, row);
                buf.set_char(x, y, g, style(sum / 8.0, cu, cv));
            }
        }
    }

    /// Render an image into the field as a **brightness ramp** — one glyph per cell
    /// from its mean intensity (e.g. [`BLOCK_RAMP`] or [`ASCII_RAMP`]).
    ///
    /// `intensity(u, v)` is as in [`render_braille`](Field::render_braille); the cell
    /// samples a small grid and picks `ramp[round(mean · (ramp.len()-1))]`. Coarser
    /// than braille but reads as glyphs you can recognise. `ramp` must be non-empty.
    /// For position-dependent colour, see [`render_ramp_xy`](Field::render_ramp_xy).
    pub fn render_ramp(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        ramp: &[char],
        style: impl Fn(f32) -> Style,
    ) {
        self.render_ramp_xy(buf, intensity, ramp, move |m, _u, _v| style(m));
    }

    /// Like [`render_ramp`](Field::render_ramp), but the colour closure also receives
    /// the cell's **position**: `style(mean, u, v)` with `(u, v)` the cell's
    /// normalised centre — see [`render_braille_xy`](Field::render_braille_xy) for why
    /// positional colour matters. The glyph still comes from the mean intensity.
    pub fn render_ramp_xy(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        ramp: &[char],
        style: impl Fn(f32, f32, f32) -> Style,
    ) {
        if ramp.is_empty() {
            return;
        }
        let (sw, sh) = (self.width as f32, self.height as f32);
        for row in 0..self.height {
            for col in 0..self.width {
                let Some((x, y)) = self.cell(col, row) else { continue };
                // Average a 2×2 sub-grid so the ramp is less point-sampled.
                let mut sum = 0.0f32;
                for sy in 0..2 {
                    for sx in 0..2 {
                        let u = (col as f32 + 0.25 + 0.5 * sx as f32) / sw;
                        let v = (row as f32 + 0.25 + 0.5 * sy as f32) / sh;
                        sum += intensity(u, v).clamp(0.0, 1.0);
                    }
                }
                let mean = sum / 4.0;
                let idx = ((mean * (ramp.len() - 1) as f32).round() as usize).min(ramp.len() - 1);
                let (cu, cv) = self.cell_centre(col, row);
                buf.set_char(x, y, ramp[idx], style(mean, cu, cv));
            }
        }
    }

    /// Render an image into the field with **structure-aware glyph matching** — a
    /// flat cell becomes a brightness `ramp` glyph, an *edge* cell becomes a
    /// directional stroke (`─ │ ╱ ╲`) running along the edge.
    ///
    /// For each cell this measures **density** (mean intensity → the ramp glyph) and
    /// the **gradient** (a 5-tap difference). When the gradient magnitude exceeds
    /// `edge` the cell is an edge, drawn as the stroke perpendicular to the gradient
    /// — so the result *evokes* the image's contours, not just its brightness. It is
    /// O(1) per cell (a handful of samples, then a direct map — no per-glyph search),
    /// fast enough to redraw the whole field every frame. `style(density)` colours
    /// the cell; for position-dependent colour, see
    /// [`render_glyphs_xy`](Field::render_glyphs_xy).
    pub fn render_glyphs(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        ramp: &[char],
        edge: f32,
        style: impl Fn(f32) -> Style,
    ) {
        self.render_glyphs_xy(buf, intensity, ramp, edge, move |d, _u, _v| style(d));
    }

    /// Like [`render_glyphs`](Field::render_glyphs), but the colour closure also
    /// receives the cell's **position**: `style(density, u, v)` with `(u, v)` the
    /// cell's normalised centre — see [`render_braille_xy`](Field::render_braille_xy)
    /// for why positional colour matters. The glyph still comes from `intensity`.
    pub fn render_glyphs_xy(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        ramp: &[char],
        edge: f32,
        style: impl Fn(f32, f32, f32) -> Style,
    ) {
        if ramp.is_empty() {
            return;
        }
        let (w, h) = (self.width as f32, self.height as f32);
        for row in 0..self.height {
            for col in 0..self.width {
                let Some((x, y)) = self.cell(col, row) else { continue };
                // 5-tap sample around the cell centre, offsets in cell units.
                let s = |du: f32, dv: f32| {
                    let u = (col as f32 + 0.5 + du) / w;
                    let v = (row as f32 + 0.5 + dv) / h;
                    intensity(u, v).clamp(0.0, 1.0)
                };
                let (l, r, up, dn, c) = (s(-0.4, 0.0), s(0.4, 0.0), s(0.0, -0.4), s(0.0, 0.4), s(0.0, 0.0));
                let density = (l + r + up + dn + c) / 5.0;
                let (gx, gy) = (r - l, dn - up);
                let glyph = if (gx * gx + gy * gy).sqrt() > edge {
                    edge_glyph(gx, gy)
                } else {
                    let idx = ((density * (ramp.len() - 1) as f32).round() as usize).min(ramp.len() - 1);
                    ramp[idx]
                };
                let (cu, cv) = self.cell_centre(col, row);
                buf.set_char(x, y, glyph, style(density, cu, cv));
            }
        }
    }

    /// The normalised centre `((col + 0.5)/width, (row + 0.5)/height)` of logical cell
    /// `(col, row)`, in the same `[0, 1]` space the intensity function is sampled in.
    fn cell_centre(&self, col: u16, row: u16) -> (f32, f32) {
        ((col as f32 + 0.5) / self.width as f32, (row as f32 + 0.5) / self.height as f32)
    }
}

/// The directional stroke that runs **along an edge** whose brightness gradient is
/// `(gx, gy)` (screen coordinates, `y` down): the edge is perpendicular to the
/// gradient, quantised to one of `─ ╲ │ ╱`.
fn edge_glyph(gx: f32, gy: f32) -> char {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    // Edge angle = gradient angle + 90°, taken mod π (an edge is undirected).
    let edge = (gy.atan2(gx) + FRAC_PI_2).rem_euclid(PI);
    match ((edge / FRAC_PI_4).round() as usize) % 4 {
        0 => '─',
        1 => '╲',
        2 => '│',
        _ => '╱',
    }
}

/// The braille dot bit for sub-pixel `(sx ∈ 0..2, sy ∈ 0..4)` — the standard
/// 2×4 dot numbering packed into `U+2800 + mask`.
fn braille_bit(sx: u16, sy: u16) -> u8 {
    match (sx, sy) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x40,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (1, 3) => 0x80,
        _ => 0,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TestBackend;
    use crate::Terminal;

    #[test]
    fn rect_maps_logical_to_screen() {
        let f = Field::rect(Rect::new(2, 3, 4, 2));
        assert_eq!((f.width(), f.height()), (4, 2));
        assert_eq!(f.cells().len(), 8);
        assert_eq!(f.cell(0, 0), Some((2, 3)));
        assert_eq!(f.cell(3, 1), Some((5, 4)));
        assert_eq!(f.cell(4, 0), None); // out of range
        assert_eq!(f.cell(0, 2), None);
    }

    #[test]
    fn strip_walks_the_path() {
        let f = Field::strip(vec![(1, 1), (2, 1), (3, 2), (3, 3)]);
        assert_eq!((f.width(), f.height()), (4, 1));
        assert_eq!(f.cell(0, 0), Some((1, 1)));
        assert_eq!(f.cell(2, 0), Some((3, 2)));
        assert_eq!(f.cell(3, 0), Some((3, 3)));
        assert_eq!(f.cell(0, 1), None);
    }

    #[test]
    fn braille_bits_pack_to_full_and_empty() {
        // All eight dots set → U+28FF (⣿); none → U+2800 (⠀).
        let mut mask = 0u8;
        for sy in 0..4 {
            for sx in 0..2 {
                mask |= braille_bit(sx, sy);
            }
        }
        assert_eq!(mask, 0xFF);
        assert_eq!(char::from_u32(0x2800 + 0xFF).unwrap(), '⣿');
        assert_eq!(char::from_u32(0x2800).unwrap(), '⠀');
    }

    #[test]
    fn braille_renders_full_and_empty_images() {
        let f = Field::rect(Rect::new(0, 0, 3, 2));
        let mut term = Terminal::new(TestBackend::new(3, 2)).unwrap();
        // All-white image → every cell is a full braille block (beats every Bayer
        // threshold).
        term.draw(|buf| f.render_braille(buf, |_, _| 1.0, |_| Style::default())).unwrap();
        for x in 0..3 {
            for y in 0..2 {
                assert_eq!(term.backend().buffer().get(x, y).symbol, "⣿");
            }
        }
        // All-black image → every cell is blank braille (beats none).
        term.draw(|buf| f.render_braille(buf, |_, _| 0.0, |_| Style::default())).unwrap();
        assert_eq!(term.backend().buffer().get(1, 1).symbol, "⠀");
    }

    #[test]
    fn ramp_picks_ends_for_extremes() {
        let f = Field::rect(Rect::new(0, 0, 2, 1));
        let mut term = Terminal::new(TestBackend::new(2, 1)).unwrap();
        term.draw(|buf| f.render_ramp(buf, |_, _| 1.0, &BLOCK_RAMP, |_| Style::default())).unwrap();
        assert_eq!(term.backend().buffer().get(0, 0).symbol, "█"); // brightest
        term.draw(|buf| f.render_ramp(buf, |_, _| 0.0, &BLOCK_RAMP, |_| Style::default())).unwrap();
        assert_eq!(term.backend().buffer().get(0, 0).symbol, " "); // darkest
    }

    #[test]
    fn glyph_matcher_maps_orientation_and_flatness() {
        let f = Field::rect(Rect::new(0, 0, 4, 4));
        let mut term = Terminal::new(TestBackend::new(4, 4)).unwrap();
        let sym = |t: &Terminal<TestBackend>| t.backend().buffer().get(1, 1).symbol.clone();
        // Brightness rising downward → vertical gradient → horizontal edge ─.
        term.draw(|buf| f.render_glyphs(buf, |_, v| v, &BLOCK_RAMP, 0.1, |_| Style::default())).unwrap();
        assert_eq!(sym(&term), "─");
        // Rising rightward → horizontal gradient → vertical edge │.
        term.draw(|buf| f.render_glyphs(buf, |u, _| u, &BLOCK_RAMP, 0.1, |_| Style::default())).unwrap();
        assert_eq!(sym(&term), "│");
        // Rising down-right → ↘ gradient → iso-lines run ╱ (perpendicular).
        term.draw(|buf| f.render_glyphs(buf, |u, v| (u + v) / 2.0, &BLOCK_RAMP, 0.05, |_| Style::default())).unwrap();
        assert_eq!(sym(&term), "╱");
        // Flat field → no edge → a ramp glyph for the density.
        term.draw(|buf| f.render_glyphs(buf, |_, _| 0.5, &BLOCK_RAMP, 0.1, |_| Style::default())).unwrap();
        assert!(BLOCK_RAMP.contains(&sym(&term).chars().next().unwrap()));
    }

    #[test]
    fn perimeter_traces_clockwise_corners_without_repeats() {
        // 4×3 box → border-cell count 2·(4+3) − 4 = 10, clockwise from the top-left.
        let f = Field::perimeter(Rect::new(0, 0, 4, 3));
        assert_eq!(f.width(), 10);
        assert_eq!(f.height(), 1);
        assert_eq!(
            f.cells(),
            &[
                (0, 0), (1, 0), (2, 0), (3, 0), // top → right
                (3, 1), (3, 2), // right edge down
                (2, 2), (1, 2), (0, 2), // bottom ← left
                (0, 1), // left edge up, back toward start
            ]
        );
        // Distinct cells (the loop is open — start not repeated).
        let mut uniq = f.cells().to_vec();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), f.cells().len());
    }

    #[test]
    fn perimeter_degenerate_row_and_column_are_straight_runs() {
        let row = Field::perimeter(Rect::new(2, 5, 4, 1));
        assert_eq!(row.cells(), &[(2, 5), (3, 5), (4, 5), (5, 5)]);
        let col = Field::perimeter(Rect::new(2, 5, 1, 3));
        assert_eq!(col.cells(), &[(2, 5), (2, 6), (2, 7)]);
    }

    #[test]
    fn xy_colour_varies_by_position() {
        use crate::style::Color;
        // A 4-wide field: cell centres u = 0.125, 0.375, 0.625, 0.875 → left two are
        // u < 0.5, right two u ≥ 0.5.
        let f = Field::rect(Rect::new(0, 0, 4, 2));
        let white = |_: f32, _: f32| 1.0;
        let split = |_value: f32, u: f32, _v: f32| {
            Style::default().fg(if u < 0.5 { Color::Red } else { Color::Blue })
        };
        let mut term = Terminal::new(TestBackend::new(4, 2)).unwrap();
        let fg = |t: &Terminal<TestBackend>, x, y| t.backend().buffer().get(x, y).style.fg;

        // The (u, v) reaches every encoder's colour closure and splits left/right.
        for label in ["braille", "ramp", "glyphs"] {
            match label {
                "braille" => term.draw(|buf| f.render_braille_xy(buf, white, split)).unwrap(),
                "ramp" => term.draw(|buf| f.render_ramp_xy(buf, white, &BLOCK_RAMP, split)).unwrap(),
                _ => term.draw(|buf| f.render_glyphs_xy(buf, white, &BLOCK_RAMP, 0.2, split)).unwrap(),
            }
            for y in 0..2 {
                assert_eq!(fg(&term, 0, y), Color::Red, "{label} col 0");
                assert_eq!(fg(&term, 1, y), Color::Red, "{label} col 1");
                assert_eq!(fg(&term, 2, y), Color::Blue, "{label} col 2");
                assert_eq!(fg(&term, 3, y), Color::Blue, "{label} col 3");
            }
        }
    }

    #[test]
    fn base_methods_match_their_xy_variant() {
        use crate::style::Color;
        // A gradient image so cells differ; the base method must produce exactly the
        // same buffer as its `_xy` variant with a position-ignoring colour closure.
        let f = Field::rect(Rect::new(0, 0, 5, 3));
        let img = |u: f32, _v: f32| u;
        let col = |m: f32| Style::default().fg(Color::Rgb((m * 255.0) as u8, 0, 0));
        let mut a = Terminal::new(TestBackend::new(5, 3)).unwrap();
        let mut b = Terminal::new(TestBackend::new(5, 3)).unwrap();
        let same = |a: &Terminal<TestBackend>, b: &Terminal<TestBackend>| {
            (0..5).all(|x| (0..3).all(|y| a.backend().buffer().get(x, y) == b.backend().buffer().get(x, y)))
        };

        a.draw(|buf| f.render_braille(buf, img, col)).unwrap();
        b.draw(|buf| f.render_braille_xy(buf, img, |m, _, _| col(m))).unwrap();
        assert!(same(&a, &b), "render_braille unchanged");

        a.draw(|buf| f.render_ramp(buf, img, &BLOCK_RAMP, col)).unwrap();
        b.draw(|buf| f.render_ramp_xy(buf, img, &BLOCK_RAMP, |m, _, _| col(m))).unwrap();
        assert!(same(&a, &b), "render_ramp unchanged");

        a.draw(|buf| f.render_glyphs(buf, img, &BLOCK_RAMP, 0.1, col)).unwrap();
        b.draw(|buf| f.render_glyphs_xy(buf, img, &BLOCK_RAMP, 0.1, |m, _, _| col(m))).unwrap();
        assert!(same(&a, &b), "render_glyphs unchanged");
    }

    #[test]
    fn paint_hits_every_cell_in_logical_order() {
        let f = Field::rect(Rect::new(0, 0, 2, 2));
        let mut seen = Vec::new();
        let mut term = Terminal::new(TestBackend::new(2, 2)).unwrap();
        term
            .draw(|buf| {
                f.paint(buf, |c, r| {
                    seen.push((c, r));
                    Some(("x".to_string(), Style::default()))
                })
            })
            .unwrap();
        assert_eq!(seen, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    use proptest::prelude::*;

    proptest! {
        /// A rect field's cells are exactly the area's cells, unique and in-bounds.
        #[test]
        fn prop_rect_cells_cover_area(x in 0u16..40, y in 0u16..40, w in 1u16..16, h in 1u16..16) {
            let area = Rect::new(x, y, w, h);
            let f = Field::rect(area);
            prop_assert_eq!(f.cells().len(), w as usize * h as usize);
            for &(cx, cy) in f.cells() {
                prop_assert!(area.contains(cx, cy));
            }
            // Unique.
            let set: std::collections::HashSet<_> = f.cells().iter().copied().collect();
            prop_assert_eq!(set.len(), f.cells().len());
        }
    }
}
