// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! A **video widget** — render a moving picture into terminal cells as faithfully as
//! the terminal allows, then (optionally) style it with **filters**.
//!
//! The widget's one job is *fidelity*: it samples a source frame and reproduces it
//! using truecolour and sub-cell resolution. Colour comes **from the picture**, never
//! decoration — a grey clip stays grey, a colour clip keeps its hues. Two cell
//! [`Encoding`]s trade detail against colour resolution:
//!
//! - [`Encoding::Braille`] — 2×4 luminance sub-pixels (ordered-dithered) tinted by the
//!   cell's average colour: the most spatial **detail**, one colour per cell.
//! - [`Encoding::HalfBlock`] — `▀` with the upper source pixel as foreground and the
//!   lower as background: full **colour** at 1×2 pixels per cell.
//!
//! Anything that *alters* the picture for effect — CRT scanlines, a vignette, a
//! phosphor tint, colour grading — is an opt-in [`Filter`], applied after sampling.
//! With no filters the output is a straight reproduction.
//!
//! The widget does **not** decode video: you supply pixels, either as a [`Frame`]
//! (a `W×H` RGB/luma buffer it samples bilinearly) or a `sample(u, v)` closure (so an
//! `ffmpeg` pipe, a camera, or a synthesised signal all work the same way).
//!
//! ```no_run
//! # use mullion::{Rect, video::{Video, Frame, Encoding, Filter}};
//! # let mut buf = mullion::Buffer::empty(Rect::new(0, 0, 40, 20));
//! # let (w, h) = (16usize, 16usize);
//! # let luma = vec![0u8; w * h];
//! let frame = Frame::from_luma(w, h, &luma);          // e.g. an ffmpeg `gray` frame
//! let tv = Video::new()                                // faithful by default…
//!     .encoding(Encoding::Braille)
//!     .filter(Filter::Scanlines(0.25))                 // …CRT look is opt-in
//!     .filter(Filter::Vignette(0.4));
//! tv.render_frame(&mut buf, Rect::new(0, 0, 40, 20), &frame);
//! ```

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::style::{Color, Style};

/// An 8-bit-per-channel RGB sample. Luma sources use `(g, g, g)`.
pub type Rgb = (u8, u8, u8);

/// A fixed-resolution source frame — a `width × height` grid of [`Rgb`] pixels, which
/// the widget samples **bilinearly** at normalised `(u, v)`, so one frame resamples to
/// any window size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<Rgb>,
}

impl Frame {
    /// A frame from a row-major RGB buffer (`pixels.len() == width · height`).
    pub fn from_rgb(width: usize, height: usize, pixels: Vec<Rgb>) -> Self {
        debug_assert_eq!(pixels.len(), width * height);
        Self { width, height, pixels }
    }

    /// A frame from a row-major **luma** buffer (one grey byte per pixel) — the shape
    /// `ffmpeg … -pix_fmt gray -f rawvideo` produces.
    pub fn from_luma(width: usize, height: usize, luma: &[u8]) -> Self {
        Self { width, height, pixels: luma.iter().map(|&g| (g, g, g)).collect() }
    }

    /// Frame width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }
    /// Frame height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Bilinearly sample the frame at normalised `(u, v) ∈ [0, 1]`; `(0, 0, 0)` for an
    /// empty frame.
    pub fn sample(&self, u: f32, v: f32) -> Rgb {
        if self.width == 0 || self.height == 0 {
            return (0, 0, 0);
        }
        let fx = (u.clamp(0.0, 1.0) * self.width as f32 - 0.5).max(0.0);
        let fy = (v.clamp(0.0, 1.0) * self.height as f32 - 0.5).max(0.0);
        let (x0, y0) = ((fx.floor() as usize).min(self.width - 1), (fy.floor() as usize).min(self.height - 1));
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let px = |x: usize, y: usize| self.pixels[y * self.width + x];
        let top = lerp_rgb(px(x0, y0), px(x1, y0), tx);
        let bot = lerp_rgb(px(x0, y1), px(x1, y1), tx);
        lerp_rgb(top, bot, ty)
    }
}

