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

use crate::border::{BorderStyle, CornerStyle, LineWeight};
use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::junction::{resolve, EdgeGrid};
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

/// Draw a **rounded outline around an arbitrary region** of cells — not just a rectangle —
/// tracing the boundary between `inside` and outside cells with light box-drawing glyphs.
///
/// `inside(x, y)` reports whether the buffer cell `(x, y)` belongs to the region. The outline
/// is laid on the ring of cells **just outside** the region (so it frames without overwriting
/// the region's own content): a straight run is `─`/`│`, a corner is `╭ ╮ ╰ ╯` (for a
/// [`Light`](LineWeight::Light) + [`Rounded`](CornerStyle::Rounded) `style`; other weights use
/// their square/heavy/double corners, per `border`'s rounded-is-Light-only rule), and where
/// the boundary branches it resolves to `├ ┤ ┬ ┴ ┼` via [`junction::resolve`](crate::junction::resolve).
/// A compact region gets a single closed loop; a lone cell gets a tiny 3×3 box.
///
/// The outline is clipped to `area`; a region flush against `area`'s edge loses the outline
/// on that side (leave a one-cell margin). `style.style` gives the glyph colour.
///
/// ```
/// use mullion::curve_map::draw_region_outline;
/// use mullion::border::{BorderStyle, CornerStyle, LineWeight};
/// use mullion::style::Style;
/// use mullion::{Buffer, Rect};
///
/// let area = Rect::new(0, 0, 8, 8);
/// let mut buf = Buffer::empty(area);
/// // Outline a single cell → a tiny rounded box around it.
/// let style = BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Rounded, style: Style::default() };
/// draw_region_outline(&mut buf, area, |x, y| (x, y) == (3, 3), &style);
/// assert_eq!(buf.get(2, 2).symbol, "╭"); // top-left of the ring
/// assert_eq!(buf.get(4, 4).symbol, "╯"); // bottom-right
/// assert_eq!(buf.get(3, 3).symbol, " "); // the region's own cell is untouched (blank)
/// ```
pub fn draw_region_outline(buf: &mut Buffer, area: Rect, inside: impl Fn(u16, u16) -> bool, style: &BorderStyle) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Signed, bounds-safe membership test — neighbours may step off the plane.
    let ins = |x: i64, y: i64| -> bool {
        (0..=i64::from(u16::MAX)).contains(&x)
            && (0..=i64::from(u16::MAX)).contains(&y)
            && inside(x as u16, y as u16)
    };

    // Accumulate the outline as unit arm-segments on the outside ring: an east arm on the
    // edge (x,y)–(x+1,y) when both cells are outside and the region lies on exactly one side
    // of that horizontal segment; symmetrically a south arm. `EdgeGrid` merges the arms and
    // forms the junctions; the arithmetic runs in i64 so `x-1`/`y-1` never underflow.
    let mut grid = EdgeGrid::new(area);
    let (x0, y0) = (i64::from(area.x), i64::from(area.y));
    let (x1, y1) = (x0 + i64::from(area.width), y0 + i64::from(area.height));
    for y in y0..y1 {
        for x in x0..x1 {
            if !ins(x, y) && !ins(x + 1, y) {
                let above = ins(x, y - 1) || ins(x + 1, y - 1);
                let below = ins(x, y + 1) || ins(x + 1, y + 1);
                if above != below {
                    grid.add_h_line(x as u16, x as u16 + 1, y as u16, style.weight);
                }
            }
            if !ins(x, y) && !ins(x, y + 1) {
                let left = ins(x - 1, y) || ins(x - 1, y + 1);
                let right = ins(x + 1, y) || ins(x + 1, y + 1);
                if left != right {
                    grid.add_v_line(y as u16, y as u16 + 1, x as u16, style.weight);
                }
            }
        }
    }

    // Resolve each cell's arms to a glyph; round the light corners when asked.
    let rounded = matches!((style.weight, style.corners), (LineWeight::Light, CornerStyle::Rounded));
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let Some(cell) = grid.get(x, y) else { continue };
            let Some(ch) = resolve(cell) else { continue };
            let ch = if rounded {
                match ch {
                    '┌' => '╭',
                    '┐' => '╮',
                    '└' => '╰',
                    '┘' => '╯',
                    other => other,
                }
            } else {
                ch
            };
            buf.set_char(x, y, ch, style.style);
        }
    }
}

