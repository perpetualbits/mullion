// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
use tile_engine::geometry::Rect;
use tile_engine::layout::{solve, Constraint, Node, Orientation, Size};

/// Classify a 2-tile split's axis from the produced rects.
fn axis_of(rects: &[(u64, Rect)]) -> &'static str {
    let a = rects.iter().find(|(id, _)| *id == 0).unwrap().1;
    let b = rects.iter().find(|(id, _)| *id == 1).unwrap().1;
    if a.x != b.x { "horizontal" } else { "vertical" }
}

// Fix 1: a binding `max` on a Fill must not create a gap — the surplus goes to
// the unsaturated Fill.
#[test]
fn fill_with_max_still_tiles_exactly() {
    let mut node = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(0)),
            (Constraint::new(Size::Fill(1)).with_max(4), Node::Tile(1)),
        ],
    };
    let rects = solve(&mut node, Rect::new(0, 0, 10, 1));
    let covered: u16 = rects.iter().map(|(_, r)| r.width).sum();
    assert_eq!(covered, 10, "children left a gap");
    let second = rects.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(second.width <= 4, "max must still be honored");
}

// Fix 1: a `min` on a Fill larger than its share is honored when feasible.
#[test]
fn fill_with_min_is_honored_when_feasible() {
    let mut node = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(0)),
            (Constraint::new(Size::Fill(1)).with_min(8), Node::Tile(1)),
        ],
    };
    let rects = solve(&mut node, Rect::new(0, 0, 10, 1));
    let covered: u16 = rects.iter().map(|(_, r)| r.width).sum();
    assert_eq!(covered, 10, "must still tile exactly");
    let second = rects.iter().find(|(id, _)| *id == 1).unwrap().1;
    assert!(second.width >= 8, "min must be honored when it fits");
}

// Hysteresis: the SAME near-square area resolves differently by history.
#[test]
fn adaptive_hysteresis_is_sticky() {
    let mut a = Node::Split {
        orientation: Orientation::Adaptive { margin_pct: 10, last: None },
        children: vec![(Constraint::default(), Node::Tile(0)), (Constraint::default(), Node::Tile(1))],
    };
    solve(&mut a, Rect::new(0, 0, 50, 100));        // tall → latches Vertical
    let a_rects = solve(&mut a, Rect::new(0, 0, 50, 52)); // near-square stays Vertical
    assert_eq!(axis_of(&a_rects), "vertical");

    let mut b = Node::Split {
        orientation: Orientation::Adaptive { margin_pct: 10, last: None },
        children: vec![(Constraint::default(), Node::Tile(0)), (Constraint::default(), Node::Tile(1))],
    };
    let b_rects = solve(&mut b, Rect::new(0, 0, 50, 52)); // fresh → default Horizontal
    assert_eq!(axis_of(&b_rects), "horizontal");
}
