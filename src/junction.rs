// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Junction resolver: an [`EdgeGrid`] that accumulates directional border arms
//! per cell and resolves each to a Unicode box-drawing glyph via [`resolve`].
//!
//! ## Typical workflow
//!
//! ```
//! use mullion::{
//!     border::LineWeight,
//!     geometry::Rect,
//!     junction::{EdgeGrid, resolve},
//! };
//! let area = Rect::new(0, 0, 5, 5);
//! let mut grid = EdgeGrid::new(area);
//! grid.add_box(area, LineWeight::Light);
//! // Resolve one cell — the top-left corner should be '┌'.
//! let ch = resolve(grid.get(0, 0).unwrap());
//! assert_eq!(ch, Some('┌'));
//! ```
//!
//! ## Glyph rules
//!
//! 1. **Light/Heavy arms only:** every combination of absent/Light/Heavy per arm
//!    has a real glyph (straight lines, corners, tees, crosses, stubs).
//! 2. **All present arms Double:** pure-double glyphs — `═ ║ ╔╗╚╝ ╠╣╦╩ ╬`.
//! 3. **Double mixed with Light/Heavy, or a lone Double stub:** no Unicode glyph
//!    exists for these cases; every Double arm is demoted to Heavy and the result
//!    is resolved via rule 1.

use std::sync::OnceLock;

use crate::{border::LineWeight, geometry::Rect};

// ── EdgeCell ──────────────────────────────────────────────────────────────────

/// The four directional arms of a single terminal cell in the edge grid.
///
/// Each arm records the [`LineWeight`] of the border segment reaching the cell
/// from that direction, or `None` when no segment is present.  Arms are set by
/// [`EdgeGrid::add_h_line`] and [`EdgeGrid::add_v_line`]; when two segments
/// meet in the same direction the stronger weight wins (see the merge rule in
/// the private `stronger` helper).
#[derive(Debug, Default, Clone)]
pub struct EdgeCell {
    /// Arm connecting upward to the cell above.
    pub up: Option<LineWeight>,
    /// Arm connecting downward to the cell below.
    pub down: Option<LineWeight>,
    /// Arm connecting to the left.
    pub left: Option<LineWeight>,
    /// Arm connecting to the right.
    pub right: Option<LineWeight>,
}

// ── EdgeGrid ──────────────────────────────────────────────────────────────────

/// A 2-D grid of [`EdgeCell`]s covering `area`, into which border segments are
/// accumulated before being resolved to box-drawing glyphs.
///
/// Build the grid with [`add_h_line`](EdgeGrid::add_h_line),
/// [`add_v_line`](EdgeGrid::add_v_line), and [`add_box`](EdgeGrid::add_box),
/// then iterate over cells and call [`resolve`] on each to obtain the character
/// to render.
///
/// # Invariants
///
/// `cells.len() == area.width as usize * area.height as usize`.  All mutating
/// methods silently ignore coordinates outside `area`.
pub struct EdgeGrid {
    /// The terminal region this grid covers.
    pub area: Rect,
    cells: Vec<EdgeCell>,
}

impl EdgeGrid {
    /// Create an empty grid for `area`, with all arms absent.
    pub fn new(area: Rect) -> Self {
        let len = area.width as usize * area.height as usize;
        Self { area, cells: vec![EdgeCell::default(); len] }
    }

    /// Linear cell index for in-bounds `(x, y)`.
    #[inline]
    fn idx(&self, x: u16, y: u16) -> usize {
        (y - self.area.y) as usize * self.area.width as usize + (x - self.area.x) as usize
    }

    /// Set the cell's left arm to `w`, keeping the stronger of any existing weight.
    #[inline]
    fn set_left(&mut self, x: u16, y: u16, w: LineWeight) {
        let i = self.idx(x, y);
        self.cells[i].left = Some(stronger(self.cells[i].left, w));
    }

