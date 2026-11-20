//! Keep windows in their zones when a display changes (resolution, scale,
//! taskbar, unplug/replug).
//!
//! When a display changes, Windows itself moves and squeezes the windows
//! on it. Those moves reach the watcher before it sees the new
//! geometry, so they look like the user moving windows out of their
//! zones. To undo that, each window that sat in a zone (snapped, or
//! maximize-contained) keeps a `Snap`: display name + zone index. A move
//! out of the zone only marks it `lost`. If the display change shows up
//! within `GRACE`, the window is put back into the same zone,
//! recomputed for the new geometry. A window the user really moved
//! away loses its snap after `GRACE`.
//!
//! A display that disappears turns its snaps into orphans: Windows
//! moves those windows elsewhere, and they go back into their zone if
//! the display comes back.
//!
//! Pure bookkeeping; the Win32 part is in watcher.rs.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::rect::{FracRect, Rect};

/// How long a "moved out of its zone" may precede the display change and
/// still count as caused by it.
pub const GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub struct Snap {
    pub monitor: String,
    pub zone: usize,
    /// Maximize-contained (re-locked as such), else a plain snap.
    pub maximize: bool,
    /// When the window was last seen out of its zone (None = in it).
    pub lost: Option<Instant>,
    /// Its display is gone; waits for it to come back.
    pub orphan: bool,
}

impl Snap {
    pub fn live(&self, now: Instant) -> bool {
        self.orphan || self.lost.is_none_or(|t| now.duration_since(t) < GRACE)
    }
}

/// What the watcher saw after evaluating a window.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    /// Plain window sitting exactly on zone `zone` of `monitor`.
    InZone { monitor: String, zone: usize },
    /// Maximize-contained in zone `zone` of `monitor`.
    Contained { monitor: String, zone: usize },
    /// Plain window anywhere else, on display `monitor`.
    Elsewhere { monitor: String },
    /// Fullscreen-held, ignored, ...: leave the record alone.
    Other,
    /// No longer managed.
    Gone,
}

#[derive(Default)]
pub struct Snaps {
    map: HashMap<isize, Snap>,
}

impl Snaps {
    pub fn get(&self, hwnd: isize) -> Option<&Snap> {
        self.map.get(&hwnd)
    }

    pub fn observe(&mut self, hwnd: isize, seen: Seen, now: Instant) {
        let fresh = |monitor: String, zone: usize, maximize: bool| Snap {
            monitor,
            zone,
            maximize,
            lost: None,
            orphan: false,
        };
        match seen {
            Seen::InZone { monitor, zone } => {
                self.map.insert(hwnd, fresh(monitor, zone, false));
            }
            Seen::Contained { monitor, zone } => {
                self.map.insert(hwnd, fresh(monitor, zone, true));
            }
            Seen::Elsewhere { monitor } => {
                if let Some(s) = self.map.get_mut(&hwnd) {
                    // an orphan parked on another display by Windows is
                    // still waiting for its own display
                    if s.orphan && s.monitor != monitor {
                        return;
                    }
                    s.orphan = false;
                    if s.lost.is_none() {
                        s.lost = Some(now);
                    }
                }
            }
            Seen::Other => {}
            Seen::Gone => {
                self.map.remove(&hwnd);
            }
        }
    }

    /// Drop stale records and dead windows.
    pub fn prune(&mut self, now: Instant, alive: impl Fn(isize) -> bool) {
        self.map.retain(|h, s| s.live(now) && alive(*h));
    }

    /// The display set changed. `present` = display names now connected.
    /// Returns the windows to put back in their zone, with their record.
    /// Records whose display left become orphans.
    pub fn on_display_change(&mut self, present: &[String], now: Instant) -> Vec<(isize, Snap)> {
        let mut out = Vec::new();
        for (h, s) in self.map.iter_mut() {
            if !s.live(now) {
                continue;
            }
            if present.iter().any(|m| *m == s.monitor) {
                s.orphan = false;
                out.push((*h, s.clone()));
            } else {
                s.orphan = true;
            }
        }
        out.sort_by_key(|(h, _)| *h);
        out
    }

    /// The window was put back in its zone.
    pub fn settled(&mut self, hwnd: isize) {
        if let Some(s) = self.map.get_mut(&hwnd) {
            s.lost = None;
        }
    }
}

/// The visible frame `v` is what snapping into zone `z` leaves: exactly the
/// zone, or - for an app whose minimum size beats the zone (Chrome
/// ~500 px, Firefox ~516 px, x scale) - the zone grown along one axis by
/// at most a third, still covering it. A window spanning two zones (an
/// Aero Snap half over a 4-column layout) is not "in" either.
pub fn fills_zone(v: &Rect, z: &Rect) -> bool {
    fills_zone_within(v, z, 3)
}

