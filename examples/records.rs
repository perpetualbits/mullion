// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 3 demo — row virtualization over a `RecordSource` (design note §4.1).
//!
//! Scrolls a window over 100,000 keyed records while keeping only a few dozen
//! rows materialized at a time. Rows are drawn through the existing
//! [`ColumnGrid`](mullion::table::ColumnGrid); a scrollbar on the right shows the
//! position.
//!
//! The scrollbar has **two truth levels** (§6.2): with an exact source the thumb
//! is a solid `█` at a true ordinal; press `e` to switch to an *estimated* source
//! (length unknown, like a remote cursor) and the thumb becomes a `▒` shade to
//! show the position is an approximation, not faked precision.
//!
//! Keys
//!   ↑ / ↓ or k / j   scroll one row
//!   PgUp / PgDn      scroll one screen
//!   g / G            jump to top / bottom
//!   e                toggle exact ↔ estimated source (watch the scrollbar)
//!   q                quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{BorderStyle, Borders, CornerStyle},
    poll_event,
    render_scrollbar,
    style::{Color, Modifier, Style},
    table::{ColumnDef, ColumnGrid, ColumnKind},
    label::Align,
    Buffer, LineWeight, Rect, Terminal, VecRecordSource, VirtualList,
};

// ── Data ────────────────────────────────────────────────────────────────────

/// A record's value half: a generated name and a number.
type Record = (String, i64);
/// The concrete source type: keyed `u64` → `(name, value)`.
type Source = VecRecordSource<u64, Record>;

const TOTAL: u64 = 100_000;

/// Build the full dataset once (the in-memory reference source still materializes
/// it; the *window* is what stays small). `estimated` hides the length to model a
/// remote cursor.
fn build_source(estimated: bool) -> Source {
    let rows: Vec<(u64, Record)> = (0..TOTAL)
        .map(|id| (id, (format!("user_{id:05}"), (id as i64 * 37) % 1000)))
        .collect();
    let src = VecRecordSource::new(rows);
    if estimated {
        src.estimated()
    } else {
        src
    }
}

// ── Demo state ──────────────────────────────────────────────────────────────────

struct State {
    list: VirtualList<Source>,
    estimated: bool,
}

impl State {
    fn new() -> Self {
        // Viewport is re-set from the box height each frame; start with a guess.
        Self { list: VirtualList::new(build_source(false), 20, 32), estimated: false }
    }

    /// Rebuild the list against an exact or estimated source (resets to top).
    fn toggle_estimated(&mut self) {
        self.estimated = !self.estimated;
        let vp = self.list.viewport();
        self.list = VirtualList::new(build_source(self.estimated), vp, 32);
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────────

/// The three columns, shared by header and body so they stay aligned.
fn columns() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(8, ColumnKind::Text).with_align(Align::End), // id
        ColumnDef::fill(1, ColumnKind::Text).with_align(Align::Start), // name
        ColumnDef::fixed(8, ColumnKind::Text).with_align(Align::End), // value
    ])
}

/// Draw one frame: a bordered table with header, virtualized body, footer, and a
/// scrollbar whose style reflects whether the position is exact or estimated.
fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 6 || area.width < 20 {
        return;
    }
    let help_y = 0;
    let status_y = area.height - 1;

    let box_rect = Rect::new(0, 1, area.width, status_y - 1);
    let inner = Rect::new(box_rect.x + 1, box_rect.y + 1, box_rect.width - 2, box_rect.height - 2);

    // Reserve the rightmost inner column for the scrollbar; the table uses the rest.
    let bar_x = inner.x + inner.width - 1;
    let table = Rect::new(inner.x, inner.y, inner.width - 1, inner.height);

    // Header on the first row, footer on the last, body in between.
    let header_y = table.y;
    let footer_y = table.y + table.height - 1;
    let body = Rect::new(table.x, table.y + 1, table.width, table.height.saturating_sub(2));
    let body_h = body.height as usize;

    // Match the virtual viewport to the body height (handles resize), then read
    // the window to draw.
    st.list.set_viewport(body_h);
    let grid = columns();

    // Box.
    let bstyle = BorderStyle {
        weight: LineWeight::Light,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::DarkGray),
    };
    mullion::border::draw_box(buf, box_rect, Borders::ALL, &bstyle);

    // Header.
    let head_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let hrects = grid.row_rects(table, header_y);
    ColumnGrid::write_text(buf, hrects[0], header_y, "id", Align::End, head_style);
    ColumnGrid::write_text(buf, hrects[1], header_y, "name", Align::Start, head_style);
    ColumnGrid::write_text(buf, hrects[2], header_y, "value", Align::End, head_style);

    // Body: one materialized row per line, drawn through the shared grid.
    let row_style = Style::default().fg(Color::White);
    let id_style = Style::default().fg(Color::DarkGray);
    let visible = st.list.visible();
    let first_key = visible.first().map(|(k, _)| *k);
    let last_key = visible.last().map(|(k, _)| *k);
    for (i, (id, (name, value))) in visible.iter().enumerate() {
        let y = body.y + i as u16;
        let r = grid.row_rects(table, y);
        ColumnGrid::write_text(buf, r[0], y, &id.to_string(), Align::End, id_style);
        ColumnGrid::write_text(buf, r[1], y, name, Align::Start, row_style);
        ColumnGrid::write_text(buf, r[2], y, &value.to_string(), Align::End, row_style);
    }

    // Footer: which rows are shown and the total (or "?" when length is hidden).
    let metrics = st.list.scroll_metrics();
    let foot_style = Style::default().fg(Color::DarkGray);
    let total_str = if metrics.exact { TOTAL.to_string() } else { "?".to_string() };
    let footer = match (first_key, last_key) {
        (Some(a), Some(b)) => format!("rows {a}–{b} of {total_str}"),
        _ => "no rows".to_string(),
    };
    ColumnGrid::write_text(buf, Rect::new(table.x, footer_y, table.width, 1), footer_y, &footer, Align::Start, foot_style);

    // Scrollbar spanning the body, styled by exactness.
    let bar_rect = Rect::new(bar_x, body.y, 1, body.height);
    let bar_style = Style::default().fg(if metrics.exact { Color::Green } else { Color::Yellow });
    render_scrollbar(buf, bar_rect, metrics, bar_style);

    // ── Help & status ──────────────────────────────────────────────────────
    let help = "records — ↑↓/kj:scroll  PgUp/PgDn:page  g/G:top/bottom  e:exact/estimate  q:quit";
    buf.set_string(0, help_y, help, Style::default().fg(Color::White).add_modifier(Modifier::BOLD));

    let mode = if metrics.exact { "EXACT" } else { "ESTIMATE" };
    let status = format!(
        " {}  position:{:>5.1}%  window:{}/{} rows  viewport:{}",
        mode,
        metrics.position * 100.0,
        st.list.visible().len(),
        st.list.capacity(),
        st.list.viewport(),
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
            Some(Event::Key(KeyEvent { code, .. })) => {
                let page = st.list.viewport() as isize;
                match code {
                    KeyCode::Char('q') => break,
                    KeyCode::Up | KeyCode::Char('k') => st.list.scroll_by(-1),
                    KeyCode::Down | KeyCode::Char('j') => st.list.scroll_by(1),
                    KeyCode::PageUp => st.list.scroll_by(-page),
                    KeyCode::PageDown => st.list.scroll_by(page),
                    KeyCode::Char('g') => st.list.scroll_by(-(TOTAL as isize)),
                    KeyCode::Char('G') => st.list.scroll_by(TOTAL as isize),
                    KeyCode::Char('e') => st.toggle_estimated(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(())
}
