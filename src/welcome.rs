//! First-run intro card: shown once when ZoneLock starts without a
//! config (a brand-new install), and again from the settings panel
//! ("Intro card" row). A centred card listing the core gestures with
//! the current bindings: native GDI, topmost, never activated. Two
//! buttons - "Open settings" jumps to the panel, "Got it" dismisses;
//! Esc works too. UI thread.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cheatsheet::{split_keys, Line};
use crate::config::HotkeyConfig;
use crate::uidraw as ui;

const CLASS: PCWSTR = w!("ZoneLockWelcome");
const ESC_HOTKEY: i32 = 1;
// logical (96 dpi) metrics
const W: i32 = 600;
const PAD: i32 = 28;
const TITLE_H: i32 = 76;
const ROW_H: i32 = 38;
const HINT_H: i32 = 34;
const FOOT_H: i32 = 68;
const BTN_H: i32 = 34;
const BTN_GAP: i32 = 10;
const BTN_GOTIT: i32 = 1;
const BTN_SETTINGS: i32 = 2;

thread_local! {
    static WIN: Cell<isize> = const { Cell::new(0) };
    static DATA: RefCell<Vec<Line>> = const { RefCell::new(Vec::new()) };
    /// Button under the cursor (0 = none, BTN_* ids), for hover paint.
    static HOVER: Cell<i32> = const { Cell::new(0) };
}

fn hwnd() -> Option<HWND> {
    let v = WIN.with(|w| w.get());
    (v != 0).then_some(HWND(v as *mut _))
}

/// The four things a new user needs, with their current bindings. Pure.
pub fn lines(hk: &HotkeyConfig) -> Vec<Line> {
    let keys = |b: &str| split_keys(&crate::hotkeys::binding_label(b));
    let mut drag = keys(&hk.drag_snap);
    drag.push("drag".into());
    let line = |a: &str, k: Vec<String>| Line { action: a.into(), keys: k };
    vec![
        line("Snap a window into a zone", drag),
        line("Window to the next zone", keys(&hk.next_zone)),
        line(
            "Fullscreen inside its zone, or whole screen",
            keys(&hk.fs_toggle),
        ),
        line("Every shortcut", keys(&hk.cheatsheet)),
    ]
}

fn height_logical() -> i32 {
    DATA.with(|d| {
        TITLE_H + d.borrow().len() as i32 * ROW_H + HINT_H + FOOT_H
    })
}

/// Show the card (or repaint it if already up).
pub fn show(hk: &HotkeyConfig) {
    DATA.with(|d| *d.borrow_mut() = lines(hk));
    if let Some(h) = hwnd() {
        unsafe {
            let _ = InvalidateRect(Some(h), None, false);
        }
        return;
    }
    unsafe {
        create();
    }
}

/// Button rects inside the client area: "Got it" (outline) left of
/// "Open settings" (primary), both bottom-right of the footer.
fn btn_rects(rc: &RECT, dpi: u32) -> (RECT, RECT) {
    let s = |v: i32| ui::sx(v, dpi);
    let mid = rc.bottom - s(FOOT_H) + (s(FOOT_H) - s(BTN_H)) / 2;
    let right = rc.right - s(PAD);
    let r2 = RECT {
        left: right - s(136),
        top: mid,
        right,
        bottom: mid + s(BTN_H),
    };
    let r1 = RECT {
        left: r2.left - s(BTN_GAP) - s(76),
        top: mid,
        right: r2.left - s(BTN_GAP),
        bottom: mid + s(BTN_H),
    };
    (r1, r2)
}

/// Which button (if any) is under client point (x, y).
fn btn_at(rc: &RECT, dpi: u32, x: i32, y: i32) -> i32 {
    let (r1, r2) = btn_rects(rc, dpi);
    let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if inside(&r1) {
        BTN_GOTIT
    } else if inside(&r2) {
        BTN_SETTINGS
    } else {
        0
    }
}

