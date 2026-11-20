//! Settings panel: a custom-drawn dark Win32 window (borderless with
//! rounded corners, painted titlebar, pill toggles, styled fields,
//! owner-drawn buttons) that edits the same Config the tray toggles
//! touch. Lives on the UI thread; one instance at a time.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::tray::UiCmd;
use crate::uidraw as ui;
use crate::watcher::Shared;

const CLASS: PCWSTR = w!("ZoneLockSettings");
const TOGGLE_CLASS: PCWSTR = w!("ZoneLockToggle");

const IDC_ENABLED: u16 = 200;
const IDC_SHIFT_SNAP: u16 = 201;
const IDC_AUTOSTART: u16 = 202;
const IDC_LOG: u16 = 203;
const IDC_GAP: u16 = 204;
const IDC_HK_TOGGLE: u16 = 205;
const IDC_HK_EDITOR: u16 = 206;
const IDC_HK_PRESET: u16 = 207;
const IDC_HK_ZONE: u16 = 208;
const IDC_FSKEYS: u16 = 209;
const IDC_SAVE: u16 = 210;
const IDC_CANCEL: u16 = 211;

// logical (96 dpi) metrics
const W: i32 = 480;
const H: i32 = 720;
const PAD: i32 = 28;
const TITLE_H: i32 = 56;
const ROW_H: i32 = 34;
const FIELD_H: i32 = 30;

thread_local! {
    static WIN: Cell<isize> = const { Cell::new(0) };
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// Painted element geometry, computed at WM_CREATE with real dpi.
    static FIELD_RECTS: RefCell<Vec<(u16, RECT)>> = const { RefCell::new(Vec::new()) };
    static LABELS: RefCell<Vec<(RECT, String, u32)>> = const { RefCell::new(Vec::new()) };
    static DPI: Cell<u32> = const { Cell::new(96) };
    static FIELD_BRUSH: Cell<isize> = const { Cell::new(0) };
    static LINK_R: Cell<RECT> = const { Cell::new(RECT { left: 0, top: 0, right: 0, bottom: 0 }) };
}

fn ctrl(hwnd: HWND, id: u16) -> HWND {
    unsafe { GetDlgItem(Some(hwnd), id as i32).unwrap_or_default() }
}

fn get_text(hwnd: HWND, id: u16) -> String {
    let mut buf = vec![0u16; 1024];
    unsafe {
        let n = GetWindowTextW(ctrl(hwnd, id), &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

fn set_text(hwnd: HWND, id: u16, s: &str) {
    let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = SendMessageW(
            ctrl(hwnd, id),
            WM_SETTEXT,
            Some(WPARAM(0)),
            Some(LPARAM(w.as_ptr() as isize)),
        );
    }
}

fn checked(hwnd: HWND, id: u16) -> bool {
    unsafe {
        SendMessageW(ctrl(hwnd, id), BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0)))
            .0 == 1
    }
}

fn set_checked(hwnd: HWND, id: u16, on: bool) {
    unsafe {
        let _ = SendMessageW(
            ctrl(hwnd, id),
            BM_SETCHECK,
            Some(WPARAM(if on { 1 } else { 0 })),
            Some(LPARAM(0)),
        );
    }
}

fn err_box(hwnd: HWND, msg: &str) {
    let wmsg: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR(wmsg.as_ptr()),
            w!("ZoneLock settings"),
            MB_ICONWARNING | MB_OK,
        );
    }
}

// ------------------------------------------------------------------
// Pill toggle: a tiny custom control answering BM_GETCHECK/SETCHECK and
// notifying its parent with WM_COMMAND(BN_CLICKED) like a checkbox.
// ------------------------------------------------------------------

