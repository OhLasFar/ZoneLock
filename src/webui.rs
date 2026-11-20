//! Settings window hosted in WebView2. The page (src/web/settings.html)
//! is compiled into the exe and loaded with NavigateToString: no files,
//! no network. It talks to us through postMessage JSON (see uimodel.rs).
//!
//! WebView2 is created only while the window is open and fully torn down
//! on close, so the idle daemon pays nothing for it. If the WebView2
//! runtime is missing or fails, we fall back to the GDI panel
//! (settings.rs).

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{w, Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::tray::UiCmd;
use crate::uimodel::{self, PageMsg};
use crate::watcher::Shared;

const CLASS: PCWSTR = w!("ZoneLockWebSettings");
const PAGE: &str = include_str!("web/settings.html");

// Dispattern type stack, embedded (no network). Licenses: web/fonts/OFL.txt
const FONTS: [(&str, &[u8]); 3] = [
    ("Figtree", include_bytes!("web/fonts/figtree-latin-wght-normal.woff2")),
    ("Syne", include_bytes!("web/fonts/syne-latin-wght-normal.woff2")),
    ("JetBrains Mono", include_bytes!("web/fonts/jetbrains-mono-latin-wght-normal.woff2")),
];

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The page with its @font-face rules filled in.
fn page_html() -> String {
    let faces: String = FONTS
        .iter()
        .map(|(name, bytes)| {
            format!(
                "@font-face{{font-family:\"{name}\";font-weight:100 900;font-display:block;\
                 src:url(data:font/woff2;base64,{}) format(\"woff2\")}}\n",
                base64(bytes)
            )
        })
        .collect();
    PAGE.replacen("/*@FONTS@*/", &faces, 1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn page_gets_fonts() {
        let p = super::page_html();
        assert!(!p.contains("/*@FONTS@*/"));
        assert_eq!(p.matches("@font-face").count(), 3);
    }
}

// logical (96 dpi) client size
const W: i32 = 940;
const H: i32 = 660;
const MIN_W: i32 = 680;
const MIN_H: i32 = 460;

struct Host {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
}

thread_local! {
    static WIN: Cell<isize> = const { Cell::new(0) };
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
    static QUIT_AFTER: Cell<bool> = const { Cell::new(false) };
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn dpi_of(hwnd: HWND) -> u32 {
    unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) }
}

fn sx(v: i32, dpi: u32) -> i32 {
    crate::uidraw::sx(v, dpi)
}

/// Is a usable WebView2 runtime installed?
pub fn runtime_available() -> bool {
    let mut ver = PWSTR::null();
    let ok = unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut ver) }
        .is_ok()
        && !ver.is_null();
    if !ver.is_null() {
        let v = take_pwstr(ver);
        crate::log::line(format!("webview2 runtime {v}"));
    }
    ok
}

pub fn is_open() -> bool {
    WIN.with(|w| w.get() != 0)
}

/// Open (or focus) the settings window; GDI fallback without WebView2.
pub fn toggle(shared: Arc<Shared>) {
    if let Some(h) = WIN.with(|w| {
        let v = w.get();
        (v != 0).then_some(HWND(v as *mut _))
    }) {
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    if crate::settings::is_open() || !runtime_available() {
        crate::settings::toggle(shared);
        return;
    }
    SHARED.with(|s| *s.borrow_mut() = Some(Arc::clone(&shared)));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        match create_window() {
            Some(hwnd) => {
                WIN.with(|w| w.set(hwnd.0 as isize));
                if let Err(e) = start_webview() {
                    fallback(hwnd, &format!("{e:?}"));
                }
            }
            None => crate::settings::toggle(shared),
        }
    }
}

/// WebView2 failed: close our window and show the GDI panel instead.
fn fallback(hwnd: HWND, why: &str) {
    crate::log::line(format!("webview2 settings failed ({why}), using GDI panel"));
    let shared = SHARED.with(|s| s.borrow().clone());
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    if let Some(sh) = shared {
        crate::settings::toggle(sh);
    }
}

