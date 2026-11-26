//! Plans: numbered window arrangements (1-9), like light-weight
//! desktops. Saving a plan records every visible app window (program,
//! where it sits as a fraction of its display's work area, z-order).
//! Going to a plan brings those windows back where they were, launches
//! the programs that are not running (and places their window when it
//! shows up), and minimizes every other app window.
//!
//! Virtual desktops: a plan works on the desktop shown. Windows on other
//! desktops are never minimized or moved, and a program whose only
//! window is on another desktop is not launched a second time.
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
    /// Hardware id of that display (`Monitor::hw_id`): preferred over the
    /// device name, which changes with ports and docks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub monitor_hw: String,
    /// Visible frame as a fraction of that display's work area.
    pub rect: FracRect,
}

/// Windows top of the z-order first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub windows: Vec<PlanWindow>,
    /// Display setup it was saved on (`setups::key_of`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub setup: String,
}

/// An app window that exists right now.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWin {
    pub hwnd: isize,
    pub exe: String,
    pub class: String,
    pub title: String,
    /// On another virtual desktop than the one shown.
    pub elsewhere: bool,
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
    /// plan.windows indexes whose program only has a window on another
    /// virtual desktop: left there, not launched again.
    pub away: Vec<usize>,
}

/// Pair plan entries with live windows. Same program is required; same
/// class is preferred, then the same title; each live window is used
/// once. Only windows of the desktop shown are placed or minimized; one
/// on another desktop only keeps its program from being launched again.
/// Pure.
pub fn assign(plan: &Plan, live: &[LiveWin]) -> Assignment {
    // windows elsewhere count as taken: never placed, never minimized
    let mut used: Vec<bool> = live.iter().map(|w| w.elsewhere).collect();
    let mut away_used = vec![false; live.len()];
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
            None => {
                let away = (0..live.len())
                    .find(|&j| live[j].elsewhere && !away_used[j] && live[j].exe.eq_ignore_ascii_case(&pw.exe));
                match away {
                    Some(j) => {
                        away_used[j] = true;
                        out.away.push(i);
                    }
                    None => out.missing.push(i),
                }
            }
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

/// The display a plan window goes to now: the same screen (hardware id,
/// else device name), otherwise the primary one (never "whatever now
/// sits at the same index": with a display unplugged that is a different
/// screen). The index is only used by entries that carry no name.
///
/// A replaced screen takes the old one's place. With `others` (the
/// screens the plan's other windows were saved on): when the exact screen
/// is missing, (1) the one connected screen of the same model, else (2)
/// the screen now plugged in at the same place (device name), as long as
/// it is not the exact screen of another window of the plan. A lone spot
/// (`others` = None, "reopen where you left it") only takes (1): with no
/// plan around it, a different screen at that name may be another desk.
pub fn resolve_monitor<'a>(
    pw: &PlanWindow,
    monitors: &'a [crate::monitor::Monitor],
    others: Option<&[&str]>,
) -> Option<&'a crate::monitor::Monitor> {
    let taken = |m: &crate::monitor::Monitor| others.is_some_and(|o| o.contains(&m.hw_id.as_str()));
    use crate::monitor::model_of;
    let named = !pw.monitor.is_empty();
    let real = || monitors.iter().filter(|m| !m.virt);
    let has_hw = !pw.monitor_hw.is_empty();
    has_hw
        .then(|| real().find(|m| m.hw_id == pw.monitor_hw))
        .flatten()
        .or_else(|| {
            if !has_hw {
                return None;
            }
            let model = model_of(&pw.monitor_hw);
            let same: Vec<_> = real()
                .filter(|m| !m.hw_id.is_empty() && model_of(&m.hw_id) == model)
                .filter(|m| !taken(m))
                .collect();
            (same.len() == 1).then(|| same[0])
        })
        .or_else(|| {
            // by name: entries saved without a hardware id, or (in a plan)
            // a new screen plugged in where the old one was
            real().find(|m| {
                named
                    && m.name == pw.monitor
                    && (pw.monitor_hw.is_empty() || m.hw_id.is_empty() || (others.is_some() && !taken(m)))
            })
        })
        .or_else(|| {
            (!named)
                .then(|| real().find(|m| m.index == pw.monitor_index))
                .flatten()
        })
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
}