/// How a cell encodes the picture — a detail-vs-colour trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// 2×4 dithered luminance sub-pixels, one average colour per cell — most detail.
    #[default]
    Braille,
    /// `▀` with the upper pixel as fg and the lower as bg — full colour, 1×2 per cell.
    HalfBlock,
    /// Braille sub-pixels over a **chroma-carrying background** — a luma/chroma split.
    /// The eye resolves detail in luminance, not colour, so the dots are brightened
    /// toward white (the high-acuity luminance channel) while the cell *background*
    /// fills with the hue — dark areas then carry colour instead of black. Brighter
    /// and more detailed than [`Braille`](Encoding::Braille), at the cost of a per-cell
    /// background colour (≈2× the output bytes, like [`HalfBlock`](Encoding::HalfBlock)).
    LumaChroma,
    /// 2×3 solid sub-blocks split into **two colours** — the sub-blocks brighter than the
    /// cell's mean luminance become the foreground glyph (a Unicode *sextant*), the rest
    /// its background. Full colour at 2×3 sub-pixels per cell: the detail-vs-colour sweet
    /// spot, far closer to the source than one colour per cell. Where [`Braille`] and
    /// [`LumaChroma`] tint a *fixed* dot pattern with the average colour,
    /// [`Sextant`](Encoding::Sextant) lets the *shape* follow the picture — each cell is
    /// the two-tone block that best matches its pixels. Costs a per-cell background colour
    /// (≈2× the output bytes, like [`HalfBlock`](Encoding::HalfBlock)).
    ///
    /// [`Braille`]: Encoding::Braille
    /// [`LumaChroma`]: Encoding::LumaChroma
    Sextant,
}

/// How [`Encoding::Braille`] decides which sub-pixels light — the dither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dither {
    /// **Ordered** (4×4 Bayer): each sub-pixel thresholds against a fixed matrix.
    /// Cheap and **temporally stable** (no frame-to-frame shimmer) — preferable for fast
    /// motion — but leaves a regular cross-hatch in flat areas.
    Bayer,
    /// **Floyd–Steinberg error diffusion**: the quantisation error of each sub-pixel is
    /// scattered into its neighbours, dissolving the grid into an organic stipple. The
    /// **default** — highest fidelity and detail; can shimmer slightly in fast motion
    /// (switch to [`Bayer`](Dither::Bayer) for a clip where that shows).
    #[default]
    FloydSteinberg,
}

/// How a [`Frame`] is resampled to the cell grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sampling {
    /// **Bilinear**: blend the four nearest source pixels — smoothest, at about twice the
    /// per-frame cost of nearest. Choose it when fidelity matters more than speed.
    Bilinear,
    /// **Nearest**: take the single closest source pixel — the **default**: about half the
    /// sampling cost, with a minor quality loss that the dither largely hides. Best for
    /// video and large panels.
    #[default]
    Nearest,
}

/// An optional post-sample picture effect, applied per sample after the source colour
/// is read. With no filters the widget reproduces the source faithfully.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Filter {
    /// Dim alternate output lines for a **CRT scanline** look (`0.0..=1.0`).
    Scanlines(f32),
    /// Darken toward the edges (`0.0..=1.0` at the corners).
    Vignette(f32),
    /// Map luminance onto a single **phosphor** hue (CRT green/amber/blue): `hue` in
    /// degrees, `sat` in `0.0..=1.0`. Monochrome — an effect, not reproduction.
    Phosphor { hue: f32, sat: f32 },
    /// Per-channel **gamma** (`< 1` brightens midtones, `> 1` darkens).
    Gamma(f32),
    /// **Saturation** multiplier (`0` = grey, `1` = unchanged, `> 1` = punchier).
    Saturation(f32),
    /// Collapse to **greyscale** (luma).
    Grayscale,
}

impl Filter {
    /// Apply this effect to sample colour `c` at output line `line` (the vertical
    /// sub-pixel index, for scanlines) and normalised position `(u, v)`.
    fn apply(self, line: usize, u: f32, v: f32, c: Rgb) -> Rgb {
        match self {
            Filter::Scanlines(s) => {
                if line % 2 == 1 {
                    scale(c, 1.0 - s.clamp(0.0, 1.0))
                } else {
                    c
                }
            }
            Filter::Vignette(s) => {
                let (dx, dy) = (u - 0.5, v - 0.5);
                let d2 = ((dx * dx + dy * dy) / 0.5).min(1.0); // 0 at centre, 1 at corners
                scale(c, 1.0 - s.clamp(0.0, 1.0) * d2)
            }
            Filter::Phosphor { hue, sat } => phosphor_rgb(hue, sat, luma(c) / 255.0),
            Filter::Gamma(g) => {
                let m = |x: u8| clamp_u8(255.0 * (x as f32 / 255.0).powf(g));
                (m(c.0), m(c.1), m(c.2))
            }
            Filter::Saturation(s) => {
                let l = luma(c);
                let m = |x: u8| clamp_u8(l + (x as f32 - l) * s);
                (m(c.0), m(c.1), m(c.2))
            }
            Filter::Grayscale => {
                let l = luma(c) as u8;
                (l, l, l)
            }
        }
    }
}

/// The video widget: a cell [`Encoding`] plus an ordered list of [`Filter`]s. Cheap to
/// build — make one per frame, or keep one and re-render.
#[derive(Debug, Clone, Default)]
pub struct Video {
    encoding: Encoding,
    dither: Dither,
    sampling: Sampling,
    filters: Vec<Filter>,
}

