// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Border drawing: glyphs, [`draw_box`], per-tile framing ([`frame_tiles`]),
//! shared-border layout ([`render_shared`]), and rim animation
//! ([`render_rim`]/[`BorderGap`]).
//!
//! ## Glyph vocabulary
//!
//! Three line weights are supported, each with a horizontal line, a vertical
//! line, and four corner glyphs.  Rounded corners are only available for
//! `Light` weight; requesting them with `Heavy` or `Double` silently falls back
//! to square corners of the same weight.
//!
//! ```text
//! Light  (square):  ─ │   ┌ ┐ └ ┘
//! Light  (rounded): ─ │   ╭ ╮ ╰ ╯
//! Heavy:            ━ ┃   ┏ ┓ ┗ ┛
//! Double:           ═ ║   ╔ ╗ ╚ ╝
//! ```
//!
//! Tee/cross glyphs (`├ ┤ ┬ ┴ ┼` and mixed-weight variants) are produced by
//! shared-border mode ([`render_shared`]), where adjacent tiles meet on one
//! shared line and the junctions resolve automatically.
//!
//! ## Per-tile vs. shared-border mode
//!
//! [`frame_tiles`] is the **per-tile mode**: every tile gets its own box.
//! Adjacent tiles produce a doubled gutter (`┐┌` / `││` / `┘└`).
//! [`render_shared`] is the **shared-border mode**: adjacent tiles share a
//! single divider line and junction glyphs emerge automatically.
//!
//! To draw one box around a group of tiles, call [`draw_box`] on the group's
//! bounding rect directly.
//!
//! ## Note for Phase 3
//!
//! Phase 3 focus highlighting will want to draw a tile's box in a different
//! [`BorderStyle`] (e.g. heavier weight or accent colour) than its neighbours.
//! Because [`draw_box`] and [`frame_tiles`] already accept the style per call,
//! a focus pass can re-draw the focused tile's box over the base frame without
//! any API change.

use bitflags::bitflags;

use crate::{
    buffer::Buffer,
    geometry::Rect,
    junction::{EdgeGrid, resolve as resolve_junction},
    layout::{Axis, Node, TileId, partition},
    style::Style,
};

// ── LineWeight ────────────────────────────────────────────────────────────────

/// Thickness of the lines used in a border.
///
/// Rounded corners are only available for [`Light`](LineWeight::Light).  When
/// [`CornerStyle::Rounded`] is paired with [`Heavy`](LineWeight::Heavy) or
/// [`Double`](LineWeight::Double), the corner glyphs fall back to the square
/// variant of the same weight.  Line glyphs are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineWeight {
    /// Single thin lines (`─` `│`).
    Light,
    /// Single thick lines (`━` `┃`).
    Heavy,
    /// Double thin lines (`═` `║`).
    Double,
}

// ── CornerStyle ───────────────────────────────────────────────────────────────

/// Whether to use curved or right-angle corners.
///
/// Only honoured for [`LineWeight::Light`].  Other weights fall back to square
/// corners of the same weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CornerStyle {
    /// Right-angle corners (`┌ ┐ └ ┘` / `┏ ┓ ┗ ┛` / `╔ ╗ ╚ ╝`).
    Square,
    /// Curved corners (`╭ ╮ ╰ ╯`).  Falls back to `Square` for non-`Light` weights.
    Rounded,
}

// ── Borders ───────────────────────────────────────────────────────────────────

bitflags! {
    /// Which sides of a box to draw.
    ///
    /// Combine flags with `|` — e.g. `Borders::TOP | Borders::LEFT`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Borders: u8 {
        const TOP    = 0b0001;
        const BOTTOM = 0b0010;
        const LEFT   = 0b0100;
        const RIGHT  = 0b1000;
        /// Shorthand for all four sides.
        const ALL    = Self::TOP.bits() | Self::BOTTOM.bits()
                     | Self::LEFT.bits() | Self::RIGHT.bits();
    }
}

