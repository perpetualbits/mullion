// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use tile_engine::{
    assert_backend_snapshot,
    backend::TestBackend,
    style::{Color, Style},
    Terminal,
};

// ── Snapshot helper ──────────────────────────────────────────────────────────

fn make_term(w: u16, h: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(w, h)).unwrap()
}

// ── Snapshot test ────────────────────────────────────────────────────────────

#[test]
fn snapshot_basic_frame() {
    let mut term = make_term(10, 3);
    term.draw(|buf| {
        buf.set_string(0, 0, "Hello", Style::default());
        buf.set_string(0, 1, "World", Style::default());
    })
    .unwrap();

    assert_backend_snapshot!(
        term,
        "Hello     \nWorld     \n          "
    );
}

#[test]
fn snapshot_wide_glyphs() {
    // "世界" needs 4 columns
    let mut term = make_term(6, 1);
    term.draw(|buf| {
        buf.set_string(0, 0, "世界", Style::default());
    })
    .unwrap();

    assert_backend_snapshot!(term, "世界  ");
}

// ── Style minimization test ───────────────────────────────────────────────────

// Drive CrosstermBackend against a Vec<u8> and assert SGR sequences are only
// emitted when the style changes.
#[test]
fn crossterm_backend_style_minimization() {
    use tile_engine::backend::{Backend, CrosstermBackend};
    use tile_engine::Cell;

    let mut buf: Vec<u8> = Vec::new();
    let mut backend = CrosstermBackend::new(&mut buf);

    let style_a = Style::default().fg(Color::Red);
    let style_b = Style::default().fg(Color::Green);

    let changes: Vec<(u16, u16, Cell)> = vec![
        (0, 0, Cell::new("A", style_a)),
        (1, 0, Cell::new("B", style_a)), // same style → no new SGR
        (2, 0, Cell::new("C", style_b)), // style change → new SGR
        (3, 0, Cell::new("D", style_b)), // same style again → no new SGR
    ];

    backend.begin_frame().unwrap();
    backend
        .draw(changes.iter().map(|(x, y, c)| (*x, *y, c)))
        .unwrap();
    backend.end_frame().unwrap();
    drop(backend); // release &mut buf before reading

    let output = String::from_utf8_lossy(&buf);

    // Each call to emit_style emits one SetColors sequence beginning with ESC[38
    // (foreground color) or ESC[39;49 (reset colors). We wrote 4 cells with
    // 2 distinct styles, so exactly 2 style emissions should appear — one when
    // style_a is first seen and one when style_b appears.  Cells B and D share
    // their preceding style and must not trigger a new emission.
    let color_set_count = output.matches("\x1b[38").count()
        + output.matches("\x1b[39;49m").count();

    assert_eq!(
        color_set_count, 2,
        "expected exactly 2 color-set sequences for 2 style groups, got {color_set_count}\noutput: {output:?}"
    );
}

// ── Synchronized-output markers ───────────────────────────────────────────────

#[test]
fn crossterm_backend_sync_markers_wrap_frame() {
    use tile_engine::backend::{Backend, CrosstermBackend};
    use tile_engine::Cell;

    let mut buf: Vec<u8> = Vec::new();
    let mut backend = CrosstermBackend::new(&mut buf);

    backend.begin_frame().unwrap();
    backend.draw(std::iter::empty::<(u16, u16, &Cell)>()).unwrap();
    backend.end_frame().unwrap();
    drop(backend); // release &mut buf before reading

    let output = String::from_utf8_lossy(&buf);
    assert!(output.contains("\x1b[?2026h"), "missing begin-sync marker");
    assert!(output.contains("\x1b[?2026l"), "missing end-sync marker");

    // begin must appear before end
    let begin_pos = output.find("\x1b[?2026h").unwrap();
    let end_pos = output.find("\x1b[?2026l").unwrap();
    assert!(begin_pos < end_pos, "begin-sync must precede end-sync");
}

// ── Resize triggers a physical clear ─────────────────────────────────────────

#[test]
fn clear_called_on_resize_not_on_steady_frame() {
    let mut term = make_term(10, 3);

    // First draw: no resize, no clear.
    term.draw(|buf| { buf.set_string(0, 0, "hello", Style::default()); }).unwrap();
    assert_eq!(term.backend().clears, 0, "steady frame must not clear");

    // Resize then draw: clear expected.
    term.backend_mut().resize(8, 3);
    term.draw(|buf| { buf.set_string(0, 0, "hi", Style::default()); }).unwrap();
    assert_eq!(term.backend().clears, 1, "resize frame must clear once");

    // Another steady draw: no additional clear.
    term.draw(|buf| { buf.set_string(0, 0, "bye", Style::default()); }).unwrap();
    assert_eq!(term.backend().clears, 1, "second steady frame must not clear again");
}

// ── Resize robustness ─────────────────────────────────────────────────────────

#[test]
fn resize_smaller_then_larger_no_panic() {
    let mut term = make_term(20, 10);

    term.draw(|buf| {
        buf.set_string(0, 0, "Initial content", Style::default());
    })
    .unwrap();

    // Resize to smaller.
    term.backend_mut().resize(5, 3);
    term.draw(|buf| {
        buf.set_string(0, 0, "Hi", Style::default());
    })
    .unwrap();
    assert_backend_snapshot!(term, "Hi   \n     \n     ");

    // Resize back to larger.
    term.backend_mut().resize(20, 5);
    term.draw(|buf| {
        buf.set_string(0, 0, "Back to big", Style::default());
    })
    .unwrap();

    let s = term.backend().render();
    assert!(s.starts_with("Back to big"), "got: {s:?}");
}
