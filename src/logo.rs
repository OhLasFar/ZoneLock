// ZoneLock logo: pure BGRA pixel painter, resolution independent.
// Shared between the runtime tray icon (`icon::app_icon`) and the
// build-time `.ico` encoder in `build.rs` (via `include!`), so the
// file must stay dependency-free (std only).
//
// Design: dark rounded square holding three zones — one tall left,
// two stacked right — with the top-right zone highlighted. Reads as
// a "window split into zones" even at 16x16.

// Dispattern "signal" colours, same as the settings page logo.
const BG: [u8; 4] = [0x22, 0x12, 0x0B, 0xFF]; // #0b1222 dark navy (BGRA)
const ZONE: [u8; 4] = [0xF6, 0x82, 0x3B, 0xFF]; // #3b82f6 signal blue
const ACCENT: [u8; 4] = [0xFD, 0xC5, 0x93, 0xFF]; // #93c5fd ice
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn in_rounded(fx: f32, fy: f32, l: f32, t: f32, r: f32, b: f32, rad: f32) -> bool {
    if fx < l || fx > r || fy < t || fy > b {
        return false;
    }
    // corner distance test
    let cx = if fx < l + rad {
        l + rad
    } else if fx > r - rad {
        r - rad
    } else {
        return true;
    };
    let cy = if fy < t + rad {
        t + rad
    } else if fy > b - rad {
        b - rad
    } else {
        return true;
    };
    let dx = fx - cx;
    let dy = fy - cy;
    dx * dx + dy * dy <= rad * rad
}

fn pixel(fx: f32, fy: f32) -> [u8; 4] {
    if !in_rounded(fx, fy, 0.04, 0.04, 0.96, 0.96, 0.18) {
        return CLEAR;
    }
    let in_zone = |l: f32, t: f32, r: f32, b: f32| {
        fx >= l && fx <= r && fy >= t && fy <= b
    };
    if in_zone(0.10, 0.10, 0.54, 0.90) {
        return ZONE;
    }
    if in_zone(0.58, 0.10, 0.90, 0.47) {
        return ACCENT;
    }
    if in_zone(0.58, 0.51, 0.90, 0.90) {
        return ZONE;
    }
    BG
}

/// `size`x`size` BGRA pixels, top-down row order.
pub fn pixels(size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let fx = (x as f32 + 0.5) / size as f32;
            let fy = (y as f32 + 0.5) / size as f32;
            let c = pixel(fx, fy);
            let i = (y * size + x) * 4;
            out[i..i + 4].copy_from_slice(&c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_transparent_center_zoned() {
        let px = pixels(64);
        let at = |x: usize, y: usize| {
            let i = (y * 64 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(1, 1), CLEAR, "corner should be transparent");
        assert_eq!(at(10, 32), ZONE, "left zone");
        assert_eq!(at(48, 15), ACCENT, "highlight zone");
        assert_eq!(at(36, 32), BG, "column gap is background");
    }
}