impl Video {
    /// A widget tuned for video: [`Braille`](Encoding::Braille),
    /// [`FloydSteinberg`](Dither::FloydSteinberg) dither, [`Nearest`](Sampling::Nearest)
    /// sampling, no filters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose the cell [`Encoding`] (builder).
    pub fn encoding(mut self, encoding: Encoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Choose the braille [`Dither`] (builder); ignored by [`Encoding::HalfBlock`].
    pub fn dither(mut self, dither: Dither) -> Self {
        self.dither = dither;
        self
    }

    /// Choose the [`Frame`] [`Sampling`] (builder); only affects [`render_frame`]
    /// (a `sample` closure does its own sampling).
    ///
    /// [`render_frame`]: Self::render_frame
    pub fn sampling(mut self, sampling: Sampling) -> Self {
        self.sampling = sampling;
        self
    }

    /// Append a [`Filter`] (builder); filters apply in the order added.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Render a [`Frame`] into `area`, resampled per the configured [`Sampling`]. This
    /// is the fast path: the source-pixel taps are precomputed once per axis (the cell
    /// grid is regular) rather than re-derived per sub-pixel.
    pub fn render_frame(&self, buf: &mut Buffer, area: Rect, frame: &Frame) {
        if area.width == 0 || area.height == 0 || frame.width == 0 || frame.height == 0 {
            return;
        }
        let compiled = self.compile_filters();
        match self.encoding {
            Encoding::Braille | Encoding::LumaChroma => {
                let (gw, gh) = (area.width as usize * 2, area.height as usize * 4);
                let s = FrameSampler::new(frame, gw, gh, self.sampling);
                self.render_braille(buf, area, gw, gh, &compiled, |gx, gy| s.at(gx, gy));
            }
            Encoding::HalfBlock => {
                let (gw, gh) = (area.width as usize, area.height as usize * 2);
                let s = FrameSampler::new(frame, gw, gh, self.sampling);
                self.render_half_block(buf, area, gh, &compiled, |gx, gy| s.at(gx, gy));
            }
            Encoding::Sextant => {
                let (gw, gh) = (area.width as usize * 2, area.height as usize * 3);
                let s = FrameSampler::new(frame, gw, gh, self.sampling);
                self.render_sextant(buf, area, gw, gh, &compiled, |gx, gy| s.at(gx, gy));
            }
        }
    }

    /// Render into `area` from a `sample(u, v) -> Rgb` closure (`u, v ∈ [0, 1]`) — for
    /// a frame source that is not a [`Frame`] buffer (a live pipe, a procedural signal).
    pub fn render(&self, buf: &mut Buffer, area: Rect, sample: impl Fn(f32, f32) -> Rgb) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let compiled = self.compile_filters();
        match self.encoding {
            Encoding::Braille | Encoding::LumaChroma => {
                let (gw, gh) = (area.width as usize * 2, area.height as usize * 4);
                let (sw, sh) = (gw as f32, gh as f32);
                self.render_braille(buf, area, gw, gh, &compiled, |gx, gy| {
                    sample((gx as f32 + 0.5) / sw, (gy as f32 + 0.5) / sh)
                });
            }
            Encoding::HalfBlock => {
                let (gw, gh) = (area.width as usize, area.height as usize * 2);
                let (sw, sh) = (gw as f32, gh as f32);
                self.render_half_block(buf, area, gh, &compiled, |gx, gy| {
                    sample((gx as f32 + 0.5) / sw, (gy as f32 + 0.5) / sh)
                });
            }
            Encoding::Sextant => {
                let (gw, gh) = (area.width as usize * 2, area.height as usize * 3);
                let (sw, sh) = (gw as f32, gh as f32);
                self.render_sextant(buf, area, gw, gh, &compiled, |gx, gy| {
                    sample((gx as f32 + 0.5) / sw, (gy as f32 + 0.5) / sh)
                });
            }
        }
    }