unsafe fn create_window() -> Option<HWND> {
    let hinst = GetModuleHandleW(None).ok()?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: CLASS,
        hIcon: crate::icon::app_icon(32),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        // page background colour: no white flash before first paint
        hbrBackground: windows::Win32::Graphics::Gdi::CreateSolidBrush(crate::uidraw::cr(
            crate::uidraw::BG,
        )),
        ..Default::default()
    };
    if !REGISTERED.with(|r| r.replace(true)) {
        RegisterClassExW(&wc);
    } else {
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(wc.hbrBackground.into());
    }

    // size for the monitor under the cursor (where the tray click was)
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let hmon = windows::Win32::Graphics::Gdi::MonitorFromPoint(
        pt,
        windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
    );
    let mut mi = windows::Win32::Graphics::Gdi::MONITORINFO {
        cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = windows::Win32::Graphics::Gdi::GetMonitorInfoW(hmon, &mut mi);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(
        hmon,
        windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
        &mut dx,
        &mut dy,
    );
    let dpi = dx.max(96);
    let wa = mi.rcWork;

    let style = WS_OVERLAPPEDWINDOW;
    let mut wr = RECT {
        left: 0,
        top: 0,
        right: sx(W, dpi),
        bottom: sx(H, dpi),
    };
    let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
        &mut wr,
        style,
        false,
        WINDOW_EX_STYLE(0),
        dpi,
    );
    let ww = (wr.right - wr.left).min(wa.right - wa.left);
    let wh = (wr.bottom - wr.top).min(wa.bottom - wa.top);
    let x = wa.left + (wa.right - wa.left - ww) / 2;
    let y = wa.top + (wa.bottom - wa.top - wh) / 2;

    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        CLASS,
        w!("ZoneLock settings"),
        style,
        x,
        y,
        ww,
        wh,
        None,
        None,
        Some(hinst.into()),
        None,
    )
    .ok()?;
    let dark: i32 = 1;
    let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
        hwnd,
        windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(20), // DWMWA_USE_IMMERSIVE_DARK_MODE
        &dark as *const _ as *const _,
        4,
    );
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    Some(hwnd)
}

fn data_dir() -> Vec<u16> {
    wide(&crate::config::config_dir().join("WebView2").to_string_lossy())
}

unsafe fn start_webview() -> windows::core::Result<()> {
    let options = CoreWebView2EnvironmentOptions::default();
    // The page is local: keep the browser from phoning home on its own.
    // Low-end PCs: one static page needs no GPU. `--disable-gpu` alone
    // falls back to SwiftShader in a ~70 MB GPU process; adding
    // `--disable-software-rasterizer` composites in software instead.
    // Measured 2026-11-19 (private bytes, panel open): 161 MB default ->
    // 101 MB with these flags, same open time (~380 ms warm).
    options.set_additional_browser_arguments(
        "--disable-background-networking --disable-component-update --disable-sync \
         --no-first-run --disable-features=msSmartScreenProtection --disable-gpu --disable-software-rasterizer"
            .to_string(),
    );
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let dir = data_dir();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |hr: windows::core::Result<()>, env: Option<ICoreWebView2Environment>| {
            if !is_open() {
                return Ok(()); // window closed while starting
            }
            let hwnd = HWND(WIN.with(|w| w.get()) as *mut _);
            match (hr, env) {
                (Ok(()), Some(env)) => {
                    let ch = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                        move |hr: windows::core::Result<()>,
                              ctl: Option<ICoreWebView2Controller>| {
                            match (hr, ctl) {
                                (Ok(()), Some(ctl)) => {
                                    if let Err(e) = attach(hwnd, ctl) {
                                        fallback(hwnd, &format!("{e:?}"));
                                    }
                                }
                                (hr, _) => fallback(hwnd, &format!("controller {hr:?}")),
                            }
                            Ok(())
                        },
                    ));
                    if let Err(e) = env.CreateCoreWebView2Controller(hwnd, &ch) {
                        fallback(hwnd, &format!("{e:?}"));
                    }
                }
                (hr, _) => fallback(hwnd, &format!("environment {hr:?}")),
            }
            Ok(())
        },
    ));
    CreateCoreWebView2EnvironmentWithOptions(
        PCWSTR::null(),
        PCWSTR(dir.as_ptr()),
        &options,
        &handler,
    )
}

