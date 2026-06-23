// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Animated **colour sources** for a [`Field`](crate::field::Field) — a value per
//! cell that evolves over time and maps, through a [`Palette`], to a colour.
//!
//! A [`Field`](crate::field::Field) carries a glyph and a colour **independently**:
//! the glyph can come from text or the video unit while the colour comes from a
//! source here, "shining through" the glyphs. Three sources:
//!
//! - [`Flame`] — a stateful **cellular automaton** (the classic "doom fire"): a heat
//!   grid sourced hot along the bottom row and propagated upward with random cooling
//!   and drift, [`step`](Flame::step)ped once per frame. Reproducible from a seed.
//! - [`Reaction`] — a stateful **reaction-diffusion** automaton (Gray-Scott): grows
//!   organic Turing patterns (spots, mazes, coral) from two diffusing, reacting
//!   chemicals. Reproducible from a seed.
//! - [`Wave`] — a stateless **analytic** field: summed travelling sinusoids (a plasma
//!   or a waving flag), sampled at `(u, v, t)`.
//!
//! All three yield a value in `[0, 1]`; [`Palette`] turns that into a [`Color`] (a fire
//! ramp, an ice ramp, or a rainbow). Neither knows about `Field` — you read the value
//! per cell inside a `paint` or `render_*_xy` closure and colour as you like.
//!
//! ```no_run
//! # use mullion::{Field, Rect, colorfield::{Flame, Palette}, style::Style};
//! # let mut buf = mullion::Buffer::empty(Rect::new(0, 0, 10, 5));
//! let field = Field::rect(Rect::new(0, 0, 10, 5));
//! let mut fire = Flame::new(field.width(), field.height());
//! fire.step(0.18); // once per frame
//! field.paint(&mut buf, |col, row| {
//!     let heat = fire.at(col, row);
//!     Some(("█".into(), Style::default().fg(Palette::Fire.color(heat))))
//! });
//! ```

use crate::style::Color;

/// A stateless **wave** colour source: a sum of travelling sinusoids sampled at
/// normalised `(u, v)` and time `t`, yielding a value in `[0, 1]`. A plasma or a
/// waving-flag shimmer to shine through a field's glyphs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wave {
    /// Spatial frequency — how many wavefronts span the field.
    pub freq: f32,
    /// Temporal speed — how fast the pattern travels with `t`.
    pub speed: f32,
}

impl Wave {
    /// A roiling **plasma** (higher frequency, isotropic).
    pub fn plasma() -> Self {
        Self { freq: 6.0, speed: 1.0 }
    }
    /// A slower, longer-wavelength **flag** ripple.
    pub fn flag() -> Self {
        Self { freq: 3.5, speed: 1.5 }
    }
    /// Sample the field at normalised `(u, v) ∈ [0, 1]` and time `t`; returns `[0, 1]`.
    pub fn value(&self, u: f32, v: f32, t: f32) -> f32 {
        let (f, s) = (self.freq, self.speed);
        let sum = (u * f + t * s).sin()
            + (v * f * 0.8 - t * s * 0.7).sin()
            + ((u + v) * f * 0.6 + t * s * 1.3).sin();
        (sum / 6.0 + 0.5).clamp(0.0, 1.0)
    }
}

/// A **flame** cellular automaton — the classic "doom fire". A heat grid is held hot
/// along the bottom row and, each [`step`](Flame::step), propagated upward with random
/// cooling and a one-cell horizontal drift, so flame tongues rise and flicker. The
/// grid is one heat value (`[0, 1]`) per logical cell; reproducible from a seed.
#[derive(Debug, Clone)]
pub struct Flame {
    width: u16,
    height: u16,
    heat: Vec<f32>,
    rng: u64,
}

impl Flame {
    /// A fresh flame grid of `width × height` cells (cold but for the hot source row),
    /// with a fixed default seed.
    pub fn new(width: u16, height: u16) -> Self {
        Self::seeded(width, height, 0x2545_F491_4F6C_DD1D)
    }

    /// As [`new`](Flame::new), but with an explicit non-zero PRNG `seed` so the
    /// animation is reproducible (a `0` seed is bumped to the default).
    pub fn seeded(width: u16, height: u16, seed: u64) -> Self {
        let mut f = Self {
            width,
            height,
            heat: vec![0.0; width as usize * height as usize],
            rng: if seed == 0 { 0x2545_F491_4F6C_DD1D } else { seed },
        };
        f.light_source();
        f
    }

