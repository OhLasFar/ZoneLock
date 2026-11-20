// ZoneLock logo: pure BGRA pixel painter, resolution independent.
// Shared between the runtime tray icon (`icon::app_icon`) and the
// build-time `.ico` encoder in `build.rs` (via `include!`), so the
// file must stay dependency-free (std only).
//
// Design ("locked zone"): dark rounded square holding two zones, a
// wide left one (2/3) carrying a keyhole and a narrow right one (1/3).
// Same geometry as docs/assets/logo.svg and the settings page logo
// (24-unit grid, divided by 24 below). Edges are 4x4 supersampled.

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

/// Keyhole of the big zone: a round head over a slot that widens down.
fn in_keyhole(fx: f32, fy: f32) -> bool {
    let u = |v: f32| v / 24.0;
    let (cx, cy, r) = (u(8.75), u(10.2), u(2.1));
    if (fx - cx) * (fx - cx) + (fy - cy) * (fy - cy) <= r * r {
        return true;
    }
    let (top, bot) = (u(11.2), u(15.8));
    if fy < top || fy > bot {
        return false;
    }
    let k = (fy - top) / (bot - top);
    let l = u(7.7) + (u(7.1) - u(7.7)) * k;
    let r = u(9.8) + (u(10.4) - u(9.8)) * k;
    fx >= l && fx <= r
}

fn pixel(fx: f32, fy: f32) -> [u8; 4] {
    let u = |v: f32| v / 24.0;
    if !in_rounded(fx, fy, u(0.5), u(0.5), u(23.5), u(23.5), u(5.5)) {
        return CLEAR;
    }
    if in_rounded(fx, fy, u(3.5), u(3.5), u(14.0), u(20.5), u(2.0)) {
        return if in_keyhole(fx, fy) { BG } else { ZONE };
    }
    if in_rounded(fx, fy, u(15.2), u(3.5), u(20.5), u(20.5), u(2.0)) {
        return ACCENT;
    }
    BG
}

/// `size`x`size` BGRA pixels, top-down row order.
pub fn pixels(size: usize) -> Vec<u8> {
    const SS: usize = 4; // supersampling per axis
    let mut out = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            // average with alpha weighting (straight alpha output)
            let (mut b, mut g, mut r, mut a) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    let c = pixel(fx, fy);
                    let ca = c[3] as u32;
                    b += c[0] as u32 * ca;
                    g += c[1] as u32 * ca;
                    r += c[2] as u32 * ca;
                    a += ca;
                }
            }
            let i = (y * size + x) * 4;
            if a > 0 {
                out[i] = (b / a) as u8;
                out[i + 1] = (g / a) as u8;
                out[i + 2] = (r / a) as u8;
                out[i + 3] = (a / (SS * SS) as u32) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_transparent_center_zoned() {
        let px = pixels(96);
        let at = |x: usize, y: usize| {
            let i = (y * 96 + x) * 4;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(1, 1), CLEAR, "corner should be transparent");
        assert_eq!(at(20, 70), ZONE, "big zone");
        assert_eq!(at(35, 41), BG, "keyhole is cut out of the big zone");
        assert_eq!(at(71, 48), ACCENT, "narrow zone");
        assert_eq!(at(58, 48), BG, "gap between the zones");
    }
}
