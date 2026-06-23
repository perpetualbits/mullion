// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Teach the layout engine your taste — the finale of the learnable-layout thread.
//!
//! The engine lays a small directed graph out (Sugiyama → anneal under the current
//! weights), with sockets on the nodes and orthogonal colour-per-net wires between
//! them. You **drag nodes** to improve it, then press **`t`** to *teach*: it records
//! `(machine layout, your layout)` as a preference pair and re-fits the
//! `ScoreWeights`. Press **`a`** to re-lay-out under *your* learned taste — less
//! fixing each round. Train it by showing it improvements.
//!
//! The wires are re-routed only when a node actually moves (cached otherwise), so the
//! drag stays responsive.
//!
//! Keys
//!   mouse drag     move a node (the wires re-route to follow)
//!   t              teach: "what I just did is better" — re-fits the weights
//!   a              re-lay-out under the learned weights (Sugiyama + anneal)
//!   d              forget all lessons (reset the weights)
//!   q              quit

use std::collections::HashSet;
use std::path::PathBuf;
use std::{fs, io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent, MouseButton, MouseEventKind};

use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle},
    float::free_cells_in_window,
    label::Side,
    mouse::tile_at,
    poll_event,
    refine::{anneal, learn_weights, score, AnnealParams, LayoutScore, Preference, ScoreWeights},
    route::{render as render_connectors, route_all, Connector, RouteRequest},
    socket::{draw_socket, Flow, Socket},
    style::{Color, Modifier, Style},
    sugiyama::{auto_layout, SugiyamaParams},
    Buffer, FloatRect, GraphCanvas, LineWeight, Rect, Terminal, TileId,
};

const NW: u16 = 12;
const NH: u16 = 5;
// Cap the canvas so re-routing stays cheap on a huge terminal; the graph fits well
// inside this, and on smaller terminals the window is used as-is.
const CAP_W: u16 = 132;
const CAP_H: u16 = 42;
const NET_COLORS: [Color; 6] =
    [Color::Cyan, Color::Magenta, Color::Yellow, Color::Green, Color::Red, Color::Blue];

struct State {
    canvas: GraphCanvas,
    edges: Vec<(TileId, TileId)>,
    weights: ScoreWeights,
    /// Corrections taught so far, and the machine layout the next is judged against.
    lessons: Vec<Preference>,
    baseline: LayoutScore,
    drag: Option<(TileId, u16, u16)>,
    msg: String,
    /// Cached routed wires + their per-net colours, recomputed only when `dirty`.
    connectors: Vec<Connector>,
    styles: Vec<Style>,
    dirty: bool,
}

impl State {
    fn new(window: Rect) -> Self {
        let (cw, ch) = (window.width.min(CAP_W), window.height.min(CAP_H));
        let mut canvas = GraphCanvas::new(cw, ch);
        let spots = [(2, 1), (2, 11), (26, 4), (26, 14), (52, 1), (52, 12)];
        for (i, &(x, y)) in spots.iter().enumerate() {
            canvas.add(i as TileId + 1, FloatRect::new(x, y, NW, NH));
        }
        let edges = vec![(1, 3), (1, 4), (2, 3), (2, 4), (3, 5), (4, 6), (3, 6), (4, 5), (5, 6)];
        // Remember across runs: reload the corrections and re-fit the weights.
        let lessons = load_lessons();
        let weights = if lessons.is_empty() { ScoreWeights::default() } else { learn_weights(&lessons, 600, 0.5) };
        let msg = if lessons.is_empty() {
            "drag a node to improve the layout, then press t to teach".into()
        } else {
            format!("remembered {} lesson(s) — laid out under your learned taste (drag + t to teach more)", lessons.len())
        };
        let mut st = Self {
            canvas,
            edges,
            weights,
            lessons,
            baseline: score(&GraphCanvas::new(1, 1), &[], ScoreWeights::default()),
            drag: None,
            msg,
            connectors: Vec::new(),
            styles: Vec::new(),
            dirty: true,
        };
        st.relayout();
        st
    }

    /// Lay the graph out afresh under the current weights; make it the next baseline.
    fn relayout(&mut self) {
        let (w, h) = self.canvas.size();
        auto_layout(&mut self.canvas, &self.edges, &SugiyamaParams { layer_gap: 5, node_gap: 2, ..Default::default() });
        self.canvas.resize(w, h); // auto_layout only grows; keep the full drag area
        anneal(&mut self.canvas, &self.edges, self.weights, AnnealParams { iters: 1500, ..Default::default() });
        self.baseline = score(&self.canvas, &self.edges, ScoreWeights::default());
        self.dirty = true;
    }

