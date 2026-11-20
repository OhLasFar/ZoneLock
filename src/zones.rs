//! Zone model and layouts. A layout is a list of fractional zones attached
//! to a monitor identity. Zones never overlap in practice but the model does
//! not forbid it; lookup picks the zone with the largest overlap.

use serde::{Deserialize, Serialize};

use crate::rect::{FracRect, Rect};

/// Identity used to match a layout to a physical monitor.
/// `name` is the GDI device name (e.g. `\\\\.\\DISPLAY1`). When it is empty or
/// does not match any connected monitor, `index` (left-to-right order) is used
/// as a fallback so a layout still applies after a monitor swap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorKey {
    pub name: String,
    #[serde(default)]
    pub index: usize,
}

/// One named zone on one monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub name: String,
    pub rect: FracRect,
}

/// A complete layout for one monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub name: String,
    pub zones: Vec<Zone>,
}

impl Layout {
    /// Zone containing point (x, y) in monitor pixel space, or the zone with
    /// the largest overlap with `window_px` when `point` is None.
    pub fn zone_at_point(&self, monitor_px: &Rect, x: i32, y: i32) -> Option<usize> {
        let px = Rect::new(x, y, x + 1, y + 1);
        self.zone_for_rect(monitor_px, &px)
    }

    /// Best zone for a window: largest overlap of the window rect with the
    /// pixel rect of each zone. Ties keep the earlier zone.
    pub fn zone_for_rect(&self, monitor_px: &Rect, window_px: &Rect) -> Option<usize> {
        let mut best: Option<(usize, i64)> = None;
        for (i, z) in self.zones.iter().enumerate() {
            let zp = z.rect.to_pixels(monitor_px, 0);
            let a = zp.overlap_area(window_px);
            let better = match best {
                None => a > 0,
                Some((_, ba)) => a > ba,
            };
            if better {
                best = Some((i, a));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Fractional rect a window should occupy when snapped to zone `idx`.
    pub fn zone_frac(&self, idx: usize) -> Option<FracRect> {
        self.zones.get(idx).map(|z| z.rect)
    }

    /// Two 50/50 columns. Default fallback layout.
    pub fn halves() -> Layout {
        Layout {
            name: "halves".into(),
            zones: vec![
                Zone {
                    name: "left".into(),
                    rect: FracRect::new(0.0, 0.0, 0.5, 1.0),
                },
                Zone {
                    name: "right".into(),
                    rect: FracRect::new(0.5, 0.0, 1.0, 1.0),
                },
            ],
        }
    }
}

/// Layouts for every monitor, keyed by device name with index fallback.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayoutSet {
    /// monitor device name -> layout
    #[serde(default)]
    pub by_monitor: std::collections::BTreeMap<String, Layout>,
    /// layout applied to any monitor that has no explicit entry
    #[serde(default)]
    pub default: Option<Layout>,
}

impl LayoutSet {
    /// Resolve the layout for a monitor, trying device name then index
    /// (`__index_N` synthetic keys) then the default.
    pub fn for_monitor(&self, device_name: &str, index: usize) -> Option<&Layout> {
        self.by_monitor
            .get(device_name)
            .or_else(|| self.by_monitor.get(&index_key(index)))
            .or(self.default.as_ref())
    }

    pub fn set_for_monitor(&mut self, device_name: &str, layout: Layout) {
        self.by_monitor.insert(device_name.to_string(), layout);
    }

    /// Ensure every listed monitor has a layout (fills with halves).
    pub fn ensure(&mut self, monitors: &[(String, usize)]) {
        for (name, _) in monitors {
            self.by_monitor
                .entry(name.clone())
                .or_insert_with(Layout::halves);
        }
        if self.default.is_none() {
            self.default = Some(Layout::halves());
        }
    }
}

pub fn index_key(index: usize) -> String {
    format!("__index_{index}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_for_window_picks_largest_overlap() {
        let mon = Rect::new(0, 0, 3440, 1440);
        let layout = Layout::halves();
        let leftish = Rect::new(100, 100, 1600, 800);
        assert_eq!(layout.zone_for_rect(&mon, &leftish), Some(0));
        let rightish = Rect::new(2000, 100, 3000, 800);
        assert_eq!(layout.zone_for_rect(&mon, &rightish), Some(1));
        let centered = Rect::new(1500, 100, 1940, 800); // 220 left, 220 right
        assert_eq!(layout.zone_for_rect(&mon, &centered), Some(0)); // tie -> first
    }

    #[test]
    fn point_lookup() {
        let mon = Rect::new(0, 0, 1000, 500);
        let layout = Layout::halves();
        assert_eq!(layout.zone_at_point(&mon, 10, 10), Some(0));
        assert_eq!(layout.zone_at_point(&mon, 900, 10), Some(1));
    }

    #[test]
    fn layoutset_fallbacks() {
        let mut set = LayoutSet::default();
        set.set_for_monitor("\\\\.\\DISPLAY2", Layout::halves());
        set.default = Some(Layout::halves());
        assert!(set.for_monitor("\\\\.\\DISPLAY2", 0).is_some());
        // unknown name falls back to index key then default
        assert!(set.for_monitor("\\\\.\\DISPLAY9", 5).is_some());
    }
}
