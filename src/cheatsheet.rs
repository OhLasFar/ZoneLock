//! "All shortcuts" card: the cheatsheet hotkey shows every ZoneLock
//! shortcut, with the user's current bindings, in a card in the middle of
//! the display under the mouse. Native GDI, topmost, never activated (the
//! app you are in keeps the focus); Esc (temporary hotkey owned by the
//! card), the same shortcut again or a click closes it. UI thread.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::HotkeyConfig;
use crate::uidraw as ui;

const CLASS: PCWSTR = w!("ZoneLockCheatsheet");
const ESC_HOTKEY: i32 = 1;
// logical (96 dpi) metrics
const W: i32 = 620;
const PAD: i32 = 28;
const TITLE_H: i32 = 64;
const SECTION_H: i32 = 34;
const ROW_H: i32 = 38;

// Dispattern signal colours (0x00BBGGRR)
const BG: u32 = 0x0021_1A17; // #171a21
const RING: u32 = 0x0035_302C;
const FG: u32 = 0x00F2_EEEC;
const MUTED: u32 = 0x009C_908B;
const KEY_BG: u32 = 0x002C_2622; // #22262c
const KEY_EDGE: u32 = 0x0046_3F3A;
const BLUE: u32 = 0x00F6_823B; // #3b82f6

/// One line of the card: what it does, and the keys (already split).
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub action: String,
    pub keys: Vec<String>,
}

/// Sections of the card for these bindings. Pure.
pub fn sections(hk: &HotkeyConfig) -> Vec<(&'static str, Vec<Line>)> {
    let keys = |b: &str| split_keys(&crate::hotkeys::binding_label(b));
    let digits = |b: &str| {
        let mut k = split_keys(&crate::hotkeys::binding_label(b));
        k.push("1\u{2026}9".into());
        k
    };
    let line = |a: &str, k: Vec<String>| Line { action: a.into(), keys: k };
    // the drag key is the configured modifier set (empty = every drag)
    let mut drag = split_keys(&crate::hotkeys::binding_label(&hk.drag_snap));
    drag.push("drag".into());
    vec![
        (
            "Windows",
            vec![
                line("Snap into a zone", drag),
                line("Next zone", keys(&hk.next_zone)),
                line("Previous zone", keys(&hk.prev_zone)),
                line("Fullscreen: zone or whole screen", keys(&hk.fs_toggle)),
            ],
        ),
        (
            "Plans",
            vec![
                line("Go to plan 1-9", digits(&hk.plan_go)),
                line("Save open apps as plan 1-9", digits(&hk.plan_save)),
            ],
        ),
        (
            "Zones and app",
            vec![
                line("Next preset (display under the mouse)", keys(&hk.next_preset)),
                line("Zone editor", keys(&hk.editor)),
                line("Pause / resume ZoneLock", keys(&hk.toggle)),
                line("Show this card", keys(&hk.cheatsheet)),
            ],
        ),
    ]
}

/// "Win+Ctrl+Alt+Right" -> ["Win", "Ctrl", "Alt", "→"].
pub fn split_keys(label: &str) -> Vec<String> {
    label
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|k| match k {
            "Left" => "\u{2190}".to_string(),
            "Up" => "\u{2191}".to_string(),
            "Right" => "\u{2192}".to_string(),
            "Down" => "\u{2193}".to_string(),
            other => other.to_string(),
        })
        .collect()
}

