// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Layout tree: partition a [`Rect`] into a tree of named tiles.
//!
//! The public API has three layers:
//!
//! 1. **Constraint** — how much space a single child requests.
//! 2. **Node** — either a leaf tile or a split that owns children.
//! 3. **`solve`** / **`min_size`** — the solver functions.
//!
//! ## Coordinate model
//!
//! All positions and sizes are in terminal *columns × rows* (`u16`).  Overflow
//! is handled with saturating arithmetic so a degenerate tree never panics.
//!
//! ## Adaptive orientation
//!
//! `Orientation::Adaptive` picks the axis that better fits the available area.
//! To prevent flicker when dimensions hover near the boundary it applies a
//! hysteresis margin: the current axis is kept unless the *other* axis is
//! strictly better by more than `margin_pct` percent of the relevant dimension.

use crate::geometry::Rect;

// ── Axis ─────────────────────────────────────────────────────────────────────

/// The axis along which a split lays out its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children are placed side-by-side (left to right).
    Horizontal,
    /// Children are stacked (top to bottom).
    Vertical,
}

// ── Orientation ──────────────────────────────────────────────────────────────

/// Determines which axis a `Split` node uses to arrange its children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Orientation {
    /// Always lay children out left-to-right.
    Horizontal,
    /// Always lay children out top-to-bottom.
    Vertical,
    /// Choose the axis at solve-time based on the available area.
    ///
    /// If `width > height` the horizontal axis is preferred, otherwise
    /// vertical.  A hysteresis margin (`margin_pct` percent of the
    /// relevant dimension) suppresses flipping when the area is nearly
    /// square, reducing flicker.
    ///
    /// `last` is the axis chosen the previous time this node was solved.
    /// Set to `None` for the first solve.
    Adaptive {
        /// Hysteresis margin as a percentage (0–100) of the tested dimension.
        /// A value of 10 means the current axis is kept unless switching would
        /// yield at least 10 % more space on the preferred axis.
        margin_pct: u16,
        /// The axis that was chosen on the most recent solve call.
        last: Option<Axis>,
    },
}

impl Orientation {
    /// Resolve to a concrete [`Axis`], updating `last` for `Adaptive`.
    ///
    /// For `Horizontal` and `Vertical` this is trivial.  For `Adaptive` the
    /// logic is:
    /// 1. Start with the previous axis (or `Horizontal` if this is the first
    ///    solve).
    /// 2. Compute how wide and how tall the area is.
    /// 3. Flip the axis only when the candidate is strictly better than the
    ///    current axis by more than `margin_pct` percent of the relevant
    ///    dimension (hysteresis).
    pub(crate) fn resolve(&mut self, area: Rect) -> Axis {
        match self {
            Orientation::Horizontal => Axis::Horizontal,
            Orientation::Vertical => Axis::Vertical,
            Orientation::Adaptive { margin_pct, last } => {
                let current = last.unwrap_or(Axis::Horizontal);
                let w = area.width as u32;
                let h = area.height as u32;
                // Margin: current axis is kept unless other is strictly better
                // by more than margin_pct% of that dimension.
                let chosen = match current {
                    Axis::Horizontal => {
                        // Keep horizontal unless height is sufficiently larger than width.
                        let threshold = w + w * (*margin_pct as u32) / 100;
                        if h > threshold { Axis::Vertical } else { Axis::Horizontal }
                    }
                    Axis::Vertical => {
                        // Keep vertical unless width is sufficiently larger than height.
                        let threshold = h + h * (*margin_pct as u32) / 100;
                        if w > threshold { Axis::Horizontal } else { Axis::Vertical }
                    }
                };
                *last = Some(chosen);
                chosen
            }
        }
    }
}

// ── Size ─────────────────────────────────────────────────────────────────────

/// How much space a child requests along the split axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Exactly `n` columns (horizontal split) or rows (vertical split).
    Fixed(u16),
    /// `p` percent of the available space (clamped to 0–100).
    Percent(u16),
    /// Claim a proportional share of leftover space after `Fixed` and
    /// `Percent` children are satisfied.  The weight is relative: a
    /// `Fill(2)` child gets twice as much leftover as a `Fill(1)` child.
    Fill(u16),
}

// ── Constraint ───────────────────────────────────────────────────────────────

/// Space request for one child in a `Split`, with optional clamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Constraint {
    /// The primary sizing mode.
    pub size: Size,
    /// Hard lower bound in cells.  The computed size is always `>= min`.
    pub min: u16,
    /// Hard upper bound in cells.  The computed size is always `<= max`.
    /// Defaults to `u16::MAX` (unconstrained).
    pub max: u16,
}

impl Constraint {
    /// Construct a constraint with no min/max clamps.
    pub fn new(size: Size) -> Self {
        Self { size, min: 0, max: u16::MAX }
    }

    /// Set the minimum cell count.
    pub fn with_min(mut self, min: u16) -> Self {
        self.min = min;
        self
    }

    /// Set the maximum cell count.
    pub fn with_max(mut self, max: u16) -> Self {
        self.max = max;
        self
    }
}

impl Default for Constraint {
    /// A single Fill(1) child with no clamps — occupies all available space.
    fn default() -> Self {
        Self::new(Size::Fill(1))
    }
}

// ── TileId / Node ─────────────────────────────────────────────────────────────

/// Opaque identifier for a leaf tile, chosen by the caller.
pub type TileId = u64;

/// A node in the layout tree.
///
/// A `Tile` is a leaf that maps to a rectangular area in the output.
/// A `Split` subdivides its assigned area among child nodes according to
/// their [`Constraint`]s and the split [`Orientation`].
/// A `Carousel` is a scrollable strip: children extend beyond the viewport
/// along the main axis and are virtualized — only those that intersect the
/// viewport produce rects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A leaf tile with a caller-assigned identifier.
    Tile(TileId),
    /// A node that partitions its area among its children.
    Split {
        /// Which axis (or adaptive choice) to use for the split.
        orientation: Orientation,
        /// Ordered list of `(constraint, child)` pairs.
        ///
        /// Constraints are applied in order; the last `Fill` child absorbs
        /// any rounding remainder to guarantee the areas tile exactly.
        children: Vec<(Constraint, Node)>,
    },
    /// A scrollable strip whose children may extend beyond the viewport along
    /// the main axis.
    ///
    /// Children have a fixed **main-axis extent** (cells); the cross axis
    /// fills the full available space.  `scroll` is the offset in cells from
    /// the start of the child list.  Has its own `id` so it can be addressed
    /// (scrolled, reconciled) via [`node_by_id`](crate::tree::node_by_id)
    /// without being a focusable leaf.
    ///
    /// Only the children whose extents intersect the viewport produce rects
    /// (virtualization); off-screen children cost nothing at solve-time.
    /// Partial tiles at either edge are clipped to the viewport boundary.
    Carousel {
        /// Caller-assigned identifier for this container.  Not a focusable
        /// leaf — addressed through `node_by_id`, not via `Tree::focus`.
        id: TileId,
        /// Scroll direction.  `Horizontal` scrolls left/right; `Vertical`
        /// scrolls up/down.  `Adaptive` is resolved at solve-time using the
        /// same hysteresis logic as [`Node::Split`].
        orientation: Orientation,
        /// Offset in cells from the first child.  Clamped in place by `solve`
        /// so the last child's tail is always flush with the viewport end —
        /// no blank gap past the content.
        scroll: u16,
        /// Ordered list of `(main_axis_extent, child)` pairs.  Each extent is
        /// the number of cells the child occupies along the main axis before
        /// viewport clipping.
        children: Vec<(u16, Node)>,
    },
}

