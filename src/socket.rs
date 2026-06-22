// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Sockets: semantic gaps in a tile's border where connectors attach
//! (design note §5.1).
//!
//! A [`Socket`] is a [`BorderGap`](crate::border::BorderGap) **with semantics** —
//! a `(side, offset, direction, type)` tuple anchored to one edge of a tile,
//! rather than a decorative opening. That sockets fall out of an existing
//! primitive is the signal the feature belongs in mullion: the hard part, placing
//! and sizing edge gaps correctly at every box size, is the gap-interval geometry
//! proven in the `spiral_stress` "surf field" (§7) and lifted here.
//!
//! ## What was lifted, and what was dropped
//!
//! Two kernels are lifted from the surf field into this public API:
//!
//! - **Gap-interval geometry** — [`Socket::gap`] computes an edge gap, clamps it
//!   to the valid edge-local range `1..len-1` (the corners are never opened), and
//!   offsets edge-local → absolute. Robust at every tile size.
//! - **Connector-flow gradient** — [`FlowStyle::color`] streams a hue along a gap
//!   to animate flow direction along a connector (signal flow, data flow). Purely
//!   decorative and parameterized.
//!
//! The surf field's **autonomous motion** (gaps drifting, pulsing, splitting and
//! merging on their own) is deliberately **not** lifted: real sockets are *pinned*
//! to connectors, not wandering. A `Socket` is static geometry; the only thing
//! that moves is the optional flow gradient, and only because the caller advances
//! its `t`.

use crate::border::BorderGap;
use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::label::Side;
use crate::style::{Color, Style};
use crate::tree::Direction;

// ── Flow ─────────────────────────────────────────────────────────────────────

/// A socket's connector direction — the "direction" of §5.1's socket tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flow {
    /// An input: signal/data flows **into** the tile here.
    #[default]
    In,
    /// An output: signal/data flows **out of** the tile here.
    Out,
    /// Carries flow in both directions.
    Bidirectional,
}

// ── Socket ───────────────────────────────────────────────────────────────────

/// A semantic gap on one edge of a tile's border — where a connector attaches.
///
/// The geometry is edge-local: `offset` and `length` are measured in cells along
/// the chosen `side`, independent of where the tile lands on screen.
/// [`gap`](Socket::gap) resolves them against a concrete tile rect, clamping the
/// opening to the edge interior (`1..len-1`) so a socket never lands on a corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Socket {
    /// Which edge of the tile this socket sits on.
    pub side: Side,
    /// Edge-local start cell along `side` (0 is the corner; clamped to ≥ 1).
    pub offset: u16,
    /// Gap length in cells (≥ 1).
    pub length: u16,
    /// Connector direction (input / output / bidirectional).
    pub flow: Flow,
    /// Caller-assigned semantic type tag — the "type" of §5.1's socket tuple.
    /// mullion attaches no meaning to it; a consuming app uses it to decide which
    /// sockets may connect (e.g. an audio port vs. a control port).
    pub kind: u16,
}

impl Socket {
    /// Create a unit-length (1-cell) socket.
    pub fn new(side: Side, offset: u16, flow: Flow, kind: u16) -> Self {
        Self { side, offset, length: 1, flow, kind }
    }

    /// Set the gap length and return `self` (builder style).
    pub fn with_length(mut self, length: u16) -> Self {
        self.length = length.max(1);
        self
    }

    /// The edge length (in cells) of `side` for a `tile` — width for top/bottom,
    /// height for left/right.
    fn edge_len(side: Side, tile: Rect) -> u16 {
        match side {
            Side::Top | Side::Bottom => tile.width,
            Side::Left | Side::Right => tile.height,
        }
    }

