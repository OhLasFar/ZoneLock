//! Plans: numbered window arrangements (1-9), like light-weight
//! desktops. Saving a plan records every visible app window (program,
//! where it sits as a fraction of its display's work area, z-order).
//! Going to a plan brings those windows back where they were, launches
//! the programs that are not running (and places their window when it
//! shows up), and minimizes every other app window.
//!
//! Plans are stored in config.json by program, so they survive reboots.
//! The matching logic is pure and unit-tested; the Win32 side lives in
//! `win` below and only runs on the UI thread (hotkeys / settings panel),
//! except `claim_new_window`, called by the watcher when a window shows.

use serde::{Deserialize, Serialize};

use crate::rect::{FracRect, Rect};

pub const SLOTS: std::ops::RangeInclusive<u8> = 1..=9;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanWindow {
    /// Executable file name ("chrome.exe"): the matching key.
    pub exe: String,
    /// Full image path, used to relaunch a desktop app.
    #[serde(default)]
    pub path: String,
    /// AppUserModelID of a packaged (Store) app: relaunched through
    /// shell:AppsFolder, its exe path is not launchable directly.
    #[serde(default)]
    pub aumid: String,
    #[serde(default)]
    pub class: String,
    /// Title at save time: only a tie-breaker between windows of the
    /// same program.
    #[serde(default)]
    pub title: String,
    /// Display device name, with its left-to-right index as fallback.
    pub monitor: String,
    #[serde(default)]
    pub monitor_index: usize,
    /// Visible frame as a fraction of that display's work area.
    pub rect: FracRect,
}

/// Windows top of the z-order first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub windows: Vec<PlanWindow>,
}

/// An app window that exists right now.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWin {
    pub hwnd: isize,
    pub exe: String,
    pub class: String,
    pub title: String,
}

/// What going to a plan has to do.
#[derive(Debug, Default, PartialEq)]
pub struct Assignment {
    /// (index in plan.windows, live hwnd), in plan order (top first).
    pub matched: Vec<(usize, isize)>,
    /// plan.windows indexes with no live window: launch these.
    pub missing: Vec<usize>,
    /// live windows that are not part of the plan: minimize these.
    pub others: Vec<isize>,
}

/// Pair plan entries with live windows. Same program is required; same
/// class is preferred, then the same title; each live window is used
/// once. Pure.
pub fn assign(plan: &Plan, live: &[LiveWin]) -> Assignment {
    let mut used = vec![false; live.len()];
    let mut out = Assignment::default();
    for (i, pw) in plan.windows.iter().enumerate() {
        let score = |lw: &LiveWin| -> Option<u8> {
            if !lw.exe.eq_ignore_ascii_case(&pw.exe) {
                return None;
            }
            let class = !pw.class.is_empty() && lw.class == pw.class;
            let title = !pw.title.is_empty() && lw.title == pw.title;
            Some(class as u8 * 2 + title as u8)
        };
        let best = live
            .iter()
            .enumerate()
            .filter(|(j, _)| !used[*j])
            .filter_map(|(j, lw)| score(lw).map(|s| (j, s)))
            .max_by_key(|(j, s)| (*s, std::cmp::Reverse(*j)));
        match best {
            Some((j, _)) => {
                used[j] = true;
                out.matched.push((i, live[j].hwnd));
            }
            None => out.missing.push(i),
        }
    }
    out.others = live
        .iter()
        .enumerate()
        .filter(|(j, _)| !used[*j])
        .map(|(_, lw)| lw.hwnd)
        .collect();
    out
}

/// Absolute visible rect of a plan window on the current displays:
/// same display by name, else by index, else the primary one.
pub fn target_rect(pw: &PlanWindow, monitors: &[crate::monitor::Monitor]) -> Option<Rect> {
    // the remembered display if it is still connected, otherwise the
    // primary one (never "whatever now sits at the same index": with a
    // display unplugged that is a different screen). The index is only
    // used by entries that carry no display name.
    let named = !pw.monitor.is_empty();
    let mon = monitors
        .iter()
        .find(|m| named && m.name == pw.monitor)
        .or_else(|| {
            (!named)
                .then(|| monitors.iter().find(|m| m.index == pw.monitor_index))
                .flatten()
        })
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())?;
    Some(pw.rect.to_pixels(&mon.work, 0))
}

/// What a "save plan N" key press does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStep {
    /// Write the plan into N now.
    Commit,
    /// N already holds a plan: show the overwrite prompt first.
    AskOverwrite,
}

/// `pending` = the slot the overwrite prompt is currently asking about.
/// Pressing the same slot again confirms; any other slot saves there if
/// it is free, or asks again if it is taken too. Pure.
pub fn save_step(pending: Option<u8>, slot: u8, slot_taken: bool) -> SaveStep {
    if pending == Some(slot) || !slot_taken {
        SaveStep::Commit
    } else {
        SaveStep::AskOverwrite
    }
}

