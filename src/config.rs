//! Local-only configuration. One JSON file under %APPDATA%\ZoneLock.
//! No cloud, no telemetry, no account.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::zones::LayoutSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Master switch; hotkey toggles it.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Pixel gap applied inside every zone edge.
    #[serde(default)]
    pub zone_gap: i32,
    /// Edge tolerance (px) when deciding a window "covers the monitor".
    #[serde(default = "default_fullscreen_tol")]
    pub fullscreen_tolerance: i32,
    /// Max consecutive re-containments of one window within
    /// `refight_window_ms`; beyond that we yield until the window leaves the
    /// monitor-covering state. Prevents ping-pong with stubborn apps.
    #[serde(default = "default_refights")]
    pub max_refights: u32,
    #[serde(default = "default_refight_window")]
    pub refight_window_ms: u64,
    /// Process names (lowercase, e.g. "game.exe") never touched.
    #[serde(default)]
    pub exclude_processes: Vec<String>,
    /// Fullscreen-exit key per process ("chrome.exe": "F11"). Used for
    /// apps in `fs_stable`, and as a fallback for apps that keep fighting
    /// the in-zone hold: we post the key so the app leaves its own
    /// fullscreen, then strip its frame and park it in the zone.
    #[serde(default = "default_fs_exit_keys")]
    pub fs_exit_keys: std::collections::BTreeMap<String, String>,
    /// Processes using "stable" fullscreen: instead of being held in real
    /// fullscreen inside the zone (Chromium flashes on every click there),
    /// they leave their fullscreen and fill the zone without a frame.
    #[serde(default)]
    pub fs_stable: Vec<String>,
    /// Delay before re-posting the exit key while waiting for the drop.
    #[serde(default = "default_exit_retry")]
    pub exit_retry_ms: u64,
    /// Max exit-key posts before ignoring the window.
    #[serde(default = "default_max_exit_sends")]
    pub max_exit_sends: u32,
    /// Extra window class prefixes never touched.
    #[serde(default)]
    pub exclude_class_prefixes: Vec<String>,
    /// Show zone overlay while dragging a window with Shift held; drop
    /// inside a zone snaps the window there (FancyZones-style).
    #[serde(default = "default_true")]
    pub shift_snap: bool,
    /// Reopen app windows where they were left (positions.json).
    #[serde(default = "default_true")]
    pub remember_positions: bool,
    /// Ask GitHub once a day whether a newer release exists (the only
    /// network use; update.rs).
    #[serde(default = "default_true")]
    pub check_updates: bool,
    /// Start with Windows (HKCU Run key). Off by default.
    #[serde(default)]
    pub autostart: bool,
    /// Hotkey bindings. Values are human strings like "Win+Alt+Z".
    #[serde(default)]
    pub hotkeys: HotkeyConfig,
    /// Zone layouts per monitor.
    #[serde(default)]
    pub layouts: LayoutSet,
    /// Plans "1".."9": saved window arrangements (see plans.rs).
    #[serde(default)]
    pub plans: std::collections::BTreeMap<String, crate::plans::Plan>,
    /// Presets saved by the user (on top of the built-in ones).
    #[serde(default)]
    pub user_presets: Vec<crate::zones::Layout>,
    /// Verbose logging to %APPDATA%\ZoneLock\zonelock.log.
    #[serde(default)]
    pub log_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotkeyConfig {
    /// Toggle containment on/off.
    #[serde(default = "hk_toggle")]
    pub toggle: String,
    /// Open the zone editor overlay.
    #[serde(default = "hk_editor")]
    pub editor: String,
    /// Cycle preset layout on the monitor under the cursor.
    #[serde(default = "hk_next_preset")]
    pub next_preset: String,
    /// Snap the active window into the next zone of its monitor.
    #[serde(default = "hk_next_zone")]
    pub next_zone: String,
    /// Snap the active window into the previous zone of its monitor.
    #[serde(default = "hk_prev_zone")]
    pub prev_zone: String,
    /// Active window's fullscreen: inside its zone <-> whole display.
    #[serde(default = "hk_fs_toggle")]
    pub fs_toggle: String,
    /// Show every shortcut in a card in the middle of the screen.
    #[serde(default = "hk_cheatsheet")]
    pub cheatsheet: String,
    /// Modifiers for "go to plan N" (the digit 1-9 is added).
    #[serde(default = "hk_plan_go")]
    pub plan_go: String,
    /// Modifiers for "save the current windows as plan N".
    #[serde(default = "hk_plan_save")]
    pub plan_save: String,
    /// Modifier set held while dragging a window to show the zones and
    /// snap on drop ("Shift", "Ctrl+Alt", ...). Empty = no key needed,
    /// the zones appear on every drag.
    #[serde(default = "hk_drag_snap")]
    pub drag_snap: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle: hk_toggle(),
            editor: hk_editor(),
            next_preset: hk_next_preset(),
            next_zone: hk_next_zone(),
            prev_zone: hk_prev_zone(),
            fs_toggle: hk_fs_toggle(),
            cheatsheet: hk_cheatsheet(),
            plan_go: hk_plan_go(),
            plan_save: hk_plan_save(),
            drag_snap: hk_drag_snap(),
        }
    }
}

