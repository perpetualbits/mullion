// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Layered (Sugiyama) auto-layout — the automatic half of placement (design note
//! §5.4). Arranges a directed graph into layers along the dataflow direction and
//! orders within each layer to cut edge crossings, writing the result back into a
//! [`GraphCanvas`] so manual placement (Phase 7) and auto-layout share one position
//! model.
//!
//! The pipeline (the dagre / Graphviz-`dot` / ELK-layered family):
//!
//! 1. **Break cycles** — a DFS feedback-arc-set: edges that close a cycle (DFS back
//!    edges) are dropped from the layering DAG (still drawn, just not constraining
//!    layers), so the rest is acyclic.
//! 2. **Assign layers** — longest-path layering on the forward DAG: every forward
//!    edge then points from a lower layer to a higher one.
//! 3. **Order within layers** — repeated up/down **barycenter** sweeps, keeping the
//!    ordering with the fewest [`crossings`].
//! 4. **Assign coordinates** — layer index → main axis, order → cross axis, snapped
//!    to the grid.
//!
//! At terminal scale (dozens of nodes) this skips dummy/virtual nodes for long
//! edges: crossing reduction is over adjacent-layer edges, and the actual wires are
//! drawn by the connector router (Phase 8).

use std::collections::{HashMap, HashSet, VecDeque};

use crate::float::FloatRect;
use crate::graph::GraphCanvas;
use crate::layout::TileId;

// ── Parameters ───────────────────────────────────────────────────────────────

/// Which way the layers run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerDir {
    /// Layers left-to-right; nodes stack top-to-bottom within a layer.
    LeftRight,
    /// Layers top-to-bottom; nodes stack left-to-right within a layer.
    TopDown,
}

/// Tunables for [`auto_layout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SugiyamaParams {
    /// The dataflow direction.
    pub dir: LayerDir,
    /// Cells of gap between adjacent layers (along the main axis).
    pub layer_gap: u16,
    /// Cells of gap between nodes within a layer (along the cross axis).
    pub node_gap: u16,
    /// Grid step that final positions snap to (≥ 1).
    pub grid: u16,
}

impl Default for SugiyamaParams {
    fn default() -> Self {
        Self { dir: LayerDir::LeftRight, layer_gap: 6, node_gap: 2, grid: 1 }
    }
}

// ── 1. Cycle breaking (feedback arc set) ───────────────────────────────────────

/// The DFS **back edges** of `edges` — a feedback arc set whose removal makes the
/// graph acyclic. Deterministic for a given node and edge order.
fn back_edges(nodes: &[TileId], edges: &[(TileId, TileId)]) -> HashSet<(TileId, TileId)> {
    let mut adj: HashMap<TileId, Vec<TileId>> = nodes.iter().map(|&n| (n, Vec::new())).collect();
    for &(u, v) in edges {
        adj.entry(u).or_default().push(v);
        adj.entry(v).or_default();
    }
    // 0 = unvisited, 1 = on the current DFS stack (gray), 2 = finished (black).
    let mut color: HashMap<TileId, u8> = adj.keys().map(|&n| (n, 0u8)).collect();
    let mut back = HashSet::new();

    for &start in nodes {
        if color[&start] != 0 {
            continue;
        }
        color.insert(start, 1);
        let mut stack: Vec<(TileId, usize)> = vec![(start, 0)];
        while let Some(&(u, i)) = stack.last() {
            if let Some(&v) = adj.get(&u).and_then(|vs| vs.get(i)) {
                stack.last_mut().unwrap().1 += 1;
                match color.get(&v).copied().unwrap_or(0) {
                    0 => {
                        color.insert(v, 1);
                        stack.push((v, 0));
                    }
                    1 => {
                        back.insert((u, v)); // edge into an ancestor on the stack
                    }
                    _ => {}
                }
            } else {
                color.insert(u, 2);
                stack.pop();
            }
        }
    }
    back
}

// ── 2. Layer assignment (longest path) ─────────────────────────────────────────

