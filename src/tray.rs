//! System tray icon + hidden message window. Lives on the UI thread.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Commands the UI thread sends to the app controller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiCmd {
    ToggleEnabled,
    OpenEditor,
    NextPreset,
    NextZone,
    PrevZone,
    /// Active window's fullscreen: in its zone <-> whole display.
    FsToggle,
    /// Show / hide the shortcuts card.
    Cheatsheet,
    ToggleAutostart,
    OpenConfigDir,
    OpenSettings,
    RebindHotkeys,
    SuspendHotkeys,
    /// Go to plan n (1-9).
    PlanGo(u8),
    /// Save the current windows as plan n (1-9).
    PlanSave(u8),
    /// Update status changed (worker thread -> UI thread): refresh the page.
    UpdateChanged,
    /// The display setup changed (watcher -> UI thread); Some(n) = arrange
    /// plan n, linked to the new setup.
    SetupChanged(Option<u8>),
    /// Another virtual desktop is shown (watcher -> UI thread); Some(n) =
    /// arrange plan n, linked to that desktop.
    DesktopChanged(Option<u8>),
    Exit,
}

pub const TRAY_MSG: u32 = WM_APP + 1;
/// Posted by the update worker: wparam 1 = also show the "new version"
/// balloon.
const UPDATE_MSG: u32 = WM_APP + 2;
/// Posted by the watcher: wparam = plan slot to arrange, 0 = none.
const SETUP_MSG: u32 = WM_APP + 3;
/// The virtual desktop shown changed (wparam = plan to arrange, 0 = none).
const DESKTOP_MSG: u32 = WM_APP + 4;
/// Balloon clicked (NIN_BALLOONUSERCLICK = WM_USER + 5).
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

/// The tray window, for posts from other threads.
static TRAY_WND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn post_tray(msg: u32, wparam: usize) {
    let h = TRAY_WND.load(std::sync::atomic::Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(wparam), LPARAM(0));
        }
    }
}

/// From any thread: the update status changed (alloon: announce it).
pub fn post_update(balloon: bool) {
    post_tray(UPDATE_MSG, balloon as usize);
}

/// From the watcher: the display setup changed (Some = arrange that plan).
pub fn post_setup(plan: Option<u8>) {
    post_tray(SETUP_MSG, plan.unwrap_or(0) as usize);
}

/// From the watcher: another virtual desktop is shown (Some = arrange
/// that plan).
pub fn post_desktop(plan: Option<u8>) {
    post_tray(DESKTOP_MSG, plan.unwrap_or(0) as usize);
}

/// From any thread: quit ZoneLock cleanly (after starting an update).
pub fn post_quit() {
    post_tray(WM_COMMAND, IDM_EXIT as usize);
}
// The tray menu is deliberately tiny: everything else lives in the
// settings panel (left click). Ids are stable (probe --traycmd uses 107).
const IDM_TOGGLE: u32 = 100;
const IDM_CONFIG: u32 = 107;
const IDM_EXIT: u32 = 106;

/// Mirror of `config.enabled` for the menu check mark (the tray has no
/// access to Shared); updated by main and the settings panel.
static ENABLED_HINT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_enabled_hint(on: bool) {
    ENABLED_HINT.store(on, std::sync::atomic::Ordering::Relaxed);
}

const CLASS: PCWSTR = w!("ZoneLockTray");

/// Global handler slot: the wndproc is a plain function so it needs a
/// static. Main installs it before creating the window.
static HANDLER: std::sync::OnceLock<Handler> = std::sync::OnceLock::new();

type Handler = Box<dyn Fn(UiCmd) + Send + Sync>;

fn handler() -> Option<&'static Handler> {
    HANDLER.get()
}

