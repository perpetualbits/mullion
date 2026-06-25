// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Showcase: a single runnable demo of mullion's distinctive features.
//!
//! Layout
//! ```text
//! ┌─ mullion showcase ─── smooth-scroll · labels · zoom ──────────────────────┐
//! │ CPU  42% │ MEM  3.2 GB │ NET  1.2MB/s                   ← render_shared  │
//! ├──────────┼─────────────┼──────────────────────────────────────────────────┤
//! │ Node-00 (alpha)  cpu: 13%  mem: …   ← marquee on top border               │
//! │l ╔══════════════════════╗  cpu: 13%  mem: 128MB  net: 17KB/s              │
//! │o ║░░░░░░░░░░░░░░░░░░░░░░╗  ← animated bar with cyan streamer              │
//! │a ║                      ║                                                  │
//! │d ╚══════════════════════╝  ← "load" upright-stacked on left border        │
//! │                            ← render_carousel (smooth-scroll, 16 tiles)    │
//! └───────────────────────────────────────────────────────────────────────────┘
//!  Node-00 (alpha)  │  C-w j/k: move  C-w z/Z: zoom  q: quit
//! ```
//!
//! Engine features in this file:
//! - `render_carousel` — smooth-scroll, partial tiles genuinely cut off at edge
//! - `render_shared`   — header shared borders + `┼` junction (compose with carousel)
//! - `draw_label` horizontal marquee — per-tile name that scrolls when too long
//! - `draw_label` vertical upright-stacked — "load" on the left border
//! - `LineWeight::Heavy` on the focused tile; `Light` on others
//! - `scroll_focus_into_view` with the body rect so reveal matches render viewport
//! - `zoom_focus` / `zoom_out` — zoomed tile fills the body area
//! - 16 tiles; `render_carousel` only calls `draw_child` for visible ones
//! - Per-frame bar animation with a staggered cyan streamer; only changed cells
//!   are flushed by the diff engine so animation cost is proportional to bar width
//!
//! Keys (Ctrl+w prefix, then a second key)
//!   j / Tab      next tile        k / BackTab  previous tile
//!   z            zoom in          Z            zoom out
//!   q            quit

use std::{io, time::Duration};

use crossterm::event::Event;

use mullion::{
    backend::CrosstermBackend,
    draw_box, draw_label, label_period, render_carousel, render_shared,
    Align, BorderStyle, Borders, Buffer, Cell, Constraint, CornerStyle, Label,
    LineWeight, Node, Orientation, Rect, Side, Size, Terminal, TileId, Tree,
    poll_event,
    input::{InputRouter, Keymap, KeyCode, KeyOutcome},
    style::{Color, Modifier, Style},
};

// ── Layout constants ──────────────────────────────────────────────────────────

/// Rows per carousel tile including its top and bottom border.
const TILE_H: u16 = 6;
/// Number of node tiles in the vertical carousel.
const NUM_TILES: u64 = 16;
/// TileId offset for the three header summary tiles (must not collide with 0..NUM_TILES).
const HDR_BASE: TileId = 100;
/// Rows reserved for the header strip (including its own border).
const HEADER_H: u16 = 3;

// ── Node metadata ─────────────────────────────────────────────────────────────

const PHONETIC: [&str; 16] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot",
    "golf", "hotel", "india", "juliet", "kilo", "lima",
    "mike", "november", "oscar", "papa",
];

/// Short name shown in the status bar.
fn node_name(id: TileId) -> &'static str {
    PHONETIC[(id as usize) % PHONETIC.len()]
}

/// Long label text for the tile's top-border marquee.  Repeated so that it
/// always exceeds ~120 display columns and will visibly scroll.
fn node_label(id: TileId) -> String {
    let name = node_name(id);
    let cpu  = (id * 7  + 13) % 100;
    let mem  = 128 + (id * 37) % 896;
    let kbs  = (id * 51 + 17) % 9999;
    format!(
        "Node-{id:02} ({name})  cpu:{cpu:3}%  mem:{mem:4}MB  net:{kbs:5}KB/s  \
         Node-{id:02} ({name})  cpu:{cpu:3}%  mem:{mem:4}MB  net:{kbs:5}KB/s"
    )
}

