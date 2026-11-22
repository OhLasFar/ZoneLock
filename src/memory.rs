//! "Reopen apps where you left them": the last spot of each app window
//! (program + window class) is remembered as a fraction of its display's
//! work area, and a freshly opened window of that app is put back there.
//!
//! Stored in %APPDATA%\ZoneLock\positions.json (a cache, separate from
//! config.json so moving windows never rewrites the user's settings).
//! Written when an app window closes and on exit, only if it changed.
//!
//! Restoring is skipped when another window of the same app is already
//! open (a second Chrome window must not land exactly on the first), for
//! windows that cannot be resized (splash screens, dialogs), windows
//! born maximized or covering a display (games) and excluded programs.
//! The pure part (keys, remember/lookup, file format) is tested in-file;
//! the Win32 part runs on the watcher thread.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::rect::FracRect;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    pub monitor: String,
    #[serde(default)]
    pub monitor_index: usize,
    /// Hardware id of the display (see `Monitor::hw_id`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub monitor_hw: String,
    /// Visible frame as a fraction of the display's work area.
    pub rect: FracRect,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default)]
    pub apps: BTreeMap<String, Spot>,
}

/// "chrome.exe|Chrome_WidgetWin_1" (program name case-insensitive).
pub fn key(exe: &str, class: &str) -> String {
    format!("{}|{}", exe.to_ascii_lowercase(), class)
}

impl Memory {
    /// Store `spot` for `key`; true when it changed something.
    pub fn remember(&mut self, key: String, spot: Spot) -> bool {
        // ignore sub-pixel jitter: 1/2000 of the work area
        let same = |a: &Spot| {
            a.monitor == spot.monitor && {
                let (x, y) = (&a.rect, &spot.rect);
                [x.left - y.left, x.top - y.top, x.right - y.right, x.bottom - y.bottom]
                    .iter()
                    .all(|d| d.abs() < 0.0005)
            }
        };
        if self.apps.get(&key).is_some_and(same) {
            return false;
        }
        self.apps.insert(key, spot);
        true
    }
}

static MEM: Mutex<Option<Memory>> = Mutex::new(None);
static DIRTY: AtomicBool = AtomicBool::new(false);
/// positions.json existed but could not be read: never overwrite it.
static BROKEN: AtomicBool = AtomicBool::new(false);

fn path() -> std::path::PathBuf {
    crate::config::config_dir().join("positions.json")
}

fn with_mem<R>(f: impl FnOnce(&mut Memory) -> R) -> R {
    let mut g = MEM.lock().unwrap();
    if g.is_none() {
        *g = Some(match std::fs::read_to_string(path()) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_else(|e| {
                crate::log::line(format!("positions.json unreadable ({e}); not saving positions"));
                BROKEN.store(true, Ordering::Relaxed);
                Memory::default()
            }),
            Err(_) => Memory::default(),
        });
    }
    f(g.as_mut().unwrap())
}

/// Settings page: screen `old` was replaced by `new`: remembered spots on
/// it now point at the new one.
pub fn replace_screen(old: &str, new_hw: &str, new_name: &str, new_index: usize) {
    let n = with_mem(|m| {
        let mut n = 0;
        for s in m.apps.values_mut().filter(|s| s.monitor_hw == old) {
            s.monitor_hw = new_hw.to_string();
            s.monitor = new_name.to_string();
            s.monitor_index = new_index;
            n += 1;
        }
        n
    });
    if n > 0 {
        DIRTY.store(true, Ordering::Relaxed);
        flush();
    }
}

/// Write positions.json if something changed (cheap no-op otherwise).
pub fn flush() {
    if !DIRTY.swap(false, Ordering::Relaxed) || BROKEN.load(Ordering::Relaxed) {
        return;
    }
    let text = with_mem(|m| serde_json::to_string_pretty(m).unwrap_or_default());
    if let Err(e) = std::fs::write(path(), text) {
        crate::log::line(format!("positions.json write failed: {e}"));
    }
}

pub use win::{remember_window, restore_new_window};

mod win {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::WS_THICKFRAME;

    use super::{key, with_mem, Spot, DIRTY};
    use crate::config::Config;
    use crate::monitor::Monitor;
    use crate::rect::FracRect;
    use crate::wininfo::WindowInfo;

    fn resizable(info: &WindowInfo) -> bool {
        info.style & WS_THICKFRAME.0 as i64 != 0
    }