/// `fills_zone` with an edge tolerance of `tol` px. With a WINDOW rect
/// (invisible resize borders included, ~7-11 px depending on DPI) use a
/// tolerance that covers those borders.
pub fn fills_zone_within(v: &Rect, z: &Rect, tol: i32) -> bool {
    let near = |a: i32, b: i32| (a - b).abs() <= tol;
    let same_x = near(v.left, z.left) && near(v.right, z.right);
    let same_y = near(v.top, z.top) && near(v.bottom, z.bottom);
    let grown = |lo: i32, hi: i32, zlo: i32, zhi: i32| {
        let (len, zlen) = (hi - lo, zhi - zlo);
        lo <= zlo + tol && hi >= zhi - tol && len <= zlen + zlen / 3
    };
    (same_x && same_y)
        || (same_y && grown(v.left, v.right, z.left, z.right))
        || (same_x && grown(v.top, v.bottom, z.top, z.bottom))
}

/// Same spot, relative to a work area that changed size.
pub fn rescale(rect: &Rect, old_work: &Rect, new_work: &Rect) -> Rect {
    if old_work.is_empty() || old_work == new_work {
        return *rect;
    }
    FracRect::from_pixels(rect, old_work).to_pixels(new_work, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn display_change_brings_back_windows_windows_moved() {
        let t0 = Instant::now();
        let mut s = Snaps::default();
        s.observe(1, Seen::InZone { monitor: d("D2"), zone: 1 }, t0);
        s.observe(2, Seen::Contained { monitor: d("D2"), zone: 0 }, t0);
        // Windows squeezes both before we see the new geometry
        s.observe(1, Seen::Elsewhere { monitor: d("D2") }, t0);
        s.observe(2, Seen::Elsewhere { monitor: d("D2") }, t0);
        let back = s.on_display_change(&[d("D1"), d("D2")], t0 + Duration::from_millis(50));
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].0, back[0].1.zone, back[0].1.maximize), (1, 1, false));
        assert_eq!((back[1].0, back[1].1.zone, back[1].1.maximize), (2, 0, true));
    }

    #[test]
    fn a_window_the_user_moved_away_long_ago_stays_put() {
        let t0 = Instant::now();
        let mut s = Snaps::default();
        s.observe(1, Seen::InZone { monitor: d("D2"), zone: 1 }, t0);
        s.observe(1, Seen::Elsewhere { monitor: d("D2") }, t0);
        let later = t0 + GRACE + Duration::from_secs(1);
        assert!(s.on_display_change(&[d("D2")], later).is_empty());
        s.prune(later, |_| true);
        assert!(s.get(1).is_none());
        // a window that never sat in a zone has no record at all
        s.observe(5, Seen::Elsewhere { monitor: d("D2") }, t0);
        assert!(s.get(5).is_none());
    }

    #[test]
    fn unplugged_display_waits_for_its_return() {
        let t0 = Instant::now();
        let mut s = Snaps::default();
        s.observe(1, Seen::InZone { monitor: d("D2"), zone: 1 }, t0);
        // D2 unplugged: orphan, nothing to place now
        assert!(s.on_display_change(&[d("D1")], t0).is_empty());
        // Windows parks it on D1; much later D2 comes back
        s.observe(1, Seen::Elsewhere { monitor: d("D1") }, t0);
        let later = t0 + Duration::from_secs(600);
        s.prune(later, |_| true);
        let back = s.on_display_change(&[d("D1"), d("D2")], later);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].1.monitor, "D2");
        // the user snapping it elsewhere meanwhile replaces the record
        let mut s2 = Snaps::default();
        s2.observe(1, Seen::InZone { monitor: d("D2"), zone: 1 }, t0);
        s2.on_display_change(&[d("D1")], t0);
        s2.observe(1, Seen::InZone { monitor: d("D1"), zone: 0 }, t0);
        assert_eq!(s2.get(1).unwrap().monitor, "D1");
        assert!(!s2.get(1).unwrap().orphan);
    }

    #[test]
    fn fills_zone_accepts_min_size_overflow_only() {
        let z = Rect::new(1920, 0, 2560, 1540); // a 640 px column
        assert!(fills_zone(&Rect::new(1921, 1, 2558, 1540), &z));
        // Firefox 645 px wide, slid back on screen: still in its zone
        assert!(fills_zone(&Rect::new(1915, 0, 2560, 1540), &z));
        // an Aero Snap half over two columns is in neither
        assert!(!fills_zone(&Rect::new(1280, 0, 2560, 1540), &z));
        // a free-floating window
        assert!(!fills_zone(&Rect::new(2000, 100, 2500, 900), &z));
    }

    #[test]
    fn rescale_keeps_the_relative_spot() {
        let old = Rect::new(0, 0, 1920, 1040);
        let new = Rect::new(0, 0, 1280, 680);
        assert_eq!(rescale(&Rect::new(960, 0, 1920, 1040), &old, &new), Rect::new(640, 0, 1280, 680));
        assert_eq!(rescale(&Rect::new(5, 6, 7, 8), &old, &old), Rect::new(5, 6, 7, 8));
    }
}
