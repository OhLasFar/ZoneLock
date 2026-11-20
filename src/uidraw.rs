//! Shared GDI style for ZoneLock surfaces: one dark palette, rounded
//! geometry helpers and double-buffered painting. Colors are stored as
//! 0x00BBGGRR (COLORREF byte order).

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::*;

pub const BG: u32 = 0x0018110E; // #0E1118 page background
pub const CARD: u32 = 0x00281B16; // #161B28 raised card
pub const FIELD: u32 = 0x0034261D; // #1D2634 input field
pub const EDGE: u32 = 0x00402E22; // #222E40 hairline
pub const ACCENT: u32 = 0x00F8BD38; // #38BDF8 sky
pub const TXT: u32 = 0x00F3EAE7; // #E7EAF3 primary text
pub const MUTED: u32 = 0x00A8938A; // #8A93A8 secondary text
pub const DANGER: u32 = 0x007171F8; // #F87171

/// Alpha-blend `fg` over `bg`, a = 0..255. Layered windows have no
/// per-pixel alpha, so translucency is simulated over our known bg.
pub fn blend(fg: u32, bg: u32, a: u32) -> u32 {
    let ch = |s: u32| -> u32 {
        let f = (fg >> s) & 0xFF;
        let b = (bg >> s) & 0xFF;
        (f * a + b * (255 - a)) / 255
    };
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

pub fn cr(c: u32) -> COLORREF {
    COLORREF(c)
}

/// DPI-scale a logical pixel value.
pub fn sx(v: i32, dpi: u32) -> i32 {
    (v as i64 * dpi as i64 / 96) as i32
}

/// Fill a rect with a solid color (temporary brush).
pub unsafe fn fill(hdc: HDC, r: &RECT, c: u32) {
    let b = CreateSolidBrush(cr(c));
    FillRect(hdc, r, b);
    let _ = DeleteObject(b.into());
}

/// Rounded rect: fill + optional border (color, width).
pub unsafe fn round_box(hdc: HDC, r: &RECT, rad: i32, fill_c: u32, edge: Option<(u32, i32)>) {
    let br = CreateSolidBrush(cr(fill_c));
    let old_b = SelectObject(hdc, br.into());
    let (pen, old_p) = match edge {
        Some((c, w)) => {
            let p = CreatePen(PS_SOLID, w, cr(c));
            (p, Some(SelectObject(hdc, p.into())))
        }
        None => (Default::default(), Some(SelectObject(hdc, GetStockObject(NULL_PEN)))),
    };
    let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, rad * 2, rad * 2);
    if let Some(p) = old_p {
        SelectObject(hdc, p);
    }
    if edge.is_some() {
        let _ = DeleteObject(pen.into());
    }
    SelectObject(hdc, old_b);
    let _ = DeleteObject(br.into());
}

/// Hollow ring (handle dot): dark fill + colored ring.
pub unsafe fn ring_dot(hdc: HDC, x: i32, y: i32, rad: i32, ring: u32, fill_c: u32) {
    let br = CreateSolidBrush(cr(fill_c));
    let pen = CreatePen(PS_SOLID, 1, cr(ring));
    let old_b = SelectObject(hdc, br.into());
    let old_p = SelectObject(hdc, pen.into());
    let _ = Ellipse(hdc, x - rad, y - rad, x + rad + 1, y + rad + 1);
    SelectObject(hdc, old_p);
    SelectObject(hdc, old_b);
    let _ = DeleteObject(pen.into());
    let _ = DeleteObject(br.into());
}

/// Filled circle.
pub unsafe fn dot(hdc: HDC, x: i32, y: i32, rad: i32, c: u32) {
    let br = CreateSolidBrush(cr(c));
    let old_b = SelectObject(hdc, br.into());
    let old_p = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, x - rad, y - rad, x + rad + 1, y + rad + 1);
    SelectObject(hdc, old_p);
    SelectObject(hdc, old_b);
    let _ = DeleteObject(br.into());
}

/// Segoe UI at point size, scaled by dpi (points -> pixels: pt*dpi/72).
/// Caller deletes.
pub unsafe fn segoe(pt: i32, weight: i32, dpi: u32) -> HFONT {
    use windows::core::w;
    CreateFontW(
        -((pt * dpi as i32 + 36) / 72).max(1),
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        (VARIABLE_PITCH.0 | FF_SWISS.0) as u32,
        w!("Segoe UI"),
    )
}

/// DrawText with utf16 conversion.
pub unsafe fn text(hdc: HDC, r: &mut RECT, s: &str, flags: DRAW_TEXT_FORMAT) {
    let mut w: Vec<u16> = s.encode_utf16().collect();
    DrawTextW(hdc, &mut w, r, flags);
}

/// Width of a string in the current font.
pub unsafe fn text_w(hdc: HDC, s: &str) -> i32 {
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut sz = windows::Win32::Foundation::SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &w, &mut sz);
    sz.cx
}

/// RAII-ish DC state: returns the objects to restore (font, pen, brush).
pub struct GdiGuard {
    pub hdc: HDC,
    font: Option<HFONT>,
    old_font: HGDIOBJ,
    pens: Vec<HPEN>,
    brushes: Vec<HBRUSH>,
}

impl GdiGuard {
    pub unsafe fn new(hdc: HDC) -> GdiGuard {
        GdiGuard {
            hdc,
            font: None,
            old_font: Default::default(),
            pens: Vec::new(),
            brushes: Vec::new(),
        }
    }
    pub unsafe fn font(&mut self, f: HFONT) {
        self.old_font = SelectObject(self.hdc, f.into());
        self.font = Some(f);
    }
    pub unsafe fn pen(&mut self, p: HPEN) -> HGDIOBJ {
        self.pens.push(p);
        SelectObject(self.hdc, p.into())
    }
    pub unsafe fn brush(&mut self, b: HBRUSH) -> HGDIOBJ {
        self.brushes.push(b);
        SelectObject(self.hdc, b.into())
    }
}

impl Drop for GdiGuard {
    fn drop(&mut self) {
        unsafe {
            if self.font.is_some() {
                SelectObject(self.hdc, self.old_font);
            }
            if let Some(f) = self.font.take() {
                let _ = DeleteObject(f.into());
            }
            for p in self.pens.drain(..) {
                let _ = DeleteObject(p.into());
            }
            for b in self.brushes.drain(..) {
                let _ = DeleteObject(b.into());
            }
        }
    }
}

/// Paint a window through a memory DC (kills flicker). `w`,`h` = client
/// size; `f` receives the offscreen DC.
pub unsafe fn buffered_paint(hwnd: windows::Win32::Foundation::HWND, w: i32, h: i32, f: impl FnOnce(HDC)) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mem = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old = SelectObject(mem, bmp.into());
    f(mem);
    let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(bmp.into());
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}