    /// Compile the filter list, baking each [`Filter::Phosphor`] into a 256-entry
    /// luma→colour LUT (its `hue`/`sat` are fixed) so the per-sample work is a lookup,
    /// not a `from_hsv`. The LUT is indexed by luma rounded to an integer, so it
    /// quantises the tint's brightness to 256 steps — a ≤1-LSB approximation versus the
    /// continuous per-sample `Filter::Phosphor`, imperceptible for a monochrome effect.
    fn compile_filters(&self) -> Vec<CompiledFilter> {
        self.filters
            .iter()
            .map(|f| match *f {
                Filter::Phosphor { hue, sat } => {
                    let mut lut = [(0u8, 0u8, 0u8); 256];
                    for (i, slot) in lut.iter_mut().enumerate() {
                        *slot = phosphor_rgb(hue, sat, i as f32 / 255.0);
                    }
                    CompiledFilter::Phosphor(Box::new(lut))
                }
                other => CompiledFilter::Simple(other),
            })
            .collect()
    }
    fn render_braille(
        &self,
        buf: &mut Buffer,
        area: Rect,
        gw: usize,
        gh: usize,
        filters: &[CompiledFilter],
        grid: impl Fn(usize, usize) -> Rgb,
    ) {
        const BIT: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
        let (aw, ah) = (area.width as usize, area.height as usize);
        let (sw, sh) = (gw as f32, gh as f32);

        // One pass: sample + filter every sub-pixel, store its luma for dithering, and
        // accumulate each cell's average colour (8 sub-pixels per cell).
        let mut lum = vec![0.0f32; gw * gh];
        let mut cell_rgb = vec![(0u32, 0u32, 0u32); aw * ah];
        for gy in 0..gh {
            for gx in 0..gw {
                let (u, v) = ((gx as f32 + 0.5) / sw, (gy as f32 + 0.5) / sh);
                let c = shade(filters, gy, u, v, grid(gx, gy));
                lum[gy * gw + gx] = luma(c) / 255.0;
                let cell = &mut cell_rgb[(gy / 4) * aw + (gx / 2)];
                cell.0 += c.0 as u32;
                cell.1 += c.1 as u32;
                cell.2 += c.2 as u32;
            }
        }

        let lit = self.dither_bits(lum, gw, gh);

        for row in 0..ah {
            for col in 0..aw {
                let mut mask = 0u8;
                for sy in 0..4 {
                    for sx in 0..2 {
                        if lit[(row * 4 + sy) * gw + (col * 2 + sx)] {
                            mask |= BIT[sy][sx];
                        }
                    }
                }
                let g = char::from_u32(0x2800 + mask as u32).unwrap_or(' ');
                let (r, gn, b) = cell_rgb[row * aw + col];
                let avg = ((r / 8) as u8, (gn / 8) as u8, (b / 8) as u8);
                let style = if matches!(self.encoding, Encoding::LumaChroma) {
                    // Luma/chroma split: brighten the dots toward white (more where the
                    // cell is brighter, so shadows keep their hue and don't speckle) and
                    // fill the background with a dimmed version of the hue. The background
                    // keeps most of its value (0.72) so shadows carry visible colour
                    // rather than going near-black — tuned by eye on skin/foliage stills.
                    let y = luma(avg) / 255.0;
                    let fg = lerp_rgb(avg, (255, 255, 255), 0.4 * y);
                    let bg = scale(avg, 0.72);
                    Style::default().fg(Color::Rgb(fg.0, fg.1, fg.2)).bg(Color::Rgb(bg.0, bg.1, bg.2))
                } else {
                    Style::default().fg(Color::Rgb(avg.0, avg.1, avg.2))
                };
                buf.set_char(area.x + col as u16, area.y + row as u16, g, style);
            }
        }
    }

    /// Quantise the luma grid (`gw × gh`, row-major) to lit/unlit sub-pixels under the
    /// configured [`Dither`]. Floyd–Steinberg mutates `lum` as it diffuses error.
    fn dither_bits(&self, mut lum: Vec<f32>, gw: usize, gh: usize) -> Vec<bool> {
        const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
        let mut lit = vec![false; gw * gh];
        match self.dither {
            Dither::Bayer => {
                for gy in 0..gh {
                    for gx in 0..gw {
                        let thr = (BAYER[gy % 4][gx % 4] as f32 + 0.5) / 16.0;
                        lit[gy * gw + gx] = lum[gy * gw + gx] > thr;
                    }
                }
            }
            Dither::FloydSteinberg => {
                for gy in 0..gh {
                    for gx in 0..gw {
                        let i = gy * gw + gx;
                        let on = lum[i] > 0.5;
                        lit[i] = on;
                        let err = lum[i] - if on { 1.0 } else { 0.0 };
                        if gx + 1 < gw {
                            lum[i + 1] += err * 7.0 / 16.0;
                        }
                        if gy + 1 < gh {
                            if gx > 0 {
                                lum[i + gw - 1] += err * 3.0 / 16.0;
                            }
                            lum[i + gw] += err * 5.0 / 16.0;
                            if gx + 1 < gw {
                                lum[i + gw + 1] += err * 1.0 / 16.0;
                            }
                        }
                    }
                }
            }
        }
        lit
    }