// ── BorderStyle ───────────────────────────────────────────────────────────────

/// Combined description of how a border looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderStyle {
    /// Thickness of the lines.
    pub weight: LineWeight,
    /// Square or rounded corners.
    pub corners: CornerStyle,
    /// Colour and text attributes applied to every border glyph.
    pub style: Style,
}

// ── Glyph lookup ──────────────────────────────────────────────────────────────

/// Return the `(h_line, v_line, top_left, top_right, bot_left, bot_right)` glyph
/// set for the given weight and corner style.
///
/// `Rounded` corners are silently coerced to square for `Heavy` and `Double`
/// weight, because no curved variants exist for those weights.
pub(crate) fn border_glyphs(
    weight: &LineWeight,
    corners: &CornerStyle,
) -> (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str) {
    match (weight, corners) {
        // Light + square corners
        (LineWeight::Light, CornerStyle::Square)  => ("─", "│", "┌", "┐", "└", "┘"),
        // Light + rounded corners (╭ ╮ ╰ ╯ are only defined for light weight)
        (LineWeight::Light, CornerStyle::Rounded) => ("─", "│", "╭", "╮", "╰", "╯"),
        // Heavy: Rounded falls back to square heavy (no curved heavy corners exist)
        (LineWeight::Heavy, _)                    => ("━", "┃", "┏", "┓", "┗", "┛"),
        // Double: Rounded falls back to square double (no curved double corners exist)
        (LineWeight::Double, _)                   => ("═", "║", "╔", "╗", "╚", "╝"),
    }
}

// ── draw_box ──────────────────────────────────────────────────────────────────

/// Draw the requested border sides of `area` into `buf` using `style`.
///
/// Only the border cells are written; the interior is left untouched.  All box
/// glyphs occupy exactly one terminal column.
///
/// ## Corner logic
///
/// At each corner cell the glyph depends on which of the two meeting sides are
/// requested:
/// - Both sides → corner glyph (`┌`, `┐`, `└`, `┘`, …).
/// - Horizontal side only → `h_line` (`─` / `━` / `═`).
/// - Vertical side only → `v_line` (`│` / `┃` / `║`).
/// - Neither → nothing written.
///
/// ## Degenerate areas
///
/// - `width == 0` or `height == 0`: nothing drawn.
/// - `width == 1`: only vertical glyphs are possible; a full-height `v_line`
///   is drawn if `LEFT` or `RIGHT` is set (corners are impossible).
/// - `height == 1`: only horizontal glyphs are possible; a full-width `h_line`
///   is drawn if `TOP` or `BOTTOM` is set.
/// - `area` is clipped to `buf.area` before drawing, so oversized rects are
///   safe.
pub fn draw_box(buf: &mut Buffer, area: Rect, borders: Borders, style: &BorderStyle) {
    // Clip to buffer; a zero-area intersection means nothing to draw.
    let area = buf.area.intersection(area);
    if area.width == 0 || area.height == 0 {
        return;
    }

    let (h_line, v_line, tl, tr, bl, br) = border_glyphs(&style.weight, &style.corners);
    let st = style.style;

    let top    = borders.contains(Borders::TOP);
    let bottom = borders.contains(Borders::BOTTOM);
    let left   = borders.contains(Borders::LEFT);
    let right  = borders.contains(Borders::RIGHT);

    // Inclusive column/row indices of the four edges.
    let x0 = area.x;
    let x1 = area.x + area.width - 1;  // equals x0 when width == 1
    let y0 = area.y;
    let y1 = area.y + area.height - 1; // equals y0 when height == 1

    // ── Degenerate: single column ────────────────────────────────────────────
    if area.width == 1 {
        // No horizontal span is available; draw only a vertical line.
        if left || right {
            for y in y0..=y1 {
                buf.set_grapheme(x0, y, v_line, st);
            }
        }
        return;
    }

    // ── Degenerate: single row ───────────────────────────────────────────────
    if area.height == 1 {
        // No vertical span is available; draw only a horizontal line.
        if top || bottom {
            for x in x0..=x1 {
                buf.set_grapheme(x, y0, h_line, st);
            }
        }
        return;
    }

    // ── General case: width >= 2 and height >= 2 ─────────────────────────────

    // Select the glyph for a corner cell given the two meeting sides.
    // `h_side` is the horizontal border (top/bottom), `v_side` the vertical
    // one (left/right).  Both present → corner; one → its line; neither → skip.
    macro_rules! corner_glyph {
        ($h_side:expr, $v_side:expr, $corner:expr) => {
            if $h_side && $v_side {
                Some($corner)
            } else if $h_side {
                Some(h_line)
            } else if $v_side {
                Some(v_line)
            } else {
                None
            }
        };
    }

    // Top-left corner
    if let Some(g) = corner_glyph!(top, left, tl) {
        buf.set_grapheme(x0, y0, g, st);
    }
    // Top row middle (columns between the two corners)
    if top {
        for x in (x0 + 1)..x1 {
            buf.set_grapheme(x, y0, h_line, st);
        }
    }
    // Top-right corner
    if let Some(g) = corner_glyph!(top, right, tr) {
        buf.set_grapheme(x1, y0, g, st);
    }

    // Left and right column middles (rows between the two corner rows)
    for y in (y0 + 1)..y1 {
        if left  { buf.set_grapheme(x0, y, v_line, st); }
        if right { buf.set_grapheme(x1, y, v_line, st); }
    }

    // Bottom-left corner
    if let Some(g) = corner_glyph!(bottom, left, bl) {
        buf.set_grapheme(x0, y1, g, st);
    }
    // Bottom row middle
    if bottom {
        for x in (x0 + 1)..x1 {
            buf.set_grapheme(x, y1, h_line, st);
        }
    }
    // Bottom-right corner
    if let Some(g) = corner_glyph!(bottom, right, br) {
        buf.set_grapheme(x1, y1, g, st);
    }
}

