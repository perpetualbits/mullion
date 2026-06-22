// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Orthogonal connector routing: wire socket to socket with grid A\* over the
//! free-cell structure (design note §5.2).
//!
//! Orthogonal connector routing is a known-hard problem (libavoid, ELK), but
//! terminal scale rescues us — dozens of connectors over an ~80×200 grid, not a
//! 10k-net PCB. The approach:
//!
//! - **Grid A\*** over the free cells (from [`crate::float::free_cells_in_window`]),
//!   with a **bend penalty** so the search prefers long straight channels and few
//!   corners — the "train tracks / tin lines on a board" look.
//! - Route in **canvas space**, so routes are stable under future scrolling and
//!   recomputed on graph *edits*, not on camera motion. Callers reroute every
//!   frame; at this scale that is fine.
//!
//! Rendering reuses the junction glyph logic ([`crate::junction`]): each route is
//! laid into an [`EdgeGrid`] as box-drawing arms, so turns — and, where two
//! connectors cross, junctions — resolve to the right glyph automatically. There
//! is no hop-over glyph (single-line box drawing has none), so a crossing that is
//! not a join is disambiguated by **colour-per-net** ([`render`]) and biased away
//! by a crossing penalty, while parallel wires are spread onto separate tracks by
//! **nudging** ([`route_all`]).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::border::LineWeight;
use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::junction::{resolve, EdgeGrid};
use crate::style::Style;
use crate::tree::Direction;

/// The four orthogonal moves.
const DIRS: [Direction; 4] = [Direction::Up, Direction::Down, Direction::Left, Direction::Right];

/// An A\* search state: a cell plus the direction it was entered from (`None` at
/// the start). Tracking direction is what lets the bend penalty see a turn.
type State = ((u16, u16), Option<Direction>);

/// A priority-queue entry: `(f, g, cell, dir-code)`. Ordered lexicographically by
/// `f` first, so wrapped in [`Reverse`] it pops the lowest-`f` state.
type HeapItem = (u32, u32, (u16, u16), u8);

// ── A* router ────────────────────────────────────────────────────────────────

/// Find an orthogonal path of free cells from `start` to `goal`, minimising
/// length plus `bend_penalty` per corner.
///
/// Grid A\* whose search state is `(cell, incoming direction)`: stepping in the
/// same direction costs `1`, turning costs `1 + bend_penalty`. A larger penalty
/// makes the route prefer long straight runs and few corners. The heuristic is
/// Manhattan distance (admissible — a turn only ever adds cost).
///
/// # Parameters
/// - `free`: the set of passable (free) cells. `start` and `goal` must be in it.
/// - `bend_penalty`: extra cost per corner; `0` is shortest-path, higher is
///   straighter.
///
/// # Returns
/// The path as cells from `start` to `goal` inclusive (every consecutive pair is
/// an axis-aligned unit step), or `None` if `goal` is unreachable through `free`.
pub fn route(
    free: &HashSet<(u16, u16)>,
    start: (u16, u16),
    goal: (u16, u16),
    bend_penalty: u32,
) -> Option<Vec<(u16, u16)>> {
    astar(free, start, goal, bend_penalty, &Occupancy::default(), 0)
}

/// Tracks of cells and edges already taken by routed connectors, so later wires
/// nudge clear of earlier ones (used by [`route_all`]).
///
/// `edges` are the segments taken: a wire may not run *along* an occupied edge,
/// which is what spreads parallel wires onto separate integer tracks and bounds a
/// gutter to its capacity. `cells` are the cells touched: entering one is a
/// crossing (a perpendicular pass), charged `crossing_penalty` so the search
/// prefers a crossing-free route when one is nearly as cheap.
#[derive(Debug, Default, Clone)]
struct Occupancy {
    edges: HashSet<(u16, u16, u8)>,
    cells: HashSet<(u16, u16)>,
}

impl Occupancy {
    /// Record every edge and cell of `path` as taken.
    fn add(&mut self, path: &[(u16, u16)]) {
        for seg in path.windows(2) {
            self.edges.insert(edge_key(seg[0], seg[1]));
        }
        self.cells.extend(path.iter().copied());
    }
}