    fn render_half_block(
        &self,
        buf: &mut Buffer,
        area: Rect,
        gh: usize,
        filters: &[CompiledFilter],
        grid: impl Fn(usize, usize) -> Rgb,
    ) {
        let (aw, ah) = (area.width as usize, area.height as usize);
        let (sw, sh) = (aw as f32, gh as f32);
        for row in 0..ah {
            for col in 0..aw {
                let u = (col as f32 + 0.5) / sw;
                let (gyt, gyb) = (row * 2, row * 2 + 1);
                let vt = (gyt as f32 + 0.5) / sh;
                let vb = (gyb as f32 + 0.5) / sh;
                let top = shade(filters, gyt, u, vt, grid(col, gyt));
                let bot = shade(filters, gyb, u, vb, grid(col, gyb));
                let style = Style::default().fg(Color::Rgb(top.0, top.1, top.2)).bg(Color::Rgb(bot.0, bot.1, bot.2));
                buf.set_char(area.x + col as u16, area.y + row as u16, '▀', style);
            }
        }
    }

    /// Render [`Encoding::Sextant`]: each cell is a 2×3 grid of solid sub-blocks split
    /// into two colours. The sub-blocks at or above the cell's mean luminance form the
    /// foreground (their average is the glyph's fg, the [`SEXTANT`] character with those
    /// bits set); the rest form the background (its average is the cell bg). A flat cell
    /// has an empty background group, so it falls back to a solid `█` of its one colour.
    fn render_sextant(
        &self,
        buf: &mut Buffer,
        area: Rect,
        gw: usize,
        gh: usize,
        filters: &[CompiledFilter],
        grid: impl Fn(usize, usize) -> Rgb,
    ) {
        let (aw, ah) = (area.width as usize, area.height as usize);
        let (sw, sh) = (gw as f32, gh as f32);
        for row in 0..ah {
            for col in 0..aw {
                // Sample the six sub-blocks (filtered), keeping colour and luma.
                let mut cols = [(0u8, 0u8, 0u8); 6];
                let mut lums = [0.0f32; 6];
                let mut mean = 0.0;
                for sy in 0..3 {
                    for sx in 0..2 {
                        let (gx, gy) = (col * 2 + sx, row * 3 + sy);
                        let (u, v) = ((gx as f32 + 0.5) / sw, (gy as f32 + 0.5) / sh);
                        let c = shade(filters, gy, u, v, grid(gx, gy));
                        let i = sy * 2 + sx;
                        cols[i] = c;
                        lums[i] = luma(c);
                        mean += lums[i];
                    }
                }
                mean /= 6.0;

                // Partition: sub-blocks at/above the mean luma are foreground. Bit
                // `sy*2 + sx` indexes the sub-block, matching the SEXTANT numbering.
                let mut mask = 0usize;
                let (mut fg, mut nf) = ((0u32, 0u32, 0u32), 0u32);
                let (mut bg, mut nb) = ((0u32, 0u32, 0u32), 0u32);
                for (i, &(r, g, b)) in cols.iter().enumerate() {
                    if lums[i] >= mean {
                        mask |= 1 << i;
                        fg.0 += r as u32; fg.1 += g as u32; fg.2 += b as u32; nf += 1;
                    } else {
                        bg.0 += r as u32; bg.1 += g as u32; bg.2 += b as u32; nb += 1;
                    }
                }
                // A flat cell lands every sub-block in one group — and float rounding of
                // the mean can put all six *below* it, emptying the foreground group — so
                // guard both counts (one is always non-zero, since `nf + nb == 6`). An
                // empty group borrows the other's colour; the glyph is then a solid fill
                // (`' '` over the bg, or `█`), so the cell reads as its one flat colour.
                let avg = |s: (u32, u32, u32), n: u32| ((s.0 / n) as u8, (s.1 / n) as u8, (s.2 / n) as u8);
                let (f, b) = match (nf, nb) {
                    (0, _) => { let c = avg(bg, nb); (c, c) }
                    (_, 0) => { let c = avg(fg, nf); (c, c) }
                    _ => (avg(fg, nf), avg(bg, nb)),
                };
                let style =
                    Style::default().fg(Color::Rgb(f.0, f.1, f.2)).bg(Color::Rgb(b.0, b.1, b.2));
                buf.set_char(area.x + col as u16, area.y + row as u16, SEXTANT[mask], style);
            }
        }
    }
}

/// Sextant glyphs indexed by a 6-bit sub-block mask — bit `row*2 + col` (row 0..3 top
/// to bottom, col 0..2 left to right). Most are the Unicode 13 *Symbols for Legacy
/// Computing* block sextants (`U+1FB00…`); the four masks Unicode unifies with older
/// Block Elements use those instead (`' '`, `▌`, `▐`, `█`). Generated from the Unicode
/// 16 character database (`BLOCK SEXTANT-…` names) — see commit notes.
#[rustfmt::skip]
const SEXTANT: [char; 64] = [
    ' ', '🬀', '🬁', '🬂', '🬃', '🬄', '🬅', '🬆',
    '🬇', '🬈', '🬉', '🬊', '🬋', '🬌', '🬍', '🬎',
    '🬏', '🬐', '🬑', '🬒', '🬓', '▌', '🬔', '🬕',
    '🬖', '🬗', '🬘', '🬙', '🬚', '🬛', '🬜', '🬝',
    '🬞', '🬟', '🬠', '🬡', '🬢', '🬣', '🬤', '🬥',
    '🬦', '🬧', '▐', '🬨', '🬩', '🬪', '🬫', '🬬',
    '🬭', '🬮', '🬯', '🬰', '🬱', '🬲', '🬳', '🬴',
    '🬵', '🬶', '🬷', '🬸', '🬹', '🬺', '🬻', '█',
];

