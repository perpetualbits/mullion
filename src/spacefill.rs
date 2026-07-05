// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! **Space-filling curves** — push a long 1-D line onto a 2-D grid while keeping
//! *locality*: points close on the line stay close on the grid, and (the property that
//! matters) a contiguous *run* of the line lands as a **compact blob**, not scattered
//! confetti. That is what makes a Hilbert-style layout the right map for an address
//! space — a subnet reads as a square patch you can point at.
//!
//! A plain [Hilbert curve] only fills a `2^k × 2^k` square. This module implements the
//! **generalized Hilbert ("Gilbert") curve** (after Jakub Červený), which fills *any*
//! `width × height` rectangle by recursively splitting it, keeping Hilbert-grade
//! locality without the power-of-two straitjacket. So an arbitrary-length line — 733
//! addresses, a `/23`, a terminal panel of whatever size — folds into a square-ish
//! region with every step a unit move (the curve never jumps).
//!
//! Two ways in:
//! - [`gilbert_cells`] returns the whole visiting order as a `Vec<(x, y)>` — index `d`
//!   of the line sits at `cells[d]`. `O(width·height)`, the workhorse for rendering a
//!   landscape (colour cell `cells[d]` by `data[d]`).
//! - [`Gilbert`] materialises both directions (`d → (x, y)` and `(x, y) → d`) for O(1)
//!   lookup — turn a cursor position back into the line index under it.
//! - [`gilbert_d2xy`] resolves a single index without materialising the whole curve.
//!
//! ## Continuity and parity
//!
//! The curve is **always 8-connected** — no step ever exceeds a Chebyshev distance of 1,
//! so there is never a real gap. It is **strictly 4-connected** (every step an edge move,
//! never a diagonal) **iff `width` and `height` have the same parity** — both even or both
//! odd — see [`strictly_continuous`]. Mixed parity (one even, one odd) forces at least one
//! diagonal seam: the checkerboard argument (a 4-connected path alternates black/white)
//! only closes corner-to-corner when the parities agree. For a *filled* landscape this is
//! invisible — every cell is coloured, the connecting segment is never drawn — but if you
//! need a strictly continuous line, pad to same-parity dimensions.
//!
//! ```
//! # use mullion::spacefill::Gilbert;
//! let g = Gilbert::new(13, 7);            // 13×7 = 91 cells, not a power of two
//! assert_eq!(g.len(), 91);
//! let (x, y) = g.d_to_xy(0);              // where the line starts
//! assert_eq!(g.xy_to_d(x, y), Some(0));   // and back again
//! // Consecutive line indices are always unit-adjacent on the grid:
//! let (x0, y0) = g.d_to_xy(40);
//! let (x1, y1) = g.d_to_xy(41);
//! assert_eq!(x0.abs_diff(x1) + y0.abs_diff(y1), 1);
//! ```
//!
//! [Hilbert curve]: https://en.wikipedia.org/wiki/Hilbert_curve

/// `-1`, `0`, or `+1` — the sign of `x`, as the unit step along an axis.
fn sgn(x: i32) -> i32 {
    (x > 0) as i32 - (x < 0) as i32
}

/// Emit the Gilbert order for the rectangle rooted at `(x, y)` and spanned by the
/// **major** axis vector `(ax, ay)` and **minor** axis vector `(bx, by)` (exactly one
/// of each pair's components is non-zero — the axes are grid-aligned). Appends the
/// cells, in curve order, to `out`. This is the recursion from Červený's `gilbert2d`.
fn gen2d(x: i32, y: i32, ax: i32, ay: i32, bx: i32, by: i32, out: &mut Vec<(u32, u32)>) {
    let w = (ax + ay).abs(); // cells along the major axis
    let h = (bx + by).abs(); // cells along the minor axis
    let (dax, day) = (sgn(ax), sgn(ay)); // major unit step
    let (dbx, dby) = (sgn(bx), sgn(by)); // minor unit step

    if h == 1 {
        // One row: walk the major axis.
        let (mut px, mut py) = (x, y);
        for _ in 0..w {
            out.push((px as u32, py as u32));
            px += dax;
            py += day;
        }
        return;
    }
    if w == 1 {
        // One column: walk the minor axis.
        let (mut px, mut py) = (x, y);
        for _ in 0..h {
            out.push((px as u32, py as u32));
            px += dbx;
            py += dby;
        }
        return;
    }

    // Halve toward negative infinity (`>> 1`, not `/ 2`): the axis vectors go negative in
    // the third sub-piece, and the split point must floor-divide like the reference.
    let (mut ax2, mut ay2) = (ax >> 1, ay >> 1);
    let (mut bx2, mut by2) = (bx >> 1, by >> 1);
    let w2 = (ax2 + ay2).abs();
    let h2 = (bx2 + by2).abs();

    if 2 * w > 3 * h {
        // Long rectangle: split into two pieces along the major axis. Prefer an even
        // split so the two halves join cleanly.
        if w2 % 2 != 0 && w > 2 {
            ax2 += dax;
            ay2 += day;
        }
        gen2d(x, y, ax2, ay2, bx, by, out);
        gen2d(x + ax2, y + ay2, ax - ax2, ay - ay2, bx, by, out);
    } else {
        // Standard case: split into three pieces (down, across, back up).
        if h2 % 2 != 0 && h > 2 {
            bx2 += dbx;
            by2 += dby;
        }
        gen2d(x, y, bx2, by2, ax2, ay2, out);
        gen2d(x + bx2, y + by2, ax, ay, bx - bx2, by - by2, out);
        gen2d(
            x + (ax - dax) + (bx2 - dbx),
            y + (ay - day) + (by2 - dby),
            -bx2,
            -by2,
            -(ax - ax2),
            -(ay - ay2),
            out,
        );
    }
}

