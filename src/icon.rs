//! Runtime HICON built from the logo painter — no .ico file needed at
//! runtime (the exe also embeds one via build.rs for Explorer).

use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO};

/// Build an HICON of `size`x`size` pixels from the logo painter.
/// The caller owns the icon; it lives for the process lifetime in
/// practice (tray icon + window class icons).
pub fn app_icon(size: i32) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    let n = size as usize;
    let px = crate::logo::pixels(n); // top-down BGRA
    // CreateBitmap consumes bottom-up rows.
    let row = n * 4;
    let mut flipped = vec![0u8; px.len()];
    for y in 0..n {
        flipped[(n - 1 - y) * row..(n - y) * row]
            .copy_from_slice(&px[y * row..(y + 1) * row]);
    }
    let mask_stride = (n.div_ceil(32)) * 4;
    let mask_bits = vec![0u8; mask_stride * n]; // all-opaque
    unsafe {
        let color = CreateBitmap(
            size,
            size,
            1,
            32,
            Some(flipped.as_ptr() as *const _),
        );
        let mask =
            CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let ii = ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon
    }
}
