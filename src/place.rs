//! Apply geometry and style changes to a real HWND.
//!
//! Non-injection by design: we cannot veto the maximize before it paints,
//! so we correct right after the WinEvent lands. Every call keeps
//! SWP_NOACTIVATE/SWP_NOZORDER so we never steal focus.

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, MAPVK_VK_TO_VSC,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::rect::Rect;

const PLACE_FLAGS: SET_WINDOW_POS_FLAGS =
    SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0 | SWP_NOZORDER.0 | SWP_NOOWNERZORDER.0);

/// "Force small zones" (config `force_small_zones`, off by default): an
/// app that stays bigger than its zone is pushed down to the zone size
/// anyway. Set from the config by main / the settings page.
static FORCE_SMALL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_force_small(on: bool) {
    FORCE_SMALL.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Grow `rect` outward by the window's invisible DWM resize borders, so
/// the VISIBLE frame lands flush on the zone edges (what Aero Snap does
/// when it places windows slightly off-screen). Without this, snapping
/// to a screen edge leaves a ~7px dead strip.
fn zone_rect_for(hwnd: HWND, r: &Rect) -> Rect {
    unsafe {
        let mut vis = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut vis as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
        {
            return *r;
        }
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return *r;
        }
        Rect::new(
            r.left - (vis.left - wr.left),
            r.top - (vis.top - wr.top),
            r.right + (wr.right - vis.right),
            r.bottom + (wr.bottom - vis.bottom),
        )
    }
}

/// Visible frame (DWM extended frame bounds, falls back to the window
/// rect) and the work area of the display the window is on.
fn visible_and_work(hwnd: HWND) -> Option<(Rect, Rect)> {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let mut wr = RECT::default();
        GetWindowRect(hwnd, &mut wr).ok()?;
        let mut vis = wr;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut vis as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return None;
        }
        let w = mi.rcWork;
        Some((
            Rect::new(vis.left, vis.top, vis.right, vis.bottom),
            Rect::new(w.left, w.top, w.right, w.bottom),
        ))
    }
}

/// After a placement: an app whose minimum size is bigger than the zone
/// (Chrome ~500px, Firefox ~516px) grows right/down from the zone's
/// top-left corner, which pushes it off the display on the rightmost or
/// bottom zone. Slide it back inside the work area, keeping its size: it
/// then overlaps the neighbouring zone instead of leaving the screen.
fn keep_on_screen(hwnd: HWND, extra: SET_WINDOW_POS_FLAGS) {
    let Some((vis, work)) = visible_and_work(hwnd) else { return };
    let Some(to) = vis.shifted_into(&work) else { return };
    let (dx, dy) = (to.left - vis.left, to.top - vis.top);
    unsafe {
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return;
        }
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            wr.left + dx,
            wr.top + dy,
            0,
            0,
            SET_WINDOW_POS_FLAGS(PLACE_FLAGS.0 | SWP_NOSIZE.0 | extra.0),
        );
    }
}

/// Move a window that is already locked by us onto `rect` (same flags as
/// the fullscreen hold, so a fullscreen Chromium keeps the new spot).
pub fn move_exact(hwnd: HWND, rect: &Rect) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SET_WINDOW_POS_FLAGS(PLACE_FLAGS.0 | SWP_NOSENDCHANGING.0),
        )
        .is_ok()
    }
}

/// Hold an app that is in its OWN fullscreen mode at `rect` without
/// taking it out of fullscreen. SWP_NOSENDCHANGING skips
/// WM_WINDOWPOSCHANGING, where Chromium snaps a fullscreen window back to
/// the monitor bounds; the app then lays its fullscreen content out in
/// the zone. Real fullscreen surfaces have no frame, so the DWM
/// compensation is zero for them; it still matters for apps that cover
/// the monitor with their frame kept.
pub fn hold_fullscreen(hwnd: HWND, rect: &Rect) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        let rect = &zone_rect_for(hwnd, rect);
        SetWindowPos(
            hwnd,
            Some(HWND_TOP), // ignored due to SWP_NOZORDER
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SET_WINDOW_POS_FLAGS(PLACE_FLAGS.0 | SWP_NOSENDCHANGING.0),
        )
        .is_ok()
            && {
                keep_on_screen(hwnd, SWP_NOSENDCHANGING);
                true
            }
    }
}