unsafe fn attach(hwnd: HWND, controller: ICoreWebView2Controller) -> windows::core::Result<()> {
    if !is_open() {
        let _ = controller.Close();
        return Ok(());
    }
    if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
        let bg = crate::uidraw::BG; // 0x00BBGGRR
        let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
            A: 255,
            R: (bg & 0xFF) as u8,
            G: ((bg >> 8) & 0xFF) as u8,
            B: ((bg >> 16) & 0xFF) as u8,
        });
    }
    let webview = controller.CoreWebView2()?;
    if let Ok(w19) = webview.cast::<ICoreWebView2_19>() {
        let _ = w19.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW);
    }
    let settings = webview.Settings()?;
    let _ = settings.SetIsStatusBarEnabled(false);
    let _ = settings.SetIsZoomControlEnabled(false);
    let _ = settings.SetAreDefaultContextMenusEnabled(cfg!(debug_assertions));
    let _ = settings.SetAreDevToolsEnabled(cfg!(debug_assertions));
    let _ = settings.SetIsBuiltInErrorPageEnabled(false);
    if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
        let _ = s3.SetAreBrowserAcceleratorKeysEnabled(cfg!(debug_assertions));
    }
    if let Ok(s5) = settings.cast::<ICoreWebView2Settings5>() {
        let _ = s5.SetIsPinchZoomEnabled(false);
    }
    if let Ok(s8) = settings.cast::<ICoreWebView2Settings8>() {
        let _ = s8.SetIsReputationCheckingRequired(false);
    }

    let mut token = 0i64;
    // the only document ever shown is ours: block every navigation away
    webview.add_NavigationStarting(
        &NavigationStartingEventHandler::create(Box::new(|_, args| {
            if let Some(args) = args {
                let mut uri = PWSTR::null();
                args.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                if !(uri.starts_with("data:") || uri.starts_with("about:")) {
                    args.SetCancel(true)?;
                }
            }
            Ok(())
        })),
        &mut token,
    )?;
    webview.add_NewWindowRequested(
        &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
            if let Some(args) = args {
                args.SetHandled(true)?;
            }
            Ok(())
        })),
        &mut token,
    )?;
    webview.add_WebMessageReceived(
        &WebMessageReceivedEventHandler::create(Box::new(|_, args| {
            if let Some(args) = args {
                let mut json = PWSTR::null();
                args.WebMessageAsJson(&mut json)?;
                on_message(&take_pwstr(json));
            }
            Ok(())
        })),
        &mut token,
    )?;

    resize(hwnd, &controller);
    controller.SetIsVisible(true)?;
    let page = wide(&page_html());
    webview.NavigateToString(PCWSTR(page.as_ptr()))?;
    let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
    HOST.with(|h| *h.borrow_mut() = Some(Host { controller, webview }));
    crate::log::line("webview2 settings ready");
    Ok(())
}

unsafe fn resize(hwnd: HWND, controller: &ICoreWebView2Controller) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let _ = controller.SetBounds(rc);
}

fn post(v: serde_json::Value) {
    let s = wide(&v.to_string());
    HOST.with(|h| {
        if let Some(h) = h.borrow().as_ref() {
            unsafe {
                let _ = h.webview.PostWebMessageAsJson(PCWSTR(s.as_ptr()));
            }
        }
    });
}