// ── Filter compilation & sampling ──────────────────────────────────────────────────

/// A [`Filter`] prepared for the per-sample loop: most are applied as-is, but
/// [`Filter::Phosphor`] is baked into a luma→colour lookup table.
enum CompiledFilter {
    Simple(Filter),
    Phosphor(Box<[Rgb; 256]>),
}

impl CompiledFilter {
    fn apply(&self, line: usize, u: f32, v: f32, c: Rgb) -> Rgb {
        match self {
            CompiledFilter::Simple(f) => f.apply(line, u, v, c),
            CompiledFilter::Phosphor(lut) => lut[(luma(c) as usize).min(255)],
        }
    }
}

/// Apply the compiled filter pipeline to one sample, in order.
fn shade(filters: &[CompiledFilter], line: usize, u: f32, v: f32, mut c: Rgb) -> Rgb {
    for f in filters {
        c = f.apply(line, u, v, c);
    }
    c
}

/// One axis of a [`FrameSampler`]: the source-pixel tap(s) for an output coordinate.
/// For nearest, `lo == hi` and `frac == 0`.
struct AxisTap {
    lo: usize,
    hi: usize,
    frac: f32,
}

/// Resamples a [`Frame`] to the `gw × gh` cell grid with the taps precomputed **once
/// per axis** — the grid is regular, so the per-sub-pixel work is just the table reads,
/// not the `clamp`/`floor`/`mul` that a fresh `(u, v)` lookup would redo every time.
struct FrameSampler<'a> {
    frame: &'a Frame,
    xs: Vec<AxisTap>,
    ys: Vec<AxisTap>,
    bilinear: bool,
}

impl<'a> FrameSampler<'a> {
    fn new(frame: &'a Frame, gw: usize, gh: usize, sampling: Sampling) -> Self {
        let bilinear = matches!(sampling, Sampling::Bilinear);
        // The taps for output index `i` of `n_out` over a source axis of `n_in` pixels.
        let axis = |n_out: usize, n_in: usize| -> Vec<AxisTap> {
            (0..n_out)
                .map(|i| {
                    let centre = (i as f32 + 0.5) / n_out as f32 * n_in as f32;
                    if bilinear {
                        let f = (centre - 0.5).max(0.0);
                        let lo = (f.floor() as usize).min(n_in - 1);
                        let hi = (lo + 1).min(n_in - 1);
                        AxisTap { lo, hi, frac: f - lo as f32 }
                    } else {
                        let n = (centre as usize).min(n_in - 1);
                        AxisTap { lo: n, hi: n, frac: 0.0 }
                    }
                })
                .collect()
        };
        Self { frame, xs: axis(gw, frame.width), ys: axis(gh, frame.height), bilinear }
    }

    fn at(&self, gx: usize, gy: usize) -> Rgb {
        let (ax, ay) = (&self.xs[gx], &self.ys[gy]);
        let w = self.frame.width;
        let px = |x: usize, y: usize| self.frame.pixels[y * w + x];
        if !self.bilinear {
            return px(ax.lo, ay.lo);
        }
        let top = lerp_rgb(px(ax.lo, ay.lo), px(ax.hi, ay.lo), ax.frac);
        let bot = lerp_rgb(px(ax.lo, ay.hi), px(ax.hi, ay.hi), ax.frac);
        lerp_rgb(top, bot, ay.frac)
    }
}

// ── Colour helpers ────────────────────────────────────────────────────────────────