unsafe extern "system" fn toggle_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        BM_GETCHECK => LRESULT(if GetWindowLongPtrW(hwnd, GWLP_USERDATA) != 0 {
            1
        } else {
            0
        }),
        BM_SETCHECK => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, if wparam.0 != 0 { 1 } else { 0 });
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let on = GetWindowLongPtrW(hwnd, GWLP_USERDATA) == 0;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, if on { 1 } else { 0 });
            let _ = InvalidateRect(Some(hwnd), None, false);
            let parent = GetParent(hwnd).unwrap_or_default();
            let id = GetDlgCtrlID(hwnd);
            let _ = SendMessageW(
                parent,
                WM_COMMAND,
                Some(WPARAM(id as usize)),
                Some(LPARAM(hwnd.0 as isize)),
            );
            LRESULT(0)
        }
        WM_PAINT => {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let on = GetWindowLongPtrW(hwnd, GWLP_USERDATA) != 0;
            ui::buffered_paint(hwnd, rc.right, rc.bottom, |hdc| {
                let w = rc.right - rc.left;
                let h = rc.bottom - rc.top;
                let track = RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                };
                let track_c = if on {
                    ui::ACCENT
                } else {
                    ui::blend(ui::MUTED, ui::BG, 90)
                };
                ui::round_box(hdc, &track, h / 2, track_c, None);
                let r = (h / 2) - 3;
                let kx = if on { w - h / 2 } else { h / 2 };
                ui::dot(hdc, kx, h / 2, r, ui::TXT);
            });
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ------------------------------------------------------------------
// Main window
// ------------------------------------------------------------------

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
            DPI.with(|d| d.set(dpi.max(96)));
            // normalize the OUTER size to THIS monitor's dpi (the window
            // may have been created from the system dpi) - account for
            // the frame so the client stays exactly W x H logical
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let exstyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let mut wr = RECT {
                left: 0,
                top: 0,
                right: ui::sx(W, dpi.max(96)),
                bottom: ui::sx(H, dpi.max(96)),
            };
            let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
                &mut wr,
                WINDOW_STYLE(style),
                false,
                WINDOW_EX_STYLE(exstyle),
                dpi.max(96),
            );
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                wr.right - wr.left,
                wr.bottom - wr.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            build_controls(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xFFFF) as u32;
            DPI.with(|d| d.set(dpi.max(96)));
            let sug = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                sug.left,
                sug.top,
                sug.right - sug.left,
                sug.bottom - sug.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            relayout(hwnd);
            let _ = InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // hand cursor over the credit link (client area only)
            if (lparam.0 & 0xFFFF) == 1 {
                let mut pt = windows::Win32::Foundation::POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                let lr = LINK_R.with(|c| c.get());
                if pt.x >= lr.left && pt.x < lr.right && pt.y >= lr.top && pt.y < lr.bottom {
                    if let Ok(h) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(Some(h));
                        return LRESULT(1);
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONUP => {
            let x = lparam.0 as i16 as i32;
            let y = (lparam.0 >> 16) as i16 as i32;
            let lr = LINK_R.with(|c| c.get());
            if x >= lr.left && x < lr.right && y >= lr.top && y < lr.bottom {
                let _ = windows::Win32::UI::Shell::ShellExecuteW(
                    None,
                    w!("open"),
                    w!("https://x.com/LasFar"),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOW,
                );
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as *mut _);
            SetTextColor(hdc, ui::cr(ui::TXT));
            SetBkColor(hdc, ui::cr(ui::FIELD));
            LRESULT(FIELD_BRUSH.with(|b| b.get()))
        }
        WM_DRAWITEM => {
            let di = &*(lparam.0 as *const DRAWITEMSTRUCT);
            draw_button(di);
            LRESULT(1)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            match id {
                IDC_SAVE => {
                    if apply(hwnd) {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                IDC_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let dpi = DPI.with(|d| d.get());
            let (w, h) = (ui::sx(W, dpi), ui::sx(H, dpi));
            ui::buffered_paint(hwnd, w, h, |hdc| paint(hwnd, hdc, dpi));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            WIN.with(|w| w.set(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn draw_button(di: &DRAWITEMSTRUCT) {
    let hdc = di.hDC;
    let r = di.rcItem;
    let is_save = di.CtlID as u16 == IDC_SAVE;
    let pressed = di.itemState.0 & 0x0001 != 0; // ODS_SELECTED
    let dpi = DPI.with(|d| d.get());
    let rad = ui::sx(8, dpi);
    // paint the parent background first: the rounded rect leaves
    // corners that would otherwise show the default white button face
    ui::fill(hdc, &r, ui::BG);
    if is_save {
        let fill_c = if pressed {
            ui::blend(ui::ACCENT, ui::BG, 200)
        } else {
            ui::ACCENT
        };
        ui::round_box(hdc, &r, rad, fill_c, None);
        SetTextColor(hdc, ui::cr(ui::BG));
    } else {
        ui::round_box(hdc, &r, rad, ui::FIELD, Some((ui::EDGE, 1)));
        SetTextColor(hdc, ui::cr(ui::TXT));
    }
    SetBkMode(hdc, TRANSPARENT);
    let font = ui::segoe(9, 600, dpi);
    let old = SelectObject(hdc, font.into());
    let text = if is_save { "Save" } else { "Cancel" };
    let mut tr = r;
    ui::text(hdc, &mut tr, text, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc, old);
    let _ = DeleteObject(font.into());
}

unsafe fn paint(_hwnd: HWND, hdc: HDC, dpi: u32) {
    let s = |v: i32| ui::sx(v, dpi);
    let w = s(W);
    let h = s(H);
    let full = RECT { left: 0, top: 0, right: w, bottom: h };
    ui::fill(hdc, &full, ui::BG);

    // titlebar strip
    let tb = RECT { left: 0, top: 0, right: w, bottom: s(TITLE_H) };
    ui::fill(hdc, &tb, ui::CARD);
    let hair = RECT { left: 0, top: s(TITLE_H) - 1, right: w, bottom: s(TITLE_H) };
    ui::fill(hdc, &hair, ui::EDGE);

    // logo + titles
    let icon = crate::icon::app_icon(20);
    let isz = s(20);
    let _ = DrawIconEx(
        hdc,
        s(PAD),
        s(TITLE_H) / 2 - isz / 2,
        icon,
        isz,
        isz,
        0,
        None,
        DI_NORMAL,
    );
    SetBkMode(hdc, TRANSPARENT);
    let f_title = ui::segoe(11, 700, dpi);
    let old_f = SelectObject(hdc, f_title.into());
    SetTextColor(hdc, ui::cr(ui::TXT));
    let mut tr = RECT {
        left: s(PAD + 30),
        top: s(10),
        right: w - s(60),
        bottom: s(30),
    };
    ui::text(hdc, &mut tr, "ZoneLock", DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc, old_f);
    let _ = DeleteObject(f_title.into());
    let f_sub = ui::segoe(8, 400, dpi);
    let old_f2 = SelectObject(hdc, f_sub.into());
    SetTextColor(hdc, ui::cr(ui::MUTED));
    let mut sr = RECT {
        left: s(PAD + 30),
        top: s(30),
        right: w - s(60),
        bottom: s(48),
    };
    ui::text(hdc, &mut sr, "Settings", DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc, old_f2);
    let _ = DeleteObject(f_sub.into());

    // labels + section headers + field chrome
    LABELS.with(|ls| {
        for (r, txt, kind) in ls.borrow().iter() {
            let f = if *kind == 1 {
                ui::segoe(8, 700, dpi)
            } else {
                ui::segoe(9, 400, dpi)
            };
            let old = SelectObject(hdc, f.into());
            SetTextColor(hdc, ui::cr(if *kind == 1 { ui::MUTED } else { ui::TXT }));
            let mut rr = *r;
            ui::text(hdc, &mut rr, txt, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            SelectObject(hdc, old);
            let _ = DeleteObject(f.into());
        }
    });
    FIELD_RECTS.with(|fs| {
        for (_, r) in fs.borrow().iter() {
            ui::round_box(hdc, r, s(8), ui::FIELD, Some((ui::EDGE, 1)));
        }
    });

    // footer credit: muted lead-in + accent handle (clickable link)
    let ff = ui::segoe(8, 400, dpi);
    let old_f = SelectObject(hdc, ff.into());
    SetTextColor(hdc, ui::cr(ui::MUTED));
    let lead: Vec<u16> = "built by ".encode_utf16().collect();
    let mut sz = windows::Win32::Foundation::SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &lead, &mut sz);
    let fy = s(H - 26);
    let mut fr = RECT {
        left: s(PAD),
        top: fy,
        right: s(PAD) + sz.cx + s(160),
        bottom: fy + s(16),
    };
    ui::text(hdc, &mut fr, "built by ", DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    SetTextColor(hdc, ui::cr(ui::ACCENT));
    let lr = RECT {
        left: fr.left + sz.cx,
        top: fy,
        right: fr.left + sz.cx + s(120),
        bottom: fy + s(16),
    };
    let mut lr2 = lr;
    ui::text(hdc, &mut lr2, "@LasFar on X", DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    LINK_R.with(|c| c.set(lr));
    SelectObject(hdc, old_f);
    let _ = DeleteObject(ff.into());
}

// ------------------------------------------------------------------
// Layout + controls
// ------------------------------------------------------------------

unsafe fn add_edit(parent: HWND, id: u16, r: RECT, multiline: bool, dpi: u32) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let style = WINDOW_STYLE(
        WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32
            | if multiline {
                ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_WANTRETURN as u32 | WS_VSCROLL.0
            } else {
                0
            },
    );
    let (x, y, w, h) = ctrl_pos(&r, &Ck::Edit(multiline), dpi);
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("Edit"),
        w!(""),
        style,
        x,
        y,
        w,
        h,
        Some(parent),
        Some(HMENU(id as usize as *mut _)),
        Some(hinst.into()),
        None,
    );
}

unsafe fn add_toggle(parent: HWND, id: u16, r: RECT, dpi: u32) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let (x, y, w, h) = ctrl_pos(&r, &Ck::Toggle, dpi);
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        TOGGLE_CLASS,
        w!(""),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0),
        x,
        y,
        w,
        h,
        Some(parent),
        Some(HMENU(id as usize as *mut _)),
        Some(hinst.into()),
        None,
    );
}

unsafe fn add_button(parent: HWND, id: u16, text: &str, r: RECT, default: bool) {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("Button"),
        PCWSTR(wide.as_ptr()),
        WINDOW_STYLE(
            WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32
                | if default { BS_DEFPUSHBUTTON as u32 } else { 0 },
        ),
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        Some(parent),
        Some(HMENU(id as usize as *mut _)),
        Some(hinst.into()),
        None,
    );
}

fn rect(l: i32, t: i32, r: i32, b: i32, dpi: u32) -> RECT {
    RECT {
        left: ui::sx(l, dpi),
        top: ui::sx(t, dpi),
        right: ui::sx(r, dpi),
        bottom: ui::sx(b, dpi),
    }
}

/// What kind of child control a spec entry creates.
#[derive(Clone, Copy)]
enum Ck {
    Toggle,
    Edit(bool /*multiline*/),
    Btn(&'static str, bool /*default*/),
}

/// Full layout spec at a given dpi: painted labels + every child
/// control (id, slot rect, kind). Slots are logical rects; the actual
/// control is inset/aligned inside its slot by `ctrl_pos`.
fn layout(dpi: u32) -> (Vec<(RECT, String, u32)>, Vec<(u16, RECT, Ck)>) {
    let mut labels: Vec<(RECT, String, u32)> = Vec::new();
    let mut ctrls: Vec<(u16, RECT, Ck)> = Vec::new();
    let mut y = TITLE_H + 22;

    macro_rules! section {
        ($t:expr) => {{
            labels.push((rect(PAD, y, W - PAD, y + 18, dpi), $t.to_string(), 1));
            y += 24;
        }};
    }
    macro_rules! row {
        ($t:expr) => {{
            labels.push((rect(PAD, y, W - PAD - 60, y + ROW_H, dpi), $t.to_string(), 0));
            let r = rect(PAD, y, W - PAD, y + ROW_H, dpi);
            y += ROW_H + 6;
            r
        }};
    }
    macro_rules! field {
        ($id:expr, $t:expr) => {{
            labels.push((rect(PAD, y, PAD + 170, y + FIELD_H, dpi), $t.to_string(), 0));
            let fr = rect(PAD + 180, y, W - PAD, y + FIELD_H, dpi);
            y += FIELD_H + 6;
            ($id, fr)
        }};
    }

    section!("GENERAL");
    ctrls.push((IDC_ENABLED, row!("Enable ZoneLock (window containment)"), Ck::Toggle));
    ctrls.push((IDC_SHIFT_SNAP, row!("Shift+drag snaps windows into zones"), Ck::Toggle));
    ctrls.push((IDC_AUTOSTART, row!("Start with Windows"), Ck::Toggle));
    ctrls.push((IDC_LOG, row!("Verbose log (%APPDATA%\\ZoneLock\\zonelock.log)"), Ck::Toggle));
    y += 10;

    section!("ZONES");
    // gap: label + small right-aligned field on one row
    labels.push((rect(PAD, y, W - PAD - 110, y + FIELD_H, dpi), "Zone gap (px)".to_string(), 0));
    let gap_r = rect(W - PAD - 90, y, W - PAD, y + FIELD_H, dpi);
    ctrls.push((IDC_GAP, gap_r, Ck::Edit(false)));
    y += FIELD_H + 16;

    section!("HOTKEYS");
    for (id, label) in [
        (IDC_HK_TOGGLE, "Enable / disable"),
        (IDC_HK_EDITOR, "Zone editor"),
        (IDC_HK_PRESET, "Next preset"),
        (IDC_HK_ZONE, "Window to next zone"),
    ] {
        let (id, fr) = field!(id, label);
        ctrls.push((id, fr, Ck::Edit(false)));
    }
    y += 10;

    section!("FULLSCREEN EXIT KEYS");
    labels.push((rect(PAD, y, W - PAD, y + 16, dpi), "exe=key, one per line".to_string(), 1));
    y += 20;
    // bottom-anchored: field ends above the buttons whatever H is
    ctrls.push((IDC_FSKEYS, rect(PAD, y, W - PAD, H - 70, dpi), Ck::Edit(true)));

    // buttons bottom-right
    let by = H - 58;
    ctrls.push((IDC_CANCEL, rect(W - PAD - 216, by, W - PAD - 108, by + 36, dpi), Ck::Btn("Cancel", false)));
    ctrls.push((IDC_SAVE, rect(W - PAD - 100, by, W - PAD, by + 36, dpi), Ck::Btn("Save", true)));

    (labels, ctrls)
}

/// Actual child-window rect inside its slot.
fn ctrl_pos(r: &RECT, ck: &Ck, dpi: u32) -> (i32, i32, i32, i32) {
    let s = |v: i32| ui::sx(v, dpi);
    match ck {
        Ck::Toggle => {
            let (w, h) = (s(44), s(24));
            (r.right - w, (r.top + r.bottom - h) / 2, w, h)
        }
        Ck::Edit(_) => (
            r.left + s(10),
            r.top + s(5),
            r.right - r.left - s(20),
            r.bottom - r.top - s(10),
        ),
        Ck::Btn(..) => (r.left, r.top, r.right - r.left, r.bottom - r.top),
    }
}

fn store_plan(dpi: u32) {
    let (labels, ctrls) = layout(dpi);
    LABELS.with(|l| *l.borrow_mut() = labels);
    FIELD_RECTS.with(|f| {
        *f.borrow_mut() = ctrls
            .iter()
            .filter_map(|(id, r, ck)| matches!(ck, Ck::Edit(_)).then_some((*id, *r)))
            .collect();
    });
}

unsafe fn build_controls(hwnd: HWND) {
    let dpi = DPI.with(|d| d.get());
    FIELD_BRUSH.with(|b| b.set(CreateSolidBrush(ui::cr(ui::FIELD)).0 as isize));
    store_plan(dpi);
    let (_, ctrls) = layout(dpi);
    for (id, r, ck) in &ctrls {
        match ck {
            Ck::Toggle => add_toggle(hwnd, *id, *r, dpi),
            Ck::Edit(multi) => add_edit(hwnd, *id, *r, *multi, dpi),
            Ck::Btn(text, def) => add_button(hwnd, *id, text, *r, *def),
        }
    }
    // font for child edits
    let font = ui::segoe(9, 400, dpi);
    for id in [IDC_GAP, IDC_HK_TOGGLE, IDC_HK_EDITOR, IDC_HK_PRESET, IDC_HK_ZONE, IDC_FSKEYS] {
        let c = ctrl(hwnd, id);
        if !c.0.is_null() {
            let _ = SendMessageW(c, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }
    }
    populate(hwnd);
}

/// Recompute geometry and move every child (DPI change / resize).
unsafe fn relayout(hwnd: HWND) {
    let dpi = DPI.with(|d| d.get());
    store_plan(dpi);
    let (_, ctrls) = layout(dpi);
    for (id, r, ck) in &ctrls {
        let c = ctrl(hwnd, *id);
        if c.0.is_null() {
            continue;
        }
        let (x, y, w, h) = ctrl_pos(r, ck, dpi);
        let _ = MoveWindow(c, x, y, w, h, true);
    }
}

fn populate(hwnd: HWND) {
    SHARED.with(|s| {
        if let Some(shared) = s.borrow().as_ref() {
            let cfg = shared.config.read().unwrap();
            set_checked(hwnd, IDC_ENABLED, cfg.enabled);
            set_checked(hwnd, IDC_SHIFT_SNAP, cfg.shift_snap);
            set_checked(hwnd, IDC_AUTOSTART, crate::autostart::is_enabled());
            set_checked(hwnd, IDC_LOG, cfg.log_enabled);
            set_text(hwnd, IDC_GAP, &cfg.zone_gap.to_string());
            set_text(hwnd, IDC_HK_TOGGLE, &cfg.hotkeys.toggle);
            set_text(hwnd, IDC_HK_EDITOR, &cfg.hotkeys.editor);
            set_text(hwnd, IDC_HK_PRESET, &cfg.hotkeys.next_preset);
            set_text(hwnd, IDC_HK_ZONE, &cfg.hotkeys.next_zone);
            let keys = cfg
                .fs_exit_keys
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            set_text(hwnd, IDC_FSKEYS, &keys);
        }
    });
}

/// Read controls, validate, write Config, persist, notify.
/// Returns true on success (window may close).
fn apply(hwnd: HWND) -> bool {
    SHARED.with(|s| {
        let Some(shared) = s.borrow().as_ref().cloned() else {
            return false;
        };
        let mut cfg = shared.config.read().unwrap().clone();

        let gap = match get_text(hwnd, IDC_GAP).trim().parse::<i32>() {
            Ok(v) => v,
            Err(_) => {
                err_box(hwnd, "Zone gap must be a number of pixels.");
                return false;
            }
        };
        let hk = [
            (IDC_HK_TOGGLE, "toggle"),
            (IDC_HK_EDITOR, "editor"),
            (IDC_HK_PRESET, "next preset"),
            (IDC_HK_ZONE, "next zone"),
        ]
        .map(|(id, name)| (name, get_text(hwnd, id)));
        for (name, v) in &hk {
            if crate::hotkeys::parse(v.trim()).is_none() {
                err_box(hwnd, &format!("'{v}' is not a valid hotkey for {name}."));
                return false;
            }
        }
        let mut keys = std::collections::BTreeMap::new();
        for (ln, line) in get_text(hwnd, IDC_FSKEYS).lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((exe, key)) = line.split_once('=') else {
                err_box(hwnd, &format!("Line {}: expected 'exe=key'.", ln + 1));
                return false;
            };
            if crate::hotkeys::vk_from_name(key.trim()).is_none() {
                err_box(hwnd, &format!("Line {}: unknown key '{}'.", ln + 1, key.trim()));
                return false;
            }
            keys.insert(exe.trim().to_string(), key.trim().to_string());
        }

        cfg.enabled = checked(hwnd, IDC_ENABLED);
        cfg.shift_snap = checked(hwnd, IDC_SHIFT_SNAP);
        cfg.log_enabled = checked(hwnd, IDC_LOG);
        cfg.zone_gap = gap;
        cfg.hotkeys.toggle = hk[0].1.trim().to_string();
        cfg.hotkeys.editor = hk[1].1.trim().to_string();
        cfg.hotkeys.next_preset = hk[2].1.trim().to_string();
        cfg.hotkeys.next_zone = hk[3].1.trim().to_string();
        cfg.fs_exit_keys = keys;

        let want_start = checked(hwnd, IDC_AUTOSTART);
        if want_start != crate::autostart::is_enabled() {
            match crate::autostart::set_enabled(want_start) {
                Ok(()) => cfg.autostart = want_start,
                Err(e) => {
                    err_box(hwnd, &format!("Autostart failed: {e}"));
                    return false;
                }
            }
        }
        if let Err(e) = cfg.save() {
            err_box(hwnd, &format!("Could not save config: {e}"));
            return false;
        }
        *shared.config.write().unwrap() = cfg;
        shared.bump();
        crate::log::set_enabled(checked(hwnd, IDC_LOG));
        crate::tray::send(UiCmd::RebindHotkeys);
        crate::log::line("settings saved");
        true
    })
}

pub fn is_open() -> bool {
    WIN.with(|w| w.get() != 0)
}

/// Open (or focus) the settings window.
pub fn toggle(shared: Arc<Shared>) {
    if let Some(h) = WIN.with(|w| {
        let v = w.get();
        if v != 0 { Some(HWND(v as *mut _)) } else { None }
    }) {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    SHARED.with(|s| *s.borrow_mut() = Some(shared));

    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hIcon: crate::icon::app_icon(32),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let twc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(toggle_proc),
            hInstance: hinst.into(),
            lpszClassName: TOGGLE_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&twc);

        let dpi = windows::Win32::UI::HiDpi::GetDpiForSystem();
        let wpx = ui::sx(W, dpi.max(96));
        let hpx = ui::sx(H, dpi.max(96));

        // real app window: native caption (drag/minimize/close),
        // fixed size, taskbar + Alt-Tab presence
        let style =
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE;
        let mut wr = RECT { left: 0, top: 0, right: wpx, bottom: hpx };
        let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
            &mut wr,
            style,
            false,
            WINDOW_EX_STYLE(0),
            dpi.max(96),
        );
        let win_w = wr.right - wr.left;
        let win_h = wr.bottom - wr.top;

        // center on the primary work area
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut wa as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let x = wa.left + (wa.right - wa.left - win_w) / 2;
        let y = wa.top + (wa.bottom - wa.top - win_h) / 2;

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("ZoneLock settings"),
            style,
            x.max(wa.left),
            y.max(wa.top),
            win_w,
            win_h,
            None,
            None,
            Some(hinst.into()),
            None,
        );
        if let Ok(hwnd) = hwnd {
            WIN.with(|w| w.set(hwnd.0 as isize));
            // dark immersive titlebar to match the theme
            let dark: i32 = 1;
            let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20), // DWMWA_USE_IMMERSIVE_DARK_MODE
                &dark as *const _ as *const _,
                4,
            );
            let _ = SetForegroundWindow(hwnd);
        }
    }
}
