//! Minimum window sizes of the apps on THIS PC, discovered two ways:
//! - asked: every app window that shows (and every open one when the
//!   zone editor opens) is sent WM_GETMINMAXINFO (Windows marshals it
//!   across processes; read-only, nothing moves) - the app's own
//!   minimum track size, minus the invisible frame;
//! - observed: an app put in a zone that comes out bigger refuses to
//!   shrink, so the size it kept is its minimum on that axis (and a
//!   later fit in a smaller zone lowers it).
//! Nothing is built in. Values are stored at 96 dpi.
//!
//! Used by the zone editor to outline zones that are too small for an
//! app, and by the "Force small zones" mode (see `place::apply_zone`).
//!
//! Stored in %APPDATA%\ZoneLock\minsizes.json (a cache like
//! positions.json: never written when it exists but cannot be read).
//! The pure part is tested in-file.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Overflow smaller than this (px) is frame rounding, not a minimum.
const TOL: u32 = 3;

/// Minimum size at 96 dpi; 0 = not known on that axis.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Min {
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
    /// Full path of the program (for its icon in the editor).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

/// An app that does not fit a zone (sizes in px at the asked dpi).
#[derive(Debug, Clone, PartialEq)]
pub struct Tight {
    pub label: String,
    pub exe: String,
    pub path: String,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinSizes {
    /// Program name, lowercase ("chrome.exe") -> minimum.
    #[serde(default)]
    pub apps: BTreeMap<String, Min>,
}

fn to_96(px: u32, dpi: u32) -> u32 {
    (px as u64 * 96 / dpi.max(1) as u64) as u32
}

fn from_96(v: u32, dpi: u32) -> u32 {
    (v as u64 * dpi.max(1) as u64 / 96) as u32
}

/// "chrome.exe" -> "Chrome", known names spelled like the apps do.
pub fn label(exe: &str) -> String {
    let stem = exe.trim_end_matches(".exe").trim_end_matches(".EXE");
    match stem.to_ascii_lowercase().as_str() {
        "msedge" => "Edge".into(),
        "mobaxterm" => "MobaXterm".into(),
        "code" => "VS Code".into(),
        "windowsterminal" => "Terminal".into(),
        "steamwebhelper" | "steam" => "Steam".into(),
        "parsecd" => "Parsec".into(),
        _ => {
            let mut c = stem.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            }
        }
    }
}

impl MinSizes {
    /// An app placed for a `target` visible size (px) ended at `got`.
    /// True when the stored minimum changed.
    pub fn observe(&mut self, exe: &str, path: &str, target: (u32, u32), got: (u32, u32), dpi: u32) -> bool {
        if exe.is_empty() {
            return false;
        }
        let key = exe.to_ascii_lowercase();
        let before = self.apps.get(&key).cloned().unwrap_or_default();
        let mut m = before.clone();
        let axis = |known: &mut u32, t: u32, g: u32| {
            if g > t + TOL {
                // it refused to go smaller: what it kept is the minimum
                *known = to_96(g, dpi);
            } else if *known > 0 && g + TOL >= t && from_96(*known, dpi) > g + TOL {
                // it fitted below what we believed: lower the minimum
                *known = to_96(g, dpi);
            }
        };
        axis(&mut m.w, target.0, got.0);
        axis(&mut m.h, target.1, got.1);
        if m.w == 0 && m.h == 0 {
            return false;
        }
        if !path.is_empty() {
            m.path = path.to_string();
        }
        if m == before {
            return false;
        }
        self.apps.insert(key, m);
        true
    }