/// Put `hwnd` exactly on `rect` (window coordinates, no compensation).
/// Returns false if the window vanished.
pub fn apply(hwnd: HWND, rect: &Rect) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return false;
        }
        // Never move a minimized window: re-placing it would restore it
        // and fight the user's explicit minimize.
        if IsIconic(hwnd).as_bool() {
            return false;
        }
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        SetWindowPos(
            hwnd,
            Some(HWND_TOP), // ignored due to SWP_NOZORDER
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            PLACE_FLAGS,
        )
        .is_ok()
    }
}

/// Like `apply`, but `rect` is a ZONE rect the visible window should
/// fill: invisible DWM borders are pushed outside so the frame sits
/// flush on the zone edges (and slightly off-screen at screen edges).
pub fn apply_zone(hwnd: HWND, rect: &Rect) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        // Measure the invisible borders in the RESTORED state: a
        // maximized window reports ~8px on all four sides, while the
        // restored frame has ~7px L/R/B and ~0 top - measuring before
        // the restore puts the visible top edge ~8px above the zone.
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
    let r = zone_rect_for(hwnd, rect);
    let ok = apply(hwnd, &r);
    if ok {
        fit_or_force(hwnd, rect);
        keep_on_screen(hwnd, SET_WINDOW_POS_FLAGS(0));
    }
    ok
}

/// After a zone placement: learn the app's minimum size when it stayed
/// bigger than the zone (`minsize`), and in "Force small zones" mode
/// squeeze it to the zone anyway. SWP_NOSENDCHANGING skips
/// WM_WINDOWPOSCHANGING, where Windows applies the app's
/// WM_GETMINMAXINFO limits. The app was not designed for that size: it
/// may cut its content, draw wrong or resize itself back.
fn fit_or_force(hwnd: HWND, zone: &Rect) {
    let Some((vis, _)) = visible_and_work(hwnd) else { return };
    let target = (zone.width().max(0) as u32, zone.height().max(0) as u32);
    let got = (vis.width().max(0) as u32, vis.height().max(0) as u32);
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }.max(96);
    let info = crate::wininfo::inspect(hwnd);
    let exe = info.exe;
    crate::minsize::observe(&exe, &crate::wininfo::process_path(info.pid), target, got, dpi);
    let over = got.0 > target.0 + 3 || got.1 > target.1 + 3;
    if !over || !FORCE_SMALL.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let r = zone_rect_for(hwnd, zone);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            r.left,
            r.top,
            r.width(),
            r.height(),
            SET_WINDOW_POS_FLAGS(PLACE_FLAGS.0 | SWP_NOSENDCHANGING.0),
        );
    }
    let after = visible_and_work(hwnd).map(|(v, _)| (v.width(), v.height()));
    crate::log::line(format!("forced {exe} below its minimum: {got:?} -> {after:?} (zone {target:?})"));
}

/// Strip WS_CAPTION | WS_THICKFRAME and place at `rect`, borderless.
/// Returns the styles the window had before stripping (needed to restore
/// the frame on release). None when the window vanished.
pub fn lock_borderless(hwnd: HWND, rect: &Rect) -> Option<(i64, i64)> {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as i64;
        let exstyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as i64;
        let stripped = style & !(WS_CAPTION.0 | WS_THICKFRAME.0) as i64;
        SetWindowLongPtrW(hwnd, GWL_STYLE, stripped as isize);
        SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SET_WINDOW_POS_FLAGS(
                PLACE_FLAGS.0 | SWP_FRAMECHANGED.0 | SWP_SHOWWINDOW.0,
            ),
        )
        .ok()?;
        keep_on_screen(hwnd, SET_WINDOW_POS_FLAGS(0));
        Some((style, exstyle))
    }
}

/// Put back styles saved by `lock_borderless` and place at `rect`.
pub fn release_borderless(hwnd: HWND, rect: &Rect, style: i64, exstyle: i64) -> bool {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        if style != 0 {
            SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
        }
        if exstyle != 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, exstyle as isize);
        }
        SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SET_WINDOW_POS_FLAGS(
                PLACE_FLAGS.0 | SWP_FRAMECHANGED.0 | SWP_SHOWWINDOW.0,
            ),
        )
        .is_ok()
    }
}

/// Post a virtual key press (down+up) to a window without stealing focus.
/// Used to make an app leave its own fullscreen mode (F11, Esc, 'f'...).
pub fn post_key(hwnd: HWND, vk: u32) {
    unsafe {
        let scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
        let lparam_down = LPARAM(1 | ((scan as isize) << 16));
        let lparam_up = LPARAM(1 | ((scan as isize) << 16) | (1 << 30) | (1 << 31));
        let _ = PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(vk as usize), lparam_down);
        let _ = PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(vk as usize), lparam_up);
    }
}
