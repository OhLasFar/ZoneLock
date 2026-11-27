//! ZoneLock: keep maximize and windowed fullscreen (F11) inside monitor
//! zones. Windows 10/11, no admin, no DLL injection, fully local config.

#![windows_subsystem = "windows"]

use std::sync::atomic::Ordering;
use std::sync::Arc;

use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use zonelock::config::{self, Config};
use zonelock::hotkeys;
use zonelock::monitor;
use zonelock::overlay;
use zonelock::place;
use zonelock::tray::{Tray, UiCmd};
use zonelock::watcher::{self, Shared};
use zonelock::wininfo;
use zonelock::{autostart, log, presets};

static TRAY_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);
static HK_IDS: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    log::init(config::config_dir().join("zonelock.log"), true);
    std::panic::set_hook(Box::new(|info| {
        log::line(format!("PANIC: {info}"));
    }));

    // `--quit` (the MSI, before replacing or removing files): close the
    // running ZoneLock cleanly and exit. Its tray window is message-only,
    // so Windows Installer cannot ask it to close by itself.
    if std::env::args().any(|a| a == "--quit") {
        zonelock::tray::quit_running();
        return;
    }

    // Started by an update: wait for the old process, clean up after it.
    zonelock::update::after_update_startup();

    // Single instance: a second launch exits quietly.
    unsafe {
        let _m = windows::Win32::System::Threading::CreateMutexW(
            None,
            true,
            w!("Local\\ZoneLockSingleton"),
        );
        if windows::Win32::Foundation::GetLastError()
            == windows::Win32::Foundation::ERROR_ALREADY_EXISTS
        {
            return;
        }
    }

    // No config file = first start after install: the load below writes
    // the defaults, so this must be checked before it.
    let first_run = !config::config_path().exists();
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            // Fail loud: corrupt config must be fixed or deleted by the user.
            let msg = format!("ZoneLock config is invalid:\n{e}\n\nFix or delete the file, then restart.");
            let wmsg: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
            unsafe {
                MessageBoxW(
                    None,
                    windows::core::PCWSTR(wmsg.as_ptr()),
                    w!("ZoneLock"),
                    MB_ICONERROR | MB_OK,
                );
            }
            std::process::exit(2);
        }
    };

    log::set_enabled(cfg.log_enabled);
    zonelock::place::set_force_small(cfg.force_small_zones);
    log::line(format!("zonelock {} starting", env!("CARGO_PKG_VERSION")));

    let shared = Shared::new(cfg);
    {
        // sync autostart flag with the registry reality
        let reg = autostart::is_enabled();
        let mut c = shared.config.write().unwrap();
        c.autostart = reg;
        // guarantee every monitor has a layout (fallbacks to halves)
        let mons: Vec<(String, usize)> = shared
            .monitors
            .read()
            .unwrap()
            .iter()
            .map(|m| (m.name.clone(), m.index))
            .collect();
        c.layouts.ensure(&mons);
        let _ = c.save();
    }

    // watcher thread owns the WinEvent hooks
    let watcher_shared = Arc::clone(&shared);
    let watcher_thread = std::thread::spawn(move || watcher::run(watcher_shared));

    let ui_shared = Arc::clone(&shared);
    let on_cmd = Box::new(move |cmd: UiCmd| handle_cmd(&ui_shared, cmd));

    let tray = match Tray::new(on_cmd) {
        Ok(t) => t,
        Err(e) => {
            log::line(format!("tray init failed: {e}"));
            std::process::exit(3);
        }
    };
    tray.refresh_tooltip(shared.config.read().unwrap().enabled);
    zonelock::tray::set_enabled_hint(shared.config.read().unwrap().enabled);
    TRAY_HWND.store(tray.hwnd.0 as isize, Ordering::Relaxed);
    let hk_ids = hotkeys::register_all(tray.hwnd, &shared.config.read().unwrap().hotkeys);
    *HK_IDS.lock().unwrap() = hk_ids;
    // daily update check (if allowed); results come back as UiCmd::UpdateChanged
    zonelock::update::spawn_auto(Arc::clone(&shared));
    // brand-new install: say hello with the core gestures
    if first_run {
        zonelock::welcome::show(&shared.config.read().unwrap().hotkeys);
    }
    // we are the new version, started by an update: say it went well
    if zonelock::update::just_updated() {
        log::line(format!("updated to {}", env!("CARGO_PKG_VERSION")));
        zonelock::planprompt::show_success(
            &format!("ZoneLock updated to {}", env!("CARGO_PKG_VERSION")),
            "Update complete. Your zones, plans and settings are all kept.",
        );
    }

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // shutdown
    hotkeys::unregister_all(tray.hwnd, &HK_IDS.lock().unwrap());
    shared.running.store(false, Ordering::Relaxed);
    // wake the watcher so it notices `running == false`
    watcher::stop(watcher::THREAD_ID.load(Ordering::Relaxed));
    let _ = watcher_thread.join();
    drop(tray);
    zonelock::memory::flush();
    log::line("zonelock exiting");
}

