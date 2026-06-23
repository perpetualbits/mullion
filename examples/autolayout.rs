// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 12 demo — Sugiyama (layered) auto-layout (design note §5.4).
//!
//! A directed graph (with one cycle, to exercise cycle-breaking). Press `a` and the
//! nodes glide into **layers** along the dataflow direction, ordered to cut
//! crossings, written back into the same `GraphCanvas` that manual drag uses. Press
//! `s` to scatter them again. The wires are routed by the Phase 8 connector router.
//!
//! Auto-layout can run wider than the screen, so the result lives on a canvas you
//! pan with the arrows / `hjkl` (Phase 10 viewport).
//!
//! Keys
//!   a              auto-layout (Sugiyama)
//!   r              refine (hill-climb the score under the active taste)
//!   w              cycle the weight taste (default / two learned tastes)
//!   s              scatter
//!   ← ↓ ↑ → / hjkl  pan
//!   q              quit

use std::collections::{HashMap, HashSet};
use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle},
    float::free_cells_in_window,
    label::Side,
    poll_event,
    refine::{learn_weights, refine, score, Preference, ScoreWeights},
    route::{render as render_connectors, route_all, Connector, RouteRequest},
    socket::{draw_socket, Flow, Socket},
    style::{Color, Modifier, Style},
    sugiyama::{auto_layout, SugiyamaParams},
    zoom::lerp_rect,
    Buffer, FloatRect, GraphCanvas, LineWeight, Rect, Terminal, TileId, Viewport,
};

const NW: u16 = 14;
const NH: u16 = 5;
const NET_COLORS: [Color; 6] =
    [Color::Cyan, Color::Magenta, Color::Yellow, Color::Green, Color::Red, Color::Blue];

struct State {
    canvas: GraphCanvas,
    edges: Vec<(TileId, TileId)>,
    /// Where each node is gliding toward (auto-layout or scatter result).
    target: HashMap<TileId, FloatRect>,
    vp: Viewport,
    /// Selectable weight tastes: `(name, weights)`; `r` refines with the active one.
    tastes: Vec<(&'static str, ScoreWeights)>,
    active: usize,
}

/// A `LayoutScore` with the given soft terms (for synthetic teaching examples).
fn ls(crossings: usize, length: f32) -> mullion::refine::LayoutScore {
    mullion::refine::LayoutScore { total: 0.0, crossings, length, area: 2000.0, alignment: 10, overlap: 0 }
}

/// Two tastes *learned* from a handful of example corrections: one that prefers
/// fewer crossings (even at the cost of length), one that prefers shorter wires.
fn learned_tastes() -> Vec<(&'static str, ScoreWeights)> {
    let few_crossings: Vec<Preference> = (0..15)
        .map(|i| Preference { worse: ls(4, 150.0 + i as f32), better: ls(1, 250.0 + i as f32) })
        .collect();
    let short_wires: Vec<Preference> = (0..15)
        .map(|i| Preference { worse: ls(1, 300.0 + i as f32), better: ls(4, 150.0 + i as f32) })
        .collect();
    vec![
        ("default", ScoreWeights::default()),
        ("learned: few crossings", learn_weights(&few_crossings, 600, 0.5)),
        ("learned: short wires", learn_weights(&short_wires, 600, 0.5)),
    ]
}

/// Scatter positions — deliberately messy, so a layout has something to fix.
fn scatter() -> HashMap<TileId, FloatRect> {
    let spots = [
        (60, 2), (8, 18), (90, 22), (30, 4), (74, 30),
        (4, 34), (50, 14), (100, 6), (24, 26),
    ];
    spots.iter().enumerate().map(|(i, &(x, y))| (i as TileId + 1, FloatRect::new(x, y, NW, NH))).collect()
}

impl State {
    fn new(window: Rect) -> Self {
        let target = scatter();
        let mut canvas = GraphCanvas::new(130, 44);
        for (&id, &r) in &target {
            canvas.add(id, r);
        }
        // A DAG with one back edge (9→1) to show cycle-breaking.
        let edges = vec![
            (1, 4), (4, 7), (2, 7), (7, 8), (8, 5), (3, 8),
            (5, 9), (3, 6), (6, 9), (9, 1),
        ];
        Self { canvas, edges, target, vp: Viewport::new(window, 130, 44), tastes: learned_tastes(), active: 0 }
    }

    fn weights(&self) -> ScoreWeights {
        self.tastes[self.active].1
    }