    /// The app itself reported its minimum visible size (`w` x `h` px at
    /// `dpi`): it replaces what was known. True when it changed.
    pub fn reported(&mut self, exe: &str, path: &str, w: u32, h: u32, dpi: u32) -> bool {
        if exe.is_empty() {
            return false;
        }
        let key = exe.to_ascii_lowercase();
        let before = self.apps.get(&key).cloned();
        let mut m = Min {
            w: to_96(w, dpi),
            h: to_96(h, dpi),
            path: before.as_ref().map(|b| b.path.clone()).unwrap_or_default(),
        };
        if !path.is_empty() {
            m.path = path.to_string();
        }
        // a 1-2 px wobble between dpis is not news
        if let Some(b) = &before {
            if b.w.abs_diff(m.w) <= 2 && b.h.abs_diff(m.h) <= 2 && b.path == m.path {
                return false;
            }
        }
        self.apps.insert(key, m);
        true
    }

    /// Apps that cannot fit a zone of `w` x `h` px at `dpi`, biggest
    /// need first: (label, min width px, min height px) at that dpi.
    pub fn too_small(&self, w: i32, h: i32, dpi: u32) -> Vec<Tight> {
        let (w, h) = (w.max(0) as u32, h.max(0) as u32);
        let mut out: Vec<Tight> = self
            .apps
            .clone()
            .into_iter()
            .filter_map(|(exe, m)| {
                let (mw, mh) = (from_96(m.w, dpi), from_96(m.h, dpi));
                (mw > w + TOL || mh > h + TOL).then(|| Tight {
                    label: label(&exe),
                    exe,
                    path: m.path,
                    w: mw,
                    h: mh,
                })
            })
            .collect();
        out.sort_by(|a, b| (b.w, b.h).cmp(&(a.w, a.h)).then(a.label.cmp(&b.label)));
        out
    }
}

static STORE: Mutex<Option<MinSizes>> = Mutex::new(None);
/// minsizes.json existed but could not be read: never overwrite it.
static BROKEN: AtomicBool = AtomicBool::new(false);

fn path() -> std::path::PathBuf {
    crate::config::config_dir().join("minsizes.json")
}

fn with<R>(f: impl FnOnce(&mut MinSizes) -> R) -> R {
    let mut g = STORE.lock().unwrap();
    if g.is_none() {
        *g = Some(match std::fs::read_to_string(path()) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_else(|e| {
                crate::log::line(format!("minsizes.json unreadable ({e}); not saving minimum sizes"));
                BROKEN.store(true, Ordering::Relaxed);
                MinSizes::default()
            }),
            Err(_) => MinSizes::default(),
        });
    }
    f(g.as_mut().unwrap())
}

/// Record a placement result (see `MinSizes::observe`); writes the file
/// when something was learned (rare: once per app and axis).
pub fn observe(exe: &str, prog: &str, target: (u32, u32), got: (u32, u32), dpi: u32) {
    let changed = with(|m| m.observe(exe, prog, target, got, dpi));
    if !changed || BROKEN.load(Ordering::Relaxed) {
        return;
    }
    crate::log::line(format!("min size learned for {exe}: zone {target:?} -> window {got:?} @ {dpi} dpi"));
    let text = with(|m| serde_json::to_string_pretty(m).unwrap_or_default());
    if let Err(e) = std::fs::write(path(), text) {
        crate::log::line(format!("minsizes.json write failed: {e}"));
    }
}

/// The app reported its minimum size (see `probe`).
fn reported(exe: &str, prog: &str, w: u32, h: u32, dpi: u32) {
    let changed = with(|m| m.reported(exe, prog, w, h, dpi));
    if !changed || BROKEN.load(Ordering::Relaxed) {
        return;
    }
    crate::log::line(format!("min size reported by {exe}: {w} x {h} @ {dpi} dpi"));
    let text = with(|m| serde_json::to_string_pretty(m).unwrap_or_default());
    if let Err(e) = std::fs::write(path(), text) {
        crate::log::line(format!("minsizes.json write failed: {e}"));
    }
}

pub use win::{probe, probe_all};

mod win {
    use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::*;