    /// Logical width (columns).
    pub fn width(&self) -> u16 {
        self.width
    }
    /// Logical height (rows).
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The heat at logical cell `(col, row)` in `[0, 1]`; `0.0` if out of range.
    pub fn at(&self, col: u16, row: u16) -> f32 {
        if col >= self.width || row >= self.height {
            return 0.0;
        }
        self.heat[row as usize * self.width as usize + col as usize]
    }

    /// Advance the fire one frame. `cooling` (≈ `0.05..0.3`) is the maximum heat a cell
    /// loses rising one row — higher burns the flame shorter. The bottom row stays the
    /// hot source; every row above takes the cell below it (drifted ±1 column) minus a
    /// random share of `cooling`.
    pub fn step(&mut self, cooling: f32) {
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h < 2 {
            return;
        }
        self.light_source();
        // Process top-down so each row reads the row below it at its *previous* value.
        for y in 0..h - 1 {
            for x in 0..w {
                let below = self.heat[(y + 1) * w + x];
                let decay = xorshift01(&mut self.rng) * cooling;
                let drift = (xorshift01(&mut self.rng) * 3.0) as i32 - 1; // -1, 0, +1
                let nx = (x as i32 + drift).clamp(0, w as i32 - 1) as usize;
                self.heat[y * w + nx] = (below - decay).max(0.0);
            }
        }
    }

    /// Hold the bottom row at full heat (the fire's fuel).
    fn light_source(&mut self) {
        if self.height == 0 {
            return;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        for x in 0..w {
            self.heat[(h - 1) * w + x] = 1.0;
        }
    }
}

/// xorshift64 → `[0, 1)`. Advances `state` (which must be non-zero).
fn xorshift01(state: &mut u64) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 40) as f32 / (1u64 << 24) as f32
}

/// A **reaction-diffusion** colour source — the Gray-Scott model, which grows the
/// organic **Turing patterns** behind animal markings: drifting spots, dividing
/// blobs, mazes, coral.
///
/// Two virtual chemicals `U` and `V` live on the grid. Each [`step`](Reaction::step)
/// they **diffuse** (a 3×3 Laplacian blur, `U` faster than `V`) and **react**
/// (`U + 2V → 3V`), with `U` fed in everywhere at rate `feed` and `V` removed at rate
/// `feed + kill`. The tug-of-war between fast-spreading `U` and slow-clumping `V` is
/// what self-organises into patterns instead of smoothing to a uniform grey. The
/// presets [`SPOTS`](Reaction::SPOTS), [`MITOSIS`](Reaction::MITOSIS),
/// [`MAZE`](Reaction::MAZE), [`CORAL`](Reaction::CORAL) are `(feed, kill)` pairs —
/// the patterns live or die by those two numbers.
///
/// [`at`](Reaction::at) reads `V ∈ [0, 1]`; the field is seeded with a central blob
/// (plus a little PRNG noise to break symmetry) and needs a few hundred steps to
/// bloom, so a demo steps it several times per frame. Reproducible from a `seed`. The
/// grid wraps toroidally at the edges.
#[derive(Debug, Clone)]
pub struct Reaction {
    width: u16,
    height: u16,
    u: Vec<f32>,
    v: Vec<f32>,
    // Double buffers so a step reads the previous state while writing the next.
    next_u: Vec<f32>,
    next_v: Vec<f32>,
}

impl Reaction {
    /// `(feed, kill)` for drifting **spots**.
    pub const SPOTS: (f32, f32) = (0.030, 0.062);
    /// `(feed, kill)` for dividing blobs (**mitosis**).
    pub const MITOSIS: (f32, f32) = (0.0367, 0.0649);
    /// `(feed, kill)` for **maze** / fingerprint labyrinths.
    pub const MAZE: (f32, f32) = (0.029, 0.057);
    /// `(feed, kill)` for branching **coral**.
    pub const CORAL: (f32, f32) = (0.0545, 0.062);

    /// A fresh grid of `width × height` cells, seeded with a default PRNG.
    pub fn new(width: u16, height: u16) -> Self {
        Self::seeded(width, height, 0x9E37_79B9_7F4A_7C15)
    }