thread_local! {
    static WIN: Cell<isize> = const { Cell::new(0) };
    static DATA: RefCell<Vec<(&'static str, Vec<Line>)>> = const { RefCell::new(Vec::new()) };
}

fn hwnd() -> Option<HWND> {
    let v = WIN.with(|w| w.get());
    (v != 0).then_some(HWND(v as *mut _))
}

/// Show the card, or close it if it is already shown.
pub fn toggle(hk: &HotkeyConfig) {
    if let Some(h) = hwnd() {
        unsafe {
            let _ = DestroyWindow(h);
        }
        return;
    }
    DATA.with(|d| *d.borrow_mut() = sections(hk));
    unsafe {
        let _ = create();
    }
}

fn height_logical() -> i32 {
    DATA.with(|d| {
        let d = d.borrow();
        let rows: usize = d.iter().map(|(_, l)| l.len()).sum();
        TITLE_H + d.len() as i32 * SECTION_H + rows as i32 * ROW_H + PAD
    })
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
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(mon, &mut mi);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(mon, windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let dpi = dx.max(96);
    let wa = mi.rcWork;
    let w = ui::sx(W, dpi).min(wa.right - wa.left - 32);
    let h = ui::sx(height_logical(), dpi).min(wa.bottom - wa.top - 32);
    let x = wa.left + (wa.right - wa.left - w) / 2;
    let y = wa.top + (wa.bottom - wa.top - h) / 2;
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        CLASS,
        w!("ZoneLock shortcuts"),
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
    let round: i32 = 2; // DWMWCP_ROUND
    let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
        hwnd,
        windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(33),
        &round as *const _ as *const _,
        4,
    );
    WIN.with(|wv| wv.set(hwnd.0 as isize));
    let _ = RegisterHotKey(Some(hwnd), ESC_HOTKEY, HOT_KEY_MODIFIERS(0), VK_ESCAPE.0 as u32);
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
    ui::buffered_paint(hwnd, rc.right, rc.bottom, |hdc| {
        ui::fill(hdc, &rc, BG);
        ui::round_box(hdc, &rc, s(12), BG, Some((RING, 1)));
        SetBkMode(hdc, TRANSPARENT);
        // title
        ui::dot(hdc, s(PAD) + s(5), s(34), s(5), BLUE);
        with_font(hdc, 14, 700, dpi, || {
            SetTextColor(hdc, ui::cr(FG));
            let mut r = RECT { left: s(PAD + 20), top: s(18), right: rc.right - s(PAD), bottom: s(50) };
            ui::text(hdc, &mut r, "ZoneLock shortcuts", DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        });
        with_font(hdc, 9, 400, dpi, || {
            SetTextColor(hdc, ui::cr(MUTED));
            let mut r = RECT { left: s(PAD), top: s(18), right: rc.right - s(PAD), bottom: s(50) };
            ui::text(hdc, &mut r, "Esc to close", DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
        });
        let mut y = s(TITLE_H);
        for (title, lines) in &data {
            with_font(hdc, 8, 700, dpi, || {
                SetTextColor(hdc, ui::cr(BLUE));
                let mut r = RECT { left: s(PAD), top: y + s(8), right: rc.right - s(PAD), bottom: y + s(SECTION_H) };
                ui::text(hdc, &mut r, &title.to_uppercase(), DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            });
            y += s(SECTION_H);
            for line in lines {
                // hairline above each row
                let hair = RECT { left: s(PAD), top: y, right: rc.right - s(PAD), bottom: y + 1 };
                ui::fill(hdc, &hair, RING);
                with_font(hdc, 10, 400, dpi, || {
                    SetTextColor(hdc, ui::cr(FG));
                    let mut r = RECT { left: s(PAD), top: y, right: rc.right / 2 + s(40), bottom: y + s(ROW_H) };
                    ui::text(hdc, &mut r, &line.action, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                });
                // key chips, right-aligned
                with_font(hdc, 9, 600, dpi, || {
                    let chip_h = s(24);
                    let top = y + (s(ROW_H) - chip_h) / 2;
                    let widths: Vec<i32> = line.keys.iter().map(|k| ui::text_w(hdc, k) + s(16)).collect();
                    let plus = s(14);
                    let total: i32 = widths.iter().sum::<i32>() + plus * (widths.len() as i32 - 1).max(0);
                    let mut x = rc.right - s(PAD) - total;
                    for (i, (k, w)) in line.keys.iter().zip(&widths).enumerate() {
                        if i > 0 {
                            SetTextColor(hdc, ui::cr(MUTED));
                            let mut r = RECT { left: x, top, right: x + plus, bottom: top + chip_h };
                            ui::text(hdc, &mut r, "+", DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                            x += plus;
                        }
                        let chip = RECT { left: x, top, right: x + w, bottom: top + chip_h };
                        ui::round_box(hdc, &chip, s(6), KEY_BG, Some((KEY_EDGE, 1)));
                        SetTextColor(hdc, ui::cr(FG));
                        let mut r = chip;
                        ui::text(hdc, &mut r, k, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                        x += w;
                    }
                });
                y += s(ROW_H);
            }
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == ESC_HOTKEY => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = UnregisterHotKey(Some(hwnd), ESC_HOTKEY);
            WIN.with(|w| w.set(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_split_with_arrows() {
        assert_eq!(split_keys("Win+Ctrl+Alt+Right"), vec!["Win", "Ctrl", "Alt", "\u{2192}"]);
        assert_eq!(split_keys("Alt + Shift"), vec!["Alt", "Shift"]);
    }

    #[test]
    fn every_shortcut_is_listed() {
        let hk = HotkeyConfig::default();
        let s = sections(&hk);
        let all: Vec<&Line> = s.iter().flat_map(|(_, l)| l.iter()).collect();
        assert_eq!(all.len(), 10);
        let snap = all.iter().find(|l| l.action == "Snap into a zone").unwrap();
        assert_eq!(snap.keys, vec!["Shift".to_string(), "drag".to_string()]);
        let plan = all.iter().find(|l| l.action.starts_with("Go to plan")).unwrap();
        assert_eq!(plan.keys.last().unwrap(), "1\u{2026}9");
        assert!(all.iter().any(|l| l.action.starts_with("Fullscreen") && l.keys.contains(&"\u{2191}".to_string())));
    }
}