    /// What a window says its minimum VISIBLE size is: (exe, program
    /// path, w, h, dpi). Resizable top-level windows of other programs;
    /// `hidden` also takes hidden ones with a caption and a title (apps
    /// living in the tray keep their main window, just hidden). A hung
    /// app is skipped after 150 ms.
    fn ask(hwnd: HWND, hidden: bool) -> Option<(String, String, u32, u32, u32)> {
        let info = crate::wininfo::inspect(hwnd);
        let captioned = info.style & WS_CAPTION.0 as i64 == WS_CAPTION.0 as i64;
        let tool = info.ex_style & WS_EX_TOOLWINDOW.0 as i64 != 0;
        if info.exe.is_empty()
            || info.style & WS_THICKFRAME.0 as i64 == 0
            || info.style & WS_CHILD.0 as i64 != 0
            || info.pid == std::process::id()
        {
            return None;
        }
        if !info.visible && !(hidden && captioned && !tool && unsafe { GetWindowTextLengthW(hwnd) } > 0) {
            return None;
        }
        unsafe {
            let (dx, dy) = (GetSystemMetrics(SM_CXMINTRACK), GetSystemMetrics(SM_CYMINTRACK));
            let mut mmi = MINMAXINFO::default();
            mmi.ptMinTrackSize.x = dx;
            mmi.ptMinTrackSize.y = dy;
            let mut res = 0usize;
            let sent = SendMessageTimeoutW(
                hwnd,
                WM_GETMINMAXINFO,
                WPARAM(0),
                LPARAM(&mut mmi as *mut _ as isize),
                SMTO_ABORTIFHUNG,
                150,
                Some(&mut res),
            );
            if sent.0 == 0 {
                return None;
            }
            let (w, h) = (mmi.ptMinTrackSize.x, mmi.ptMinTrackSize.y);
            if w <= dx && h <= dy {
                return None; // no minimum of its own
            }
            // the minimum covers the invisible resize borders: keep the
            // visible part, what a zone has to hold
            let mut wr = RECT::default();
            let mut vis = RECT::default();
            let (mut bw, mut bh) = (0, 0);
            if GetWindowRect(hwnd, &mut wr).is_ok()
                && DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS,
                    &mut vis as *mut _ as *mut _,
                    std::mem::size_of::<RECT>() as u32,
                )
                .is_ok()
            {
                bw = ((wr.right - wr.left) - (vis.right - vis.left)).max(0);
                bh = ((wr.bottom - wr.top) - (vis.bottom - vis.top)).max(0);
            }
            let dpi = GetDpiForWindow(hwnd).max(96);
            let path = crate::wininfo::process_path(info.pid);
            Some((info.exe, path, (w - bw).max(0) as u32, (h - bh).max(0) as u32, dpi))
        }
    }

    /// A window just showed: remember its app's minimum size.
    pub fn probe(hwnd: HWND) {
        if let Some((exe, path, w, h, dpi)) = ask(hwnd, false) {
            super::reported(&exe, &path, w, h, dpi);
        }
    }

    /// Ask every top-level window, hidden tray apps included (ZoneLock
    /// start, zone editor opening). Per program the biggest minimum wins
    /// (its main window, not a small helper).
    pub fn probe_all() {
        use std::collections::HashMap;
        type Found = HashMap<String, (String, String, u32, u32, u32)>;
        unsafe extern "system" fn each(h: HWND, l: LPARAM) -> windows::core::BOOL {
            let found = &mut *(l.0 as *mut Found);
            if let Some(a) = ask(h, true) {
                let at96 = |a: &(String, String, u32, u32, u32)| (a.2 * 96 / a.4, a.3 * 96 / a.4);
                let k = a.0.to_ascii_lowercase();
                if found.get(&k).is_none_or(|b| at96(&a) > at96(b)) {
                    found.insert(k, a);
                }
            }
            true.into()
        }
        let mut found: Found = HashMap::new();
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut Found as isize));
        }
        for (exe, path, w, h, dpi) in found.into_values() {
            super::reported(&exe, &path, w, h, dpi);
        }
    }
}