/// The full Gilbert visiting order for a `width × height` grid: `cells[d]` is the grid
/// position of line index `d`, for `d ∈ 0..width·height`. Consecutive indices are always
/// unit-adjacent. Empty if either dimension is `0`. `O(width·height)`.
#[must_use]
pub fn gilbert_cells(width: u32, height: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::with_capacity((width as usize) * (height as usize));
    if width == 0 || height == 0 {
        return out;
    }
    // Orient so the major axis is the longer side (matches Červený's entry point).
    if width >= height {
        gen2d(0, 0, width as i32, 0, 0, height as i32, &mut out);
    } else {
        gen2d(0, 0, 0, height as i32, width as i32, 0, &mut out);
    }
    out
}

/// Resolve a *single* line index `d` to its grid cell on the `width × height` Gilbert
/// curve, without materialising the whole order — `O(log(width·height))` via
/// area-accounting descent. Returns `(0, 0)` for an empty grid or out-of-range `d`
/// (callers pass `d < width·height`).
#[must_use]
pub fn gilbert_d2xy(width: u32, height: u32, d: u64) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let (x, y) = if width >= height {
        d2xy(0, 0, width as i32, 0, 0, height as i32, d as i64)
    } else {
        d2xy(0, 0, 0, height as i32, width as i32, 0, d as i64)
    };
    (x as u32, y as u32)
}

/// Area-accounting mirror of [`gen2d`]: descend into whichever sub-rectangle holds the
/// `idx`-th cell, subtracting the areas skipped, until a single row/column pins it down.
fn d2xy(x: i32, y: i32, ax: i32, ay: i32, bx: i32, by: i32, idx: i64) -> (i32, i32) {
    let w = (ax + ay).abs() as i64;
    let h = (bx + by).abs() as i64;
    let (dax, day) = (sgn(ax), sgn(ay));
    let (dbx, dby) = (sgn(bx), sgn(by));

    if h == 1 {
        return (x + dax * idx as i32, y + day * idx as i32);
    }
    if w == 1 {
        return (x + dbx * idx as i32, y + dby * idx as i32);
    }

    // Floor-halve (see [`gen2d`]) so the area split matches the enumerated order.
    let (mut ax2, mut ay2) = (ax >> 1, ay >> 1);
    let (mut bx2, mut by2) = (bx >> 1, by >> 1);
    let w2 = (ax2 + ay2).abs() as i64;
    let h2 = (bx2 + by2).abs() as i64;

    if 2 * w > 3 * h {
        if w2 % 2 != 0 && w > 2 {
            ax2 += dax;
            ay2 += day;
        }
        let a1 = (ax2 + ay2).abs() as i64 * h; // first half's area
        if idx < a1 {
            d2xy(x, y, ax2, ay2, bx, by, idx)
        } else {
            d2xy(x + ax2, y + ay2, ax - ax2, ay - ay2, bx, by, idx - a1)
        }
    } else {
        if h2 % 2 != 0 && h > 2 {
            bx2 += dbx;
            by2 += dby;
        }
        let h2v = (bx2 + by2).abs() as i64;
        let w2v = (ax2 + ay2).abs() as i64;
        let a1 = h2v * w2v; // down piece
        let a2 = w * (h - h2v); // across piece
        if idx < a1 {
            d2xy(x, y, bx2, by2, ax2, ay2, idx)
        } else if idx < a1 + a2 {
            d2xy(x + bx2, y + by2, ax, ay, bx - bx2, by - by2, idx - a1)
        } else {
            d2xy(
                x + (ax - dax) + (bx2 - dbx),
                y + (ay - day) + (by2 - dby),
                -bx2,
                -by2,
                -(ax - ax2),
                -(ay - ay2),
                idx - a1 - a2,
            )
        }
    }
}