/// Normalised id of the edge between adjacent cells `a` and `b`: the upper-left
/// cell plus an axis flag (`0` horizontal, `1` vertical). Two collinear wires that
/// would overlap on this edge map to the same key.
fn edge_key(a: (u16, u16), b: (u16, u16)) -> (u16, u16, u8) {
    if a.1 == b.1 {
        (a.0.min(b.0), a.1, 0)
    } else {
        (a.0, a.1.min(b.1), 1)
    }
}

/// Grid A\* with a bend penalty, honouring `occ`: an occupied edge is impassable
/// (no collinear overlap), and entering an occupied cell — a crossing — costs an
/// extra `crossing_penalty`. With an empty `occ` this is plain shortest-with-bends
/// routing (see [`route`]).
fn astar(
    free: &HashSet<(u16, u16)>,
    start: (u16, u16),
    goal: (u16, u16),
    bend_penalty: u32,
    occ: &Occupancy,
    crossing_penalty: u32,
) -> Option<Vec<(u16, u16)>> {
    if !free.contains(&start) || !free.contains(&goal) {
        return None;
    }
    if start == goal {
        return Some(vec![start]);
    }
    let h = |c: (u16, u16)| (c.0.abs_diff(goal.0) as u32) + (c.1.abs_diff(goal.1) as u32);

    let mut g_score: HashMap<State, u32> = HashMap::new();
    let mut came: HashMap<State, State> = HashMap::new();
    // Heap ordered by (f, g, cell, dir-code); Reverse makes it a min-heap.
    let mut open: BinaryHeap<Reverse<HeapItem>> = BinaryHeap::new();

    g_score.insert((start, None), 0);
    open.push(Reverse((h(start), 0, start, 0)));

    while let Some(Reverse((_f, g, cell, dcode))) = open.pop() {
        let dir = decode(dcode);
        if cell == goal {
            return Some(reconstruct(&came, (cell, dir), start));
        }
        // Skip stale heap entries superseded by a cheaper path to this state.
        if g > *g_score.get(&(cell, dir)).unwrap_or(&u32::MAX) {
            continue;
        }
        for &d in &DIRS {
            let (dx, dy) = d.delta();
            let (nx, ny) = (cell.0 as i32 + dx, cell.1 as i32 + dy);
            if nx < 0 || ny < 0 {
                continue;
            }
            let n = (nx as u16, ny as u16);
            // Blocked by a node, or the segment is already another wire's track.
            if !free.contains(&n) || occ.edges.contains(&edge_key(cell, n)) {
                continue;
            }
            // A turn is any move whose direction differs from how we entered.
            let turn = matches!(dir, Some(pd) if pd != d);
            let cross = if occ.cells.contains(&n) { crossing_penalty } else { 0 };
            let ng = g + 1 + if turn { bend_penalty } else { 0 } + cross;
            let ns = (n, Some(d));
            if ng < *g_score.get(&ns).unwrap_or(&u32::MAX) {
                g_score.insert(ns, ng);
                came.insert(ns, (cell, dir));
                open.push(Reverse((ng + h(n), ng, n, encode(Some(d)))));
            }
        }
    }
    None
}

/// Walk `came` from state `s` (the goal) back to the cell `start`, returning the
/// path in forward order.
fn reconstruct(came: &HashMap<State, State>, mut s: State, start: (u16, u16)) -> Vec<(u16, u16)> {
    let mut path = vec![s.0];
    while s.0 != start {
        s = came[&s];
        path.push(s.0);
    }
    path.reverse();
    path
}

/// Encode an optional direction as a small integer for the heap ordering.
fn encode(d: Option<Direction>) -> u8 {
    match d {
        None => 0,
        Some(Direction::Up) => 1,
        Some(Direction::Down) => 2,
        Some(Direction::Left) => 3,
        Some(Direction::Right) => 4,
    }
}

