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

/// Whether an allowed region can host a **continuous** (4-connected) space-filling
/// strand — the necessary gate from the checkerboard-parity theorem. See
/// [`region_feasibility`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feasibility {
    /// The allowed cells form a single 4-connected component.
    pub connected: bool,
    /// `#black − #white` over the allowed cells on a checkerboard colouring `(x+y)%2`.
    pub color_balance: i32,
    /// A Hamiltonian **path** (open strand) can exist: connected and `|balance| ≤ 1`.
    pub path_possible: bool,
    /// A Hamiltonian **cycle** (closed loop) can exist: connected and `balance == 0`.
    pub cycle_possible: bool,
}

/// Test whether an allowed/forbidden region *could* carry a continuous unit-step strand
/// visiting every allowed cell once — the exclusion-zone gate. A morphing mask that would
/// break [`path_possible`](Feasibility::path_possible) is one the repulsion must reject.
///
/// This checks the two **necessary** conditions exactly, in `O(width·height)`:
/// - **connectivity** — the allowed cells are one 4-connected blob (else no single strand
///   can reach them all);
/// - **parity balance** — a 4-connected path alternates black/white, so it needs
///   `|#black − #white| ≤ 1` (a closed cycle needs `= 0`). A balanced hole (e.g. an even×even
///   block) leaves the balance untouched wherever it sits — which is why such holes may bob
///   freely, while an odd-area hole shifts the balance and can forbid a closed tour.
///
/// These are necessary, not sufficient (grid-graph Hamiltonicity is NP-hard in general);
/// a constructive engine narrows the admissible masks further (e.g. to 2×2-block-aligned
/// regions) so that passing the gate also *guarantees* a curve.
#[must_use]
pub fn region_feasibility(
    width: u32,
    height: u32,
    allowed: impl Fn(u32, u32) -> bool,
) -> Feasibility {
    let (w, h) = (width as usize, height as usize);
    let is_allowed: Vec<bool> = (0..w * h).map(|i| allowed((i % w) as u32, (i / w) as u32)).collect();

    // Checkerboard balance over the allowed cells.
    let mut balance = 0i32;
    let mut first = None;
    let mut total = 0usize;
    for (i, &a) in is_allowed.iter().enumerate() {
        if a {
            total += 1;
            first.get_or_insert(i);
            if (i % w + i / w) % 2 == 0 {
                balance += 1;
            } else {
                balance -= 1;
            }
        }
    }

    // Connectivity: flood-fill from the first allowed cell.
    let connected = match first {
        None => false, // an empty region hosts no strand
        Some(start) => {
            let mut seen = vec![false; w * h];
            let mut stack = vec![start];
            seen[start] = true;
            let mut reached = 0usize;
            while let Some(id) = stack.pop() {
                reached += 1;
                let (x, y) = (id % w, id / w);
                let mut push = |nx: usize, ny: usize| {
                    let nid = ny * w + nx;
                    if is_allowed[nid] && !seen[nid] {
                        seen[nid] = true;
                        stack.push(nid);
                    }
                };
                if x > 0 { push(x - 1, y); }
                if x + 1 < w { push(x + 1, y); }
                if y > 0 { push(x, y - 1); }
                if y + 1 < h { push(x, y + 1); }
            }
            reached == total
        }
    };

    Feasibility {
        connected,
        color_balance: balance,
        path_possible: connected && balance.abs() <= 1,
        cycle_possible: connected && balance == 0,
    }
}