// ── Tree factory ──────────────────────────────────────────────────────────────

/// Build a tree whose root is a vertical carousel of `NUM_TILES` node tiles.
fn build_tree() -> Tree {
    let carousel = Node::Carousel {
        id: 1,
        orientation: Orientation::Vertical,
        scroll: 0,
        children: (0..NUM_TILES).map(|i| (TILE_H, Node::Tile(i))).collect(),
    };
    Tree::new(carousel)
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Derive both sub-areas from the terminal size so every call site uses the same
/// formula and the body rect passed to `scroll_focus_into_view` matches the one
/// passed to `render_carousel`.
fn layout(area: Rect) -> (Rect, Rect, u16) {
    let header_h  = HEADER_H.min(area.height.saturating_sub(2));
    let status_y  = area.height.saturating_sub(1);
    let body_h    = status_y.saturating_sub(header_h);
    let header    = Rect::new(0, 0,       area.width, header_h);
    let body      = Rect::new(0, header_h, area.width, body_h);
    (header, body, status_y)
}

/// Render a complete frame into `buf`.
fn render_frame(buf: &mut Buffer, tree: &mut Tree, frame: u64) {
    let area = buf.area;
    if area.height < 4 { return; }

    let (header_area, body_area, status_y) = layout(area);

    render_header(buf, header_area, frame);
    render_body(buf, tree, frame, body_area);
    render_status(buf, status_y, area.width, tree);
}

/// Render the header using `render_shared` so shared borders and junctions are
/// visible, then overlay a horizontal marquee title on the top edge.
///
/// This is the `render_shared` half of the composition: the body below uses
/// `render_carousel`.  The two regions share no cells so no border conflicts arise.
fn render_header(buf: &mut Buffer, area: Rect, frame: u64) {
    if area.is_empty() { return; }

    // Build a throwaway H-split of three summary tiles.
    let mut node = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(HDR_BASE)),
            (Constraint::new(Size::Fill(1)), Node::Tile(HDR_BASE + 1)),
            (Constraint::new(Size::Fill(1)), Node::Tile(HDR_BASE + 2)),
        ],
    };
    let bdr_st = BorderStyle {
        weight: LineWeight::Light,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::DarkGray),
    };
    let crects = render_shared(buf, &mut node, area, &bdr_st, &[]);

    // Static summary content for each header tile.
    const SUMMARIES: [(&str, &str, Color); 3] = [
        ("CPU", " 42%",    Color::Green),
        ("MEM", " 3.2 GB", Color::Yellow),
        ("NET", " 1.2MB/s", Color::Cyan),
    ];
    for i in 0..SUMMARIES.len().min(crects.len()) {
        let (_, cr) = crects[i];
        if cr.is_empty() { continue; }
        let (metric, val, color) = SUMMARIES[i];
        // metric is ASCII so .len() == display cols
        buf.set_string(cr.x, cr.y, metric, Style::default().fg(Color::DarkGray));
        let vx = cr.x + metric.len() as u16;
        if vx < cr.x + cr.width {
            buf.set_string(vx, cr.y, val, Style::default().fg(color).add_modifier(Modifier::BOLD));
        }
    }

    // Marquee title on the top border edge — drawn after render_shared so the
    // title overwrites the border-line cells without touching corners/junctions.
    let title = "mullion showcase \u{2500}\u{2500}\u{2500} \
                 smooth-scroll \u{b7} labels \u{b7} zoom \u{b7} animation \u{b7} \
                 16 virtual nodes \u{2500}\u{2500}\u{2500} mullion showcase";
    let run_len = area.width.saturating_sub(2);
    let offset = label_period(title, run_len, Side::Top)
        .map(|p| (frame / 2) as u16 % p)
        .unwrap_or(0);
    draw_label(buf, area, &Label {
        text:   title.into(),
        side:   Side::Top,
        align:  Align::Center,
        offset,
    }, &Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
}