unsafe fn create() -> Option<HWND> {
    let hinst = GetModuleHandleW(None).ok()?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: CLASS,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc);
    // centre of the display under the mouse
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(mon, &mut mi);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(
        mon,
        windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
        &mut dx,
        &mut dy,
    );
    let dpi = dx.max(96);
    let wa = mi.rcWork;
    let w = ui::sx(W, dpi).min(wa.right - wa.left - 32);
    let h = ui::sx(height_logical(), dpi).min(wa.bottom - wa.top - 32);
    let x = wa.left + (wa.right - wa.left - w) / 2;
    let y = wa.top + (wa.bottom - wa.top - h) / 2;
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        CLASS,
        w!("Welcome to ZoneLock"),
        WS_POPUP,
        x,
        y,
        w,
        h,
        None,
        None,
        Some(hinst.into()),
        None,
    );
    let hwnd = hwnd.ok()?;
    let round: i32 = 2; // DWMWCP_ROUND
    let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
        hwnd,
        windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(33),
        &round as *const _ as *const _,
        4,
    );
    WIN.with(|wv| wv.set(hwnd.0 as isize));
    let _ = RegisterHotKey(
        Some(hwnd),
        ESC_HOTKEY,
        HOT_KEY_MODIFIERS(0),
        VK_ESCAPE.0 as u32,
    );
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    Some(hwnd)
}

unsafe fn with_font<R>(hdc: HDC, pt: i32, weight: i32, dpi: u32, f: impl FnOnce() -> R) -> R {
    let font = ui::segoe(pt, weight, dpi);
    let old = SelectObject(hdc, font.into());
    let r = f();
    SelectObject(hdc, old);
    let _ = DeleteObject(font.into());
    r
}