// ── Carousel visibility helper ────────────────────────────────────────────────

/// Clamp `scroll` and collect the visible children for a carousel viewport.
///
/// Takes the children's main-axis `extents`, the requested `scroll` offset, and
/// the viewport `main_extent`.  Returns `(clamped_scroll, entries)` where each
/// entry is `(child_idx, virtual_start, extent)`:
/// - `child_idx` — index into the caller's children slice.
/// - `virtual_start` — offset in content space (0 = start of first child).
/// - `extent` — the child's full main-axis extent, before viewport clipping.
///
/// Only children whose extent overlaps `[clamped_scroll, clamped_scroll +
/// main_extent)` are included.  `virtual_start` uses `u32` to avoid overflow
/// when many large children are present.
///
/// Used by both `solve_into` (for logical rects / focus) and `render_carousel`
/// (for smooth-scroll rendering) so the two paths cannot diverge.
pub(crate) fn carousel_visible_entries(
    extents: &[u16],
    scroll: u16,
    main_extent: u16,
) -> (u16, Vec<(usize, u32, u16)>) {
    let total: u32 = extents.iter().map(|&e| e as u32).sum();
    // max_scroll: clamp so the content tail is always flush with the viewport end.
    let max_scroll = total.saturating_sub(main_extent as u32).min(u16::MAX as u32) as u16;
    let clamped = scroll.min(max_scroll);

    let vp_start = clamped as u32;
    let vp_end = vp_start + main_extent as u32;

    let mut entries: Vec<(usize, u32, u16)> = Vec::new();
    let mut v_start: u32 = 0;

    for (i, &ext) in extents.iter().enumerate() {
        let v_end = v_start + ext as u32;
        if v_end > vp_start && v_start < vp_end {
            entries.push((i, v_start, ext));
        }
        v_start = v_end;
        if v_start >= vp_end {
            break; // no further children can intersect the viewport
        }
    }

    (clamped, entries)
}

// ── Solver ───────────────────────────────────────────────────────────────────

/// Solve the layout tree rooted at `node` within `area`.
///
/// Returns a flat list of `(TileId, Rect)` pairs — one entry per **visible**
/// leaf tile — in depth-first, left-to-right order.
///
/// The solver guarantees:
/// - Every returned `Rect` is contained within `area` (or its recursive
///   sub-area for nested splits/carousels).
/// - The children of a `Split` exactly tile their assigned area when at least
///   one `Fill` child has enough `max` headroom to absorb the leftover pool.
///   If every `Fill` at a level is pinned at its `max` before the pool is
///   exhausted, the residual is left unassigned — `max` wins over exact tiling
///   in this documented edge case.
/// - A `Split` whose children are all `Fixed` or `Percent` and whose sizes
///   sum to less than the available space leaves the trailing space unassigned
///   (no implicit stretch).  This is intended.
/// - If space runs out, remaining children receive zero-sized rects; no panic
///   occurs.  `min` is best-effort when the area cannot fit every child's
///   minimum.
/// - For a `Carousel`, `scroll` is clamped in place so that the last child's
///   tail is always flush with the viewport end (no blank gap past the content).
///   Only children that intersect the viewport produce rects; partial tiles at
///   either edge are clipped to the viewport boundary.
///
/// `node` is taken as `&mut` so that `Orientation::Adaptive` can store the
/// chosen axis in `last` for hysteresis on subsequent solves, and so that
/// `Carousel::scroll` can be clamped in place.
pub fn solve(node: &mut Node, area: Rect) -> Vec<(TileId, Rect)> {
    let mut out = Vec::new();
    solve_into(node, area, &mut out);
    out
}

/// Recursive helper that appends `(TileId, Rect)` pairs to `out`.
///
/// For `Split`, partitions the area and recurses into every child.
/// For `Carousel`, only the children that intersect the current viewport
/// produce rects; `scroll` is clamped in place before the pass.
pub(crate) fn solve_into(node: &mut Node, area: Rect, out: &mut Vec<(TileId, Rect)>) {
    match node {
        Node::Tile(id) => {
            out.push((*id, area));
        }
        Node::Split { orientation, children } => {
            if children.is_empty() {
                return;
            }

            let axis = orientation.resolve(area);
            // Total available space along the split axis.
            let total = match axis {
                Axis::Horizontal => area.width,
                Axis::Vertical => area.height,
            };

            let sizes = partition(children, total);

            // Walk children, advancing the origin along the axis.
            let mut pos = match axis {
                Axis::Horizontal => area.x,
                Axis::Vertical => area.y,
            };

            for ((_, child), size) in children.iter_mut().zip(sizes.iter()) {
                let child_area = match axis {
                    Axis::Horizontal => Rect::new(pos, area.y, *size, area.height),
                    Axis::Vertical => Rect::new(area.x, pos, area.width, *size),
                };
                solve_into(child, child_area, out);
                pos = pos.saturating_add(*size);
            }
        }
        Node::Carousel { orientation, scroll, children, .. } => {
            if children.is_empty() {
                return;
            }
            let axis = orientation.resolve(area);
            let main_extent = match axis {
                Axis::Horizontal => area.width,
                Axis::Vertical => area.height,
            };

            // Collect extents separately so the shared helper can run without
            // holding a borrow on `children` (needed for the mutable recurse below).
            let extents: Vec<u16> = children.iter().map(|(e, _)| *e).collect();
            let (clamped, entries) = carousel_visible_entries(&extents, *scroll, main_extent);
            *scroll = clamped;

            let vp_main_origin = match axis {
                Axis::Horizontal => area.x,
                Axis::Vertical => area.y,
            };

            for (child_idx, v_start, ext) in entries {
                // Clip the child's content span to the viewport.
                let vis_content_start = v_start.max(clamped as u32);
                let vis_content_end =
                    (v_start + ext as u32).min(clamped as u32 + main_extent as u32);
                let vis_len = (vis_content_end - vis_content_start) as u16;
                // Screen offset from viewport origin for the visible portion.
                let screen_start = vp_main_origin + (vis_content_start - clamped as u32) as u16;

                let child_area = match axis {
                    Axis::Horizontal => Rect::new(screen_start, area.y, vis_len, area.height),
                    Axis::Vertical => Rect::new(area.x, screen_start, area.width, vis_len),
                };
                solve_into(&mut children[child_idx].1, child_area, out);
            }
        }
    }
}

