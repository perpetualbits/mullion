// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 11 demo — semantic (level-of-detail) zoom (design note §5.6).
//!
//! A grid of tiles laid out by `layout::solve`. Zoom **grows the focused tile
//! through the solver** — its `Fill` weight eases up, so the solver itself expands
//! it while its neighbours reflow and shrink. As a tile gains (or loses) cells it
//! crosses **discrete LoD thresholds**, swapping its renderer:
//!
//!   collapsed (·) → titled → titled + ports → full internal graph
//!
//! So zooming in on a tile smoothly reveals more structure, ending in a little
//! wired node-graph (Phases 6–8) drawn inside it; zooming out collapses it back.
//!
//! Keys
//!   Tab            focus the next tile
//!   space or z     zoom the focused tile in / out (animated)
//!   q              quit

use std::collections::HashSet;
use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle},
    float::free_cells_in_window,
    label::Side,
    layout::{self, Constraint, Node, Orientation, Size},
    poll_event,
    route::{render as render_connectors, route_all, Connector, RouteRequest},
    socket::{draw_socket, Flow, Socket},
    style::{Color, Modifier, Style},
    Buffer, FloatRect, GraphCanvas, LineWeight, Lod, LodScale, Rect, Terminal, TileId, Zoom,
};

const COLS: usize = 3;
const ROWS: usize = 2;
const MAX_WEIGHT: u16 = 400;
const STEP: f32 = 0.06; // zoom easing speed per frame

struct State {
    zoom: Zoom,
    /// Raw zoom target (0 = grid, 1 = focused tile maximised) and current value.
    target: f32,
    raw: f32,
    scale: LodScale,
}

impl State {
    fn new() -> Self {
        Self { zoom: Zoom::new(1, MAX_WEIGHT), target: 1.0, raw: 0.0, scale: LodScale::default() }
    }
}

/// Build the grid as a vertical split of horizontal splits, giving the focus tile's
/// row and column the zoom's eased `Fill` weight so the solver grows it.
fn build_grid(zoom: &Zoom) -> Node {
    let (frow, fcol) = ((zoom.focus as usize - 1) / COLS, (zoom.focus as usize - 1) % COLS);
    let big = zoom.weight(zoom.focus);
    let rows = (0..ROWS).map(|r| {
        let cols = (0..COLS).map(|c| {
            let id = (r * COLS + c) as TileId + 1;
            let w = if r == frow && c == fcol { big } else { 1 };
            (Constraint::new(Size::Fill(w)), Node::Tile(id))
        });
        let row_w = if r == frow { big } else { 1 };
        (Constraint::new(Size::Fill(row_w)),
            Node::Split { orientation: Orientation::Horizontal, children: cols.collect() })
    });
    Node::Split { orientation: Orientation::Vertical, children: rows.collect() }
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 6 || area.width < 20 {
        return;
    }
    let grid_area = Rect::new(0, 1, area.width, area.height - 2);
    let mut root = build_grid(&st.zoom);
    let tiles = layout::solve(&mut root, grid_area);

    for (id, rect) in &tiles {
        if rect.width >= 2 && rect.height >= 2 {
            let lod = Lod::for_rect(*rect, st.scale);
            draw_tile(buf, *id, *rect, lod, *id == st.zoom.focus);
        }
    }

    let focus_lod = tiles
        .iter()
        .find(|(i, _)| *i == st.zoom.focus)
        .map(|(_, r)| Lod::for_rect(*r, st.scale));
    buf.set_string(0, 0, "lod-zoom — Tab:focus  space/z:zoom  q:quit",
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    let status = format!(" focus tile {}  zoom {:>3.0}%  lod {:?}",
        st.zoom.focus, st.zoom.progress() * 100.0, focus_lod.unwrap_or(Lod::Collapsed));
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, area.height - 1, " ", sstyle);
    }
    buf.set_string(0, area.height - 1, &status, sstyle);
}

