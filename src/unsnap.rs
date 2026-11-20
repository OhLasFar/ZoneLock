//! "Unsnap": dragging a window out of a zone gives it back the size it had
//! before it was snapped (what Aero Snap does).
//!
//! The size is recorded when ZoneLock puts a window in a zone (Shift+drag
//! snap, next-zone hotkey, maximize kept in the zone), together with the
//! rect it was placed on. Moving from one zone to another keeps the
//! original size. When a drag starts on a window still on its snapped
//! rect and the window really moves (same size, new position; a resize
//! drag changes the size instead), it is resized to the old size under
//! the cursor, at the same relative spot along its width.
//!
//! Window rects (GetWindowRect) throughout. Shared by the UI thread
//! (hotkey) and the watcher thread (drag, maximize), hence the Mutex.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::rect::Rect;

/// Slack when comparing "still on the snapped rect".
const TOL: i32 = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    /// Width / height before the first snap.
    pub size: (i32, i32),
    /// Where the window was put (window rect); None until placed.
    pub snapped: Option<Rect>,
}

#[derive(Default)]
pub struct Book {
    map: HashMap<isize, Entry>,
}

impl Book {
    /// About to put `hwnd` (now at `cur`) in a zone.
    pub fn before_snap(&mut self, hwnd: isize, cur: &Rect) {
        let chained = self
            .map
            .get(&hwnd)
            .and_then(|e| e.snapped)
            .is_some_and(|s| s.approx_eq(cur, TOL));
        if !chained && cur.width() > 0 && cur.height() > 0 {
            self.map.insert(
                hwnd,
                Entry { size: (cur.width(), cur.height()), snapped: None },
            );
        }
    }

    /// `hwnd` now sits on `placed`.
    pub fn after_snap(&mut self, hwnd: isize, placed: &Rect) {
        if let Some(e) = self.map.get_mut(&hwnd) {
            e.snapped = Some(*placed);
        }
    }

    pub fn get(&self, hwnd: isize) -> Option<&Entry> {
        self.map.get(&hwnd)
    }

    pub fn forget(&mut self, hwnd: isize) {
        self.map.remove(&hwnd);
    }