/// Assign each node a layer index so every **forward** edge (cycle-closing back
/// edges excluded) runs from a lower layer to a higher one.
///
/// Longest-path layering: a node's layer is the longest forward path reaching it,
/// so sources sit at layer 0. On a DAG every edge satisfies `layer(to) > layer(from)`.
pub fn assign_layers(nodes: &[TileId], edges: &[(TileId, TileId)]) -> HashMap<TileId, u32> {
    let back = back_edges(nodes, edges);
    let mut out: HashMap<TileId, Vec<TileId>> = nodes.iter().map(|&n| (n, Vec::new())).collect();
    let mut indeg: HashMap<TileId, usize> = nodes.iter().map(|&n| (n, 0)).collect();
    for &(u, v) in edges {
        if back.contains(&(u, v)) || u == v {
            continue;
        }
        out.entry(u).or_default().push(v);
        *indeg.entry(v).or_insert(0) += 1;
        indeg.entry(u).or_insert(0);
    }

    let mut layer: HashMap<TileId, u32> = nodes.iter().map(|&n| (n, 0)).collect();
    let mut queue: VecDeque<TileId> =
        nodes.iter().copied().filter(|n| indeg[n] == 0).collect();
    while let Some(u) = queue.pop_front() {
        let lu = layer[&u];
        for &v in out.get(&u).into_iter().flatten() {
            if layer[&v] < lu + 1 {
                layer.insert(v, lu + 1);
            }
            let d = indeg.get_mut(&v).unwrap();
            *d -= 1;
            if *d == 0 {
                queue.push_back(v);
            }
        }
    }
    layer
}

// ── 3. Ordering within layers (barycenter) ─────────────────────────────────────

/// Group `nodes` into layers (by [`assign_layers`]) and order each layer to reduce
/// crossings: repeated up/down **barycenter** sweeps, keeping the best ordering seen
/// (so the result never has more [`crossings`] than the initial by-id order).
pub fn order_layers(nodes: &[TileId], edges: &[(TileId, TileId)]) -> Vec<Vec<TileId>> {
    let layer = assign_layers(nodes, edges);
    let n_layers = layer.values().copied().max().map(|m| m as usize + 1).unwrap_or(0);
    if n_layers == 0 {
        return Vec::new();
    }

    // Initial order: by id within each layer — deterministic, so the whole pass is
    // idempotent (it never reads node positions).
    let mut order: Vec<Vec<TileId>> = vec![Vec::new(); n_layers];
    let mut by_id = nodes.to_vec();
    by_id.sort_unstable();
    for n in by_id {
        order[layer[&n] as usize].push(n);
    }

    let neighbors = neighbors_of(nodes, edges);
    let mut best = order.clone();
    let mut best_cross = crossings(&order, edges);

    for it in 0..8 {
        let down = it % 2 == 0;
        sweep(&mut order, &neighbors, down);
        let c = crossings(&order, edges);
        if c < best_cross {
            best_cross = c;
            best = order.clone();
        }
    }
    best
}

/// Undirected neighbour lists (both edge directions).
fn neighbors_of(nodes: &[TileId], edges: &[(TileId, TileId)]) -> HashMap<TileId, Vec<TileId>> {
    let mut m: HashMap<TileId, Vec<TileId>> = nodes.iter().map(|&n| (n, Vec::new())).collect();
    for &(u, v) in edges {
        if u == v {
            continue;
        }
        m.entry(u).or_default().push(v);
        m.entry(v).or_default().push(u);
    }
    m
}

/// One barycenter sweep. `down` reorders each layer by its predecessor layer (top
/// to bottom); otherwise by its successor layer (bottom to top). A node with no
/// neighbours in the fixed layer keeps its current position.
fn sweep(order: &mut [Vec<TileId>], neighbors: &HashMap<TileId, Vec<TileId>>, down: bool) {
    let n = order.len();
    let layers: Vec<usize> =
        if down { (1..n).collect() } else { (0..n.saturating_sub(1)).rev().collect() };
    for l in layers {
        let fixed = if down { l - 1 } else { l + 1 };
        let pos: HashMap<TileId, usize> =
            order[fixed].iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let mut keyed: Vec<(f32, usize, TileId)> = order[l]
            .iter()
            .enumerate()
            .map(|(cur, &id)| {
                let ps: Vec<usize> = neighbors
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .filter_map(|m| pos.get(m).copied())
                    .collect();
                let bc = if ps.is_empty() {
                    cur as f32
                } else {
                    ps.iter().sum::<usize>() as f32 / ps.len() as f32
                };
                (bc, cur, id)
            })
            .collect();
        // Sort by barycenter, breaking ties by current index (a stable nudge).
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
        order[l] = keyed.into_iter().map(|(_, _, id)| id).collect();
    }
}