    /// Record "the current layout beats the baseline" and re-fit the weights.
    fn teach(&mut self) {
        let better = score(&self.canvas, &self.edges, ScoreWeights::default());
        self.lessons.push(Preference { worse: self.baseline, better });
        self.weights = learn_weights(&self.lessons, 600, 0.5);
        self.baseline = better;
        save_lessons(&self.lessons); // remember it for next time
        self.msg = format!("taught & saved — {} lesson(s); press a to re-lay-out under your taste", self.lessons.len());
    }

    fn node_rects(&self) -> Vec<(TileId, Rect)> {
        self.canvas.nodes().iter().map(|n| (n.id, Rect::new(n.place.x, n.place.y, n.place.width, n.place.height))).collect()
    }

    /// Re-route the wires from the current node positions (called only when `dirty`).
    fn reroute(&mut self) {
        let rects = self.node_rects();
        let obstacles: Vec<Rect> = rects.iter().map(|&(_, r)| r).collect();
        let (cw, ch) = self.canvas.size();
        let cr = Rect::new(0, 0, cw, ch);
        let free: HashSet<(u16, u16)> = free_cells_in_window(cr, &obstacles, 0, cr).into_iter().collect();
        let rect_of = |id: TileId| rects.iter().find(|&&(i, _)| i == id).map(|&(_, r)| r);
        let mut reqs = Vec::new();
        let mut colors = Vec::new();
        for (i, &(a, b)) in self.edges.iter().enumerate() {
            let (Some(ra), Some(rb)) = (rect_of(a), rect_of(b)) else { continue };
            let (s, d) = (out_socket(ra.height), in_socket(rb.height));
            if let (Some(p), Some(q)) = (s.attach(ra), d.attach(rb)) {
                reqs.push(RouteRequest::new(p, q, s.outward().opposite(), d.outward().opposite()));
                colors.push(NET_COLORS[i % NET_COLORS.len()]);
            }
        }
        let (cons, styles): (Vec<Connector>, Vec<Style>) = route_all(&free, &reqs, 4, 8)
            .into_iter()
            .zip(&colors)
            .filter_map(|(c, &col)| c.map(|c| (c, Style::default().fg(col))))
            .unzip();
        self.connectors = cons;
        self.styles = styles;
        self.dirty = false;
    }
}

// ── Persistence ───────────────────────────────────────────────────────────────
//
// The taught corrections are saved to a file and reloaded on startup, so the engine
// *remembers* your taste across runs. mullion provides the learning; an app chooses
// how to persist — here, one preference per line as its twelve soft terms.

fn lessons_path() -> PathBuf {
    let mut p = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    p.push(".mullion-teach-lessons.txt");
    p
}

fn load_lessons() -> Vec<Preference> {
    let Ok(text) = fs::read_to_string(lessons_path()) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let v: Vec<f32> = line.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if v.len() != 12 {
                return None;
            }
            let mk = |o: usize| LayoutScore {
                total: 0.0,
                crossings: v[o] as usize,
                length: v[o + 1],
                area: v[o + 2],
                alignment: v[o + 3] as usize,
                bends: v[o + 4] as usize,
                edge_node: v[o + 5] as usize,
                overlap: 0,
            };
            Some(Preference { worse: mk(0), better: mk(6) })
        })
        .collect()
}

fn save_lessons(lessons: &[Preference]) {
    let row = |l: &LayoutScore| format!("{} {} {} {} {} {}", l.crossings, l.length, l.area, l.alignment, l.bends, l.edge_node);
    let text: String = lessons.iter().map(|p| format!("{} {}\n", row(&p.worse), row(&p.better))).collect();
    let _ = fs::write(lessons_path(), text);
}

/// The learned weights as **emphasis percentages** — each weight scaled by a typical
/// magnitude of its term, then normalised — so you can read what the engine values.
fn emphasis(w: ScoreWeights) -> String {
    // Representative term magnitudes for a small graph (cells, counts).
    let c = [
        ("cross", w.crossings * 2.0),
        ("len", w.length * 200.0),
        ("area", w.area * 1500.0),
        ("align", w.alignment * 10.0),
        ("bend", w.bends * 5.0),
        ("over-node", w.edge_node * 2.0),
    ];
    let total: f32 = c.iter().map(|(_, v)| *v).sum::<f32>().max(1e-6);
    let mut parts: Vec<(&str, f32)> = c.iter().map(|&(n, v)| (n, v / total * 100.0)).collect();
    parts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    parts.iter().filter(|(_, p)| *p >= 1.0).map(|(n, p)| format!("{n} {:.0}%", p)).collect::<Vec<_>>().join(" · ")
}