/// Render the body: either the full carousel (normal) or a single zoomed tile.
fn render_body(buf: &mut Buffer, tree: &mut Tree, frame: u64, body_area: Rect) {
    if body_area.is_empty() { return; }

    // Read focus and zoom state before any mutable borrow of tree.
    let focused_id = tree.focus();

    // Extract the zoomed tile id if we're zoomed into a single Tile node.
    // The shared borrow of tree via effective_root() ends at the closing `}`.
    let zoomed_tile: Option<TileId> = match tree.effective_root() {
        Node::Tile(id) => Some(*id),
        _ => None,
    };

    // draw_child captures only Copy values — no borrow of tree.
    let mut draw_child = |buf: &mut Buffer, id: TileId, rect: Rect| {
        paint_tile(buf, id, rect, focused_id, frame);
    };

    if let Some(tile_id) = zoomed_tile {
        // Zoomed: render the single tile filling the entire body rect.
        paint_tile(buf, tile_id, body_area, focused_id, frame);
    } else {
        // Normal: smooth-scroll carousel.  render_carousel calls draw_child only
        // for tiles that intersect the viewport, so all 16 tiles need not be drawn.
        render_carousel(buf, tree.effective_root_mut(), body_area, &mut draw_child);
    }
}

/// Paint one node tile into `buf` at its full outer `rect` (border included).
///
/// Called by `render_carousel`'s draw_child for each visible tile (drawing into
/// the temp buffer that is later blitted) and directly when a tile is zoomed to
/// fill the body.
fn paint_tile(buf: &mut Buffer, id: TileId, rect: Rect, focused_id: Option<TileId>, frame: u64) {
    let is_focused = focused_id == Some(id);

    // Border: heavy yellow when focused, light dark-gray otherwise.
    let (weight, bdr_color) = if is_focused {
        (LineWeight::Heavy, Color::Yellow)
    } else {
        (LineWeight::Light, Color::DarkGray)
    };
    draw_box(buf, rect, Borders::ALL, &BorderStyle {
        weight,
        corners: CornerStyle::Square,
        style: Style::default().fg(bdr_color),
    });

    // Top-border marquee: long enough to scroll on typical terminal widths.
    let label_text = node_label(id);
    let run_len    = rect.width.saturating_sub(2);
    let top_off    = label_period(&label_text, run_len, Side::Top)
        .map(|p| (frame / 2) as u16 % p)
        .unwrap_or(0);
    draw_label(buf, rect, &Label {
        text:   label_text,
        side:   Side::Top,
        align:  Align::Center,
        offset: top_off,
    }, &Style::default().fg(if is_focused { Color::Yellow } else { Color::White }));

    // Left-border vertical label: "load" stacked upright (one char per row).
    draw_label(buf, rect, &Label {
        text:   "load".into(),
        side:   Side::Left,
        align:  Align::Center,
        offset: 0,
    }, &Style::default().fg(Color::DarkGray));

    // Inner content (inside the four border cells).
    let inner = Rect::new(
        rect.x + 1,
        rect.y + 1,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );
    if inner.is_empty() { return; }

    // Row 0: static stats derived from tile id.
    let cpu = (id * 7  + 13) % 100;
    let mem = 128 + (id * 37) % 896;
    let kbs = (id * 51 + 17) % 9999;
    buf.set_string(inner.x, inner.y,
        &format!("cpu:{cpu:3}%  mem:{mem:4}MB  net:{kbs:5}KB/s"),
        Style::default().fg(Color::Gray));

    // Row 1: animated fill bar with a per-tile staggered cyan streamer.
    // The diff engine flushes only the cells that changed, so animation cost
    // is proportional to bar width, not screen area.
    if inner.height > 1 {
        paint_bar(buf, inner.x, inner.y + 1, inner.width, cpu as u16, id, frame);
    }
}

