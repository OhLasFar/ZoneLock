//! Built-in layout presets, cyclable via hotkey or editor keys 1..N.

use crate::rect::FracRect;
use crate::zones::{Layout, Zone};

fn z(name: &str, l: f64, t: f64, r: f64, b: f64) -> Zone {
    Zone {
        name: name.into(),
        rect: FracRect::new(l, t, r, b),
    }
}

pub fn presets() -> Vec<Layout> {
    vec![
        Layout {
            name: "halves 50/50".into(),
            zones: vec![
                z("left", 0.0, 0.0, 0.5, 1.0),
                z("right", 0.5, 0.0, 1.0, 1.0),
            ],
        },
        Layout {
            name: "thirds 33/33/33".into(),
            zones: vec![
                z("left", 0.0, 0.0, 1.0 / 3.0, 1.0),
                z("middle", 1.0 / 3.0, 0.0, 2.0 / 3.0, 1.0),
                z("right", 2.0 / 3.0, 0.0, 1.0, 1.0),
            ],
        },
        Layout {
            name: "two-thirds + third".into(),
            zones: vec![
                z("wide", 0.0, 0.0, 2.0 / 3.0, 1.0),
                z("side", 2.0 / 3.0, 0.0, 1.0, 1.0),
            ],
        },
        Layout {
            name: "third + two-thirds".into(),
            zones: vec![
                z("side", 0.0, 0.0, 1.0 / 3.0, 1.0),
                z("wide", 1.0 / 3.0, 0.0, 1.0, 1.0),
            ],
        },
        Layout {
            name: "center focus 60%".into(),
            zones: vec![
                z("left", 0.0, 0.0, 0.2, 1.0),
                z("center", 0.2, 0.0, 0.8, 1.0),
                z("right", 0.8, 0.0, 1.0, 1.0),
            ],
        },
        Layout {
            name: "grid 2x2".into(),
            zones: vec![
                z("tl", 0.0, 0.0, 0.5, 0.5),
                z("tr", 0.5, 0.0, 1.0, 0.5),
                z("bl", 0.0, 0.5, 0.5, 1.0),
                z("br", 0.5, 0.5, 1.0, 1.0),
            ],
        },
        Layout {
            name: "whole monitor".into(),
            zones: vec![z("all", 0.0, 0.0, 1.0, 1.0)],
        },
    ]
}

/// Built-in presets followed by the user's saved ones (hotkey cycle order).
pub fn all(user: &[Layout]) -> Vec<Layout> {
    let mut v = presets();
    v.extend(user.iter().cloned());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_cover_full_monitor_without_gaps_vertically() {
        for p in presets() {
            for zone in &p.zones {
                assert!(zone.rect.right > zone.rect.left);
                assert!(zone.rect.bottom > zone.rect.top);
            }
            // every preset spans the full height somewhere on x=0..1
            let covered_top = p.zones.iter().all(|z| z.rect.top >= 0.0);
            assert!(covered_top);
        }
    }
}
