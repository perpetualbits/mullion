// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Semantic (level-of-detail) zoom: grow a tile through the layout solver and swap
//! its renderer as it crosses area thresholds (design note §5.6).
//!
//! Terminal cells do not scale continuously — there is no 1.7× cell — so "zoom" is
//! not optical magnification. It is **level of detail**, driven by two cooperating
//! mechanisms:
//!
//! - **Continuous area animation** ([`Zoom`]) grows a focused tile by easing its
//!   solver `Fill` weight up, so [`layout::solve`](crate::layout::solve) itself
//!   expands it smoothly and the rest of the layout reflows around it — the
//!   technique from aerie's `spiral_stress` animated zoom, lifted here (the
//!   alternative, [`Tree::zoom_to`](crate::Tree::zoom_to), is a discrete jump).
//! - **Discrete LoD thresholds** ([`Lod::for_area`]) pick a renderer from the cells
//!   a tile ends up with: collapsed → titled → ported → full internal graph.
//!
//! The two line up because cells and detail levels are both discrete — a better fit
//! for terminals than smooth optical zoom would be. A focus may be a tiling child, a
//! floating child, or a node inside a nested graph (see [`FocusTarget`]).

use crate::ease::{lerp, smoothstep};
use crate::geometry::Rect;
use crate::layout::TileId;

// ── Level of detail ────────────────────────────────────────────────────────────

/// How much of a tile to render, in increasing detail. The variants are **ordered**
/// (`Collapsed < Titled < Ported < Full`), so "more detail" compares greater.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lod {
    /// Just a marker — the tile is too small for anything legible.
    Collapsed,
    /// A title/label, but no internals.
    Titled,
    /// Title plus visible ports (sockets).
    Ported,
    /// The full internal graph.
    Full,
}

/// The smallest tile **area** (in cells) that earns each [`Lod`]. Below `titled` a
/// tile is [`Collapsed`](Lod::Collapsed); the fields must be non-decreasing for
/// selection to stay monotonic (`titled ≤ ported ≤ full`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LodScale {
    /// Area at which a tile gains a title.
    pub titled: u32,
    /// Area at which it gains visible ports.
    pub ported: u32,
    /// Area at which it shows its full internal graph.
    pub full: u32,
}

impl Default for LodScale {
    fn default() -> Self {
        // Roughly: a title fits by ~14×3, ports by ~20×8, an internal graph by ~30×12.
        Self { titled: 40, ported: 160, full: 360 }
    }
}

impl Lod {
    /// The detail level for a tile of `area` cells under `scale`. **Monotonic**: a
    /// larger area never returns a less-detailed level.
    pub fn for_area(area: u32, scale: LodScale) -> Lod {
        if area < scale.titled {
            Lod::Collapsed
        } else if area < scale.ported {
            Lod::Titled
        } else if area < scale.full {
            Lod::Ported
        } else {
            Lod::Full
        }
    }

    /// The detail level for a `rect`'s area (see [`for_area`](Lod::for_area)).
    pub fn for_rect(rect: Rect, scale: LodScale) -> Lod {
        Lod::for_area(rect.area(), scale)
    }
}

// ── Animated zoom (through the solver) ───────────────────────────────────────────

/// A continuous zoom that grows a focus tile by easing up its layout `Fill` weight.
///
/// The solver does the actual growing — feed [`weight`](Zoom::weight) into each
/// sibling's `Constraint::new(Size::Fill(_))` and re-solve every frame — so the
/// focus expands smoothly while the rest of the layout reflows around it, rather
/// than the focus snapping to fullscreen as [`Tree::zoom_to`](crate::Tree::zoom_to)
/// does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// The id of the tile being zoomed; its weight grows, all others stay at `1`.
    pub focus: TileId,
    /// Eased progress in `[0, 1]` (`0` = base layout, `1` = fully zoomed).
    eased: f32,
    /// Extra `Fill` weight given to the focus at full zoom.
    max_weight: u16,
}

