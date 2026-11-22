//! Configurable global hotkeys. Strings like "Win+Alt+Z" or "Ctrl+Shift+F9".

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_CONTROL,
    VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

use crate::config::HotkeyConfig;

pub const ID_TOGGLE: i32 = 1;
pub const ID_EDITOR: i32 = 2;
pub const ID_NEXT_PRESET: i32 = 3;
pub const ID_NEXT_ZONE: i32 = 4;
pub const ID_PREV_ZONE: i32 = 5;
pub const ID_FS_TOGGLE: i32 = 6;
pub const ID_CHEATSHEET: i32 = 7;
/// Plan hotkeys: ID_PLAN_GO + n / ID_PLAN_SAVE + n, n = 1..=9.
pub const ID_PLAN_GO: i32 = 100;
pub const ID_PLAN_SAVE: i32 = 200;

/// Modifiers-only binding ("Alt+Shift") is valid when, with a digit
/// added, it parses to that digit and carries at least one modifier.
pub fn parse_mods(mods: &str) -> Option<HOT_KEY_MODIFIERS> {
    let t = mods.trim();
    if t.is_empty() {
        return None;
    }
    let only_mods = t.split('+').all(|p| {
        matches!(
            p.trim().to_ascii_lowercase().as_str(),
            "win" | "windows" | "super" | "alt" | "ctrl" | "control" | "shift"
        )
    });
    if !only_mods {
        return None;
    }
    match parse(&format!("{t}+1")) {
        Some((m, 0x31)) if m.0 != 0 => Some(m),
        _ => None,
    }
}

/// True while every modifier in `m` is physically held. Polled, not a
/// registered hotkey: combos the OS reserves (Win+Alt+arrows, ...) work.
pub fn mods_held(m: HOT_KEY_MODIFIERS) -> bool {
    let down = |vk: u16| unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 };
    (m.0 & MOD_SHIFT.0 == 0 || down(VK_SHIFT.0))
        && (m.0 & MOD_CONTROL.0 == 0 || down(VK_CONTROL.0))
        && (m.0 & MOD_ALT.0 == 0 || down(VK_MENU.0))
        && (m.0 & MOD_WIN.0 == 0 || down(VK_LWIN.0) || down(VK_RWIN.0))
}

/// Parse "Mod+Mod+Key" into (modifiers, vk). Returns None when malformed.
pub fn parse(binding: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key: Option<u32> = None;
    for part in binding.split('+') {
        let p = part.trim();
        match p.to_ascii_lowercase().as_str() {
            "win" | "windows" | "super" => mods |= MOD_WIN,
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "" => {}
            k => key = Some(vk_from_name(k)?),
        }
    }
    key.map(|k| (mods, k))
}

/// Named keys accepted in config strings (case-insensitive), besides
/// single letters/digits and the generic `VK_XX` hex form. The first name
/// of a vk is its canonical spelling.
const NAMED: &[(&str, u32)] = &[
    ("F1", 0x70), ("F2", 0x71), ("F3", 0x72), ("F4", 0x73), ("F5", 0x74),
    ("F6", 0x75), ("F7", 0x76), ("F8", 0x77), ("F9", 0x78), ("F10", 0x79),
    ("F11", 0x7A), ("F12", 0x7B), ("F13", 0x7C), ("F14", 0x7D), ("F15", 0x7E),
    ("F16", 0x7F), ("F17", 0x80), ("F18", 0x81), ("F19", 0x82), ("F20", 0x83),
    ("F21", 0x84), ("F22", 0x85), ("F23", 0x86), ("F24", 0x87),
    ("Left", 0x25), ("Up", 0x26), ("Right", 0x27), ("Down", 0x28),
    ("Space", 0x20), ("Tab", 0x09), ("Enter", 0x0D), ("Return", 0x0D),
    ("Esc", 0x1B), ("Escape", 0x1B), ("Backspace", 0x08),
    ("Insert", 0x2D), ("Ins", 0x2D), ("Delete", 0x2E), ("Del", 0x2E),
    ("Home", 0x24), ("End", 0x23), ("PageUp", 0x21), ("PageDown", 0x22),
    ("Pause", 0x13), ("PrintScreen", 0x2C), ("ScrollLock", 0x91),
    ("Num0", 0x60), ("Num1", 0x61), ("Num2", 0x62), ("Num3", 0x63), ("Num4", 0x64),
    ("Num5", 0x65), ("Num6", 0x66), ("Num7", 0x67), ("Num8", 0x68), ("Num9", 0x69),
    ("NumMultiply", 0x6A), ("NumAdd", 0x6B), ("NumSubtract", 0x6D),
    ("NumDecimal", 0x6E), ("NumDivide", 0x6F),
    ("VolumeMute", 0xAD), ("VolumeDown", 0xAE), ("VolumeUp", 0xAF),
    ("MediaNext", 0xB0), ("MediaPrev", 0xB1), ("MediaStop", 0xB2),
    ("MediaPlayPause", 0xB3),
];