// ── frame_tiles ───────────────────────────────────────────────────────────────

/// Frame each solved leaf rect with a box and return the interior content rect.
///
/// For each `(TileId, Rect)` pair in `tiles`:
/// 1. Calls [`draw_box`] to draw the border into `buf`.
/// 2. Computes the *interior* rect by deflating the original rect by 1 on each
///    bordered side (saturating, so an under-sized tile yields a zero-area
///    interior).
///
/// Returns a `Vec` of `(TileId, interior_rect)` in the same order as `tiles`.
///
/// ## Degenerate tiles
///
/// A tile too small to have a usable interior (e.g. a 1×1 tile framed with
/// `Borders::ALL`) yields a content rect with zero width or height.  The caller
/// should check [`Rect::is_empty`] before rendering into the content rect.
///
/// ## Per-tile vs. shared-border mode
///
/// This is the **per-tile mode**: each tile gets its own box.  Adjacent tiles
/// produce a doubled gutter (`┐┌` / `││` / `┘└`).  For a single shared border
/// line with junction glyphs (`├ ┤ ┬ ┴ ┼`) between adjacent tiles, use
/// [`render_shared`] instead — that is the shared-border entry point.
///
/// To draw one box around a *group* of tiles, call [`draw_box`] on the bounding
/// rect of the group instead.
pub fn frame_tiles(
    buf: &mut Buffer,
    tiles: &[(TileId, Rect)],
    borders: Borders,
    style: &BorderStyle,
) -> Vec<(TileId, Rect)> {
    // Deflation amounts: 1 when the corresponding border is active, else 0.
    // Cast bool → u16 (true=1, false=0) avoids branches.
    let dl = borders.contains(Borders::LEFT)   as u16;
    let dt = borders.contains(Borders::TOP)    as u16;
    let dr = borders.contains(Borders::RIGHT)  as u16;
    let db = borders.contains(Borders::BOTTOM) as u16;

    tiles.iter().map(|&(id, rect)| {
        draw_box(buf, rect, borders, style);

        // Shrink the rect inward on each bordered side to get the interior.
        // Saturating arithmetic ensures we never underflow for tiny tiles.
        let x = rect.x.saturating_add(dl);
        let y = rect.y.saturating_add(dt);
        let w = rect.width.saturating_sub(dl + dr);
        let h = rect.height.saturating_sub(dt + db);

        (id, Rect::new(x, y, w, h))
    }).collect()
}