impl Zoom {
    /// A zoom on `focus`, at rest (progress 0), reaching `max_weight` extra `Fill`
    /// weight at full zoom (≈400 makes the focus nearly fill its axis).
    pub fn new(focus: TileId, max_weight: u16) -> Self {
        Self { focus, eased: 0.0, max_weight }
    }

    /// Set the raw progress, clamped to `[0, 1]` and stored **smoothstep-eased** so
    /// the growth starts and stops gently.
    pub fn set_progress(&mut self, raw: f32) {
        self.eased = smoothstep(raw.clamp(0.0, 1.0));
    }

    /// The eased progress in `[0, 1]`.
    pub fn progress(&self) -> f32 {
        self.eased
    }

    /// The `Fill` weight for tile `id`: the eased large weight for the focus, `1`
    /// for every other tile.
    pub fn weight(&self, id: TileId) -> u16 {
        if id == self.focus {
            1 + (self.eased * self.max_weight as f32) as u16
        } else {
            1
        }
    }
}

/// Interpolate a rect from `from` to `to` at `t` in `[0, 1]` (ease `t` first if you
/// want gentle motion). At `t = 0` it equals `from`, at `t = 1` exactly `to`.
///
/// Use this to grow a **floating child** or a **graph node** toward a target rect —
/// these do not pass through the `Fill` solver, so their zoom is a direct rect
/// animation rather than a [`Zoom`] weight.
pub fn lerp_rect(from: Rect, to: Rect, t: f32) -> Rect {
    if t >= 1.0 {
        return to;
    }
    let li = |a: u16, b: u16| lerp(a as f32, b as f32, t).round() as u16;
    Rect::new(li(from.x, to.x), li(from.y, to.y), li(from.width, to.width), li(from.height, to.height))
}

// ── Focus targeting ──────────────────────────────────────────────────────────────

/// What a zoom is focused on. All three carry a [`TileId`]; the variant says which
/// structure holds it, so the caller queries the matching solver — a tiling
/// [`Tree`](crate::Tree)/[`layout::solve`](crate::layout::solve), a
/// [`FloatLayer`](crate::FloatLayer), or a nested [`GraphCanvas`](crate::GraphCanvas).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    /// A tiling child of a layout tree.
    Tile(TileId),
    /// A floating child of a `FloatLayer`.
    Float(TileId),
    /// A node of a nested `GraphCanvas`.
    Node(TileId),
}

impl FocusTarget {
    /// The target id, whichever kind it is.
    pub fn id(self) -> TileId {
        match self {
            FocusTarget::Tile(id) | FocusTarget::Float(id) | FocusTarget::Node(id) => id,
        }
    }