/// Inverse of [`encode`].
fn decode(c: u8) -> Option<Direction> {
    match c {
        1 => Some(Direction::Up),
        2 => Some(Direction::Down),
        3 => Some(Direction::Left),
        4 => Some(Direction::Right),
        _ => None,
    }
}

// ── Connector ────────────────────────────────────────────────────────────────

/// A routed connector: its [`path`](Connector::path) plus the directions each end
/// connects toward its socket (so the wire's endpoint glyph points into the
/// socket — the ball-into-socket join).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    /// Cells from the start socket's attach point to the goal's, in canvas
    /// coordinates. Every consecutive pair is an axis-aligned unit step.
    pub path: Vec<(u16, u16)>,
    /// Direction from `path[0]` toward its socket (i.e. the socket's *inward*
    /// direction). Used only when rendering the endpoint stub.
    pub from: Direction,
    /// Direction from the last cell toward its socket.
    pub to: Direction,
}

impl Connector {
    /// Route a connector between two socket attach cells (see
    /// [`Socket::attach`](crate::socket::Socket::attach)).
    ///
    /// `from`/`to` are the directions from each attach cell toward its socket —
    /// the socket's *inward* direction, i.e. `socket.outward().opposite()`.
    pub fn route(
        free: &HashSet<(u16, u16)>,
        start: (u16, u16),
        goal: (u16, u16),
        bend_penalty: u32,
        from: Direction,
        to: Direction,
    ) -> Option<Connector> {
        route(free, start, goal, bend_penalty).map(|path| Connector { path, from, to })
    }
}

// ── Nudging: route many wires onto separate tracks ─────────────────────────────

/// One wire to route between two socket attach cells. `from`/`to` are the inward
/// socket directions, as for [`Connector::route`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteRequest {
    /// The start socket's attach cell (see [`Socket::attach`](crate::socket::Socket::attach)).
    pub start: (u16, u16),
    /// The goal socket's attach cell.
    pub goal: (u16, u16),
    /// Inward direction at `start` (toward its socket).
    pub from: Direction,
    /// Inward direction at `goal`.
    pub to: Direction,
}

impl RouteRequest {
    /// A request from `start` to `goal` with the two inward socket directions.
    pub fn new(start: (u16, u16), goal: (u16, u16), from: Direction, to: Direction) -> Self {
        Self { start, goal, from, to }
    }
}

