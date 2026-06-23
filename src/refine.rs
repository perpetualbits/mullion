// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Layout **quality score** and a local-search **refiner** — a sketch of a
//! learnable layout engine (no machine learning required).
//!
//! A layout's quality is a weighted sum of measurable aesthetic criteria (the
//! classic graph-drawing metrics): edge **crossings**, total edge **length**,
//! bounding-box **area** (compactness), column/row **alignment**, and a hard
//! **overlap** penalty. [`score`] computes them over a [`GraphCanvas`]; lower is
//! better. Several terms are things we already measure elsewhere (crossings cf.
//! [`sugiyama::crossings`](crate::sugiyama::crossings); length is the connector
//! budget).
//!
//! [`refine`] then does greedy **hill-climbing**: it repeatedly swaps the positions
//! of two nodes and keeps the swap only when it lowers the score, so the score
//! **never increases**. Run it after [`auto_layout`](crate::sugiyama::auto_layout)
//! to polish, or on any hand-placed layout.
//!
//! The point of the explicit, weighted score is that it is **tunable** — and the
//! weights can be *learned* from a human's corrections: because manual placement and
//! auto-layout share one `GraphCanvas`, a drag-improved layout B versus the machine's
//! A is a free preference pair (`B ≻ A`) to fit the weights to a user's taste. That
//! learning layer is out of scope here; this is the deterministic scaffolding it
//! would sit on.

use std::collections::HashMap;

use crate::graph::GraphCanvas;
use crate::layout::TileId;

/// How much each aesthetic term counts toward the [`score`]. Lower total is better;
/// every weight is ≥ 0. These are the knobs a preference-learner would fit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreWeights {
    /// Per edge **crossing** (usually the dominant term).
    pub crossings: f32,
    /// Per cell of total straight-line edge **length**.
    pub length: f32,
    /// Per cell² of bounding-box **area** (compactness).
    pub area: f32,
    /// Per distinct node column/row — fewer means better **alignment**.
    pub alignment: f32,
    /// Per overlapping node pair — a hard penalty (keep it large).
    pub overlap: f32,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self { crossings: 20.0, length: 0.05, area: 0.002, alignment: 0.4, overlap: 1000.0 }
    }
}

/// A scored layout: the weighted `total` (lower is better) plus the raw terms, so a
/// caller (or a learner) can see *why* one layout beats another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutScore {
    /// The weighted sum — the figure of merit.
    pub total: f32,
    /// Number of edge crossings (straight node-centre segments).
    pub crossings: usize,
    /// Total straight-line edge length, in cells.
    pub length: f32,
    /// Bounding-box area of all nodes, in cells².
    pub area: f32,
    /// Distinct node columns + rows (a proxy for grid alignment).
    pub alignment: usize,
    /// Number of overlapping node pairs (should be 0).
    pub overlap: usize,
}

impl LayoutScore {
    /// Re-weight the raw terms under `w` — the figure of merit for a given set of
    /// weights, without recomputing from a canvas. Useful for ranking a stored
    /// score under learned weights (see [`learn_weights`]).
    pub fn weighted(&self, w: ScoreWeights) -> f32 {
        w.crossings * self.crossings as f32
            + w.length * self.length
            + w.area * self.area
            + w.alignment * self.alignment as f32
            + w.overlap * self.overlap as f32
    }
}

