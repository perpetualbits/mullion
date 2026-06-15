use tile_engine::geometry::Rect;
use tile_engine::layout::{solve, Constraint, Node, Orientation, Size};

#[test]
fn zero_weight_fill_should_not_absorb_remainder() {
    let mut n = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(0)),
            (Constraint::new(Size::Fill(1)), Node::Tile(1)),
            (Constraint::new(Size::Fill(0)), Node::Tile(2)),
        ],
    };
    let mut r = solve(&mut n, Rect::new(0, 0, 11, 1));
    r.sort_by_key(|(id, _)| *id);
    let w: Vec<u16> = r.iter().map(|(_, x)| x.width).collect();
    assert_eq!(w.iter().sum::<u16>(), 11, "must tile exactly");
    assert_eq!(w[2], 0, "Fill(0) must stay collapsed; remainder belongs to weighted fills");
}