/// The 4×4 ordered (Bayer) dither matrix — 16 thresholds spread so neighbouring cells sit on
/// different sub-phases. The same matrix the video dither uses; here it staggers the temporal
/// overlay *in space* so the lit fraction crawls rather than blinking globally.
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// One cell of a temporal overlay: a glyph + style to show at `(x, y)`, with a `duty` in
/// `[0, 1]` giving the fraction of the phase cycle it is *visible* — `1.0` opaque (always
/// drawn, e.g. a label glyph that must never flicker), `0.5` half see-through (the layer
/// beneath breathes through, e.g. a leader wire over a busy map), `0.0` never drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayCell {
    /// Column.
    pub x: u16,
    /// Row.
    pub y: u16,
    /// The glyph to show when this cell is on-phase.
    pub glyph: char,
    /// The style to paint it in.
    pub style: Style,
    /// Visible fraction of the phase cycle, `[0, 1]` (see [`OverlayCell`]).
    pub duty: f32,
}

/// Composite a temporal overlay onto an **already-painted** `buf`: for each [`OverlayCell`],
/// draw its glyph **only on its on-phase**, and otherwise leave the buffer cell untouched —
/// so whatever was painted underneath (a space-filling curve, a heatmap) *shows through* on
/// the off-phase. Never clears first, so the see-through is intrinsic.
///
/// The on/off decision is a **spatiotemporal ordered dither**: each cell has a threshold from
/// the [`BAYER4`] matrix keyed on `(x, y)` (so neighbours are staggered *in space*), crawled by
/// the caller's `phase` (so the lit set advances *in time*). The mean lit fraction is `duty` at
/// **every** phase — no global luminance swing — so a partly-transparent overlay reads as
/// smoothly crawling "marching ants", legible even at a slow (≈ 20 Hz) frame clock, rather than
/// a flicker. Per-cell `duty` lets one call carry an opaque label over a see-through wire.
///
/// `phase` is an external `f32` you advance each frame (fractional part is what matters); drive
/// it from the same animation clock as the rest of the view so everything breathes as one.
///
/// ```
/// use mullion::curve_map::{temporal_overlay, OverlayCell};
/// use mullion::style::{Color, Style};
/// use mullion::{Buffer, Rect};
///
/// let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
/// let st = Style::default().fg(Color::Rgb(255, 255, 255));
/// // An opaque label cell (duty 1.0) is drawn at any phase; a half-duty wire cell isn't always.
/// let cells = [OverlayCell { x: 0, y: 0, glyph: 'A', style: st, duty: 1.0 }];
/// temporal_overlay(&mut buf, &cells, 0.37);
/// assert_eq!(buf.get(0, 0).symbol, "A"); // opaque: always shown
/// ```
pub fn temporal_overlay(buf: &mut Buffer, cells: &[OverlayCell], phase: f32) {
    for c in cells {
        // Per-cell threshold in [0,1): the spatial Bayer offset, crawled by the phase.
        let base = f32::from(BAYER4[(c.y % 4) as usize][(c.x % 4) as usize]) / 16.0;
        let threshold = (base + phase).rem_euclid(1.0);
        if c.duty > threshold {
            buf.set_char(c.x, c.y, c.glyph, c.style);
        }
        // else: off-phase — leave the underlying cell so it shows through.
    }
}