unsafe fn paint(hwnd: HWND) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96);
    let s = |v: i32| ui::sx(v, dpi);
    let data = DATA.with(|d| d.borrow().clone());
    let hover = HOVER.with(|h| h.get());
    ui::buffered_paint(hwnd, rc.right, rc.bottom, |hdc| {
        ui::fill(hdc, &rc, ui::BG);
        ui::round_box(hdc, &rc, s(12), ui::BG, Some((ui::EDGE, 1)));
        SetBkMode(hdc, TRANSPARENT);
        // title + version
        ui::dot(hdc, s(PAD) + s(5), s(30), s(5), ui::ACCENT);
        with_font(hdc, 15, 700, dpi, || {
            SetTextColor(hdc, ui::cr(ui::TXT));
            let mut r = RECT { left: s(PAD + 18), top: s(14), right: rc.right - s(PAD), bottom: s(46) };
            ui::text(hdc, &mut r, "Welcome to ZoneLock", DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        });
        with_font(hdc, 9, 400, dpi, || {
            SetTextColor(hdc, ui::cr(ui::MUTED));
            let mut r = RECT { left: s(PAD), top: s(44), right: rc.right - s(PAD), bottom: s(70) };
            ui::text(hdc, &mut r, "Zones keep every window where you put it. The gist:", DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        });
        let mut y = s(TITLE_H);
        for line in &data {
            let hair = RECT { left: s(PAD), top: y, right: rc.right - s(PAD), bottom: y + 1 };
            ui::fill(hdc, &hair, ui::EDGE);
            with_font(hdc, 10, 400, dpi, || {
                SetTextColor(hdc, ui::cr(ui::TXT));
                let mut r = RECT { left: s(PAD), top: y, right: rc.right / 2 + s(70), bottom: y + s(ROW_H) };
                ui::text(hdc, &mut r, &line.action, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            });
            // key chips, right-aligned (same recipe as the cheatsheet)
            with_font(hdc, 9, 600, dpi, || {
                let chip_h = s(24);
                let top = y + (s(ROW_H) - chip_h) / 2;
                let widths: Vec<i32> =
                    line.keys.iter().map(|k| ui::text_w(hdc, k) + s(16)).collect();
                let plus = s(14);
                let total: i32 = widths.iter().sum::<i32>()
                    + plus * (widths.len() as i32 - 1).max(0);
                let mut x = rc.right - s(PAD) - total;
                for (i, (k, w)) in line.keys.iter().zip(&widths).enumerate() {
                    if i > 0 {
                        SetTextColor(hdc, ui::cr(ui::MUTED));
                        let mut r = RECT { left: x, top, right: x + plus, bottom: top + chip_h };
                        ui::text(hdc, &mut r, "+", DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                        x += plus;
                    }
                    let chip = RECT { left: x, top, right: x + w, bottom: top + chip_h };
                    ui::round_box(hdc, &chip, s(6), ui::CARD, Some((ui::EDGE, 1)));
                    SetTextColor(hdc, ui::cr(ui::TXT));
                    let mut r = chip;
                    ui::text(hdc, &mut r, k, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    x += w;
                }
            });
            y += s(ROW_H);
        }
        with_font(hdc, 9, 400, dpi, || {
            SetTextColor(hdc, ui::cr(ui::MUTED));
            let mut r = RECT { left: s(PAD), top: y, right: rc.right - s(PAD), bottom: y + s(HINT_H) };
            ui::text(
                hdc,
                &mut r,
                "Left-click the tray icon anytime for settings, plans and presets.",
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );
        });
        // buttons: "Got it" (outline) then "Open settings" (primary)
        let (r1, r2) = btn_rects(&rc, dpi);
        ui::round_box(
            hdc,
            &r1,
            s(8),
            if hover == BTN_GOTIT { ui::blend(ui::TXT, ui::BG, 26) } else { ui::BG },
            Some((ui::EDGE, 1)),
        );
        ui::round_box(
            hdc,
            &r2,
            s(8),
            if hover == BTN_SETTINGS { ui::blend(ui::ACCENT, ui::TXT, 225) } else { ui::ACCENT },
            None,
        );
        with_font(hdc, 9, 600, dpi, || {
            SetTextColor(hdc, ui::cr(if hover == BTN_GOTIT { ui::TXT } else { ui::MUTED }));
            let mut r = r1;
            ui::text(hdc, &mut r, "Got it", DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SetTextColor(hdc, ui::cr(ui::BG));
            let mut r = r2;
            ui::text(hdc, &mut r, "Open settings", DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        });
    });
}

/// Button under the cursor right now (client coords from GetMessagePos).
unsafe fn hit_button(hwnd: HWND) -> i32 {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96);
    let pos = GetMessagePos();
    let mut pt = POINT {
        x: (pos & 0xFFFF) as i16 as i32,
        y: ((pos >> 16) & 0xFFFF) as i16 as i32,
    };
    let _ = ScreenToClient(hwnd, &mut pt);
    btn_at(&rc, dpi, pt.x, pt.y)
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // never take the focus, even on click
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let b = hit_button(hwnd);
            if HOVER.with(|h| h.replace(b)) != b {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_SETCURSOR
            if lparam.0 as u16 as u32 == HTCLIENT
                && hit_button(hwnd) != 0 =>
        {
            let hand = LoadCursorW(None, IDC_HAND).unwrap_or_default();
            SetCursor(Some(hand));
            LRESULT(1)
        }
        WM_LBUTTONUP => match hit_button(hwnd) {
            BTN_GOTIT => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            BTN_SETTINGS => {
                let _ = DestroyWindow(hwnd);
                crate::tray::send(crate::tray::UiCmd::OpenSettings);
                LRESULT(0)
            }
            _ => LRESULT(0),
        },
        WM_HOTKEY if wparam.0 as i32 == ESC_HOTKEY => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = UnregisterHotKey(Some(hwnd), ESC_HOTKEY);
            WIN.with(|w| w.set(0));
            HOVER.with(|h| h.set(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_registers() {
        unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: w!("ZoneLockWelcomeTest"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            let atom = RegisterClassExW(&wc);
            eprintln!("hinst={:?} atom={atom} err={:?}", hinst, windows::Win32::Foundation::GetLastError());
            assert_ne!(atom, 0);
        }
    }

    #[test]
    fn lines_follow_the_bindings() {
        let hk = HotkeyConfig::default();
        let l = lines(&hk);
        assert_eq!(l.len(), 4);
        assert_eq!(l[0].keys, vec!["Shift".to_string(), "drag".to_string()]);
        // a cleared drag key leaves just the gesture
        let mut hk = hk;
        hk.drag_snap = String::new();
        assert_eq!(lines(&hk)[0].keys, vec!["drag".to_string()]);
    }
}