fn hk_toggle() -> String {
    "Win+Alt+Z".into()
}
fn hk_editor() -> String {
    "Win+Alt+E".into()
}
fn hk_next_preset() -> String {
    "Win+Alt+P".into()
}
// Win+Alt+arrows belong to Windows 11 (keyboard snap) and Ctrl+Alt+letter
// is AltGr on many layouts (Ctrl+Alt+E = euro sign): zone moves use
// Win+Ctrl+Alt+arrows, free on a stock Windows.
fn hk_next_zone() -> String {
    "Win+Ctrl+Alt+Right".into()
}
fn hk_prev_zone() -> String {
    "Win+Ctrl+Alt+Left".into()
}
fn hk_fs_toggle() -> String {
    "Win+Ctrl+Alt+Up".into()
}
// Win+Alt+K is Windows' "mute in call"; H for help is free
fn hk_cheatsheet() -> String {
    "Win+Alt+H".into()
}
fn hk_plan_go() -> String {
    "Alt+Shift".into()
}
fn hk_plan_save() -> String {
    "Ctrl+Alt+Shift".into()
}
fn hk_drag_snap() -> String {
    "Shift".into()
}

fn default_fs_exit_keys() -> std::collections::BTreeMap<String, String> {
    [
        ("chrome.exe", "F11"),
        ("msedge.exe", "F11"),
        ("firefox.exe", "F11"),
        ("vivaldi.exe", "F11"),
        ("brave.exe", "F11"),
        ("opera.exe", "F11"),
        ("vlc.exe", "f"),
        ("mpv.exe", "f"),
        ("mpvnet.exe", "f"),
        ("mpc-hc64.exe", "f"),
        ("mpc-be64.exe", "f"),
        ("PotPlayerMini64.exe", "f"),
        ("wmplayer.exe", "Escape"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn default_exit_retry() -> u64 {
    800
}
fn default_max_exit_sends() -> u32 {
    3
}

fn default_true() -> bool {
    true
}
fn default_fullscreen_tol() -> i32 {
    8
}
fn default_refights() -> u32 {
    5
}
fn default_refight_window() -> u64 {
    2000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            zone_gap: 0,
            fullscreen_tolerance: default_fullscreen_tol(),
            max_refights: default_refights(),
            refight_window_ms: default_refight_window(),
            exclude_processes: vec![],
            exclude_class_prefixes: vec![],
            fs_exit_keys: default_fs_exit_keys(),
            fs_stable: vec![],
            exit_retry_ms: default_exit_retry(),
            max_exit_sends: default_max_exit_sends(),
            shift_snap: true,
            remember_positions: true,
            check_updates: true,
            autostart: false,
            hotkeys: HotkeyConfig::default(),
            layouts: LayoutSet::default(),
            user_presets: vec![],
            plans: Default::default(),
            log_enabled: false,
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ZoneLock")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

impl Config {
    /// Load from disk; missing file = defaults + written out once.
    /// A corrupt file fails loud: the error is returned, never silently
    /// replaced.
    pub fn load() -> Result<Config, String> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &PathBuf) -> Result<Config, String> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                let _ = cfg.save_to(path);
                Ok(cfg)
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &PathBuf) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| e.to_string())
    }

    pub fn is_excluded_process(&self, exe_name: &str) -> bool {
        let exe = exe_name.to_lowercase();
        self.exclude_processes
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&exe))
    }

    /// Fullscreen-exit virtual key for a process, if one is configured.
    pub fn fs_exit_vk(&self, exe_name: &str) -> Option<u32> {
        self.fs_exit_keys
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(exe_name))
            .and_then(|(_, v)| crate::hotkeys::vk_from_name(v))
    }

    /// True when this process uses stable (frameless, non-fullscreen)
    /// zone fullscreen.
    pub fn is_fs_stable(&self, exe_name: &str) -> bool {
        self.fs_stable.iter().any(|p| p.eq_ignore_ascii_case(exe_name))
    }

    pub fn is_excluded_class(&self, class: &str) -> bool {
        self.exclude_class_prefixes
            .iter()
            .any(|p| class.starts_with(p.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zl_cfg_{}", std::process::id()));
        let path = dir.join("config.json");
        let cfg = Config::load_from(&path).expect("first load creates defaults");
        assert!(cfg.enabled);
        assert_eq!(cfg.fullscreen_tolerance, 8);
        let loaded = Config::load_from(&path).expect("second load reads file");
        assert_eq!(loaded.zone_gap, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_fails_loud() {
        let dir = std::env::temp_dir().join(format!("zl_cfgbad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Config::load_from(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exclusion_match() {
        let mut cfg = Config::default();
        cfg.exclude_processes.push("vlc.exe".into());
        cfg.exclude_class_prefixes.push("Qt".into());
        assert!(cfg.is_excluded_process("VLC.EXE"));
        assert!(cfg.is_excluded_class("Qt5152WindowIcon"));
        assert!(!cfg.is_excluded_class("Chrome_WidgetWin_1"));
    }
}