/// Per-layer visibility for [`callout`], each a `duty` in `[0, 1]` (see [`OverlayCell`]). The
/// defaults keep **label glyphs fully opaque** (they never dither — always readable) while the
/// leader wire and the box fill go half see-through so the map breathes through them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalloutDuty {
    /// The lasso ring around the region (a near-opaque one-cell band).
    pub ring: f32,
    /// The leader wire from the region to the box (see-through, so it doesn't mask the map).
    pub leader: f32,
    /// The box's border.
    pub box_border: f32,
    /// The box's interior fill behind the text (see-through). Text glyphs are always opaque.
    pub box_fill: f32,
}

impl Default for CalloutDuty {
    fn default() -> Self {
        CalloutDuty { ring: 0.85, leader: 0.5, box_border: 0.85, box_fill: 0.5 }
    }
}

/// Round a resolved square corner glyph to its light-rounded form (`┌ ┐ └ ┘ → ╭ ╮ ╰ ╯`) when
/// `rounded`; other glyphs pass through.
fn round_corner(ch: char, rounded: bool) -> char {
    if rounded {
        match ch {
            '┌' => '╭',
            '┐' => '╮',
            '└' => '╰',
            '┘' => '╯',
            other => other,
        }
    } else {
        ch
    }
}

/// Draw a **callout**: a rounded outline around the `inside` region, a leader wire from
/// `anchor` to a floating text `box_rect` (with a `●` bookend where it leaves the region), and
/// the box itself carrying up to `box_rect.height − 2` `lines` — all composited over the
/// already-painted `buf` via [`temporal_overlay`], so the map shows *through* the chrome. The
/// per-layer [`CalloutDuty`] keeps the label glyphs opaque while the wire and box-fill dither.
///
/// `area` is the region the `inside` predicate is scanned over (the map's rect); `style` styles
/// the outline/leader/box; `phase` is the shared animation clock. The caller places `box_rect`
/// (it knows its own free space) and mullion routes the leader to it.
///
/// This is a composition of [`draw_region_outline`], [`junction`](crate::junction) leader
/// routing, [`Panel`](crate::panel::Panel), and [`temporal_overlay`]; reach for those atoms
/// directly if you need finer control.
#[allow(clippy::too_many_arguments)]
pub fn callout(
    buf: &mut Buffer,
    area: Rect,
    inside: impl Fn(u16, u16) -> bool,
    anchor: (u16, u16),
    box_rect: Rect,
    lines: &[&str],
    style: &BorderStyle,
    phase: f32,
    duty: CalloutDuty,
) {
    use crate::junction::{resolve, EdgeGrid};
    use crate::panel::{draw_panel, Panel};

    let rounded = matches!((style.weight, style.corners), (LineWeight::Light, CornerStyle::Rounded));
    let mut cells: Vec<OverlayCell> = Vec::new();

    // ── the lasso ring — render the outline to a scratch buffer, lift its glyphs ──
    if area.width > 0 && area.height > 0 {
        let mut ring = Buffer::empty(area);
        draw_region_outline(&mut ring, area, &inside, style);
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let c = ring.get(x, y);
                if let Some(ch) = c.symbol.chars().next().filter(|&ch| ch != ' ') {
                    cells.push(OverlayCell { x, y, glyph: ch, style: c.style, duty: duty.ring });
                }
            }
        }
    }

    // ── the leader — an L from the anchor to the nearest box edge, via an EdgeGrid ──
    let (ax, ay) = anchor;
    let (br, bb) = (box_rect.x + box_rect.width.saturating_sub(1), box_rect.y + box_rect.height.saturating_sub(1));
    let clamp_x = ax.clamp(box_rect.x, br);
    let clamp_y = ay.clamp(box_rect.y, bb);
    let attach = if ax < box_rect.x {
        (box_rect.x, clamp_y)
    } else if ax > br {
        (br, clamp_y)
    } else if ay < box_rect.y {
        (clamp_x, box_rect.y)
    } else {
        (clamp_x, bb)
    };
    let (lx0, lx1) = (ax.min(attach.0), ax.max(attach.0));
    let (ly0, ly1) = (ay.min(attach.1), ay.max(attach.1));
    let mut lg = EdgeGrid::new(Rect::new(lx0, ly0, lx1 - lx0 + 1, ly1 - ly0 + 1));
    lg.add_h_line(lx0, lx1, ay, style.weight); // horizontal at the anchor's row
    lg.add_v_line(ly0, ly1, attach.0, style.weight); // vertical up/down the attach column
    for y in ly0..=ly1 {
        for x in lx0..=lx1 {
            if let Some(ch) = lg.get(x, y).and_then(resolve) {
                cells.push(OverlayCell { x, y, glyph: round_corner(ch, rounded), style: style.style, duty: duty.leader });
            }
        }
    }
    // The bookend where the leader leaves the region (drawn last so it wins its cell).
    cells.push(OverlayCell { x: ax, y: ay, glyph: '●', style: style.style, duty: duty.leader });

    // ── the box — draw a filled panel + the lines to a scratch, classify each cell ──
    if box_rect.width >= 2 && box_rect.height >= 2 {
        let fill = Style::default().bg(Color::Rgb(20, 22, 28));
        let mut bx = Buffer::empty(box_rect);
        let panel = Panel::new(*style).fill(fill);
        let interior = draw_panel(&mut bx, box_rect, &panel);
        let text = Style::default().fg(style.style.fg).bg(Color::Rgb(20, 22, 28));
        for (i, line) in lines.iter().take(interior.height as usize).enumerate() {
            let clipped: String = line.chars().take(interior.width as usize).collect();
            bx.set_string(interior.x, interior.y + i as u16, &clipped, text);
        }
        for y in box_rect.y..box_rect.y + box_rect.height {
            for x in box_rect.x..box_rect.x + box_rect.width {
                let c = bx.get(x, y);
                let Some(ch) = c.symbol.chars().next() else { continue };
                let d = if ch == ' ' {
                    duty.box_fill // interior fill — the map breathes through
                } else if is_box_glyph(ch) {
                    duty.box_border
                } else {
                    1.0 // a label glyph — always opaque, never flickers
                };
                cells.push(OverlayCell { x, y, glyph: ch, style: c.style, duty: d });
            }
        }
    }

    temporal_overlay(buf, &cells, phase);
}

