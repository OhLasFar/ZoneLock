//! Zone editor overlay. One topmost translucent window per monitor; draw
//! zones with the mouse, drag handle dots to move/resize them, right-
//! click deletes, number keys apply presets, Enter/Esc commits to the
//! shared config. Painting is double-buffered and edges snap
//! magnetically to monitor fractions and other zones' edges.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::monitor::Monitor;
use crate::rect::{FracRect, Rect};
use crate::uidraw as ui;
use crate::watcher::Shared;
use crate::zones::{Layout, Zone};

const CLASS: PCWSTR = w!("ZoneLockOverlay");
const ALPHA: u8 = 235; // editor is nearly opaque so zones read clearly

const SNAP_PX: i32 = 14;
const MIN_ZONE_PX: i32 = 48;
const HANDLE_PX: i32 = 11;
const DOT_R: i32 = 5;

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NW,
    NE,
    SW,
    SE,
}

#[derive(Clone, Copy)]
struct EditDrag {
    zone: usize,
    handle: Handle,
    start: POINT,
    orig: Rect, // client-px rect at drag start
}

struct OvWin {
    mon: Monitor,
    layout: Layout,
    drag_start: Option<POINT>,
    drag_cur: Option<POINT>,
    edit: Option<EditDrag>,
    /// Positions of snapped edges to highlight, (vertical?, client px).
    guides: Vec<(bool, i32)>,
    hover: Option<(usize, Handle)>,
    dpi: u32,
}

