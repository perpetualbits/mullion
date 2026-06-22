// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 8 demo — orthogonal connector routing (design note §5.2).
//!
//! Three nodes, each with an input socket (left) and an output socket (right),
//! wired in a triangle. The connectors are routed with grid A\* over the free
//! cells around the nodes, with a bend penalty so they prefer long straight runs
//! ("train tracks"). Drag a node and the wires **reroute live** around it.
//!
//! Routing is in canvas space and recomputed each frame (cheap at this scale).
//! No connectors cross-disambiguation yet — a crossing reads as a `┼` join
//! (Phase 9 adds color-per-net).
//!
//! Keys
//!   Tab                  select the next node
//!   ← ↓ ↑ → or h j k l    nudge the selected node
//!   mouse drag           reposition a node (wires follow)
//!   q                    quit

use std::collections::HashSet;
use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent, MouseButton, MouseEventKind};

use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle},
    float::free_cells_in_window,
    label::Side,
    mouse::tile_at,
    poll_event,
    route::{render as render_connectors, Connector},
    socket::{draw_socket, Flow, Socket},
    style::{Color, Modifier, Style},
    Buffer, FloatRect, GraphCanvas, LineWeight, Rect, Terminal, TileId,
};

const BEND: u32 = 4; // bend penalty: higher = straighter

struct State {
    canvas: GraphCanvas,
    /// Output→input wires as `(from node, to node)`.
    wires: Vec<(TileId, TileId)>,
    selected: TileId,
    drag: Option<(TileId, u16, u16)>,
}

impl State {
    fn new() -> Self {
        let mut canvas = GraphCanvas::new(80, 24);
        canvas.add(1, FloatRect::new(4, 2, 16, 7));
        canvas.add(2, FloatRect::new(40, 4, 16, 7));
        canvas.add(3, FloatRect::new(22, 14, 16, 7));
        Self { canvas, wires: vec![(1, 2), (2, 3), (3, 1)], selected: 1, drag: None }
    }
}

/// The output socket (right edge, centred) of a node of the given height.
fn out_socket(h: u16) -> Socket {
    Socket::new(Side::Right, h / 2, Flow::Out, 0)
}
/// The input socket (left edge, centred).
fn in_socket(h: u16) -> Socket {
    Socket::new(Side::Left, h / 2, Flow::In, 0)
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 6 || area.width < 16 {
        return;
    }
    let status_y = area.height - 1;
    let window = Rect::new(0, 1, area.width, status_y - 1);
    st.canvas.resize(window.width, window.height);

    // Node rects in CANVAS coordinates (origin 0,0).
    let canvas_rect = Rect::new(0, 0, window.width, window.height);
    let node_rects: Vec<(TileId, Rect)> = st
        .canvas
        .nodes()
        .iter()
        .map(|n| (n.id, Rect::new(n.place.x, n.place.y, n.place.width, n.place.height)))
        .collect();
    let obstacles: Vec<Rect> = node_rects.iter().map(|&(_, r)| r).collect();

    // Free cells = canvas minus node bodies (gutter 0 so wires may run adjacent).
    let free: HashSet<(u16, u16)> =
        free_cells_in_window(canvas_rect, &obstacles, 0, canvas_rect).into_iter().collect();

    // Route every wire fresh this frame.
    let rect_of = |id: TileId| node_rects.iter().find(|&&(i, _)| i == id).map(|&(_, r)| r);
    let mut connectors: Vec<Connector> = Vec::new();
    for &(a, b) in &st.wires {
        let (Some(ra), Some(rb)) = (rect_of(a), rect_of(b)) else { continue };
        let src = out_socket(ra.height);
        let dst = in_socket(rb.height);
        let (Some(start), Some(goal)) = (src.attach(ra), dst.attach(rb)) else { continue };
        if let Some(c) = Connector::route(&free, start, goal, BEND,
            src.outward().opposite(), dst.outward().opposite())
        {
            connectors.push(c);
        }
    }

    // Connectors first (under the nodes), then nodes on top.
    render_connectors(buf, canvas_rect, (window.x, window.y), &connectors, &obstacles,
        LineWeight::Light, Style::default().fg(Color::Green));

    for (id, crect) in &node_rects {
        // Map the canvas rect to screen.
        let screen = Rect::new(window.x + crect.x, window.y + crect.y, crect.width, crect.height);
        draw_node(buf, *id, screen, *id == st.selected);
    }

    // ── Help & status ──────────────────────────────────────────────────────
    buf.set_string(0, 0, "wires — Tab:select  hjkl/arrows:nudge  drag:move  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let status = format!(" node {}   {} wires rerouted/frame", st.selected, connectors.len());
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, status_y, " ", sstyle);
    }
    buf.set_string(0, status_y, &status, sstyle);
}

fn draw_node(buf: &mut Buffer, id: TileId, rect: Rect, selected: bool) {
    let (weight, color) = if selected {
        (LineWeight::Heavy, Color::Cyan)
    } else {
        (LineWeight::Light, Color::Gray)
    };
    draw_box(buf, rect, Borders::ALL, &BorderStyle {
        weight, corners: CornerStyle::Rounded, style: Style::default().fg(color),
    });
    if rect.width > 6 {
        buf.set_string(rect.x + 2, rect.y + rect.height / 2, &format!("node {id}"),
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
    let mut st = State::new();
    loop {
        term.draw(|buf| render(buf, &mut st))?;
        match poll_event(Duration::from_millis(50))? {
            None | Some(Event::Resize(_, _)) => {}
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Tab => {
                    let ids: Vec<TileId> = st.canvas.nodes().iter().map(|n| n.id).collect();
                    if let Some(i) = ids.iter().position(|&i| i == st.selected) {
                        st.selected = ids[(i + 1) % ids.len()];
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => st.canvas.nudge(st.selected, -1, 0),
                KeyCode::Right | KeyCode::Char('l') => st.canvas.nudge(st.selected, 1, 0),
                KeyCode::Up | KeyCode::Char('k') => st.canvas.nudge(st.selected, 0, -1),
                KeyCode::Down | KeyCode::Char('j') => st.canvas.nudge(st.selected, 0, 1),
                _ => {}
            },
            Some(Event::Mouse(m)) => handle_mouse(term, &mut st, m)?,
            _ => {}
        }
    }
    Ok(())
}

fn handle_mouse(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    st: &mut State,
    m: crossterm::event::MouseEvent,
) -> io::Result<()> {
    let size = mullion::backend::Backend::size(term.backend())?;
    let window = Rect::new(0, 1, size.width, size.height.saturating_sub(2));
    let (mx, my) = (m.column, m.row);
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Screen node rects for hit-testing.
            let rects: Vec<(TileId, Rect)> = st.canvas.nodes().iter()
                .map(|n| (n.id, Rect::new(window.x + n.place.x, window.y + n.place.y, n.place.width, n.place.height)))
                .collect();
            if let Some(id) = tile_at(&rects, mx, my) {
                st.selected = id;
                let r = rects.iter().find(|(i, _)| *i == id).unwrap().1;
                st.drag = Some((id, mx - r.x, my - r.y));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some((id, ox, oy)) = st.drag {
                let cx = mx.saturating_sub(window.x).saturating_sub(ox);
                let cy = my.saturating_sub(window.y).saturating_sub(oy);
                st.canvas.move_to(id, cx, cy);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => st.drag = None,
        _ => {}
    }
    Ok(())
}