/// The phosphor tint for luminance `value ∈ [0, 1]` at fixed `hue`/`sat`.
fn phosphor_rgb(hue: f32, sat: f32, value: f32) -> Rgb {
    match Color::from_hsv(hue, sat, value) {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

/// Rec. 601 luma of an RGB sample (`0..=255`).
fn luma(c: Rgb) -> f32 {
    0.299 * c.0 as f32 + 0.587 * c.1 as f32 + 0.114 * c.2 as f32
}

/// Scale every channel by `f`, clamped to a byte.
fn scale(c: Rgb, f: f32) -> Rgb {
    (clamp_u8(c.0 as f32 * f), clamp_u8(c.1 as f32 * f), clamp_u8(c.2 as f32 * f))
}

/// Linear interpolation between two RGB samples, `t ∈ [0, 1]`.
fn lerp_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    // Interpolating two in-range bytes with t ∈ [0,1] never leaves [0,255], so no clamp.
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn clamp_u8(x: f32) -> u8 {
    x.clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_samples_bilinearly() {
        // A 2×2 checker of black/white; the centre averages to mid-grey.
        let f = Frame::from_rgb(2, 2, vec![(0, 0, 0), (255, 255, 255), (255, 255, 255), (0, 0, 0)]);
        let (r, g, b) = f.sample(0.5, 0.5);
        assert!((120..=135).contains(&r) && r == g && g == b, "centre {r},{g},{b}");
        assert_eq!(f.sample(0.0, 0.0), (0, 0, 0)); // top-left pixel
    }

    #[test]
    fn braille_fully_lights_a_bright_frame() {
        let frame = Frame::from_rgb(1, 1, vec![(255, 255, 255)]);
        let area = Rect::new(0, 0, 2, 2);
        let mut buf = Buffer::empty(area);
        Video::new().render_frame(&mut buf, area, &frame);
        // Max luma beats every dither threshold → all eight dots set → solid braille.
        assert_eq!(buf.get(0, 0).symbol, "⣿");
        assert_eq!(buf.get(0, 0).style.fg, Color::Rgb(255, 255, 255));
    }

    #[test]
    fn half_block_carries_two_colours_per_cell() {
        let frame = Frame::from_rgb(1, 2, vec![(255, 0, 0), (0, 0, 255)]); // top red, bottom blue
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        Video::new().encoding(Encoding::HalfBlock).render_frame(&mut buf, area, &frame);
        let cell = buf.get(0, 0);
        assert_eq!(cell.symbol, "▀");
        assert_eq!(cell.style.fg, Color::Rgb(255, 0, 0)); // upper pixel
        assert_eq!(cell.style.bg, Color::Rgb(0, 0, 255)); // lower pixel
    }

    #[test]
    fn no_filters_is_faithful_but_grayscale_removes_chroma() {
        let frame = Frame::from_rgb(1, 1, vec![(255, 0, 0)]);
        let area = Rect::new(0, 0, 1, 1);
        let mut a = Buffer::empty(area);
        Video::new().render_frame(&mut a, area, &frame);
        assert_eq!(a.get(0, 0).style.fg, Color::Rgb(255, 0, 0)); // untouched

        let mut b = Buffer::empty(area);
        Video::new().filter(Filter::Grayscale).render_frame(&mut b, area, &frame);
        if let Color::Rgb(r, g, bl) = b.get(0, 0).style.fg {
            assert!(r == g && g == bl, "grayscale should be neutral, got {r},{g},{bl}");
        } else {
            panic!("expected Rgb");
        }
    }

    #[test]
    fn floyd_steinberg_dithers_mid_grey_to_about_half() {
        // A flat 50%-grey frame: error diffusion should light roughly half the dots
        // (and neither all nor none), where ordered dither would tile a fixed pattern.
        let frame = Frame::from_luma(1, 1, &[128]);
        let area = Rect::new(0, 0, 8, 8);
        let mut buf = Buffer::empty(area);
        Video::new().dither(Dither::FloydSteinberg).render_frame(&mut buf, area, &frame);
        let mut lit = 0u32;
        for y in 0..8 {
            for x in 0..8 {
                lit += (buf.get(x, y).symbol.chars().next().unwrap() as u32 - 0x2800).count_ones();
            }
        }
        let total = 8 * 8 * 8; // cells × 8 dots
        assert!((total / 4..=3 * total / 4).contains(&lit), "FS lit {lit}/{total} should be ~half");
    }

    #[test]
    fn scanlines_dim_alternate_lines() {
        // White, half-block: the top pixel (line 0) stays bright, the bottom (line 1)
        // is dimmed — fg brighter than bg.
        let frame = Frame::from_luma(1, 2, &[255, 255]);
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        Video::new()
            .encoding(Encoding::HalfBlock)
            .filter(Filter::Scanlines(0.5))
            .render_frame(&mut buf, area, &frame);
        let cell = buf.get(0, 0);
        let bright = |c: Color| if let Color::Rgb(r, _, _) = c { r } else { 0 };
        assert!(bright(cell.style.fg) > bright(cell.style.bg), "scanline should dim the lower line");
    }

    #[test]
    fn nearest_sampling_picks_one_pixel_without_blending() {
        // 2×1 frame (left red, right blue) into a 4-wide half-block area: nearest must
        // pick exactly one source pixel per cell — never a blended colour.
        let frame = Frame::from_rgb(2, 1, vec![(255, 0, 0), (0, 0, 255)]);
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        Video::new()
            .encoding(Encoding::HalfBlock)
            .sampling(Sampling::Nearest)
            .render_frame(&mut buf, area, &frame);
        for x in 0..4 {
            let fg = buf.get(x, 0).style.fg;
            assert!(
                fg == Color::Rgb(255, 0, 0) || fg == Color::Rgb(0, 0, 255),
                "nearest must not blend, got {fg:?} at {x}"
            );
        }
        assert_eq!(buf.get(0, 0).style.fg, Color::Rgb(255, 0, 0));
        assert_eq!(buf.get(3, 0).style.fg, Color::Rgb(0, 0, 255));
    }

    #[test]
    fn luma_chroma_fills_the_background_with_hue() {
        // Braille leaves the cell background default (gaps show through to terminal bg);
        // LumaChroma fills it with a dimmed version of the cell's hue.
        let frame = Frame::from_rgb(1, 1, vec![(40, 160, 60)]); // green
        let area = Rect::new(0, 0, 2, 2);
        let mut braille = Buffer::empty(area);
        Video::new().render_frame(&mut braille, area, &frame);
        let mut lc = Buffer::empty(area);
        Video::new().encoding(Encoding::LumaChroma).render_frame(&mut lc, area, &frame);

        assert_eq!(braille.get(0, 0).style.bg, Style::default().bg, "braille leaves bg default");
        match lc.get(0, 0).style.bg {
            Color::Rgb(r, g, b) => assert!(g > r && g > b, "LumaChroma bg carries the green hue: {r},{g},{b}"),
            other => panic!("LumaChroma bg should be an Rgb fill, got {other:?}"),
        }
    }

    #[test]
    fn sextant_splits_a_cell_into_two_colours() {
        // One cell (2×3 sub-blocks): top row white, lower two rows black. The bright
        // sub-blocks become the fg glyph, the dark ones the bg — two colours, and the
        // glyph is the top-row sextant (not blank, not a full block).
        let frame = Frame::from_rgb(
            2, 3,
            vec![(255, 255, 255), (255, 255, 255), (0, 0, 0), (0, 0, 0), (0, 0, 0), (0, 0, 0)],
        );
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        Video::new().encoding(Encoding::Sextant).sampling(Sampling::Nearest).render_frame(&mut buf, area, &frame);
        let cell = buf.get(0, 0);
        assert_eq!(cell.style.fg, Color::Rgb(255, 255, 255), "fg is the bright group");
        assert_eq!(cell.style.bg, Color::Rgb(0, 0, 0), "bg is the dark group");
        assert_eq!(cell.symbol, "🬂", "top-row sextant (BLOCK SEXTANT-12)");
    }

    #[test]
    fn sextant_renders_a_flat_cell_as_a_solid_block() {
        // A uniform cell has no dark group, so it falls back to a solid `█` of its colour.
        let frame = Frame::from_rgb(1, 1, vec![(40, 160, 60)]);
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        Video::new().encoding(Encoding::Sextant).render_frame(&mut buf, area, &frame);
        let cell = buf.get(0, 0);
        assert_eq!(cell.symbol, "█", "flat cell is a full block");
        assert_eq!(cell.style.fg, Color::Rgb(40, 160, 60));
    }

    #[test]
    fn sextant_flat_cells_render_solid_without_dividing_by_zero() {
        // A uniform frame gives every sub-block the same luma, so float rounding of the
        // mean can leave one colour group empty. Each cell must render as a single solid
        // colour (fg == bg), never divide by zero. `(0, 0, 255)` is the exact synth
        // colour bar that crashed `render_sextant` (its luma's 6-fold sum rounds *above*
        // itself, emptying the foreground group); the others guard nearby cases.
        let area = Rect::new(0, 0, 5, 5);
        for c in [(0, 0, 255), (0, 0, 25), (0, 0, 90), (0, 0, 0), (255, 255, 255), (100, 150, 200)] {
            let frame = Frame::from_rgb(2, 2, vec![c, c, c, c]);
            let mut buf = Buffer::empty(area);
            Video::new().encoding(Encoding::Sextant).render_frame(&mut buf, area, &frame);
            for y in 0..5 {
                for x in 0..5 {
                    let cell = buf.get(x, y);
                    assert_eq!(cell.style.fg, cell.style.bg, "flat cell {c:?} should be one solid colour");
                }
            }
        }
    }

    #[test]
    fn phosphor_lut_matches_direct_filter_on_integer_luma() {
        // For grey inputs luma is an exact integer, so the LUT (indexed by integer
        // luma) equals the continuous per-sample filter exactly. For colour inputs the
        // LUT quantises luma to 256 steps — a ≤1-LSB approximation, by design.
        let filter = Filter::Phosphor { hue: 120.0, sat: 0.5 };
        let compiled = Video::new().filter(filter).compile_filters();
        for g in [0u8, 64, 128, 200, 255] {
            let c = (g, g, g);
            assert_eq!(compiled[0].apply(0, 0.0, 0.0, c), filter.apply(0, 0.0, 0.0, c));
        }
    }
}
