use std::fs;
use std::path::PathBuf;

// The logo painter is pure std and dependency-free so it can be shared
// between this build script and the runtime icon module.
include!("src/logo.rs");

/// Wrap 32bpp BGRA pixels (top-down) into a single-image .ico.
fn encode_ico(px: &[u8], size: usize) -> Vec<u8> {
    let xor = size * size * 4;
    let mask_stride = (size.div_ceil(32)) * 4;
    let and = mask_stride * size;
    let img_size = 40 + xor + and;

    let mut out = Vec::with_capacity(22 + img_size);
    // ICONDIR
    out.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    // ICONDIRENTRY
    let dim = if size >= 256 { 0u8 } else { size as u8 };
    out.push(dim); // width (0 = 256)
    out.push(dim); // height
    out.push(0); // palette colors
    out.push(0); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    out.extend_from_slice(&(img_size as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes()); // image offset
    // BITMAPINFOHEADER (height field doubles: XOR + AND)
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(img_size as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes()); // x ppm
    out.extend_from_slice(&0i32.to_le_bytes()); // y ppm
    out.extend_from_slice(&0u32.to_le_bytes()); // colors used
    out.extend_from_slice(&0u32.to_le_bytes()); // important colors
    // XOR bitmap: BGRA rows bottom-up
    for y in (0..size).rev() {
        out.extend_from_slice(&px[y * size * 4..(y + 1) * size * 4]);
    }
    // AND mask: all zero (opaque; real transparency comes from BGRA alpha)
    out.resize(out.len() + and, 0);
    out
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "ZoneLock");
    res.set(
        "FileDescription",
        "ZoneLock - keep maximize and F11 inside monitor zones",
    );
    res.set("LegalCopyright", "ZoneLock contributors");
    res.set_manifest(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/PM</dpiAware>
    </windowsSettings>
  </application>
</assembly>"#,
    );

    let ico_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = ico_dir.join("zonelock.ico");
    let px = pixels(256);
    fs::write(&ico_path, encode_ico(&px, 256)).expect("write ico");
    res.set_icon(ico_path.to_str().unwrap());

    let _ = res.compile();
}
