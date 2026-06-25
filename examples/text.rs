// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 2 demo — the bidi-aware text engine core (design note §3).
//!
//! A paragraph (mixing LTR English, a hard newline, and an RTL Arabic run) is
//! wrapped to a width and shown in a box. The demo exercises the three things
//! Phase 2 ships:
//!
//! - **Wrapping + BiDi reorder** — narrow/widen the wrap width with `[`/`]` and
//!   watch lines reflow; the Arabic run always reads right-to-left while the
//!   surrounding English stays left-to-right.
//! - **The logical↔visual cursor map (§3.2)** — move the cursor with the arrow
//!   keys. It steps **visually** (left always moves one cell left on screen), but
//!   the status line reports the **logical** index it maps to. On the Arabic line
//!   the logical index runs opposite to the visual one — the bijection made
//!   visible.
//! - **Pagination vs. scrolling (§3.4)** — press `p` to flip between continuous
//!   scrolling and fixed-height pages; both are views over the same wrapped model.
//!
//! Keys
//!   ← / →            move the cursor one cell left / right (visual)
//!   ↑ / ↓ or k / j   move the cursor up / down a line (auto-scrolls)
//!   [ / ]            narrow / widen the wrap width
//!   p                toggle pagination ↔ continuous scrolling
//!   q                quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{BorderStyle, Borders, CornerStyle},
    poll_event,
    style::{Color, Modifier, Style},
    text::{wrap, BaseDirection, WrappedText},
    visible_window, Buffer, LineWeight, Rect, Terminal,
};

// ── Sample content ────────────────────────────────────────────────────────────

/// The paragraph laid out by the demo. The middle line is a hard newline; the
/// last line embeds a right-to-left Arabic run inside left-to-right text.
const SAMPLE: &str = "mullion's text engine wraps a paragraph to a width, reorders \
each visual line for bidi, and exposes a logical-to-visual cursor map.\n\
A hard newline starts this second paragraph.\n\
Mixed direction: العربية reads right-to-left inside this line.";

// ── Demo state ──────────────────────────────────────────────────────────────────

/// Mutable demo state: the chosen wrap width, the cursor position, the scroll
/// offset, and whether pagination is on.
struct State {
    /// Columns the paragraph is wrapped to (clamped to the box's inner width).
    wrap_width: u16,
    /// Cursor line: index into the wrapped lines.
    cur_line: usize,
    /// Cursor column in **visual** order within `cur_line`.
    cur_vis: usize,
    /// Index of the first visible line (continuous-scroll mode).
    scroll: usize,
    /// When true, the viewport snaps to fixed-height pages instead of scrolling.
    paginate: bool,
}

impl State {
    fn new() -> Self {
        Self { wrap_width: 32, cur_line: 0, cur_vis: 0, scroll: 0, paginate: false }
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────────

/// Draw one frame: wrap the sample, render the visible window in a box, overlay
/// the cursor, and write the status line.
///
/// Returns nothing; all geometry is derived from the buffer each frame so the
/// demo reflows on resize.
fn render(buf: &mut Buffer, st: &mut State) {
    let area = buf.area;
    if area.height < 5 || area.width < 10 {
        return;
    }
    let help_y = 0;
    let status_y = area.height - 1;

    // The bordered viewport sits between the help and status rows.
    let box_rect = Rect::new(0, 1, area.width, status_y - 1);
    let inner = Rect::new(box_rect.x + 1, box_rect.y + 1, box_rect.width - 2, box_rect.height - 2);

    // Clamp the wrap width to the inner width, then wrap. Re-wrapping every frame
    // is fine at this scale and keeps the model in sync with the current width.
    st.wrap_width = st.wrap_width.clamp(4, inner.width.max(4));
    let wrapped = wrap(SAMPLE, st.wrap_width, BaseDirection::Ltr);

    // Clamp the cursor into the (possibly just re-wrapped) model.
    let line_count = wrapped.line_count().max(1);
    st.cur_line = st.cur_line.min(line_count - 1);
    let cur_len = wrapped.lines().get(st.cur_line).map(|l| l.cells.len()).unwrap_or(0);
    st.cur_vis = st.cur_vis.min(cur_len.saturating_sub(1));

    let vh = inner.height as usize; // viewport height in lines
    // Keep the cursor in view: pagination snaps to page boundaries; continuous
    // scrolling nudges the window just far enough (`visible_window` does the
    // keep-cursor-in-view arithmetic for the scrolling case).
    if st.paginate {
        st.scroll = (st.cur_line / vh.max(1)) * vh.max(1);
    } else {
        visible_window(st.cur_line, &mut st.scroll, line_count, vh);
    }

    // Box + title.
    let bstyle = BorderStyle {
        weight: LineWeight::Light,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::DarkGray),
    };
    mullion::border::draw_box(buf, box_rect, Borders::ALL, &bstyle);

    // Render the visible window line by line so the cursor cell can be inverted.
    let visible = wrapped.visible(st.scroll, vh);
    let text_style = Style::default().fg(Color::White);
    let cursor_style = Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD);
    for (row, line) in visible.iter().enumerate() {
        let y = inner.y + row as u16;
        let mut cx = inner.x;
        let limit = inner.x + inner.width;
        let is_cur_line = st.scroll + row == st.cur_line;
        for (v, cell) in line.cells.iter().enumerate() {
            let w = cell.width as u16;
            if cx + w > limit {
                break;
            }
            let style = if is_cur_line && v == st.cur_vis { cursor_style } else { text_style };
            buf.set_grapheme(cx, y, &cell.symbol, style);
            cx += w;
        }
        // Show the cursor at end-of-line (empty line or past last cell).
        if is_cur_line && cur_len == 0 {
            buf.set_grapheme(inner.x, y, " ", cursor_style);
        }
    }