/// Slots 1-9 with no plan yet.
pub fn free_slots(cfg: &crate::config::Config) -> Vec<u8> {
    SLOTS.filter(|n| !cfg.plans.contains_key(&n.to_string())).collect()
}

/// Program names of a plan, once each, without ".exe" ("Discord, Chrome").
pub fn app_names(plan: &Plan) -> String {
    let mut seen: Vec<String> = Vec::new();
    for w in &plan.windows {
        let n = w.exe.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
        if !seen.iter().any(|s| s.eq_ignore_ascii_case(&n)) {
            seen.push(n);
        }
    }
    seen.join(", ")
}

pub use win::{claim_new_window, go, save_current};
pub(crate) use win::{app_windows, is_app_window, visible_rect};

mod win {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    use super::{assign, target_rect, LiveWin, Plan, PlanWindow};
    use crate::config::Config;
    use crate::rect::{FracRect, Rect};
    use crate::watcher::Shared;
    use crate::wininfo::{self, WindowInfo};
    use crate::{log, place};

    /// A program we launched for a plan: its next windows go here.
    struct Pending {
        exe: String,
        target: Rect,
        until: Instant,
    }

    static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
    /// How long after a launch a new window of that program is placed.
    const LAUNCH_WINDOW: Duration = Duration::from_secs(20);

    pub(crate) fn window_text(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    /// Real app windows: what you see in Alt+Tab, ours and excluded
    /// programs left out. Minimized ones count (they can be restored).
    pub(crate) fn is_app_window(hwnd: HWND, info: &WindowInfo, cfg: &Config) -> bool {
        unsafe {
            info.valid
                && info.visible
                && !info.cloaked
                && info.pid != GetCurrentProcessId()
                && !info.exe.is_empty()
                && (info.ex_style & WS_EX_TOOLWINDOW.0 as i64) == 0
                && (info.style & WS_CHILD.0 as i64) == 0
                && GetWindow(hwnd, GW_OWNER).map(|o| o.is_invalid()).unwrap_or(true)
                && !window_text(hwnd).is_empty()
                && !wininfo::BUILTIN_CLASS_DENYLIST
                    .iter()
                    .any(|c| info.class.starts_with(c))
                && !cfg.is_excluded_process(&info.exe)
                && !cfg.is_excluded_class(&info.class)
        }
    }

    /// App windows in z-order, top first.
    pub(crate) fn app_windows(cfg: &Config) -> Vec<(HWND, WindowInfo)> {
        unsafe extern "system" fn cb(hwnd: HWND, l: LPARAM) -> windows::core::BOOL {
            let v = &mut *(l.0 as *mut Vec<HWND>);
            v.push(hwnd);
            windows::core::BOOL(1)
        }
        let mut all: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
        }
        all.into_iter()
            .map(|h| (h, wininfo::inspect(h)))
            .filter(|(h, i)| is_app_window(*h, i, cfg))
            .collect()
    }

