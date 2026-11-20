//! "Start with Windows" via HKCU Run key. No admin required (HKCU only).

use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, REG_SZ, REG_VALUE_TYPE,
};

const RUN_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "ZoneLock";

fn with_run_key(mut f: impl FnMut(HKEY)) {
    unsafe {
        let mut hkey = HKEY::default();
        let path: Vec<u16> = RUN_PATH.encode_utf16().chain(Some(0)).collect();
        if RegCreateKeyW(
            HKEY_CURRENT_USER,
            windows::core::PCWSTR(path.as_ptr()),
            &mut hkey,
        )
        .is_ok()
        {
            f(hkey);
            let _ = RegCloseKey(hkey);
        }
    }
}

pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let val = format!("\"{}\"", exe.display());
    let mut data: Vec<u16> = val.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe {
        std::slice::from_raw_parts(data.as_mut_ptr() as *const u8, data.len() * 2)
    };
    let name: Vec<u16> = VALUE_NAME.encode_utf16().chain(Some(0)).collect();
    let mut res = Err("RegCreateKey failed".to_string());
    with_run_key(|hkey| unsafe {
        let r = RegSetValueExW(
            hkey,
            windows::core::PCWSTR(name.as_ptr()),
            Some(0),
            REG_SZ,
            Some(bytes),
        );
        res = if r.is_ok() {
            Ok(())
        } else {
            Err(format!("RegSetValueExW error {}", r.0))
        };
    });
    res
}

pub fn disable() {
    let name: Vec<u16> = VALUE_NAME.encode_utf16().chain(Some(0)).collect();
    with_run_key(|hkey| unsafe {
        let _ = RegDeleteValueW(hkey, windows::core::PCWSTR(name.as_ptr()));
    });
}

pub fn is_enabled() -> bool {
    let mut found = false;
    let name: Vec<u16> = VALUE_NAME.encode_utf16().chain(Some(0)).collect();
    with_run_key(|hkey| unsafe {
        let mut ty = REG_VALUE_TYPE(0);
        let mut size = 0u32;
        let r = windows::Win32::System::Registry::RegQueryValueExW(
            hkey,
            windows::core::PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut size),
        );
        found = r.is_ok();
    });
    found
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    if on {
        enable()
    } else {
        disable();
        Ok(())
    }
}