    // ── Help row ──────────────────────────────────────────────────────────
    let help = "text engine — ←→:cursor  ↑↓/kj:line  [ ]:width  p:page/scroll  q:quit";
    buf.set_string(0, help_y, help, Style::default().fg(Color::White).add_modifier(Modifier::BOLD));

    // ── Status row ────────────────────────────────────────────────────────
    let status = status_text(&wrapped, st, vh);
    let sstyle = Style::default().fg(Color::Black).bg(Color::Gray);
    for x in 0..area.width {
        buf.set_string(x, status_y, " ", sstyle);
    }
    buf.set_string(0, status_y, &status, sstyle);
}

/// Build the status line: mode, width, position, and — the point of the demo —
/// the cursor's visual index alongside the **logical** index it maps to.
fn status_text(wrapped: &WrappedText, st: &State, vh: usize) -> String {
    let line = wrapped.lines().get(st.cur_line);
    let (logical, glyph) = match line {
        Some(l) if !l.cells.is_empty() => {
            // The cursor steps visually; the map tells us what it edits logically.
            let log = l.map.visual_to_logical(st.cur_vis).unwrap_or(0);
            let g = l.cells[st.cur_vis].symbol.clone();
            (log as isize, g)
        }
        _ => (-1, String::new()),
    };
    let pos = if st.paginate {
        let pages = wrapped.page_count(vh).max(1);
        format!("page {}/{}", st.scroll / vh.max(1) + 1, pages)
    } else {
        format!("line {}/{}", st.cur_line + 1, wrapped.line_count())
    };
    let log_str = if logical < 0 { "—".to_string() } else { logical.to_string() };
    format!(
        " {}  width:{}  {}  cursor vis:{} → logical:{}  glyph:'{}'",
        if st.paginate { "PAGE" } else { "SCROLL" },
        st.wrap_width,
        pos,
        st.cur_vis,
        log_str,
        glyph,
    )
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

/// Event loop: draw, then handle one key. Cursor and width edits mutate `State`;
/// the next `draw` re-wraps and re-renders so every change is immediate.
fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut st = State::new();
    loop {
        term.draw(|buf| render(buf, &mut st))?;

        match poll_event(Duration::from_millis(50))? {
            None | Some(Event::Resize(_, _)) => {}
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Left => st.cur_vis = st.cur_vis.saturating_sub(1),
                KeyCode::Right => st.cur_vis += 1, // clamped to line length next frame
                KeyCode::Up | KeyCode::Char('k') => {
                    st.cur_line = st.cur_line.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => st.cur_line += 1, // clamped next frame
                KeyCode::Char('[') => st.wrap_width = st.wrap_width.saturating_sub(1),
                KeyCode::Char(']') => st.wrap_width += 1,
                KeyCode::Char('p') => st.paginate = !st.paginate,
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}
