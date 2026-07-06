// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! A reusable **space-filling-curve map view** over a [`Gilbert`](crate::spacefill::Gilbert)
//! curve: lay a 1-D sequence of `N` cells on a curve that fills a rectangle, and draw each
//! cell in a caller-supplied colour with the curve's own path glyphs.
//!
//! The module is content-agnostic — it knows a **cell count** and a **per-cell colour**, and
//! nothing about what a cell means. A program decides that a cell is an address block, a
//! cluster, a file, a pixel bucket, …; here it is just index `d ∈ 0..N` on the curve.
//!
//! Three pieces cooperate:
//! - [`fit_dims`] picks the grid `(width, height)` for the drawable rectangle so every cell
//!   covers the same number of items and the curve stays unbroken;
//! - [`cell_glyph`] gives the rounded box-drawing glyph joining a cell to its neighbours on
//!   the curve (`─ │ ╭ ╮ ╰ ╯`), so the serpentine path is visible;
//! - [`render`] paints the whole grid — the glyph in each cell over its `(fg, bg)`.
//!
//! Each cell is **two columns wide and one row tall**; the second column continues the line
//! (`─`) when the curve proceeds right, else it is blank, so a horizontal run reads unbroken.
//!
//! ```
//! use mullion::curve_map;
//! use mullion::spacefill::Gilbert;
//! use mullion::style::Color;
//! use mullion::{Buffer, Rect};
//!
//! let area = Rect::new(0, 0, 32, 16);
//! // 64 items → a power-of-two grid dividing 64 that fits the 16-wide (×2), 16-tall area.
//! let (w, h) = curve_map::fit_dims(area, 64);
//! assert_eq!(w as u128 * h as u128, 64); // every cell holds exactly one item
//! let g = Gilbert::new(w, h);
//! let mut buf = Buffer::empty(area);
//! // Paint every cell the same here; a real caller colours by occupancy/identity.
//! curve_map::render(&mut buf, area, &g, |_d| (Color::Rgb(220, 220, 220), Color::Reset));
//! ```

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::spacefill::Gilbert;
use crate::style::{Color, Style};

/// A grid step from one cell to a 4-adjacent neighbour.
#[derive(Clone, Copy, PartialEq)]
enum Dir {
    L,
    R,
    U,
    D,
}

/// The direction from grid cell `a` to cell `b`, or `None` if they are not 4-adjacent.
fn dir_between(a: (u32, u32), b: (u32, u32)) -> Option<Dir> {
    match (i64::from(b.0) - i64::from(a.0), i64::from(b.1) - i64::from(a.1)) {
        (1, 0) => Some(Dir::R),
        (-1, 0) => Some(Dir::L),
        (0, 1) => Some(Dir::D),
        (0, -1) => Some(Dir::U),
        _ => None,
    }
}

/// The two curve ports of cell `d`: the direction toward its previous cell and toward its
/// next cell (each `None` at a curve endpoint, or for a lone cell). A `Gilbert` curve is
/// contiguous, so an interior cell's two neighbours are always 4-adjacent.
fn ports(g: &Gilbert, d: usize) -> (Option<Dir>, Option<Dir>) {
    let cur = g.d_to_xy(d);
    let prev = (d > 0).then(|| dir_between(cur, g.d_to_xy(d - 1))).flatten();
    let next = (d + 1 < g.len()).then(|| dir_between(cur, g.d_to_xy(d + 1))).flatten();
    (prev, next)
}

/// The rounded box-drawing glyph joining the two ports `a` and `b`: `─ │` for a straight,
/// `╭ ╮ ╰ ╯` for a turn, a single stroke for a curve endpoint (one port), and `·` for a
/// lone cell (no ports).
fn glyph_from_ports(a: Option<Dir>, b: Option<Dir>) -> char {
    let has = |d: Dir| a == Some(d) || b == Some(d);
    let (l, r, u, dn) = (has(Dir::L), has(Dir::R), has(Dir::U), has(Dir::D));
    if l && r {
        '─'
    } else if u && dn {
        '│'
    } else if r && u {
        '╰'
    } else if l && u {
        '╯'
    } else if r && dn {
        '╭'
    } else if l && dn {
        '╮'
    } else if l || r {
        '─' // a horizontal endpoint of the curve
    } else if u || dn {
        '│' // a vertical endpoint
    } else {
        '·' // a lone cell (a 1×1 grid)
    }
}

/// Whether cell `d`'s curve continues to the **right**, so its 2-wide cell's spacer column is
/// drawn as `─` and the line stays unbroken.
fn has_right(a: Option<Dir>, b: Option<Dir>) -> bool {
    a == Some(Dir::R) || b == Some(Dir::R)
}