    /// Resolve this socket to a [`BorderGap`] on `tile`, or `None` if it does not
    /// fit (the edge is shorter than 3 cells, or the socket lies entirely on/past
    /// the corners).
    ///
    /// The opening is clamped to the edge interior `1..len-1`: the start is pulled
    /// to ≥ 1 and the end to ≤ `edge_len - 1`, so the two corner cells are never
    /// opened (this is the surf field's `make_gap` clamp). The clamped edge-local
    /// interval is then offset into absolute coordinates for the side: along the
    /// top/bottom row, or down the left/right column.
    pub fn gap(&self, tile: Rect) -> Option<BorderGap> {
        let edge_len = Self::edge_len(self.side, tile);
        // Need at least one interior cell: corners at 0 and edge_len-1.
        if edge_len < 3 {
            return None;
        }
        // Clamp the edge-local interval to [1, edge_len-1).
        let start = self.offset.max(1);
        let end = self.offset.saturating_add(self.length).min(edge_len - 1);
        if end <= start {
            return None; // collapsed (socket sits on/beyond the corners)
        }
        let span = end - start;

        // Edge-local → absolute for the chosen side.
        let rect = match self.side {
            Side::Top => Rect::new(tile.x.saturating_add(start), tile.y, span, 1),
            Side::Bottom => {
                Rect::new(tile.x.saturating_add(start), tile.bottom() - 1, span, 1)
            }
            Side::Left => Rect::new(tile.x, tile.y.saturating_add(start), 1, span),
            Side::Right => Rect::new(tile.right() - 1, tile.y.saturating_add(start), 1, span),
        };
        Some(BorderGap::new(rect))
    }

    /// The absolute cell rect of this socket's gap on `tile` (the [`gap`](Socket::gap)
    /// rect), or `None` if it does not fit.
    pub fn rect(&self, tile: Rect) -> Option<Rect> {
        self.gap(tile).map(|g| g.rect)
    }

    /// The single cell a connector attaches to — the middle of the gap on the
    /// border — or `None` if the socket does not fit. Useful as a routing
    /// endpoint (Phase 8).
    pub fn anchor(&self, tile: Rect) -> Option<(u16, u16)> {
        let r = self.rect(tile)?;
        // Middle cell of the one-row or one-column strip.
        let x = r.x + (r.width.saturating_sub(1)) / 2;
        let y = r.y + (r.height.saturating_sub(1)) / 2;
        Some((x, y))
    }

    /// Place `count` unit sockets evenly along `side` of an edge `edge_len` cells
    /// long, packed so their gaps never overlap.
    ///
    /// All sockets land in the edge interior `[1, edge_len-2]` and are spaced at
    /// least one cell apart. At most `edge_len - 2` sockets fit (one per interior
    /// cell); a larger `count` is truncated to that many. Returns them with
    /// [`Flow::In`] and `kind = 0` — set those on the results as needed.
    pub fn pack(side: Side, count: usize, edge_len: u16) -> Vec<Socket> {
        let mut out = Vec::new();
        if edge_len < 3 || count == 0 {
            return out;
        }
        // Interior cells are indices 1..=edge_len-2 (that many cells).
        let interior = (edge_len - 2) as usize;
        let n = count.min(interior);
        for k in 0..n {
            // floor(k*interior/n) is strictly increasing by ≥ 1 when interior ≥ n,
            // so unit gaps at `1 + that` never collide.
            let offset = 1 + (k * interior) / n;
            out.push(Socket::new(side, offset as u16, Flow::In, 0));
        }
        out
    }

    /// The direction this socket faces, away from the tile: `Top`→`Up`,
    /// `Bottom`→`Down`, `Left`→`Left`, `Right`→`Right`. A connector approaches
    /// the socket along this axis.
    pub fn outward(&self) -> Direction {
        match self.side {
            Side::Top => Direction::Up,
            Side::Bottom => Direction::Down,
            Side::Left => Direction::Left,
            Side::Right => Direction::Right,
        }
    }

    /// The cell just **outside** the socket — one step in the
    /// [`outward`](Socket::outward) direction from the [`anchor`](Socket::anchor).
    /// This is the free cell a connector routes to/from (its endpoint), so the
    /// wire stays clear of the node's border. `None` if the socket does not fit,
    /// or if the outward cell would fall off the coordinate origin.
    pub fn attach(&self, tile: Rect) -> Option<(u16, u16)> {
        let (ax, ay) = self.anchor(tile)?;
        let (dx, dy) = self.outward().delta();
        let x = u16::try_from(ax as i32 + dx).ok()?;
        let y = u16::try_from(ay as i32 + dy).ok()?;
        Some((x, y))
    }
}

/// The pair of box-drawing **bookend** caps that flank a socket's opening in an
/// edge, as `(start, end)`.
///
/// A socket is a gap carved into the border: the border line stops at a bookend
/// cap on each side of the opening, with a circle terminal floating between them.
/// On a **horizontal** edge (top/bottom) the caps are `┤` (left) and `├` (right);
/// on a **vertical** edge (left/right) they are `┴` (above) and `┬` (below).
/// See [`draw_socket`].
pub fn bookends(side: Side) -> (&'static str, &'static str) {
    match side {
        Side::Top | Side::Bottom => ("┤", "├"),
        Side::Left | Side::Right => ("┴", "┬"),
    }
}

