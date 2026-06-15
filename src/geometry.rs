/// A rectangle in terminal cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    pub fn area(self) -> u32 {
        u32::from(self.width) * u32::from(self.height)
    }

    /// One past the last column.
    pub fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// One past the last row.
    pub fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Returns the largest `Rect` that fits within both `self` and `other`.
    pub fn intersection(self, other: Rect) -> Rect {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = self.right().min(other.right());
        let y2 = self.bottom().min(other.bottom());
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