    /// Set the cell's right arm to `w`, keeping the stronger of any existing weight.
    #[inline]
    fn set_right(&mut self, x: u16, y: u16, w: LineWeight) {
        let i = self.idx(x, y);
        self.cells[i].right = Some(stronger(self.cells[i].right, w));
    }

    /// Set the cell's up arm to `w`, keeping the stronger of any existing weight.
    #[inline]
    fn set_up(&mut self, x: u16, y: u16, w: LineWeight) {
        let i = self.idx(x, y);
        self.cells[i].up = Some(stronger(self.cells[i].up, w));
    }

    /// Set the cell's down arm to `w`, keeping the stronger of any existing weight.
    #[inline]
    fn set_down(&mut self, x: u16, y: u16, w: LineWeight) {
        let i = self.idx(x, y);
        self.cells[i].down = Some(stronger(self.cells[i].down, w));
    }

    /// Add a horizontal line at row `y` covering columns `x0..=x1` at weight `w`.
    ///
    /// For each cell in the column range, sets `right` on every non-terminal
    /// cell and `left` on every non-first cell, merging (stronger weight wins)
    /// with any arm already present.  This means junction cells shared with a
    /// vertical line automatically accumulate both horizontal and vertical arms,
    /// producing the correct tee or cross glyph when resolved.
    ///
    /// Coordinates outside `self.area` are clamped/skipped silently.
    pub fn add_h_line(&mut self, x0: u16, x1: u16, y: u16, w: LineWeight) {
        if y < self.area.y || y >= self.area.y + self.area.height {
            return;
        }
        let ax = self.area.x;
        let ax1 = self.area.x + self.area.width - 1;
        let x0 = x0.max(ax);
        let x1 = x1.min(ax1);
        if x0 > x1 {
            return;
        }
        for x in x0..=x1 {
            if x > x0 { self.set_left(x, y, w); }
            if x < x1 { self.set_right(x, y, w); }
        }
    }

    /// Add a vertical line at column `x` covering rows `y0..=y1` at weight `w`.
    ///
    /// Sets `down` on every non-terminal cell and `up` on every non-first cell,
    /// merging with any arm already present.  Coordinates outside `self.area`
    /// are clamped/skipped silently.
    pub fn add_v_line(&mut self, y0: u16, y1: u16, x: u16, w: LineWeight) {
        if x < self.area.x || x >= self.area.x + self.area.width {
            return;
        }
        let ay = self.area.y;
        let ay1 = self.area.y + self.area.height - 1;
        let y0 = y0.max(ay);
        let y1 = y1.min(ay1);
        if y0 > y1 {
            return;
        }
        for y in y0..=y1 {
            if y > y0 { self.set_up(x, y, w); }
            if y < y1 { self.set_down(x, y, w); }
        }
    }

    /// Add the four sides of `area` as border lines at weight `w`.
    ///
    /// Calls [`add_h_line`](EdgeGrid::add_h_line) for the top and bottom rows
    /// and [`add_v_line`](EdgeGrid::add_v_line) for the left and right columns.
    /// Corner cells receive both a horizontal and a vertical arm, so they
    /// resolve to the correct corner glyph automatically.  Does nothing when
    /// `area` has zero width or height.
    pub fn add_box(&mut self, area: Rect, w: LineWeight) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let x0 = area.x;
        let x1 = area.x + area.width - 1;
        let y0 = area.y;
        let y1 = area.y + area.height - 1;
        self.add_h_line(x0, x1, y0, w);
        self.add_h_line(x0, x1, y1, w);
        self.add_v_line(y0, y1, x0, w);
        self.add_v_line(y0, y1, x1, w);
    }

    /// Return the [`EdgeCell`] at `(x, y)`, or `None` if outside `self.area`.
    pub fn get(&self, x: u16, y: u16) -> Option<&EdgeCell> {
        if x < self.area.x
            || x >= self.area.x + self.area.width
            || y < self.area.y
            || y >= self.area.y + self.area.height
        {
            return None;
        }
        Some(&self.cells[self.idx(x, y)])
    }
}