/// Whether `ch` is a box-drawing glyph (so a callout classifies it as border, not a label).
fn is_box_glyph(ch: char) -> bool {
    matches!(
        ch,
        '─' | '│' | '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼'
            | '━' | '┃' | '═' | '║' | '╔' | '╗' | '╚' | '╝'
    )
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

    // ── draw_region_outline ──

    fn light_rounded() -> BorderStyle {
        BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Rounded, style: Style::default() }
    }

    fn sym(buf: &Buffer, x: u16, y: u16) -> String {
        buf.get(x, y).symbol.clone()
    }

    #[test]
    fn one_cell_region_is_a_tiny_rounded_box() {
        let area = Rect::new(0, 0, 8, 8);
        let mut buf = Buffer::empty(area);
        draw_region_outline(&mut buf, area, |x, y| (x, y) == (3, 3), &light_rounded());
        // A clean 3×3 box on the ring around (3,3); the region cell itself untouched.
        assert_eq!(sym(&buf, 2, 2), "╭");
        assert_eq!(sym(&buf, 3, 2), "─");
        assert_eq!(sym(&buf, 4, 2), "╮");
        assert_eq!(sym(&buf, 2, 3), "│");
        assert_eq!(sym(&buf, 3, 3), " "); // region cell blank (untouched)
        assert_eq!(sym(&buf, 4, 3), "│");
        assert_eq!(sym(&buf, 2, 4), "╰");
        assert_eq!(sym(&buf, 3, 4), "─");
        assert_eq!(sym(&buf, 4, 4), "╯");
    }

    #[test]
    fn rectangle_region_is_a_rounded_box_just_outside() {
        let area = Rect::new(0, 0, 12, 10);
        let mut buf = Buffer::empty(area);
        // Region cells x∈4..=6, y∈4..=5 → outline ring at x∈3..=7, y∈3..=6.
        let inside = |x: u16, y: u16| (4..=6).contains(&x) && (4..=5).contains(&y);
        draw_region_outline(&mut buf, area, inside, &light_rounded());
        assert_eq!(sym(&buf, 3, 3), "╭");
        assert_eq!(sym(&buf, 7, 3), "╮");
        assert_eq!(sym(&buf, 3, 6), "╰");
        assert_eq!(sym(&buf, 7, 6), "╯");
        assert_eq!(sym(&buf, 4, 3), "─"); // a top-edge run
        assert_eq!(sym(&buf, 3, 4), "│"); // a left-edge run
        assert_eq!(sym(&buf, 5, 5), " "); // region interior untouched (blank)
    }

    #[test]
    fn l_shaped_region_is_a_single_closed_loop() {
        let area = Rect::new(0, 0, 18, 18);
        let mut buf = Buffer::empty(area);
        // A fat L: a 6×6 block (x,y ∈ 5..=10) minus its top-right 3×3 (x ∈ 8..=10, y ∈ 5..=7).
        let inside = |x: u16, y: u16| {
            let block = (5..=10).contains(&x) && (5..=10).contains(&y);
            let notch = (8..=10).contains(&x) && (5..=7).contains(&y);
            block && !notch
        };
        draw_region_outline(&mut buf, area, inside, &light_rounded());
        // A single closed loop: every drawn glyph is a 2-arm stroke (straight or rounded
        // corner) — no ├┤┬┴┼ junction and no dangling stub anywhere on the outline.
        let mut drawn = 0;
        for y in 0..18 {
            for x in 0..18 {
                let s = sym(&buf, x, y);
                if s.is_empty() || s == " " {
                    continue; // a blank (untouched) cell
                }
                drawn += 1;
                assert!(matches!(s.as_str(), "─" | "│" | "╭" | "╮" | "╰" | "╯"), "junction/stub {s:?} at {x},{y}");
            }
        }
        assert!(drawn > 12, "expected a sizeable single loop, got {drawn} cells");
    }

    // ── temporal_overlay ──

    fn overlay_block(w: u16, h: u16, glyph: char, duty: f32) -> Vec<OverlayCell> {
        let st = Style::default().fg(Color::Rgb(220, 220, 220));
        (0..w)
            .flat_map(move |x| (0..h).map(move |y| OverlayCell { x, y, glyph, style: st, duty }))
            .collect()
    }

    #[test]
    fn temporal_overlay_opaque_always_transparent_never() {
        let area = Rect::new(0, 0, 8, 4);
        // duty 1.0 → drawn at every phase; duty 0.0 → never drawn.
        for &ph in &[0.0f32, 0.3, 0.7, 0.99, 3.4, -1.2] {
            let mut opaque = Buffer::empty(area);
            temporal_overlay(&mut opaque, &overlay_block(8, 4, '#', 1.0), ph);
            let mut clear = Buffer::empty(area);
            temporal_overlay(&mut clear, &overlay_block(8, 4, '#', 0.0), ph);
            for x in 0..8 {
                for y in 0..4 {
                    assert_eq!(opaque.get(x, y).symbol, "#", "opaque not drawn at phase {ph}");
                    assert_eq!(clear.get(x, y).symbol, " ", "transparent drawn at phase {ph}");
                }
            }
        }
    }

    #[test]
    fn temporal_overlay_lit_fraction_tracks_duty_at_every_phase() {
        // Over a 16×16 block (whole Bayer tiles), the lit fraction is exactly `duty` at phase 0
        // for a duty that is a multiple of 1/16, and within one Bayer step of `duty·N` at any
        // phase — i.e. the lit fraction never swings globally (marching ants, not a blink).
        let area = Rect::new(0, 0, 16, 16);
        let lit = |buf: &Buffer| (0..16).flat_map(|x| (0..16).map(move |y| (x, y))).filter(|&(x, y)| buf.get(x, y).symbol == "*").count();

        for (duty, exact) in [(0.25f32, 64usize), (0.5, 128), (0.75, 192)] {
            let mut buf = Buffer::empty(area);
            temporal_overlay(&mut buf, &overlay_block(16, 16, '*', duty), 0.0);
            assert_eq!(lit(&buf), exact, "phase-0 lit count for duty {duty}");
        }
        for &ph in &[0.13f32, 0.42, 0.88, 5.6, -2.7] {
            let mut buf = Buffer::empty(area);
            temporal_overlay(&mut buf, &overlay_block(16, 16, '*', 0.5), ph);
            assert!((lit(&buf) as i32 - 128).abs() <= 16, "duty 0.5 lit {} swings too far at phase {ph}", lit(&buf));
        }
    }

    #[test]
    fn temporal_overlay_composites_never_clears() {
        // Paint a base "curve", overlay the top row at half duty: on-phase cells become the
        // overlay, off-phase cells KEEP the base (never cleared); other cells are untouched.
        let area = Rect::new(0, 0, 4, 4);
        let base = Style::default().fg(Color::Rgb(80, 80, 80));
        let over = Style::default().fg(Color::Rgb(255, 0, 0));
        let mut buf = Buffer::empty(area);
        for x in 0..4 {
            for y in 0..4 {
                buf.set_char(x, y, '.', base);
            }
        }
        let cells: Vec<_> = (0..4u16).map(|x| OverlayCell { x, y: 0, glyph: '#', style: over, duty: 0.5 }).collect();
        temporal_overlay(&mut buf, &cells, 0.0);
        for x in 0..4 {
            let s = buf.get(x, 0).symbol.clone();
            assert!(s == "#" || s == ".", "top cell {x} was cleared: {s:?}"); // overlay or base, never blank
        }
        for x in 0..4 {
            for y in 1..4 {
                assert_eq!(buf.get(x, y).symbol, ".", "a non-overlay cell was touched");
            }
        }
    }

    #[test]
    fn callout_composites_ring_leader_and_box_with_opaque_labels() {
        let area = Rect::new(0, 0, 40, 16);
        let mut buf = Buffer::empty(area);
        // A base "map" so we can see the callout composite over it.
        let base = Style::default().fg(Color::Rgb(60, 60, 60));
        for y in 0..16 {
            for x in 0..40 {
                buf.set_char(x, y, '·', base);
            }
        }
        let style = BorderStyle {
            weight: LineWeight::Light,
            corners: CornerStyle::Rounded,
            style: Style::default().fg(Color::Rgb(255, 255, 255)),
        };
        let inside = |x: u16, y: u16| (4..=7).contains(&x) && (4..=6).contains(&y);
        let box_rect = Rect::new(20, 2, 16, 5);
        let lines = ["host-alpha", "10.0.0.0/24", "vlan 30"];
        callout(&mut buf, area, inside, (8, 5), box_rect, &lines, &style, 0.0, CalloutDuty::default());

        // Label glyphs are opaque (duty 1.0) → the box text is present at any phase.
        let row: String = (21..35).map(|x| buf.get(x, 3).symbol.clone()).collect();
        assert!(row.contains("host-alpha"), "label text not rendered opaquely: {row:?}");
        // The callout composited a meaningful amount over the base map (ring + leader + box).
        let changed = (0..16).flat_map(|y| (0..40).map(move |x| (x, y))).filter(|&(x, y)| buf.get(x, y).symbol != "·").count();
        assert!(changed > 20, "callout drew too little: {changed}");
        // Cells far from the callout are untouched base.
        assert_eq!(buf.get(0, 15).symbol, "·");
    }
}