/// Build a **continuous** (4-connected) Hamiltonian cycle visiting every allowed cell of
/// a **2×2-block-aligned** mask — a genuine unbroken space-filling strand through the
/// holes, not just a filtered order.
///
/// The construction is the spanning-tree space-filling curve, which needs no search and
/// is always continuous when it applies: seed each allowed 2×2 block as a 4-cell loop,
/// then for every edge of a spanning tree of the block-adjacency graph, splice the two
/// blocks' loops into one (remove the shared-side edge from each loop, add the two
/// cross-boundary edges). A spanning tree has exactly `blocks − 1` edges and no cycles,
/// so each splice joins two *distinct* loops — all block-loops merge into a single cycle
/// over every allowed cell. Locality follows the tree, grown in Gilbert order so a
/// contiguous run of the resulting line stays a compact blob.
///
/// Returns `None` — the configurations the exclusion zone must repel — when `width` or
/// `height` is odd, when the mask is not block-aligned (some 2×2 block is only partly
/// allowed), or when the allowed blocks are not a single connected region. `O(w·h)`.
#[must_use]
pub fn spanning_curve(
    width: u32,
    height: u32,
    allowed: impl Fn(u32, u32) -> bool,
) -> Option<Vec<(u32, u32)>> {
    if width % 2 != 0 || height % 2 != 0 {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let (bw, bh) = (w / 2, h / 2); // block grid

    // Block-allowed + alignment: every 2×2 block must be fully allowed or fully forbidden.
    let mut ballowed = vec![false; bw * bh];
    for by in 0..bh {
        for bx in 0..bw {
            let cells = [(2 * bx, 2 * by), (2 * bx + 1, 2 * by), (2 * bx, 2 * by + 1), (2 * bx + 1, 2 * by + 1)];
            let cnt = cells.iter().filter(|&&(x, y)| allowed(x as u32, y as u32)).count();
            match cnt {
                4 => ballowed[by * bw + bx] = true,
                0 => {}
                _ => return None, // a split block — mask is not 2×2-aligned
            }
        }
    }

    // Blocks in Gilbert order (for a locality-friendly spanning tree), filtered to allowed.
    let gb = Gilbert::new(bw as u32, bh as u32);
    let order_blocks: Vec<usize> =
        gb.cells().iter().map(|&(x, y)| y as usize * bw + x as usize).filter(|&b| ballowed[b]).collect();
    let start = *order_blocks.first()?;
    let mut grank = vec![usize::MAX; bw * bh];
    for (i, &b) in order_blocks.iter().enumerate() {
        grank[b] = i;
    }

    // DFS spanning tree over allowed blocks, exploring neighbours in Gilbert-rank order so
    // the tree hugs the Hilbert path.
    let mut visited = vec![false; bw * bh];
    let mut tree: Vec<(usize, usize)> = Vec::new();
    let mut stack = vec![start];
    visited[start] = true;
    while let Some(cur) = stack.pop() {
        let (cbx, cby) = ((cur % bw) as isize, (cur / bw) as isize);
        let mut nbrs = Vec::new();
        for (nx, ny) in [(cbx - 1, cby), (cbx + 1, cby), (cbx, cby - 1), (cbx, cby + 1)] {
            if nx >= 0 && ny >= 0 && (nx as usize) < bw && (ny as usize) < bh {
                let nb = ny as usize * bw + nx as usize;
                if ballowed[nb] && !visited[nb] {
                    nbrs.push(nb);
                }
            }
        }
        nbrs.sort_by_key(|&b| grank[b]);
        for &nb in nbrs.iter().rev() {
            if !visited[nb] {
                visited[nb] = true;
                tree.push((cur, nb));
                stack.push(nb);
            }
        }
    }
    if order_blocks.iter().any(|&b| !visited[b]) {
        return None; // allowed blocks are disconnected
    }

    // Fine-cell 2-regular graph: `nbr[cell]` holds the (≤2) cycle neighbours.
    let mut nbr = vec![[-1i64, -1i64]; w * h];
    let cell = |x: usize, y: usize| y * w + x;
    fn link(nbr: &mut [[i64; 2]], a: usize, b: usize) {
        for s in &mut nbr[a] {
            if *s < 0 {
                *s = b as i64;
                break;
            }
        }
        for s in &mut nbr[b] {
            if *s < 0 {
                *s = a as i64;
                break;
            }
        }
    }
    fn unlink(nbr: &mut [[i64; 2]], a: usize, b: usize) {
        for s in &mut nbr[a] {
            if *s == b as i64 {
                *s = -1;
                break;
            }
        }
        for s in &mut nbr[b] {
            if *s == a as i64 {
                *s = -1;
                break;
            }
        }
    }

    // Seed each allowed block as a 4-cell loop TL-TR-BR-BL-TL.
    for by in 0..bh {
        for bx in 0..bw {
            if !ballowed[by * bw + bx] {
                continue;
            }
            let (tl, tr) = (cell(2 * bx, 2 * by), cell(2 * bx + 1, 2 * by));
            let (br, bl) = (cell(2 * bx + 1, 2 * by + 1), cell(2 * bx, 2 * by + 1));
            link(&mut nbr, tl, tr);
            link(&mut nbr, tr, br);
            link(&mut nbr, br, bl);
            link(&mut nbr, bl, tl);
        }
    }

    // Splice loops along each tree edge. Orient so `B` is immediately east or south of `A`.
    for &(p, q) in &tree {
        let (pbx, pby) = (p % bw, p / bw);
        let (qbx, qby) = (q % bw, q / bw);
        let (abx, aby, bbx, bby, east) = if qby == pby && qbx == pbx + 1 {
            (pbx, pby, qbx, qby, true)
        } else if pby == qby && pbx == qbx + 1 {
            (qbx, qby, pbx, pby, true)
        } else if qbx == pbx && qby == pby + 1 {
            (pbx, pby, qbx, qby, false)
        } else {
            (qbx, qby, pbx, pby, false)
        };
        if east {
            // A's right edge (TR-BR) and B's left edge (TL-BL) → two horizontal crosses.
            let (a_tr, a_br) = (cell(2 * abx + 1, 2 * aby), cell(2 * abx + 1, 2 * aby + 1));
            let (b_tl, b_bl) = (cell(2 * bbx, 2 * bby), cell(2 * bbx, 2 * bby + 1));
            unlink(&mut nbr, a_tr, a_br);
            unlink(&mut nbr, b_tl, b_bl);
            link(&mut nbr, a_tr, b_tl);
            link(&mut nbr, a_br, b_bl);
        } else {
            // A's bottom edge (BL-BR) and B's top edge (TL-TR) → two vertical crosses.
            let (a_bl, a_br) = (cell(2 * abx, 2 * aby + 1), cell(2 * abx + 1, 2 * aby + 1));
            let (b_tl, b_tr) = (cell(2 * bbx, 2 * bby), cell(2 * bbx + 1, 2 * bby));
            unlink(&mut nbr, a_bl, a_br);
            unlink(&mut nbr, b_tl, b_tr);
            link(&mut nbr, a_bl, b_tl);
            link(&mut nbr, a_br, b_tr);
        }
    }

    // Walk the single cycle from a start cell.
    let total = order_blocks.len() * 4;
    let start_cell = cell(2 * (start % bw), 2 * (start / bw)) as i64;
    let mut order = Vec::with_capacity(total);
    let (mut prev, mut cur) = (-1i64, start_cell);
    loop {
        order.push(((cur as usize % w) as u32, (cur as usize / w) as u32));
        let [n0, n1] = nbr[cur as usize];
        let next = if n0 != prev && n0 >= 0 { n0 } else { n1 };
        if next < 0 {
            return None; // malformed (should not happen for a valid spanning tree)
        }
        prev = cur;
        cur = next;
        if cur == start_cell {
            break;
        }
    }
    (order.len() == total).then_some(order)
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

    #[test]
    fn feasibility_gate_reads_connectivity_and_parity() {
        // Full even×even grid: balanced and connected → a closed tour is possible.
        let f = region_feasibility(8, 8, |_, _| true);
        assert!(f.connected && f.color_balance == 0 && f.cycle_possible);

        // Full odd-area grid (7×7 = 49): imbalance 1 → path yes, cycle no.
        let f = region_feasibility(7, 7, |_, _| true);
        assert_eq!(f.color_balance.abs(), 1);
        assert!(f.path_possible && !f.cycle_possible);

        // A balanced 2×2 hole anywhere leaves the balance at 0 (holes may bob freely).
        for (hx, hy) in [(0u32, 0u32), (2, 4), (6, 6)] {
            let f = region_feasibility(8, 8, |x, y| {
                !((hx..hx + 2).contains(&x) && (hy..hy + 2).contains(&y))
            });
            assert_eq!(f.color_balance, 0, "2×2 hole at ({hx},{hy}) stays balanced");
            assert!(f.cycle_possible);
        }

        // A single-cell hole unbalances by 1 → no closed tour, but a path still can.
        let f = region_feasibility(8, 8, |x, y| !(x == 3 && y == 3));
        assert_eq!(f.color_balance.abs(), 1);
        assert!(f.path_possible && !f.cycle_possible);

        // Two separated allowed patches → disconnected → no single strand at all.
        let f = region_feasibility(8, 8, |x, _y| x < 3 || x > 4);
        // (a full-height gap at columns 3..=4 splits the region in two)
        assert!(!f.connected && !f.path_possible);
    }

    /// A `spanning_curve` result must visit every allowed cell once, in unit steps, and
    /// close back on itself (a genuine continuous Hamiltonian cycle).
    fn assert_continuous_cycle(w: u32, h: u32, order: &[(u32, u32)], allowed: impl Fn(u32, u32) -> bool) {
        let expect = (0..w * h).filter(|&i| allowed(i % w, i / w)).count();
        assert_eq!(order.len(), expect, "covers every allowed cell once");
        let set: HashSet<_> = order.iter().copied().collect();
        assert_eq!(set.len(), order.len(), "no repeats");
        assert!(order.iter().all(|&(x, y)| allowed(x, y)), "never enters a hole");
        for i in 0..order.len() {
            let a = order[i];
            let b = order[(i + 1) % order.len()]; // wrap → checks closure too
            assert_eq!(a.0.abs_diff(b.0) + a.1.abs_diff(b.1), 1, "unit step {a:?}->{b:?}");
        }
    }

    #[test]
    fn spanning_curve_is_a_continuous_cycle_over_holes() {
        // Solid even grid.
        let all = |_: u32, _: u32| true;
        let c = spanning_curve(16, 16, all).expect("solid grid");
        assert_continuous_cycle(16, 16, &c, all);

        // A single 2×2-aligned hole.
        let hole1 = |x: u32, y: u32| !((6..8).contains(&x) && (6..8).contains(&y));
        let c = spanning_curve(16, 16, hole1).expect("one hole");
        assert_continuous_cycle(16, 16, &c, hole1);

        // Several holes of different even sizes and positions (all block-aligned).
        let holes = |x: u32, y: u32| {
            let a = (2..6).contains(&x) && (2..4).contains(&y); // 4×2
            let b = (10..14).contains(&x) && (10..16).contains(&y); // 4×6
            let d = (14..16).contains(&x) && (2..4).contains(&y); // 2×2
            !(a || b || d)
        };
        let c = spanning_curve(20, 20, holes).expect("multi holes");
        assert_continuous_cycle(20, 20, &c, holes);

        // Inverse mask (mostly forbidden, a connected allowed patch).
        let patch = |x: u32, y: u32| (4..12).contains(&x) && (4..12).contains(&y);
        let c = spanning_curve(20, 20, patch).expect("patch");
        assert_continuous_cycle(20, 20, &c, patch);
    }

    #[test]
    fn spanning_curve_rejects_infeasible_masks() {
        // Odd dimension.
        assert!(spanning_curve(15, 16, |_, _| true).is_none());
        // Not 2×2-aligned: a single-cell hole splits a block.
        assert!(spanning_curve(16, 16, |x, y| !(x == 5 && y == 5)).is_none());
        // Disconnected allowed blocks (a full-height forbidden channel, block-aligned).
        assert!(spanning_curve(16, 16, |x, _y| !(8..10).contains(&x)).is_none());
    }
}