/// Key name -> virtual-key code. Accepts letters/digits, the names in
/// `NAMED`, and `VK_XX` (hex) for any other physical key, which is how
/// layout-specific keys (AZERTY `ù`, `²`, ...) are stored.
pub fn vk_from_name(name: &str) -> Option<u32> {
    let up = name.trim().to_ascii_uppercase();
    if up.len() == 1 {
        let c = up.chars().next().unwrap();
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
    if let Some(hex) = up.strip_prefix("VK_") {
        return u32::from_str_radix(hex, 16)
            .ok()
            .filter(|v| (0x01..=0xFE).contains(v) && !is_modifier_vk(*v));
    }
    NAMED
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&up))
        .map(|(_, v)| *v)
}

/// Shift/Ctrl/Alt/Win (any side): never a hotkey's main key.
fn is_modifier_vk(vk: u32) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

/// Canonical config spelling for a vk (inverse of `vk_from_name`).
pub fn vk_to_name(vk: u32) -> String {
    if (0x30..=0x39).contains(&vk) || (0x41..=0x5A).contains(&vk) {
        return char::from_u32(vk).unwrap().to_string();
    }
    match NAMED.iter().find(|(_, v)| *v == vk) {
        Some((n, _)) => n.to_string(),
        None => format!("VK_{vk:02X}"),
    }
}

/// Human label for a vk on the CURRENT keyboard layout: `VK_DE` shows as
/// `Ù` on AZERTY, `]` on QWERTY.
pub fn vk_label(vk: u32) -> String {
    let name = vk_to_name(vk);
    if !name.starts_with("VK_") {
        return name;
    }
    use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_CHAR};
    let ch = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) } & 0x7FFF_FFFF;
    match char::from_u32(ch) {
        Some(c) if ch > 0x20 => c.to_uppercase().to_string(),
        _ => name,
    }
}

