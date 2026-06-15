// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use mullion::{geometry::Rect, style::Style, Buffer, Cell};
use unicode_width::UnicodeWidthStr;

// Fix 3: wide grapheme that doesn't fit, landing on an existing continuation cell.
#[test]
fn wide_at_edge_over_continuation_keeps_width() {
    let mut b = Buffer::empty(Rect::new(0, 0, 2, 1));
    b.set_grapheme(0, 0, "世", Style::default()); // col0=世, col1=continuation
    b.set_grapheme(1, 0, "界", Style::default()); // can't fit; must not strand col0
    let disp = b.get(0, 0).symbol.width() + b.get(1, 0).symbol.width();
    assert_eq!(disp, 2, "rendered width {disp} != buffer width 2 (stale wide left half)");
}

// Fix 4: `set` with a wide cell must establish a continuation.
#[test]
fn set_wide_cell_establishes_continuation() {
    let mut b = Buffer::empty(Rect::new(0, 0, 4, 1));
    b.set(0, 0, Cell::new("世", Style::default()));
    assert!(b.get(1, 0).is_continuation(), "set() with wide cell left no continuation at col1");
}