/// Draw an animated load bar of `width` cells.
///
/// `fill` cells (derived from `load_pct`) are drawn as `█` (green); the remainder
/// as `░` (dark gray).  A 3-cell cyan highlight window advances across the full bar
/// width at ~6 cells/sec (frame/3), offset by `id * 7` per tile so the streamers
/// are visually staggered rather than marching in sync.  The highlight is visible
/// only in the filled portion; in the empty portion it is indistinguishable from
/// the background `░`.
fn paint_bar(buf: &mut Buffer, x: u16, y: u16, width: u16, load_pct: u16, id: TileId, frame: u64) {
    if width == 0 { return; }

    let fill   = ((load_pct as u32 * width as u32) / 100).min(width as u32) as u16;
    let hl_len = 3u16.min(width);
    // Position wraps at bar boundary; per-tile phase offset staggers the streamers.
    let hl_pos = ((frame / 3).wrapping_add(id * 7) % width as u64) as u16;

    for i in 0..width {
        let filled = i < fill;
        // Detect whether this cell falls inside the wrapping highlight window.
        let in_hl = if hl_pos + hl_len <= width {
            i >= hl_pos && i < hl_pos + hl_len
        } else {
            // Window straddles the right edge: active region is [hl_pos, width) ∪ [0, wrap_end).
            i >= hl_pos || i < (hl_pos + hl_len) % width
        };

        let (glyph, fg) = match (filled, in_hl) {
            (true,  true)  => ("\u{2588}", Color::Cyan),     // █ streamer highlight
            (true,  false) => ("\u{2588}", Color::Green),    // █ normal fill
            (false, _)     => ("\u{2591}", Color::DarkGray), // ░ empty portion
        };
        buf.set_grapheme(x + i, y, glyph, Style::default().fg(fg));
    }
}

/// Render the bottom status row.
fn render_status(buf: &mut Buffer, y: u16, width: u16, tree: &Tree) {
    let st = Style::default().fg(Color::Black).bg(Color::Gray);
    // Fill the entire row with the status background before drawing text.
    buf.fill(Rect::new(0, y, width, 1), Cell::new(" ", st));

    let focus_str = match tree.focus() {
        Some(id) => format!("Node-{id:02} ({})", node_name(id)),
        None     => "no focus".into(),
    };
    let zoom_str = if tree.is_zoomed() {
        format!("  [ZOOM\u{d7}{}]", tree.zoom_depth())
    } else {
        String::new()
    };
    buf.set_string(0, y,
        &format!(" {focus_str}{zoom_str}  \u{2502}  C-w j/k: move  C-w z/Z: zoom  q: quit"),
        st);
}

// ── Main / event loop ─────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;
    let result = run(&mut term);
    term.leave()?;
    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut tree   = build_tree();
    let mut router = InputRouter::with_keymap(Keymap::vim_prefix());
    let mut frame  = 0u64;

    loop {
        term.draw(|buf| {
            let area = buf.area;

            // Compute the body rect using the same formula as render_frame so the
            // reveal viewport matches the render viewport exactly.
            let (_, body_area, _) = layout(area);

            // Auto-reveal: scroll the carousel so the focused tile is flush-visible
            // before rendering.  Must use body_area (not full area) so the scroll
            // calculation matches the carousel's render viewport.
            tree.scroll_focus_into_view(body_area);

            render_frame(buf, &mut tree, frame);
        })?;

        frame = frame.wrapping_add(1);

        match poll_event(Duration::from_millis(50))? {
            None | Some(Event::Resize(_, _)) => {}
            Some(Event::Key(key)) => {
                if let KeyOutcome::Forward(k) = router.handle(key, &mut tree) {
                    if k.code == KeyCode::Char('q') {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
