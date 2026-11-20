//! Dev probe: list top-level windows, or evaluate one hwnd through the
//! ZoneLock engine. Usage:
//!   cargo run --example probe            -> list windows
//!   cargo run --example probe -- 0x1234  -> evaluate that hwnd

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW};

use zonelock::config::Config;
use zonelock::watcher::{self, Shared};
use zonelock::wininfo;

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    let arg = std::env::args().nth(1);

    let mut cfg = Config::default();
    let mons = zonelock::monitor::enumerate();
    for (i, m) in mons.iter().enumerate() {
        println!("mon{i}: {} rect={:?} work={:?} dpi={}", m.name, m.rect, m.work, m.dpi_x);
        cfg.layouts
            .set_for_monitor(&m.name, zonelock::zones::Layout::halves());
    }
    let shared = Shared::new(cfg);
    watcher::install_for_test(std::sync::Arc::clone(&shared));

    if let Some(a) = arg {
        if a == "--postf11" {
            // --postf11 <hwnd>: PostMessage WM_KEYDOWN/UP F11 (no focus steal)
            let hwnd_val = isize::from_str_radix(
                std::env::args().nth(2).expect("hwnd").trim_start_matches("0x"),
                16,
            )
            .or_else(|_| std::env::args().nth(2).unwrap().parse::<isize>())
            .expect("hwnd");
            let hwnd = HWND(hwnd_val as *mut _);
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    PostMessageW, WM_KEYDOWN, WM_KEYUP,
                };
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_KEYDOWN,
                    windows::Win32::Foundation::WPARAM(0x7A),
                    LPARAM(0x00380001),
                );
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_KEYUP,
                    windows::Win32::Foundation::WPARAM(0x7A),
                    LPARAM(0xC0380001),
                );
            }
            println!("posted WM_KEYDOWN/UP F11");
            return;
        }
        if a == "--max" || a == "--restore" || a == "--min" {
            // --max <hwnd> / --restore <hwnd> / --min <hwnd>:
            // SC_MAXIMIZE / SC_RESTORE / SC_MINIMIZE via WM_SYSCOMMAND
            let hwnd_val = isize::from_str_radix(
                std::env::args()
                    .nth(2)
                    .expect("hwnd")
                    .trim_start_matches("0x"),
                16,
            )
            .or_else(|_| std::env::args().nth(2).unwrap().parse::<isize>())
            .expect("hwnd");
            let hwnd = HWND(hwnd_val as *mut _);
            let sc: usize = match a.as_str() {
                "--max" => 0xF030,
                "--min" => 0xF020,
                _ => 0xF120,
            };
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    PostMessageW, WM_SYSCOMMAND,
                };
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_SYSCOMMAND,
                    windows::Win32::Foundation::WPARAM(sc),
                    LPARAM(0),
                );
            }
            println!("posted {a}");
            return;
        }
        if a == "--close" {
            // --close <hwnd>: graceful WM_CLOSE
            let hwnd_val = isize::from_str_radix(
                std::env::args()
                    .nth(2)
                    .expect("hwnd")
                    .trim_start_matches("0x"),
                16,
            )
            .or_else(|_| std::env::args().nth(2).unwrap().parse::<isize>())
            .expect("hwnd");
            let hwnd = HWND(hwnd_val as *mut _);
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_CLOSE,
                    windows::Win32::Foundation::WPARAM(0),
                    LPARAM(0),
                );
            }
            println!("posted WM_CLOSE");
            return;
        }
        if a == "--set" {
            // --set <hwnd> l t r b: one manual SetWindowPos
            let v: Vec<isize> = std::env::args()
                .skip(2)
                .map(|s| {
                    isize::from_str_radix(s.trim_start_matches("0x"), 16)
                        .or_else(|_| s.parse::<isize>())
                        .unwrap()
                })
                .collect();
            let hwnd = HWND(v[0] as *mut _);
            let r = zonelock::rect::Rect::new(
                v[1] as i32, v[2] as i32, v[3] as i32, v[4] as i32,
            );
            let ok = zonelock::place::apply(hwnd, &r);
            println!("apply -> {ok}");
            return;
        }
        if a == "--f11" {
            // --f11 <hwnd>: focus the window and send a real F11 keystroke
            let hwnd_val = isize::from_str_radix(
                std::env::args()
                    .nth(2)
                    .expect("hwnd")
                    .trim_start_matches("0x"),
                16,
            )
            .or_else(|_| std::env::args().nth(2).unwrap().parse::<isize>())
            .expect("hwnd: hex (0x...) or decimal");
            let hwnd = HWND(hwnd_val as *mut _);
            unsafe {
                use windows::Win32::UI::Input::KeyboardAndMouse::*;
                use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
                let _ = SetForegroundWindow(hwnd);
                std::thread::sleep(std::time::Duration::from_millis(250));
                let mk = |flags: KEYBD_EVENT_FLAGS| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_F11,
                            wScan: 0,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let inputs = [mk(KEYBD_EVENT_FLAGS(0)), mk(KEYEVENTF_KEYUP)];
                let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                println!("SendInput sent {sent} events");
            }
            return;
        }
        if a == "--drag" {
            // --drag <hwnd> <x> <y> [late|noshift]: real title-bar drag.
            // Default holds Shift from the start; "late" presses Shift
            // halfway through the move; "noshift" never presses it.
            let v: Vec<String> = std::env::args().skip(2).collect();
            let hwnd_val = isize::from_str_radix(
                v[0].trim_start_matches("0x"),
                16,
            )
            .or_else(|_| v[0].parse::<isize>())
            .expect("hwnd");
            let (tx, ty): (i32, i32) =
                (v[1].parse().unwrap(), v[2].parse().unwrap());
            let mode = v.get(3).map(|s| s.as_str()).unwrap_or("early");
            let hwnd = HWND(hwnd_val as *mut _);
            unsafe {
                use windows::Win32::Foundation::RECT;
                use windows::Win32::UI::Input::KeyboardAndMouse::*;
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetSystemMetrics, GetWindowRect, SetForegroundWindow,
                    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                    SM_YVIRTUALSCREEN,
                };
                let mut rc = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rc);
                let sx = (rc.left + rc.right) / 2;
                let sy = rc.top + 12;
                let _ = SetForegroundWindow(hwnd);
                std::thread::sleep(std::time::Duration::from_millis(200));
                // normalized absolute coords for SendInput
                let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                let norm = |x: i32, y: i32| -> (i32, i32) {
                    (
                        ((x - vx) * 65535) / (vw - 1).max(1),
                        ((y - vy) * 65535) / (vh - 1).max(1),
                    )
                };
                let kin = |up: bool| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_SHIFT,
                            wScan: 0,
                            dwFlags: if up {
                                KEYEVENTF_KEYUP
                            } else {
                                KEYBD_EVENT_FLAGS(0)
                            },
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let mabs = |x: i32, y: i32, f: MOUSE_EVENT_FLAGS| {
                    let (nx, ny) = norm(x, y);
                    INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: nx,
                                dy: ny,
                                mouseData: 0,
                                dwFlags: f | MOUSEEVENTF_ABSOLUTE
                                    | MOUSEEVENTF_MOVE
                                    | MOUSEEVENTF_VIRTUALDESK,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    }
                };
                let mbtn = |x: i32, y: i32, f: MOUSE_EVENT_FLAGS| {
                    let (nx, ny) = norm(x, y);
                    INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: nx,
                                dy: ny,
                                mouseData: 0,
                                dwFlags: f | MOUSEEVENTF_ABSOLUTE
                                    | MOUSEEVENTF_VIRTUALDESK,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    }
                };
                let min_down = |x: i32, y: i32| mbtn(x, y, MOUSEEVENTF_LEFTDOWN);
                let min_up = |x: i32, y: i32| mbtn(x, y, MOUSEEVENTF_LEFTUP);
                let sz = std::mem::size_of::<INPUT>() as i32;
                if mode != "noshift" && mode != "late" {
                    SendInput(&[kin(false)], sz);
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
                SendInput(&[mabs(sx, sy, MOUSE_EVENT_FLAGS(0))], sz);
                std::thread::sleep(std::time::Duration::from_millis(120));
                SendInput(
                    &[mabs(sx, sy, MOUSE_EVENT_FLAGS(0)),
                      min_down(sx, sy)],
                    sz,
                );
                std::thread::sleep(std::time::Duration::from_millis(150));
                for i in 1..=16 {
                    if mode == "late" && i == 8 {
                        SendInput(&[kin(false)], sz);
                        std::thread::sleep(std::time::Duration::from_millis(60));
                    }
                    let cx = sx + (tx - sx) * i / 16;
                    let cy = sy + (ty - sy) * i / 16;
                    SendInput(&[mabs(cx, cy, MOUSE_EVENT_FLAGS(0))], sz);
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                SendInput(&[min_up(tx, ty)], sz);
                std::thread::sleep(std::time::Duration::from_millis(150));
                if mode != "noshift" {
                    SendInput(&[kin(true)], sz);
                }
            }
            println!("drag done -> {tx},{ty}");
            return;
        }
        if a == "--altenter" {
            // --altenter <hwnd>: focus + real SendInput Alt+Enter
            let hwnd_val = isize::from_str_radix(
                std::env::args()
                    .nth(2)
                    .expect("hwnd")
                    .trim_start_matches("0x"),
                16,
            )
            .or_else(|_| std::env::args().nth(2).unwrap().parse::<isize>())
            .expect("hwnd");
            let hwnd = HWND(hwnd_val as *mut _);
            unsafe {
                use windows::Win32::UI::Input::KeyboardAndMouse::*;
                use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
                let _ = SetForegroundWindow(hwnd);
                std::thread::sleep(std::time::Duration::from_millis(300));
                let kin = |vk: VIRTUAL_KEY, up: bool| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: if up {
                                KEYEVENTF_KEYUP
                            } else {
                                KEYBD_EVENT_FLAGS(0)
                            },
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let inputs = [
                    kin(VK_MENU, false),
                    kin(VK_RETURN, false),
                    kin(VK_RETURN, true),
                    kin(VK_MENU, true),
                ];
                let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                println!("SendInput sent {sent} events");
            }
            return;
        }
        if a == "--postkey" {
            // --postkey <hwnd> <vk_hex> [lparam_hex]: generic WM_KEYDOWN/UP
            let v: Vec<String> = std::env::args().skip(2).collect();
            let hwnd_val = isize::from_str_radix(
                v[0].trim_start_matches("0x"),
                16,
            )
            .or_else(|_| v[0].parse::<isize>())
            .expect("hwnd");
            let vk = u32::from_str_radix(
                v[1].trim_start_matches("0x"),
                16,
            )
            .or_else(|_| v[1].parse::<u32>())
            .expect("vk");
            let lp: isize = v
                .get(2)
                .and_then(|s| {
                    isize::from_str_radix(s.trim_start_matches("0x"), 16)
                        .ok()
                        .or_else(|| s.parse().ok())
                })
                .unwrap_or(0x00000001);
            let hwnd = HWND(hwnd_val as *mut _);
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    PostMessageW, WM_KEYDOWN, WM_KEYUP,
                };
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_KEYDOWN,
                    windows::Win32::Foundation::WPARAM(vk as usize),
                    LPARAM(lp),
                );
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_KEYUP,
                    windows::Win32::Foundation::WPARAM(vk as usize),
                    LPARAM(lp | 0xC0000000),
                );
            }
            println!("posted vk={vk:#x} lp={lp:#x}");
            return;
        }
        if a == "--ovdrag" || a == "--ovkey" {
            // --ovdrag x1 y1 x2 y2: mouse drag on the zone editor overlay
            // --ovkey <vk>: key to the overlay (13=Enter commit, 27=Esc)
            use windows::core::w;
            use windows::Win32::UI::WindowsAndMessaging::{
                FindWindowW, PostMessageW, WM_KEYDOWN, WM_KEYUP,
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
            };
            let v: Vec<i32> = std::env::args()
                .skip(2)
                .map(|s| s.parse().unwrap())
                .collect();
            unsafe {
                let hwnd = FindWindowW(w!("ZoneLockOverlay"), w!("ZoneLock zones"))
                    .expect("overlay window not found");
                if a == "--ovkey" {
                    let vk = v[0] as usize;
                    let _ = PostMessageW(
                        Some(hwnd),
                        WM_KEYDOWN,
                        windows::Win32::Foundation::WPARAM(vk),
                        LPARAM(0),
                    );
                    let _ = PostMessageW(
                        Some(hwnd),
                        WM_KEYUP,
                        windows::Win32::Foundation::WPARAM(vk),
                        LPARAM(0),
                    );
                    println!("posted key {vk}");
                } else {
                    let (x1, y1, x2, y2) = (v[0], v[1], v[2], v[3]);
                    let lp = |x: i32, y: i32| LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
                    let _ = PostMessageW(
                        Some(hwnd),
                        WM_LBUTTONDOWN,
                        windows::Win32::Foundation::WPARAM(1),
                        lp(x1, y1),
                    );
                    for i in 1..=8 {
                        let cx = x1 + (x2 - x1) * i / 8;
                        let cy = y1 + (y2 - y1) * i / 8;
                        let _ = PostMessageW(
                            Some(hwnd),
                            WM_MOUSEMOVE,
                            windows::Win32::Foundation::WPARAM(1),
                            lp(cx, cy),
                        );
                        std::thread::sleep(std::time::Duration::from_millis(15));
                    }
                    let _ = PostMessageW(
                        Some(hwnd),
                        WM_LBUTTONUP,
                        windows::Win32::Foundation::WPARAM(0),
                        lp(x2, y2),
                    );
                    println!("ovdrag {x1},{y1} -> {x2},{y2}");
                }
            }
            return;
        }
        if a == "--traycmd" {
            // --traycmd <id>: post WM_COMMAND to the ZoneLock tray window
            let id: usize = std::env::args()
                .nth(2)
                .expect("id")
                .parse()
                .unwrap();
            unsafe {
                use windows::core::w;
                use windows::Win32::UI::WindowsAndMessaging::{
                    FindWindowW, PostMessageW, WM_COMMAND,
                };
                let hwnd =
                    FindWindowW(w!("ZoneLockTray"), w!("ZoneLock"));
                let hwnd = hwnd.expect("tray window not found");
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_COMMAND,
                    windows::Win32::Foundation::WPARAM(id),
                    LPARAM(0),
                );
                println!("posted WM_COMMAND {id} to tray");
            }
            return;
        }
        if a == "--watch" {
            // --watch <hwnd> <seconds>: poll-evaluate like the real watcher
            let hwnd_val = isize::from_str_radix(
                std::env::args()
                    .nth(2)
                    .expect("hwnd")
                    .trim_start_matches("0x"),
                16,
            )
            .or_else(|_| std::env::args().nth(2).unwrap().parse::<isize>())
            .expect("hwnd: hex (0x...) or decimal");
            let secs: u64 = std::env::args()
                .nth(3)
                .and_then(|s| s.parse().ok())
                .unwrap_or(15);
            let hwnd = HWND(hwnd_val as *mut _);
            let end = std::time::Instant::now()
                + std::time::Duration::from_secs(secs);
            let mut last = String::new();
            while std::time::Instant::now() < end {
                let action = watcher::evaluate(hwnd);
                let info = wininfo::inspect(hwnd);
                let line = format!(
                    "{action:?} rect={:?} zoomed={}",
                    info.rect, info.zoomed
                );
                if line != last {
                    println!("{line}");
                    last = line;
                }
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
            return;
        }
        let hwnd_val = isize::from_str_radix(a.trim_start_matches("0x"), 16)
            .or_else(|_| a.parse::<isize>())
            .expect("hwnd: hex (0x...) or decimal");
        let hwnd = HWND(hwnd_val as *mut _);
        let action = watcher::evaluate(hwnd);
        let info = wininfo::inspect(hwnd);
        println!("action={action:?}\nrect={:?} zoomed={} class={} exe={}",
            info.rect, info.zoomed, info.class, info.exe);
        return;
    }

    unsafe extern "system" fn cb(hwnd: HWND, _l: LPARAM) -> windows::core::BOOL {
        let mut t = [0u16; 256];
        let n = unsafe { GetWindowTextW(hwnd, &mut t) };
        let info = wininfo::inspect(hwnd);
        if info.visible && !info.rect.is_empty() {
            println!(
                "{:#x} [{:24}] {:24} rect={:?} zoomed={} \"{}\"",
                info.hwnd,
                info.exe,
                &info.class[..info.class.len().min(24)],
                info.rect,
                info.zoomed,
                String::from_utf16_lossy(&t[..n as usize])
            );
        }
        windows::core::BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }
}