/// Absolute visible rect of a lone window spot (no plan around it).
pub fn target_rect(pw: &PlanWindow, monitors: &[crate::monitor::Monitor]) -> Option<Rect> {
    let mon = resolve_monitor(pw, monitors, None)?;
    Some(pw.rect.to_pixels(&mon.work, 0))
}

/// The screens the other windows of `plan` were saved on.
pub fn plan_screens(plan: &Plan) -> Vec<&str> {
    plan.windows
        .iter()
        .map(|w| w.monitor_hw.as_str())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Absolute visible rect of window `i` of `plan` on the current displays.
pub fn plan_target(plan: &Plan, i: usize, monitors: &[crate::monitor::Monitor]) -> Option<Rect> {
    let pw = &plan.windows[i];
    let others: Vec<&str> = plan_screens(plan).into_iter().filter(|s| *s != pw.monitor_hw).collect();
    let mon = resolve_monitor(pw, monitors, Some(&others))?;
    Some(pw.rect.to_pixels(&mon.work, 0))
}

/// A window whose visible frame is `now` already sits at `target`: same
/// spot within 2 px, and the same size or bigger (an app whose minimum
/// size is larger than the spot never gets smaller). Pure.
pub fn in_place(now: &Rect, target: &Rect) -> bool {
    let near = |a: i32, b: i32| (a - b).abs() <= 2;
    near(now.left, target.left)
        && near(now.top, target.top)
        && now.width() + 2 >= target.width()
        && now.height() + 2 >= target.height()
        && (near(now.right, target.right) || now.width() > target.width())
        && (near(now.bottom, target.bottom) || now.height() > target.height())
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

pub use win::{arrange, claim_new_window, go, save_current};
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

    use super::{assign, plan_target, LiveWin, Plan, PlanWindow};
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
        !info.cloaked && is_app_window_anywhere(hwnd, info, cfg)
    }

    /// `is_app_window` without the cloaked test: the caller decides what
    /// a cloaked window is (another virtual desktop, a suspended app).
    fn is_app_window_anywhere(hwnd: HWND, info: &WindowInfo, cfg: &Config) -> bool {
        unsafe {
            info.valid
                && info.visible
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
        all_windows()
            .into_iter()
            .map(|h| (h, wininfo::inspect(h)))
            .filter(|(h, i)| is_app_window(*h, i, cfg))
            .collect()
    }

    /// The app windows of the desktop shown (z-order, top first), then
    /// those on other virtual desktops, as plan matching input.
    fn live_windows(cfg: &Config) -> (Vec<(HWND, WindowInfo)>, Vec<LiveWin>) {
        let mut here = Vec::new();
        let mut away = Vec::new();
        for h in all_windows() {
            let info = wininfo::inspect(h);
            if is_app_window(h, &info, cfg) {
                here.push((h, info));
            } else if info.cloaked && is_app_window_anywhere(h, &info, cfg) && crate::vdesk::is_elsewhere(h) {
                away.push(LiveWin {
                    hwnd: h.0 as isize,
                    exe: info.exe.clone(),
                    class: info.class.clone(),
                    title: window_text(h),
                    elsewhere: true,
                });
            }
        }
        let live = here
            .iter()
            .map(|(h, i)| LiveWin {
                hwnd: h.0 as isize,
                exe: i.exe.clone(),
                class: i.class.clone(),
                title: window_text(*h),
                elsewhere: false,
            })
            .chain(away)
            .collect();
        (here, live)
    }

    /// Every top-level window, z-order top first.
    fn all_windows() -> Vec<HWND> {
        unsafe extern "system" fn cb(hwnd: HWND, l: LPARAM) -> windows::core::BOOL {
            let v = &mut *(l.0 as *mut Vec<HWND>);
            v.push(hwnd);
            windows::core::BOOL(1)
        }
        let mut all: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
        }
        all
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
        let mut plan = Plan {
            setup: crate::setups::key_of(&mons),
            ..Default::default()
        };
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
                monitor_hw: mon.hw_id.clone(),
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
                // start it in its own folder, like a shortcut would: some
                // apps (Steam...) write logs / look for files in the
                // current directory, which would otherwise be ours
                let dir = std::path::Path::new(&pw.path)
                    .parent()
                    .map(|d| wide(&d.to_string_lossy()))
                    .unwrap_or_else(|| wide(""));
                ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR(dir.as_ptr()), SW_SHOWNORMAL)
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
        let (wins, live) = live_windows(&cfg);
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
                if let Some(t) = plan_target(&plan, i, &mons) {
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
            let Some(target) = plan_target(&plan, i, &mons) else { continue };
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
        let away: Vec<&str> = a.away.iter().map(|&i| plan.windows[i].exe.as_str()).collect();
        log::line(format!(
            "plan {slot}: placed {}, launched [{}], minimized {}, on another desktop [{}]",
            a.matched.len(),
            launched.join(", "),
            a.others.len(),
            away.join(", ")
        ));
        if !away.is_empty() {
            let names = super::app_names(&Plan {
                windows: a.away.iter().map(|&i| plan.windows[i].clone()).collect(),
                setup: String::new(),
            });
            let one = a.away.len() == 1;
            crate::planprompt::show_success(
                &format!("Plan {slot}"),
                &format!(
                    "{names} {} open on another desktop: left there, not opened again.",
                    if one { "is" } else { "are" }
                ),
            );
        }
        Ok(())
    }

    /// The gentle version of `go`, run when the connected screens change
    /// to a setup linked to plan `slot`: the plan's windows that are open
    /// and not minimized go to their place (and z-order), without taking
    /// the focus. Nothing is launched, minimized or restored: unplugging a
    /// cable must not open apps or hide what the user is doing.
    /// Also run when the user switches to a virtual desktop linked to the
    /// plan: only that desktop's windows are seen. Windows already in
    /// place are left alone, and when none had to move nothing is touched
    /// (not even the z-order the user picked).
    /// Returns how many windows were moved.
    pub fn arrange(shared: &Shared, slot: u8) -> Result<usize, String> {
        let cfg = shared.config.read().unwrap().clone();
        let Some(plan) = cfg.plans.get(&slot.to_string()).cloned() else {
            return Err(format!("Plan {slot} is empty."));
        };
        let mons = shared.monitors.read().unwrap().clone();
        let wins: Vec<(HWND, WindowInfo)> = app_windows(&cfg)
            .into_iter()
            .filter(|(_, i)| !i.iconic)
            .collect();
        let live: Vec<LiveWin> = wins
            .iter()
            .map(|(h, i)| LiveWin {
                hwnd: h.0 as isize,
                exe: i.exe.clone(),
                class: i.class.clone(),
                title: window_text(*h),
                elsewhere: false,
            })
            .collect();
        let a = assign(&plan, &live);
        let off: Vec<(HWND, Rect)> = a
            .matched
            .iter()
            .filter_map(|&(i, h)| {
                let hwnd = HWND(h as *mut _);
                let t = plan_target(&plan, i, &mons)?;
                let now = wins.iter().find(|(w, _)| *w == hwnd).map(|(w, info)| visible_rect(*w, info.rect))?;
                (!super::in_place(&now, &t)).then_some((hwnd, t))
            })
            .collect();
        if off.is_empty() {
            log::line(format!("plan {slot} arrange: every window already in place"));
            return Ok(0);
        }
        // bottom of the stack first, so the plan's top window ends on top
        for &(_, h) in a.matched.iter().rev() {
            let hwnd = HWND(h as *mut _);
            if let Some((_, t)) = off.iter().find(|(w, _)| *w == hwnd) {
                place::apply_zone(hwnd, t);
            }
            unsafe {
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
        log::line(format!("plan {slot} arranged: moved {} of {}", off.len(), a.matched.len()));
        Ok(off.len())
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
            monitor_hw: String::new(),
            rect: FracRect::new(0.0, 0.0, 0.5, 1.0),
        }
    }

    fn lw(hwnd: isize, exe: &str, class: &str, title: &str) -> LiveWin {
        LiveWin {
            hwnd,
            exe: exe.into(),
            class: class.into(),
            title: title.into(),
            elsewhere: false,
        }
    }

    #[test]
    fn windows_on_other_desktops_are_left_alone() {
        let plan = Plan {
            windows: vec![pw("discord.exe", "", ""), pw("chrome.exe", "C", "Mail"), pw("spotify.exe", "", "")],
            ..Default::default()
        };
        let away = |h, exe: &str| LiveWin { elsewhere: true, ..lw(h, exe, "C", "") };
        let live = vec![
            away(1, "discord.exe"),
            away(2, "chrome.exe"),
            lw(3, "chrome.exe", "C", "Docs"),
            away(4, "notepad.exe"),
        ];
        let a = assign(&plan, &live);
        // the Chrome window of this desktop is used, even with a worse title
        assert_eq!(a.matched, vec![(1, 3)]);
        // Discord only exists elsewhere: not launched; Spotify is missing
        assert_eq!(a.away, vec![0]);
        assert_eq!(a.missing, vec![2]);
        // nothing on another desktop is minimized
        assert!(a.others.is_empty());
    }

    #[test]
    fn in_place_allows_min_size_overflow() {
        let t = Rect::new(0, 0, 400, 300);
        assert!(in_place(&Rect::new(1, 0, 401, 299), &t));
        assert!(in_place(&Rect::new(0, 0, 520, 300), &t));
        assert!(!in_place(&Rect::new(0, 0, 300, 300), &t));
        assert!(!in_place(&Rect::new(50, 0, 450, 300), &t));
    }

    #[test]
    fn assigns_by_program_and_minimizes_the_rest() {
        let plan = Plan {
            windows: vec![pw("Discord.exe", "Chrome_WidgetWin_1", "Discord"), pw("spotify.exe", "", "")],
            ..Default::default()
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
            ..Default::default()
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
        cfg.plans.insert("3".into(), Plan { windows: vec![pw("Discord.exe", "", ""), pw("chrome.exe", "", ""), pw("discord.exe", "", "")], ..Default::default() });
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
            ..Default::default()
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

        // hardware id wins over the device name: the screen saved as C is
        // now plugged in as "A", and "C" is another screen
        let hw = |mut m: Monitor, id: &str| {
            m.hw_id = id.into();
            m
        };
        let mons = vec![
            hw(m("A", 0, 0, false), "HP"),
            hw(m("B", 1, 1920, true), "LAPTOP"),
            hw(m("C", 2, 3840, false), "DELL"),
        ];
        let mut w = pw("x.exe", "", "");
        w.monitor = "C".into();
        w.monitor_hw = "HP".into();
        assert_eq!(target_rect(&w, &mons), Some(Rect::new(0, 0, 960, 1040)));
        // that screen gone: not "C" (a different screen now), the primary
        let two = vec![hw(m("B", 0, 1920, true), "LAPTOP"), hw(m("C", 1, 3840, false), "DELL")];
        assert_eq!(target_rect(&w, &two), Some(Rect::new(1920, 0, 2880, 1040)));
        // the HP broke and was replaced by another HP of the same model:
        // the new one takes its windows; with two of that model connected,
        // the one that is not another window's exact screen
        w.monitor_hw = "220E-3688~old".into();
        let mons = vec![hw(m("A", 0, 0, false), "220E-3688~new"), hw(m("B", 1, 1920, true), "LAPTOP")];
        assert_eq!(target_rect(&w, &mons), Some(Rect::new(0, 0, 960, 1040)));
        let mons2 = vec![
            hw(m("A", 0, 0, false), "220E-3688~new"),
            hw(m("B", 1, 1920, true), "LAPTOP"),
            hw(m("C", 2, 3840, false), "220E-3688~kept"),
        ];
        assert_eq!(resolve_monitor(&w, &mons2, None).map(|m| m.name.as_str()), Some("B"), "ambiguous: primary");
        assert_eq!(resolve_monitor(&w, &mons2, Some(&["220E-3688~kept"])).map(|m| m.name.as_str()), Some("A"));

        // the gaming screen (saved on "C") replaced by another model on the
        // same port: in a plan it takes the windows, unless that screen is
        // the exact screen of another window of the plan
        let mut g = pw("game.exe", "", "");
        g.monitor = "C".into();
        g.monitor_hw = "OLD-1111~aa".into();
        let mons3 = vec![hw(m("B", 0, 0, true), "LAPTOP"), hw(m("C", 1, 1920, false), "NEW-2222~bb")];
        assert_eq!(resolve_monitor(&g, &mons3, Some(&["LAPTOP"])).map(|m| m.name.as_str()), Some("C"));
        assert_eq!(resolve_monitor(&g, &mons3, Some(&["NEW-2222~bb"])).map(|m| m.name.as_str()), Some("B"));
        // a lone spot does not follow a different model: primary
        assert_eq!(resolve_monitor(&g, &mons3, None).map(|m| m.name.as_str()), Some("B"));
        // a virtual display is never a target, even by name
        let mut v = m("C", 1, 3840, false);
        v.virt = true;
        let mut w2 = pw("x.exe", "", "");
        w2.monitor = "C".into();
        assert_eq!(
            target_rect(&w2, &[m("B", 0, 1920, true), v]),
            Some(Rect::new(1920, 0, 2880, 1040))
        );
    }
}
