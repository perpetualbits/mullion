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
    pub fn strip(cells: Vec<(u16, u16)>) -> Self {
        let width = cells.len() as u16;
        Self { cells, width, height: 1 }
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
    /// the cell from its mean intensity (return a fixed [`Style`] to ignore it).
    pub fn render_braille(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        style: impl Fn(f32) -> Style,
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
                buf.set_grapheme(x, y, &g.to_string(), style(sum / 8.0));
            }
        }
    }

    /// Render an image into the field as a **brightness ramp** — one glyph per cell
    /// from its mean intensity (e.g. [`BLOCK_RAMP`] or [`ASCII_RAMP`]).
    ///
    /// `intensity(u, v)` is as in [`render_braille`](Field::render_braille); the cell
    /// samples a small grid and picks `ramp[round(mean · (ramp.len()-1))]`. Coarser
    /// than braille but reads as glyphs you can recognise. `ramp` must be non-empty.
    pub fn render_ramp(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        ramp: &[char],
        style: impl Fn(f32) -> Style,
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
                buf.set_grapheme(x, y, &ramp[idx].to_string(), style(mean));
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
    /// the cell.
    pub fn render_glyphs(
        &self,
        buf: &mut Buffer,
        intensity: impl Fn(f32, f32) -> f32,
        ramp: &[char],
        edge: f32,
        style: impl Fn(f32) -> Style,
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
                buf.set_grapheme(x, y, &glyph.to_string(), style(density));
            }
        }
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