/// Deliver a command to the installed handler from anywhere on the UI
/// thread (used by the settings panel to trigger a hotkey rebind).
pub fn send(cmd: UiCmd) {
    if let Some(h) = handler() {
        h(cmd);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        x if x == TRAY_MSG => {
            let evt = lparam.0 as u32;
            if evt == WM_LBUTTONUP || evt == NIN_BALLOONUSERCLICK {
                // left click opens the dashboard, right click the menu
                if let Some(h) = handler() {
                    h(UiCmd::OpenSettings);
                }
            } else if evt == WM_RBUTTONUP || evt == WM_CONTEXTMENU {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u32;
            if let (Some(cmd), Some(h)) = (menu_to_cmd(id), handler()) {
                h(cmd);
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            let id = wparam.0 as i32;
            let cmd = match id {
                crate::hotkeys::ID_TOGGLE => Some(UiCmd::ToggleEnabled),
                crate::hotkeys::ID_EDITOR => Some(UiCmd::OpenEditor),
                crate::hotkeys::ID_NEXT_PRESET => Some(UiCmd::NextPreset),
                crate::hotkeys::ID_NEXT_ZONE => Some(UiCmd::NextZone),
                crate::hotkeys::ID_PREV_ZONE => Some(UiCmd::PrevZone),
                crate::hotkeys::ID_FS_TOGGLE => Some(UiCmd::FsToggle),
                crate::hotkeys::ID_CHEATSHEET => Some(UiCmd::Cheatsheet),
                i if (crate::hotkeys::ID_PLAN_GO + 1..=crate::hotkeys::ID_PLAN_GO + 9).contains(&i) => {
                    Some(UiCmd::PlanGo((i - crate::hotkeys::ID_PLAN_GO) as u8))
                }
                i if (crate::hotkeys::ID_PLAN_SAVE + 1..=crate::hotkeys::ID_PLAN_SAVE + 9).contains(&i) => {
                    Some(UiCmd::PlanSave((i - crate::hotkeys::ID_PLAN_SAVE) as u8))
                }
                _ => None,
            };
            if let (Some(c), Some(h)) = (cmd, handler()) {
                h(c);
            }
            LRESULT(0)
        }
        x if x == SETUP_MSG => {
            if let Some(h) = handler() {
                h(UiCmd::SetupChanged((wparam.0 != 0).then_some(wparam.0 as u8)));
            }
            LRESULT(0)
        }
        x if x == DESKTOP_MSG => {
            if let Some(h) = handler() {
                h(UiCmd::DesktopChanged((wparam.0 != 0).then_some(wparam.0 as u8)));
            }
            LRESULT(0)
        }
        x if x == UPDATE_MSG => {
            if wparam.0 == 1 {
                show_update_balloon(hwnd);
            }
            if let Some(h) = handler() {
                h(UiCmd::UpdateChanged);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn menu_to_cmd(id: u32) -> Option<UiCmd> {
    Some(match id {
        IDM_TOGGLE => UiCmd::ToggleEnabled,
        IDM_CONFIG => UiCmd::OpenSettings,
        IDM_EXIT => UiCmd::Exit,
        _ => return None,
    })
}

/// ZoneLock X is available balloon; a click opens Settings.
unsafe fn show_update_balloon(hwnd: HWND) {
    let v = crate::update::view();
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: windows::Win32::UI::Shell::NIF_INFO,
        dwInfoFlags: windows::Win32::UI::Shell::NIIF_INFO,
        ..Default::default()
    };
    let put = |dst: &mut [u16], text: &str| {
        let w: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
        dst[..w.len()].copy_from_slice(&w);
    };
    put(&mut nid.szInfoTitle, &format!("ZoneLock {} is available", v.latest));
    put(&mut nid.szInfo, "Click to open Settings and install it.");
    let _ = Shell_NotifyIconW(windows::Win32::UI::Shell::NIM_MODIFY, &mut nid);
}

unsafe fn show_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let s = |t: &str| {
        t.encode_utf16().chain(Some(0)).collect::<Vec<u16>>()
    };
    let on = ENABLED_HINT.load(std::sync::atomic::Ordering::Relaxed);
    let _ = AppendMenuW(menu, MF_STRING, IDM_CONFIG as usize, PCWSTR(s("Settings\u{2026}").as_ptr()));
    let _ = AppendMenuW(
        menu,
        MF_STRING | if on { MF_CHECKED } else { MF_UNCHECKED },
        IDM_TOGGLE as usize,
        PCWSTR(s("Active").as_ptr()),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, IDM_EXIT as usize, PCWSTR(s("Quit ZoneLock").as_ptr()));
    let _ = SetMenuDefaultItem(menu, IDM_CONFIG, 0);

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenuEx(
        menu,
        (TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN).0,
        pt.x,
        pt.y,
        hwnd,
        None,
    );
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
}

pub struct Tray {
    pub hwnd: HWND,
    icon_added: bool,
}

impl Tray {
    /// Create the message window + tray icon. `on_cmd` receives every user
    /// command (menu items and hotkeys alike).
    pub fn new(on_cmd: Handler) -> Result<Tray, String> {
        let _ = HANDLER.set(on_cmd);
        unsafe {
            let hinst = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS,
                w!("ZoneLock"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinst.into()),
                None,
            )
            .map_err(|e| e.to_string())?;
            TRAY_WND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);

            let icon = crate::icon::app_icon(32);

            let mut tip = [0u16; 128];
            let text: Vec<u16> = "ZoneLock - zones for maximize & F11"
                .encode_utf16()
                .collect();
            let n = text.len().min(127);
            tip[..n].copy_from_slice(&text[..n]);

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: TRAY_MSG,
                hIcon: icon,
                szTip: tip,
                ..Default::default()
            };
            let added = Shell_NotifyIconW(NIM_ADD, &mut nid).as_bool();
            Ok(Tray {
                hwnd,
                icon_added: added,
            })
        }
    }

    /// Set the checked state of the two toggleable menu entries on next
    /// popup; Windows tray menus are rebuilt per open so nothing else needed.
    pub fn refresh_tooltip(&self, enabled: bool) {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: 1,
                uFlags: NIF_TIP,
                ..Default::default()
            };
            let text = format!(
                "ZoneLock - {}",
                if enabled { "active" } else { "paused" }
            );
            let v: Vec<u16> = text.encode_utf16().take(127).collect();
            nid.szTip[..v.len()].copy_from_slice(&v);
            let _ = Shell_NotifyIconW(
                windows::Win32::UI::Shell::NIM_MODIFY,
                &mut nid,
            );
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.icon_added {
            unsafe {
                let mut nid = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.hwnd,
                    uID: 1,
                    ..Default::default()
                };
                let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
            }
        }
    }
}
