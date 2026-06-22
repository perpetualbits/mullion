// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 5 demo (multi-tile) — text running around several floating tiles, with
//! mixed bidirectional content (design note §3.5).
//!
//! Three tiles carve holes out of one paragraph. The text mixes English (LTR) and
//! Arabic (RTL), so within each slot you see bidi reordering; press `d` to flip
//! the base direction and watch the within-row slot order reverse too (right-of-
//! tile slot fills first under RTL).
//!
//! Keys
//!   Tab                  select the next tile
//!   ← ↓ ↑ → or h j k l    move the selected tile (text reflows around all three)
//!   [ / ]                shrink / grow the gutter kept clear around the tiles
//!   d                    toggle LTR ↔ RTL base direction
//!   q                    quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{BorderStyle, Borders, CornerStyle},
    poll_event,
    runaround::{flow, render_flow},
    style::{Color, Modifier, Style},
    text::BaseDirection,
    Buffer, LineWeight, Rect, Terminal,
};

// A paragraph mixing English (LTR) and Arabic (RTL) so both the runaround and the
// bidi reordering inside each slot are visible.
const TEXT: &str = "mullion flows wrapped text around floating tiles by reading the \
free space as a stream of slots. النص العربي يتدفق حول البلاطات من اليمين إلى اليسار. \
Each row left and right of a tile is its own slot, and the obstacle-free rows are \
simply one slot each, so the very same path covers flat text and runaround alike.";

// ── Demo state ──────────────────────────────────────────────────────────────────

/// A tile's parent-local placement.
#[derive(Clone, Copy)]
struct Tile {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

struct State {
    tiles: [Tile; 3],
    active: usize,
    gutter: u16,
    rtl: bool,
}

impl State {
    fn new() -> Self {
        Self {
            tiles: [
                Tile { x: 10, y: 1, w: 12, h: 4 },
                Tile { x: 34, y: 5, w: 14, h: 5 },
                Tile { x: 6, y: 10, w: 16, h: 4 },
            ],
            active: 0,
            gutter: 1,
            rtl: false,
        }
    }

    fn base(&self) -> BaseDirection {
        if self.rtl { BaseDirection::Rtl } else { BaseDirection::Ltr }
    }

    /// Clamp every tile fully inside a `parent`-sized area.
    fn clamp_to(&mut self, parent: Rect) {
        for t in &mut self.tiles {
            t.x = t.x.min(parent.width.saturating_sub(t.w));
            t.y = t.y.min(parent.height.saturating_sub(t.h));
        }
    }

    fn nudge(&mut self, dx: i32, dy: i32) {
        let t = &mut self.tiles[self.active];
        t.x = (t.x as i32 + dx).max(0) as u16;
        t.y = (t.y as i32 + dy).max(0) as u16;
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 6 || area.width < 20 {
        return;
    }
    let help_y = 0;
    let status_y = area.height - 1;
    let parent = Rect::new(0, 1, area.width, status_y - 1);

    st.clamp_to(parent);
    // Absolute rects for the three obstacles.
    let rects: Vec<Rect> = st
        .tiles
        .iter()
        .map(|t| Rect::new(parent.x + t.x, parent.y + t.y, t.w, t.h))
        .collect();

    // Flow the paragraph around all three tiles over the parent's rows.
    let placed = flow(TEXT, parent, &rects, st.gutter, st.base(), parent.y..parent.bottom());
    render_flow(buf, &placed, Style::default().fg(Color::White));

    // Draw the tiles on top so they read as solid holes; highlight the active one.
    for (i, &r) in rects.iter().enumerate() {
        let active = i == st.active;
        let color = if active { Color::Cyan } else { Color::DarkGray };
        // Blank the interior, then frame it.
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                buf.set_string(x, y, " ", Style::default());
            }
        }
        let bstyle = BorderStyle {
            weight: if active { LineWeight::Heavy } else { LineWeight::Light },
            corners: CornerStyle::Rounded,
            style: Style::default().fg(color),
        };
        mullion::border::draw_box(buf, r, Borders::ALL, &bstyle);
        if r.width > 4 && r.height > 1 {
            let label = format!("T{}", i + 1);
            buf.set_string(r.x + 2, r.y + r.height / 2, &label, Style::default().fg(color).add_modifier(Modifier::BOLD));
        }
    }

    // Help & status.
    let help = "runaround (multi) — Tab:select  hjkl/arrows:move  [ ]:gutter  d:LTR/RTL  q:quit";
    buf.set_string(0, help_y, help, Style::default().fg(Color::White).add_modifier(Modifier::BOLD));

    let status = format!(
        " {}  active:T{}  gutter:{}  tiles:3  flowed lines:{}",
        if st.rtl { "RTL" } else { "LTR" },
        st.active + 1,
        st.gutter,
        placed.iter().filter(|p| !p.line.cells.is_empty()).count(),
    );
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, status_y, " ", sstyle);
    }
    buf.set_string(0, status_y, &status, sstyle);
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
                KeyCode::Tab => st.active = (st.active + 1) % st.tiles.len(),
                KeyCode::Left | KeyCode::Char('h') => st.nudge(-1, 0),
                KeyCode::Right | KeyCode::Char('l') => st.nudge(1, 0),
                KeyCode::Up | KeyCode::Char('k') => st.nudge(0, -1),
                KeyCode::Down | KeyCode::Char('j') => st.nudge(0, 1),
                KeyCode::Char('[') => st.gutter = st.gutter.saturating_sub(1),
                KeyCode::Char(']') => st.gutter = (st.gutter + 1).min(6),
                KeyCode::Char('d') => st.rtl = !st.rtl,
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}