/// Route many connectors **with nudging**: each `request` is routed in turn over
/// `free`, but may not run *along* an edge an earlier wire already took — so
/// parallel wires spread onto separate integer tracks and a gutter holds at most
/// as many wires as it is cells wide. Crossing an earlier wire (a perpendicular
/// pass) is allowed but charged `crossing_penalty`, biasing the search toward
/// crossing-free routes.
///
/// Returns one entry per request, **in order**. An entry is `None` where that wire
/// could not be routed — e.g. its gutter is already at capacity — so the router
/// fails a net gracefully rather than overlapping it onto another. Routing is
/// greedy in request order; a different order can pack differently.
pub fn route_all(
    free: &HashSet<(u16, u16)>,
    requests: &[RouteRequest],
    bend_penalty: u32,
    crossing_penalty: u32,
) -> Vec<Option<Connector>> {
    let mut occ = Occupancy::default();
    let mut out = Vec::with_capacity(requests.len());
    for req in requests {
        match astar(free, req.start, req.goal, bend_penalty, &occ, crossing_penalty) {
            Some(path) => {
                occ.add(&path);
                out.push(Some(Connector { path, from: req.from, to: req.to }));
            }
            None => out.push(None),
        }
    }
    out
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render `connectors` (canvas coordinates) into `buf`, with the canvas's `(0,0)`
/// drawn at screen `origin`, **colour-per-net**.
///
/// Each connector's path and its two socket stubs are laid into a shared
/// [`EdgeGrid`] covering `canvas`, so turns and crossings between connectors
/// resolve to the right box-drawing glyph through [`resolve`]. `styles[i]` colours
/// connector `i` (the disambiguation strategy of §5.2 — there is no hop-over
/// glyph, so a `┼` crossing reads by following each net's colour); at a crossing
/// the cell takes the later net's colour. Missing entries fall back to
/// [`Style::default`]. Cells inside any `skip` rect (the node bodies, whose borders
/// carry the sockets) are not drawn — the connectors meet the sockets there.
/// `weight` is the line weight.
pub fn render(
    buf: &mut Buffer,
    canvas: Rect,
    origin: (u16, u16),
    connectors: &[Connector],
    styles: &[Style],
    skip: &[Rect],
    weight: LineWeight,
) {
    let mut grid = EdgeGrid::new(canvas);
    // Per-cell colour, so each net keeps its hue through a shared crossing glyph.
    let mut colors: HashMap<(u16, u16), Style> = HashMap::new();
    for (i, c) in connectors.iter().enumerate() {
        let style = styles.get(i).copied().unwrap_or_default();
        // Lay each straight run of the path as a box-drawing line.
        for seg in c.path.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            if a.1 == b.1 {
                grid.add_h_line(a.0.min(b.0), a.0.max(b.0), a.1, weight);
            } else {
                grid.add_v_line(a.1.min(b.1), a.1.max(b.1), a.0, weight);
            }
        }
        // One-cell stub from each endpoint toward its socket, so the wire's end
        // glyph points into the socket (the socket cell itself is in `skip`).
        stub(&mut grid, c.path[0], c.from, weight);
        if let Some(&last) = c.path.last() {
            stub(&mut grid, last, c.to, weight);
        }
        for &cell in &c.path {
            colors.insert(cell, style);
        }
    }

    for y in canvas.y..canvas.bottom() {
        for x in canvas.x..canvas.right() {
            if skip.iter().any(|r| r.contains(x, y)) {
                continue;
            }
            if let Some(ch) = grid.get(x, y).and_then(resolve) {
                let style = colors.get(&(x, y)).copied().unwrap_or_default();
                let sx = origin.0 + (x - canvas.x);
                let sy = origin.1 + (y - canvas.y);
                buf.set_grapheme(sx, sy, &ch.to_string(), style);
            }
        }
    }
}