    /// Resolve to the target's rect by looking its id up in `solved` — the
    /// `(id, rect)` pairs from the solver this target names. `None` if absent (e.g.
    /// the target was culled or removed).
    pub fn resolve(self, solved: &[(TileId, Rect)]) -> Option<Rect> {
        solved.iter().find(|(id, _)| *id == self.id()).map(|(_, r)| *r)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_thresholds() {
        let s = LodScale::default(); // 40 / 160 / 360
        assert_eq!(Lod::for_area(12, s), Lod::Collapsed); // 4×3
        assert_eq!(Lod::for_area(60, s), Lod::Titled); // 15×4
        assert_eq!(Lod::for_area(200, s), Lod::Ported); // 20×10
        assert_eq!(Lod::for_area(600, s), Lod::Full); // 30×20
        // Ordering holds.
        assert!(Lod::Collapsed < Lod::Titled);
        assert!(Lod::Ported < Lod::Full);
    }

    #[test]
    fn zoom_weight_grows_only_the_focus() {
        let mut z = Zoom::new(2, 400);
        assert_eq!(z.weight(2), 1); // at rest
        assert_eq!(z.weight(1), 1);
        z.set_progress(1.0);
        assert_eq!(z.weight(2), 401); // fully zoomed focus
        assert_eq!(z.weight(1), 1); // others unchanged
        assert_eq!(z.weight(3), 1);
    }

    #[test]
    fn zoom_grows_focus_through_the_solver() {
        use crate::layout::{self, Constraint, Node, Orientation, Size};
        let build = |z: &Zoom| Node::Split {
            orientation: Orientation::Horizontal,
            children: (1..=3u64)
                .map(|id| (Constraint::new(Size::Fill(z.weight(id))), Node::Tile(id)))
                .collect(),
        };
        let area = Rect::new(0, 0, 90, 10);
        let focus_w = |tiles: &[(TileId, Rect)]| tiles.iter().find(|(i, _)| *i == 2).unwrap().1.width;

        let mut z = Zoom::new(2, 400);
        z.set_progress(0.0);
        let w0 = focus_w(&layout::solve(&mut build(&z), area));
        z.set_progress(1.0);
        let w1 = focus_w(&layout::solve(&mut build(&z), area));
        assert!(w1 > w0 * 2, "focus grows under zoom: {w0} → {w1}");
        assert!(w1 >= 84, "fully-zoomed focus nearly fills the 90-wide area: {w1}");
    }

    #[test]
    fn lerp_rect_converges_to_target() {
        let from = Rect::new(10, 5, 4, 2);
        let to = Rect::new(0, 0, 40, 20);
        assert_eq!(lerp_rect(from, to, 0.0), from);
        assert_eq!(lerp_rect(from, to, 1.0), to); // exact at the end
        let mid = lerp_rect(from, to, 0.5);
        assert_eq!(mid, Rect::new(5, 3, 22, 11)); // halfway, rounded
    }

    #[test]
    fn focus_target_resolves_per_kind() {
        // Each kind is just a TileId looked up in its own solver's output.
        let tiling = [(1u64, Rect::new(0, 0, 10, 5)), (2, Rect::new(10, 0, 10, 5))];
        let floats = [(7u64, Rect::new(2, 2, 6, 3))];
        let nodes = [(9u64, Rect::new(40, 40, 8, 4))];
        assert_eq!(FocusTarget::Tile(2).resolve(&tiling), Some(Rect::new(10, 0, 10, 5)));
        assert_eq!(FocusTarget::Float(7).resolve(&floats), Some(Rect::new(2, 2, 6, 3)));
        assert_eq!(FocusTarget::Node(9).resolve(&nodes), Some(Rect::new(40, 40, 8, 4)));
        assert_eq!(FocusTarget::Tile(2).id(), 2);
        assert_eq!(FocusTarget::Node(5).resolve(&nodes), None); // absent
    }

    use proptest::prelude::*;

    proptest! {
        /// LoD selection is monotonic in area: more cells never picks a *less*
        /// detailed level, for any non-decreasing scale.
        #[test]
        fn prop_lod_monotonic_in_area(
            a1 in 0u32..2000, a2 in 0u32..2000,
            t in 1u32..200, p in 200u32..600, f in 600u32..1500,
        ) {
            let scale = LodScale { titled: t, ported: p, full: f };
            let (lo, hi) = (a1.min(a2), a1.max(a2));
            prop_assert!(Lod::for_area(lo, scale) <= Lod::for_area(hi, scale));
        }

        /// Easing a zoom's progress up never shrinks the focus weight (monotone) and
        /// always lands within `[1, 1 + max]`.
        #[test]
        fn prop_zoom_weight_monotone(steps in prop::collection::vec(0.0f32..1.0, 1..20), max in 1u16..800) {
            let mut z = Zoom::new(1, max);
            let mut sorted = steps.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut prev = 1u16;
            for p in sorted {
                z.set_progress(p);
                let w = z.weight(1);
                prop_assert!(w >= prev, "weight dropped as progress rose");
                prop_assert!((1..=1 + max).contains(&w));
                prev = w;
            }
        }
    }
}