thread_local! {
    static WINDOWS: RefCell<HashMap<isize, OvWin>> = RefCell::new(HashMap::new());
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// (monitor, preset name) when the editor was opened by New / Edit
    /// preset in the settings panel.
    static PENDING: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

fn pending_for(mon: &str) -> Option<String> {
    PENDING.with(|p| {
        p.borrow()
            .as_ref()
            .filter(|(m, _)| m == mon)
            .map(|(_, n)| n.clone())
    })
}

/// Open the editor to draw preset `name` on display `monitor`; Enter
/// stores the drawing as that user preset and applies it there.
pub fn open_for_preset(shared: Arc<Shared>, monitor: String, name: String) {
    if is_open() {
        close_all(false);
    }
    PENDING.with(|p| *p.borrow_mut() = Some((monitor, name)));
    toggle(shared);
}

/// Snap `v` to the nearest target within `tol`; returns (value, target).
fn snap_to(v: i32, targets: &[i32], tol: i32) -> (i32, Option<i32>) {
    targets
        .iter()
        .filter(|t| (v - **t).abs() <= tol)
        .min_by_key(|t| (v - **t).abs())
        .map(|t| (*t, Some(*t)))
        .unwrap_or((v, None))
}

/// Snap targets along one axis: monitor edges, fractions (halves,
/// thirds, quarters, sixths) and every other zone's edges. Client px.
fn axis_targets(ov: &OvWin, vertical: bool, skip: Option<usize>) -> Vec<i32> {
    // zone canvas = the work area (excludes the taskbar) in client coords
    let (a, b) = if vertical {
        (
            ov.mon.work.top - ov.mon.rect.top,
            ov.mon.work.bottom - ov.mon.rect.top,
        )
    } else {
        (
            ov.mon.work.left - ov.mon.rect.left,
            ov.mon.work.right - ov.mon.rect.left,
        )
    };
    let total = b - a;
    let mut v = vec![a, b];
    for d in [2i64, 3, 4, 6] {
        for n in 1..d {
            v.push(a + (total as i64 * n / d) as i32);
        }
    }
    for (i, z) in ov.layout.zones.iter().enumerate() {
        if Some(i) == skip {
            continue;
        }
        let r = zone_client_rect(z, &ov.mon.work, &ov.mon.rect);
        if vertical {
            v.push(r.top);
            v.push(r.bottom);
        } else {
            v.push(r.left);
            v.push(r.right);
        }
    }
    v
}

fn win_rect_from_drag(a: POINT, b: POINT) -> Rect {
    Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
}

/// `x`,`y` are overlay client coords; zones map onto the work area
/// (`work`) and the overlay window sits at `origin` (monitor rect).
fn point_in_zone(layout: &Layout, work: &Rect, origin: &Rect, x: i32, y: i32) -> Option<usize> {
    layout
        .zones
        .iter()
        .position(|z| z.rect.to_pixels(work, 0).contains_point(x + origin.left, y + origin.top))
}

/// Zone rect in overlay client coordinates: fractions are resolved on
/// the work area, then translated by the overlay's monitor origin.
fn zone_client_rect(z: &Zone, work: &Rect, origin: &Rect) -> Rect {
    let p = z.rect.to_pixels(work, 0);
    Rect::new(
        p.left - origin.left,
        p.top - origin.top,
        p.right - origin.left,
        p.bottom - origin.top,
    )
}

/// Handle anchor points for a zone rect, in client coordinates.
/// Index 0 is the center (Move); the rest are edges and corners.
fn handle_points(r: &Rect) -> [(Handle, i32, i32); 9] {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    [
        (Handle::Move, cx, cy),
        (Handle::NW, r.left, r.top),
        (Handle::N, cx, r.top),
        (Handle::NE, r.right, r.top),
        (Handle::E, r.right, cy),
        (Handle::W, r.left, cy),
        (Handle::SW, r.left, r.bottom),
        (Handle::S, cx, r.bottom),
        (Handle::SE, r.right, r.bottom),
    ]
}

/// Hit-test resize/move handles across zones (topmost zone wins).
fn handle_at(layout: &Layout, work: &Rect, origin: &Rect, x: i32, y: i32) -> Option<(usize, Handle)> {
    for (i, z) in layout.zones.iter().enumerate().rev() {
        let r = zone_client_rect(z, work, origin);
        for (h, hx, hy) in handle_points(&r).into_iter().skip(1) {
            if (x - hx).abs() <= HANDLE_PX && (y - hy).abs() <= HANDLE_PX {
                return Some((i, h));
            }
        }
        let (_, cx, cy) = handle_points(&r)[0];
        if (x - cx).abs() <= HANDLE_PX * 2 && (y - cy).abs() <= HANDLE_PX * 2 {
            return Some((i, Handle::Move));
        }
    }
    None
}

/// Apply an edit-drag to the zone's client rect with magnetic snapping.
/// Returns the new rect and the guide positions actually snapped to.
/// `Move` snaps the whole rect rigidly (same size), edge/corner handles
/// snap the moved edges.
fn edited_rect(ed: &EditDrag, x: i32, y: i32, ov: &OvWin) -> (Rect, Vec<(bool, i32)>) {
    let tol = ui::sx(SNAP_PX, ov.dpi);
    let xt = axis_targets(ov, false, Some(ed.zone));
    let yt = axis_targets(ov, true, Some(ed.zone));
    let dx = x - ed.start.x;
    let dy = y - ed.start.y;
    let o = ed.orig;
    // keep zones inside the work area (client coords)
    let (mw_l, mw_r) = (
        ov.mon.work.left - ov.mon.rect.left,
        ov.mon.work.right - ov.mon.rect.left,
    );
    let (mh_t, mh_b) = (
        ov.mon.work.top - ov.mon.rect.top,
        ov.mon.work.bottom - ov.mon.rect.top,
    );
    let mut guides = Vec::new();
    let (mut l, mut t, mut r, mut b) = (o.left, o.top, o.right, o.bottom);

    match ed.handle {
        Handle::Move => {
            // keep the size: pick the snapped translation closest to the
            // raw delta, trying leading and trailing edges on each axis
            let snap_axis = |lo: i32, hi: i32, d: i32, targets: &[i32]| {
                let (lo_s, lo_t) = snap_to(lo + d, targets, tol);
                let (hi_s, hi_t) = snap_to(hi + d, targets, tol);
                match (lo_t, hi_t) {
                    (Some(_), None) => lo_s - lo,
                    (None, Some(_)) => hi_s - hi,
                    (Some(lt), Some(ht)) => {
                        if (lt - (lo + d)).abs() <= (ht - (hi + d)).abs() {
                            lo_s - lo
                        } else {
                            hi_s - hi
                        }
                    }
                    _ => d,
                }
            };
            let sdx = snap_axis(o.left, o.right, dx, &xt);
            let sdy = snap_axis(o.top, o.bottom, dy, &yt);
            if sdx != dx {
                guides.push((true, o.left + sdx));
            }
            if sdy != dy {
                guides.push((false, o.top + sdy));
            }
            l += sdx;
            r += sdx;
            t += sdy;
            b += sdy;
            l = l.clamp(mw_l, mw_r - (r - l));
            r = l + (o.right - o.left);
            t = t.clamp(mh_t, mh_b - (b - t));
            b = t + (o.bottom - o.top);
        }
        h => {
            let snap_edge = |v: i32, targets: &[i32], g: &mut Vec<(bool, i32)>, vert: bool| {
                let (s, t) = snap_to(v, targets, tol);
                if let Some(tp) = t {
                    g.push((vert, tp));
                }
                s
            };
            let moves_l = matches!(h, Handle::W | Handle::NW | Handle::SW);
            let moves_r = matches!(h, Handle::E | Handle::NE | Handle::SE);
            let moves_t = matches!(h, Handle::N | Handle::NW | Handle::NE);
            let moves_b = matches!(h, Handle::S | Handle::SW | Handle::SE);
            if moves_l {
                l = snap_edge(o.left + dx, &xt, &mut guides, true).clamp(mw_l, mw_r - MIN_ZONE_PX);
            }
            if moves_r {
                r = snap_edge(o.right + dx, &xt, &mut guides, true).clamp(mw_l + MIN_ZONE_PX, mw_r);
            }
            if moves_t {
                t = snap_edge(o.top + dy, &yt, &mut guides, false).clamp(mh_t, mh_b - MIN_ZONE_PX);
            }
            if moves_b {
                b = snap_edge(o.bottom + dy, &yt, &mut guides, false).clamp(mh_t + MIN_ZONE_PX, mh_b);
            }
            if r - l < MIN_ZONE_PX {
                if moves_l {
                    l = r - MIN_ZONE_PX;
                } else {
                    r = l + MIN_ZONE_PX;
                }
            }
            if b - t < MIN_ZONE_PX {
                if moves_t {
                    t = b - MIN_ZONE_PX;
                } else {
                    b = t + MIN_ZONE_PX;
                }
            }
        }
    }
    (Rect::new(l, t, r, b), guides)
}

/// Cursor for a handle, used on hover so users discover the dots.
fn handle_cursor(h: Handle) -> PCWSTR {
    match h {
        Handle::Move => IDC_SIZEALL,
        Handle::N | Handle::S => IDC_SIZENS,
        Handle::E | Handle::W => IDC_SIZEWE,
        Handle::NW | Handle::SE => IDC_SIZENWSE,
        Handle::NE | Handle::SW => IDC_SIZENESW,
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let (mw, mh) = WINDOWS
                .with(|w| {
                    w.borrow()
                        .get(&(hwnd.0 as isize))
                        .map(|ov| (ov.mon.rect.width(), ov.mon.rect.height()))
                })
                .unwrap_or((0, 0));
            ui::buffered_paint(hwnd, mw, mh, |hdc| paint(hwnd, hdc));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR => {
            // keep our contextual cursor (SetCursor in mousemove); the
            // class cursor is the crosshair for empty space
            let hit = WINDOWS.with(|w| {
                w.borrow().get(&(hwnd.0 as isize)).map(|ov| {
                    ov.hover.map(|(_, h)| h)
                })
            });
            if let Some(Some(h)) = hit {
                unsafe {
                    let _ = SetCursor(Some(LoadCursorW(None, handle_cursor(h)).unwrap_or_default()));
                }
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    if let Some((zi, h)) = handle_at(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y) {
                        let r = zone_client_rect(&ov.layout.zones[zi], &ov.mon.work, &ov.mon.rect);
                        ov.edit = Some(EditDrag {
                            zone: zi,
                            handle: h,
                            start: POINT { x, y },
                            orig: r,
                        });
                        let _ = SetCapture(hwnd);
                    } else if point_in_zone(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y).is_none() {
                        // start drag only on empty space (not inside a zone)
                        ov.drag_start = Some(POINT { x, y });
                        ov.drag_cur = Some(POINT { x, y });
                        let _ = SetCapture(hwnd);
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            let mut dirty = false;
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    if let Some(ed) = ov.edit {
                        let (r, guides) = edited_rect(&ed, x, y, ov);
                        let scr = Rect::new(
                            r.left + ov.mon.rect.left,
                            r.top + ov.mon.rect.top,
                            r.right + ov.mon.rect.left,
                            r.bottom + ov.mon.rect.top,
                        );
                        ov.layout.zones[ed.zone].rect =
                            FracRect::from_pixels(&scr, &ov.mon.work);
                        ov.guides = guides;
                        dirty = true;
                    } else if ov.drag_start.is_some() {
                        ov.drag_cur = Some(POINT { x, y });
                        dirty = true;
                    } else {
                        let h = handle_at(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y);
                        if h != ov.hover {
                            ov.hover = h;
                            dirty = true;
                        }
                    }
                }
            });
            if dirty {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    if ov.edit.take().is_some() {
                        ov.guides.clear();
                        let _ = ReleaseCapture();
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if let Some(a) = ov.drag_start.take() {
                        ov.drag_cur = None;
                        let _ = ReleaseCapture();
                        let tol = ui::sx(SNAP_PX, ov.dpi);
                        let xt = axis_targets(ov, false, None);
                        let yt = axis_targets(ov, true, None);
                        let (sx, _) = snap_to(x, &xt, tol);
                        let (sy, _) = snap_to(y, &yt, tol);
                        let mut r = win_rect_from_drag(a, POINT { x: sx, y: sy });
                        r = Rect::new(
                            snap_to(r.left, &xt, tol).0,
                            snap_to(r.top, &yt, tol).0,
                            snap_to(r.right, &xt, tol).0,
                            snap_to(r.bottom, &yt, tol).0,
                        );
                        if r.width() >= MIN_ZONE_PX && r.height() >= MIN_ZONE_PX {
                            let frac = FracRect::from_pixels(&r, &ov.mon.work);
                            if frac.right > frac.left && frac.bottom > frac.top {
                                ov.layout.zones.push(Zone {
                                    name: format!("zone {}", ov.layout.zones.len() + 1),
                                    rect: frac,
                                });
                            }
                        }
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    if let Some(i) = point_in_zone(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y) {
                        ov.layout.zones.remove(i);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u32;
            match vk {
                0x1B | 0x0D => {
                    // Esc / Enter: commit and close
                    if vk == 0x0D {
                        commit(hwnd);
                    }
                    close_all(vk == 0x0D);
                }
                // '1'..'7' -> preset on this monitor
                k @ (0x31..=0x37) => {
                    let idx = (k - 0x31) as usize;
                    WINDOWS.with(|w| {
                        if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                            if let Some(p) = crate::presets::presets().get(idx) {
                                ov.layout = p.clone();
                                let _ = InvalidateRect(Some(hwnd), None, false);
                            }
                        }
                    });
                }
                0x2E => {
                    WINDOWS.with(|w| {
                        if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                            ov.layout.zones.pop();
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOWS.with(|w| {
                w.borrow_mut().remove(&(hwnd.0 as isize));
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint(hwnd: HWND, hdc: HDC) {
    WINDOWS.with(|w| {
        let map = w.borrow();
        let Some(ov) = map.get(&(hwnd.0 as isize)) else {
            return;
        };
        let mon = ov.mon.rect;
        let dpi = ov.dpi;
        let rc = RECT {
            left: 0,
            top: 0,
            right: mon.width(),
            bottom: mon.height(),
        };
        ui::fill(hdc, &rc, ui::BG);

        SetBkMode(hdc, TRANSPARENT);
        let font = ui::segoe(11, 600, dpi);
        let mut g = ui::GdiGuard::new(hdc);
        g.font(font);
        SetTextColor(hdc, ui::cr(ui::TXT));

        // faint fraction grid so the magnet targets read on screen
        let grid = CreatePen(PS_SOLID, 1, ui::cr(ui::blend(ui::EDGE, ui::BG, 160)));
        let old_p = SelectObject(hdc, grid.into());
        for d in [2i64, 3, 4] {
            for n in 1..d {
                let gx = (mon.width() as i64 * n / d) as i32;
                let gy = (mon.height() as i64 * n / d) as i32;
                let _ = MoveToEx(hdc, gx, 0, None);
                let _ = LineTo(hdc, gx, mon.height());
                let _ = MoveToEx(hdc, 0, gy, None);
                let _ = LineTo(hdc, mon.width(), gy);
            }
        }
        SelectObject(hdc, old_p);
        let _ = DeleteObject(grid.into());

        // zones
        for (i, z) in ov.layout.zones.iter().enumerate() {
            let r = zone_client_rect(z, &ov.mon.work, &mon);
            let zr = RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            };
            let active = ov.hover.map(|(zi, _)| zi) == Some(i)
                || ov.edit.map(|e| e.zone) == Some(i);
            ui::round_box(
                hdc,
                &zr,
                ui::sx(10, dpi),
                ui::blend(ui::ACCENT, ui::BG, if active { 46 } else { 30 }),
                Some((ui::blend(ui::ACCENT, ui::BG, if active { 200 } else { 110 }), 1)),
            );

            // badge + name, top-left inside the zone
            let bx = r.left + ui::sx(16, dpi);
            let by = r.top + ui::sx(16, dpi);
            let rad = ui::sx(11, dpi);
            ui::dot(hdc, bx, by, rad, ui::blend(ui::ACCENT, ui::BG, 230));
            SetTextColor(hdc, ui::cr(ui::BG));
            let mut br = RECT {
                left: bx - rad,
                top: by - rad,
                right: bx + rad,
                bottom: by + rad,
            };
            ui::text(hdc, &mut br, &format!("{}", i + 1), DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SetTextColor(hdc, ui::cr(ui::MUTED));
            let mut nr = RECT {
                left: bx + rad + ui::sx(8, dpi),
                top: by - ui::sx(9, dpi),
                right: r.right - ui::sx(8, dpi),
                bottom: by + ui::sx(9, dpi),
            };
            ui::text(hdc, &mut nr, &z.name, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            SetTextColor(hdc, ui::cr(ui::TXT));

            // handle dots
            let hr2 = Rect::new(zr.left, zr.top, zr.right, zr.bottom);
            for (h, hx, hy) in handle_points(&hr2) {
                let hovered = ov.hover == Some((i, h));
                let rad = ui::sx(if h == Handle::Move { DOT_R + 1 } else { DOT_R }, dpi);
                let (fill_c, ring_c) = if hovered {
                    (ui::ACCENT, ui::ACCENT)
                } else if h == Handle::Move {
                    (ui::blend(ui::ACCENT, ui::BG, 140), ui::ACCENT)
                } else {
                    (ui::CARD, ui::blend(ui::ACCENT, ui::BG, 200))
                };
                ui::ring_dot(hdc, hx, hy, rad, ring_c, fill_c);
            }
        }

        // magnetic guides while an edge is snapped
        for (vert, pos) in &ov.guides {
            let gp = CreatePen(PS_SOLID, ui::sx(2, dpi), ui::cr(ui::ACCENT));
            let old = SelectObject(hdc, gp.into());
            if *vert {
                let _ = MoveToEx(hdc, *pos, 0, None);
                let _ = LineTo(hdc, *pos, mon.height());
            } else {
                let _ = MoveToEx(hdc, 0, *pos, None);
                let _ = LineTo(hdc, mon.width(), *pos);
            }
            SelectObject(hdc, old);
            let _ = DeleteObject(gp.into());
        }

        // rubber band for a new zone
        if let (Some(a), Some(b)) = (ov.drag_start, ov.drag_cur) {
            let tol = ui::sx(SNAP_PX, dpi);
            let xt = axis_targets(ov, false, None);
            let yt = axis_targets(ov, true, None);
            let sb = POINT {
                x: snap_to(b.x, &xt, tol).0,
                y: snap_to(b.y, &yt, tol).0,
            };
            let r = win_rect_from_drag(a, sb);
            let mut br = RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            };
            let bp = CreatePen(PS_DOT, 1, ui::cr(ui::ACCENT));
            let old_p2 = SelectObject(hdc, bp.into());
            let old_b2 = SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
            let _ = Rectangle(hdc, br.left, br.top, br.right, br.bottom);
            SelectObject(hdc, old_b2);
            SelectObject(hdc, old_p2);
            let _ = DeleteObject(bp.into());
            let pct = FracRect::from_pixels(&r, &mon);
            let info = format!(
                "{:.0}% x {:.0}%",
                (pct.right - pct.left) * 100.0,
                (pct.bottom - pct.top) * 100.0
            );
            SetTextColor(hdc, ui::cr(ui::ACCENT));
            ui::text(hdc, &mut br, &info, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SetTextColor(hdc, ui::cr(ui::TXT));
        }

        // instruction pill, bottom center
        let base = "drag: new zone   dots: move / resize   right-click: delete   1-7: preset   Enter: save   Esc: cancel";
        let hint_owned = match pending_for(&ov.mon.name) {
            Some(n) => format!("New preset \u{201c}{n}\u{201d}   \u{00b7}   {base}"),
            None => base.to_string(),
        };
        let hint = hint_owned.as_str();
        let hint_font = ui::segoe(10, 400, dpi);
        SelectObject(hdc, hint_font.into());
        let tw = ui::text_w(hdc, hint);
        let th = ui::sx(30, dpi);
        let hx = (mon.width() - tw) / 2;
        let hy = mon.height() - th - ui::sx(22, dpi);
        let mut hr = RECT {
            left: hx - ui::sx(16, dpi),
            top: hy,
            right: hx + tw + ui::sx(16, dpi),
            bottom: hy + th,
        };
        ui::round_box(hdc, &hr, ui::sx(15, dpi), ui::CARD, Some((ui::EDGE, 1)));
        SetTextColor(hdc, ui::cr(ui::MUTED));
        ui::text(hdc, &mut hr, hint, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        let _ = DeleteObject(hint_font.into());
    });
}

fn commit(hwnd: HWND) {
    WINDOWS.with(|w| {
        SHARED.with(|s| {
            if let Some(ov) = w.borrow().get(&(hwnd.0 as isize)) {
                if let Some(shared) = s.borrow().as_ref() {
                    let mut cfg = shared.config.write().unwrap();
                    let preset = pending_for(&ov.mon.name);
                    let next = crate::uimodel::commit_editor_layout(
                        &cfg,
                        &ov.mon.name,
                        ov.layout.clone(),
                        preset.as_deref(),
                    );
                    *cfg = next;
                    let _ = cfg.save();
                    shared.bump();
                }
            }
        });
    });
}

/// Save the layouts of every open overlay then destroy them.
/// `save` == false cancels without writing.
fn close_all(save: bool) {
    let hwnds: Vec<isize> = WINDOWS.with(|w| w.borrow().keys().copied().collect());
    for h in hwnds {
        if save {
            commit(HWND(h as *mut _));
        }
        unsafe {
            let _ = DestroyWindow(HWND(h as *mut _));
        }
    }
    PENDING.with(|p| p.borrow_mut().take());
    // the settings panel (if open) shows the new layouts / presets
    crate::webui::refresh();
}

pub fn is_open() -> bool {
    WINDOWS.with(|w| !w.borrow().is_empty())
}

/// Toggle the overlay on/off. When turning off, edits are committed.
pub fn toggle(shared: Arc<Shared>) {
    if is_open() {
        close_all(true);
        return;
    }
    SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));
    let mons = shared.monitors.read().unwrap().clone();
    let layouts = shared.config.read().unwrap().layouts.clone();

    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        for mon in mons {
            let layout = layouts
                .for_monitor(&mon.name, mon.index)
                .cloned()
                .unwrap_or_else(Layout::halves);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
                CLASS,
                w!("ZoneLock zones"),
                WS_POPUP | WS_VISIBLE,
                mon.rect.left,
                mon.rect.top,
                mon.rect.width(),
                mon.rect.height(),
                None,
                None,
                Some(hinst.into()),
                None,
            );
            let Ok(hwnd) = hwnd else { continue };
            let _ = SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0),
                ALPHA,
                LWA_ALPHA,
            );
            WINDOWS.with(|w| {
                w.borrow_mut().insert(
                    hwnd.0 as isize,
                    OvWin {
                        mon: mon.clone(),
                        layout,
                        drag_start: None,
                        drag_cur: None,
                        edit: None,
                        guides: Vec::new(),
                        hover: None,
                        dpi: mon.dpi_x.max(96),
                    },
                );
            });
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
    }
}