/// Draw a socket as a **bookended gap** in `tile`'s edge, with a circle terminal
/// in the opening: `●` when `connected`, `○` when not.
///
/// The look is `┤○ ├` along a horizontal edge and `┴●┬` stacked along a vertical
/// one — the border line capped by [`bookends`] on each side of the circle, which
/// floats with breathing room (a round glyph never has to meet a line). `style`
/// colours both the caps and the circle.
///
/// Does nothing if the socket does not fit with its caps clear of the corners.
pub fn draw_socket(buf: &mut Buffer, tile: Rect, socket: &Socket, connected: bool, style: Style) {
    let Some((ax, ay)) = socket.anchor(tile) else { return };
    let (cap_a, cap_b) = bookends(socket.side);
    let circle = if connected { "●" } else { "○" };
    match socket.side {
        Side::Top | Side::Bottom => {
            // ┤○ ├  — both caps must stay clear of the corner cells.
            if ax < tile.x + 2 || ax + 2 > tile.right() - 2 {
                return;
            }
            buf.set_grapheme(ax - 1, ay, cap_a, style);
            buf.set_grapheme(ax, ay, circle, style);
            buf.set_grapheme(ax + 1, ay, " ", style);
            buf.set_grapheme(ax + 2, ay, cap_b, style);
        }
        Side::Left | Side::Right => {
            // ┴●┬ stacked.
            if ay < tile.y + 2 || ay + 1 > tile.bottom() - 2 {
                return;
            }
            buf.set_grapheme(ax, ay - 1, cap_a, style);
            buf.set_grapheme(ax, ay, circle, style);
            buf.set_grapheme(ax, ay + 1, cap_b, style);
        }
    }
}

// ── FlowStyle ────────────────────────────────────────────────────────────────

/// The connector-flow gradient: a hue that scrolls along a gap/connector to show
/// flow direction or activity (§7's `stream_color`, parameterized).
///
/// Purely decorative — nothing here is required to use sockets. The colour is a
/// pure function of position and time, so the animation only advances when the
/// caller advances `t`; there is no hidden state and no autonomous drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowStyle {
    /// Hue family. Each band gets a maximally distinct base hue via golden-angle
    /// spacing, so different connectors read as different colours.
    pub band: usize,
    /// Scroll speed: how fast the gradient travels along the connector per unit
    /// `t`. Sign is folded into `direction`.
    pub speed: f32,
    /// Degrees of hue swept along the connector's length.
    pub sweep: f32,
    /// Flow direction: `+1.0` streams forward, `-1.0` reverses (e.g. from a
    /// socket's [`Flow`]).
    pub direction: f32,
}

impl Default for FlowStyle {
    /// The surf-field look: band 0, gentle forward scroll, a 90° hue sweep.
    fn default() -> Self {
        Self { band: 0, speed: 0.55, sweep: 90.0, direction: 1.0 }
    }
}