    pub(crate) fn visible_rect(hwnd: HWND, fallback: Rect) -> Rect {
        let mut r = RECT::default();
        let ok = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut r as *mut _ as *mut _,
                std::mem::size_of::<RECT>() as u32,
            )
        }
        .is_ok();
        if ok {
            Rect::new(r.left, r.top, r.right, r.bottom)
        } else {
            fallback
        }
    }

    /// Full image path and AppUserModelID (packaged apps) of a process.
    fn launch_info(pid: u32) -> (String, String) {
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return (String::new(), String::new());
            };
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let path = if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok()
            {
                String::from_utf16_lossy(&buf[..len as usize])
            } else {
                String::new()
            };
            let mut abuf = vec![0u16; 256];
            let mut alen = abuf.len() as u32;
            let aumid = if GetApplicationUserModelId(h, &mut alen, Some(PWSTR(abuf.as_mut_ptr()))).0 == 0 {
                String::from_utf16_lossy(&abuf[..alen.saturating_sub(1) as usize])
            } else {
                String::new()
            };
            let _ = CloseHandle(h);
            (path, aumid)
        }
    }

    /// Record the visible (not minimized) app windows as plan `slot`.
    pub fn save_current(shared: &Shared, slot: u8) -> Result<Plan, String> {
        let cfg = shared.config.read().unwrap().clone();
        let mons = shared.monitors.read().unwrap().clone();
        let mut plan = Plan::default();
        for (h, info) in app_windows(&cfg) {
            if info.iconic {
                continue;
            }
            let vis = visible_rect(h, info.rect);
            let Some(mon) = crate::monitor::monitor_at(&vis, &mons) else { continue };
            let (path, aumid) = launch_info(info.pid);
            plan.windows.push(PlanWindow {
                exe: info.exe.clone(),
                path,
                aumid,
                class: info.class.clone(),
                title: window_text(h),
                monitor: mon.name.clone(),
                monitor_index: mon.index,
                rect: FracRect::from_pixels(&vis, &mon.work),
            });
        }
        log::line(format!(
            "plan {slot} captured: {}",
            plan.windows.iter().map(|w| w.exe.as_str()).collect::<Vec<_>>().join(", ")
        ));
        Ok(plan)
    }

    fn launch(pw: &PlanWindow) -> bool {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let ok = unsafe {
            if !pw.aumid.is_empty() {
                let arg = wide(&format!("shell:AppsFolder\\{}", pw.aumid));
                ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(arg.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL)
            } else if !pw.path.is_empty() {
                let p = wide(&pw.path);
                ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
            } else {
                return false;
            }
        };
        ok.0 as isize > 32
    }

    /// Go to plan `slot`: restore and place its windows (top of the plan
    /// ends up in front and focused), launch missing programs, minimize
    /// every other app window. User-initiated (hotkey / panel), so it is
    /// the one place ZoneLock deliberately brings a window to the front.
    pub fn go(shared: &Shared, slot: u8) -> Result<(), String> {
        let cfg = shared.config.read().unwrap().clone();
        let Some(plan) = cfg.plans.get(&slot.to_string()).cloned() else {
            return Err(format!("Plan {slot} is empty."));
        };
        let mons = shared.monitors.read().unwrap().clone();
        let wins = app_windows(&cfg);
        let live: Vec<LiveWin> = wins
            .iter()
            .map(|(h, i)| LiveWin {
                hwnd: h.0 as isize,
                exe: i.exe.clone(),
                class: i.class.clone(),
                title: window_text(*h),
            })
            .collect();
        let a = assign(&plan, &live);
        let iconic = |h: isize| wins.iter().any(|(w, i)| w.0 as isize == h && i.iconic);

        // others out of the way first (async: a hung app cannot block us)
        for &h in &a.others {
            if !iconic(h) {
                unsafe {
                    let _ = ShowWindowAsync(HWND(h as *mut _), SW_SHOWMINNOACTIVE);
                }
            }
        }
        // plan windows: bottom of the stack first, so the top one ends last
        for &(i, h) in a.matched.iter().rev() {
            let hwnd = HWND(h as *mut _);
            unsafe {
                if iconic(h) {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
                if let Some(t) = target_rect(&plan.windows[i], &mons) {
                    place::apply_zone(hwnd, &t);
                }
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
        if let Some(&(_, top)) = a.matched.first() {
            unsafe {
                let _ = SetForegroundWindow(HWND(top as *mut _));
            }
        }
        // not running: launch, the watcher places the window when it shows
        let mut launched = Vec::new();
        for &i in &a.missing {
            let pw = &plan.windows[i];
            let Some(target) = target_rect(pw, &mons) else { continue };
            if launch(pw) {
                PENDING.lock().unwrap().push(Pending {
                    exe: pw.exe.to_ascii_lowercase(),
                    target,
                    until: Instant::now() + LAUNCH_WINDOW,
                });
                launched.push(pw.exe.as_str());
            } else {
                log::line(format!("plan {slot}: cannot launch {}", pw.exe));
            }
        }
        log::line(format!(
            "plan {slot}: placed {}, launched [{}], minimized {}",
            a.matched.len(),
            launched.join(", "),
            a.others.len()
        ));
        Ok(())
    }

    /// Called by the watcher when a window shows: if it belongs to a
    /// program a plan just launched, put it where the plan wants it.
    /// One placement per launch entry. Returns true when it placed it.
    pub fn claim_new_window(hwnd: HWND) -> bool {
        {
            let mut p = PENDING.lock().unwrap();
            let now = Instant::now();
            p.retain(|x| x.until > now);
            if p.is_empty() {
                return false;
            }
        }
        let info = wininfo::inspect(hwnd);
        if !info.visible || info.iconic || window_text(hwnd).is_empty() {
            return false;
        }
        let exe = info.exe.to_ascii_lowercase();
        let target = {
            let mut p = PENDING.lock().unwrap();
            match p.iter().position(|x| x.exe == exe) {
                Some(i) => p.remove(i).target,
                None => return false,
            }
        };
        let ok = place::apply_zone(hwnd, &target);
        log::line(format!(
            "plan launch placed hwnd={:#x} {} -> {:?} ok={ok}",
            info.hwnd, info.exe, target
        ));
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pw(exe: &str, class: &str, title: &str) -> PlanWindow {
        PlanWindow {
            exe: exe.into(),
            path: String::new(),
            aumid: String::new(),
            class: class.into(),
            title: title.into(),
            monitor: "D1".into(),
            monitor_index: 0,
            rect: FracRect::new(0.0, 0.0, 0.5, 1.0),
        }
    }

    fn lw(hwnd: isize, exe: &str, class: &str, title: &str) -> LiveWin {
        LiveWin {
            hwnd,
            exe: exe.into(),
            class: class.into(),
            title: title.into(),
        }
    }

    #[test]
    fn assigns_by_program_and_minimizes_the_rest() {
        let plan = Plan {
            windows: vec![pw("Discord.exe", "Chrome_WidgetWin_1", "Discord"), pw("spotify.exe", "", "")],
        };
        let live = vec![
            lw(1, "chrome.exe", "Chrome_WidgetWin_1", "YouTube"),
            lw(2, "discord.exe", "Chrome_WidgetWin_1", "#general - Discord"),
            lw(3, "notepad.exe", "Notepad", "notes"),
        ];
        let a = assign(&plan, &live);
        assert_eq!(a.matched, vec![(0, 2)]);
        assert_eq!(a.missing, vec![1]);
        assert_eq!(a.others, vec![1, 3]);
    }

    #[test]
    fn two_windows_of_one_program_prefer_same_title() {
        let plan = Plan {
            windows: vec![pw("chrome.exe", "C", "Mail"), pw("chrome.exe", "C", "Docs")],
        };
        let live = vec![lw(10, "chrome.exe", "C", "Docs"), lw(11, "chrome.exe", "C", "Mail")];
        let a = assign(&plan, &live);
        assert_eq!(a.matched, vec![(0, 11), (1, 10)]);
        assert!(a.missing.is_empty() && a.others.is_empty());
        // titles changed since: still paired, once each
        let live = vec![lw(10, "chrome.exe", "C", "x"), lw(11, "chrome.exe", "C", "y")];
        let a = assign(&plan, &live);
        assert_eq!(a.matched.len(), 2);
        assert_ne!(a.matched[0].1, a.matched[1].1);
    }

    #[test]
    fn save_steps_and_free_slots() {
        // free slot: saved right away
        assert_eq!(save_step(None, 4, false), SaveStep::Commit);
        // taken: ask first, same key again confirms
        assert_eq!(save_step(None, 3, true), SaveStep::AskOverwrite);
        assert_eq!(save_step(Some(3), 3, true), SaveStep::Commit);
        // while asking about 3, another number: free -> saved there,
        // taken -> ask about that one
        assert_eq!(save_step(Some(3), 4, false), SaveStep::Commit);
        assert_eq!(save_step(Some(3), 5, true), SaveStep::AskOverwrite);

        let mut cfg = crate::config::Config::default();
        assert_eq!(free_slots(&cfg), (1..=9).collect::<Vec<u8>>());
        cfg.plans.insert("3".into(), Plan { windows: vec![pw("Discord.exe", "", ""), pw("chrome.exe", "", ""), pw("discord.exe", "", "")] });
        assert_eq!(free_slots(&cfg), vec![1, 2, 4, 5, 6, 7, 8, 9]);
        assert_eq!(app_names(&cfg.plans["3"]), "Discord, chrome");
    }

    #[test]
    fn plan_target_follows_display_else_primary() {
        use crate::monitor::Monitor;
        let m = |name: &str, index: usize, x: i32, primary: bool| Monitor {
            name: name.into(),
            rect: Rect::new(x, 0, x + 1920, 1080),
            work: Rect::new(x, 0, x + 1920, 1040),
            index,
            dpi_x: 96,
            dpi_y: 96,
            primary,
            handle: 0,
        };
        // primary is the RIGHT display here, so "index 0" != primary
        let mons = vec![m("A", 0, 0, false), m("B", 1, 1920, true), m("C", 2, 3840, false)];
        let mut w = pw("x.exe", "", "");
        w.monitor = "C".into();
        w.monitor_index = 2;
        assert_eq!(target_rect(&w, &mons), Some(Rect::new(3840, 0, 4800, 1040)));
        // C unplugged: primary, not whatever is now at index 2 / leftmost
        let two = vec![m("A", 0, 0, false), m("B", 1, 1920, true)];
        assert_eq!(target_rect(&w, &two), Some(Rect::new(1920, 0, 2880, 1040)));
        w.monitor = "gone".into();
        w.monitor_index = 0;
        assert_eq!(target_rect(&w, &mons), Some(Rect::new(1920, 0, 2880, 1040)));
        // nameless entry: index still works
        w.monitor = String::new();
        assert_eq!(target_rect(&w, &mons), Some(Rect::new(0, 0, 960, 1040)));
    }
}
