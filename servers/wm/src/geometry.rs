//! Integer rectangles. Coordinates are screen pixels; sizes of zero or less mean empty.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const EMPTY: Rect = Rect::new(0, 0, 0, 0);

    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(self) -> i32 {
        self.x.saturating_add(self.w)
    }
    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.h)
    }
    pub fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn contains(self, x: i32, y: i32) -> bool {
        !self.is_empty() && x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn intersect(self, other: Rect) -> Rect {
        let (x, y) = (self.x.max(other.x), self.y.max(other.y));
        let (right, bottom) = (
            self.right().min(other.right()),
            self.bottom().min(other.bottom()),
        );
        if self.is_empty() || other.is_empty() || right <= x || bottom <= y {
            Rect::EMPTY
        } else {
            Rect::new(x, y, right - x, bottom - y)
        }
    }
    pub fn intersects(self, other: Rect) -> bool {
        !self.intersect(other).is_empty()
    }
    /// Smallest rectangle covering both; an empty operand is ignored.
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }
    pub fn inset(self, amount: i32) -> Rect {
        Rect::new(
            self.x + amount,
            self.y + amount,
            self.w - 2 * amount,
            self.h - 2 * amount,
        )
    }
    pub fn translate(self, dx: i32, dy: i32) -> Rect {
        Rect::new(
            self.x.saturating_add(dx),
            self.y.saturating_add(dy),
            self.w,
            self.h,
        )
    }
    /// Moves the rectangle (never resizing it) so it lies inside `bounds`; a rectangle
    /// larger than the bounds is aligned to the bounds' top-left.
    pub fn clamp_into(self, bounds: Rect) -> Rect {
        let x = self.x.min(bounds.right() - self.w).max(bounds.x);
        let y = self.y.min(bounds.bottom() - self.h).max(bounds.y);
        Rect::new(x, y, self.w, self.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersection_union_and_emptiness() {
        let a = Rect::new(10, 10, 20, 20);
        let b = Rect::new(20, 25, 20, 20);
        assert_eq!(a.intersect(b), Rect::new(20, 25, 10, 5));
        assert_eq!(a.intersect(b), b.intersect(a));
        assert_eq!(a.union(b), Rect::new(10, 10, 30, 35));
        // Touching edges do not intersect; empty rectangles intersect nothing.
        assert!(a.intersect(Rect::new(30, 10, 5, 5)).is_empty());
        assert!(a.intersect(Rect::new(10, 30, 5, 5)).is_empty());
        assert!(!a.intersects(Rect::new(0, 0, 10, 10)));
        assert!(a.intersect(Rect::new(15, 15, 0, 9)).is_empty());
        assert!(a.intersect(Rect::new(15, 15, -3, 9)).is_empty());
        assert_eq!(Rect::EMPTY.union(a), a);
        assert_eq!(a.union(Rect::EMPTY), a);
        assert!(Rect::EMPTY.union(Rect::EMPTY).is_empty());
    }

    #[test]
    fn containment_is_half_open_and_extreme_values_do_not_overflow() {
        let r = Rect::new(5, 5, 10, 10);
        assert!(r.contains(5, 5) && r.contains(14, 14));
        assert!(!r.contains(15, 14) && !r.contains(14, 15) && !r.contains(4, 5));
        assert!(!Rect::new(5, 5, 0, 10).contains(5, 5));
        let huge = Rect::new(i32::MAX - 2, i32::MAX - 2, 10, 10);
        assert_eq!(huge.right(), i32::MAX);
        assert!(huge.intersect(Rect::new(0, 0, 100, 100)).is_empty());
        let moved = Rect::new(i32::MAX - 1, 0, 5, 5).translate(10, -10);
        assert_eq!((moved.x, moved.y), (i32::MAX, -10));
    }

    #[test]
    fn clamping_keeps_rectangles_inside_without_resizing() {
        let bounds = Rect::new(0, 0, 100, 80);
        assert_eq!(
            Rect::new(-20, -5, 30, 30).clamp_into(bounds),
            Rect::new(0, 0, 30, 30)
        );
        assert_eq!(
            Rect::new(90, 70, 30, 30).clamp_into(bounds),
            Rect::new(70, 50, 30, 30)
        );
        assert_eq!(
            Rect::new(10, 10, 30, 30).clamp_into(bounds),
            Rect::new(10, 10, 30, 30)
        );
        // Larger than the bounds: aligned to the top-left.
        assert_eq!(
            Rect::new(50, 50, 200, 200).clamp_into(bounds),
            Rect::new(0, 0, 200, 200)
        );
        assert_eq!(Rect::new(3, 4, 10, 10).inset(2), Rect::new(5, 6, 6, 6));
        assert!(Rect::new(0, 0, 3, 3).inset(2).is_empty());
    }
}
