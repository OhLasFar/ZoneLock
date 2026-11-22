//! Program icons for the zone editor (UI thread): extracted from the
//! program file at the asked size and cached for the process lifetime
//! (a handful of apps). When the program path is unknown, the per-app
//! "App Paths" registry entry is tried (how Windows itself finds
//! chrome.exe, firefox.exe...).

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::UI::Shell::SHDefExtractIconW;
use windows::Win32::UI::WindowsAndMessaging::HICON;

thread_local! {
    /// (program, size) -> HICON handle (0 = none found).
    static CACHE: RefCell<HashMap<(String, i32), isize>> = RefCell::new(HashMap::new());
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `App Paths\<exe>` default value, unquoted ("" when absent).
fn app_path(exe: &str) -> String {
    let key = wide(&format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}"));
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if let Some(p) = reg_sz(root, &key) {
            return p.trim().trim_matches('"').to_string();
        }
    }
    String::new()
}

fn reg_sz(root: HKEY, key: &[u16]) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            root,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    } == ERROR_SUCCESS;
    if !ok {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// Icon of program `exe` (full `path` when known) at `size` px.
pub fn get(exe: &str, path: &str, size: i32) -> Option<HICON> {
    let id = if path.is_empty() { exe.to_ascii_lowercase() } else { path.to_ascii_lowercase() };
    let key = (id, size);
    if let Some(h) = CACHE.with(|c| c.borrow().get(&key).copied()) {
        return (h != 0).then_some(HICON(h as *mut _));
    }
    let file = if path.is_empty() { app_path(exe) } else { path.to_string() };
    let mut icon = HICON::default();
    if !file.is_empty() {
        let w = wide(&file);
        let s = size.clamp(16, 256) as u32;
        unsafe {
            let _ = SHDefExtractIconW(PCWSTR(w.as_ptr()), 0, 0, Some(&mut icon), None, s);
        }
    }
    CACHE.with(|c| c.borrow_mut().insert(key, icon.0 as isize));
    (!icon.is_invalid()).then_some(icon)
}