    fn rect_of(&self, id: TileId) -> Option<Rect> {
        self.canvas.place(id).map(|p| Rect::new(p.x, p.y, p.width, p.height))
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 6 || area.width < 20 {
        return;
    }
    let window = Rect::new(0, 1, area.width, area.height - 2);
    st.vp.set_window(window);
    let (cw, ch) = st.canvas.size();
    st.vp.set_canvas(cw, ch);

    // Node rects (canvas space) and obstacles.
    let node_rects: Vec<(TileId, Rect)> = st
        .canvas
        .nodes()
        .iter()
        .map(|n| (n.id, Rect::new(n.place.x, n.place.y, n.place.width, n.place.height)))
        .collect();
    let obstacles: Vec<Rect> = node_rects.iter().map(|&(_, r)| r).collect();

    // Route each directed edge output→input.
    let free: HashSet<(u16, u16)> = {
        let cr = Rect::new(0, 0, cw, ch);
        free_cells_in_window(cr, &obstacles, 0, cr).into_iter().collect()
    };
    let mut reqs: Vec<RouteRequest> = Vec::new();
    let mut colors: Vec<Color> = Vec::new();
    for (i, &(a, b)) in st.edges.iter().enumerate() {
        let (Some(ra), Some(rb)) = (st.rect_of(a), st.rect_of(b)) else { continue };
        let (src, dst) = (out_socket(ra.height), in_socket(rb.height));
        let (Some(s), Some(g)) = (src.attach(ra), dst.attach(rb)) else { continue };
        reqs.push(RouteRequest::new(s, g, src.outward().opposite(), dst.outward().opposite()));
        colors.push(NET_COLORS[i % NET_COLORS.len()]);
    }
    let (connectors, styles): (Vec<Connector>, Vec<Style>) = route_all(&free, &reqs, 4, 8)
        .into_iter()
        .zip(&colors)
        .filter_map(|(c, &col)| c.map(|c| (c, Style::default().fg(col))))
        .unzip();
    render_connectors(buf, st.vp.visible(), st.vp.origin(), &connectors, &styles, &obstacles, LineWeight::Light);

    // Cull + draw nodes.
    for (id, crect) in &node_rects {
        if st.vp.is_visible(*crect, 1) {
            if let Some(screen) = st.vp.project(*crect) {
                draw_node(buf, *id, screen, screen.width == crect.width && screen.height == crect.height);
            }
        }
    }

    // ── Help & status ──────────────────────────────────────────────────────
    let s = score(&st.canvas, &st.edges, st.weights());
    buf.set_string(0, 0, "autolayout — a:layout  r:refine  w:taste  s:scatter  hjkl/arrows:pan  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let status = format!(" taste: {}   {} crossings  len {:.0}  score {:.0}",
        st.tastes[st.active].0, s.crossings, s.length, s.total);
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, area.height - 1, " ", sstyle);
    }
    buf.set_string(0, area.height - 1, &status, sstyle);
}

fn out_socket(h: u16) -> Socket {
    Socket::new(Side::Right, h / 2, Flow::Out, 0)
}
fn in_socket(h: u16) -> Socket {
    Socket::new(Side::Left, h / 2, Flow::In, 0)
}

fn draw_node(buf: &mut Buffer, id: TileId, rect: Rect, full: bool) {
    draw_box(buf, rect, Borders::ALL, &BorderStyle {
        weight: LineWeight::Light, corners: CornerStyle::Rounded, style: Style::default().fg(Color::Gray),
    });
    if full {
        if rect.width > 6 {
            buf.set_string(rect.x + 2, rect.y + rect.height / 2, &format!("n{id}"),
                Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
        }
        let s = Style::default().fg(Color::Green);
        draw_socket(buf, rect, &in_socket(rect.height), true, s);
        draw_socket(buf, rect, &out_socket(rect.height), true, s);
    }
}

// ── Main / event loop ───────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;
    let result = run(&mut term);
    term.leave()?;
    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let size = mullion::backend::Backend::size(term.backend())?;
    let mut st = State::new(Rect::new(0, 1, size.width, size.height.saturating_sub(2)));
    loop {
        term.draw(|buf| render(buf, &mut st))?;
        match poll_event(Duration::from_millis(33))? {
            None | Some(Event::Resize(_, _)) => glide(&mut st),
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Char('a') => st.target = layout_targets(&st),
                KeyCode::Char('r') => st.target = refine_targets(&st),
                KeyCode::Char('w') => {
                    st.active = (st.active + 1) % st.tastes.len();
                    st.target = refine_targets(&st); // re-refine under the new taste
                }
                KeyCode::Char('s') => st.target = scatter(),
                KeyCode::Left | KeyCode::Char('h') => st.vp.pan_by(-2, 0),
                KeyCode::Right | KeyCode::Char('l') => st.vp.pan_by(2, 0),
                KeyCode::Up | KeyCode::Char('k') => st.vp.pan_by(0, -1),
                KeyCode::Down | KeyCode::Char('j') => st.vp.pan_by(0, 1),
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}

/// Run Sugiyama on a clone and read back the target positions (so the real canvas
/// can glide there rather than snap).
fn layout_targets(st: &State) -> HashMap<TileId, FloatRect> {
    let mut tmp = st.canvas.clone();
    auto_layout(&mut tmp, &st.edges, &SugiyamaParams { layer_gap: 8, node_gap: 2, ..Default::default() });
    tmp.nodes().iter().map(|n| (n.id, n.place)).collect()
}

/// Hill-climb the score on a clone and read back the refined positions, so the
/// nodes glide into their improved spots (swaps that drop crossings + wire length).
fn refine_targets(st: &State) -> HashMap<TileId, FloatRect> {
    let mut tmp = st.canvas.clone();
    refine(&mut tmp, &st.edges, st.weights(), 30);
    tmp.nodes().iter().map(|n| (n.id, n.place)).collect()
}

/// Ease every node one step toward its target; snap when within a cell.
fn glide(st: &mut State) {
    // Grow the canvas to hold the targets, so gliding nodes are not clamped.
    let (mut ex, mut ey) = st.canvas.size();
    for r in st.target.values() {
        ex = ex.max(r.x + r.width);
        ey = ey.max(r.y + r.height);
    }
    st.canvas.resize(ex, ey);
    let ids: Vec<TileId> = st.canvas.nodes().iter().map(|n| n.id).collect();
    for id in ids {
        let (Some(cur), Some(&tgt)) = (st.canvas.place(id), st.target.get(&id)) else { continue };
        let cur = Rect::new(cur.x, cur.y, cur.width, cur.height);
        let tgt_r = Rect::new(tgt.x, tgt.y, tgt.width, tgt.height);
        let next = if cur.x.abs_diff(tgt.x) <= 1 && cur.y.abs_diff(tgt.y) <= 1 {
            tgt_r
        } else {
            lerp_rect(cur, tgt_r, 0.25)
        };
        st.canvas.move_to(id, next.x, next.y);
    }
}
