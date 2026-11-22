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
pub const WARN: u32 = 0x0024BFFB; // #FBBF24 amber (too small, risky)

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

/// Anti-aliased drawing through GDI+ (part of Windows, nothing shipped):
/// plain GDI shapes have jagged edges that read as pixelated on the
/// dark UI. Started once per process; every helper falls back to GDI if
/// GDI+ is unavailable.
mod aa {
    use std::sync::OnceLock;
    use windows::Win32::Graphics::Gdi::HDC;
    use windows::Win32::Graphics::GdiPlus::*;

    fn ready() -> bool {
        static OK: OnceLock<bool> = OnceLock::new();
        *OK.get_or_init(|| unsafe {
            let mut token = 0usize;
            let input = GdiplusStartupInput {
                GdiplusVersion: 1,
                ..Default::default()
            };
            GdiplusStartup(&mut token, &input, std::ptr::null_mut()) == Ok
        })
    }

    /// 0x00BBGGRR -> opaque 0xAARRGGBB.
    fn argb(c: u32) -> u32 {
        0xFF00_0000 | ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
    }

    /// A smoothing GDI+ surface over `hdc`; coordinates follow GDI (pixel
    /// i covers [i, i+1]).
    pub struct G(*mut GpGraphics);

    impl G {
        pub unsafe fn new(hdc: HDC) -> Option<G> {
            if !ready() {
                return None;
            }
            let mut g = std::ptr::null_mut();
            if GdipCreateFromHDC(hdc, &mut g) != Ok || g.is_null() {
                return None;
            }
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
            Some(G(g))
        }

        unsafe fn brush(c: u32) -> *mut GpBrush {
            let mut b = std::ptr::null_mut();
            GdipCreateSolidFill(argb(c), &mut b);
            b as *mut GpBrush
        }

        unsafe fn pen(c: u32, w: f32) -> *mut GpPen {
            let mut p = std::ptr::null_mut();
            GdipCreatePen1(argb(c), w, UnitPixel, &mut p);
            GdipSetPenStartCap(p, LineCapRound);
            GdipSetPenEndCap(p, LineCapRound);
            p
        }

        unsafe fn round_path(l: f32, t: f32, r: f32, b: f32, rad: f32) -> *mut GpPath {
            let mut path = std::ptr::null_mut();
            GdipCreatePath(FillModeAlternate, &mut path);
            let d = (rad * 2.0).min(r - l).min(b - t).max(0.0);
            if d < 1.0 {
                GdipAddPathArc(path, l, t, 0.0, 0.0, 180.0, 90.0);
                GdipAddPathArc(path, r, t, 0.0, 0.0, 270.0, 90.0);
                GdipAddPathArc(path, r, b, 0.0, 0.0, 0.0, 90.0);
                GdipAddPathArc(path, l, b, 0.0, 0.0, 90.0, 90.0);
            } else {
                GdipAddPathArc(path, l, t, d, d, 180.0, 90.0);
                GdipAddPathArc(path, r - d, t, d, d, 270.0, 90.0);
                GdipAddPathArc(path, r - d, b - d, d, d, 0.0, 90.0);
                GdipAddPathArc(path, l, b - d, d, d, 90.0, 90.0);
            }
            GdipClosePathFigure(path);
            path
        }

        pub unsafe fn round_box(&self, r: (f32, f32, f32, f32), rad: f32, fill: u32, edge: Option<(u32, f32)>) {
            let (l, t, rr, b) = r;
            let path = Self::round_path(l, t, rr, b, rad);
            let br = Self::brush(fill);
            GdipFillPath(self.0, br, path);
            GdipDeleteBrush(br);
            GdipDeletePath(path);
            if let Some((c, w)) = edge {
                let h = w / 2.0;
                let path = Self::round_path(l + h, t + h, rr - h, b - h, (rad - h).max(0.0));
                let p = Self::pen(c, w);
                GdipDrawPath(self.0, p, path);
                GdipDeletePen(p);
                GdipDeletePath(path);
            }
        }

        /// Dashed outline of a rounded rect (`w` px stroke).
        pub unsafe fn dashed_round(&self, r: (f32, f32, f32, f32), rad: f32, c: u32, w: f32) {
            let h = w / 2.0;
            let path = Self::round_path(r.0 + h, r.1 + h, r.2 - h, r.3 - h, (rad - h).max(0.0));
            let p = Self::pen(c, w);
            GdipSetPenStartCap(p, LineCapFlat);
            GdipSetPenEndCap(p, LineCapFlat);
            GdipSetPenDashStyle(p, DashStyleDash);
            GdipDrawPath(self.0, p, path);
            GdipDeletePen(p);
            GdipDeletePath(path);
        }

        pub unsafe fn circle(&self, cx: f32, cy: f32, rad: f32, fill: u32, ring: Option<(u32, f32)>) {
            let br = Self::brush(fill);
            GdipFillEllipse(self.0, br, cx - rad, cy - rad, rad * 2.0, rad * 2.0);
            GdipDeleteBrush(br);
            if let Some((c, w)) = ring {
                let rr = rad - w / 2.0;
                let p = Self::pen(c, w);
                GdipDrawEllipse(self.0, p, cx - rr, cy - rr, rr * 2.0, rr * 2.0);
                GdipDeletePen(p);
            }
        }

