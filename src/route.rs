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
//! is no hop-over glyph (single-line box drawing has none); disambiguating a
//! crossing that is not a join is Phase 9 (color-per-net / avoidance).

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
            if !free.contains(&n) {
                continue;
            }
            // A turn is any move whose direction differs from how we entered.
            let turn = matches!(dir, Some(pd) if pd != d);
            let ng = g + 1 + if turn { bend_penalty } else { 0 };
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

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render `connectors` (canvas coordinates) into `buf`, with the canvas's `(0,0)`
/// drawn at screen `origin`.
///
/// Each connector's path and its two socket stubs are laid into a shared
/// [`EdgeGrid`] covering `canvas`, so turns and crossings between connectors
/// resolve to the right box-drawing glyph through [`resolve`]. Cells inside any
/// `skip` rect (the node bodies, whose borders carry the sockets) are not drawn —
/// the connectors meet the sockets there. `weight` is the line weight; `style`
/// colours every connector cell.
pub fn render(
    buf: &mut Buffer,
    canvas: Rect,
    origin: (u16, u16),
    connectors: &[Connector],
    skip: &[Rect],
    weight: LineWeight,
    style: Style,
) {
    let mut grid = EdgeGrid::new(canvas);
    for c in connectors {
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
    }

    for y in canvas.y..canvas.bottom() {
        for x in canvas.x..canvas.right() {
            if skip.iter().any(|r| r.contains(x, y)) {
                continue;
            }
            if let Some(ch) = grid.get(x, y).and_then(resolve) {
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
    }
}
