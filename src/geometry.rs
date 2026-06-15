/// A rectangle in terminal cell coordinates.
///
/// The coordinate system places the origin `(0, 0)` at the top-left corner of
/// the terminal.  Both axes increase downward and rightward.  All values are in
/// **terminal cell units** (columns for x/width, rows for y/height).
///
/// `Rect` is used as both a region descriptor (the area a buffer covers) and a
/// clipping/intersection primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// Column of the left edge (inclusive).
    pub x: u16,
    /// Row of the top edge (inclusive).
    pub y: u16,
    /// Number of columns.
    pub width: u16,
    /// Number of rows.
    pub height: u16,
}

impl Rect {
    /// Construct a `Rect` from its top-left corner and dimensions.
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    /// Return the total number of cells in the rectangle.
    ///
    /// Widened to `u32` to avoid overflow for large terminals (a 65535×65535
    /// rect would overflow `u16`).
    pub fn area(self) -> u32 {
        u32::from(self.width) * u32::from(self.height)
    }

    /// Return the column one past the right edge (exclusive right bound).
    ///
    /// `saturating_add` is used so that a rect at the maximum `u16` position
    /// never wraps around to 0.
    pub fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Return the row one past the bottom edge (exclusive bottom bound).
    ///
    /// `saturating_add` is used for the same overflow-safety reason as `right`.
    pub fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Return `true` if the rectangle has no cells.
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Return `true` if the cell at `(x, y)` lies within this rectangle.
    ///
    /// The bounds are `[self.x, self.right())` and `[self.y, self.bottom())`,
    /// i.e. the right and bottom edges are exclusive.
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Return the largest `Rect` that fits within both `self` and `other`.
    ///
    /// Computes the overlap by taking the maximum of the two left/top edges and
    /// the minimum of the two right/bottom edges.  Returns `Rect::default()`
    /// (zero-sized, at the origin) when the two rectangles are adjacent or
    /// non-overlapping — callers should check `is_empty()` before using the
    /// result.
    pub fn intersection(self, other: Rect) -> Rect {
        // The inner corners of the potential overlap region.
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = self.right().min(other.right());
        let y2 = self.bottom().min(other.bottom());
        // If the right edge did not extend past the left edge (or bottom past
        // top), the rectangles do not overlap.
        if x2 <= x1 || y2 <= y1 {
            Rect::default()
        } else {
            Rect { x: x1, y: y1, width: x2 - x1, height: y2 - y1 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area() {
        assert_eq!(Rect::new(0, 0, 10, 5).area(), 50);
        assert_eq!(Rect::new(0, 0, 0, 5).area(), 0);
    }

    #[test]
    fn contains() {
        let r = Rect::new(2, 3, 4, 5);
        assert!(r.contains(2, 3));
        assert!(r.contains(5, 7));
        assert!(!r.contains(6, 7)); // x == right()
        assert!(!r.contains(5, 8)); // y == bottom()
        assert!(!r.contains(1, 3));
    }

    #[test]
    fn intersection_overlap() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersection(b), Rect::new(5, 5, 5, 5));
    }

    #[test]
    fn intersection_no_overlap() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(10, 10, 5, 5);
        assert!(a.intersection(b).is_empty());
    }

    #[test]
    fn intersection_adjacent() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(5, 0, 5, 5);
        assert!(a.intersection(b).is_empty());
    }
}