        pub unsafe fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, c: u32) {
            let p = Self::pen(c, w);
            GdipDrawLine(self.0, p, x1, y1, x2, y2);
            GdipDeletePen(p);
        }
    }

    impl Drop for G {
        fn drop(&mut self) {
            unsafe {
                GdipDeleteGraphics(self.0);
            }
        }
    }
}

/// Rounded rect: fill + optional border (color, width). Anti-aliased.
pub unsafe fn round_box(hdc: HDC, r: &RECT, rad: i32, fill_c: u32, edge: Option<(u32, i32)>) {
    if let Some(g) = aa::G::new(hdc) {
        g.round_box(
            (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32),
            rad as f32,
            fill_c,
            edge.map(|(c, w)| (c, w as f32)),
        );
        return;
    }
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

/// Dashed rounded outline (warnings). Anti-aliased; plain dotted GDI
/// rectangle without GDI+.
pub unsafe fn dashed_box(hdc: HDC, r: &RECT, rad: i32, c: u32, w: i32) {
    if let Some(g) = aa::G::new(hdc) {
        g.dashed_round((r.left as f32, r.top as f32, r.right as f32, r.bottom as f32), rad as f32, c, w as f32);
        return;
    }
    let pen = CreatePen(PS_DOT, 1, cr(c));
    let old_p = SelectObject(hdc, pen.into());
    let old_b = SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
    let _ = Rectangle(hdc, r.left, r.top, r.right, r.bottom);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(pen.into());
}

/// "X" centred on (x, y), `arm` px from the centre on each diagonal,
/// `w` px thick, drawn over a `halo` colour so it reads on any icon.
pub unsafe fn x_mark(hdc: HDC, x: i32, y: i32, arm: i32, w: f32, c: u32, halo: u32) {
    let (cx, cy, a) = (x as f32 + 0.5, y as f32 + 0.5, arm as f32);
    if let Some(g) = aa::G::new(hdc) {
        for (col, ww) in [(halo, w + 3.0), (c, w)] {
            g.line(cx - a, cy - a, cx + a, cy + a, ww, col);
            g.line(cx - a, cy + a, cx + a, cy - a, ww, col);
        }
        return;
    }
    let pen = CreatePen(PS_SOLID, w.round() as i32, cr(c));
    let old = SelectObject(hdc, pen.into());
    let _ = MoveToEx(hdc, x - arm, y - arm, None);
    let _ = LineTo(hdc, x + arm, y + arm);
    let _ = MoveToEx(hdc, x - arm, y + arm, None);
    let _ = LineTo(hdc, x + arm, y - arm);
    SelectObject(hdc, old);
    let _ = DeleteObject(pen.into());
}

/// Ring width that stays visible on high-dpi dots.
fn ring_w(rad: i32) -> f32 {
    (rad as f32 / 5.0).clamp(1.0, 2.0)
}

/// Hollow ring (handle dot): dark fill + colored ring. Anti-aliased.
pub unsafe fn ring_dot(hdc: HDC, x: i32, y: i32, rad: i32, ring: u32, fill_c: u32) {
    if let Some(g) = aa::G::new(hdc) {
        g.circle(x as f32 + 0.5, y as f32 + 0.5, rad as f32 + 0.5, fill_c, Some((ring, ring_w(rad))));
        return;
    }
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

/// Filled circle. Anti-aliased.
pub unsafe fn dot(hdc: HDC, x: i32, y: i32, rad: i32, c: u32) {
    if let Some(g) = aa::G::new(hdc) {
        g.circle(x as f32 + 0.5, y as f32 + 0.5, rad as f32 + 0.5, c, None);
        return;
    }
    let br = CreateSolidBrush(cr(c));
    let old_b = SelectObject(hdc, br.into());
    let old_p = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, x - rad, y - rad, x + rad + 1, y + rad + 1);
    SelectObject(hdc, old_p);
    SelectObject(hdc, old_b);
    let _ = DeleteObject(br.into());
}

/// "+" mark centred on (x, y): two round-capped strokes, `arm` px from
/// the centre, `w` px thick. Anti-aliased.
pub unsafe fn plus_mark(hdc: HDC, x: i32, y: i32, arm: i32, w: f32, c: u32) {
    let (cx, cy, a) = (x as f32 + 0.5, y as f32 + 0.5, arm as f32);
    if let Some(g) = aa::G::new(hdc) {
        g.line(cx - a, cy, cx + a, cy, w, c);
        g.line(cx, cy - a, cx, cy + a, w, c);
        return;
    }
    let pen = CreatePen(PS_SOLID, w.round() as i32, cr(c));
    let old = SelectObject(hdc, pen.into());
    let _ = MoveToEx(hdc, x - arm, y, None);
    let _ = LineTo(hdc, x + arm + 1, y);
    let _ = MoveToEx(hdc, x, y - arm, None);
    let _ = LineTo(hdc, x, y + arm + 1);
    SelectObject(hdc, old);
    let _ = DeleteObject(pen.into());
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
        ANTIALIASED_QUALITY, // grayscale: ClearType fringes on layered windows
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
