// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Mullion interactive demo — Phases 1–5.
//!
//! Layout
//! ```text
//! ┌────────────────────┬────────────────────┬────────────────────┐
//! │ Alpha (0)          │ Beta (1)            │ Gamma (2)          │
//! │                    │                     │                    │
//! ├────────────────────┴────────────────────┴────────────────────┤
//! │ Delta(10)   Epsilon(11)   Zeta(12)   Eta(13)   Theta(14) ──► │
//! │                                                               │
//! └───────────────────────────────────────────────────────────────┘
//!  focus: Alpha (0)   ^Wj/k:move  ^Wz:zoom  ^WZ:unzoom  q:quit
//! ```
//!
//! The bottom strip is a horizontal carousel — only 3 of the 5 tiles fit at
//! once.  Navigate into the carousel tiles (`^W j` past Gamma) and watch them
//! scroll into view automatically via [`Tree::scroll_focus_into_view`].
//!
//! Zoom a pane with `^W z`; it fills the whole render area.  `^W Z` steps back
//! out.  From inside a zoomed carousel you can still navigate between its tiles.
//!
//! Keys (two-step: press Ctrl+w, release, then press the second key)
//!   Ctrl+w, then j / Tab      move focus to next tile (DFS order)
//!   Ctrl+w, then k / BackTab  move focus to previous tile
//!   Ctrl+w, then z            zoom into focused pane
//!   Ctrl+w, then Z            zoom out one level
//!   q                         quit

use std::{io, time::Duration};

use crossterm::event::Event;

use mullion::{
    backend::CrosstermBackend,
    focus_override, render_shared,
    input::{InputRouter, KeyCode, KeyOutcome},
    layout::{Constraint, Node, Orientation, Size, TileId},
    poll_event,
    style::{Color, Modifier, Style},
    Buffer, LineWeight, Rect, Terminal, Tree,
};

// ── Tile metadata ─────────────────────────────────────────────────────────────

fn tile_name(id: TileId) -> &'static str {
    match id {
        0  => "Alpha",
        1  => "Beta",
        2  => "Gamma",
        10 => "Delta",
        11 => "Epsilon",
        12 => "Zeta",
        13 => "Eta",
        14 => "Theta",
        _  => "?",
    }
}

// ── Tree factory ──────────────────────────────────────────────────────────────

/// Build the demo tree:
///   V-Split
///     ├─ H-Split [Tile(0) │ Tile(1) │ Tile(2)]
///     └─ Carousel(id=99, H) [Tile(10) … Tile(14)]  24 cols each
///
/// The carousel holds 5 tiles of 24 columns each (120 cols total).  A typical
/// 80-col terminal shows ~3 at once, so navigating to the far-right tiles
/// demonstrates `scroll_focus_into_view`.
fn build_tree() -> Tree {
    let top = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(0)),
            (Constraint::new(Size::Fill(1)), Node::Tile(1)),
            (Constraint::new(Size::Fill(1)), Node::Tile(2)),
        ],
    };
    let carousel = Node::Carousel {
        id: 99,
        orientation: Orientation::Horizontal,
        scroll: 0,
        children: (10u64..15).map(|i| (24u16, Node::Tile(i))).collect(),
    };
    let root = Node::Split {
        orientation: Orientation::Vertical,
        children: vec![
            (Constraint::new(Size::Fill(1)), top),
            (Constraint::new(Size::Fixed(8)), carousel),
        ],
    };
    Tree::new(root)
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, tree: &mut Tree) {
    let area = buf.area;
    if area.height < 4 {
        return;
    }

    // Reserve the bottom row for the status line.
    let status_y = area.height - 1;
    let render_area = Rect::new(0, 0, area.width, status_y);

    // Focus highlight: draw the focused tile's borders at Heavy weight so they
    // stand out against the Light-weight dividers of their neighbours.
    let overrides = focus_override(tree, LineWeight::Heavy);
    let border_style = Style::default().fg(Color::DarkGray);

    // Paint all borders and collect the content rect for every visible leaf.
    let rects = render_shared(
        buf,
        tree.effective_root_mut(),
        render_area,
        LineWeight::Light,
        &border_style,
        &overrides,
    );

    // Draw tile content.
    let focused = tree.focus();
    let hi    = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let normal = Style::default().fg(Color::White);
    let dim    = Style::default().fg(Color::DarkGray);
    let accent = Style::default().fg(Color::Cyan);

    for &(id, crect) in &rects {
        if crect.is_empty() {
            continue;
        }
        let is_focused = focused == Some(id);
        buf.set_string(crect.x, crect.y, tile_name(id), if is_focused { hi } else { normal });
        if crect.height > 1 {
            buf.set_string(crect.x, crect.y + 1, &format!("#{}", id), dim);
        }
        if is_focused && crect.height > 2 {
            buf.set_string(crect.x, crect.y + 2, "< focused", accent);
        }
    }

    // Status line.
    let focus_str = match focused {
        Some(id) => format!("{} ({})", tile_name(id), id),
        None     => "none".into(),
    };
    let zoom_str = if tree.is_zoomed() {
        format!(" [ZOOM\u{00d7}{}]", tree.zoom_depth())
    } else {
        String::new()
    };
    let status = format!(
        " focus: {}{}  C-w j/k:move  C-w z:zoom  C-w Z:unzoom  q:quit",
        focus_str, zoom_str,
    );
    let st = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, status_y, " ", st);
    }
    buf.set_string(0, status_y, &status, st);
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
    let mut tree = build_tree();
    let mut router = InputRouter::new();

    loop {
        term.draw(|buf| {
            let area = buf.area;
            // Nudge any carousel whose focused child is out of view before
            // rendering, so the focused tile is always flush-visible.
            tree.scroll_focus_into_view(area);
            render(buf, &mut tree);
        })?;

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