/// Add a one-cell arm at `cell` pointing in `dir` (toward the socket).
fn stub(grid: &mut EdgeGrid, cell: (u16, u16), dir: Direction, weight: LineWeight) {
    let (dx, dy) = dir.delta();
    let (nx, ny) = (cell.0 as i32 + dx, cell.1 as i32 + dy);
    if nx < 0 || ny < 0 {
        return;
    }
    let n = (nx as u16, ny as u16);
    match dir {
        Direction::Left | Direction::Right => {
            grid.add_h_line(cell.0.min(n.0), cell.0.max(n.0), cell.1, weight);
        }
        Direction::Up | Direction::Down => {
            grid.add_v_line(cell.1.min(n.1), cell.1.max(n.1), cell.0, weight);
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::float::free_cells_in_window;
    use crate::label::Side;
    use crate::socket::{Flow, Socket};

    /// Free cells of a `w×h` canvas with the given node rects removed (gutter 0).
    fn free_grid(w: u16, h: u16, nodes: &[Rect]) -> HashSet<(u16, u16)> {
        let canvas = Rect::new(0, 0, w, h);
        free_cells_in_window(canvas, nodes, 0, canvas).into_iter().collect()
    }

    fn is_orthogonal(path: &[(u16, u16)]) -> bool {
        path.windows(2).all(|s| {
            let (a, b) = (s[0], s[1]);
            a.0.abs_diff(b.0) + a.1.abs_diff(b.1) == 1
        })
    }

    #[test]
    fn straight_shot_has_no_bends() {
        let free = free_grid(20, 5, &[]);
        let path = route(&free, (1, 2), (15, 2), 4).unwrap();
        assert_eq!(path.first(), Some(&(1, 2)));
        assert_eq!(path.last(), Some(&(15, 2)));
        assert!(is_orthogonal(&path));
        // A clear horizontal shot: one row throughout, zero turns.
        assert!(path.iter().all(|c| c.1 == 2), "should stay on row 2");
        assert_eq!(path.len(), 15, "straight Manhattan length");
    }

    #[test]
    fn routes_around_an_obstacle() {
        // A node spanning the middle forces a detour.
        let node = Rect::new(6, 0, 4, 4); // blocks cols 6..10 on rows 0..4
        let free = free_grid(16, 6, &[node]);
        let path = route(&free, (2, 1), (13, 1), 4).unwrap();
        assert!(is_orthogonal(&path));
        // The path never enters the node.
        assert!(path.iter().all(|&(x, y)| !node.contains(x, y)));
        assert_eq!(path.first(), Some(&(2, 1)));
        assert_eq!(path.last(), Some(&(13, 1)));
    }

    #[test]
    fn unreachable_returns_none() {
        // A full-height wall splits the canvas in two.
        let wall = Rect::new(5, 0, 1, 6);
        let free = free_grid(10, 6, &[wall]);
        assert_eq!(route(&free, (2, 2), (8, 2), 4), None);
    }

    #[test]
    fn endpoints_coincide_with_socket_attach_cells() {
        // Two nodes; wire an output of one to an input of the other.
        let a = Rect::new(2, 2, 8, 5);
        let b = Rect::new(20, 6, 8, 5);
        let free = free_grid(40, 16, &[a, b]);
        let out = Socket::new(Side::Right, 2, Flow::Out, 0);
        let inp = Socket::new(Side::Left, 2, Flow::In, 0);
        let start = out.attach(a).unwrap();
        let goal = inp.attach(b).unwrap();
        let path = route(&free, start, goal, 4).unwrap();
        assert_eq!(path.first(), Some(&start));
        assert_eq!(path.last(), Some(&goal));
        assert!(is_orthogonal(&path));
    }

    // ── Nudging & crossing resolution (Phase 9) ───────────────────────────

    fn edges_of(path: &[(u16, u16)]) -> HashSet<(u16, u16, u8)> {
        path.windows(2).map(|w| edge_key(w[0], w[1])).collect()
    }

    #[test]
    fn parallel_wires_nudge_onto_separate_tracks() {
        // A horizontal corridor exactly two rows tall (rows 2,3) between two walls.
        let walls = [Rect::new(4, 0, 4, 2), Rect::new(4, 4, 4, 1)];
        let free = free_grid(12, 5, &walls);
        let reqs = [
            RouteRequest::new((1, 2), (10, 2), Direction::Left, Direction::Right),
            RouteRequest::new((1, 3), (10, 3), Direction::Left, Direction::Right),
        ];
        let cons: Vec<_> = route_all(&free, &reqs, 4, 2).into_iter().flatten().collect();
        assert_eq!(cons.len(), 2, "both fit in a 2-row gutter");
        // No shared edge (the nudging invariant)…
        assert!(edges_of(&cons[0].path).is_disjoint(&edges_of(&cons[1].path)));
        // …and in a sufficient gutter, no shared cell at all.
        let first: HashSet<_> = cons[0].path.iter().copied().collect();
        assert!(cons[1].path.iter().all(|c| !first.contains(c)));
    }

    #[test]
    fn one_cell_corridor_holds_one_wire() {
        // The only crossing of cols 4..8 is row 2 (a 1-cell corridor).
        let walls = [Rect::new(4, 0, 4, 2), Rect::new(4, 3, 4, 2)];
        let free = free_grid(12, 5, &walls);
        let reqs = [
            RouteRequest::new((1, 2), (10, 2), Direction::Left, Direction::Right),
            RouteRequest::new((1, 1), (10, 1), Direction::Left, Direction::Right),
        ];
        let out = route_all(&free, &reqs, 4, 0);
        assert!(out[0].is_some(), "first wire takes the corridor");
        assert!(out[1].is_none(), "second has no track left — fails gracefully");
    }

    #[test]
    fn crossing_resolves_to_plus_and_keeps_net_colour() {
        use crate::style::Color;
        let free = free_grid(11, 7, &[]);
        let h = Connector::route(&free, (1, 3), (9, 3), 4, Direction::Left, Direction::Right).unwrap();
        let v = Connector::route(&free, (5, 1), (5, 5), 4, Direction::Up, Direction::Down).unwrap();
        let area = Rect::new(0, 0, 11, 7);
        let mut buf = Buffer::empty(area);
        let styles = [Style::default().fg(Color::Red), Style::default().fg(Color::Blue)];
        render(&mut buf, area, (0, 0), &[h, v], &styles, &[], LineWeight::Light);
        // A horizontal wire crossing a vertical one resolves deterministically to ┼…
        assert_eq!(buf.get(5, 3).symbol, "┼");
        // …and the crossing keeps the later net's colour (blue).
        assert_eq!(buf.get(5, 3).style.fg, Color::Blue);
        // Each net keeps its own colour away from the crossing.
        assert_eq!(buf.get(2, 3).style.fg, Color::Red);
        assert_eq!(buf.get(5, 2).style.fg, Color::Blue);
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// Every routed path is orthogonal, hits its endpoints, and never enters a
        /// node (interior or border).
        #[test]
        fn prop_path_is_valid(
            sx in 0u16..30, sy in 0u16..16,
            gx in 0u16..30, gy in 0u16..16,
            nx in 0u16..28, ny in 0u16..14, nw in 1u16..6, nh in 1u16..6,
        ) {
            let node = Rect::new(nx, ny, nw, nh);
            let free = free_grid(30, 16, &[node]);
            // Only test endpoints that are actually free (outside the node).
            prop_assume!(free.contains(&(sx, sy)) && free.contains(&(gx, gy)));
            if let Some(path) = route(&free, (sx, sy), (gx, gy), 4) {
                prop_assert_eq!(path.first(), Some(&(sx, sy)));
                prop_assert_eq!(path.last(), Some(&(gx, gy)));
                prop_assert!(is_orthogonal(&path));
                for &c in &path {
                    prop_assert!(!node.contains(c.0, c.1), "path entered node at {:?}", c);
                    prop_assert!(free.contains(&c));
                }
            }
        }

        /// A higher bend penalty never yields more corners than a lower one for the
        /// same query (straightness is monotone in the penalty).
        #[test]
        fn prop_bend_penalty_reduces_corners(len in 4u16..28) {
            let free = free_grid(30, 8, &[]);
            let corners = |p: &[(u16,u16)]| p.windows(3).filter(|w| {
                // a corner: the three cells are not colinear
                !((w[0].0 == w[1].0 && w[1].0 == w[2].0) || (w[0].1 == w[1].1 && w[1].1 == w[2].1))
            }).count();
            let cheap = route(&free, (1, 1), (len, 6), 0).unwrap();
            let dear = route(&free, (1, 1), (len, 6), 20).unwrap();
            prop_assert!(corners(&dear) <= corners(&cheap));
        }

        /// Wires routed together via `route_all` are pairwise edge-disjoint — no
        /// wire ever runs along another's track (gutter capacity is never exceeded).
        #[test]
        fn prop_routed_wires_are_edge_disjoint(
            ends in prop::collection::vec(
                (0u16..24, 0u16..12, 0u16..24, 0u16..12), 2..6),
        ) {
            let free = free_grid(24, 12, &[]);
            let reqs: Vec<RouteRequest> = ends.iter().map(|&(sx, sy, gx, gy)| {
                RouteRequest::new((sx, sy), (gx, gy), Direction::Left, Direction::Right)
            }).collect();
            let cons: Vec<_> = route_all(&free, &reqs, 4, 4).into_iter().flatten().collect();
            for i in 0..cons.len() {
                for j in (i + 1)..cons.len() {
                    let (a, b) = (edges_of(&cons[i].path), edges_of(&cons[j].path));
                    prop_assert!(a.is_disjoint(&b), "wires {} and {} share an edge", i, j);
                }
            }
        }
    }
}
