//! Pixel-rectangle math, independent of the Win32 `RECT` type so it is
//! unit-testable without a desktop session.

use serde::{Deserialize, Serialize};

/// Physical-pixel rectangle (exclusive bottom/right, like `RECT`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    pub fn center(&self) -> (i32, i32) {
        (
            self.left + self.width() / 2,
            self.top + self.height() / 2,
        )
    }

    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// Largest overlapping rectangle between self and other, if any.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let r = Rect::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        );
        (!r.is_empty()).then_some(r)
    }

    pub fn area(&self) -> i64 {
        self.width() as i64 * self.height() as i64
    }

    pub fn overlap_area(&self, other: &Rect) -> i64 {
        self.intersection(other).map_or(0, |r| r.area())
    }

    /// Grow (positive) or shrink (negative) each edge by `px`.
    pub fn inflate(&self, px: i32) -> Rect {
        Rect::new(
            self.left - px,
            self.top - px,
            self.right + px,
            self.bottom + px,
        )
    }

    /// Edges equal within `tol` pixels. Used to detect "covers the monitor".
    /// Same size, slid so it fits inside `area` (right/bottom overflow is
    /// pushed back first; if it is wider/taller than `area` it is pinned to
    /// the left/top edge). None when it already fits.
    pub fn shifted_into(&self, area: &Rect) -> Option<Rect> {
        let mut dx = 0;
        if self.right > area.right {
            dx = area.right - self.right;
        }
        if self.left + dx < area.left {
            dx = area.left - self.left;
        }
        let mut dy = 0;
        if self.bottom > area.bottom {
            dy = area.bottom - self.bottom;
        }
        if self.top + dy < area.top {
            dy = area.top - self.top;
        }
        (dx != 0 || dy != 0).then(|| {
            Rect::new(self.left + dx, self.top + dy, self.right + dx, self.bottom + dy)
        })
    }

    pub fn approx_eq(&self, other: &Rect, tol: i32) -> bool {
        (self.left - other.left).abs() <= tol
            && (self.top - other.top).abs() <= tol
            && (self.right - other.right).abs() <= tol
            && (self.bottom - other.bottom).abs() <= tol
    }

    /// Smallest rect covering both self and other.
    pub fn union(&self, other: &Rect) -> Rect {
        Rect::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    /// True when `inner` is fully inside `outer` with `tol` slack.
    pub fn is_within(&self, outer: &Rect, tol: i32) -> bool {
        self.left >= outer.left - tol
            && self.top >= outer.top - tol
            && self.right <= outer.right + tol
            && self.bottom <= outer.bottom + tol
    }

    /// Rect that covers `outer` exactly (used in tests).
    pub fn equals(&self, other: &Rect) -> bool {
        self == other
    }
}

/// Fractional rectangle inside a monitor, 0.0..=1.0 on each axis.
/// Storing fractions keeps layouts stable across DPI and resolution changes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FracRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl FracRect {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        debug_assert!(left >= 0.0 && top >= 0.0 && right <= 1.0 && bottom <= 1.0);
        debug_assert!(right > left && bottom > top);
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Pixel rect inside `monitor`, minus `gap` on every edge.
    pub fn to_pixels(&self, monitor: &Rect, gap: i32) -> Rect {
        let w = monitor.width() as f64;
        let h = monitor.height() as f64;
        let r = Rect::new(
            monitor.left + (self.left * w).round() as i32,
            monitor.top + (self.top * h).round() as i32,
            monitor.left + (self.right * w).round() as i32,
            monitor.top + (self.bottom * h).round() as i32,
        );
        r.inflate(-gap)
    }

    /// Fractions of `monitor` covered by `px` (clamped to 0..1).
    pub fn from_pixels(px: &Rect, monitor: &Rect) -> FracRect {
        let w = monitor.width().max(1) as f64;
        let h = monitor.height().max(1) as f64;
        let f = |v: f64| v.clamp(0.0, 1.0);
        FracRect {
            left: f((px.left - monitor.left) as f64 / w),
            top: f((px.top - monitor.top) as f64 / h),
            right: f((px.right - monitor.left) as f64 / w),
            bottom: f((px.bottom - monitor.top) as f64 / h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_into_keeps_size_and_stays_on_screen() {
        let work = Rect::new(0, 0, 1920, 1032);
        // Chrome min width past the right edge: slides left
        assert_eq!(
            Rect::new(1440, 0, 1954, 1032).shifted_into(&work),
            Some(Rect::new(1406, 0, 1920, 1032))
        );
        // already inside
        assert_eq!(Rect::new(0, 0, 514, 1032).shifted_into(&work), None);
        // wider than the area: pinned left
        assert_eq!(
            Rect::new(100, 0, 2100, 500).shifted_into(&work),
            Some(Rect::new(0, 0, 2000, 500))
        );
        // bottom overflow
        assert_eq!(
            Rect::new(0, 600, 500, 1100).shifted_into(&work),
            Some(Rect::new(0, 532, 500, 1032))
        );
    }

    #[test]
    fn intersection_basic() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 150, 150);
        assert_eq!(a.intersection(&b), Some(Rect::new(50, 50, 100, 100)));
        assert_eq!(a.overlap_area(&b), 2500);
    }

    #[test]
    fn frac_to_pixels_5050() {
        let mon = Rect::new(0, 0, 3440, 1440);
        let left = FracRect::new(0.0, 0.0, 0.5, 1.0).to_pixels(&mon, 0);
        assert_eq!(left, Rect::new(0, 0, 1720, 1440));
        let with_gap = FracRect::new(0.0, 0.0, 0.5, 1.0).to_pixels(&mon, 8);
        assert_eq!(with_gap, Rect::new(8, 8, 1712, 1432));
    }

    #[test]
    fn frac_roundtrip_on_second_monitor() {
        let mon = Rect::new(3440, 0, 6000, 1080);
        let px = Rect::new(3440, 0, 4720, 1080);
        let frac = FracRect::from_pixels(&px, &mon);
        assert!((frac.left - 0.0).abs() < 1e-6);
        assert!((frac.right - 0.5).abs() < 1e-6);
        assert_eq!(frac.to_pixels(&mon, 0), px);
    }

    #[test]
    fn covers_monitor_within_tolerance() {
        let mon = Rect::new(0, 0, 2560, 1440);
        let win = Rect::new(-2, 0, 2561, 1440);
        assert!(win.approx_eq(&mon, 4));
        let off = Rect::new(-2, 0, 2561, 1400);
        assert!(!off.approx_eq(&mon, 4));
    }
}