    fn covers_a_display(info: &WindowInfo, mons: &[Monitor], tol: i32) -> bool {
        mons.iter()
            .any(|m| info.rect.approx_eq(&m.rect, tol) || info.rect.approx_eq(&m.work, tol))
    }

    /// Record where an app window sits now (called by the watcher after
    /// each evaluation that leaves it Normal or maximize-contained).
    pub fn remember_window(hwnd: HWND, info: &WindowInfo, cfg: &Config, mons: &[Monitor]) {
        if !cfg.remember_positions
            || info.iconic
            || info.is_zoomed()
            || !resizable(info)
            || covers_a_display(info, mons, cfg.fullscreen_tolerance)
            || !crate::plans::is_app_window(hwnd, info, cfg)
        {
            return;
        }
        let vis = crate::plans::visible_rect(hwnd, info.rect);
        let Some(mon) = crate::monitor::monitor_at(&vis, mons) else { return };
        let spot = Spot {
            monitor: mon.name.clone(),
            monitor_index: mon.index,
            monitor_hw: mon.hw_id.clone(),
            rect: FracRect::from_pixels(&vis, &mon.work),
        };
        if with_mem(|m| m.remember(key(&info.exe, &info.class), spot)) {
            DIRTY.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// A window just showed for the first time: if it is the only open
    /// window of its app and we know where that app was left, put it
    /// back there. Returns true when it moved the window.
    pub fn restore_new_window(hwnd: HWND, info: &WindowInfo, cfg: &Config, mons: &[Monitor]) -> bool {
        if !cfg.remember_positions
            || info.iconic
            || info.is_zoomed()
            || !resizable(info)
            || covers_a_display(info, mons, cfg.fullscreen_tolerance)
            || !crate::plans::is_app_window(hwnd, info, cfg)
        {
            return false;
        }
        let k = key(&info.exe, &info.class);
        let Some(spot) = with_mem(|m| m.apps.get(&k).cloned()) else { return false };
        // another window of this app is already open: leave the new one
        // where the app put it
        let siblings = crate::plans::app_windows(cfg).into_iter().any(|(h, i)| {
            h != hwnd && key(&i.exe, &i.class) == k
        });
        if siblings {
            return false;
        }
        let pw = crate::plans::PlanWindow {
            exe: info.exe.clone(),
            path: String::new(),
            aumid: String::new(),
            class: info.class.clone(),
            title: String::new(),
            monitor: spot.monitor.clone(),
            monitor_index: spot.monitor_index,
            monitor_hw: spot.monitor_hw.clone(),
            rect: spot.rect,
        };
        let Some(target) = crate::plans::target_rect(&pw, mons) else { return false };
        let vis = crate::plans::visible_rect(hwnd, info.rect);
        if vis.approx_eq(&target, 2) {
            return false; // the app already reopened there itself
        }
        let ok = crate::place::apply_zone(hwnd, &target);
        crate::log::line(format!(
            "reopen-where-left hwnd={:#x} {} -> {:?} ok={ok}",
            info.hwnd, info.exe, target
        ));
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(l: f64) -> Spot {
        Spot {
            monitor: "D1".into(),
            monitor_index: 0,
            monitor_hw: String::new(),
            rect: FracRect::new(l, 0.0, l + 0.25, 1.0),
        }
    }

    #[test]
    fn remember_keeps_latest_and_ignores_jitter() {
        let mut m = Memory::default();
        let k = key("Chrome.EXE", "Chrome_WidgetWin_1");
        assert_eq!(k, "chrome.exe|Chrome_WidgetWin_1");
        assert!(m.remember(k.clone(), spot(0.25)));
        assert!(!m.remember(k.clone(), spot(0.25)));
        assert!(!m.remember(k.clone(), spot(0.2501)));
        assert!(m.remember(k.clone(), spot(0.75)));
        assert_eq!(m.apps[&k], spot(0.75));
        // other class of the same program is its own entry
        assert!(m.remember(key("chrome.exe", "Other"), spot(0.0)));
        assert_eq!(m.apps.len(), 2);
    }

    #[test]
    fn file_format_roundtrips() {
        let mut m = Memory::default();
        m.remember(key("discord.exe", "Chrome_WidgetWin_1"), spot(0.5));
        let text = serde_json::to_string_pretty(&m).unwrap();
        let back: Memory = serde_json::from_str(&text).unwrap();
        assert_eq!(back, m);
        // an empty / older file still loads
        let empty: Memory = serde_json::from_str("{}").unwrap();
        assert!(empty.apps.is_empty());
    }
}