/// The rounded box-drawing glyph for cell `d`'s segment of the curve, from the direction to
/// its previous and next cell: `─ │` straights, `╭ ╮ ╰ ╯` turns, a single stroke for a curve
/// endpoint, `·` for a lone cell.
///
/// ```
/// use mullion::curve_map::cell_glyph;
/// use mullion::spacefill::Gilbert;
///
/// let g = Gilbert::new(2, 2); // a 4-cell path: two endpoints, two turns
/// assert!(matches!(cell_glyph(&g, 0), '─' | '│')); // a curve endpoint: one stroke
/// assert!(matches!(cell_glyph(&g, 1), '╭' | '╮' | '╰' | '╯')); // an interior turn
/// ```
#[must_use]
pub fn cell_glyph(g: &Gilbert, d: usize) -> char {
    let (a, b) = ports(g, d);
    glyph_from_ports(a, b)
}

/// Choose the grid `(width, height)`, in **cells**, for laying `cells` items on a Gilbert
/// curve that fills `area`. Each cell is two columns wide and one row tall.
///
/// The cell count `width · height` is the largest **power of two that divides `cells`** and
/// fits the drawable rectangle, so every cell covers the same `cells / (width · height)`
/// items (an exact integer — and itself a power of two exactly when `cells` is). Both
/// dimensions are powers of two `≥ 2` — or a single `1×1` when nothing larger divides `cells`
/// or fits (an odd or `≤ 1` `cells`, or a one-column area) — so `width + height` is even and
/// the curve is [strictly continuous](crate::spacefill::strictly_continuous). Among grids
/// with the most cells, the squarest is chosen, then the wider.
///
/// ```
/// use mullion::curve_map::fit_dims;
/// use mullion::Rect;
///
/// // 16 items in a roomy area → 4×4 (16 cells, one item each), the squarest split.
/// assert_eq!(fit_dims(Rect::new(0, 0, 40, 20), 16), (4, 4));
/// // Only 6 items: the largest power of two dividing 6 is 2, so a 2×1 would be uneven —
/// // instead a single 1×1 cell covers all six.
/// assert_eq!(fit_dims(Rect::new(0, 0, 40, 20), 6), (1, 1));
/// ```
#[must_use]
pub fn fit_dims(area: Rect, cells: u128) -> (u32, u32) {
    let w_max = u32::from(area.width / 2).max(1);
    let h_max = u32::from(area.height).max(1);
    // Largest power-of-two extent that fits each axis, as an exponent (`2^aw ≤ w_max`).
    let aw = w_max.ilog2();
    let ah = h_max.ilog2();
    // The cell count must *divide* `cells`, so cap the total exponent at the largest
    // power-of-two divisor of `cells` (its trailing-zero count) — not `floor(log2 cells)`,
    // which would allow a non-dividing count when `cells` is not itself a power of two.
    let kb = if cells <= 1 { 0 } else { cells.trailing_zeros() };

    // Maximise the cell-count exponent `a + b` (finest resolution) with `a ∈ [1, aw]`,
    // `b ∈ [1, ah]`, `a + b ≤ kb`. Score higher-is-better as `(a+b, squareness, a)`: most
    // cells, then squarest (largest `MAX − |a−b|`), then wider. No valid split (a tiny
    // `cells` or a one-cell-wide area) falls back to a single 1×1 cell.
    let mut best: Option<(u32, u32, (u32, u32, u32))> = None;
    for a in 1..=aw {
        for b in 1..=ah {
            if a + b > kb {
                break; // b only grows; no larger b helps for this a
            }
            let score = (a + b, u32::MAX - a.abs_diff(b), a);
            if best.is_none_or(|(_, _, bs)| score > bs) {
                best = Some((a, b, score));
            }
        }
    }
    match best {
        Some((a, b, _)) => (1 << a, 1 << b),
        None => (1, 1),
    }
}