/// Score `canvas`'s layout under `weights` (lower is better). `edges` are directed
/// `(from, to)` pairs; only the geometry of the node rectangles is used.
pub fn score(canvas: &GraphCanvas, edges: &[(TileId, TileId)], weights: ScoreWeights) -> LayoutScore {
    let nodes: Vec<(TileId, crate::geometry::Rect)> = canvas
        .nodes()
        .iter()
        .map(|n| (n.id, crate::geometry::Rect::new(n.place.x, n.place.y, n.place.width, n.place.height)))
        .collect();
    let centre: HashMap<TileId, (f32, f32)> = nodes
        .iter()
        .map(|&(id, r)| (id, (r.x as f32 + r.width as f32 / 2.0, r.y as f32 + r.height as f32 / 2.0)))
        .collect();

    // Crossings: each pair of edges that do not share a node and whose centre
    // segments properly intersect.
    let mut crossings = 0;
    for i in 0..edges.len() {
        for j in (i + 1)..edges.len() {
            let (a, b) = edges[i];
            let (c, d) = edges[j];
            if a == c || a == d || b == c || b == d {
                continue;
            }
            if let (Some(&pa), Some(&pb), Some(&pc), Some(&pd)) =
                (centre.get(&a), centre.get(&b), centre.get(&c), centre.get(&d))
            {
                if segments_cross(pa, pb, pc, pd) {
                    crossings += 1;
                }
            }
        }
    }

    // Total edge length.
    let length: f32 = edges
        .iter()
        .filter_map(|(a, b)| Some((centre.get(a)?, centre.get(b)?)))
        .map(|(p, q)| ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt())
        .sum();

    // Bounding-box area.
    let area = if nodes.is_empty() {
        0.0
    } else {
        let (mut x0, mut y0, mut x1, mut y1) = (u16::MAX, u16::MAX, 0u16, 0u16);
        for &(_, r) in &nodes {
            x0 = x0.min(r.x);
            y0 = y0.min(r.y);
            x1 = x1.max(r.right());
            y1 = y1.max(r.bottom());
        }
        (x1 - x0) as f32 * (y1 - y0) as f32
    };

    // Alignment: how many distinct columns (x) and rows (y) the nodes occupy.
    let cols: std::collections::HashSet<u16> = nodes.iter().map(|&(_, r)| r.x).collect();
    let rows: std::collections::HashSet<u16> = nodes.iter().map(|&(_, r)| r.y).collect();
    let alignment = cols.len() + rows.len();

    // Overlap: node-rectangle pairs whose interiors intersect.
    let mut overlap = 0;
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            if !nodes[i].1.intersection(nodes[j].1).is_empty() {
                overlap += 1;
            }
        }
    }

    let mut s = LayoutScore { total: 0.0, crossings, length, area, alignment, overlap };
    s.total = s.weighted(weights);
    s
}

/// Polish `canvas` by greedy **hill-climbing** on the [`score`]: repeatedly try
/// swapping the positions of two nodes and keep a swap only when it lowers the
/// score. The score **never increases**; returns `(before, after)` totals.
///
/// Converges to a local optimum (a swap-stable layout); run it after
/// [`auto_layout`](crate::sugiyama::auto_layout) to refine, or on a hand-placed
/// graph. `max_passes` bounds the work (each pass is `O(nodes² · edges²)`).
pub fn refine(
    canvas: &mut GraphCanvas,
    edges: &[(TileId, TileId)],
    weights: ScoreWeights,
    max_passes: usize,
) -> (f32, f32) {
    let before = score(canvas, edges, weights).total;
    let ids: Vec<TileId> = canvas.nodes().iter().map(|n| n.id).collect();
    let mut current = before;

    for _ in 0..max_passes {
        let mut improved = false;
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                swap_positions(canvas, ids[i], ids[j]);
                let s = score(canvas, edges, weights).total;
                if s < current - f32::EPSILON {
                    current = s; // keep the swap
                    improved = true;
                } else {
                    swap_positions(canvas, ids[i], ids[j]); // revert
                }
            }
        }
        if !improved {
            break; // swap-stable: a local optimum
        }
    }
    (before, current)
}

// ── Learning the weights from preferences ──────────────────────────────────────

/// One **preference**: a human improved `worse` into `better`, so `better ≻ worse`.
/// Each side is a layout's raw [`LayoutScore`] terms (the `total` is ignored — the
/// weights are what we are fitting). A drag-correction yields one of these for free,
/// since manual placement and auto-layout share a [`GraphCanvas`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preference {
    /// The layout the human moved *away* from.
    pub worse: LayoutScore,
    /// The layout the human moved *toward* (preferred).
    pub better: LayoutScore,
}

impl Preference {
    /// A preference from two layouts: `better` is preferred over `worse`. (Scored
    /// with default weights, since only the weight-independent raw terms are used.)
    pub fn from_layouts(worse: &GraphCanvas, better: &GraphCanvas, edges: &[(TileId, TileId)]) -> Self {
        let w = ScoreWeights::default();
        Self { worse: score(worse, edges, w), better: score(better, edges, w) }
    }
}

/// The four learnable (soft) terms of a score, as a vector. `overlap` is a hard
/// constraint, not learned.
fn terms(s: &LayoutScore) -> [f32; 4] {
    [s.crossings as f32, s.length, s.area, s.alignment as f32]
}