/// See `MinSizes::too_small`.
pub fn too_small(w: i32, h: i32, dpi: u32) -> Vec<Tight> {
    with(|m| m.too_small(w, h, dpi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learns_from_overflow_and_lowers_when_it_fits() {
        let mut m = MinSizes::default();
        // zone 400 wide, Discord stayed 940: min width 940
        assert!(m.observe("Discord.exe", "", (400, 800), (940, 800), 96));
        assert_eq!((m.apps["discord.exe"].w, m.apps["discord.exe"].h), (940, 0));
        // same again: nothing new
        assert!(!m.observe("Discord.exe", "", (400, 800), (940, 800), 96));
        // at 144 dpi it fitted a 1200 px zone (= 800 at 96): min lowered
        assert!(m.observe("discord.exe", "", (1200, 900), (1200, 900), 144));
        assert_eq!(m.apps["discord.exe"].w, 800);
        // height overflow learned separately
        assert!(m.observe("discord.exe", "", (1200, 300), (1200, 500), 96));
        assert_eq!((m.apps["discord.exe"].w, m.apps["discord.exe"].h), (800, 500));
        // the program path is kept for its icon
        assert!(m.observe("discord.exe", "C:/D/Discord.exe", (1200, 300), (1200, 500), 96));
        assert_eq!(m.apps["discord.exe"].path, "C:/D/Discord.exe");
        // frame rounding is not a minimum
        assert!(!m.observe("notepad.exe", "", (600, 400), (602, 400), 96));
        assert!(!m.observe("", "", (1, 1), (900, 900), 96));
    }

    #[test]
    fn too_small_lists_apps_biggest_first() {
        let mut m = MinSizes::default();
        let min = |w, h| Min { w, h, path: String::new() };
        m.apps.insert("discord.exe".into(), min(940, 500));
        m.apps.insert("mobaxterm.exe".into(), min(600, 0));
        let names = |v: Vec<Tight>| v.into_iter().map(|x| x.label).collect::<Vec<_>>();
        // nothing built in: only what this PC taught
        assert_eq!(names(m.too_small(480, 1000, 96)), ["Discord", "MobaXterm"]);
        assert_eq!(names(m.too_small(700, 1000, 96)), ["Discord"]);
        assert_eq!(names(m.too_small(1000, 400, 96)), ["Discord"]);
        assert!(m.too_small(1000, 1000, 96).is_empty());
        // 150%: Discord needs 1410 px
        assert_eq!(names(m.too_small(1300, 1000, 144)), ["Discord"]);
    }

    #[test]
    fn reported_minimum_replaces_what_was_known() {
        let mut m = MinSizes::default();
        // Windows Terminal says 480 x 103 at 96 dpi
        assert!(m.reported("WindowsTerminal.exe", "C:/W/wt.exe", 480, 103, 96));
        assert_eq!((m.apps["windowsterminal.exe"].w, m.apps["windowsterminal.exe"].h), (480, 103));
        // same at 144 dpi (720 x 155 px) is not news
        assert!(!m.reported("windowsterminal.exe", "", 720, 155, 144));
        assert_eq!(m.apps["windowsterminal.exe"].path, "C:/W/wt.exe");
        // a real change replaces it
        assert!(m.reported("windowsterminal.exe", "", 600, 103, 96));
        assert_eq!(m.apps["windowsterminal.exe"].w, 600);
        assert!(!m.reported("", "", 900, 900, 96));
    }

    #[test]
    fn labels() {
        assert_eq!(label("chrome.exe"), "Chrome");
        assert_eq!(label("MobaXterm.exe"), "MobaXterm");
        assert_eq!(label("msedge.exe"), "Edge");
        assert_eq!(label("Discord.exe"), "Discord");
        assert_eq!(label("steamwebhelper.exe"), "Steam");
    }
}