/// Paint the whole `g` grid into `buf` at `area`'s origin: for each cell `d`, draw
/// [`cell_glyph`] over the `(fg, bg)` colours from `paint(d)`, filling both of the cell's two
/// columns — the second continues the line with `─` when the curve proceeds right, else a
/// blank in `bg`.
///
/// `g` is normally sized by [`fit_dims`] so it fits `area`; any cell that would fall outside
/// `area` is skipped, so an oversized `g` clips rather than overflowing the buffer.
///
/// ```
/// use mullion::curve_map::{fit_dims, render};
/// use mullion::spacefill::Gilbert;
/// use mullion::style::Color;
/// use mullion::{Buffer, Rect};
///
/// let area = Rect::new(0, 0, 16, 8);
/// let (w, h) = fit_dims(area, 32);
/// let g = Gilbert::new(w, h);
/// let mut buf = Buffer::empty(area);
/// render(&mut buf, area, &g, |d| {
///     // e.g. brighten with position along the curve
///     let v = (d as u8).wrapping_mul(7);
///     (Color::Rgb(v, v, v), Color::Reset)
/// });
/// ```
pub fn render(buf: &mut Buffer, area: Rect, g: &Gilbert, paint: impl Fn(usize) -> (Color, Color)) {
    if area.width < 2 || area.height == 0 {
        return;
    }
    let (right, bottom) = (area.x.saturating_add(area.width), area.y.saturating_add(area.height));
    for d in 0..g.len() {
        let (gx, gy) = g.d_to_xy(d);
        let x = area.x.saturating_add((gx as u16).saturating_mul(2));
        let y = area.y.saturating_add(gy as u16);
        if x + 1 >= right || y >= bottom {
            continue; // this cell falls outside the drawable area — clip it
        }
        let (a, b) = ports(g, d);
        let (fg, bg) = paint(d);
        let style = Style::default().fg(fg).bg(bg);
        buf.set_char(x, y, glyph_from_ports(a, b), style);
        buf.set_char(x + 1, y, if has_right(a, b) { '─' } else { ' ' }, style);
    }
}

