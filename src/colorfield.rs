// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Animated **colour sources** for a [`Field`](crate::field::Field) — a value per
//! cell that evolves over time and maps, through a [`Palette`], to a colour.
//!
//! A [`Field`](crate::field::Field) carries a glyph and a colour **independently**:
//! the glyph can come from text or the video unit while the colour comes from a
//! source here, "shining through" the glyphs. Two sources:
//!
//! - [`Flame`] — a stateful **cellular automaton** (the classic "doom fire"): a heat
//!   grid sourced hot along the bottom row and propagated upward with random cooling
//!   and drift, [`step`](Flame::step)ped once per frame. Reproducible from a seed.
//! - [`Wave`] — a stateless **analytic** field: summed travelling sinusoids (a plasma
//!   or a waving flag), sampled at `(u, v, t)`.
//!
//! Both yield a value in `[0, 1]`; [`Palette`] turns that into a [`Color`] (a fire
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
                let decay = self.rand01() * cooling;
                let drift = (self.rand01() * 3.0) as i32 - 1; // -1, 0, +1
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

    /// xorshift64 → `[0, 1)`. Advances the internal PRNG.
    fn rand01(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
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