/// Partition `total` cells among `children` according to their constraints.
///
/// ## Algorithm
///
/// 1. **Fixed pass** — each `Fixed(n)` child receives `clamp(n, min, max)`.
/// 2. **Percent pass** — each `Percent(p)` child receives
///    `clamp(total*p/100, min, max)` (fraction of the *original* total).
/// 3. **Fill pass (water-filling)** — the pool (`total − Σ Fixed − Σ Percent`)
///    is shared among `Fill` children:
///    - Every Fill is seeded at its `min`.
///    - The leftover above those seeds is distributed proportionally by weight
///      among fills that still have headroom (`size < max`).  Fills whose
///      proportional share would push them past `max` are *pinned* at `max`
///      and removed from the active set; their un-awarded excess stays in the
///      pool for subsequent rounds.
///    - Rounds repeat until leftover is exhausted or all fills are pinned.
///    - After a round that pins nothing, the integer rounding remainder is
///      handed to the last fills backward in two passes: weighted fills
///      (`weight > 0`) first, then zero-weight fills as a fallback.  This
///      ensures `Fill(0)` children never absorb the remainder when any
///      weighted sibling still has headroom.
/// 4. **Over-constrained pass** — if the sum exceeds `total` (e.g. Fill mins
///    alone overflow the pool), children are trimmed from the end; `min` is
///    best-effort when the area cannot fit every minimum.
///
/// ## Guarantees
///
/// - Sums to `total` exactly when at least one Fill has enough `max` headroom.
/// - When every Fill is pinned at its `max` before the pool is exhausted, the
///   residual is left unassigned (`max` wins over exact tiling).
/// - A split with no Fill children leaves any trailing space unassigned.
/// - Trimming policy: greedy in declaration order; later children yield first;
///   sizes are never negative and never exceed `total`.
///
/// Returns a `Vec<u16>` of the same length as `children`.
pub(crate) fn partition(children: &[(Constraint, Node)], total: u16) -> Vec<u16> {
    let n = children.len();
    let mut sizes = vec![0u16; n];

    // Pass 1: Fixed children receive their requested size, clamped to [min, max].
    let mut used: u32 = 0;
    for (i, (c, _)) in children.iter().enumerate() {
        if let Size::Fixed(v) = c.size {
            let clamped = clamp(v, c.min, c.max);
            sizes[i] = clamped;
            used += clamped as u32;
        }
    }

    // Pass 2: Percent children take a fraction of the *original* total, clamped.
    for (i, (c, _)) in children.iter().enumerate() {
        if let Size::Percent(p) = c.size {
            let raw = (total as u32 * p.min(100) as u32 / 100) as u16;
            let clamped = clamp(raw, c.min, c.max);
            sizes[i] = clamped;
            used += clamped as u32;
        }
    }

    // Pass 3: Fill children divide the remaining pool via water-filling.
    // `pool` = cells left after Fixed and Percent children are satisfied.
    let pool = (total as u32).saturating_sub(used);

    // Collect indices of Fill children; all other slots are already settled.
    let fill_indices: Vec<usize> = children.iter().enumerate()
        .filter_map(|(i, (c, _))| if matches!(c.size, Size::Fill(_)) { Some(i) } else { None })
        .collect();

    if !fill_indices.is_empty() {
        // Step 3a: seed every Fill at its min, clamped to max to handle the
        // degenerate case where a caller supplies min > max.
        let mut seeded_sum: u32 = 0;
        for &i in &fill_indices {
            let seed = children[i].0.min.min(children[i].0.max);
            sizes[i] = seed;
            seeded_sum += seed as u32;
        }

        // leftover = cells above the seeds available to distribute.  Saturates
        // at 0 if the seeds already exceed the pool; pass 4 will handle that.
        let mut leftover = pool.saturating_sub(seeded_sum);

        // Water-filling loop: each round distributes `leftover` proportionally
        // among fills that still have headroom, pinning any that hit their max.
        loop {
            // Active set: fills that have room to grow above their current size.
            let active: Vec<usize> = fill_indices.iter()
                .copied()
                .filter(|&i| sizes[i] < children[i].0.max)
                .collect();

            if active.is_empty() || leftover == 0 {
                break;
            }

            // Total weight of active fills (0 when all remaining are Fill(0)).
            let active_weight: u32 = active.iter()
                .map(|&i| if let Size::Fill(w) = children[i].0.size { w as u32 } else { 0 })
                .sum();

            // Distribute proportional shares; pin any fill that would exceed max.
            let mut any_pinned = false;
            let mut round_assigned: u32 = 0;
            for &i in &active {
                let w = if let Size::Fill(w) = children[i].0.size { w as u32 } else { 0 };
                // Proportional share; 0 for weight-0 fills.  checked_div guards
                // against the all-zero-weight case (share becomes 0, handled
                // by the no-pin branch below which then distributes the full
                // leftover as "remainder" to the last fill).
                let share = (leftover * w).checked_div(active_weight).unwrap_or(0);
                let new_size = sizes[i] as u32 + share;
                if new_size >= children[i].0.max as u32 {
                    // Pin at max.  The un-awarded portion (share − headroom)
                    // stays in `leftover` for redistribution in the next round.
                    let headroom = children[i].0.max as u32 - sizes[i] as u32;
                    sizes[i] = children[i].0.max;
                    round_assigned += headroom;
                    any_pinned = true;
                } else {
                    sizes[i] = new_size as u16;
                    round_assigned += share;
                }
            }
            leftover -= round_assigned;

            if !any_pinned {
                // All proportional shares fit without pinning anyone.  Distribute
                // the integer rounding remainder back-to-front in two passes so
                // that Fill(0) children (weight 0) are never preferred over
                // siblings that carry a positive weight.
                //
                // Pass A: weighted fills (weight > 0), last to first.
                for &i in fill_indices.iter().rev() {
                    if leftover == 0 { break; }
                    let w = if let Size::Fill(w) = children[i].0.size { w } else { 0 };
                    if w == 0 { continue; } // skip zero-weight fills in this pass
                    let headroom = children[i].0.max as u32 - sizes[i] as u32;
                    let give = leftover.min(headroom);
                    sizes[i] = (sizes[i] as u32 + give) as u16;
                    leftover -= give;
                }
                // Pass B: zero-weight fills (fallback for the all-zero-weight
                // case, where every share was 0 and the full leftover remains).
                for &i in fill_indices.iter().rev() {
                    if leftover == 0 { break; }
                    let w = if let Size::Fill(w) = children[i].0.size { w } else { 0 };
                    if w > 0 { continue; } // already handled in pass A
                    let headroom = children[i].0.max as u32 - sizes[i] as u32;
                    let give = leftover.min(headroom);
                    sizes[i] = (sizes[i] as u32 + give) as u16;
                    leftover -= give;
                }
                break;
            }
            // At least one fill was pinned: loop to redistribute the excess
            // among the remaining active (non-pinned) fills.
        }
    }

    // Pass 4: if the sum exceeds `total` (over-constrained — Fill mins alone
    // overflow the pool), trim from the end so we never exceed the area.
    // Later children yield space first; min is best-effort when infeasible.
    let sum: u32 = sizes.iter().map(|&s| s as u32).sum();
    if sum > total as u32 {
        let mut excess = sum - total as u32;
        for s in sizes.iter_mut().rev() {
            if excess == 0 { break; }
            let trim = (*s as u32).min(excess);
            *s -= trim as u16;
            excess -= trim;
        }
    }

    sizes
}