fn state_json(shared: &Shared) -> serde_json::Value {
    let cfg = shared.config.read().unwrap().clone();
    let mons = shared.monitors.read().unwrap().clone();
    let mut st = uimodel::build_state(&cfg, crate::autostart::is_enabled(), &mons);
    st.hotkeys_rejected = crate::hotkeys::rejected();
    st.update = crate::update::view();
    let bindings = [
        &cfg.hotkeys.toggle,
        &cfg.hotkeys.editor,
        &cfg.hotkeys.next_preset,
        &cfg.hotkeys.next_zone,
        &cfg.hotkeys.prev_zone,
        &cfg.hotkeys.fs_toggle,
        &cfg.hotkeys.cheatsheet,
        &cfg.hotkeys.plan_go,
        &cfg.hotkeys.plan_save,
    ];
    for b in bindings.into_iter().chain(cfg.fs_exit_keys.values()) {
        st.key_labels.insert(b.clone(), crate::hotkeys::binding_label(b));
    }
    serde_json::to_value(st).unwrap_or_default()
}

fn close() {
    let h = WIN.with(|w| w.get());
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn on_message(json: &str) {
    let Some(shared) = SHARED.with(|s| s.borrow().clone()) else {
        return;
    };
    let msg: PageMsg = match serde_json::from_str(json) {
        Ok(m) => m,
        Err(e) => {
            crate::log::line(format!("settings page: bad message {e}"));
            return;
        }
    };
    let cur = shared.config.read().unwrap().clone();
    let mons = shared.monitors.read().unwrap().clone();
    let result = match msg {
        PageMsg::Ready => {
            post(serde_json::json!({ "type": "state", "state": state_json(&shared) }));
            return;
        }
        PageMsg::Capture { on } => {
            if CAPTURING.with(|c| c.replace(on)) != on {
                crate::tray::send(if on { UiCmd::SuspendHotkeys } else { UiCmd::RebindHotkeys });
            }
            return;
        }
        PageMsg::Close => return close(),
        PageMsg::Quit => {
            QUIT_AFTER.with(|q| q.set(true));
            return close();
        }
        PageMsg::OpenEditor => return crate::tray::send(UiCmd::OpenEditor),
        PageMsg::OpenConfigDir => return crate::tray::send(UiCmd::OpenConfigDir),
        PageMsg::UpdateCheck => return crate::update::check(false),
        PageMsg::UpdateInstall => return crate::update::install(),
        PageMsg::UpdatePage => return crate::update::open_page(),
        PageMsg::OpenCredit => {
            unsafe {
                let _ = windows::Win32::UI::Shell::ShellExecuteW(
                    None,
                    w!("open"),
                    w!("https://x.com/LasFar"),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOW,
                );
            }
            return;
        }
        PageMsg::Apply { data } => uimodel::apply_settings(&cur, &data).and_then(|mut cfg| {
            if data.autostart != crate::autostart::is_enabled() {
                crate::autostart::set_enabled(data.autostart).map_err(|e| uimodel::UiError {
                    field: "autostart".into(),
                    message: format!("Could not change autostart: {e}"),
                })?;
            }
            cfg.autostart = data.autostart;
            Ok(cfg)
        }),
        PageMsg::UsePreset { monitor, name } => uimodel::use_preset(&cur, &mons, &monitor, &name),
        PageMsg::SavePreset { monitor, name } => uimodel::save_preset(&cur, &mons, &monitor, &name),
        PageMsg::DeletePreset { name } => uimodel::delete_preset(&cur, &name),
        PageMsg::PlanSave { slot } => crate::plans::save_current(&shared, slot)
            .map_err(|e| uimodel::UiError { field: "plans".into(), message: e })
            .and_then(|plan| uimodel::set_plan(&cur, slot, plan)),
        PageMsg::PlanClear { slot } => Ok(uimodel::clear_plan(&cur, slot)),
        PageMsg::PlanGo { slot } => {
            if let Err(e) = crate::plans::go(&shared, slot) {
                post(serde_json::json!({ "type": "error", "error": { "field": "plans", "message": e } }));
            }
            return;
        }
        PageMsg::NewPreset { monitor, name } => {
            match uimodel::check_new_preset_name(&cur, &name) {
                Ok(name) if mons.iter().any(|m| m.name == monitor) => {
                    crate::overlay::open_for_preset(Arc::clone(&shared), monitor, name);
                    return;
                }
                Ok(_) => Err(uimodel::UiError {
                    field: "monitors".into(),
                    message: "That display is no longer connected.".into(),
                }),
                Err(e) => Err(e),
            }
        }
        PageMsg::EditPreset { monitor, name } => {
            // show the preset on that display first, so the editor starts
            // from its zones; Enter then overwrites the preset
            if !cur.user_presets.iter().any(|p| p.name == name) {
                Err(uimodel::UiError {
                    field: "presets".into(),
                    message: "Only your own presets can be edited.".into(),
                })
            } else {
                match uimodel::use_preset(&cur, &mons, &monitor, &name)
                    .and_then(|cfg| commit(&shared, cfg))
                {
                    Ok(()) => {
                        crate::overlay::open_for_preset(Arc::clone(&shared), monitor, name);
                        return;
                    }
                    Err(e) => Err(e),
                }
            }
        }
    };
    match result.and_then(|cfg| commit(&shared, cfg)) {
        Ok(()) => post(serde_json::json!({ "type": "saved", "state": state_json(&shared) })),
        Err(e) => post(serde_json::json!({ "type": "error", "error": e })),
    }
}

/// Persist + publish a validated config.
fn commit(shared: &Arc<Shared>, cfg: crate::config::Config) -> std::result::Result<(), uimodel::UiError> {
    cfg.save().map_err(|e| uimodel::UiError {
        field: String::new(),
        message: format!("Could not write config: {e}"),
    })?;
    let (log_on, enabled) = (cfg.log_enabled, cfg.enabled);
    let hk_changed = shared.config.read().unwrap().hotkeys != cfg.hotkeys;
    *shared.config.write().unwrap() = cfg;
    shared.bump();
    crate::log::set_enabled(log_on);
    crate::tray::set_enabled_hint(enabled);
    if hk_changed {
        crate::tray::send(UiCmd::RebindHotkeys);
    }
    Ok(())
}

/// Push fresh state to an open page (tray / hotkey changed something).
pub fn refresh() {
    if !is_open() {
        return;
    }
    if let Some(shared) = SHARED.with(|s| s.borrow().clone()) {
        post(serde_json::json!({ "type": "sync", "state": state_json(&shared) }));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            HOST.with(|h| {
                if let Some(h) = h.borrow().as_ref() {
                    resize(hwnd, &h.controller);
                }
            });
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            let dpi = dpi_of(hwnd);
            mmi.ptMinTrackSize.x = sx(MIN_W, dpi);
            mmi.ptMinTrackSize.y = sx(MIN_H, dpi);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let sug = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                sug.left,
                sug.top,
                sug.right - sug.left,
                sug.bottom - sug.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            LRESULT(0)
        }
        WM_MOVE | WM_MOVING => {
            HOST.with(|h| {
                if let Some(h) = h.borrow().as_ref() {
                    let _ = h.controller.NotifyParentWindowPositionChanged();
                }
            });
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETFOCUS => {
            HOST.with(|h| {
                if let Some(h) = h.borrow().as_ref() {
                    let _ = h.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // tear WebView2 down completely: browser processes exit and
            // the idle daemon is back to its tiny footprint
            if let Some(h) = HOST.with(|h| h.borrow_mut().take()) {
                let _ = h.controller.Close();
            }
            WIN.with(|w| w.set(0));
            if CAPTURING.with(|c| c.replace(false)) {
                crate::tray::send(UiCmd::RebindHotkeys);
            }
            if QUIT_AFTER.with(|q| q.replace(false)) {
                crate::tray::send(UiCmd::Exit);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