/// Fit [`ScoreWeights`] from `prefs` so preferred layouts score lower — **logistic
/// preference learning**: project each preference to the difference of its layouts'
/// terms and fit a (non-negative) weight vector by gradient descent so that
/// `score(worse) > score(better)`.
///
/// This is the "train by showing improvements" step: a handful of drag-corrections
/// teach the engine *how much each aesthetic criterion matters to you*. The result
/// keeps the default `overlap` penalty (a hard constraint) and learns the four soft
/// weights. `iters`/`learn_rate` control the fit (e.g. 400, 0.5). Returns the
/// defaults if `prefs` is empty.
pub fn learn_weights(prefs: &[Preference], iters: usize, learn_rate: f32) -> ScoreWeights {
    if prefs.is_empty() {
        return ScoreWeights::default();
    }
    // Per-term scale (mean magnitude over all layouts) so the terms — which span
    // wildly different ranges — are comparable during the fit.
    let mut scale = [0.0f32; 4];
    for p in prefs {
        for s in [&p.worse, &p.better] {
            let f = terms(s);
            for k in 0..4 {
                scale[k] += f[k].abs();
            }
        }
    }
    for s in &mut scale {
        *s = (*s / (prefs.len() as f32 * 2.0)).max(1e-6);
    }

    // Difference vectors d = (worse − better), scaled. We want w·d > 0 for each.
    let diffs: Vec<[f32; 4]> = prefs
        .iter()
        .map(|p| {
            let (fw, fb) = (terms(&p.worse), terms(&p.better));
            let mut d = [0.0f32; 4];
            for k in 0..4 {
                d[k] = (fw[k] - fb[k]) / scale[k];
            }
            d
        })
        .collect();

    let mut w = [1.0f32; 4]; // start equal; non-negative throughout
    let lambda = 1e-3; // L2 regularisation keeps it bounded
    for _ in 0..iters {
        let mut grad = [0.0f32; 4];
        for d in &diffs {
            let z: f32 = (0..4).map(|k| w[k] * d[k]).sum();
            let sig = 1.0 / (1.0 + z.exp()); // sigmoid(−z) = P(misordered)
            for k in 0..4 {
                grad[k] -= d[k] * sig;
            }
        }
        for k in 0..4 {
            grad[k] = grad[k] / diffs.len() as f32 + lambda * w[k];
            w[k] = (w[k] - learn_rate * grad[k]).max(0.0);
        }
    }

    // Convert scaled weights back to raw-term weights (raw = scaled / scale).
    ScoreWeights {
        crossings: w[0] / scale[0],
        length: w[1] / scale[1],
        area: w[2] / scale[2],
        alignment: w[3] / scale[3],
        overlap: ScoreWeights::default().overlap,
    }
}

/// Swap the canvas positions of nodes `a` and `b` (a no-op if either is absent).
fn swap_positions(canvas: &mut GraphCanvas, a: TileId, b: TileId) {
    if let (Some(ra), Some(rb)) = (canvas.place(a), canvas.place(b)) {
        canvas.move_to(a, rb.x, rb.y);
        canvas.move_to(b, ra.x, ra.y);
    }
}

