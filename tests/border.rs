// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use mullion::{
    assert_backend_snapshot,
    backend::TestBackend,
    border::{draw_box, frame_tiles, render_shared, BorderStyle, Borders, CornerStyle, LineWeight},
    buffer::Buffer,
    geometry::Rect,
    layout::{Constraint, Node, Orientation, Size},
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

// ── render_shared snapshot tests ──────────────────────────────────────────────

fn two_h_tiles() -> Node {
    Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(0)),
            (Constraint::new(Size::Fill(1)), Node::Tile(1)),
        ],
    }
}

/// Two tiles side by side in a 7×3 area.
#[test]
fn render_shared_two_tiles_h() {
    let mut term = Terminal::new(TestBackend::new(7, 3)).unwrap();
    let mut node = two_h_tiles();
    term.draw(|buf| {
        render_shared(buf, &mut node, Rect::new(0, 0, 7, 3), LineWeight::Light, &Style::default(), &[]);
    }).unwrap();
    assert_backend_snapshot!(term, "┌──┬──┐\n│  │  │\n└──┴──┘");
}

/// 2×2 grid (V split of two H splits) with a central ┼ junction.
#[test]
fn render_shared_2x2_grid() {
    let mut term = Terminal::new(TestBackend::new(7, 5)).unwrap();
    let mut node = Node::Split {
        orientation: Orientation::Vertical,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(0)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                ],
            }),
            (Constraint::new(Size::Fill(1)), Node::Split {
                orientation: Orientation::Horizontal,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(2)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(3)),
                ],
            }),
        ],
    };
    term.draw(|buf| {
        render_shared(buf, &mut node, Rect::new(0, 0, 7, 5), LineWeight::Light, &Style::default(), &[]);
    }).unwrap();
    assert_backend_snapshot!(term,
        "┌──┬──┐\n│  │  │\n├──┼──┤\n│  │  │\n└──┴──┘"
    );
}

/// Two tiles stacked vertically — shared horizontal divider with ├/┤ ends.
#[test]
fn render_shared_two_tiles_v() {
    let mut term = Terminal::new(TestBackend::new(7, 5)).unwrap();
    let mut node = Node::Split {
        orientation: Orientation::Vertical,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(0)),
            (Constraint::new(Size::Fill(1)), Node::Tile(1)),
        ],
    };
    term.draw(|buf| {
        render_shared(buf, &mut node, Rect::new(0, 0, 7, 5), LineWeight::Light, &Style::default(), &[]);
    }).unwrap();
    assert_backend_snapshot!(term,
        "┌─────┐\n│     │\n├─────┤\n│     │\n└─────┘"
    );
}

/// Nested split: left half is a V split of 2 tiles, right half is a single tile.
/// The inner H divider must terminate at the vertical divider with ┤ (not ┼),
/// verifying no off-by-one in the h-line extent.
#[test]
fn render_shared_nested_split() {
    let mut term = Terminal::new(TestBackend::new(7, 5)).unwrap();
    let mut node = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Split {
                orientation: Orientation::Vertical,
                children: vec![
                    (Constraint::new(Size::Fill(1)), Node::Tile(0)),
                    (Constraint::new(Size::Fill(1)), Node::Tile(1)),
                ],
            }),
            (Constraint::new(Size::Fill(1)), Node::Tile(2)),
        ],
    };
    term.draw(|buf| {
        render_shared(buf, &mut node, Rect::new(0, 0, 7, 5), LineWeight::Light, &Style::default(), &[]);
    }).unwrap();
    // The divider at y=2 goes only from x=0..3 (left child's column span),
    // so (3,2) is ┤ not ┼, and (4,2)..(5,2) are spaces.
    assert_backend_snapshot!(term,
        "┌──┬──┐\n│  │  │\n├──┤  │\n│  │  │\n└──┴──┘"
    );
}

// ── render_shared content-rect test ──────────────────────────────────────────

/// Content rects for the 7×3 two-tile case are (1,1,2,1) and (4,1,2,1).
#[test]
fn render_shared_content_rects() {
    let mut node = two_h_tiles();
    let mut buf = Buffer::empty(Rect::new(0, 0, 7, 3));
    let rects = render_shared(
        &mut buf, &mut node, Rect::new(0, 0, 7, 3),
        LineWeight::Light, &Style::default(), &[],
    );
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0], (0, Rect::new(1, 1, 2, 1)));
    assert_eq!(rects[1], (1, Rect::new(4, 1, 2, 1)));
}

// ── render_shared override / focus-weight test ────────────────────────────────

/// Override the left tile's weight to Heavy.  The expected render is:
/// ┏━━┱──┐  (heavy top edge for left tile; mixed junction at x=3)
/// ┃  ┃  │
/// ┗━━┹──┘  (heavy bottom; mixed junction at x=3)
#[test]
fn render_shared_override_heavy_left() {
    let mut term = Terminal::new(TestBackend::new(7, 3)).unwrap();
    let mut node = two_h_tiles();
    term.draw(|buf| {
        render_shared(
            buf, &mut node, Rect::new(0, 0, 7, 3),
            LineWeight::Light, &Style::default(),
            &[(0, LineWeight::Heavy)],
        );
    }).unwrap();
    // The heavy override thickens the left tile's four edges; the shared divider
    // at x=3 becomes a mixed-weight junction (┱ top, ┹ bottom, ┃ middle).
    assert_backend_snapshot!(term, "┏━━┱──┐\n┃  ┃  │\n┗━━┹──┘");
}

// ── render_shared degenerate tests ───────────────────────────────────────────

/// 1×1 area: no border arms can be set; no panic; content rect is zero-area.
#[test]
fn render_shared_degenerate_1x1() {
    let mut node = Node::Tile(42);
    let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
    let rects = render_shared(
        &mut buf, &mut node, Rect::new(0, 0, 1, 1),
        LineWeight::Light, &Style::default(), &[],
    );
    assert_eq!(rects.len(), 1);
    assert!(rects[0].1.is_empty(), "1×1 must yield zero-area content rect");
}

/// 2×2 area: only corners can be drawn; inner area is zero; no panic.
#[test]
fn render_shared_degenerate_2x2() {
    let mut node = Node::Tile(42);
    let mut buf = Buffer::empty(Rect::new(0, 0, 2, 2));
    let rects = render_shared(
        &mut buf, &mut node, Rect::new(0, 0, 2, 2),
        LineWeight::Light, &Style::default(), &[],
    );
    assert_eq!(rects.len(), 1);
    assert!(rects[0].1.is_empty(), "2×2 must yield zero-area content rect");
}

/// 5 children in a 6-wide area: available width saturates to 0; no panic;
/// all 5 content rects are returned (all zero-width).
#[test]
fn render_shared_degenerate_5_children_6_wide() {
    let mut node = Node::Split {
        orientation: Orientation::Horizontal,
        children: (0..5u64).map(|i| (Constraint::new(Size::Fill(1)), Node::Tile(i))).collect(),
    };
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
    let rects = render_shared(
        &mut buf, &mut node, Rect::new(0, 0, 6, 3),
        LineWeight::Light, &Style::default(), &[],
    );
    assert_eq!(rects.len(), 5, "all 5 tiles must be returned");
    // No panic is the primary assertion; all content rects may be zero-area.
}