/// A per-cell **glow** for a highlighted run `seg` of a length-`len` curve, so a selected
/// segment brightens with no visible seam where it meets its neighbours on the curve.
///
/// The returned closure maps a cell index `d` to a glow in `[0, 1]`: exactly `0` outside
/// `seg` **and at `seg`'s two end cells**, ramping smoothly (a [`smoothstep`](crate::ease::smoothstep)
/// over `taper` cells at each end) up to a time-varying pulse in the middle. It is a boost,
/// not a replacement — apply it as `luma * (1.0 + glow)` (or add it), so cells outside `seg`
/// are untouched and the highlight fades into the curve rather than ending on a hard edge.
///
/// `t` advances the pulse (call once per frame for the current time); `taper` is clamped to
/// `≥ 1` so the ends always reach `0`. The result is finite and non-negative for every `t`.
///
/// ```
/// use mullion::curve_map::pulse_segment;
///
/// let glow = pulse_segment(100, 20..40, 1.3, 4);
/// assert_eq!(glow(10), 0.0); // before the segment: untouched
/// assert_eq!(glow(20), 0.0); // first cell of the segment: seam-free start
/// assert_eq!(glow(39), 0.0); // last cell: seam-free end
/// assert!(glow(30) >= 0.0);  // the middle glows (0 only at the pulse's trough)
/// ```
#[must_use]
pub fn pulse_segment(len: usize, seg: core::ops::Range<usize>, t: f32, taper: usize) -> impl Fn(usize) -> f32 {
    let lo = seg.start.min(len);
    let hi = seg.end.min(len);
    let taper = taper.max(1) as f32;
    // A smooth, always-non-negative breathing pulse in `[0, 1]` (0 at the trough).
    let pulse = 0.5 - 0.5 * t.cos();
    move |d: usize| -> f32 {
        if d < lo || d >= hi {
            return 0.0;
        }
        // Distance to the nearer end of the segment — symmetric by construction.
        let edge = (d - lo).min(hi - 1 - d);
        let ramp = crate::ease::smoothstep((edge as f32 / taper).clamp(0.0, 1.0));
        pulse * ramp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Whether `n` is a power of two (`> 0` with a single bit set).
    fn is_pow2(n: u128) -> bool {
        n > 0 && n & (n - 1) == 0
    }

    #[test]
    fn fit_dims_examples() {
        // A power-of-two count fills to one item per cell when the area allows.
        assert_eq!(fit_dims(Rect::new(0, 0, 40, 20), 16), (4, 4));
        // Wider-than-tall areas still pick the squarest among the finest grids.
        assert_eq!(fit_dims(Rect::new(0, 0, 64, 16), 256), (16, 16));
        // Tiny counts / degenerate areas collapse to a single cell.
        assert_eq!(fit_dims(Rect::new(0, 0, 40, 20), 1), (1, 1));
        assert_eq!(fit_dims(Rect::new(0, 0, 40, 20), 6), (1, 1)); // 6 = 2·3, only 2 divides — uneven
        assert_eq!(fit_dims(Rect::new(0, 0, 1, 20), 256), (1, 1)); // one column of cells
    }

    proptest! {
        /// `fit_dims` always yields a power-of-two cell count that divides `cells` into equal
        /// cells, fits the drawable area, and keeps the curve continuous (even `w + h`).
        #[test]
        fn fit_dims_gives_equal_power_of_two_cells(
            w in 0u16..=400,
            h in 0u16..=200,
            cells in 1u128..=(1u128 << 48),
        ) {
            let area = Rect::new(0, 0, w, h);
            let (gw, gh) = fit_dims(area, cells);
            let count = u128::from(gw) * u128::from(gh);

            prop_assert!(is_pow2(count), "cell count {} not a power of two ({}×{})", count, gw, gh);
            prop_assert_eq!(cells % count, 0, "count {} does not divide {}", count, cells);
            prop_assert!(count <= cells, "more cells ({}) than items ({})", count, cells);
            prop_assert_eq!((gw + gh) % 2, 0, "w+h must be even for a continuous curve");
            // Each dimension is 1, or a power of two ≥ 2 (never a lone odd > 1).
            for dim in [gw, gh] {
                prop_assert!(dim == 1 || (dim >= 2 && dim & (dim - 1) == 0), "bad dim {}", dim);
            }
            // Fits the drawable rectangle: width in 2-wide cells, height in rows.
            prop_assert!(u128::from(gw) <= u128::from(w / 2).max(1));
            prop_assert!(u128::from(gh) <= u128::from(h).max(1));
        }
    }

    #[test]
    fn render_paints_every_cell_and_the_glyphs_form_a_path() {
        let area = Rect::new(0, 0, 16, 8);
        let (w, h) = fit_dims(area, 64);
        let g = Gilbert::new(w, h);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &g, |_| (Color::Rgb(200, 200, 200), Color::Rgb(10, 10, 10)));

        // Every cell position carries a curve glyph (never blank), over the painted bg.
        for d in 0..g.len() {
            let (gx, gy) = g.d_to_xy(d);
            let (bx, by) = (area.x + (gx as u16) * 2, area.y + gy as u16);
            let cell = buf.get(bx, by);
            assert!(
                matches!(cell.symbol.as_str(), "─" | "│" | "╭" | "╮" | "╰" | "╯" | "·"),
                "cell glyph {:?} at {bx},{by} is not a curve stroke",
                cell.symbol
            );
            assert_eq!(cell.style.bg, Color::Rgb(10, 10, 10), "background not painted at {bx},{by}");
        }

        // The path is connected: consecutive cells are 4-adjacent (the curve never jumps),
        // so each cell's glyph genuinely joins its neighbours.
        for d in 0..g.len().saturating_sub(1) {
            let here = g.d_to_xy(d);
            let next = g.d_to_xy(d + 1);
            assert_eq!(here.0.abs_diff(next.0) + here.1.abs_diff(next.1), 1, "curve jumped at d={d}");
        }
        // The curve's two ends are single strokes, never the lone-cell dot.
        assert_ne!(cell_glyph(&g, 0), '·');
        assert_ne!(cell_glyph(&g, g.len() - 1), '·');
    }

    #[test]
    fn lone_cell_is_a_dot() {
        let g = Gilbert::new(1, 1);
        assert_eq!(cell_glyph(&g, 0), '·');
    }

    proptest! {
        /// `pulse_segment`'s glow is finite and non-negative everywhere, exactly `0` outside
        /// the segment and at its two end cells, and symmetric about the segment's middle.
        #[test]
        fn pulse_segment_is_seam_free_and_bounded(
            len in 1usize..300,
            start in 0usize..300,
            span in 0usize..80,
            taper in 0usize..30,
            t in -50.0f32..50.0,
        ) {
            let lo = start.min(len);
            let hi = (start + span).min(len);
            let glow = pulse_segment(len, lo..hi, t, taper);

            for d in 0..len {
                let g = glow(d);
                prop_assert!(g.is_finite() && g >= 0.0, "glow {} at d={} not finite/non-negative", g, d);
                if d < lo || d >= hi {
                    prop_assert_eq!(g, 0.0, "glow nonzero outside seg at d={}", d);
                }
            }
            if hi > lo {
                prop_assert_eq!(glow(lo), 0.0, "first cell of seg must be seam-free (0)");
                prop_assert_eq!(glow(hi - 1), 0.0, "last cell of seg must be seam-free (0)");
                // Symmetric: the k-th cell from each end glows identically.
                let last = hi - 1 - lo;
                for k in 0..=last {
                    prop_assert!((glow(lo + k) - glow(hi - 1 - k)).abs() < 1e-6, "asymmetric at k={}", k);
                }
            }
        }
    }
}
