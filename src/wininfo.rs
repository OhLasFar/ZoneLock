//! Read-only inspection of a foreign window: geometry, styles, class,
//! owning process, and the "should we even look at it" exclusions.

use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, IsZoomed, GWL_EXSTYLE, GWL_STYLE,
    WS_EX_TOOLWINDOW, WS_VISIBLE, WS_MAXIMIZE,
};

use crate::rect::Rect;

/// Window classes the tool never touches: the shell itself, our own overlay,
/// IME and other pseudo-windows.
pub const BUILTIN_CLASS_DENYLIST: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "DV2ControlHost",
    "MsgrIMEWindowClass",
    "Windows.UI.Core.CoreWindow",
    "ApplicationFrameWindow", // UWP shells are handled by their child check below
    "ZoneLockOverlay",
    "ForegroundStaging",
    "DummyDWMListeningWindow",
];

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub rect: Rect,
    pub style: i64,
    pub ex_style: i64,
    pub class: String,
    pub exe: String,
    pub pid: u32,
    pub visible: bool,
    pub iconic: bool,
    pub zoomed: bool,
    pub cloaked: bool,
    pub valid: bool,
}

impl WindowInfo {
    pub fn is_zoomed(&self) -> bool {
        self.zoomed || (self.style & WS_MAXIMIZE.0 as i64) != 0
    }

    /// Cheap always-on exclusions (shell, invisible, tool windows, ours).
    pub fn excludable(&self, extra_class_prefixes: &[String]) -> bool {
        if !self.valid || !self.visible || self.iconic {
            return true;
        }
        if (self.ex_style & WS_EX_TOOLWINDOW.0 as i64) != 0 {
            return true;
        }
        // child windows (browser render widgets, WebView2 hosts...) are
        // never ours to place: only top-level windows are managed
        if (self.style & windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0 as i64) != 0 {
            return true;
        }
        if self.cloaked {
            return true;
        }
        if (self.style & WS_VISIBLE.0 as i64) == 0 {
            return true;
        }
        if BUILTIN_CLASS_DENYLIST
            .iter()
            .any(|c| self.class.starts_with(c))
        {
            return true;
        }
        extra_class_prefixes
            .iter()
            .any(|p| self.class.starts_with(p.as_str()))
    }
}

pub fn inspect(hwnd: HWND) -> WindowInfo {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as i64;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as i64;

        let mut buf = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut buf);
        let class = String::from_utf16_lossy(&buf[..n as usize]);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let exe = process_exe(pid);

        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        );

        WindowInfo {
            hwnd: hwnd.0 as isize,
            rect: Rect::new(rc.left, rc.top, rc.right, rc.bottom),
            style,
            ex_style,
            class,
            exe,
            pid,
            visible: IsWindowVisible(hwnd).as_bool(),
            iconic: IsIconic(hwnd).as_bool(),
            zoomed: IsZoomed(hwnd).as_bool(),
            cloaked: cloaked != 0,
            valid: IsWindow(Some(hwnd)).as_bool(),
        }
    }
}

fn process_exe(pid: u32) -> String {
    let full = process_path(pid);
    full.rsplit('\\').next().unwrap_or("").to_string()
}

/// Full path of a process' program ("" when it cannot be read).
pub fn process_path(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = vec![0u16; 512];
        let mut len = buf.len() as u32;
        let name = if QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };
        let _ = CloseHandle(h);
        name
    }
}