/// Clamp `v` to `[lo, hi]`, saturating on `u16` overflow.
#[inline]
fn clamp(v: u16, lo: u16, hi: u16) -> u16 {
    v.max(lo).min(hi)
}

// ── Minimum size ─────────────────────────────────────────────────────────────

/// Compute the minimum `(width, height)` that `node` needs to render without
/// any child being squeezed to zero.
///
/// Rules:
/// - `Tile` → `(1, 1)` (a tile always needs at least one cell).
/// - `Split` along the horizontal axis → width is the *sum* of children's
///   minimum widths, height is the *max* of children's minimum heights.
/// - `Split` along the vertical axis → height is the *sum* of children's
///   minimum heights, width is the *max* of children's minimum widths.
/// - `Split Adaptive` → the max of both orientations' minima (because we do
///   not know which axis will be chosen at solve-time).
/// - `Carousel` → main axis is `0` (it scrolls, so zero cells is valid);
///   cross axis is the *max* of children's cross-axis minimums.  `Adaptive`
///   carousels take the conservative maximum of both orientations' minimums.
///
/// The returned values saturate at `u16::MAX`.
pub fn min_size(node: &Node) -> (u16, u16) {
    match node {
        Node::Tile(_) => (1, 1),
        Node::Split { orientation, children } => {
            if children.is_empty() {
                return (0, 0);
            }
            match orientation {
                Orientation::Horizontal => {
                    min_size_along(children, Axis::Horizontal)
                }
                Orientation::Vertical => {
                    min_size_along(children, Axis::Vertical)
                }
                Orientation::Adaptive { .. } => {
                    // Conservative: must fit either axis.
                    let (wh, hh) = min_size_along(children, Axis::Horizontal);
                    let (wv, hv) = min_size_along(children, Axis::Vertical);
                    (wh.max(wv), hh.max(hv))
                }
            }
        }
        Node::Carousel { orientation, children, .. } => {
            if children.is_empty() {
                return (0, 0);
            }
            match orientation {
                // main = 0 (scrolls); cross = max child cross-min.
                Orientation::Horizontal => {
                    let cross = children.iter()
                        .map(|(_, c)| min_size(c).1)
                        .fold(0u16, u16::max);
                    (0, cross)
                }
                Orientation::Vertical => {
                    let cross = children.iter()
                        .map(|(_, c)| min_size(c).0)
                        .fold(0u16, u16::max);
                    (cross, 0)
                }
                Orientation::Adaptive { .. } => {
                    // Conservative: could run either axis, so bound both.
                    let w = children.iter()
                        .map(|(_, c)| min_size(c).0)
                        .fold(0u16, u16::max);
                    let h = children.iter()
                        .map(|(_, c)| min_size(c).1)
                        .fold(0u16, u16::max);
                    (w, h)
                }
            }
        }
    }
}

/// Compute the minimum size for a `Split` on a known `axis`.
///
/// Along the split axis the sizes are summed (each child needs its share).
/// Across the axis the sizes are maxed (the narrowest viable cross-section
/// must accommodate every child).
fn min_size_along(children: &[(Constraint, Node)], axis: Axis) -> (u16, u16) {
    let mut main_sum: u32 = 0;
    let mut cross_max: u16 = 0;

    for (c, child) in children {
        let (cw, ch) = min_size(child);
        // The minimum along the main axis is the larger of the child's natural
        // minimum and the constraint's own `min` field.
        let (main, cross) = match axis {
            Axis::Horizontal => (cw.max(c.min), ch),
            Axis::Vertical => (ch.max(c.min), cw),
        };
        main_sum = main_sum.saturating_add(main as u32);
        cross_max = cross_max.max(cross);
    }

    let main = main_sum.min(u16::MAX as u32) as u16;
    match axis {
        Axis::Horizontal => (main, cross_max),
        Axis::Vertical => (cross_max, main),
    }
}

// ── region_of ────────────────────────────────────────────────────────────────

/// The rect allotted to the node with `id` when `root` is laid out in `area`.
///
/// Performs the same descent as [`solve`] but stops and returns the rect as
/// soon as it reaches the target node.
///
/// - For a [`Node::Tile`], returns the exact rect `solve` would assign it.
/// - For a [`Node::Carousel`], returns its **viewport rect** (the area before
///   virtualization) — pass this directly to `render_carousel`.
/// - Returns `None` if `id` is not found, or if the tile is inside a carousel
///   but currently scrolled out of view.
///
/// ## Composition pattern
///
/// ```rust
/// # use mullion::{Node, Rect, layout::{region_of, solve}};
/// # use mullion::border::render_shared;
/// # fn demo(mut root: Node, carousel_id: u64, area: Rect) {
/// // 1. Solve and render the split skeleton.
/// let rects = solve(&mut root, area);
/// // 2. Find the carousel's viewport rect.
/// if let Some(carousel_rect) = region_of(&mut root, area, carousel_id) {
///     // 3. Feed it to render_carousel.
///     // render_carousel(buf, &mut root, carousel_rect, &mut |buf, id, rect| { ... });
/// }
/// # }
/// ```
pub fn region_of(root: &mut Node, area: Rect, id: TileId) -> Option<Rect> {
    region_of_impl(root, area, id)
}