/// Twice the signed area of triangle `(a, b, c)` — its sign is the turn direction.
fn cross(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// Whether segments `p1p2` and `p3p4` **properly** cross (interiors intersect; a
/// shared endpoint or collinear touch does not count).
fn segments_cross(p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), p4: (f32, f32)) -> bool {
    let d1 = cross(p3, p4, p1);
    let d2 = cross(p3, p4, p2);
    let d3 = cross(p1, p2, p3);
    let d4 = cross(p1, p2, p4);
    (d1 * d2 < 0.0) && (d3 * d4 < 0.0)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FloatRect;

    /// Four nodes in two columns; edges 1→4 and 2→3 cross as an X.
    fn crossed() -> (GraphCanvas, Vec<(TileId, TileId)>) {
        let mut c = GraphCanvas::new(60, 30);
        c.add(1, FloatRect::new(0, 0, 8, 4)); // top-left
        c.add(2, FloatRect::new(0, 12, 8, 4)); // bottom-left
        c.add(3, FloatRect::new(30, 0, 8, 4)); // top-right
        c.add(4, FloatRect::new(30, 12, 8, 4)); // bottom-right
        (c, vec![(1, 4), (2, 3)])
    }

    #[test]
    fn score_is_deterministic() {
        let (c, e) = crossed();
        let w = ScoreWeights::default();
        assert_eq!(score(&c, &e, w), score(&c, &e, w));
    }

    #[test]
    fn the_x_layout_has_one_crossing() {
        let (c, e) = crossed();
        let s = score(&c, &e, ScoreWeights::default());
        assert_eq!(s.crossings, 1);
        assert_eq!(s.overlap, 0);
    }

    #[test]
    fn refine_uncrosses_the_x() {
        // Swapping nodes 3 and 4 turns the X into two parallel horizontal edges.
        let (mut c, e) = crossed();
        let w = ScoreWeights::default();
        let (before, after) = refine(&mut c, &e, w, 8);
        assert!(after < before, "score improved: {before} → {after}");
        assert_eq!(score(&c, &e, w).crossings, 0, "the crossing is gone");
        assert_eq!(score(&c, &e, w).overlap, 0, "no overlap introduced");
    }

    #[test]
    fn refine_is_idempotent_at_a_local_optimum() {
        let (mut c, e) = crossed();
        let w = ScoreWeights::default();
        refine(&mut c, &e, w, 8);
        let settled = score(&c, &e, w).total;
        let (before, after) = refine(&mut c, &e, w, 8); // second run: nothing to do
        assert_eq!(before, settled);
        assert_eq!(after, settled);
    }

    // ── Learning the weights ──────────────────────────────────────────────

    /// A bare score with the given soft terms (overlap 0, total unset).
    fn ls(crossings: usize, length: f32, area: f32, alignment: usize) -> LayoutScore {
        LayoutScore { total: 0.0, crossings, length, area, alignment, overlap: 0 }
    }

    #[test]
    fn empty_prefs_keep_defaults() {
        assert_eq!(learn_weights(&[], 100, 0.5), ScoreWeights::default());
    }

    #[test]
    fn learning_is_deterministic() {
        let prefs = vec![Preference { worse: ls(3, 200.0, 2000.0, 12), better: ls(0, 180.0, 2000.0, 9) }];
        assert_eq!(learn_weights(&prefs, 200, 0.5), learn_weights(&prefs, 200, 0.5));
    }

    #[test]
    fn learns_crossings_outrank_length() {
        // A human who always prefers FEWER crossings — even when it costs length.
        let prefs: Vec<Preference> = (0..15)
            .map(|i| Preference {
                worse: ls(4, 150.0 + i as f32, 2000.0, 10), // crossy but short
                better: ls(1, 250.0 + i as f32, 2000.0, 10), // clean but longer
            })
            .collect();
        let w = learn_weights(&prefs, 600, 0.5);
        // Learned weights rank every training pair the way the human did…
        for p in &prefs {
            assert!(p.better.weighted(w) < p.worse.weighted(w));
        }
        // …and decide a *fresh* crossings-vs-length tradeoff in favour of fewer.
        let crossy_short = ls(3, 100.0, 2000.0, 10);
        let clean_long = ls(0, 320.0, 2000.0, 10);
        assert!(clean_long.weighted(w) < crossy_short.weighted(w));
        // All learned weights are non-negative (every term is a cost).
        for v in [w.crossings, w.length, w.area, w.alignment] {
            assert!(v >= 0.0);
        }
    }

    #[test]
    fn learns_the_opposite_taste_too() {
        // The mirror: a human who prefers SHORTER wires even at the cost of a crossing.
        let prefs: Vec<Preference> = (0..15)
            .map(|i| Preference {
                worse: ls(1, 300.0 + i as f32, 2000.0, 10), // clean but long
                better: ls(4, 150.0 + i as f32, 2000.0, 10), // crossy but short
            })
            .collect();
        let w = learn_weights(&prefs, 600, 0.5);
        let clean_long = ls(0, 320.0, 2000.0, 10);
        let crossy_short = ls(3, 120.0, 2000.0, 10);
        assert!(crossy_short.weighted(w) < clean_long.weighted(w), "this taste prefers short");
    }

    use proptest::prelude::*;

    proptest! {
        /// Refining never increases the score, for any starting layout / edge set.
        #[test]
        fn prop_refine_never_worsens(
            pos in prop::collection::vec((0u16..50, 0u16..24), 5),
            pairs in prop::collection::vec((0usize..5, 0usize..5), 0..8),
        ) {
            let mut c = GraphCanvas::new(64, 32);
            for (i, &(x, y)) in pos.iter().enumerate() {
                c.add(i as TileId + 1, FloatRect::new(x, y, 6, 3));
            }
            let edges: Vec<(TileId, TileId)> = pairs
                .into_iter()
                .filter(|(a, b)| a != b)
                .map(|(a, b)| (a as TileId + 1, b as TileId + 1))
                .collect();
            let w = ScoreWeights::default();
            let (before, after) = refine(&mut c, &edges, w, 6);
            prop_assert!(after <= before + f32::EPSILON);
        }
    }
}