// ── render_shared ─────────────────────────────────────────────────────────────

/// Render a layout tree in **shared-border mode** and return each leaf's content rect.
///
/// In shared-border mode a single outer frame is drawn around `area`, and
/// adjacent tiles share a single divider line rather than drawing two touching
/// borders.  Junction glyphs (`├ ┤ ┬ ┴ ┼` and all mixed-weight variants) emerge
/// automatically from the [`EdgeGrid`] — none are special-cased here.
///
/// ## Layout rules
///
/// 1. The outer frame occupies one cell on each side of `area`.
/// 2. A `Split` node divides its inner area (the frame-minus-1-cell inset) among
///    its children, reserving one cell per internal divider: `n` children →
///    `n−1` dividers.  The Phase 1 sizing algorithm assigns widths/heights from
///    the reduced extent.
/// 3. Every leaf tile's returned content rect is the space inside all surrounding
///    borders — the caller does not inset again.
///
/// ## Corners
///
/// The frame's [`weight`](BorderStyle::weight) and glyph colour come from `style`,
/// matching [`draw_box`] and [`frame_tiles`].  When
/// [`corners`](BorderStyle::corners) is [`CornerStyle::Rounded`] the four **outer
/// frame corners** are emitted as `╭╮╰╯`; internal divider junctions (tees and
/// crosses) and any non-`Light` corner are left square, consistent with
/// [`border_glyphs`]'s "rounded is Light-only" rule.  In a shared-border layout
/// every divider spans the full frame, so the only pure two-arm Light corners are
/// the four outer corners — rounding is therefore well-defined and never touches
/// an interior junction.
///
/// ## Overrides
///
/// Each `(TileId, LineWeight)` pair in `overrides` adds that tile's four bordering
/// edges to the grid at the given weight.  Because the merge rule takes the
/// stronger weight, a heavy override thickens the focused tile's edges and
/// produces correct mixed-weight junction glyphs at shared corners/tees.
///
/// ## Degenerate inputs
///
/// `area` smaller than 2×2, or more dividers than available space: never panics;
/// draws what fits; zero-area content rects are returned for tiles with no room.
///
/// # Parameters
/// - `buf`: the buffer to paint into; cells are left untouched where no border arm
///   is present (i.e. the tile interiors remain blank).
/// - `root`: mutable so that `Orientation::Adaptive` can record its chosen axis.
/// - `style`: the frame's [`BorderStyle`] (weight + corners + colour); the same
///   type [`draw_box`] takes, so the three border entry points are consistent.
/// - `overrides`: weight overrides keyed by `TileId`; may be empty.
/// # Returns
/// Flat list of `(TileId, content_rect)` in depth-first, left-to-right order.
pub fn render_shared(
    buf: &mut Buffer,
    root: &mut Node,
    area: Rect,
    style: &BorderStyle,
    overrides: &[(TileId, LineWeight)],
) -> Vec<(TileId, Rect)> {
    let weight = style.weight;
    let mut grid = EdgeGrid::new(area);

    // Outer frame: one cell on all four sides.
    grid.add_box(area, weight);

    // Recursively add internal dividers and collect (id, box_rect, content_rect).
    let mut tile_info: Vec<(TileId, Rect, Rect)> = Vec::new();
    add_dividers(&mut grid, root, area, weight, &mut tile_info);

    // Per-tile weight overrides: add each tile's four bordering edges at the
    // override weight; the merge rule ensures heavier always wins.
    for &(id, ow) in overrides {
        if let Some(&(_, box_rect, _)) = tile_info.iter().find(|&&(tid, _, _)| tid == id) {
            grid.add_box(box_rect, ow);
        }
    }

    // Resolve every cell in the grid and write glyphs to `buf`.
    // `set_grapheme` silently ignores out-of-bounds positions, so no clip needed.
    let round = matches!(style.corners, CornerStyle::Rounded);
    let mut tmp = [0u8; 4];
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = grid.get(x, y) {
                if let Some(ch) = resolve_junction(cell) {
                    // Rounding applies only to pure two-arm Light corners — the
                    // four outer frame corners.  `round_light_corner` is a no-op
                    // for tees, crosses, lines, stubs, and heavy/double corners.
                    let ch = if round { round_light_corner(ch) } else { ch };
                    buf.set_grapheme(x, y, ch.encode_utf8(&mut tmp), style.style);
                }
            }
        }
    }

    tile_info.into_iter().map(|(id, _, content)| (id, content)).collect()
}

