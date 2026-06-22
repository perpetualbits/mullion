// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Phase 4 demo — wrapped-line virtualization (design note §4.2).
//!
//! Scrolls and seeks through a large flowed document without ever wrapping the
//! whole thing. A lazy byte-offset → wrapped-line index is built incrementally as
//! you move; only the visible window is wrapped for display (via the Phase 2 text
//! engine).
//!
//! The scrollbar reuses the Phase 3 exact/estimate distinction: the thumb is a
//! `▒` **estimate** (byte position) while the document is only partly indexed, and
//! turns into a solid `█` **exact** thumb once it has been fully indexed — press
//! `G` (go to bottom) to force a full index and watch it flip.
//!
//! Keys
//!   ↑ / ↓ or k / j   scroll one line
//!   PgUp / PgDn      scroll one screen
//!   g / G            jump to top / bottom (G forces a full index)
//!   [ / ]            narrow / widen the wrap width (re-wraps, keeps position)
//!   q                quit

use std::{io, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    border::{BorderStyle, Borders, CornerStyle},
    poll_event, render_doc, render_scrollbar,
    style::{Color, Modifier, Style},
    vlist::ScrollMetrics,
    Buffer, DocView, LineWeight, Rect, Terminal,
};

// ── Sample document ───────────────────────────────────────────────────────────

/// Build a multi-paragraph document: ~400 paragraphs of varying length with the
/// occasional blank line, long enough that the index is built lazily as you
/// scroll rather than all at once.
fn sample_document() -> String {
    const WORDS: [&str; 12] = [
        "mullion", "wraps", "flowed", "text", "lazily", "across", "a", "virtual",
        "viewport", "without", "materializing", "everything",
    ];
    let mut doc = String::new();
    for p in 0..400u32 {
        // Paragraph length varies 6..30 words, deterministically from the index.
        let n = 6 + (p * 7) % 24;
        for w in 0..n {
            if w > 0 {
                doc.push(' ');
            }
            doc.push_str(WORDS[((p + w) as usize) % WORDS.len()]);
        }
        doc.push_str(&format!(" [paragraph {p}]"));
        doc.push('\n');
        if p % 9 == 8 {
            doc.push('\n'); // a blank line every so often
        }
    }
    doc
}

// ── Rendering ───────────────────────────────────────────────────────────────────

/// Draw one frame: the document body in a box, a scrollbar, and a status line.
fn render(buf: &mut Buffer, view: &mut DocView, doc_len: usize) {
    let area = buf.area;
    if area.height < 5 || area.width < 12 {
        return;
    }
    let help_y = 0;
    let status_y = area.height - 1;

    let box_rect = Rect::new(0, 1, area.width, status_y - 1);
    let inner = Rect::new(box_rect.x + 1, box_rect.y + 1, box_rect.width - 2, box_rect.height - 2);
    // Reserve the rightmost inner column for the scrollbar.
    let bar_x = inner.x + inner.width - 1;
    let body = Rect::new(inner.x, inner.y, inner.width - 1, inner.height);

    // Box.
    let bstyle = BorderStyle {
        weight: LineWeight::Light,
        corners: CornerStyle::Rounded,
        style: Style::default().fg(Color::DarkGray),
    };
    mullion::border::draw_box(buf, box_rect, Borders::ALL, &bstyle);

    // Document body — only the visible window is wrapped.
    render_doc(buf, body, view, Style::default().fg(Color::White));

    // Scrollbar: position is the byte fraction of the top line (always known);
    // it reads as an estimate until the whole document has been indexed.
    let top_byte = view.line_to_byte(view.top()).unwrap_or(0);
    let (indexed, complete) = view.line_count_hint();
    let position = if doc_len == 0 { 0.0 } else { top_byte as f32 / doc_len as f32 };
    let extent = if complete && indexed > 0 {
        body.height as f32 / indexed as f32
    } else {
        0.0
    };
    let metrics = ScrollMetrics { position, extent, exact: complete };
    let bar_style = Style::default().fg(if complete { Color::Green } else { Color::Yellow });
    render_scrollbar(buf, Rect::new(bar_x, body.y, 1, body.height), metrics, bar_style);

    // Help & status.
    let help = "document — ↑↓/kj:scroll  PgUp/PgDn:page  g/G:top/bottom  [ ]:width  q:quit";
    buf.set_string(0, help_y, help, Style::default().fg(Color::White).add_modifier(Modifier::BOLD));

    let count = if complete { format!("{indexed}") } else { format!("{indexed}+") };
    let status = format!(
        " line {}  of {}  byte {}/{}  width:{}  index:{}",
        view.top() + 1,
        count,
        top_byte,
        doc_len,
        view.width(),
        if complete { "complete" } else { "building…" },
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
    let doc = sample_document();
    let doc_len = doc.len();
    let mut view = DocView::new(doc, 64);
    // Body height of the last frame, used to size a page-scroll; captured in the
    // draw closure so it tracks resizes without a separate size query.
    let mut page: isize = 1;

    loop {
        term.draw(|buf| {
            page = (buf.area.height as isize - 3).max(1); // minus help + 2 border rows
            render(buf, &mut view, doc_len);
        })?;

        match poll_event(Duration::from_millis(50))? {
            None | Some(Event::Resize(_, _)) => {}
            Some(Event::Key(KeyEvent { code, .. })) => match code {
                KeyCode::Char('q') => break,
                KeyCode::Up | KeyCode::Char('k') => view.scroll_by(-1),
                KeyCode::Down | KeyCode::Char('j') => view.scroll_by(1),
                KeyCode::PageUp => view.scroll_by(-page),
                KeyCode::PageDown => view.scroll_by(page),
                KeyCode::Char('g') => view.scroll_to_line(0),
                KeyCode::Char('G') => view.seek_to_byte(doc_len), // forces a full index
                KeyCode::Char('[') => view.set_width(view.width().saturating_sub(2)),
                KeyCode::Char(']') => view.set_width(view.width() + 2),
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}