/// Label for a whole binding ("Ctrl+Alt+VK_DE" -> "Ctrl+Alt+Ù"). Unknown
/// parts are kept verbatim so the user sees what is in the config.
pub fn binding_label(binding: &str) -> String {
    binding
        .split('+')
        .map(|p| {
            let t = p.trim();
            match t.to_ascii_lowercase().as_str() {
                "win" | "windows" | "super" => "Win".to_string(),
                "ctrl" | "control" => "Ctrl".to_string(),
                "alt" => "Alt".to_string(),
                "shift" => "Shift".to_string(),
                _ => vk_from_name(t).map(vk_label).unwrap_or_else(|| t.to_string()),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Register all configured hotkeys on `hwnd` (a message window on the UI
/// thread). Unbound/invalid bindings are skipped, never fatal.
pub fn register_all(hwnd: HWND, cfg: &HotkeyConfig) -> Vec<i32> {
    let mut ok = Vec::new();
    let mut rejected = Vec::new();
    for (id, field, binding) in [
        (ID_TOGGLE, "toggle", &cfg.toggle),
        (ID_EDITOR, "editor", &cfg.editor),
        (ID_NEXT_PRESET, "next_preset", &cfg.next_preset),
        (ID_NEXT_ZONE, "next_zone", &cfg.next_zone),
        (ID_PREV_ZONE, "prev_zone", &cfg.prev_zone),
        (ID_FS_TOGGLE, "fs_toggle", &cfg.fs_toggle),
        (ID_CHEATSHEET, "cheatsheet", &cfg.cheatsheet),
    ] {
        if let Some((mods, vk)) = parse(binding) {
            unsafe {
                if RegisterHotKey(Some(hwnd), id, mods | MOD_NOREPEAT, vk).is_ok() {
                    ok.push(id);
                } else {
                    crate::log::line(format!("hotkey '{binding}' rejected by OS"));
                    rejected.push(field.to_string());
                }
            }
        }
    }
    // plans: modifiers + top-row digit 1..9
    for (base, field, mods) in [
        (ID_PLAN_GO, "plan_go", &cfg.plan_go),
        (ID_PLAN_SAVE, "plan_save", &cfg.plan_save),
    ] {
        let Some(m) = parse_mods(mods) else { continue };
        let mut failed = false;
        for n in 1..=9u32 {
            let id = base + n as i32;
            unsafe {
                if RegisterHotKey(Some(hwnd), id, m | MOD_NOREPEAT, 0x30 + n).is_ok() {
                    ok.push(id);
                } else {
                    failed = true;
                }
            }
        }
        if failed {
            crate::log::line(format!("plan hotkeys '{mods}+1..9' partly rejected by OS"));
            rejected.push(field.to_string());
        }
    }
    *REJECTED.lock().unwrap() = rejected;
    ok
}

static REJECTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Config field names (`toggle`, `editor`, ...) whose binding the OS
/// refused at the last `register_all` (taken by another app).
pub fn rejected() -> Vec<String> {
    REJECTED.lock().unwrap().clone()
}

pub fn unregister_all(hwnd: HWND, ids: &[i32]) {
    unsafe {
        for id in ids {
            let _ = UnregisterHotKey(Some(hwnd), *id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_win_alt_z() {
        let (m, k) = parse("Win+Alt+Z").unwrap();
        assert_eq!(k, 'Z' as u32);
        assert_eq!(m.0 & MOD_WIN.0, MOD_WIN.0);
        assert_eq!(m.0 & MOD_ALT.0, MOD_ALT.0);
    }

    #[test]
    fn parse_bad_is_none() {
        assert!(parse("Win+").is_none());
        assert!(parse("nonsense+keys").is_none());
    }

    #[test]
    fn layout_keys_roundtrip_as_vk_tokens() {
        assert_eq!(vk_from_name("VK_DE"), Some(0xDE));
        assert_eq!(vk_from_name("vk_c0"), Some(0xC0));
        assert_eq!(vk_to_name(0xDE), "VK_DE");
        assert_eq!(vk_to_name(0x41), "A");
        assert_eq!(vk_to_name(0x7A), "F11");
        assert_eq!(vk_to_name(0x0D), "Enter");
        assert_eq!(parse("Ctrl+Alt+VK_DE").unwrap().1, 0xDE);
        // modifiers are never a main key, bad hex rejected
        assert!(vk_from_name("VK_10").is_none());
        assert!(vk_from_name("VK_ZZ").is_none());
        assert!(vk_from_name("VK_FF").is_none());
        assert_eq!(parse("Ctrl+F20").unwrap().1, 0x83);
        assert_eq!(parse("Win+Num5").unwrap().1, 0x65);
    }

    #[test]
    fn plan_modifier_sets() {
        assert!(parse_mods("Alt+Shift").is_some());
        assert!(parse_mods("Ctrl+Alt+Shift").is_some());
        assert!(parse_mods("").is_none());
        assert!(parse_mods("Alt+Q").is_none());
        assert!(parse_mods("Banana").is_none());
    }

    #[test]
    fn parse_f_keys_and_arrows() {
        assert_eq!(parse("Ctrl+F11").unwrap().1, 0x7A);
        assert_eq!(parse("Win+Alt+Right").unwrap().1, 0x27);
    }
}
