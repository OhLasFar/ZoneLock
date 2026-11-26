//! Windows virtual desktops (Win+Ctrl+D / Win+Ctrl+arrows): the list
//! and names, the one shown now, and on which one a window lives.
//!
//! Only the documented `IVirtualDesktopManager` and the registry that
//! Explorer keeps (`HKCU\...\Explorer\VirtualDesktops`). The documented
//! API cannot move another program's window to a desktop or switch
//! desktops: ZoneLock does neither. With a single desktop Windows writes
//! no list, so everything here answers "none" and nothing changes.

/// One virtual desktop, in Windows' order.
#[derive(Debug, Clone, PartialEq)]
pub struct Desktop {
    /// GUID as Windows writes it in the registry: "{25BCECC0-...}".
    pub id: String,
    /// The user's name for it, else "Desktop N".
    pub name: String,
}

/// A GUID in its in-memory layout (what the registry values hold) as
/// "{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}". Pure.
pub fn guid_str(b: &[u8]) -> String {
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    let tail: String = b[10..16].iter().map(|x| format!("{x:02X}")).collect();
    format!("{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{tail}}}", b[8], b[9])
}

/// The ids in a `VirtualDesktopIDs` value (16 bytes each). Pure.
pub fn parse_ids(bytes: &[u8]) -> Vec<String> {
    bytes.chunks_exact(16).map(guid_str).collect()
}

/// Name shown for desktop `index` (0-based): the user's, else Windows'
/// default "Desktop N". Pure.
pub fn label(index: usize, custom: &str) -> String {
    let c = custom.trim();
    if c.is_empty() {
        format!("Desktop {}", index + 1)
    } else {
        c.to_string()
    }
}

pub use win::{current, is_elsewhere, list};

mod win {
    use std::cell::RefCell;

    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Foundation::{ERROR_SUCCESS, HWND};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_SZ};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

    use super::{label, parse_ids, Desktop};

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn reg_binary(key: &str, value: &str) -> Option<Vec<u8>> {
        let (k, v) = (wide(key), wide(value));
        let mut buf = vec![0u8; 16 * 64];
        let mut len = buf.len() as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(k.as_ptr()),
                PCWSTR(v.as_ptr()),
                RRF_RT_REG_BINARY,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            )
        };
        (r == ERROR_SUCCESS).then(|| {
            buf.truncate(len as usize);
            buf
        })
    }

    fn reg_sz(key: &str, value: &str) -> Option<String> {
        let (k, v) = (wide(key), wide(value));
        let mut buf = vec![0u16; 260];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(k.as_ptr()),
                PCWSTR(v.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            )
        };
        (r == ERROR_SUCCESS).then(|| {
            let n = (len as usize / 2).saturating_sub(1);
            String::from_utf16_lossy(&buf[..n.min(buf.len())])
        })
    }

    // kernel32, declared here: the windows crate's RemoteDesktop feature
    // made rustc crash in release builds for this one function
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    }

    /// Windows 10 keeps the per-session values under SessionInfo.
    fn session_key() -> String {
        let mut sid = 0u32;
        let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) };
        format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{sid}\VirtualDesktops")
    }

    fn value(name: &str) -> Option<Vec<u8>> {
        reg_binary(KEY, name).or_else(|| reg_binary(&session_key(), name))
    }

    /// The virtual desktops in Windows' order. Empty with a single one.
    pub fn list() -> Vec<Desktop> {
        let ids = value("VirtualDesktopIDs").map(|b| parse_ids(&b)).unwrap_or_default();
        if ids.len() < 2 {
            return vec![];
        }
        ids.into_iter()
            .enumerate()
            .map(|(i, id)| {
                let name = reg_sz(&format!(r"{KEY}\Desktops\{id}"), "Name").unwrap_or_default();
                Desktop { name: label(i, &name), id }
            })
            .collect()
    }

    /// Id of the desktop shown now, None with a single desktop.
    pub fn current() -> Option<String> {
        let b = value("CurrentVirtualDesktop")?;
        if b.len() < 16 || b.iter().all(|x| *x == 0) {
            return None;
        }
        // with one desktop the value can linger: only report it when
        // there really is a choice
        let ids = value("VirtualDesktopIDs").map(|b| parse_ids(&b)).unwrap_or_default();
        (ids.len() >= 2).then(|| super::guid_str(&b[..16]))
    }

    thread_local! {
        /// Per thread (COM apartment): the watcher and the UI thread.
        static MGR: RefCell<Option<IVirtualDesktopManager>> = const { RefCell::new(None) };
    }

    fn with_mgr<R>(f: impl Fn(&IVirtualDesktopManager) -> windows::core::Result<R>) -> Option<R> {
        MGR.with(|m| {
            let mut m = m.borrow_mut();
            if m.is_none() {
                unsafe {
                    // S_FALSE / RPC_E_CHANGED_MODE: COM is already up here
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                    *m = CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok();
                }
            }
            let r = f(m.as_ref()?);
            if r.is_err() {
                // Explorer restarted: get a fresh object next time
                *m = None;
            }
            r.ok()
        })
    }

    /// The window lives on another virtual desktop than the one shown
    /// (Windows cloaks it). False for windows with no desktop (cloaked
    /// suspended Store apps, tool windows) and with a single desktop.
    pub fn is_elsewhere(hwnd: HWND) -> bool {
        let Some(id) = with_mgr(|m| unsafe { m.GetWindowDesktopId(hwnd) }) else { return false };
        if id == GUID::zeroed() {
            return false;
        }
        with_mgr(|m| unsafe { m.IsWindowOnCurrentVirtualDesktop(hwnd) }).is_some_and(|on| !on.as_bool())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_read_like_the_registry_writes_them() {
        // {25BCECC0-58EB-4980-B6BD-9513551CAE8E} as stored in memory
        let b = [
            0xC0, 0xEC, 0xBC, 0x25, 0xEB, 0x58, 0x80, 0x49, 0xB6, 0xBD, 0x95, 0x13, 0x55, 0x1C, 0xAE, 0x8E,
        ];
        assert_eq!(guid_str(&b), "{25BCECC0-58EB-4980-B6BD-9513551CAE8E}");
        let mut two = b.to_vec();
        two.extend([0u8; 16]);
        two.push(7); // a trailing partial id is ignored
        let ids = parse_ids(&two);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[1], "{00000000-0000-0000-0000-000000000000}");
    }

    #[test]
    fn default_names_count_from_one() {
        assert_eq!(label(0, ""), "Desktop 1");
        assert_eq!(label(2, "  "), "Desktop 3");
        assert_eq!(label(1, " Work "), "Work");
    }
}