/// Draw one tile at the given level of detail.
fn draw_tile(buf: &mut Buffer, id: TileId, rect: Rect, lod: Lod, focused: bool) {
    let (weight, color) = if focused {
        (LineWeight::Heavy, Color::Cyan)
    } else {
        (LineWeight::Light, Color::Gray)
    };
    draw_box(buf, rect, Borders::ALL, &BorderStyle {
        weight, corners: CornerStyle::Rounded, style: Style::default().fg(color),
    });
    let cx = rect.x + rect.width / 2;
    let cy = rect.y + rect.height / 2;
    match lod {
        Lod::Collapsed => {
            // Just a marker that the tile is here.
            buf.set_grapheme(cx, cy, "·", Style::default().fg(color));
        }
        Lod::Titled => {
            title(buf, id, rect, color);
        }
        Lod::Ported => {
            title(buf, id, rect, color);
            ports(buf, rect);
        }
        Lod::Full => {
            ports(buf, rect);
            internal_graph(buf, rect);
            title(buf, id, rect, color);
        }
    }
}

fn title(buf: &mut Buffer, id: TileId, rect: Rect, color: Color) {
    let label = format!(" node {id} ");
    if (label.len() as u16) < rect.width {
        let x = rect.x + (rect.width - label.len() as u16) / 2;
        buf.set_string(x, rect.y, &label, Style::default().fg(color).add_modifier(Modifier::BOLD));
    }
}

fn ports(buf: &mut Buffer, rect: Rect) {
    let s = Style::default().fg(Color::Green);
    draw_socket(buf, rect, &Socket::new(Side::Left, rect.height / 2, Flow::In, 0), true, s);
    draw_socket(buf, rect, &Socket::new(Side::Right, rect.height / 2, Flow::Out, 0), true, s);
}

/// A small wired node-graph (Phases 6–8) drawn inside the tile's interior.
fn internal_graph(buf: &mut Buffer, rect: Rect) {
    let interior = Rect::new(rect.x + 2, rect.y + 2, rect.width.saturating_sub(4), rect.height.saturating_sub(4));
    if interior.width < 14 || interior.height < 7 {
        return;
    }
    let (cw, ch) = (interior.width, interior.height);
    let nw = (cw / 2).saturating_sub(3).clamp(8, 16);
    let nh = (ch / 2).clamp(5, 7);
    let mut g = GraphCanvas::new(cw, ch);
    g.add(1, FloatRect::new(0, 0, nw, nh));
    g.add(2, FloatRect::new(cw - nw, ch - nh, nw, nh));
    let rects: Vec<(TileId, Rect)> =
        g.nodes().iter().map(|n| (n.id, Rect::new(n.place.x, n.place.y, n.place.width, n.place.height))).collect();
    let obstacles: Vec<Rect> = rects.iter().map(|&(_, r)| r).collect();
    let cr = Rect::new(0, 0, cw, ch);
    let free: HashSet<(u16, u16)> = free_cells_in_window(cr, &obstacles, 0, cr).into_iter().collect();

    let out = Socket::new(Side::Right, nh / 2, Flow::Out, 0);
    let inp = Socket::new(Side::Left, nh / 2, Flow::In, 0);
    let connectors: Vec<Connector> = match (out.attach(rects[0].1), inp.attach(rects[1].1)) {
        (Some(s), Some(g2)) => {
            let req = [RouteRequest::new(s, g2, out.outward().opposite(), inp.outward().opposite())];
            route_all(&free, &req, 4, 8).into_iter().flatten().collect()
        }
        _ => Vec::new(),
    };
    let styles = vec![Style::default().fg(Color::Yellow); connectors.len()];
    render_connectors(buf, cr, (interior.x, interior.y), &connectors, &styles, &obstacles, LineWeight::Light);
    for (_, r) in &rects {
        let screen = Rect::new(interior.x + r.x, interior.y + r.y, r.width, r.height);
        draw_box(buf, screen, Borders::ALL, &BorderStyle {
            weight: LineWeight::Light, corners: CornerStyle::Rounded, style: Style::default().fg(Color::Gray),
        });
        ports(buf, screen);
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
        match poll_event(Duration::from_millis(33))? {
            // Ease the zoom toward its target each idle frame.
            None | Some(Event::Resize(_, _)) if (st.raw - st.target).abs() > f32::EPSILON => {
                st.raw += (st.target - st.raw).clamp(-STEP, STEP);
                st.raw = st.raw.clamp(0.0, 1.0);
                st.zoom.set_progress(st.raw);
            }
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Tab => {
                    let n = (COLS * ROWS) as TileId;
                    st.zoom.focus = st.zoom.focus % n + 1;
                }
                KeyCode::Char(' ') | KeyCode::Char('z') => {
                    st.target = if st.target > 0.5 { 0.0 } else { 1.0 };
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}
