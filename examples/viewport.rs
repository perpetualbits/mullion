// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 10 demo — graph viewport: 2D pan-and-cull (design note §5.7).
//!
//! A graph that lives on a logical canvas larger than the screen. Pan the camera
//! with the arrows / `hjkl`, the mouse wheel, or by dragging — in all four
//! directions. Only nodes and wires intersecting the window (plus a margin) are
//! drawn (culling); off-canvas content is skipped. **Exact** scrollbars on the
//! right and bottom show the true position (the canvas bounding box is known).
//!
//! Because the wires are routed in **canvas space**, the tracks stay put as you
//! scroll — they do not crawl — and are recomputed only when the graph changes.
//!
//! Keys
//!   ← ↓ ↑ → or h j k l    pan the camera
//!   mouse drag / wheel    pan
//!   q                     quit

use std::collections::HashSet;
use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent, MouseButton, MouseEventKind};

use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle},
    float::free_cells_in_window,
    label::Side,
    poll_event,
    route::{render as render_connectors, route_all, Connector, RouteRequest},
    socket::{draw_socket, Flow, Socket},
    style::{Color, Modifier, Style},
    vlist::render_scrollbar,
    Buffer, FloatRect, GraphCanvas, LineWeight, Rect, Terminal, TileId, Viewport,
};

const CANVAS_W: u16 = 140;
const CANVAS_H: u16 = 44;
const MARGIN: u16 = 1; // cull margin, so nodes just off-screen do not pop
const BEND: u32 = 4;
const CROSS: u32 = 8;
const NET_COLORS: [Color; 6] =
    [Color::Cyan, Color::Magenta, Color::Yellow, Color::Green, Color::Red, Color::Blue];

struct State {
    canvas: GraphCanvas,
    wires: Vec<(TileId, TileId)>,
    vp: Viewport,
    /// Last mouse position while drag-panning.
    drag: Option<(u16, u16)>,
}

impl State {
    fn new(window: Rect) -> Self {
        let mut canvas = GraphCanvas::new(CANVAS_W, CANVAS_H);
        let spots = [
            (4, 2), (40, 4), (82, 3), (112, 7),
            (10, 22), (50, 20), (92, 25), (118, 32),
        ];
        for (i, &(x, y)) in spots.iter().enumerate() {
            canvas.add(i as TileId + 1, FloatRect::new(x, y, 16, 7));
        }
        let wires = vec![(1, 2), (2, 3), (3, 4), (5, 6), (6, 7), (7, 8), (1, 5), (4, 8)];
        Self { canvas, wires, vp: Viewport::new(window, CANVAS_W, CANVAS_H), drag: None }
    }
}

fn out_socket(h: u16) -> Socket {
    Socket::new(Side::Right, h / 2, Flow::Out, 0)
}
fn in_socket(h: u16) -> Socket {
    Socket::new(Side::Left, h / 2, Flow::In, 0)
}