    /// As [`new`](Reaction::new), but with an explicit non-zero PRNG `seed` so the
    /// pattern is reproducible (a `0` seed is bumped to the default).
    pub fn seeded(width: u16, height: u16, seed: u64) -> Self {
        let n = width as usize * height as usize;
        let mut r = Self {
            width,
            height,
            u: vec![1.0; n], // U fills the dish; V is introduced by the seed
            v: vec![0.0; n],
            next_u: vec![0.0; n],
            next_v: vec![0.0; n],
        };
        r.seed(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed });
        r
    }

    /// Logical width (columns).
    pub fn width(&self) -> u16 {
        self.width
    }
    /// Logical height (rows).
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The `V` concentration at logical cell `(col, row)` in `[0, 1]`; `0.0` if out of
    /// range. This is the value to colour by.
    pub fn at(&self, col: u16, row: u16) -> f32 {
        if col >= self.width || row >= self.height {
            return 0.0;
        }
        self.v[row as usize * self.width as usize + col as usize]
    }

    /// Advance the reaction one step under a `(feed, kill)` pair (e.g.
    /// [`Reaction::MAZE`]). Diffusion rates are fixed (`U` at `1.0`, `V` at `0.5`);
    /// patterns are tuned entirely by `feed`/`kill`. Call several times per frame to
    /// bloom faster.
    pub fn step(&mut self, feed: f32, kill: f32) {
        const DU: f32 = 1.0;
        const DV: f32 = 0.5;
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h == 0 {
            return;
        }
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let (lu, lv) = self.laplacian(x, y);
                let (u, v) = (self.u[i], self.v[i]);
                let uvv = u * v * v;
                self.next_u[i] = (u + DU * lu - uvv + feed * (1.0 - u)).clamp(0.0, 1.0);
                self.next_v[i] = (v + DV * lv + uvv - (kill + feed) * v).clamp(0.0, 1.0);
            }
        }
        std::mem::swap(&mut self.u, &mut self.next_u);
        std::mem::swap(&mut self.v, &mut self.next_v);
    }

    /// The 3×3 Laplacian of `U` and `V` at `(x, y)` — the standard Gray-Scott kernel
    /// (centre `−1`, orthogonal neighbours `0.2`, diagonals `0.05`), edges wrapping.
    fn laplacian(&self, x: usize, y: usize) -> (f32, f32) {
        let (w, h) = (self.width as usize, self.height as usize);
        let (xm, xp) = ((x + w - 1) % w, (x + 1) % w);
        let (ym, yp) = ((y + h - 1) % h, (y + 1) % h);
        let f = &self.u;
        let g = &self.v;
        let idx = |xx: usize, yy: usize| yy * w + xx;
        let lap = |a: &[f32]| {
            -a[idx(x, y)]
                + 0.2 * (a[idx(xm, y)] + a[idx(xp, y)] + a[idx(x, ym)] + a[idx(x, yp)])
                + 0.05 * (a[idx(xm, ym)] + a[idx(xp, ym)] + a[idx(xm, yp)] + a[idx(xp, yp)])
        };
        (lap(f), lap(g))
    }

    /// Seed a central blob of `V` (with `U` halved there) plus sparse random specks, so
    /// the pattern grows from a few nuclei rather than a symmetric mandala.
    fn seed(&mut self, seed: u64) {
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h == 0 {
            return;
        }
        let r = (w.min(h) / 8).max(3);
        let (cx, cy) = (w / 2, h / 2);
        for y in cy.saturating_sub(r)..(cy + r).min(h) {
            for x in cx.saturating_sub(r)..(cx + r).min(w) {
                let i = y * w + x;
                self.u[i] = 0.5;
                self.v[i] = 1.0;
            }
        }
        let mut rng = seed;
        for i in 0..w * h {
            if xorshift01(&mut rng) < 0.0008 {
                self.u[i] = 0.5;
                self.v[i] = 1.0;
            }
        }
    }
}

/// Maps a colour source's `[0, 1]` value to a [`Color`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// Black → red → orange → yellow → white (for [`Flame`]).
    Fire,
    /// Black → blue → cyan → white.
    Ice,
    /// Full-spectrum hue sweep, brightening with the value.
    Rainbow,
}