/// Whether a `width × height` Gilbert curve is **strictly 4-continuous** (every step an
/// edge move, no diagonal seams) — true exactly when the two dimensions share parity.
/// A filled landscape does not care; a drawn line does. To make an arbitrary target
/// strictly continuous, grow one side by 1 so the parities match.
#[must_use]
pub fn strictly_continuous(width: u32, height: u32) -> bool {
    width % 2 == height % 2
}

/// A materialised Gilbert mapping for a fixed `width × height` grid: both `d → (x, y)`
/// and `(x, y) → d` in O(1). Build once, query per cell — the natural backing for a
/// landscape where you colour each grid cell by the line value at its index.
#[derive(Debug, Clone)]
pub struct Gilbert {
    width: u32,
    height: u32,
    /// `d → (x, y)`; `forward[d]` is the cell of line index `d`.
    forward: Vec<(u32, u32)>,
    /// `y·width + x → d`; the inverse of `forward`.
    inverse: Vec<u32>,
}

impl Gilbert {
    /// Build the mapping for a `width × height` grid. `O(width·height)` time and space.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let forward = gilbert_cells(width, height);
        let mut inverse = vec![0u32; (width as usize) * (height as usize)];
        for (d, &(x, y)) in forward.iter().enumerate() {
            inverse[(y * width + x) as usize] = d as u32;
        }
        Gilbert {
            width,
            height,
            forward,
            inverse,
        }
    }

    /// The grid width.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The grid height.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The number of cells on the curve (`width · height`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.forward.len()
    }

    /// Whether the grid is empty (either dimension `0`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    /// The grid cell holding line index `d`. Panics if `d >= len()`.
    #[must_use]
    pub fn d_to_xy(&self, d: usize) -> (u32, u32) {
        self.forward[d]
    }

    /// The line index at grid cell `(x, y)`, or `None` if `(x, y)` is off the grid.
    #[must_use]
    pub fn xy_to_d(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(self.inverse[(y * self.width + x) as usize])
    }

    /// The whole visiting order, `cells[d] == d_to_xy(d)`.
    #[must_use]
    pub fn cells(&self) -> &[(u32, u32)] {
        &self.forward
    }

    /// The **allowed** cells only, in curve order — the full Gilbert order with the
    /// *forbidden* cells filtered out. `allowed(x, y)` decides membership.
    ///
    /// This is how holes of *any* shape (scattered exclusions, mostly-forbidden with a
    /// few inclusions, regular windows) map an address line onto a punched grid: line
    /// index `i` is `masked_order(allowed)[i]`. Locality carries straight over — a
    /// contiguous slice of the returned line still occupies a compact region, so a
    /// subnet stays a blob even with holes — because dropping cells from a
    /// locality-preserving order can only shrink each run's footprint. The returned
    /// length is exactly the allowed-cell count, so an area-preserving change to the
    /// mask keeps the line length fixed. `O(width·height)`.
    ///
    /// Note this preserves *locality*, not *4-continuity*: consecutive allowed cells
    /// that straddle a hole need not be grid-adjacent. For a filled landscape that is
    /// invisible (nothing draws the joining segment); a genuine continuous strand
    /// through the allowed set is a stricter problem bounded by the parity rule above.
    #[must_use]
    pub fn masked_order(&self, allowed: impl Fn(u32, u32) -> bool) -> Vec<(u32, u32)> {
        self.forward.iter().copied().filter(|&(x, y)| allowed(x, y)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every cell of a `w×h` grid is visited exactly once; every step is at most a
    /// diagonal (8-connected, never a real gap); and steps are strictly unit moves
    /// (4-connected) exactly when the dimensions share parity — the parity theorem.
    fn assert_valid(w: u32, h: u32) {
        let cells = gilbert_cells(w, h);
        assert_eq!(cells.len(), (w * h) as usize, "coverage count {w}×{h}");
        let set: HashSet<_> = cells.iter().copied().collect();
        assert_eq!(set.len(), cells.len(), "no repeats {w}×{h}");
        for &(x, y) in &cells {
            assert!(x < w && y < h, "in bounds {w}×{h}");
        }
        let mut diagonals = 0;
        for pair in cells.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy) = (a.0.abs_diff(b.0), a.1.abs_diff(b.1));
            assert!(
                dx.max(dy) == 1,
                "8-connected {w}×{h} between {a:?} and {b:?}"
            );
            if dx + dy != 1 {
                diagonals += 1;
            }
        }
        if strictly_continuous(w, h) {
            assert_eq!(
                diagonals, 0,
                "same-parity {w}×{h} must be strictly 4-continuous"
            );
        }
    }

    #[test]
    fn covers_and_is_connected_for_many_shapes() {
        for w in 1..=40u32 {
            for h in 1..=40u32 {
                assert_valid(w, h);
            }
        }
    }

    #[test]
    fn parity_theorem_same_parity_is_strictly_continuous() {
        // Exhaustive within a range: same-parity ⇒ zero diagonals, always.
        for w in 1..=48u32 {
            for h in 1..=48u32 {
                if !strictly_continuous(w, h) {
                    continue;
                }
                let cells = gilbert_cells(w, h);
                for pair in cells.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    assert_eq!(
                        a.0.abs_diff(b.0) + a.1.abs_diff(b.1),
                        1,
                        "same-parity {w}×{h} diagonal at {a:?}->{b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn power_of_two_square_is_valid() {
        for order in 1..=7u32 {
            assert_valid(1 << order, 1 << order);
        }
    }

    #[test]
    fn direct_d2xy_agrees_with_enumeration() {
        // The O(log) point resolver must match the enumerated order for every cell.
        for w in 1..=33u32 {
            for h in 1..=33u32 {
                let cells = gilbert_cells(w, h);
                for (d, &xy) in cells.iter().enumerate() {
                    assert_eq!(gilbert_d2xy(w, h, d as u64), xy, "d2xy {w}×{h} d={d}");
                }
            }
        }
    }

    #[test]
    fn struct_round_trips_both_directions() {
        let g = Gilbert::new(23, 17);
        assert_eq!(g.len(), 23 * 17);
        for d in 0..g.len() {
            let (x, y) = g.d_to_xy(d);
            assert_eq!(g.xy_to_d(x, y), Some(d as u32), "round trip d={d}");
        }
        assert_eq!(g.xy_to_d(23, 0), None, "off-grid x");
        assert_eq!(g.xy_to_d(0, 17), None, "off-grid y");
    }

    #[test]
    fn contiguous_runs_stay_compact() {
        // The locality property that matters: a contiguous slice of the line occupies a
        // bounding box whose area is within a small constant of the slice length — i.e.
        // it reads as a blob, not a scattered spray. (A random Hamiltonian snake fails
        // this badly.) 8 is a generous ceiling; Hilbert-family curves sit well under it.
        let (w, h) = (64u32, 64u32);
        let g = Gilbert::new(w, h);
        for &len in &[16usize, 64, 256, 1024] {
            for &start in &[0usize, 777, 2048, 3000] {
                if start + len > g.len() {
                    continue;
                }
                let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0, 0);
                for d in start..start + len {
                    let (x, y) = g.d_to_xy(d);
                    minx = minx.min(x);
                    miny = miny.min(y);
                    maxx = maxx.max(x);
                    maxy = maxy.max(y);
                }
                let bbox = ((maxx - minx + 1) * (maxy - miny + 1)) as usize;
                assert!(bbox <= 8 * len, "run start={start} len={len} bbox={bbox}");
            }
        }
    }

    #[test]
    fn masked_order_excludes_holes_and_keeps_locality() {
        // Holes of arbitrary shape: forbid a central rectangle plus a scattered set.
        let (w, h) = (48u32, 48u32);
        let g = Gilbert::new(w, h);
        let forbidden = |x: u32, y: u32| {
            let central = (18..30).contains(&x) && (18..30).contains(&y);
            let scatter = (x * 7 + y * 13) % 11 == 0; // pseudo-scattershot exclusions
            central || scatter
        };
        let allowed = |x: u32, y: u32| !forbidden(x, y);
        let order = g.masked_order(allowed);

        // Length is exactly the allowed-cell count, and no forbidden cell appears.
        let allowed_count =
            (0..w * h).filter(|&i| allowed(i % w, i / w)).count();
        assert_eq!(order.len(), allowed_count, "line length == allowed area");
        assert!(order.iter().all(|&(x, y)| allowed(x, y)), "no hole in the line");
        // The filtered order is a subsequence of the full order (locality inherited).
        let full = g.cells();
        let mut fi = 0usize;
        for &c in &order {
            while full[fi] != c {
                fi += 1;
            }
            fi += 1;
        }

        // A contiguous slice of the *masked* line still lands as a compact blob.
        for &(start, len) in &[(0usize, 200usize), (500, 200), (1000, 300)] {
            if start + len > order.len() {
                continue;
            }
            let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0, 0);
            for &(x, y) in &order[start..start + len] {
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
            let bbox = ((maxx - minx + 1) * (maxy - miny + 1)) as usize;
            assert!(bbox <= 10 * len, "masked run start={start} len={len} bbox={bbox}");
        }
    }
}