fn window_of(area: Rect) -> Rect {
    Rect::new(0, 1, area.width, area.height.saturating_sub(2))
}
fn out_socket(h: u16) -> Socket {
    Socket::new(Side::Right, h / 2, Flow::Out, 0)
}
fn in_socket(h: u16) -> Socket {
    Socket::new(Side::Left, h / 2, Flow::In, 0)
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 6 || area.width < 24 {
        return;
    }
    let window = window_of(area);
    let target = (window.width.min(CAP_W), window.height.min(CAP_H));
    if st.canvas.size() != target {
        st.canvas.resize(target.0, target.1);
        st.dirty = true;
    }
    if st.dirty {
        st.reroute();
    }

    let rects = st.node_rects();
    let obstacles: Vec<Rect> = rects.iter().map(|&(_, r)| r).collect();
    let (cw, ch) = st.canvas.size();
    render_connectors(buf, Rect::new(0, 0, cw, ch), (window.x, window.y), &st.connectors, &st.styles, &obstacles, LineWeight::Light);

    for (id, crect) in &rects {
        let screen = Rect::new(window.x + crect.x, window.y + crect.y, crect.width, crect.height);
        draw_node(buf, *id, screen, st.drag.map(|(d, _, _)| d) == Some(*id));
    }

    // ── Header (learned emphasis) & status ─────────────────────────────────
    let s = score(&st.canvas, &st.edges, st.weights);
    buf.set_string(0, 0, &format!(
        "teach — drag · t:teach · a:re-lay-out · d:forget · q:quit    {} lessons   learned emphasis: {}",
        st.lessons.len(), emphasis(st.weights)),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let status = format!(" {} crossings · {} bends · {} over-node · len {:.0}    {}",
        s.crossings, s.bends, s.edge_node, s.length, st.msg);
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, area.height - 1, " ", sstyle);
    }
    buf.set_string(0, area.height - 1, &status, sstyle);
}

fn draw_node(buf: &mut Buffer, id: TileId, rect: Rect, grabbed: bool) {
    let (weight, color) = if grabbed { (LineWeight::Heavy, Color::White) } else { (LineWeight::Light, Color::Gray) };
    draw_box(buf, rect, Borders::ALL, &BorderStyle { weight, corners: CornerStyle::Rounded, style: Style::default().fg(color) });
    if rect.width > 5 {
        buf.set_string(rect.x + 2, rect.y + rect.height / 2, &format!("n{id}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD));
    }
    let sock = Style::default().fg(Color::Green);
    draw_socket(buf, rect, &in_socket(rect.height), true, sock);
    draw_socket(buf, rect, &out_socket(rect.height), true, sock);
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
    let mut st = State::new(window_of(size));
    loop {
        term.draw(|buf| render(buf, &mut st))?;
        // Block for one event, then drain the rest, so a burst of mouse-drag events
        // collapses into a single re-route + redraw instead of backing up.
        let mut ev = poll_event(Duration::from_millis(50))?;
        let mut quit = false;
        while let Some(e) = ev {
            quit |= handle(term, &mut st, e)?;
            ev = poll_event(Duration::ZERO)?;
        }
        if quit {
            break;
        }
    }
    Ok(())
}

/// Handle one event; returns `true` to quit.
fn handle(term: &mut Terminal<CrosstermBackend<io::Stdout>>, st: &mut State, ev: Event) -> io::Result<bool> {
    match ev {
        Event::Key(KeyEvent { code, .. }) => match code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('t') => st.teach(),
            KeyCode::Char('a') => {
                st.relayout();
                st.msg = "re-laid-out under your learned taste".into();
            }
            KeyCode::Char('d') => {
                st.weights = ScoreWeights::default();
                st.lessons.clear();
                save_lessons(&st.lessons); // clear the saved file too
                st.relayout();
                st.msg = "forgot all lessons; weights reset".into();
            }
            _ => {}
        },
        Event::Mouse(m) => {
            let size = mullion::backend::Backend::size(term.backend())?;
            let window = window_of(size);
            let (mx, my) = (m.column, m.row);
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let rects: Vec<(TileId, Rect)> = st.canvas.nodes().iter()
                        .map(|n| (n.id, Rect::new(window.x + n.place.x, window.y + n.place.y, n.place.width, n.place.height)))
                        .collect();
                    if let Some(id) = tile_at(&rects, mx, my) {
                        let r = rects.iter().find(|(i, _)| *i == id).unwrap().1;
                        st.drag = Some((id, mx - r.x, my - r.y));
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some((id, ox, oy)) = st.drag {
                        let cx = mx.saturating_sub(window.x).saturating_sub(ox);
                        let cy = my.saturating_sub(window.y).saturating_sub(oy);
                        st.canvas.move_to(id, cx, cy);
                        st.dirty = true; // a node moved → wires need re-routing
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => st.drag = None,
                _ => {}
            }
        }
        _ => {}
    }
    Ok(false)
}