// ── Merge rule ────────────────────────────────────────────────────────────────

/// Return the stronger of `existing` and `new`, ordering `Light < Heavy < Double`.
fn stronger(existing: Option<LineWeight>, new: LineWeight) -> LineWeight {
    match (existing, new) {
        (Some(LineWeight::Double), _) | (_, LineWeight::Double) => LineWeight::Double,
        (Some(LineWeight::Heavy), _) | (_, LineWeight::Heavy) => LineWeight::Heavy,
        _ => LineWeight::Light,
    }
}

// ── Glyph resolver ────────────────────────────────────────────────────────────

/// Resolve one [`EdgeCell`] to its Unicode box-drawing glyph.
///
/// Total function — never panics.  Returns `None` only when all four arms are
/// absent; every other combination yields `Some(char)`.
///
/// ## Algorithm
///
/// Each arm is encoded as 2 bits: None=0, Light=1, Heavy=2, Double=3.  The
/// four codes are packed into an 8-bit lookup key `(up<<6)|(down<<4)|(left<<2)|right`.
/// Two 256-entry tables are built once on first call and cached for the lifetime
/// of the process:
///
/// - **LH table** — all combinations where every present arm is Light or Heavy.
///   Covers the 80 non-absent elements of the 3^4 = 81 valid Light/Heavy keys,
///   including stubs, lines, corners, tees, and crosses.
/// - **D table** — all combinations where every present arm is Double.  Only
///   11 entries (≥ 2 Double arms): `═ ║ ╔╗╚╝ ╠╣╦╩ ╬`.
///
/// Before the lookup the resolver classifies the cell:
/// 1. Count Double arms (`n_d`) and Light/Heavy arms (`n_lh`).
/// 2. If `n_d ≥ 2` and `n_lh == 0` → pure-double path (D table).
/// 3. Otherwise, if any Double arm is present (mixed, or lone stub) → demote
///    every Double arm to Heavy, then use the LH table.
/// 4. No Double arms → LH table directly.
pub fn resolve(cell: &EdgeCell) -> Option<char> {
    let u = arm_code(cell.up);
    let d = arm_code(cell.down);
    let l = arm_code(cell.left);
    let r = arm_code(cell.right);

    if u | d | l | r == 0 {
        return None;
    }

    let n_d = u8::from(u == 3) + u8::from(d == 3) + u8::from(l == 3) + u8::from(r == 3);
    let n_lh = u8::from(u == 1 || u == 2)
        + u8::from(d == 1 || d == 2)
        + u8::from(l == 1 || l == 2)
        + u8::from(r == 1 || r == 2);

    if n_d >= 2 && n_lh == 0 {
        return d_table()[pack(u, d, l, r) as usize];
    }

    // Demote every Double arm to Heavy (covers mixed Double+LH and lone Double stubs).
    let u = if u == 3 { 2 } else { u };
    let d = if d == 3 { 2 } else { d };
    let l = if l == 3 { 2 } else { l };
    let r = if r == 3 { 2 } else { r };

    lh_table()[pack(u, d, l, r) as usize]
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Encode an arm's optional weight as 2 bits: None=0, Light=1, Heavy=2, Double=3.
#[inline]
fn arm_code(w: Option<LineWeight>) -> u8 {
    match w {
        None => 0,
        Some(LineWeight::Light) => 1,
        Some(LineWeight::Heavy) => 2,
        Some(LineWeight::Double) => 3,
    }
}

/// Pack four 2-bit arm codes: `(up<<6) | (down<<4) | (left<<2) | right`.
#[inline]
fn pack(up: u8, down: u8, left: u8, right: u8) -> u8 {
    (up << 6) | (down << 4) | (left << 2) | right
}

/// The lazily-built, process-lifetime-cached 256-entry Light/Heavy lookup table.
fn lh_table() -> &'static [Option<char>; 256] {
    static T: OnceLock<[Option<char>; 256]> = OnceLock::new();
    T.get_or_init(build_lh_table)
}