/// Map a square **Light** corner glyph to its rounded variant; pass everything
/// else through unchanged.
///
/// Only `┌ ┐ └ ┘` have curved forms (`╭ ╮ ╰ ╯`).  Heavy/double corners, tees,
/// crosses, lines, and stubs have no rounded variant and are returned as-is —
/// the same "rounded is Light-only, else fall back" rule [`border_glyphs`] uses.
fn round_light_corner(ch: char) -> char {
    match ch {
        '┌' => '╭',
        '┐' => '╮',
        '└' => '╰',
        '┘' => '╯',
        other => other,
    }
}

/// Recursively add internal divider lines to `grid` and collect tile metadata.
///
/// Each recursive call is handed the full `box_rect` of the node — including the
/// one-cell border that surrounds it.  For a `Tile`, the content rect is the
/// inner deflation of `box_rect` and is recorded in `out`.  For a `Split`, the
/// inner area is partitioned among children, a divider line is added between each
/// consecutive pair of children, and the function recurses into each child with
/// its own `box_rect`.  For a `Carousel`, visible children are determined using
/// the same viewport-intersection logic as `solve` and recorded without dividers
/// between items (full shared-border-within-carousel styling is Phase 4b).
///
/// The outer frame is *not* re-added here — it was added once by the caller.
/// Dividers that happen to coincide with the outer frame (e.g. when a child's
/// box_rect touches the root area) are merged harmlessly by the EdgeGrid.
fn add_dividers(
    grid: &mut EdgeGrid,
    node: &mut Node,
    box_rect: Rect,
    weight: LineWeight,
    out: &mut Vec<(TileId, Rect, Rect)>,
) {
    match node {
        Node::Tile(id) => {
            // Leaf: record the border rect and its deflated content rect.
            out.push((*id, box_rect, deflate(box_rect)));
        }
        Node::Split { orientation, children } => {
            if children.is_empty() {
                return;
            }
            let inner = deflate(box_rect);
            let axis = orientation.resolve(inner);
            let n = children.len();

            // Reserve one cell per internal divider from the content extent.
            let n_div = (n - 1) as u16;
            let available = match axis {
                Axis::Horizontal => inner.width.saturating_sub(n_div),
                Axis::Vertical => inner.height.saturating_sub(n_div),
            };

            let sizes = partition(children, available);

            // Starting position (in content coordinates) for the first child.
            let mut pos = match axis {
                Axis::Horizontal => inner.x,
                Axis::Vertical => inner.y,
            };

            for (i, ((_, child), &size)) in children.iter_mut().zip(sizes.iter()).enumerate() {
                // Compute the full box_rect for this child.  The first child's
                // near edge is the parent's outer border; subsequent children's
                // near edge is the shared divider one cell before `pos`.
                let child_box = match axis {
                    Axis::Horizontal => {
                        let left = if i == 0 { box_rect.x } else { pos.saturating_sub(1) };
                        let right = if i + 1 == n {
                            box_rect.right().saturating_sub(1)
                        } else {
                            pos.saturating_add(size)
                        };
                        Rect::new(left, box_rect.y, right.saturating_sub(left).saturating_add(1), box_rect.height)
                    }
                    Axis::Vertical => {
                        let top = if i == 0 { box_rect.y } else { pos.saturating_sub(1) };
                        let bot = if i + 1 == n {
                            box_rect.bottom().saturating_sub(1)
                        } else {
                            pos.saturating_add(size)
                        };
                        Rect::new(box_rect.x, top, box_rect.width, bot.saturating_sub(top).saturating_add(1))
                    }
                };

                // Draw the divider between this child and the next (skip for the last child).
                if i + 1 < n {
                    let div = pos.saturating_add(size);
                    match axis {
                        Axis::Horizontal => grid.add_v_line(
                            box_rect.y,
                            box_rect.bottom().saturating_sub(1),
                            div,
                            weight,
                        ),
                        Axis::Vertical => grid.add_h_line(
                            box_rect.x,
                            box_rect.right().saturating_sub(1),
                            div,
                            weight,
                        ),
                    }
                }

                add_dividers(grid, child, child_box, weight, out);

                // Advance past this child's content and the following divider.
                pos = pos.saturating_add(size).saturating_add(1);
            }
        }
        Node::Carousel { orientation, scroll, children, .. } => {
            if children.is_empty() {
                return;
            }
            // Carousel items live inside the carousel's own border frame.
            let inner = deflate(box_rect);
            let axis = orientation.resolve(inner);
            let main_extent = match axis {
                Axis::Horizontal => inner.width as i32,
                Axis::Vertical => inner.height as i32,
            };

            let total: i32 = children.iter().map(|(ext, _)| *ext as i32).sum();
            let max_scroll = (total - main_extent).max(0) as u16;
            *scroll = (*scroll).min(max_scroll);
            let scroll_val = *scroll;

            let vp_start = match axis {
                Axis::Horizontal => inner.x as i32,
                Axis::Vertical => inner.y as i32,
            };
            let vp_end = vp_start + main_extent;
            let mut pos = vp_start - scroll_val as i32;

            for (ext, child) in children.iter_mut() {
                let child_start = pos;
                let child_end = pos + *ext as i32;
                pos = child_end;

                let vis_start = child_start.max(vp_start);
                let vis_end = child_end.min(vp_end);
                if vis_start >= vis_end {
                    continue; // off-screen; skip
                }

                let vis_len = (vis_end - vis_start) as u16;
                // No dividers between carousel items.  Leaf tiles record their
                // visible rect directly as content (no extra border deflation);
                // nested Split/Carousel children receive the visible rect as
                // their box_rect and handle their own internal borders.
                let child_box = match axis {
                    Axis::Horizontal => Rect::new(
                        vis_start as u16, inner.y, vis_len, inner.height,
                    ),
                    Axis::Vertical => Rect::new(
                        inner.x, vis_start as u16, inner.width, vis_len,
                    ),
                };
                match child {
                    Node::Tile(id) => {
                        // Content rect = visible rect; carousel items share no inner border.
                        out.push((*id, child_box, child_box));
                    }
                    _ => {
                        add_dividers(grid, child, child_box, weight, out);
                    }
                }
            }
        }
    }
}

