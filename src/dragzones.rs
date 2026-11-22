//! Drag-snap overlay (FancyZones-style). One click-through translucent
//! window per monitor shows the zone outlines while the user drags a
//! window with the configured modifier held (`hotkeys.drag_snap`, Shift
//! by default; empty = no key needed, every drag shows the zones); the
//! zone under the cursor is highlighted and, on drop, the window is
//! placed there.
//!
//! Lives entirely on the watcher thread: that thread already owns a
//! message loop and our own-process WinEvents are filtered out, so these
//! windows never feed back into the engine. `WS_EX_TRANSPARENT` makes
//! them click-through so the drag itself is never disturbed.
//!
//! The modifier may be held before the drag starts OR pressed mid-drag:
//! the drag is always tracked and the overlay opens/closes live with its
//! state until MOVESIZEEND. Drop snaps only while the overlay is
//! visible.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::monitor::Monitor;
use crate::rect::Rect;
use crate::uidraw as ui;
use crate::zones::{Layout, LayoutSet};

const CLASS: PCWSTR = w!("ZoneLockDragZones");
const ALPHA: u8 = 150; // backdrop dim

struct DragWin {
    hwnd: HWND,
    mon: Monitor,
    layout: Layout,
    hot: Option<usize>,
}

thread_local! {
    static WINS: RefCell<Vec<DragWin>> = const { RefCell::new(Vec::new()) };
    /// Monitor+layout snapshot taken at MOVESIZESTART, consumed lazily
    /// when the trigger first goes down.
    static PENDING: RefCell<Vec<(Monitor, Layout)>> = const { RefCell::new(Vec::new()) };
    /// Modifier set that opens the overlay for the current drag
    /// (None = no key needed, the overlay follows every drag).
    static TRIGGER: Cell<Option<HOT_KEY_MODIFIERS>> = const { Cell::new(None) };
    static DRAGGED: Cell<isize> = const { Cell::new(0) };
}

/// Overlay windows are up.
pub fn active() -> bool {
    WINS.with(|w| !w.borrow().is_empty())
}

/// A window drag is being tracked (overlay may be hidden).
pub fn dragging() -> bool {
    DRAGGED.with(|d| d.get() != 0)
}

/// Hwnd currently being dragged, if any.
pub fn dragged() -> isize {
    DRAGGED.with(|d| d.get())
}

/// True while the trigger condition is met: every configured modifier
/// held, or always when no key is configured.
pub fn trigger_held() -> bool {
    TRIGGER.with(|t| match t.get() {
        None => true,
        Some(m) => crate::hotkeys::mods_held(m),
    })
}

/// Pure hit-test: zone pixel-rect under (x, y), or None.
pub fn target_at(layout: &Layout, mon: &Rect, x: i32, y: i32) -> Option<Rect> {
    layout
        .zone_at_point(mon, x, y)
        .and_then(|i| layout.zone_frac(i))
        .map(|f| f.to_pixels(mon, 0))
}

/// Track a window drag. The overlay itself appears as soon as the
/// trigger is held, now or mid-drag.
pub fn begin(
    hwnd: isize,
    mons: &[Monitor],
    layouts: &LayoutSet,
    trigger: Option<HOT_KEY_MODIFIERS>,
) {
    end();
    DRAGGED.with(|d| d.set(hwnd));
    TRIGGER.with(|t| t.set(trigger));
    PENDING.with(|p| {
        *p.borrow_mut() = mons
            .iter()
            .filter_map(|m| {
                layouts
                    .for_monitor(&m.name, m.index)
                    .cloned()
                    .map(|l| (m.clone(), l))
            })
            .collect();
    });
    if trigger_held() {
        open_now();
    }
}