impl FlowStyle {
    /// The colour for a point at normalised position `pos` (0..1 along the
    /// connector) at time `t`. `active` brightens the cell (e.g. a set bit or a
    /// live segment) so a payload stays legible against the gradient.
    ///
    /// The position is shifted by `t * direction * speed` so the gradient scrolls;
    /// the hue sweeps `sweep` degrees from the band's base hue, while saturation
    /// and value shimmer on their own cycles for a lively, non-flat look.
    pub fn color(&self, pos: f32, t: f32, active: bool) -> Style {
        use std::f32::consts::TAU;
        // Golden-angle base hue keeps bands maximally distinct.
        let base_hue = (self.band as f32 * 137.508) % 360.0;
        // Scroll the position with time → streaming motion.
        let p = pos + t * self.direction * self.speed;
        let hue = base_hue + p * self.sweep;
        // Saturation/value shimmer on independent cycles for sparkle.
        let val = 0.45 + 0.55 * (p * TAU * 1.5).sin().powi(2);
        let sat = 0.70 + 0.30 * (p * TAU * 2.3).cos().abs();
        // Active cells glow brighter; inactive ones recede.
        let val = if active { (val + 0.30).min(1.0) } else { val * 0.75 };
        Style::default().fg(Color::from_hsv(hue, sat, val))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_on_each_side_is_interior() {
        let tile = Rect::new(2, 3, 10, 6);
        // Top: offset 4, length 2 → cells (6,3) and (7,3) on the top row.
        let top = Socket::new(Side::Top, 4, Flow::In, 0).with_length(2);
        assert_eq!(top.rect(tile), Some(Rect::new(6, 3, 2, 1)));
        // Bottom row is tile.bottom()-1 = 3+6-1 = 8.
        let bot = Socket::new(Side::Bottom, 4, Flow::Out, 0);
        assert_eq!(bot.rect(tile), Some(Rect::new(6, 8, 1, 1)));
        // Left column is tile.x = 2; right is tile.right()-1 = 11.
        let lft = Socket::new(Side::Left, 2, Flow::In, 0);
        assert_eq!(lft.rect(tile), Some(Rect::new(2, 5, 1, 1)));
        let rgt = Socket::new(Side::Right, 2, Flow::Out, 0);
        assert_eq!(rgt.rect(tile), Some(Rect::new(11, 5, 1, 1)));
    }

    #[test]
    fn corners_are_never_opened() {
        let tile = Rect::new(0, 0, 8, 4);
        // A unit socket sitting exactly on the corner (offset 0) has no interior
        // overlap, so it yields nothing (faithful to make_gap: the interval is
        // intersected with the interior, not shifted onto it).
        assert_eq!(Socket::new(Side::Top, 0, Flow::In, 0).rect(tile), None);
        // A longer corner socket keeps only its interior portion: [0,3) → [1,3).
        let across = Socket::new(Side::Top, 0, Flow::In, 0).with_length(3);
        assert_eq!(across.rect(tile), Some(Rect::new(1, 0, 2, 1)));
        // A socket whose length would reach the far corner is clamped short of it.
        let long = Socket::new(Side::Top, 1, Flow::In, 0).with_length(99);
        let r = long.rect(tile).unwrap();
        assert!(r.right() < tile.right(), "gap must not reach the corner");
    }

    #[test]
    fn too_small_or_offscreen_yields_none() {
        // Edge length 2 has no interior.
        assert_eq!(Socket::new(Side::Top, 1, Flow::In, 0).rect(Rect::new(0, 0, 2, 2)), None);
        // Offset at/past the far corner collapses.
        assert_eq!(Socket::new(Side::Top, 9, Flow::In, 0).rect(Rect::new(0, 0, 8, 4)), None);
    }

    #[test]
    fn anchor_is_gap_centre() {
        let tile = Rect::new(0, 0, 12, 4);
        let s = Socket::new(Side::Top, 3, Flow::In, 0).with_length(4); // cells x=3..7
        assert_eq!(s.anchor(tile), Some((3 + 3 / 2, 0))); // middle of [3,7) → x=4
    }

    #[test]
    fn pack_places_non_overlapping_sockets() {
        let sockets = Socket::pack(Side::Left, 3, 12);
        assert_eq!(sockets.len(), 3);
        let tile = Rect::new(0, 0, 6, 12);
        let rects: Vec<Rect> = sockets.iter().map(|s| s.rect(tile).unwrap()).collect();
        // Distinct, ascending rows, all interior.
        for w in rects.windows(2) {
            assert!(w[0].y < w[1].y, "sockets must be ordered and distinct");
        }
        for r in &rects {
            assert!(r.y > tile.y && r.bottom() < tile.bottom());
        }
    }

    #[test]
    fn pack_truncates_to_interior() {
        // Edge length 5 → interior is 3 cells; asking for 10 yields 3.
        assert_eq!(Socket::pack(Side::Top, 10, 5).len(), 3);
        assert!(Socket::pack(Side::Top, 5, 2).is_empty());
    }

    /// Lock the glyph positions of sockets on all four sides of a box.
    #[test]
    fn four_sided_socket_snapshot() {
        use crate::backend::TestBackend;
        use crate::border::{draw_box, BorderStyle, Borders, CornerStyle, LineWeight};
        use crate::Terminal;

        let tile = Rect::new(0, 0, 13, 7);
        // (side, offset, connected)
        let sockets = [
            (Side::Top, 5u16, false),
            (Side::Bottom, 5, true),
            (Side::Left, 3, true),
            (Side::Right, 3, false),
        ];

        let mut term = Terminal::new(TestBackend::new(13, 7)).unwrap();
        term
            .draw(|buf| {
                draw_box(buf, tile, Borders::ALL, &BorderStyle {
                    weight: LineWeight::Light,
                    corners: CornerStyle::Rounded,
                    style: Style::default(),
                });
                for &(side, offset, connected) in &sockets {
                    let s = Socket::new(side, offset, Flow::In, 0);
                    draw_socket(buf, tile, &s, connected, Style::default());
                }
            })
            .unwrap();

        // Bookended gaps: ┤○ ├ / ┴●┬, circle floating in the opening.
        crate::assert_backend_snapshot!(
            term,
            "╭───┤○ ├────╮\n\
             │           │\n\
             ┴           ┴\n\
             ●           ○\n\
             ┬           ┬\n\
             │           │\n\
             ╰───┤● ├────╯"
        );
    }

    /// Socket positions stay correct (and clear of corners) at a different size.
    #[test]
    fn sockets_track_a_larger_tile() {
        let tile = Rect::new(3, 2, 20, 10);
        // Two packed inputs on the left, two outputs on the right.
        let ins = Socket::pack(Side::Left, 2, tile.height);
        let outs = Socket::pack(Side::Right, 2, tile.height);
        for s in ins.iter().chain(outs.iter()) {
            let r = s.rect(tile).unwrap();
            // On the correct column, and never on a corner row.
            let col = if s.side == Side::Left { tile.x } else { tile.right() - 1 };
            assert_eq!(r.x, col);
            assert!(r.y > tile.y && r.bottom() < tile.bottom());
        }
    }

    #[test]
    fn bookends_per_orientation() {
        // Horizontal edges are capped left/right; vertical edges above/below.
        assert_eq!(bookends(Side::Top), ("┤", "├"));
        assert_eq!(bookends(Side::Bottom), ("┤", "├"));
        assert_eq!(bookends(Side::Left), ("┴", "┬"));
        assert_eq!(bookends(Side::Right), ("┴", "┬"));
    }

    #[test]
    fn flow_color_is_deterministic_and_scrolls() {
        let fs = FlowStyle::default();
        // Same inputs → same colour (pure function, no hidden state).
        assert_eq!(fs.color(0.2, 1.0, true), fs.color(0.2, 1.0, true));
        // Advancing t changes the colour (the gradient scrolls).
        assert_ne!(fs.color(0.2, 0.0, true), fs.color(0.2, 5.0, true));
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    fn side() -> impl Strategy<Value = Side> {
        prop_oneof![Just(Side::Top), Just(Side::Bottom), Just(Side::Left), Just(Side::Right)]
    }

    proptest! {
        /// A socket's gap always lies within the edge interior `1..edge_len-1`
        /// (never on a corner), at any tile size and offset/length.
        #[test]
        fn prop_gap_within_interior(
            s in side(),
            offset in 0u16..30,
            length in 1u16..30,
            w in 1u16..40, h in 1u16..40,
        ) {
            let tile = Rect::new(5, 7, w, h);
            let socket = Socket::new(s, offset, Flow::In, 0).with_length(length);
            if let Some(r) = socket.rect(tile) {
                let edge_len = Socket::edge_len(s, tile);
                // Express the gap back in edge-local coordinates and check bounds.
                let (lo, hi) = match s {
                    Side::Top | Side::Bottom => (r.x - tile.x, r.right() - tile.x),
                    Side::Left | Side::Right => (r.y - tile.y, r.bottom() - tile.y),
                };
                prop_assert!(lo >= 1, "gap starts on a corner: lo={lo}");
                prop_assert!(hi < edge_len, "gap reaches the far corner: hi={hi} len={edge_len}");
                prop_assert!(hi > lo, "gap is non-empty");
            }
        }

        /// Packed sockets on one edge never overlap, however many are requested.
        #[test]
        fn prop_packed_sockets_never_overlap(
            s in side(),
            count in 0usize..40,
            edge in 1u16..40,
        ) {
            // Build a tile whose relevant edge is `edge` long.
            let tile = match s {
                Side::Top | Side::Bottom => Rect::new(0, 0, edge, 6),
                Side::Left | Side::Right => Rect::new(0, 0, 6, edge),
            };
            let sockets = Socket::pack(s, count, edge);
            let rects: Vec<Rect> = sockets.iter().filter_map(|sk| sk.rect(tile)).collect();
            for i in 0..rects.len() {
                for j in (i + 1)..rects.len() {
                    prop_assert!(rects[i].intersection(rects[j]).is_empty(),
                        "packed sockets overlap: {:?} vs {:?}", rects[i], rects[j]);
                }
            }
        }
    }
}
