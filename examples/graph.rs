// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 7 demo — manual node placement on a graph canvas (design note §5.4/§5.7).
//!
//! A [`GraphCanvas`] holds a few nodes (bordered tiles carrying Phase 6 sockets).
//! Select one with `Tab`, nudge it with the arrows / `hjkl`, snap it to the grid
//! with `s` — or drag any node with the mouse. Nodes keep stable ids and stay
//! inside the canvas. No connectors yet (that is Phase 8); the sockets just show
//! where wires will attach.
//!
//! Keys
//!   Tab                  select the next node
//!   ← ↓ ↑ → or h j k l    nudge the selected node one cell
//!   s                    snap the selected node to the grid
//!   mouse drag           reposition a node
//!   q                    quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent, MouseButton, MouseEventKind};

use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle},
    label::Side,
    mouse::tile_at,
    poll_event,
    socket::{draw_socket, Flow, Socket},
    style::{Color, Modifier, Style},
    Buffer, FloatRect, GraphCanvas, LineWeight, Rect, Terminal, TileId,
};

const GRID: u16 = 4;

struct State {
    canvas: GraphCanvas,
    selected: TileId,
    /// While dragging: the grabbed node and the cursor's offset within it.
    drag: Option<(TileId, u16, u16)>,
}

impl State {
    fn new() -> Self {
        let mut canvas = GraphCanvas::new(80, 24).with_grid(GRID);
        canvas.add(1, FloatRect::new(4, 2, 16, 7));
        canvas.add(2, FloatRect::new(30, 9, 18, 8));
        canvas.add(3, FloatRect::new(10, 14, 16, 7));
        Self { canvas, selected: 1, drag: None }
    }
}

/// Bookended-socket anchor offsets down an edge `edge_len` long: `count`, centred,
/// stride 3 so the `┴●┬` stacks never collide.
fn port_offsets(edge_len: u16, count: usize) -> Vec<u16> {
    if edge_len < 5 {
        return Vec::new();
    }
    let (lo, hi) = (2u16, edge_len - 3);
    let max_n = ((hi - lo) / 3 + 1) as usize;
    let n = count.min(max_n);
    if n == 0 {
        return Vec::new();
    }
    let start = lo + ((hi - lo) - (n as u16 - 1) * 3) / 2;
    (0..n).map(|k| start + k as u16 * 3).collect()
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 5 || area.width < 10 {
        return;
    }
    let help_y = 0;
    let status_y = area.height - 1;
    let window = Rect::new(0, 1, area.width, status_y - 1);
    st.canvas.resize(window.width, window.height);

    // Faint grid dots to make snapping legible.
    for gy in (0..window.height).step_by(GRID as usize) {
        for gx in (0..window.width).step_by(GRID as usize) {
            buf.set_string(window.x + gx, window.y + gy, "·", Style::default().fg(Color::DarkGray));
        }
    }

    for (id, rect) in st.canvas.solve(window) {
        if rect.is_empty() {
            continue;
        }
        draw_node(buf, id, rect, id == st.selected);
    }

    // ── Help & status ──────────────────────────────────────────────────────
    buf.set_string(0, help_y, "graph — Tab:select  hjkl/arrows:nudge  s:snap  drag:move  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let p = st.canvas.place(st.selected);
    let status = match p {
        Some(r) => format!(" node {} at ({},{}) {}×{}   grid:{}   {} nodes",
            st.selected, r.x, r.y, r.width, r.height, GRID, st.canvas.nodes().len()),
        None => " no node".into(),
    };
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
        weight,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(color),
    });
    if rect.width > 6 {
        buf.set_string(rect.x + 2, rect.y + rect.height / 2, &format!("node {id}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD));
    }
    // Two input sockets on the left edge, two outputs on the right (unconnected).
    let sock = Style::default().fg(Color::DarkGray);
    for off in port_offsets(rect.height, 2) {
        draw_socket(buf, rect, &Socket::new(Side::Left, off, Flow::In, 0), false, sock);
        draw_socket(buf, rect, &Socket::new(Side::Right, off, Flow::Out, 0), false, sock);
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
    let mut st = State::new();
    loop {
        term.draw(|buf| render(buf, &mut st))?;
        match poll_event(Duration::from_millis(50))? {
            None | Some(Event::Resize(_, _)) => {}
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Tab => {
                    // Cycle selection across node ids in their current order.
                    let ids: Vec<TileId> = st.canvas.nodes().iter().map(|n| n.id).collect();
                    if let Some(i) = ids.iter().position(|&i| i == st.selected) {
                        st.selected = ids[(i + 1) % ids.len()];
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => st.canvas.nudge(st.selected, -1, 0),
                KeyCode::Right | KeyCode::Char('l') => st.canvas.nudge(st.selected, 1, 0),
                KeyCode::Up | KeyCode::Char('k') => st.canvas.nudge(st.selected, 0, -1),
                KeyCode::Down | KeyCode::Char('j') => st.canvas.nudge(st.selected, 0, 1),
                KeyCode::Char('s') => st.canvas.snap_to_grid(st.selected),
                _ => {}
            },
            Some(Event::Mouse(m)) => handle_mouse(term, &mut st, m)?,
            _ => {}
        }
    }
    Ok(())
}

/// Mouse drag: grab the node under the cursor, then track it until release.
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
            let rects = st.canvas.solve(window);
            if let Some(id) = tile_at(&rects, mx, my) {
                st.selected = id;
                let r = rects.iter().find(|(i, _)| *i == id).unwrap().1;
                st.drag = Some((id, mx - r.x, my - r.y)); // cursor offset within the node
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some((id, ox, oy)) = st.drag {
                // Screen → canvas-local: subtract the window origin and grab offset.
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