/// Recursive descent for [`region_of`].
///
/// Mirrors `solve_into` in structure: the same partition / carousel-viewport
/// logic is used so the returned rect is always consistent with `solve` output.
fn region_of_impl(node: &mut Node, area: Rect, id: TileId) -> Option<Rect> {
    match node {
        Node::Tile(tid) => {
            if *tid == id { Some(area) } else { None }
        }

        Node::Split { orientation, children } => {
            if children.is_empty() {
                return None;
            }
            let axis = orientation.resolve(area);
            let total = match axis {
                Axis::Horizontal => area.width,
                Axis::Vertical => area.height,
            };
            let sizes = partition(children, total);
            let mut pos = match axis {
                Axis::Horizontal => area.x,
                Axis::Vertical => area.y,
            };
            for ((_, child), &size) in children.iter_mut().zip(sizes.iter()) {
                let child_area = match axis {
                    Axis::Horizontal => Rect::new(pos, area.y, size, area.height),
                    Axis::Vertical   => Rect::new(area.x, pos, area.width, size),
                };
                if let Some(rect) = region_of_impl(child, child_area, id) {
                    return Some(rect);
                }
                pos = pos.saturating_add(size);
            }
            None
        }

        Node::Carousel { id: carousel_id, orientation, scroll, children, .. } => {
            // A Carousel's viewport rect is its full assigned area — hand it
            // directly to render_carousel.
            if *carousel_id == id {
                return Some(area);
            }
            if children.is_empty() {
                return None;
            }
            let axis = orientation.resolve(area);
            let main_extent = match axis {
                Axis::Horizontal => area.width,
                Axis::Vertical => area.height,
            };
            let extents: Vec<u16> = children.iter().map(|(e, _)| *e).collect();
            let (clamped, entries) = carousel_visible_entries(&extents, *scroll, main_extent);
            *scroll = clamped;

            let vp_main_origin = match axis {
                Axis::Horizontal => area.x,
                Axis::Vertical => area.y,
            };
            for (child_idx, v_start, ext) in entries {
                // Compute the on-screen clipped rect for this child, matching
                // solve_into exactly so region_of and solve agree on positions.
                let vis_start = v_start.max(clamped as u32);
                let vis_end   = (v_start + ext as u32).min(clamped as u32 + main_extent as u32);
                let vis_len   = (vis_end - vis_start) as u16;
                let screen_start = vp_main_origin + (vis_start - clamped as u32) as u16;
                let child_area = match axis {
                    Axis::Horizontal => Rect::new(screen_start, area.y, vis_len, area.height),
                    Axis::Vertical   => Rect::new(area.x, screen_start, area.width, vis_len),
                };
                if let Some(rect) = region_of_impl(&mut children[child_idx].1, child_area, id) {
                    return Some(rect);
                }
            }
            None
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Hand-written example tests ────────────────────────────────────────

    #[test]
    fn single_tile_gets_full_area() {
        let area = Rect::new(0, 0, 80, 24);
        let mut node = Node::Tile(1);
        let tiles = solve(&mut node, area);
        assert_eq!(tiles, vec![(1, area)]);
    }

    #[test]
    fn two_equal_fill_horizontal() {
        let area = Rect::new(0, 0, 80, 24);
        let mut node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let tiles = solve(&mut node, area);
        // Last Fill child absorbs remainder (80 is even, so both get 40).
        assert_eq!(tiles[0].1.width, 40);
        assert_eq!(tiles[1].1.width, 40);
        // x positions tile correctly.
        assert_eq!(tiles[0].1.x, 0);
        assert_eq!(tiles[1].1.x, 40);
    }

    #[test]
    fn fixed_percent_fill_partition() {
        // 80 cols: Fixed(10) + Percent(25) + Fill(1)
        // Fixed = 10, Percent = 80*25/100 = 20, Fill = 80-10-20 = 50
        let area = Rect::new(0, 0, 80, 24);
        let mut node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fixed(10)), Node::Tile(1)),
                (Constraint::new(Size::Percent(25)), Node::Tile(2)),
                (Constraint::new(Size::Fill(1)), Node::Tile(3)),
            ],
        };
        let tiles = solve(&mut node, area);
        assert_eq!(tiles[0].1.width, 10);
        assert_eq!(tiles[1].1.width, 20);
        assert_eq!(tiles[2].1.width, 50);
        // Widths sum to total.
        let sum: u16 = tiles.iter().map(|(_, r)| r.width).sum();
        assert_eq!(sum, 80);
    }

    #[test]
    fn vertical_split_stacks_rows() {
        let area = Rect::new(0, 0, 80, 24);
        let mut node = Node::Split {
            orientation: Orientation::Vertical,
            children: vec![
                (Constraint::new(Size::Fixed(3)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let tiles = solve(&mut node, area);
        assert_eq!(tiles[0].1.y, 0);
        assert_eq!(tiles[0].1.height, 3);
        assert_eq!(tiles[1].1.y, 3);
        assert_eq!(tiles[1].1.height, 21);
    }

    #[test]
    fn nested_split() {
        // Outer: H split into left=40 and right=40.
        // Right: V split into top=12 and bottom=12.
        let area = Rect::new(0, 0, 80, 24);
        let mut node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Split {
                    orientation: Orientation::Vertical,
                    children: vec![
                        (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                        (Constraint::new(Size::Fill(1)), Node::Tile(3)),
                    ],
                }),
            ],
        };
        let tiles = solve(&mut node, area);
        assert_eq!(tiles.len(), 3);
        // Left panel
        assert_eq!(tiles[0].1, Rect::new(0, 0, 40, 24));
        // Top-right panel
        assert_eq!(tiles[1].1, Rect::new(40, 0, 40, 12));
        // Bottom-right panel
        assert_eq!(tiles[2].1, Rect::new(40, 12, 40, 12));
    }

    #[test]
    fn min_max_clamps_respected() {
        // Percent(50) of 80 = 40, but max=30 → clamped to 30.
        // Fill(1) gets the remaining 50.
        let area = Rect::new(0, 0, 80, 24);
        let mut node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint { size: Size::Percent(50), min: 0, max: 30 }, Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let tiles = solve(&mut node, area);
        assert_eq!(tiles[0].1.width, 30);
        assert_eq!(tiles[1].1.width, 50);
    }

    #[test]
    fn odd_width_remainder_goes_to_last_fill() {
        // 81 cols split between two Fill(1): first gets 40, last gets 41.
        let area = Rect::new(0, 0, 81, 24);
        let mut node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let tiles = solve(&mut node, area);
        let sum: u16 = tiles.iter().map(|(_, r)| r.width).sum();
        assert_eq!(sum, 81, "widths must sum to total");
    }

    #[test]
    fn adaptive_picks_horizontal_for_wide_area() {
        let area = Rect::new(0, 0, 200, 24);
        let mut node = Node::Split {
            orientation: Orientation::Adaptive { margin_pct: 10, last: None },
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let tiles = solve(&mut node, area);
        // Horizontal split → tiles are side by side → same y, same height.
        assert_eq!(tiles[0].1.y, tiles[1].1.y);
        assert_eq!(tiles[0].1.height, tiles[1].1.height);
    }

    #[test]
    fn adaptive_picks_vertical_for_tall_area() {
        let area = Rect::new(0, 0, 20, 200);
        let mut node = Node::Split {
            orientation: Orientation::Adaptive { margin_pct: 10, last: None },
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let tiles = solve(&mut node, area);
        // Vertical split → tiles are stacked → same x, same width.
        assert_eq!(tiles[0].1.x, tiles[1].1.x);
        assert_eq!(tiles[0].1.width, tiles[1].1.width);
    }

    #[test]
    fn min_size_tile() {
        assert_eq!(min_size(&Node::Tile(0)), (1, 1));
    }

    #[test]
    fn min_size_horizontal_split() {
        let node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        // Two tiles → min width = 1+1 = 2, min height = max(1,1) = 1.
        assert_eq!(min_size(&node), (2, 1));
    }

    #[test]
    fn min_size_vertical_split() {
        let node = Node::Split {
            orientation: Orientation::Vertical,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                (Constraint::new(Size::Fill(1)), Node::Tile(3)),
            ],
        };
        // Three tiles → min height = 3, min width = 1.
        assert_eq!(min_size(&node), (1, 3));
    }

    #[test]
    fn empty_split_returns_nothing() {
        let area = Rect::new(0, 0, 80, 24);
        let mut node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![],
        };
        let tiles = solve(&mut node, area);
        assert!(tiles.is_empty());
    }

    #[test]
    fn zero_area_does_not_panic() {
        let area = Rect::new(0, 0, 0, 0);
        let mut node = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let tiles = solve(&mut node, area);
        // Must not panic; tiles exist but are zero-sized.
        assert_eq!(tiles.len(), 2);
        for (_, r) in &tiles {
            assert_eq!(r.width, 0);
        }
    }

    // ── Carousel tests ────────────────────────────────────────────────────

    fn carousel_10x3(scroll: u16) -> Node {
        Node::Carousel {
            id: 99,
            orientation: Orientation::Horizontal,
            scroll,
            children: (0u64..10).map(|i| (3u16, Node::Tile(i))).collect(),
        }
    }

    #[test]
    fn carousel_virtualization_only_visible_tiles() {
        // 10 tiles × 3 cells each = 30 total; viewport = 10 cells.
        // At scroll=0: tiles 0,1,2 fully in [0,10); tile 3 clips at [9,10).
        let area = Rect::new(0, 0, 10, 5);
        let tiles = solve(&mut carousel_10x3(0), area);
        assert_eq!(tiles.len(), 4, "only 4 tiles intersect [0,10)");
        assert_eq!(tiles[0], (0, Rect::new(0, 0, 3, 5)));
        assert_eq!(tiles[1], (1, Rect::new(3, 0, 3, 5)));
        assert_eq!(tiles[2], (2, Rect::new(6, 0, 3, 5)));
        // Tile 3 occupies [9,12) but only [9,10) is visible → clipped to 1 cell.
        assert_eq!(tiles[3], (3, Rect::new(9, 0, 1, 5)));
    }

    #[test]
    fn carousel_clipped_tile_at_edge() {
        let area = Rect::new(0, 0, 10, 5);
        let tiles = solve(&mut carousel_10x3(0), area);
        // The straddling tile at the right edge is 1 cell wide (10-9=1).
        let edge = tiles.last().unwrap();
        assert_eq!(edge.1.width, 1);
        assert_eq!(edge.1.right(), area.right());
    }

    #[test]
    fn carousel_visible_rects_contiguous_and_in_order() {
        let area = Rect::new(0, 0, 10, 5);
        let tiles = solve(&mut carousel_10x3(0), area);
        for w in tiles.windows(2) {
            assert_eq!(w[0].1.right(), w[1].1.x, "rects must be contiguous");
        }
        // All rects within the viewport.
        for (_, r) in &tiles {
            assert!(r.x >= area.x && r.right() <= area.right());
        }
    }

    #[test]
    fn carousel_scroll_shows_later_tiles() {
        // scroll=6: first child screen pos = 0-6=-6
        // tiles 0→[-6,-3), 1→[-3,0) both invisible.
        // tile 2→[0,3), 3→[3,6), 4→[6,9) fully visible; tile 5→[9,12) clipped.
        let area = Rect::new(0, 0, 10, 5);
        let tiles = solve(&mut carousel_10x3(6), area);
        assert_eq!(tiles.len(), 4);
        assert_eq!(tiles[0].0, 2);
        assert_eq!(tiles[1].0, 3);
        assert_eq!(tiles[2].0, 4);
        assert_eq!(tiles[3].0, 5);
    }

    #[test]
    fn carousel_scroll_clamped_to_max() {
        // total=30, viewport=10, max_scroll=20.  scroll=9999 → clamped to 20.
        let area = Rect::new(0, 0, 10, 5);
        let mut node = carousel_10x3(9999);
        let tiles = solve(&mut node, area);
        if let Node::Carousel { scroll, .. } = &node {
            assert_eq!(*scroll, 20, "scroll must be clamped to max_scroll=20");
        }
        // The last visible tile must end exactly at the viewport's right edge.
        let last_right = tiles.last().unwrap().1.right();
        assert_eq!(last_right, area.right(), "content must be flush at the end");
    }

    #[test]
    fn carousel_vertical_scroll() {
        // Vertical carousel: 5 tiles × 4 rows each; viewport height=10.
        let area = Rect::new(0, 0, 20, 10);
        let mut node = Node::Carousel {
            id: 0,
            orientation: Orientation::Vertical,
            scroll: 4,
            children: (0u64..5).map(|i| (4u16, Node::Tile(i))).collect(),
        };
        let tiles = solve(&mut node, area);
        // scroll=4: tile 0→[-4,0) invisible; tile 1→[0,4); tile 2→[4,8); tile 3→[8,12) clips.
        assert_eq!(tiles[0].0, 1);
        assert_eq!(tiles[0].1, Rect::new(0, 0, 20, 4));
        // tile 3 clips to [8,10) → height=2
        assert_eq!(tiles.last().unwrap().0, 3);
        assert_eq!(tiles.last().unwrap().1.height, 2);
    }

    #[test]
    fn carousel_adaptive_horizontal_for_wide() {
        let area = Rect::new(0, 0, 100, 10);
        let mut node = Node::Carousel {
            id: 0,
            orientation: Orientation::Adaptive { margin_pct: 0, last: None },
            scroll: 0,
            children: vec![(50u16, Node::Tile(0)), (50u16, Node::Tile(1))],
        };
        let tiles = solve(&mut node, area);
        // Wide area → Horizontal → tiles side by side (same y and height).
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].1.y, tiles[1].1.y);
        assert_eq!(tiles[0].1.height, tiles[1].1.height);
    }

    #[test]
    fn carousel_adaptive_vertical_for_tall() {
        let area = Rect::new(0, 0, 10, 100);
        let mut node = Node::Carousel {
            id: 0,
            orientation: Orientation::Adaptive { margin_pct: 0, last: None },
            scroll: 0,
            children: vec![(50u16, Node::Tile(0)), (50u16, Node::Tile(1))],
        };
        let tiles = solve(&mut node, area);
        // Tall area → Vertical → tiles stacked (same x and width).
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].1.x, tiles[1].1.x);
        assert_eq!(tiles[0].1.width, tiles[1].1.width);
    }

    #[test]
    fn carousel_min_size_horizontal() {
        let node = Node::Carousel {
            id: 0,
            orientation: Orientation::Horizontal,
            scroll: 0,
            // Children with min heights of 3 and 5.
            children: vec![
                (10, Node::Tile(0)), // min_size=(1,1)
                (10, Node::Split {
                    orientation: Orientation::Vertical,
                    children: vec![
                        (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                        (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                        (Constraint::new(Size::Fill(1)), Node::Tile(3)),
                    ],
                }),
            ],
        };
        // Horizontal carousel: main=0, cross(height)=max(1,3)=3.
        assert_eq!(min_size(&node), (0, 3));
    }

    #[test]
    fn carousel_min_size_vertical() {
        let node = Node::Carousel {
            id: 0,
            orientation: Orientation::Vertical,
            scroll: 0,
            children: vec![
                (10, Node::Tile(0)),
                (10, Node::Split {
                    orientation: Orientation::Horizontal,
                    children: vec![
                        (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                        (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                    ],
                }),
            ],
        };
        // Vertical carousel: main=0, cross(width)=max(1,2)=2.
        assert_eq!(min_size(&node), (2, 0));
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// For any Horizontal split with only Fill children, widths sum to total.
        #[test]
        fn prop_fill_widths_sum_to_total(total in 0u16..=500u16) {
            let area = Rect::new(0, 0, total, 24);
            let mut node = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(3)),
                ],
            };
            let tiles = solve(&mut node, area);
            let sum: u16 = tiles.iter().map(|(_, r)| r.width).sum();
            prop_assert_eq!(sum, total);
        }

        /// For any Vertical split with only Fill children, heights sum to total.
        #[test]
        fn prop_fill_heights_sum_to_total(total in 0u16..=300u16) {
            let area = Rect::new(0, 0, 80, total);
            let mut node = Node::Split {
                orientation: Orientation::Vertical,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                ],
            };
            let tiles = solve(&mut node, area);
            let sum: u16 = tiles.iter().map(|(_, r)| r.height).sum();
            prop_assert_eq!(sum, total);
        }

        /// All returned rects must fit within the root area (x+width <= area.right).
        #[test]
        fn prop_rects_contained_in_area(
            w in 1u16..=200u16,
            h in 1u16..=100u16,
        ) {
            let area = Rect::new(0, 0, w, h);
            let mut node = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                    (Constraint::new(Size::Fill(2)), Node::Tile(2)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(3)),
                ],
            };
            let tiles = solve(&mut node, area);
            for (_, r) in tiles {
                prop_assert!(r.x >= area.x);
                prop_assert!(r.y >= area.y);
                prop_assert!(r.right() <= area.right(),
                    "r.right()={} > area.right()={}", r.right(), area.right());
                prop_assert!(r.bottom() <= area.bottom(),
                    "r.bottom()={} > area.bottom()={}", r.bottom(), area.bottom());
            }
        }

        /// Cross-axis dimension must equal the area's cross dimension for Fill-only splits.
        #[test]
        fn prop_cross_axis_unchanged(w in 1u16..=200u16, h in 1u16..=100u16) {
            let area = Rect::new(0, 0, w, h);
            let mut node = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                ],
            };
            let tiles = solve(&mut node, area);
            for (_, r) in tiles {
                prop_assert_eq!(r.height, h,
                    "height should equal area height for H split");
                prop_assert_eq!(r.y, area.y,
                    "y should equal area.y for H split");
            }
        }

        /// Fixed children must respect their exact size (when space allows).
        #[test]
        fn prop_fixed_child_exact_size(
            fixed in 0u16..=50u16,
            total in 0u16..=200u16,
        ) {
            let area = Rect::new(0, 0, total, 24);
            let mut node = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fixed(fixed)), Node::Tile(1)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                ],
            };
            let tiles = solve(&mut node, area);
            let actual_fixed = tiles[0].1.width;
            if total >= fixed {
                prop_assert_eq!(actual_fixed, fixed,
                    "fixed child should get exactly {} when total={}", fixed, total);
            } else {
                // Over-constrained: fixed child gets at most total.
                prop_assert!(actual_fixed <= total);
            }
        }

        /// Percent children must not exceed their stated percentage of total.
        #[test]
        fn prop_percent_child_at_most_pct(
            pct in 0u16..=100u16,
            total in 0u16..=200u16,
        ) {
            let area = Rect::new(0, 0, total, 24);
            let mut node = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Percent(pct)), Node::Tile(1)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                ],
            };
            let tiles = solve(&mut node, area);
            let expected_max = (total as u32 * pct as u32 / 100) as u16;
            prop_assert!(tiles[0].1.width <= expected_max + 1, // +1 for rounding
                "percent child width {} > expected_max {} for pct={} total={}",
                tiles[0].1.width, expected_max, pct, total);
        }

        /// `min_size` for a two-child horizontal split is always >= (2, 1).
        #[test]
        fn prop_min_size_horizontal_at_least_num_children(n in 1usize..=8usize) {
            let children: Vec<_> = (0..n as u64)
                .map(|id| (Constraint::new(Size::Fill(1)), Node::Tile(id)))
                .collect();
            let node = Node::Split {
                orientation: Orientation::Horizontal,
                children,
            };
            let (w, h) = min_size(&node);
            prop_assert!(w >= n as u16,
                "min width {w} < n={n} for horizontal split");
            prop_assert!(h >= 1);
        }

        /// `min_size` for a vertical split is always >= (1, n).
        #[test]
        fn prop_min_size_vertical_at_least_num_children(n in 1usize..=8usize) {
            let children: Vec<_> = (0..n as u64)
                .map(|id| (Constraint::new(Size::Fill(1)), Node::Tile(id)))
                .collect();
            let node = Node::Split {
                orientation: Orientation::Vertical,
                children,
            };
            let (w, h) = min_size(&node);
            prop_assert!(h >= n as u16,
                "min height {h} < n={n} for vertical split");
            prop_assert!(w >= 1);
        }

        /// When combined Fill max headroom covers the pool, children sum to total;
        /// in all cases each child's width must not exceed its max.
        #[test]
        fn prop_tiling_exact_when_feasible(
            total in 0u16..=200u16,
            cap_a in 0u16..=200u16,
            cap_b in 0u16..=200u16,
        ) {
            let area = Rect::new(0, 0, total, 1);
            let mut node = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)).with_max(cap_a), Node::Tile(0)),
                    (Constraint::new(Size::Fill(1)).with_max(cap_b), Node::Tile(1)),
                ],
            };
            let tiles = solve(&mut node, area);
            let w0 = tiles[0].1.width;
            let w1 = tiles[1].1.width;
            // max must always be respected.
            prop_assert!(w0 <= cap_a, "w0={} > cap_a={}", w0, cap_a);
            prop_assert!(w1 <= cap_b, "w1={} > cap_b={}", w1, cap_b);
            // When combined caps can cover total, widths must sum exactly to total.
            if (cap_a as u32) + (cap_b as u32) >= total as u32 {
                prop_assert_eq!(w0 + w1, total,
                    "gap: w0={} w1={} cap_a={} cap_b={} total={}", w0, w1, cap_a, cap_b, total);
            } else {
                // Both fills pinned at max before pool exhausted — sum ≤ total.
                prop_assert!(w0 + w1 <= total);
            }
        }

        /// A Carousel's visible rects are in ascending order, non-overlapping,
        /// and fully contained within the viewport; off-screen children never appear.
        #[test]
        fn prop_carousel_visible_rects_invariants(
            scroll in 0u16..=100u16,
            viewport in 1u16..=50u16,
            num_children in 0usize..=15usize,
        ) {
            let area = Rect::new(0, 0, viewport, 5);
            let mut node = Node::Carousel {
                id: 0,
                orientation: Orientation::Horizontal,
                scroll,
                children: (0..num_children as u64)
                    .map(|i| (4u16, Node::Tile(i)))
                    .collect(),
            };
            let tiles = solve(&mut node, area);

            for (_, r) in &tiles {
                prop_assert!(r.x >= area.x,
                    "rect.x={} < area.x={}", r.x, area.x);
                prop_assert!(r.right() <= area.right(),
                    "rect.right={} > area.right={}", r.right(), area.right());
            }
            // Adjacent rects must be strictly ordered and non-overlapping.
            for w in tiles.windows(2) {
                prop_assert!(w[0].0 < w[1].0, "tile ids out of child order");
                prop_assert!(w[0].1.right() <= w[1].1.x,
                    "rects overlap: {:?} and {:?}", w[0].1, w[1].1);
            }
        }

        /// Solving the same freshly-constructed tree twice with the same area
        /// must produce identical results (solver is deterministic).
        #[test]
        fn prop_solve_is_deterministic(w in 1u16..=200u16, h in 1u16..=100u16) {
            let area = Rect::new(0, 0, w, h);
            let make = || Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)).with_max(60), Node::Tile(0)),
                    (Constraint::new(Size::Fill(2)), Node::Tile(1)),
                    (Constraint::new(Size::Fill(1)).with_min(5), Node::Tile(2)),
                ],
            };
            let tiles1 = solve(&mut make(), area);
            let tiles2 = solve(&mut make(), area);
            prop_assert_eq!(tiles1, tiles2, "solve must be deterministic");
        }

        /// For arbitrary Fill constraints (including binding min/max), every
        /// returned rect lies within the root area and widths never exceed total.
        #[test]
        fn prop_rects_within_area_under_clamps(
            total in 0u16..=200u16,
            min_a in 0u16..=50u16,
            min_b in 0u16..=50u16,
            extra_max in 0u16..=150u16,
        ) {
            let area = Rect::new(0, 0, total, 24);
            // Ensure max >= min to avoid degenerate constraints.
            let max_a = min_a.saturating_add(extra_max);
            let max_b = min_b.saturating_add(extra_max);
            let mut node = Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint { size: Size::Fill(1), min: min_a, max: max_a }, Node::Tile(0)),
                    (Constraint { size: Size::Fill(1), min: min_b, max: max_b }, Node::Tile(1)),
                ],
            };
            let tiles = solve(&mut node, area);
            for (_, r) in &tiles {
                prop_assert!(r.right() <= area.right(),
                    "r.right={} > area.right={}", r.right(), area.right());
                prop_assert!(r.bottom() <= area.bottom(),
                    "r.bottom={} > area.bottom={}", r.bottom(), area.bottom());
            }
            let sum: u16 = tiles.iter().map(|(_, r)| r.width).sum();
            prop_assert!(sum <= total, "sum={} > total={}", sum, total);
        }
    }

    // ── region_of ─────────────────────────────────────────────────────────

    #[test]
    fn region_of_tile_matches_solve() {
        // A simple 2-tile horizontal split in a 40×10 area.
        // solve gives each tile 20 columns.
        let area = Rect::new(0, 0, 40, 10);
        let mut root = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Tile(2)),
            ],
        };
        let rects = solve(&mut root, area);
        let r1 = rects.iter().find(|&&(id, _)| id == 1).map(|&(_, r)| r).unwrap();
        let r2 = rects.iter().find(|&&(id, _)| id == 2).map(|&(_, r)| r).unwrap();

        let mut root2 = root.clone();
        assert_eq!(region_of(&mut root2, area, 1), Some(r1));

        let mut root3 = root.clone();
        assert_eq!(region_of(&mut root3, area, 2), Some(r2));
    }

    #[test]
    fn region_of_missing_id_returns_none() {
        let area = Rect::new(0, 0, 40, 10);
        let mut root = Node::Tile(1);
        assert_eq!(region_of(&mut root, area, 999), None);
    }

    #[test]
    fn region_of_carousel_returns_its_viewport_rect() {
        // Carousel occupies the right half of a horizontal split.
        let area = Rect::new(0, 0, 40, 10);
        let mut root = Node::Split {
            orientation: Orientation::Horizontal,
            children: vec![
                (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                (Constraint::new(Size::Fill(1)), Node::Carousel {
                    id: 42,
                    orientation: Orientation::Vertical,
                    scroll: 0,
                    children: vec![(5, Node::Tile(10)), (5, Node::Tile(11))],
                }),
            ],
        };
        // The carousel occupies columns 20–39 (right half of a 40-wide area).
        let expected = Rect::new(20, 0, 20, 10);
        assert_eq!(region_of(&mut root, area, 42), Some(expected));
    }

    #[test]
    fn region_of_tile_inside_carousel_matches_visible_clipped_rect() {
        // Vertical carousel 10 rows tall, two children of 6 rows each, scroll=0.
        // Child 0 (id=1): rows 0–5 (clamped to viewport 0–9 → rows 0–5, height 6).
        // Child 1 (id=2): rows 6–11 (clamped to viewport 0–9 → rows 6–9, height 4).
        let area = Rect::new(0, 0, 20, 10);
        let mut root = Node::Carousel {
            id: 99,
            orientation: Orientation::Vertical,
            scroll: 0,
            children: vec![(6, Node::Tile(1)), (6, Node::Tile(2))],
        };
        let rects = solve(&mut root, area);
        let r1 = rects.iter().find(|&&(id,_)| id==1).map(|&(_,r)| r).unwrap();
        let r2 = rects.iter().find(|&&(id,_)| id==2).map(|&(_,r)| r).unwrap();

        let mut root2 = root.clone();
        assert_eq!(region_of(&mut root2, area, 1), Some(r1));

        let mut root3 = root.clone();
        assert_eq!(region_of(&mut root3, area, 2), Some(r2));
    }

    #[test]
    fn region_of_scrolled_out_tile_returns_none() {
        // Carousel with scroll past child 0 so child 0 is invisible.
        // Two children of 6 rows each in a 6-row viewport.
        // scroll=6 → child 1 fills the viewport, child 0 is scrolled off.
        let area = Rect::new(0, 0, 20, 6);
        let mut root = Node::Carousel {
            id: 99,
            orientation: Orientation::Vertical,
            scroll: 6,
            children: vec![(6, Node::Tile(1)), (6, Node::Tile(2))],
        };
        // Child 0 (id=1) is fully scrolled off screen.
        assert_eq!(region_of(&mut root, area, 1), None);
        // Child 1 (id=2) is visible.
        assert!(region_of(&mut root, area, 2).is_some());
    }
}
