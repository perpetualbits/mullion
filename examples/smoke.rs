// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Smoke test for mullion Phase 0.
//!
//! Enters the alternate screen, draws a few lines demonstrating wide graphemes,
//! combining marks, and terminal-dimension display.  Redraws automatically on
//! resize.  Quit with `q`.

use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent};

use mullion::{
    backend::CrosstermBackend,
    poll_event,
    style::{Color, Modifier, Style},
    Terminal,
};

/// Program entry point.
///
/// Constructs a `CrosstermBackend` over stdout, enters interactive mode, then
/// delegates to `run`.  `leave()` is called on the normal-exit path so the
/// shell is restored cleanly.
///
/// Panics and early returns are handled by `CrosstermBackend`'s `Drop` impl
/// and the panic hook installed by `enter()`, so the shell is also restored in
/// those cases.
// BUG?: the comment below ("Ensure we always clean up, even on panic") is
// misleading: `term.leave()` only runs on the normal-exit path.  Panic cleanup
// is handled by the Drop impl and the panic hook in CrosstermBackend::enter().
fn main() -> io::Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.enter()?;

    // Run the event loop; capture any error so we can still call leave().
    let result = run(&mut term);

    // Restore the terminal on normal exit.  Panics are covered by
    // CrosstermBackend's Drop impl and the panic hook installed by enter().
    term.leave()?;
    result
}

/// Event loop: draw frames and handle input until the user presses `q`.
///
/// On each iteration:
/// 1. Draw a frame showing status text and grapheme samples.
/// 2. Poll for an event with a 100 ms timeout (short enough to remain
///    responsive, long enough not to spin the CPU).
/// 3. On `q` / `Q`, break and return.
/// 4. On a resize event, just loop — `Terminal::draw` calls `check_resize`
///    internally and will re-layout on the next iteration.
fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    loop {
        term.draw(|buf| {
            let area = buf.area;
            let title_style = Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD);
            let normal = Style::default();

            buf.set_string(0, 0, "mullion smoke test — press q to quit", title_style);
            // Two full-width CJK characters (each 2 columns) and one emoji (2 columns).
            buf.set_string(0, 1, "Wide glyphs: 世界 🌍 (4 cols + 2 cols)", normal);
            // é decomposed as base 'e' + combining acute; must land in one cell.
            buf.set_string(0, 2, "Combining:   e\u{0301} (one cell)", normal);
            buf.set_string(
                0,
                3,
                &format!("Terminal: {}×{}", area.width, area.height),
                normal,
            );
        })?;

        match poll_event(Duration::from_millis(100))? {
            Some(Event::Key(KeyEvent { code: KeyCode::Char('q'), .. })) => break,
            Some(Event::Resize(_, _)) => {
                // check_resize() is called inside draw(); just loop to redraw.
            }
            _ => {}
        }
    }
    Ok(())
}
