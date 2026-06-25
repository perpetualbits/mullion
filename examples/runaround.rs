// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 5 demo — word-wrap runaround (design note §3.5).
//!
//! A paragraph flows around a floating tile (the "figure"). Move the tile and the
//! text reflows around it, live — bounded by the visible rows, not the whole
//! document. The free space the text reads around is exactly the Phase 1
//! slot model (§3.15); the wrapping is the Phase 2 engine (§3.16).
//!
//! Press `d` to flip the base direction. Under LTR the text fills the
//! left-of-tile slot of each row first; under RTL it fills the right-of-tile slot
//! first — the §3.5 within-row slot-order flip.
//!
//! Keys
//!   ← ↓ ↑ → or h j k l   move the figure (text reflows around it)
//!   [ / ]                shrink / grow the gutter kept clear around the figure
//!   d                    toggle LTR ↔ RTL flow direction
//!   q                    quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{BorderStyle, CornerStyle},
    panel::{draw_panel, Panel},
    poll_event,
    runaround::flow,
    style::{Color, Modifier, Style},
    text::BaseDirection,
    Buffer, LineWeight, Rect, Terminal,
};

const TEXT: &str = "mullion treats the free space around a floating tile as an \
ordered stream of slots, one per row to the left and right of the obstacle, and \
flows wrapped tokens into those slots instead of into full-width lines. The \
obstacle-free case is simply one slot per row, so the very same code path covers \
both flat text and runaround. Move the figure and only the visible rows reflow.";

// ── Demo state ──────────────────────────────────────────────────────────────────

/// The figure's parent-local placement, the gutter, and the flow direction.
struct State {
    fx: u16,
    fy: u16,
    fw: u16,
    fh: u16,
    gutter: u16,
    rtl: bool,
}

impl State {
    fn new() -> Self {
        Self { fx: 18, fy: 4, fw: 16, fh: 6, gutter: 1, rtl: false }
    }

    fn base(&self) -> BaseDirection {
        if self.rtl { BaseDirection::Rtl } else { BaseDirection::Ltr }
    }

    /// Clamp the figure fully inside a `parent`-sized area.
    fn clamp_to(&mut self, parent: Rect) {
        self.fx = self.fx.min(parent.width.saturating_sub(self.fw));
        self.fy = self.fy.min(parent.height.saturating_sub(self.fh));
    }

    fn nudge(&mut self, dx: i32, dy: i32) {
        self.fx = (self.fx as i32 + dx).max(0) as u16;
        self.fy = (self.fy as i32 + dy).max(0) as u16;
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────────

fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 5 || area.width < 16 {
        return;
    }
    let help_y = 0;
    let status_y = area.height - 1;
    let parent = Rect::new(0, 1, area.width, status_y - 1);

    st.clamp_to(parent);
    // The figure's absolute rect, the obstacle the text flows around.
    let figure = Rect::new(parent.x + st.fx, parent.y + st.fy, st.fw, st.fh);

    // Flow the paragraph around the figure over the parent's rows.
    let placed = flow(TEXT, parent, &[figure], st.gutter, st.base(), parent.y..parent.bottom());
    mullion::runaround::render_flow(buf, &placed, Style::default().fg(Color::White));

    // Draw the figure on top so it reads as a solid obstacle: a filled panel
    // clears the interior (the flow never wrote there, but be safe) and frames it.
    let bstyle = BorderStyle {
        weight: LineWeight::Heavy,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::Cyan),
    };
    draw_panel(buf, figure, &Panel::new(bstyle).fill(Style::default()));
    if figure.width > 8 && figure.height > 1 {
        buf.set_string(
            figure.x + 2,
            figure.y + figure.height / 2,
            "FIGURE",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        );
    }

    // Help & status.
    let help = "runaround — hjkl/arrows:move figure  [ ]:gutter  d:LTR/RTL  q:quit";
    buf.set_string(0, help_y, help, Style::default().fg(Color::White).add_modifier(Modifier::BOLD));

    let status = format!(
        " {}  figure:({},{}) {}×{}  gutter:{}  flowed lines:{}",
        if st.rtl { "RTL" } else { "LTR" },
        st.fx, st.fy, st.fw, st.fh, st.gutter,
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
