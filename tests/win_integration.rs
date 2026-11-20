//! Integration tests against real windows on the live desktop.
//! Each test installs the engine context on its own thread (the tracker is
//! thread-local by design) and exercises the same code path the WinEvent
//! watcher uses at runtime.

#![cfg(windows)]

use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

use zonelock::config::Config;
use zonelock::engine::Action;
use zonelock::monitor;
use zonelock::rect::{FracRect, Rect};
use zonelock::watcher::{self, Shared};
use zonelock::wininfo;
use zonelock::zones::{Layout, Zone};

// Windows tests touch the real desktop; serialize them.
static SERIAL: Mutex<()> = Mutex::new(());

const TEST_CLASS: PCWSTR = w!("ZoneLockTestWindow");

unsafe extern "system" fn test_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_DESTROY {
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn ensure_class() {
    static ONCE: Mutex<()> = Mutex::new(());
    let _g = ONCE.lock().unwrap();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(test_wndproc),
            hInstance: hinst.into(),
            lpszClassName: TEST_CLASS,
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    }
}

fn make_window(rect: &Rect) -> HWND {
    ensure_class();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            TEST_CLASS,
            w!("zl-test"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("CreateWindowExW failed")
    }
}

fn pump(ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    unsafe {
        let mut msg = MSG::default();
        while std::time::Instant::now() < deadline {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

fn window_rect(hwnd: HWND) -> Rect {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        Rect::new(rc.left, rc.top, rc.right, rc.bottom)
    }
}

/// Visible bounds (DWM extended frame) — what the user actually sees.
/// Zone placement compensates the invisible border, so assertions on
/// "snapped to zone" must compare the visible rect, not GetWindowRect.
fn visible_rect(hwnd: HWND) -> Rect {
    unsafe {
        let mut rc = RECT::default();
        let ok = windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rc as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        );
        if ok.is_ok() {
            Rect::new(rc.left, rc.top, rc.right, rc.bottom)
        } else {
            window_rect(hwnd)
        }
    }
}

/// Shared with a two-zone layout installed on every monitor.
fn test_shared() -> (std::sync::Arc<Shared>, monitor::Monitor) {
    let mut cfg = Config::default();
    cfg.log_enabled = false;
    let mons = monitor::enumerate();
    assert!(!mons.is_empty(), "no monitors - needs a desktop session");
    let mon = mons
        .iter()
        .find(|m| m.primary)
        .cloned()
        .unwrap_or_else(|| mons[0].clone());
    let layout = Layout {
        name: "test-halves".into(),
        zones: vec![
            Zone {
                name: "left".into(),
                rect: FracRect::new(0.0, 0.0, 0.5, 1.0),
            },
            Zone {
                name: "right".into(),
                rect: FracRect::new(0.5, 0.0, 1.0, 1.0),
            },
        ],
    };
    for m in &mons {
        cfg.layouts.set_for_monitor(&m.name, layout.clone());
    }
    let shared = Shared::new(cfg);
    *shared.monitors.write().unwrap() = mons;
    watcher::install_for_test(std::sync::Arc::clone(&shared));
    (shared, mon)
}

/// Small rect inside the left zone of `mon`.
fn normal_spot(mon: &monitor::Monitor) -> Rect {
    let z = FracRect::new(0.0, 0.0, 0.5, 1.0).to_pixels(&mon.work, 0);
    Rect::new(z.left + 60, z.top + 60, z.left + 460, z.top + 360)
}

#[test]
fn maximize_is_contained_to_zone() {
    let _g = SERIAL.lock().unwrap();
    let (_shared, mon) = test_shared();
    let spot = normal_spot(&mon);
    let hwnd = make_window(&spot);
    pump(150);

    // first evaluation: normal window gets tracked
    let a1 = watcher::evaluate(hwnd);
    assert!(matches!(a1, Action::Track(_)), "expected Track, got {a1:?}");

    // maximize it
    unsafe {
        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
    }
    pump(200);
    let a2 = watcher::evaluate(hwnd);
    assert!(matches!(a2, Action::ContainMaximize(_)), "expected ContainMaximize, got {a2:?}");

    // final geometry = left zone (visible bounds - zone placement
    // compensates the invisible DWM frame)
    let want = FracRect::new(0.0, 0.0, 0.5, 1.0).to_pixels(&mon.work, 0);
    let got = visible_rect(hwnd);
    assert!(got.approx_eq(&want, 4), "rect {got:?} != zone {want:?}");
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

#[test]
fn f11_style_fullcover_is_contained() {
    let _g = SERIAL.lock().unwrap();
    let (_shared, mon) = test_shared();
    let spot = normal_spot(&mon);
    let hwnd = make_window(&spot);
    pump(150);
    assert!(matches!(watcher::evaluate(hwnd), Action::Track(_)));

    // a FLOATING window going fullscreen gets the whole display
    let cover = |h| unsafe {
        SetWindowPos(
            h,
            None,
            mon.rect.left,
            mon.rect.top,
            mon.rect.width(),
            mon.rect.height(),
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
        .unwrap();
    };
    cover(hwnd);
    pump(150);
    let a = watcher::evaluate(hwnd);
    assert_eq!(a, Action::Ignore, "floating window: real fullscreen");
    unsafe {
        let _ = DestroyWindow(hwnd);
    }

    // a window SNAPPED in its zone keeps its fullscreen there
    let hwnd = make_window(&spot);
    pump(150);
    let zone = FracRect::new(0.0, 0.0, 0.5, 1.0).to_pixels(&mon.work, 0);
    assert!(zonelock::place::apply_zone(hwnd, &zone));
    pump(150);
    assert!(matches!(watcher::evaluate(hwnd), Action::Track(_)));

    // simulate F11: borderless covering of the whole monitor, not zoomed
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            mon.rect.left,
            mon.rect.top,
            mon.rect.width(),
            mon.rect.height(),
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
        .unwrap();
    }
    pump(150);
    let a = watcher::evaluate(hwnd);
    // no fs-exit key configured for the test exe -> cooperative move
    assert!(matches!(a, Action::ContainFsMove(_)), "expected ContainFsMove, got {a:?}");
    let want = FracRect::new(0.0, 0.0, 0.5, 1.0).to_pixels(&mon.work, 0);
    assert!(visible_rect(hwnd).approx_eq(&want, 4));
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

#[test]
fn born_fullscreen_window_is_ignored() {
    let _g = SERIAL.lock().unwrap();
    let (_shared, mon) = test_shared();
    // window created already covering the monitor (like an exclusive
    // fullscreen game): engine must stay passive
    let hwnd = make_window(&mon.rect);
    pump(150);
    let a = watcher::evaluate(hwnd);
    assert_eq!(a, Action::Ignore);
    // and stays passive on re-evaluation
    let a2 = watcher::evaluate(hwnd);
    assert_eq!(a2, Action::Nothing);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

#[test]
fn restore_after_lock_returns_to_normal() {
    let _g = SERIAL.lock().unwrap();
    let (_shared, mon) = test_shared();
    let spot = normal_spot(&mon);
    let hwnd = make_window(&spot);
    pump(150);
    watcher::evaluate(hwnd);
    unsafe {
        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
    }
    pump(200);
    assert!(matches!(watcher::evaluate(hwnd), Action::ContainMaximize(_)));

    // user restores to a normal rect: engine releases the lock
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        SetWindowPos(
            hwnd,
            None,
            spot.left,
            spot.top,
            spot.width(),
            spot.height(),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .unwrap();
    }
    pump(150);
    let a = watcher::evaluate(hwnd);
    assert!(matches!(a, Action::Track(_)), "expected Track, got {a:?}");
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// End-to-end proof for P0: the real WinEvent hook fires on maximize and the
/// engine contains the window without any manual re-evaluation.
#[test]
fn winevent_hook_catches_real_maximize() {
    let _g = SERIAL.lock().unwrap();
    let (_shared, mon) = test_shared();
    let spot = normal_spot(&mon);
    let hwnd = make_window(&spot);
    pump(150);
    assert!(matches!(watcher::evaluate(hwnd), Action::Track(_)));

    unsafe extern "system" fn noop_hook(
        _h: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_obj: i32,
        id_child: i32,
        _t: u32,
        _tm: u32,
    ) {
        if id_obj == 0 && id_child == 0 && event == 0x800B {
            watcher::evaluate(hwnd);
        }
    }

    unsafe {
        let hook = SetWinEventHook(
            0x800B,
            0x800B,
            None,
            Some(noop_hook),
            0,
            0,
            // note: no SKIPOWNPROCESS here - the test window belongs to
            // this test process; production correctly skips only its own
            // overlay/tray windows.
            WINEVENT_OUTOFCONTEXT,
        );
        assert!(!hook.0.is_null(), "SetWinEventHook failed");

        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        pump(600); // let the out-of-context event arrive

        let want = FracRect::new(0.0, 0.0, 0.5, 1.0).to_pixels(&mon.work, 0);
        let got = visible_rect(hwnd);
        let _ = UnhookWinEvent(hook);
        let _ = DestroyWindow(hwnd);
        assert!(
            got.approx_eq(&want, 4),
            "WinEvent path did not contain: {got:?} != {want:?}"
        );
    }
}

#[test]
fn exclusions_are_respected() {
    let _g = SERIAL.lock().unwrap();
    let (shared, _mon) = test_shared();
    // exclude our own test window class via config prefixes
    shared
        .config
        .write()
        .unwrap()
        .exclude_class_prefixes
        .push("ZoneLockTest".into());
    let hwnd = make_window(&normal_spot(&shared.monitors.read().unwrap()[0]));
    pump(100);
    let a = watcher::evaluate(hwnd);
    assert_eq!(a, Action::Forget);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

#[test]
fn windowinfo_sanity() {
    let _g = SERIAL.lock().unwrap();
    let (_shared, mon) = test_shared();
    let hwnd = make_window(&normal_spot(&mon));
    pump(100);
    let info = wininfo::inspect(hwnd);
    assert!(info.valid);
    assert!(info.visible);
    assert_eq!(info.class, "ZoneLockTestWindow");
    assert!(!info.exe.is_empty());
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

