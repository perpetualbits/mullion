// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use tile_engine::{
    assert_backend_snapshot,
    backend::TestBackend,
    border::{draw_box, frame_tiles, BorderStyle, Borders, CornerStyle, LineWeight},
    buffer::Buffer,
    geometry::Rect,
    style::Style,
    Terminal,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn light() -> BorderStyle {
    BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Square, style: Style::default() }
}

fn rounded() -> BorderStyle {
    BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Rounded, style: Style::default() }
}

fn heavy() -> BorderStyle {
    BorderStyle { weight: LineWeight::Heavy, corners: CornerStyle::Square, style: Style::default() }
}

// ── Snapshot tests via Terminal<TestBackend> ──────────────────────────────────

/// A single 4×3 light box.
#[test]
fn single_light_box() {
    let mut term = Terminal::new(TestBackend::new(4, 3)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 4, 3), Borders::ALL, &light());
    }).unwrap();
    assert_backend_snapshot!(term, "┌──┐\n│  │\n└──┘");
}

/// Two adjacent 4×3 tiles each framed separately → doubled gutter.
#[test]
fn two_adjacent_tiles_doubled_gutter() {
    let mut term = Terminal::new(TestBackend::new(8, 3)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 4, 3), Borders::ALL, &light());
        draw_box(buf, Rect::new(4, 0, 4, 3), Borders::ALL, &light());
    }).unwrap();
    assert_backend_snapshot!(term, "┌──┐┌──┐\n│  ││  │\n└──┘└──┘");
}

/// Light box with rounded corners.
#[test]
fn rounded_light_box() {
    let mut term = Terminal::new(TestBackend::new(4, 3)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 4, 3), Borders::ALL, &rounded());
    }).unwrap();
    assert_backend_snapshot!(term, "╭──╮\n│  │\n╰──╯");
}

/// Heavy box.
#[test]
fn heavy_box() {
    let mut term = Terminal::new(TestBackend::new(4, 3)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 4, 3), Borders::ALL, &heavy());
    }).unwrap();
    assert_backend_snapshot!(term, "┏━━┓\n┃  ┃\n┗━━┛");
}

/// Partial border: TOP | LEFT only — corner at top-left, no right/bottom.
#[test]
fn partial_borders_top_left() {
    let mut term = Terminal::new(TestBackend::new(4, 3)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 4, 3), Borders::TOP | Borders::LEFT, &light());
    }).unwrap();
    // TL=(┌), top middle=(──), TR position=(─, top only → h_line),
    // left middle=(│), BL position=(│, left only → v_line).
    assert_backend_snapshot!(term, "┌───\n│   \n│   ");
}

// ── Degenerate-area tests ─────────────────────────────────────────────────────

/// 1×1 area: width == 1, so only a vertical line is drawn.
#[test]
fn degenerate_1x1_no_panic() {
    let mut term = Terminal::new(TestBackend::new(1, 1)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 1, 1), Borders::ALL, &light());
    }).unwrap();
    assert_backend_snapshot!(term, "│");
}

/// 2×2 area: corners only, no middle cells.
#[test]
fn degenerate_2x2_corners_only() {
    let mut term = Terminal::new(TestBackend::new(2, 2)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 2, 2), Borders::ALL, &light());
    }).unwrap();
    assert_backend_snapshot!(term, "┌┐\n└┘");
}

/// 1×5 area: draws a full-height vertical line (no corners possible).
#[test]
fn degenerate_1x5_vertical_line() {
    let mut term = Terminal::new(TestBackend::new(1, 5)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 1, 5), Borders::ALL, &light());
    }).unwrap();
    assert_backend_snapshot!(term, "│\n│\n│\n│\n│");
}

/// 5×1 area: draws a full-width horizontal line (no corners possible).
#[test]
fn degenerate_5x1_horizontal_line() {
    let mut term = Terminal::new(TestBackend::new(5, 1)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 5, 1), Borders::ALL, &light());
    }).unwrap();
    assert_backend_snapshot!(term, "─────");
}

/// Zero-area rect draws nothing and does not panic.
#[test]
fn zero_area_no_panic() {
    let mut term = Terminal::new(TestBackend::new(4, 3)).unwrap();
    term.draw(|buf| {
        draw_box(buf, Rect::new(0, 0, 0, 3), Borders::ALL, &light());
        draw_box(buf, Rect::new(0, 0, 4, 0), Borders::ALL, &light());
    }).unwrap();
    // Buffer should remain all spaces.
    assert_backend_snapshot!(term, "    \n    \n    ");
}

// ── frame_tiles interior rect tests ──────────────────────────────────────────

/// A 10×4 tile framed with ALL borders yields content rect (1, 1, 8, 2).
#[test]
fn frame_tiles_interior_10x4() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
    let tiles = vec![(1u64, Rect::new(0, 0, 10, 4))];
    let result = frame_tiles(&mut buf, &tiles, Borders::ALL, &light());
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, 1);
    assert_eq!(result[0].1, Rect::new(1, 1, 8, 2));
}

/// A 1×1 tile framed with ALL borders yields a zero-area content rect.
#[test]
fn frame_tiles_1x1_zero_interior() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
    let tiles = vec![(0u64, Rect::new(0, 0, 1, 1))];
    let result = frame_tiles(&mut buf, &tiles, Borders::ALL, &light());
    assert_eq!(result.len(), 1);
    assert!(result[0].1.is_empty(), "1×1 tile must yield a zero-area content rect");
}

/// frame_tiles draws borders AND returns correct interior rects for two tiles.
#[test]
fn frame_tiles_two_tiles_draws_and_deflates() {
    let tiles = vec![
        (10u64, Rect::new(0, 0, 4, 3)),
        (20u64, Rect::new(4, 0, 4, 3)),
    ];
    let mut buf = Buffer::empty(Rect::new(0, 0, 8, 3));
    let interiors = frame_tiles(&mut buf, &tiles, Borders::ALL, &light());

    // Interior rects: each 4×3 tile deflated by 1 on all 4 sides → 2×1.
    assert_eq!(interiors[0], (10, Rect::new(1, 1, 2, 1)));
    assert_eq!(interiors[1], (20, Rect::new(5, 1, 2, 1)));
}