/// The lazily-built, process-lifetime-cached 256-entry pure-Double lookup table.
fn d_table() -> &'static [Option<char>; 256] {
    static T: OnceLock<[Option<char>; 256]> = OnceLock::new();
    T.get_or_init(build_d_table)
}

/// Build the Light/Heavy lookup table.
///
/// Key encoding: `(up<<6)|(down<<4)|(left<<2)|right` where each field is
/// None=0, Light=1, Heavy=2.  There are 3^4=81 valid keys; 80 map to glyphs
/// (the all-absent key 0 is `None`).  All 256 slots outside the valid 81 are
/// never accessed after arm demotion but remain `None` as a safety net.
fn build_lh_table() -> [Option<char>; 256] {
    let mut t = [None::<char>; 256];
    macro_rules! e {
        ($u:expr,$d:expr,$l:expr,$r:expr,$ch:literal) => {
            t[pack($u, $d, $l, $r) as usize] = Some($ch);
        };
    }
    // Stubs (one arm)
    e!(0,0,0,1,'╶'); e!(0,0,0,2,'╺');
    e!(0,0,1,0,'╴'); e!(0,0,2,0,'╸');
    e!(0,1,0,0,'╷'); e!(0,2,0,0,'╻');
    e!(1,0,0,0,'╵'); e!(2,0,0,0,'╹');
    // Horizontal lines (opposite arms, no vertical)
    e!(0,0,1,1,'─'); e!(0,0,1,2,'╼'); e!(0,0,2,1,'╾'); e!(0,0,2,2,'━');
    // Vertical lines (opposite arms, no horizontal)
    e!(1,1,0,0,'│'); e!(1,2,0,0,'╽'); e!(2,1,0,0,'╿'); e!(2,2,0,0,'┃');
    // Corners: top-left (down+right)
    e!(0,1,0,1,'┌'); e!(0,1,0,2,'┍'); e!(0,2,0,1,'┎'); e!(0,2,0,2,'┏');
    // Corners: top-right (down+left)
    e!(0,1,1,0,'┐'); e!(0,1,2,0,'┑'); e!(0,2,1,0,'┒'); e!(0,2,2,0,'┓');
    // Corners: bottom-left (up+right)
    e!(1,0,0,1,'└'); e!(1,0,0,2,'┕'); e!(2,0,0,1,'┖'); e!(2,0,0,2,'┗');
    // Corners: bottom-right (up+left)
    e!(1,0,1,0,'┘'); e!(1,0,2,0,'┙'); e!(2,0,1,0,'┚'); e!(2,0,2,0,'┛');
    // Down tees (up arm absent)
    e!(0,1,1,1,'┬'); e!(0,1,1,2,'┮'); e!(0,1,2,1,'┭'); e!(0,1,2,2,'┯');
    e!(0,2,1,1,'┰'); e!(0,2,1,2,'┲'); e!(0,2,2,1,'┱'); e!(0,2,2,2,'┳');
    // Up tees (down arm absent)
    e!(1,0,1,1,'┴'); e!(1,0,1,2,'┶'); e!(1,0,2,1,'┵'); e!(1,0,2,2,'┷');
    e!(2,0,1,1,'┸'); e!(2,0,1,2,'┺'); e!(2,0,2,1,'┹'); e!(2,0,2,2,'┻');
    // Right tees (left arm absent)
    e!(1,1,0,1,'├'); e!(1,1,0,2,'┝'); e!(1,2,0,1,'┟'); e!(1,2,0,2,'┢');
    e!(2,1,0,1,'┞'); e!(2,1,0,2,'┡'); e!(2,2,0,1,'┠'); e!(2,2,0,2,'┣');
    // Left tees (right arm absent)
    e!(1,1,1,0,'┤'); e!(1,1,2,0,'┥'); e!(1,2,1,0,'┧'); e!(1,2,2,0,'┪');
    e!(2,1,1,0,'┦'); e!(2,1,2,0,'┩'); e!(2,2,1,0,'┨'); e!(2,2,2,0,'┫');
    // Crosses (all four arms)
    e!(1,1,1,1,'┼'); e!(1,1,1,2,'┾'); e!(1,1,2,1,'┽'); e!(1,1,2,2,'┿');
    e!(1,2,1,1,'╁'); e!(1,2,1,2,'╆'); e!(1,2,2,1,'╅'); e!(1,2,2,2,'╈');
    e!(2,1,1,1,'╀'); e!(2,1,1,2,'╄'); e!(2,1,2,1,'╃'); e!(2,1,2,2,'╇');
    e!(2,2,1,1,'╂'); e!(2,2,1,2,'╊'); e!(2,2,2,1,'╉'); e!(2,2,2,2,'╋');
    t
}