/// Count edge crossings of an ordering: for each pair of **adjacent-layer** edges
/// sharing a layer gap, a crossing is an inversion (one edge's upper endpoint is
/// left of the other's while its lower endpoint is right). Long edges (spanning more
/// than one layer) are not counted.
pub fn crossings(order: &[Vec<TileId>], edges: &[(TileId, TileId)]) -> usize {
    let mut pos: HashMap<TileId, (usize, usize)> = HashMap::new();
    for (l, layer) in order.iter().enumerate() {
        for (i, &id) in layer.iter().enumerate() {
            pos.insert(id, (l, i));
        }
    }
    // Bucket each adjacent-layer edge as (upper index, lower index) by its gap.
    let mut by_gap: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    for &(a, b) in edges {
        let (Some(&(la, ia)), Some(&(lb, ib))) = (pos.get(&a), pos.get(&b)) else {
            continue;
        };
        if la.abs_diff(lb) != 1 {
            continue;
        }
        let (gap, up, lo) = if la < lb { (la, ia, ib) } else { (lb, ib, ia) };
        by_gap.entry(gap).or_default().push((up, lo));
    }
    let mut total = 0;
    for es in by_gap.values() {
        for i in 0..es.len() {
            for j in (i + 1)..es.len() {
                let ((u1, l1), (u2, l2)) = (es[i], es[j]);
                if (u1 < u2 && l1 > l2) || (u1 > u2 && l1 < l2) {
                    total += 1;
                }
            }
        }
    }
    total
}

// ── 4. Coordinates + write-back ────────────────────────────────────────────────