fn handle_cmd(shared: &Arc<Shared>, cmd: UiCmd) {
    match cmd {
        UiCmd::ToggleEnabled => {
            let mut cfg = shared.config.write().unwrap();
            cfg.enabled = !cfg.enabled;
            let _ = cfg.save();
            shared.bump();
            zonelock::tray::set_enabled_hint(cfg.enabled);
            log::line(format!("enabled={}", cfg.enabled));
            drop(cfg);
            zonelock::webui::refresh();
        }
        UiCmd::OpenEditor => overlay::toggle(Arc::clone(shared)),
        UiCmd::NextPreset => {
            next_preset(shared);
            zonelock::webui::refresh();
        }
        UiCmd::NextZone => step_zone(shared, 1),
        UiCmd::PrevZone => step_zone(shared, -1),
        UiCmd::FsToggle => watcher::post_fs_toggle(unsafe { GetForegroundWindow() }),
        UiCmd::Cheatsheet => {
            let hk = shared.config.read().unwrap().hotkeys.clone();
            zonelock::cheatsheet::toggle(&hk);
        }
        UiCmd::ToggleAutostart => {
            let mut cfg = shared.config.write().unwrap();
            let on = !autostart::is_enabled();
            match autostart::set_enabled(on) {
                Ok(()) => {
                    cfg.autostart = on;
                    let _ = cfg.save();
                }
                Err(e) => log::line(format!("autostart failed: {e}")),
            }
        }
        UiCmd::OpenConfigDir => {
            let _ = std::process::Command::new("explorer")
                .arg(config::config_dir())
                .spawn();
        }
        UiCmd::OpenSettings => zonelock::webui::toggle(Arc::clone(shared)),
        UiCmd::PlanGo(n) => {
            if let Err(e) = zonelock::plans::go(shared, n) {
                log::line(format!("plan {n}: {e}"));
            }
        }
        UiCmd::PlanSave(n) => save_plan_from_keys(shared, n),
        UiCmd::UpdateChanged => zonelock::webui::refresh(),
        UiCmd::SetupChanged(plan) => {
            if let Some(n) = plan {
                match zonelock::plans::arrange(shared, n) {
                    Ok(placed) if placed > 0 => {
                        let mons = shared.monitors.read().unwrap().clone();
                        let setup = zonelock::setups::label(&zonelock::setups::snapshot(&mons, 0));
                        zonelock::planprompt::show_success(
                            &format!("Plan {n} arranged"),
                            &format!("{setup}: {placed} window{} moved.", if placed == 1 { "" } else { "s" }),
                        );
                    }
                    Ok(_) => {}
                    Err(e) => log::line(format!("plan {n}: {e}")),
                }
            }
            zonelock::webui::refresh();
        }
        UiCmd::DesktopChanged(plan) => {
            if let Some(n) = plan {
                match zonelock::plans::arrange(shared, n) {
                    Ok(moved) if moved > 0 => {
                        let cur = zonelock::vdesk::current().unwrap_or_default();
                        let desk = zonelock::vdesk::list()
                            .into_iter()
                            .find(|d| d.id == cur)
                            .map(|d| d.name)
                            .unwrap_or_else(|| "This desktop".into());
                        zonelock::planprompt::show_success(
                            &format!("Plan {n} arranged"),
                            &format!("{desk}: {moved} window{} moved.", if moved == 1 { "" } else { "s" }),
                        );
                    }
                    Ok(_) => {}
                    Err(e) => log::line(format!("plan {n}: {e}")),
                }
            }
            zonelock::webui::refresh();
        }
        UiCmd::SuspendHotkeys => {
            let hwnd = HWND(TRAY_HWND.load(Ordering::Relaxed) as *mut _);
            let mut ids = HK_IDS.lock().unwrap();
            hotkeys::unregister_all(hwnd, &ids);
            ids.clear();
        }
        UiCmd::RebindHotkeys => {
            let hwnd = HWND(TRAY_HWND.load(Ordering::Relaxed) as *mut _);
            let mut ids = HK_IDS.lock().unwrap();
            hotkeys::unregister_all(hwnd, &ids);
            *ids = hotkeys::register_all(
                hwnd,
                &shared.config.read().unwrap().hotkeys,
            );
        }
        UiCmd::Exit => unsafe {
            // a stop without this line in the log = killed or crashed
            log::line("quit requested (tray menu, settings page, --quit or update)");
            if zonelock::webui::is_open() {
                // tear WebView2 down cleanly before the loop ends
                if let Ok(h) = FindWindowW(w!("ZoneLockWebSettings"), None) {
                    let _ = DestroyWindow(h);
                }
            }
            if overlay::is_open() {
                overlay::toggle(Arc::clone(shared));
            }
            PostQuitMessage(0);
        },
    }
}