/// Deflate `r` by one cell on each side (saturating).
///
/// Returns the inner content region of a bordered tile: if any dimension is less
/// than 2 the result has zero extent on that axis.
fn deflate(r: Rect) -> Rect {
    Rect::new(
        r.x.saturating_add(1),
        r.y.saturating_add(1),
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    )
}

// ── BorderGap ─────────────────────────────────────────────────────────────────

/// A labelled region carved out of one edge of a border box.
///
/// Declare gaps *before* any rim animation so the animation can decide
/// whether to colour the gap cells or leave them for the caller to fill.
///
/// # Three-pass rendering pattern
///
/// ```text
/// 1. Structural pass  — draw corners, side bars, dash fills
/// 2. Rim-glow pass    — apply animation; cells inside gaps with
///                        rim_glow = false are skipped entirely
/// 3. Content pass     — render text / own animation into each gap rect;
///                        this naturally overrides glow on rim_glow gaps,
///                        or fills the clean dim cells on non-glow gaps
/// ```
///
/// Setting `rim_glow = true` lets the animation flow through the gap
/// uninterrupted; content drawn in pass 3 will land on already-coloured
/// glyphs and can blend or override as it chooses.
///
/// Setting `rim_glow = false` (the **default**) keeps these cells dark
/// and untouched by the animation, giving the gap full colour control.
pub struct BorderGap {
    /// The cell rectangle occupied by this gap.
    ///
    /// Typically a one-row strip on the top/bottom border edge, or a
    /// one-column strip on a side.  Whether the `┤`/`├` connector
    /// characters at each end of the gap are included is up to the
    /// caller; including them lets the animation colour the connectors,
    /// excluding them keeps them structurally dim.
    pub rect: Rect,