/// The window rect for the canvas: everything but the help row, the status row,
/// and the two scrollbar tracks.
fn layout(area: Rect) -> (Rect, Rect, Rect) {
    let window = Rect::new(0, 1, area.width.saturating_sub(1), area.height.saturating_sub(3));
    let vscroll = Rect::new(area.width - 1, 1, 1, window.height);
    let hscroll = Rect::new(0, area.height - 2, window.width, 1);
    (window, vscroll, hscroll)
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 6 || area.width < 16 {
        return;
    }
    let (window, vscroll, hscroll) = layout(area);
    st.vp.set_window(window);

    // Node rects in canvas coordinates (the model; independent of the camera).
    let node_rects: Vec<(TileId, Rect)> = st
        .canvas
        .nodes()
        .iter()
        .map(|n| (n.id, Rect::new(n.place.x, n.place.y, n.place.width, n.place.height)))
        .collect();
    let obstacles: Vec<Rect> = node_rects.iter().map(|&(_, r)| r).collect();

    // Route every wire over the whole canvas (stable in canvas space).
    let free: HashSet<(u16, u16)> = {
        let cr = Rect::new(0, 0, CANVAS_W, CANVAS_H);
        free_cells_in_window(cr, &obstacles, 0, cr).into_iter().collect()
    };
    let rect_of = |id: TileId| node_rects.iter().find(|&&(i, _)| i == id).map(|&(_, r)| r);
    let mut reqs: Vec<RouteRequest> = Vec::new();
    let mut colors: Vec<Color> = Vec::new();
    for (i, &(a, b)) in st.wires.iter().enumerate() {
        let (Some(ra), Some(rb)) = (rect_of(a), rect_of(b)) else { continue };
        let (src, dst) = (out_socket(ra.height), in_socket(rb.height));
        let (Some(start), Some(goal)) = (src.attach(ra), dst.attach(rb)) else { continue };
        reqs.push(RouteRequest::new(start, goal, src.outward().opposite(), dst.outward().opposite()));
        colors.push(NET_COLORS[i % NET_COLORS.len()]);
    }
    let (connectors, styles): (Vec<Connector>, Vec<Style>) = route_all(&free, &reqs, BEND, CROSS)
        .into_iter()
        .zip(&colors)
        .filter_map(|(c, &col)| c.map(|c| (c, Style::default().fg(col))))
        .unzip();

    // Connectors: render the visible canvas sub-rect at the window origin (culls).
    render_connectors(buf, st.vp.visible(), st.vp.origin(), &connectors, &styles,
        &obstacles, LineWeight::Light);

    // Nodes: cull to the window+margin, then draw each visible one.
    let mut shown = 0;
    for (id, crect) in &node_rects {
        if st.vp.is_visible(*crect, MARGIN) && draw_node(buf, *id, *crect, &st.vp) {
            shown += 1;
        }
    }

    // Exact 2D scrollbars (the canvas bounding box is known).
    let bar = Style::default().fg(Color::DarkGray);
    render_scrollbar(buf, vscroll, st.vp.v_metrics(), bar);
    render_scrollbar(buf, hscroll, st.vp.h_metrics(), bar);

    // ── Help & status ──────────────────────────────────────────────────────
    buf.set_string(0, 0, "viewport — hjkl/arrows/drag/wheel:pan  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let (px, py) = st.vp.pan();
    let status = format!(" pan ({px},{py})/{:?}   {} of {} nodes shown   {} wires",
        st.vp.max_pan(), shown, node_rects.len(), connectors.len());
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, area.height - 1, " ", sstyle);
    }
    buf.set_string(0, area.height - 1, &status, sstyle);
}

/// Draw one node, projected and clipped to the window. Returns whether it drew.
fn draw_node(buf: &mut Buffer, id: TileId, crect: Rect, vp: &Viewport) -> bool {
    let Some(screen) = vp.project(crect) else { return false };
    draw_box(buf, screen, Borders::ALL, &BorderStyle {
        weight: LineWeight::Light,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::Gray),
    });
    // Label and sockets only when the node is fully visible (an unclipped box), so
    // they never land at the wrong offset on a node straddling an edge.
    if screen.width == crect.width && screen.height == crect.height {
        if screen.width > 6 {
            buf.set_string(screen.x + 2, screen.y + screen.height / 2, &format!("node {id}"),
                Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
        }
        let sock = Style::default().fg(Color::Green);
        draw_socket(buf, screen, &in_socket(screen.height), true, sock);
        draw_socket(buf, screen, &out_socket(screen.height), true, sock);
    }
    true
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
    let (window, _, _) = layout(size);
    let mut st = State::new(window);
    loop {
        term.draw(|buf| render(buf, &mut st))?;
        match poll_event(Duration::from_millis(50))? {
            None | Some(Event::Resize(_, _)) => {}
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Left | KeyCode::Char('h') => st.vp.pan_by(-2, 0),
                KeyCode::Right | KeyCode::Char('l') => st.vp.pan_by(2, 0),
                KeyCode::Up | KeyCode::Char('k') => st.vp.pan_by(0, -1),
                KeyCode::Down | KeyCode::Char('j') => st.vp.pan_by(0, 1),
                _ => {}
            },
            Some(Event::Mouse(m)) => handle_mouse(&mut st, m),
            _ => {}
        }
    }
    Ok(())
}

/// Drag the canvas to pan (grab-and-move), and scroll the wheel to pan vertically.
fn handle_mouse(st: &mut State, m: crossterm::event::MouseEvent) {
    let (mx, my) = (m.column, m.row);
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => st.drag = Some((mx, my)),
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some((px, py)) = st.drag {
                // Dragging right/down pulls the canvas with the cursor → pan back.
                st.vp.pan_by(px as i32 - mx as i32, py as i32 - my as i32);
                st.drag = Some((mx, my));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => st.drag = None,
        MouseEventKind::ScrollUp => st.vp.pan_by(0, -2),
        MouseEventKind::ScrollDown => st.vp.pan_by(0, 2),
        MouseEventKind::ScrollLeft => st.vp.pan_by(-2, 0),
        MouseEventKind::ScrollRight => st.vp.pan_by(2, 0),
        _ => {}
    }
}
