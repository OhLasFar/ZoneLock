//! Small on-screen card for saving plans from the keyboard: "Plan 4
//! saved" confirmations, and the overwrite question when the slot is
//! already taken. Native GDI (instant, no WebView2), bottom-centre of the
//! display under the mouse, topmost, NEVER activated: the app you are
//! working in keeps the focus.
//!
//! While the overwrite question is shown, the plan captured at the first
//! key press waits here (`pending`): pressing the same save shortcut again
//! writes it, another number saves it there, Esc (temporary hotkey owned
//! by this window), a click or the timeout cancel it. Lives on the UI
//! thread.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::plans::Plan;
use crate::uidraw as ui;

const CLASS: PCWSTR = w!("ZoneLockPlanPrompt");
// logical (96 dpi) size
const W: i32 = 660;
const H: i32 = 104;
const TIMER: usize = 1;
const ESC_HOTKEY: i32 = 1;
const SAVED_MS: u32 = 2200;
const ASK_MS: u32 = 7000;

// Dispattern signal colours (0x00BBGGRR)
const BG: u32 = 0x0021_1A17; // #171a21 elevated
const RING: u32 = 0x0035_302C; // white ~8% over BG
const FG: u32 = 0x00F2_EEEC; // #eceef2
const MUTED: u32 = 0x009C_908B; // #8b909c
const OK: u32 = 0x0099_D334; // #34d399
const WARN: u32 = 0x0024_BFFB; // #fbbf24

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Saved,
    Ask,
    Info,
}

struct Card {
    kind: Kind,
    title: String,
    body: String,
    hint: String,
}