/// Lay the canvas's nodes out with Sugiyama and write the grid-snapped positions
/// back via [`GraphCanvas::move_to`]. `edges` are directed `(from, to)` pairs over
/// the node ids; the canvas is resized to fit the result.
///
/// Idempotent: the layout is a function of the node ids, their sizes, and `edges` —
/// not of current positions — so running it again reproduces the same placement.
pub fn auto_layout(canvas: &mut GraphCanvas, edges: &[(TileId, TileId)], params: &SugiyamaParams) {
    let nodes: Vec<TileId> = canvas.nodes().iter().map(|n| n.id).collect();
    if nodes.is_empty() {
        return;
    }
    let size: HashMap<TileId, (u16, u16)> = canvas
        .nodes()
        .iter()
        .map(|n| (n.id, (n.place.width, n.place.height)))
        .collect();
    let order = order_layers(&nodes, edges);
    let g = params.grid.max(1);
    let snap = |v: u16| ((v + g / 2) / g) * g;
    let main = |id: TileId| {
        let (w, h) = size[&id];
        if params.dir == LayerDir::LeftRight { w } else { h }
    };
    let cross = |id: TileId| {
        let (w, h) = size[&id];
        if params.dir == LayerDir::LeftRight { h } else { w }
    };

    // Main-axis position of each layer: cumulative max node extent + gap.
    let mut layer_main = vec![0u16; order.len()];
    let mut acc = 0u16;
    for (l, layer) in order.iter().enumerate() {
        layer_main[l] = snap(acc);
        let widest = layer.iter().map(|&id| main(id)).max().unwrap_or(0);
        acc = acc.saturating_add(widest).saturating_add(params.layer_gap);
    }

    // Assign and collect positions, then resize the canvas to fit, then write.
    let mut placed: Vec<(TileId, FloatRect)> = Vec::with_capacity(nodes.len());
    let (mut ext_x, mut ext_y) = (0u16, 0u16);
    for (l, layer) in order.iter().enumerate() {
        let mut c = 0u16;
        for &id in layer {
            let (w, h) = size[&id];
            let (x, y) = if params.dir == LayerDir::LeftRight {
                (layer_main[l], snap(c))
            } else {
                (snap(c), layer_main[l])
            };
            placed.push((id, FloatRect::new(x, y, w, h)));
            ext_x = ext_x.max(x.saturating_add(w));
            ext_y = ext_y.max(y.saturating_add(h));
            c = c.saturating_add(cross(id)).saturating_add(params.node_gap);
        }
    }

    let (cw, ch) = canvas.size();
    canvas.resize(cw.max(ext_x), ch.max(ext_y));
    for (id, r) in placed {
        canvas.move_to(id, r.x, r.y);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A small DAG pipeline: 1→{2,3}, {2,3}→4, 4→{5,6}, {5,6}→7.
    fn dag() -> (Vec<TileId>, Vec<(TileId, TileId)>) {
        (
            vec![1, 2, 3, 4, 5, 6, 7],
            vec![(1, 2), (1, 3), (2, 4), (3, 4), (4, 5), (4, 6), (5, 7), (6, 7)],
        )
    }

    /// Group nodes into layers by id order (the pre-ordering baseline).
    fn by_id_layers(nodes: &[TileId], edges: &[(TileId, TileId)]) -> Vec<Vec<TileId>> {
        let layer = assign_layers(nodes, edges);
        let n = layer.values().copied().max().map(|m| m as usize + 1).unwrap_or(0);
        let mut order = vec![Vec::new(); n];
        let mut ids = nodes.to_vec();
        ids.sort_unstable();
        for id in ids {
            order[layer[&id] as usize].push(id);
        }
        order
    }

    #[test]
    fn dag_edges_point_to_higher_layers() {
        let (nodes, edges) = dag();
        let layer = assign_layers(&nodes, &edges);
        for &(u, v) in &edges {
            assert!(layer[&v] > layer[&u], "edge {u}->{v}: {} -> {}", layer[&u], layer[&v]);
        }
        assert_eq!(layer[&1], 0);
        assert_eq!(layer[&4], 2);
        assert_eq!(layer[&7], 4);
    }

    #[test]
    fn cycles_are_broken() {
        // Add a back edge 7→1; layering must still succeed and stay finite.
        let (nodes, mut edges) = dag();
        edges.push((7, 1));
        let back = back_edges(&nodes, &edges);
        assert!(back.contains(&(7, 1)), "the cycle-closing edge is the feedback arc");
        let layer = assign_layers(&nodes, &edges);
        // Forward edges still go uphill; the back edge is excluded from layering.
        for &(u, v) in &edges {
            if !back.contains(&(u, v)) {
                assert!(layer[&v] > layer[&u]);
            }
        }
    }

    #[test]
    fn barycenter_never_increases_crossings() {
        // A graph whose by-id order crosses, but a reordering does not.
        let nodes = vec![1, 2, 3, 4];
        let edges = vec![(1, 4), (2, 3)]; // 1,2 in layer 0; 3,4 in layer 1
        let before = crossings(&by_id_layers(&nodes, &edges), &edges);
        let after = crossings(&order_layers(&nodes, &edges), &edges);
        assert!(after <= before, "crossings rose: {before} -> {after}");
    }

    #[test]
    fn auto_layout_is_idempotent_and_layered() {
        use crate::FloatRect;
        let (nodes, edges) = dag();
        let mut canvas = GraphCanvas::new(200, 80);
        for (i, &id) in nodes.iter().enumerate() {
            // Scatter the nodes; layout must not depend on these positions.
            canvas.add(id, FloatRect::new((i as u16 * 7) % 40, (i as u16 * 13) % 30, 10, 4));
        }
        let params = SugiyamaParams { grid: 2, ..Default::default() };
        auto_layout(&mut canvas, &edges, &params);
        let first: Vec<_> = canvas.nodes().iter().map(|n| (n.id, n.place)).collect();
        auto_layout(&mut canvas, &edges, &params);
        let second: Vec<_> = canvas.nodes().iter().map(|n| (n.id, n.place)).collect();
        assert_eq!(first, second, "auto_layout is idempotent");

        // Layered: a successor sits strictly to the right of its predecessor.
        let x = |id: TileId| canvas.place(id).unwrap().x;
        for &(u, v) in &edges {
            assert!(x(v) > x(u), "edge {u}->{v} not left-to-right: {} vs {}", x(u), x(v));
        }
        // Grid-snapped to a multiple of 2.
        for n in canvas.nodes() {
            assert_eq!(n.place.x % 2, 0);
            assert_eq!(n.place.y % 2, 0);
        }
    }

    use proptest::prelude::*;

    proptest! {
        /// On any DAG (edges only ever go from a lower-id to a higher-id node, so it
        /// is acyclic), every edge points from a lower layer to a higher one.
        #[test]
        fn prop_dag_layered_uphill(
            pairs in prop::collection::vec((0u64..8, 0u64..8), 0..20),
        ) {
            let nodes: Vec<TileId> = (0..8).collect();
            // Force acyclicity: orient every edge low-id → high-id, drop self-loops.
            let edges: Vec<(TileId, TileId)> = pairs
                .into_iter()
                .filter(|(a, b)| a != b)
                .map(|(a, b)| (a.min(b), a.max(b)))
                .collect();
            let layer = assign_layers(&nodes, &edges);
            for &(u, v) in &edges {
                prop_assert!(layer[&v] > layer[&u]);
            }
        }

        /// Ordering never makes crossings worse than the by-id baseline.
        #[test]
        fn prop_ordering_never_worse(
            pairs in prop::collection::vec((0u64..7, 0u64..7), 0..16),
        ) {
            let nodes: Vec<TileId> = (0..7).collect();
            let edges: Vec<(TileId, TileId)> = pairs
                .into_iter()
                .filter(|(a, b)| a != b)
                .map(|(a, b)| (a.min(b), a.max(b)))
                .collect();
            let before = crossings(&by_id_layers(&nodes, &edges), &edges);
            let after = crossings(&order_layers(&nodes, &edges), &edges);
            prop_assert!(after <= before);
        }
    }
}