    /// A drag started at `start` and the window is now at `now` with the
    /// cursor at (cx, cy). Returns where to put the window back at its
    /// pre-snap size, or None (not snapped, a resize, not moved yet).
    pub fn on_drag_motion(
        &self,
        hwnd: isize,
        start: &Rect,
        now: &Rect,
        cx: i32,
        cy: i32,
        work: Option<&Rect>,
    ) -> Option<Rect> {
        let e = self.map.get(&hwnd)?;
        if !e.snapped?.approx_eq(start, TOL) {
            return None;
        }
        let resized = (now.width() - start.width()).abs() > 1
            || (now.height() - start.height()).abs() > 1;
        if resized || (now.left == start.left && now.top == start.top) {
            return None;
        }
        let (w, h) = e.size;
        if (w - now.width()).abs() <= 2 && (h - now.height()).abs() <= 2 {
            return None; // already that size
        }
        // keep the cursor at the same fraction of the width and at the
        // same height inside the title bar
        let fx = if now.width() > 0 {
            ((cx - now.left) as f64 / now.width() as f64).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let mut left = cx - (fx * w as f64).round() as i32;
        let top = now.top.min(cy);
        // a big pre-snap size grabbed near its edge would land half off
        // the display: slide it inside the work area, as long as the
        // cursor stays on the window
        if let Some(wk) = work {
            const GRIP: i32 = 24;
            let lo = (wk.left).max(cx - w + GRIP);
            let hi = (wk.right - w).min(cx - GRIP);
            if lo <= hi {
                left = left.clamp(lo, hi);
            }
        }
        Some(Rect::new(left, top, left + w, top + h))
    }
}

/// WM_NCHITTEST codes that start a resize: HTLEFT (10) .. HTBOTTOMRIGHT
/// (17) and HTBORDER (18).
pub fn is_resize_hit(code: i32) -> bool {
    (10..=18).contains(&code)
}

/// Cursor on the visible frame edge, or up to `band` px outside it
/// (where Windows 10/11 keep the invisible resize borders), or `band`/2
/// inside: a resize grab.
pub fn near_edge(vis: &Rect, x: i32, y: i32, band: i32) -> bool {
    let inside_outer = x >= vis.left - band
        && x <= vis.right + band
        && y >= vis.top - band
        && y <= vis.bottom + band;
    let inner = band / 2;
    let in_core = x > vis.left + inner
        && x < vis.right - inner
        && y > vis.top + inner
        && y < vis.bottom - inner;
    inside_outer && !in_core
}

static BOOK: Mutex<Option<Book>> = Mutex::new(None);

pub fn with<R>(f: impl FnOnce(&mut Book) -> R) -> R {
    let mut g = BOOK.lock().unwrap();
    f(g.get_or_insert_with(Book::default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_out_of_a_zone_restores_the_size_under_the_cursor() {
        let mut b = Book::default();
        let before = Rect::new(300, 200, 1100, 800); // 800x600
        b.before_snap(1, &before);
        let zone = Rect::new(-7, 0, 967, 1047);
        b.after_snap(1, &zone);
        // snapped again into the next zone: the original size is kept
        b.before_snap(1, &zone);
        let zone2 = Rect::new(953, 0, 1927, 1047);
        b.after_snap(1, &zone2);
        assert_eq!(b.get(1).unwrap().size, (800, 600));

        // drag grabbed at the middle of the title bar, moved 30 px
        let now = Rect::new(983, 0, 1957, 1047);
        let (cx, cy) = (983 + 487, 12);
        let r = b.on_drag_motion(1, &zone2, &now, cx, cy, None).unwrap();
        assert_eq!((r.width(), r.height()), (800, 600));
        assert_eq!(r.left, cx - 400);
        assert_eq!(r.top, 0);
    }

    #[test]
    fn resize_grabs_are_recognised() {
        // hit-test codes: caption / client move, borders resize
        assert!(!is_resize_hit(2)); // HTCAPTION
        assert!(!is_resize_hit(1)); // HTCLIENT
        for c in 10..=18 {
            assert!(is_resize_hit(c), "{c}");
        }
        // geometry fallback
        let v = Rect::new(100, 100, 900, 700);
        assert!(near_edge(&v, 96, 400, 6)); // just outside the left edge
        assert!(near_edge(&v, 901, 699, 6)); // bottom-right corner
        assert!(near_edge(&v, 500, 101, 6)); // top edge
        assert!(!near_edge(&v, 500, 115, 6)); // title bar
        assert!(!near_edge(&v, 50, 400, 6)); // far outside
    }

    #[test]
    fn a_big_restored_window_stays_on_screen_under_the_cursor() {
        let mut b = Book::default();
        // was almost as wide as the display before it got maximized
        b.before_snap(1, &Rect::new(0, 0, 2400, 1500));
        let zone = Rect::new(633, 0, 1287, 1547);
        b.after_snap(1, &zone);
        let work = Rect::new(0, 0, 2560, 1540);
        // grabbed near the zone's right end: the raw spot is x = -1162
        let now = Rect::new(653, 0, 1307, 1547);
        let (cx, cy) = (1250, 15);
        let r = b.on_drag_motion(1, &zone, &now, cx, cy, Some(&work)).unwrap();
        assert_eq!(r.width(), 2400);
        assert!(r.left >= 0 && r.right <= 2560, "{r:?}");
        assert!(r.left + 24 <= cx && cx <= r.right - 24);
    }

    #[test]
    fn no_restore_for_resizes_unsnapped_or_unmoved_windows() {
        let mut b = Book::default();
        b.before_snap(1, &Rect::new(300, 200, 1100, 800));
        let zone = Rect::new(-7, 0, 967, 1047);
        b.after_snap(1, &zone);
        // resize drag: size changes
        assert!(b.on_drag_motion(1, &zone, &Rect::new(-7, 0, 1000, 1047), 990, 500, None).is_none());
        // not moved yet
        assert!(b.on_drag_motion(1, &zone, &zone, 400, 10, None).is_none());
        // the window left its zone some other way before this drag
        let elsewhere = Rect::new(500, 300, 1474, 1347);
        assert!(b.on_drag_motion(1, &elsewhere, &Rect::new(520, 300, 1494, 1347), 900, 310, None).is_none());
        // never snapped by us
        assert!(b.on_drag_motion(2, &zone, &Rect::new(10, 0, 984, 1047), 400, 10, None).is_none());
        // a moved-away window snapped again records its new size
        b.before_snap(1, &Rect::new(0, 0, 500, 400));
        assert_eq!(b.get(1).unwrap().size, (500, 400));
    }
}