thread_local! {
    static WIN: Cell<isize> = const { Cell::new(0) };
    static CARD: RefCell<Option<Card>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<(u8, Plan)>> = const { RefCell::new(None) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}

/// Slot the overwrite question is about, if one is on screen.
pub fn pending_slot() -> Option<u8> {
    PENDING.with(|p| p.borrow().as_ref().map(|(n, _)| *n))
}

/// Take the plan captured when the question was asked.
pub fn take_pending() -> Option<Plan> {
    PENDING.with(|p| p.borrow_mut().take().map(|(_, plan)| plan))
}

pub fn show_saved(slot: u8, plan: &Plan) {
    PENDING.with(|p| p.borrow_mut().take());
    show(
        Card {
            kind: Kind::Saved,
            title: format!("Plan {slot} saved"),
            body: crate::plans::app_names(plan),
            hint: String::new(),
        },
        SAVED_MS,
        false,
    );
}

pub fn show_info(title: &str, body: &str) {
    PENDING.with(|p| p.borrow_mut().take());
    show(
        Card {
            kind: Kind::Info,
            title: title.into(),
            body: body.into(),
            hint: String::new(),
        },
        SAVED_MS,
        false,
    );
}

/// A success card (green, like "Plan saved") that stays a bit longer:
/// used to confirm a finished update.
pub fn show_success(title: &str, body: &str) {
    PENDING.with(|p| p.borrow_mut().take());
    show(
        Card {
            kind: Kind::Saved,
            title: title.into(),
            body: body.into(),
            hint: String::new(),
        },
        6000,
        false,
    );
}

/// Ask before overwriting `slot`. `captured` is what will be written if
/// the user confirms; `save_label` is the save shortcut modifiers label.
pub fn ask_overwrite(slot: u8, captured: Plan, existing: &Plan, free: &[u8], save_label: &str) {
    let instead = match free.first() {
        Some(f) => format!("   \u{00b7}   {save_label}+{f}: plan {f} instead"),
        None => String::new(),
    };
    let card = Card {
        kind: Kind::Ask,
        title: format!("Plan {slot} already holds {}", crate::plans::app_names(existing)),
        body: format!("Save {} over it?", crate::plans::app_names(&captured)),
        hint: format!("{save_label}+{slot} again: overwrite{instead}   \u{00b7}   Esc: cancel"),
    };
    PENDING.with(|p| *p.borrow_mut() = Some((slot, captured)));
    show(card, ASK_MS, true);
}

fn hwnd() -> Option<HWND> {
    let v = WIN.with(|w| w.get());
    (v != 0).then_some(HWND(v as *mut _))
}

pub fn close() {
    PENDING.with(|p| p.borrow_mut().take());
    if let Some(h) = hwnd() {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

/// Bottom-centre of the work area of the display under the mouse.
unsafe fn placement() -> (i32, i32, i32, i32, u32) {
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
    let (w, h) = (ui::sx(W, dpi).min(wa.right - wa.left - 16), ui::sx(H, dpi));
    let x = wa.left + (wa.right - wa.left - w) / 2;
    let y = wa.bottom - h - ui::sx(28, dpi);
    (x, y, w, h, dpi)
}

fn show(card: Card, ms: u32, esc: bool) {
    CARD.with(|c| *c.borrow_mut() = Some(card));
    unsafe {
        let h = match hwnd() {
            Some(h) => h,
            None => match create() {
                Some(h) => h,
                None => return,
            },
        };
        let (x, y, w, hh, _) = placement();
        let _ = SetWindowPos(h, Some(HWND_TOPMOST), x, y, w, hh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let reg = REGISTERED.with(|r| r.get());
        if esc && !reg {
            // Esc belongs to this card only while the question is shown
            if RegisterHotKey(Some(h), ESC_HOTKEY, HOT_KEY_MODIFIERS(0), VK_ESCAPE.0 as u32).is_ok() {
                REGISTERED.with(|r| r.set(true));
            }
        } else if !esc && reg {
            let _ = UnregisterHotKey(Some(h), ESC_HOTKEY);
            REGISTERED.with(|r| r.set(false));
        }
        SetTimer(Some(h), TIMER, ms, None);
        let _ = InvalidateRect(Some(h), None, false);
    }
}

unsafe fn create() -> Option<HWND> {
    let hinst = GetModuleHandleW(None).ok()?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: CLASS,
        hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc); // fails harmlessly when already registered
    let (x, y, w, h, _) = placement();
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        CLASS,
        w!("ZoneLock plan"),
        WS_POPUP,
        x,
        y,
        w,
        h,
        None,
        None,
        Some(hinst.into()),
        None,
    )
    .ok()?;
    // Windows 11 rounded corners on a popup
    let round: i32 = 2; // DWMWCP_ROUND
    let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
        hwnd,
        windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(33), // DWMWA_WINDOW_CORNER_PREFERENCE
        &round as *const _ as *const _,
        4,
    );
    WIN.with(|wv| wv.set(hwnd.0 as isize));
    Some(hwnd)
}

unsafe fn paint(hwnd: HWND) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96);
    let s = |v: i32| ui::sx(v, dpi);
    CARD.with(|c| {
        let c = c.borrow();
        let Some(card) = c.as_ref() else { return };
        ui::buffered_paint(hwnd, rc.right, rc.bottom, |hdc| {
            ui::fill(hdc, &rc, BG);
            // 1px ring
            let edge = RECT { left: 0, top: 0, right: rc.right, bottom: rc.bottom };
            ui::round_box(hdc, &edge, s(10), BG, Some((RING, 1)));
            let dot = match card.kind {
                Kind::Saved => OK,
                Kind::Ask => WARN,
                Kind::Info => MUTED,
            };
            ui::dot(hdc, s(24), s(29), s(5), dot);
            SetBkMode(hdc, TRANSPARENT);
            let lines: [(&str, i32, i32, u32, i32, i32); 3] = [
                (&card.title, 11, 700, FG, s(16), s(42)),
                (&card.body, 10, 400, MUTED, s(44), s(66)),
                (&card.hint, 9, 600, FG, s(70), s(94)),
            ];
            for (text, pt, weight, color, top, bottom) in lines {
                if text.is_empty() {
                    continue;
                }
                let f = ui::segoe(pt, weight, dpi);
                let old = SelectObject(hdc, f.into());
                SetTextColor(hdc, ui::cr(color));
                let mut r = RECT { left: s(42), top, right: rc.right - s(18), bottom };
                ui::text(hdc, &mut r, text, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                SelectObject(hdc, old);
                let _ = DeleteObject(f.into());
            }
        });
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // never take the focus, even on click
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP | WM_TIMER => {
            close();
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == ESC_HOTKEY => {
            close();
            crate::log::line("plan overwrite cancelled");
            LRESULT(0)
        }
        WM_DESTROY => {
            if REGISTERED.with(|r| r.replace(false)) {
                let _ = UnregisterHotKey(Some(hwnd), ESC_HOTKEY);
            }
            let _ = KillTimer(Some(hwnd), TIMER);
            WIN.with(|w| w.set(0));
            CARD.with(|c| c.borrow_mut().take());
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