impl Palette {
    /// The colour for value `v` (clamped to `[0, 1]`).
    pub fn color(self, v: f32) -> Color {
        let v = v.clamp(0.0, 1.0);
        let byte = |x: f32| (x.clamp(0.0, 1.0) * 255.0) as u8;
        match self {
            Palette::Fire => Color::Rgb(byte(v * 3.0), byte((v - 0.33) * 2.2), byte((v - 0.75) * 4.0)),
            Palette::Ice => Color::Rgb(byte((v - 0.7) * 3.0), byte((v - 0.4) * 2.0), byte(v * 2.0)),
            Palette::Rainbow => Color::from_hsv(v * 300.0, 0.85, 0.4 + 0.6 * v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flame_source_is_hot_and_cools_upward() {
        let mut f = Flame::new(16, 12);
        for _ in 0..40 {
            f.step(0.15);
        }
        // Bottom row (the fuel) is full heat.
        for x in 0..f.width() {
            assert_eq!(f.at(x, f.height() - 1), 1.0);
        }
        // The flame is cooler higher up: top-row mean well below the source.
        let top: f32 = (0..f.width()).map(|x| f.at(x, 0)).sum::<f32>() / f.width() as f32;
        assert!(top < 0.5, "top mean {top} should be much cooler than the 1.0 source");
    }

    #[test]
    fn flame_is_deterministic_from_a_seed() {
        let mut a = Flame::seeded(20, 10, 12345);
        let mut b = Flame::seeded(20, 10, 12345);
        for _ in 0..25 {
            a.step(0.2);
            b.step(0.2);
        }
        for row in 0..10 {
            for col in 0..20 {
                assert_eq!(a.at(col, row), b.at(col, row));
            }
        }
    }

    #[test]
    fn flame_at_is_zero_out_of_range() {
        let f = Flame::new(4, 4);
        assert_eq!(f.at(4, 0), 0.0);
        assert_eq!(f.at(0, 4), 0.0);
    }

    #[test]
    fn reaction_uniform_field_stays_uniform() {
        // With no spatial variation the Laplacian is 0 everywhere, so every cell
        // evolves identically — a check that diffusion/reaction are spatially correct.
        let mut r = Reaction::new(8, 6);
        for c in r.u.iter_mut() {
            *c = 0.7;
        }
        for c in r.v.iter_mut() {
            *c = 0.2;
        }
        let (f, k) = Reaction::MAZE;
        r.step(f, k);
        let first = r.at(0, 0);
        for row in 0..6 {
            for col in 0..8 {
                assert!((r.at(col, row) - first).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn reaction_is_deterministic_from_a_seed() {
        let mut a = Reaction::seeded(24, 16, 777);
        let mut b = Reaction::seeded(24, 16, 777);
        let (f, k) = Reaction::CORAL;
        for _ in 0..30 {
            a.step(f, k);
            b.step(f, k);
        }
        for row in 0..16 {
            for col in 0..24 {
                assert_eq!(a.at(col, row), b.at(col, row));
            }
        }
    }

    #[test]
    fn reaction_sustains_a_non_uniform_pattern() {
        // After enough maze steps the field is neither dead nor saturated: some cells
        // hold high V and some near-zero — a structured pattern, not uniform grey.
        let mut r = Reaction::seeded(48, 32, 12345);
        let (f, k) = Reaction::MAZE;
        for _ in 0..120 {
            r.step(f, k);
        }
        let mut hi = false;
        let mut lo = false;
        for row in 0..r.height() {
            for col in 0..r.width() {
                let v = r.at(col, row);
                assert!((0.0..=1.0).contains(&v));
                hi |= v > 0.2;
                lo |= v < 0.05;
            }
        }
        assert!(hi && lo, "expected a structured pattern (hi={hi}, lo={lo})");
    }

    #[test]
    fn reaction_at_is_zero_out_of_range() {
        let r = Reaction::new(4, 4);
        assert_eq!(r.at(4, 0), 0.0);
        assert_eq!(r.at(0, 4), 0.0);
    }

    #[test]
    fn wave_value_stays_in_unit_range() {
        let w = Wave::plasma();
        for i in 0..50 {
            let (u, v, t) = (i as f32 / 50.0, (i as f32 * 0.37) % 1.0, i as f32 * 0.1);
            let val = w.value(u, v, t);
            assert!((0.0..=1.0).contains(&val), "value {val} out of range");
        }
    }

    #[test]
    fn palette_fire_runs_dark_to_bright() {
        let brightness = |c: Color| match c {
            Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32,
            _ => unreachable!(),
        };
        assert!(brightness(Palette::Fire.color(0.0)) < brightness(Palette::Fire.color(0.5)));
        assert!(brightness(Palette::Fire.color(0.5)) < brightness(Palette::Fire.color(1.0)));
    }
}