/// Save shortcut: free slot -> saved now (small "Plan N saved" card);
/// taken slot -> the card asks first, holding the arrangement captured
/// on this first press. Same shortcut again overwrites, another number
/// saves there, Esc / timeout cancels (see planprompt.rs).
fn save_plan_from_keys(shared: &Arc<Shared>, n: u8) {
    use zonelock::plans::{self, SaveStep};
    use zonelock::planprompt;
    let cfg = shared.config.read().unwrap().clone();
    let taken = cfg.plans.get(&n.to_string()).cloned();
    let step = plans::save_step(planprompt::pending_slot(), n, taken.is_some());
    // what gets saved: the arrangement captured when the question was
    // asked, otherwise the windows open right now
    let plan = match planprompt::take_pending() {
        Some(p) => p,
        None => match plans::save_current(shared, n) {
            Ok(p) => p,
            Err(e) => {
                log::line(format!("plan {n} save failed: {e}"));
                return;
            }
        },
    };
    if plan.windows.is_empty() {
        planprompt::show_info("Nothing to save", "No app window is open.");
        return;
    }
    match (step, taken) {
        (SaveStep::AskOverwrite, Some(existing)) => {
            let label = zonelock::hotkeys::binding_label(&cfg.hotkeys.plan_save);
            planprompt::ask_overwrite(n, plan, &existing, &plans::free_slots(&cfg), &label);
        }
        _ => {
            let mut c = shared.config.write().unwrap();
            c.plans.insert(n.to_string(), plan.clone());
            let _ = c.save();
            drop(c);
            log::line(format!("plan {n} saved from keys"));
            planprompt::show_saved(n, &plan);
            zonelock::webui::refresh();
        }
    }
}

/// Cycle the preset on the monitor under the mouse cursor.
fn next_preset(shared: &Arc<Shared>) {
    let pt = unsafe {
        let mut p = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut p);
        p
    };
    let mons = shared.monitors.read().unwrap().clone();
    let Some(mon) = mons
        .iter()
        .find(|m| m.rect.contains_point(pt.x, pt.y))
        .cloned()
    else {
        return;
    };
    let mut cfg = shared.config.write().unwrap();
    let list = presets::all(&cfg.user_presets);
    let cur = cfg
        .layouts
        .for_monitor(&mon.name, mon.index)
        .map(|l| l.name.clone())
        .unwrap_or_default();
    let idx = list
        .iter()
        .position(|p| p.name == cur)
        .map(|i| (i + 1) % list.len())
        .unwrap_or(0);
    cfg.layouts.set_for_monitor(&mon.name, list[idx].clone());
    let _ = cfg.save();
    shared.bump();
    log::line(format!("preset '{}' on {}", list[idx].name, mon.name));
}

/// Move the foreground window into the next zone of its monitor.
/// Move the active window one zone forward (`dir` = 1) or back (-1).
fn step_zone(shared: &Arc<Shared>, dir: isize) {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == HWND::default() {
            return;
        }
        let info = wininfo::inspect(hwnd);
        let mons = shared.monitors.read().unwrap().clone();
        let cfg = shared.config.read().unwrap().clone();
        let Some(mon) = monitor::monitor_at(&info.rect, &mons) else {
            return;
        };
        let Some(layout) = cfg.layouts.for_monitor(&mon.name, mon.index) else {
            return;
        };
        if layout.zones.is_empty() {
            return;
        }
        let cur = layout
            .zone_for_rect(&mon.work, &info.rect)
            .unwrap_or(0);
        let n = layout.zones.len() as isize;
        let next = ((cur as isize + dir).rem_euclid(n)) as usize;
        if let Some(frac) = layout.zone_frac(next) {
            // zones live on the work area (never under the taskbar) and
            // apply_zone keeps the visible frame flush + on screen
            let target = frac.to_pixels(&mon.work, 0).inflate(-cfg.zone_gap);
            zonelock::unsnap::with(|b| b.before_snap(info.hwnd, &info.rect));
            if place::apply_zone(hwnd, &target) {
                let placed = wininfo::inspect(hwnd).rect;
                zonelock::unsnap::with(|b| b.after_snap(info.hwnd, &placed));
                log::line(format!("zone-step {dir:+} hwnd={:#x} -> {:?}", info.hwnd, target));
                // feed the engine so its tracker stays truthful
                let _ = watcher::evaluate(hwnd);
            }
        }
    }
}