    /// `false` (default) — the rim animation skips these cells; the gap
    /// renders its own colours from scratch in the content pass.
    ///
    /// `true` — the rim animation colours these cells like any other
    /// border cell; the content pass then draws on top of that colour.
    pub rim_glow: bool,
}

impl BorderGap {
    /// Create a gap that opts **out** of rim glow (the common case).
    pub fn new(rect: Rect) -> Self {
        Self { rect, rim_glow: false }
    }

    /// Builder — opt **in** to rim glow.
    pub fn with_rim_glow(mut self) -> Self {
        self.rim_glow = true;
        self
    }

    /// Returns `true` if the cell at `(x, y)` falls inside this gap's rect.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        let r = &self.rect;
        x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
    }
}

// ── render_rim ──────────────────────────────────────────────────────────────────

/// Re-style the perimeter cells of `rect` from a caller-owned policy — a stateless
/// rim pass for travelling-glow border animations.
///
/// Walks the discrete border cells clockwise via [`Rect::border_cells`].  For each
/// cell it computes the normalised perimeter position [`Rect::border_pos`] and hands
/// it, together with the cell's **current** [`Style`], to `style_at`.  When the
/// closure returns `Some(new)` the cell's glyph is preserved and re-drawn in `new`;
/// `None` leaves the cell untouched.  Easing, colour, and phase therefore all stay
/// in the caller — mullion contributes only the walk and the `border_pos` lookup,
/// so the engine stays content-agnostic.
///
/// Cells inside a [`BorderGap`] whose [`rim_glow`](BorderGap::rim_glow) is `false`
/// are **skipped**, so a gap that paints its own colours in a later pass is never
/// overwritten by the animation.  Gaps with `rim_glow == true` are walked like any
/// other border cell.
///
/// Walks whatever [`Rect::border_cells`] yields, so it only skips everything for a
/// zero-area rect; a degenerate 1×N / N×1 rect has no distinct perimeter, so
/// [`Rect::border_pos`] reports every cell at position `0.0` and the whole run is
/// styled uniformly.  Never panics.  Call it after the structural border pass and
/// before the gap-content pass (the three-pass pattern documented on [`BorderGap`]).
pub fn render_rim(
    buf: &mut Buffer,
    rect: Rect,
    gaps: &[BorderGap],
    style_at: impl Fn(f32, Style) -> Option<Style>,
) {
    for (x, y) in rect.border_cells() {
        // Skip cells owned by a non-glow gap: they render themselves later.
        if gaps.iter().any(|g| !g.rim_glow && g.contains(x, y)) {
            continue;
        }
        if !buf.area.contains(x, y) {
            continue;
        }
        let pos = rect.border_pos(x, y);
        let current = buf.get(x, y).style;
        if let Some(new_style) = style_at(pos, current) {
            // Preserve the existing glyph; only the style changes.
            let symbol = buf.get(x, y).symbol.clone();
            buf.set_grapheme(x, y, &symbol, new_style);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_sets_are_correct() {
        // Light/Square
        assert_eq!(
            border_glyphs(&LineWeight::Light, &CornerStyle::Square),
            ("─", "│", "┌", "┐", "└", "┘")
        );
        // Light/Rounded
        assert_eq!(
            border_glyphs(&LineWeight::Light, &CornerStyle::Rounded),
            ("─", "│", "╭", "╮", "╰", "╯")
        );
        // Heavy/Square
        assert_eq!(
            border_glyphs(&LineWeight::Heavy, &CornerStyle::Square),
            ("━", "┃", "┏", "┓", "┗", "┛")
        );
        // Heavy/Rounded → square heavy fallback (no curved heavy corners exist)
        assert_eq!(
            border_glyphs(&LineWeight::Heavy, &CornerStyle::Rounded),
            ("━", "┃", "┏", "┓", "┗", "┛"),
            "Rounded+Heavy must fall back to square heavy corners"
        );
        // Double/Square
        assert_eq!(
            border_glyphs(&LineWeight::Double, &CornerStyle::Square),
            ("═", "║", "╔", "╗", "╚", "╝")
        );
        // Double/Rounded → square double fallback
        assert_eq!(
            border_glyphs(&LineWeight::Double, &CornerStyle::Rounded),
            ("═", "║", "╔", "╗", "╚", "╝"),
            "Rounded+Double must fall back to square double corners"
        );
    }

    #[test]
    fn round_light_corner_only_touches_light_corners() {
        assert_eq!(round_light_corner('┌'), '╭');
        assert_eq!(round_light_corner('┘'), '╯');
        // Tees, crosses, heavy/double corners, and lines pass through unchanged.
        for ch in ['┼', '├', '┬', '┏', '╔', '─', '│', '╶'] {
            assert_eq!(round_light_corner(ch), ch, "{ch} must not round");
        }
    }

    #[test]
    fn render_rim_recolors_border_and_skips_nonglow_gaps() {
        use crate::style::Color;

        let area = Rect::new(0, 0, 6, 4);
        let style = BorderStyle {
            weight: LineWeight::Light,
            corners: CornerStyle::Square,
            style: Style::default(),
        };

        // A non-glow gap over one top-edge cell, and a glow gap over another.
        let quiet = BorderGap::new(Rect::new(2, 0, 1, 1));
        let glow = BorderGap::new(Rect::new(3, 0, 1, 1)).with_rim_glow();
        let gaps = [quiet, glow];

        let mut buf = Buffer::empty(area);
        draw_box(&mut buf, area, Borders::ALL, &style);
        let quiet_before = buf.get(2, 0).style;

        render_rim(&mut buf, area, &gaps, |_pos, cur| Some(cur.fg(Color::Red)));

        // Ordinary border cells are recoloured but keep their glyph.
        assert_eq!(buf.get(0, 0).symbol, "┌", "glyph preserved");
        assert_eq!(buf.get(0, 0).style.fg, Color::Red);
        assert_eq!(buf.get(1, 0).style.fg, Color::Red);
        // The non-glow gap cell is left exactly as it was.
        assert_eq!(buf.get(2, 0).style, quiet_before, "non-glow gap skipped");
        // The glow gap cell IS walked and recoloured.
        assert_eq!(buf.get(3, 0).style.fg, Color::Red, "glow gap recoloured");
        // Interior cells are never on the perimeter walk.
        assert_eq!(buf.get(2, 2).style.fg, quiet_before.fg, "interior untouched");
    }
}