/// Build the pure-Double lookup table.
///
/// Same key encoding, with Double=3.  Only the 11 valid pure-double glyphs
/// (two or more Double arms, all others absent) receive entries; lone Double
/// stubs and mixed cases never reach this table.
fn build_d_table() -> [Option<char>; 256] {
    let mut t = [None::<char>; 256];
    macro_rules! e {
        ($u:expr,$d:expr,$l:expr,$r:expr,$ch:literal) => {
            t[pack($u, $d, $l, $r) as usize] = Some($ch);
        };
    }
    e!(0,0,3,3,'═'); e!(3,3,0,0,'║');
    e!(0,3,0,3,'╔'); e!(0,3,3,0,'╗'); e!(3,0,0,3,'╚'); e!(3,0,3,0,'╝');
    e!(0,3,3,3,'╦'); e!(3,0,3,3,'╩'); e!(3,3,0,3,'╠'); e!(3,3,3,0,'╣');
    e!(3,3,3,3,'╬');
    t
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use LineWeight::{Double, Heavy, Light};

    fn cell(up: Option<LineWeight>, down: Option<LineWeight>, left: Option<LineWeight>, right: Option<LineWeight>) -> EdgeCell {
        EdgeCell { up, down, left, right }
    }

    const L: Option<LineWeight> = Some(Light);
    const H: Option<LineWeight> = Some(Heavy);
    const D: Option<LineWeight> = Some(Double);
    const N: Option<LineWeight> = None;

    #[test]
    fn exhaustive_256_combinations() {
        let arms = [None, Some(Light), Some(Heavy), Some(Double)];
        for &up in &arms {
            for &down in &arms {
                for &left in &arms {
                    for &right in &arms {
                        let c = EdgeCell { up, down, left, right };
                        let result = resolve(&c);
                        if up.is_none() && down.is_none() && left.is_none() && right.is_none() {
                            assert_eq!(result, None, "all-absent must be None");
                        } else {
                            assert!(result.is_some(), "expected Some for {c:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn pure_light_glyphs() {
        // Stubs
        assert_eq!(resolve(&cell(N,N,N,L)), Some('╶'));
        assert_eq!(resolve(&cell(N,N,L,N)), Some('╴'));
        assert_eq!(resolve(&cell(N,L,N,N)), Some('╷'));
        assert_eq!(resolve(&cell(L,N,N,N)), Some('╵'));
        // Lines
        assert_eq!(resolve(&cell(N,N,L,L)), Some('─'));
        assert_eq!(resolve(&cell(L,L,N,N)), Some('│'));
        // Corners
        assert_eq!(resolve(&cell(N,L,N,L)), Some('┌'));
        assert_eq!(resolve(&cell(N,L,L,N)), Some('┐'));
        assert_eq!(resolve(&cell(L,N,N,L)), Some('└'));
        assert_eq!(resolve(&cell(L,N,L,N)), Some('┘'));
        // Tees
        assert_eq!(resolve(&cell(N,L,L,L)), Some('┬'));
        assert_eq!(resolve(&cell(L,N,L,L)), Some('┴'));
        assert_eq!(resolve(&cell(L,L,N,L)), Some('├'));
        assert_eq!(resolve(&cell(L,L,L,N)), Some('┤'));
        // Cross
        assert_eq!(resolve(&cell(L,L,L,L)), Some('┼'));
    }

    #[test]
    fn pure_heavy_glyphs() {
        // Stubs
        assert_eq!(resolve(&cell(N,N,N,H)), Some('╺'));
        assert_eq!(resolve(&cell(N,N,H,N)), Some('╸'));
        assert_eq!(resolve(&cell(N,H,N,N)), Some('╻'));
        assert_eq!(resolve(&cell(H,N,N,N)), Some('╹'));
        // Lines
        assert_eq!(resolve(&cell(N,N,H,H)), Some('━'));
        assert_eq!(resolve(&cell(H,H,N,N)), Some('┃'));
        // Corners
        assert_eq!(resolve(&cell(N,H,N,H)), Some('┏'));
        assert_eq!(resolve(&cell(N,H,H,N)), Some('┓'));
        assert_eq!(resolve(&cell(H,N,N,H)), Some('┗'));
        assert_eq!(resolve(&cell(H,N,H,N)), Some('┛'));
        // Tees
        assert_eq!(resolve(&cell(N,H,H,H)), Some('┳'));
        assert_eq!(resolve(&cell(H,N,H,H)), Some('┻'));
        assert_eq!(resolve(&cell(H,H,N,H)), Some('┣'));
        assert_eq!(resolve(&cell(H,H,H,N)), Some('┫'));
        // Cross
        assert_eq!(resolve(&cell(H,H,H,H)), Some('╋'));
    }

    #[test]
    fn pure_double_glyphs() {
        assert_eq!(resolve(&cell(N,N,D,D)), Some('═'));
        assert_eq!(resolve(&cell(D,D,N,N)), Some('║'));
        assert_eq!(resolve(&cell(N,D,N,D)), Some('╔'));
        assert_eq!(resolve(&cell(N,D,D,N)), Some('╗'));
        assert_eq!(resolve(&cell(D,N,N,D)), Some('╚'));
        assert_eq!(resolve(&cell(D,N,D,N)), Some('╝'));
        assert_eq!(resolve(&cell(N,D,D,D)), Some('╦'));
        assert_eq!(resolve(&cell(D,N,D,D)), Some('╩'));
        assert_eq!(resolve(&cell(D,D,N,D)), Some('╠'));
        assert_eq!(resolve(&cell(D,D,D,N)), Some('╣'));
        assert_eq!(resolve(&cell(D,D,D,D)), Some('╬'));
    }

    #[test]
    fn mixed_light_heavy_corner_tee_cross() {
        // Corner: down=Light + right=Heavy → ┍
        assert_eq!(resolve(&cell(N,L,N,H)), Some('┍'));
        // Tee: down=Light + left=Heavy + right=Heavy → ┯
        assert_eq!(resolve(&cell(N,L,H,H)), Some('┯'));
        // Cross: up=Light + down=Light + left=Heavy + right=Heavy → ┿
        assert_eq!(resolve(&cell(L,L,H,H)), Some('┿'));
    }

    #[test]
    fn double_mixing_demotes_to_heavy() {
        // Lone Double stub: single arm Double → demotes to Heavy stub
        assert_eq!(resolve(&cell(N,N,N,D)), Some('╺'));
        // Double up + Light right → demote Double→Heavy → (up=H, right=L) = ┖
        assert_eq!(resolve(&cell(D,N,N,L)), Some('┖'));
    }

    #[test]
    fn merge_stronger_weight_wins() {
        let mut grid = EdgeGrid::new(Rect::new(0, 0, 5, 5));
        grid.add_h_line(1, 3, 2, Light);
        grid.add_h_line(1, 3, 2, Heavy); // heavy should win
        let c = grid.get(2, 2).unwrap();
        assert_eq!(c.left, Some(Heavy));
        assert_eq!(c.right, Some(Heavy));
        assert_eq!(resolve(c), Some('━'));
    }
}