fn open_now() {
    if active() {
        return;
    }
    let set = PENDING.with(|p| p.borrow().clone());
    if set.is_empty() {
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        for (mon, layout) in set {
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW,
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
            WINS.with(|w| {
                w.borrow_mut().push(DragWin {
                    hwnd,
                    mon: mon.clone(),
                    layout,
                    hot: None,
                })
            });
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    if active() {
        crate::log::line(format!(
            "drag-overlay open hwnd={:#x}",
            dragged()
        ));
        update_hot();
    }
}

fn close_now() {
    let hwnds: Vec<HWND> =
        WINS.with(|w| w.borrow_mut().drain(..).map(|d| d.hwnd).collect());
    for h in hwnds {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

/// Called on the fast watcher tick while a drag is tracked: shows /
/// hides the overlay with the live trigger state and refreshes the hot
/// zone.
pub fn poll() {
    if !dragging() {
        return;
    }
    if trigger_held() {
        if !active() {
            open_now();
        } else {
            update_hot();
        }
    } else if active() {
        close_now();
        crate::log::line("drag-overlay hidden (trigger released)");
    }
}

fn update_hot() {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    WINS.with(|w| {
        for dw in w.borrow_mut().iter_mut() {
            let hot = dw.layout.zone_at_point(&dw.mon.work, pt.x, pt.y);
            if hot != dw.hot {
                dw.hot = hot;
                unsafe {
                    let _ = InvalidateRect(Some(dw.hwnd), None, false);
                }
            }
        }
    });
}

/// Zone rect under the cursor right now (None when the overlay is
/// hidden, so a drop without Shift is left alone).
pub fn target_at_cursor() -> Option<Rect> {
    if !active() {
        return None;
    }
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    WINS.with(|w| {
        w.borrow().iter().find_map(|dw| {
            target_at(&dw.layout, &dw.mon.work, pt.x, pt.y)
        })
    })
}

/// Stop tracking the drag and destroy any overlay windows.
pub fn end() {
    DRAGGED.with(|d| d.set(0));
    TRIGGER.with(|t| t.set(None));
    PENDING.with(|p| p.borrow_mut().clear());
    close_now();
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let (mw, mh) = WINS
                .with(|w| {
                    w.borrow()
                        .iter()
                        .find(|d| d.hwnd == hwnd)
                        .map(|d| (d.mon.rect.width(), d.mon.rect.height()))
                })
                .unwrap_or((0, 0));
            ui::buffered_paint(hwnd, mw, mh, |hdc| paint(hwnd, hdc));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint(hwnd: HWND, hdc: HDC) {
    WINS.with(|w| {
        let map = w.borrow();
        let Some(dw) = map.iter().find(|d| d.hwnd == hwnd) else {
            return;
        };
        let mon = dw.mon.rect;
        let dpi = dw.mon.dpi_x.max(96);
        let rc = RECT {
            left: 0,
            top: 0,
            right: mon.width(),
            bottom: mon.height(),
        };
        ui::fill(hdc, &rc, ui::BG);

        SetBkMode(hdc, TRANSPARENT);
        let font = ui::segoe(15, 700, dpi);
        let mut g = ui::GdiGuard::new(hdc);
        g.font(font);

        for (i, z) in dw.layout.zones.iter().enumerate() {
            let p = z.rect.to_pixels(&dw.mon.work, 0);
            let zr = RECT {
                left: p.left - mon.left + ui::sx(4, dpi),
                top: p.top - mon.top + ui::sx(4, dpi),
                right: p.right - mon.left - ui::sx(4, dpi),
                bottom: p.bottom - mon.top - ui::sx(4, dpi),
            };
            let hot = dw.hot == Some(i);
            let (fill_a, edge_a, pen_w) =
                if hot { (72, 255, 2) } else { (26, 120, 1) };
            ui::round_box(
                hdc,
                &zr,
                ui::sx(10, dpi),
                ui::blend(ui::ACCENT, ui::BG, fill_a),
                Some((ui::blend(ui::ACCENT, ui::BG, edge_a), ui::sx(pen_w, dpi))),
            );
            if hot {
                // soft outer glow: a second, fainter outline around it
                let grow = RECT {
                    left: zr.left - ui::sx(3, dpi),
                    top: zr.top - ui::sx(3, dpi),
                    right: zr.right + ui::sx(3, dpi),
                    bottom: zr.bottom + ui::sx(3, dpi),
                };
                ui::round_box(
                    hdc,
                    &grow,
                    ui::sx(13, dpi),
                    ui::blend(ui::ACCENT, ui::BG, 0),
                    Some((ui::blend(ui::ACCENT, ui::BG, 90), 1)),
                );
            }

            // number badge centered in the zone
            let cx = (zr.left + zr.right) / 2;
            let cy = (zr.top + zr.bottom) / 2;
            let rad = ui::sx(16, dpi);
            let badge_bg = if hot { ui::ACCENT } else { ui::blend(ui::ACCENT, ui::BG, 200) };
            ui::dot(hdc, cx, cy, rad, badge_bg);
            SetTextColor(hdc, ui::cr(if hot { ui::BG } else { ui::TXT }));
            let label = format!("{}", i + 1);
            let mut br = RECT {
                left: cx - rad,
                top: cy - rad,
                right: cx + rad,
                bottom: cy + rad,
            };
            ui::text(hdc, &mut br, &label, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }

        // hint pill, bottom center
        let hint = "drop to snap into the highlighted zone";
        let hint_font = ui::segoe(10, 400, dpi);
        SelectObject(hdc, hint_font.into());
        SetTextColor(hdc, ui::cr(ui::MUTED));
        let tw = ui::text_w(hdc, hint);
        let th = ui::sx(30, dpi);
        let hx = (mon.width() - tw) / 2;
        let hy = mon.height() - th - ui::sx(24, dpi);
        let mut hr = RECT {
            left: hx - ui::sx(14, dpi),
            top: hy,
            right: hx + tw + ui::sx(14, dpi),
            bottom: hy + th,
        };
        ui::round_box(hdc, &hr, ui::sx(15, dpi), ui::blend(ui::CARD, ui::BG, 220), Some((ui::EDGE, 1)));
        ui::text(hdc, &mut hr, hint, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        let _ = DeleteObject(hint_font.into());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rect::FracRect;
    use crate::zones::Zone;

    fn halves() -> (Layout, Rect) {
        let mon = Rect::new(0, 0, 1920, 1080);
        let l = Layout {
            name: "halves".into(),
            zones: vec![
                Zone {
                    name: "left".into(),
                    rect: FracRect {
                        left: 0.0,
                        top: 0.0,
                        right: 0.5,
                        bottom: 1.0,
                    },
                },
                Zone {
                    name: "right".into(),
                    rect: FracRect {
                        left: 0.5,
                        top: 0.0,
                        right: 1.0,
                        bottom: 1.0,
                    },
                },
            ],
        };
        (l, mon)
    }

    #[test]
    fn target_hit_test() {
        let (l, mon) = halves();
        assert_eq!(
            target_at(&l, &mon, 100, 500),
            Some(Rect::new(0, 0, 960, 1080))
        );
        assert_eq!(
            target_at(&l, &mon, 1500, 500),
            Some(Rect::new(960, 0, 1920, 1080))
        );
        assert_eq!(target_at(&l, &mon, 5000, 500), None);
    }
}
