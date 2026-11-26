//! WinEvent-driven window watcher. Runs on a dedicated thread with its own
//! message loop (required for WINEVENT_OUTOFCONTEXT delivery).
//!
//! No injection: hooks are out-of-context, notifications arrive in our
//! process. We cannot veto the maximize before it happens, so we correct
//! within the same frame tick; a one-frame flicker on slow machines is the
//! documented trade-off versus DLL injection.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Accessibility::{
    SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow,
    MsgWaitForMultipleObjectsEx, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    KBDLLHOOKSTRUCT, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT,
    WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_KEYDOWN,
    WM_QUIT, WM_SYSKEYDOWN,
};

use crate::config::Config;
use crate::dragzones;
use crate::engine::{self, Action, Policy};
use crate::monitor::{self, Monitor};
use crate::resnap::{self, Seen, Snaps};
use crate::state::{LockKind, NormalState, Tracker, WinState};
use crate::wininfo::{self, WindowInfo};
use crate::zones::Layout;
use crate::{log, place};

// WinEvent ids we care about (constants are stable ABI values).
const EVENT_SYSTEM_MOVESIZESTART: u32 = 0x000A;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const EVENT_OBJECT_DESTROY: u32 = 0x8001;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_HIDE: u32 = 0x8003;
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
const OBJID_WINDOW: i32 = 0;
const CHILDID_SELF: i32 = 0;

const WM_STOP: u32 = 0x0400 + 0x7A; // WM_APP+0x7A internal stop
/// Fullscreen zone <-> whole display for the window in wparam.
const WM_FS_TOGGLE: u32 = 0x0400 + 0x7B;
const TICK_MS: u32 = 2000;

/// State shared between the UI thread (tray/hotkeys/overlay) and the
/// watcher thread.
pub struct Shared {
    pub config: RwLock<Config>,
    pub monitors: RwLock<Vec<Monitor>>,
    /// Bumped whenever config or monitors were swapped, so the watcher
    /// rebuilds cheap derived data.
    pub version: AtomicU32,
    /// Kill switch for the whole app.
    pub running: AtomicBool,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Shared> {
        Arc::new(Shared {
            config: RwLock::new(config),
            monitors: RwLock::new(monitor::enumerate()),
            version: AtomicU32::new(0),
            running: AtomicBool::new(true),
        })
    }

    pub fn bump(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }
}

struct Ctx {
    shared: Arc<Shared>,
    tracker: Tracker,
    /// Which zone each window sits in, to survive display changes.
    snaps: Snaps,
    /// After a display change, keep re-checking snapped windows until
    /// then: Windows and the apps settle in several steps.
    resnap_until: Option<Instant>,
    /// Window being dragged: (hwnd, rect at drag start, size restored).
    drag: Option<(isize, crate::rect::Rect, bool)>,
    /// Windows that just left a held fullscreen: (rect to keep, since,
    /// corrections made). Some apps (Firefox) re-apply their own saved
    /// size a moment after leaving fullscreen, which would throw them out
    /// of their zone (even off screen).
    settle: std::collections::HashMap<isize, (crate::rect::Rect, Instant, u8)>,
    /// Fullscreen windows the user sent to the whole display with the
    /// fs_toggle keys: their zone lock, to send them back on a 2nd press.
    released_fs: std::collections::HashMap<isize, crate::state::LockedState>,
    /// Display setup connected now (`setups::key_of`).
    setup_key: String,
    /// The setup changed: (setup before the change, when to act). The
    /// linked plan is arranged once the screens have settled; changing
    /// back meanwhile cancels it.
    setup_due: Option<(String, Instant)>,
    /// Virtual desktop shown now (`vdesk::current`, "" = only one).
    desk: String,
    /// Switched to another desktop: when to arrange its plan.
    desk_due: Option<Instant>,
}

/// How long a newly shown virtual desktop must stay before its plan is
/// arranged (the switch animation, and quick Win+Ctrl+arrow runs through
/// several desktops arrange only the last one).
const DESK_SETTLE: Duration = Duration::from_millis(400);

/// How long the screens must stay the same before a setup's plan is
/// arranged: Windows moves windows and apps re-layout in several steps
/// (longer than RESNAP_WINDOW, so the re-snap pass is over).
const SETUP_SETTLE: Duration = Duration::from_millis(3000);

/// How long after leaving fullscreen a self-inflicted move is undone.
const SETTLE_WINDOW: Duration = Duration::from_millis(1500);

/// How long after a display change snapped windows are re-checked.
const RESNAP_WINDOW: Duration = Duration::from_millis(2500);

unsafe extern "system" fn hook_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW || id_child != CHILDID_SELF {
        return;
    }
    match event {
        EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE => {
            with_ctx(|ctx| {
                ctx.tracker.remove(hwnd.0 as isize);
                ctx.snaps.observe(hwnd.0 as isize, Seen::Gone, Instant::now());
                if event == EVENT_OBJECT_DESTROY {
                    ctx.released_fs.remove(&(hwnd.0 as isize));
                }
            });
            if event == EVENT_OBJECT_DESTROY {
                crate::unsnap::with(|b| b.forget(hwnd.0 as isize));
                crate::memory::flush();
            }
        }
        EVENT_SYSTEM_MOVESIZESTART => {
            let start = wininfo::inspect(hwnd).rect;
            // a resize from an edge/corner must never "unsnap": mark the
            // drag as already handled
            let resize = grabbed_for_resize(hwnd, &start);
            with_ctx(|ctx| ctx.drag = Some((hwnd.0 as isize, start, resize)));
            on_drag_start(hwnd);
        }
        EVENT_SYSTEM_MOVESIZEEND => {
            with_ctx(|ctx| ctx.drag = None);
            on_drag_end(hwnd);
            evaluate_hwnd(hwnd);
        }
        EVENT_OBJECT_LOCATIONCHANGE | EVENT_SYSTEM_MINIMIZEEND
        | EVENT_OBJECT_SHOW => {
            if event == EVENT_OBJECT_LOCATIONCHANGE {
                unsnap_on_drag(hwnd);
            }
            if event == EVENT_OBJECT_SHOW {
                let known = with_ctx(|c| c.tracker.get(hwnd.0 as isize).is_some()).unwrap_or(true);
                if !known {
                    // learn this app's minimum size (minsizes.json)
                    crate::minsize::probe(hwnd);
                }
                if !crate::plans::claim_new_window(hwnd) && !known {
                    restore_remembered(hwnd);
                }
            }
            // Mid-drag rects are meaningless; wait for the drop.
            if dragzones::dragged() != hwnd.0 as isize {
                evaluate_hwnd(hwnd);
            }
        }
        _ => {}
    }
}

/// A window shows for the first time: put it back where its app was
/// left (memory.rs decides whether it applies).
fn restore_remembered(hwnd: HWND) {
    let Some((cfg, mons)) = with_ctx(|ctx| {
        (
            ctx.shared.config.read().unwrap().clone(),
            ctx.shared.monitors.read().unwrap().clone(),
        )
    }) else {
        return;
    };
    if !cfg.enabled || !cfg.remember_positions {
        return;
    }
    let info = wininfo::inspect(hwnd);
    crate::memory::restore_new_window(hwnd, &info, &cfg, &mons);
}

/// Is this move/size loop a RESIZE (border or corner grabbed)? Asks the
/// window what is under the cursor (WM_NCHITTEST, short timeout: the
/// window's thread is entering its modal loop and still answers sent
/// messages). Falls back to geometry: cursor on or just outside the
/// visible frame edge = resize.
fn grabbed_for_resize(hwnd: HWND, rect: &crate::rect::Rect) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NCHITTEST,
    };
    let mut p = windows::Win32::Foundation::POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return false;
    }
    let lp = LPARAM((((p.y as i16 as u16 as u32) << 16) | (p.x as i16 as u16 as u32)) as isize);
    let mut res = 0usize;
    let ok = unsafe {
        SendMessageTimeoutW(hwnd, WM_NCHITTEST, WPARAM(0), lp, SMTO_ABORTIFHUNG, 60, Some(&mut res))
    };
    if ok.0 != 0 {
        // HTLEFT..HTBOTTOMRIGHT = 10..=17 (HTTOP 12), HTBORDER 18
        return crate::unsnap::is_resize_hit(res as i32);
    }
    let vis = crate::plans::visible_rect(hwnd, *rect);
    crate::unsnap::near_edge(&vis, p.x, p.y, 6)
}

/// First real move of a drag on a window we snapped: give it back its
/// pre-snap size under the cursor (once per drag).
fn unsnap_on_drag(hwnd: HWND) {
    let h = hwnd.0 as isize;
    let Some(start) = with_ctx(|ctx| match ctx.drag {
        Some((d, start, false)) if d == h => Some(start),
        _ => None,
    })
    .flatten() else {
        return;
    };
    let enabled = with_ctx(|ctx| ctx.shared.config.read().unwrap().enabled).unwrap_or(false);
    if !enabled {
        return;
    }
    let now = wininfo::inspect(hwnd).rect;
    let mut cur = windows::Win32::Foundation::POINT::default();
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cur) }.is_err() {
        return;
    }
    let cursor = crate::rect::Rect::new(cur.x, cur.y, cur.x + 1, cur.y + 1);
    let work = with_ctx(|ctx| {
        let mons = ctx.shared.monitors.read().unwrap();
        monitor::monitor_at(&cursor, &mons).map(|m| m.work)
    })
    .flatten();
    let Some(to) = crate::unsnap::with(|b| {
        b.on_drag_motion(h, &start, &now, cur.x, cur.y, work.as_ref())
    }) else {
        return;
    };
    with_ctx(|ctx| ctx.drag = Some((h, start, true)));
    crate::unsnap::with(|b| b.forget(h));
    // The system move loop keeps the size it captured at drag start and
    // re-applies it on the next mouse move, so a plain resize does not
    // stick. End that loop, resize under the cursor, then start a new
    // move loop: the button is still down, the drag simply goes on.
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{
        PostMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_CANCELMODE, WM_SYSCOMMAND,
    };
    unsafe {
        let _ = SendMessageTimeoutW(
            hwnd,
            WM_CANCELMODE,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            200,
            None,
        );
    }
    let ok = place::apply(hwnd, &to);
    let held = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
    if ok && held {
        const SC_DRAGMOVE: usize = 0xF012; // SC_MOVE | HTCAPTION
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(SC_DRAGMOVE), LPARAM(0));
        }
    }
    log::line(format!("unsnap hwnd={h:#x} -> {to:?} ok={ok} drag-continues={held}"));
}

/// The visible frame of `hwnd` fills zone `zone` of `mon` (what a snap
/// or a zone hotkey leaves).
fn in_zone(hwnd: HWND, info: &WindowInfo, mon: &Monitor, layout: Option<&Layout>, zone: usize, gap: i32) -> bool {
    let Some(frac) = layout.and_then(|l| l.zone_frac(zone)) else {
        return false;
    };
    let z = frac.to_pixels(&mon.work, 0).inflate(-gap);
    // cheap pre-check on the window rect (invisible borders ~7-8 px)
    // before asking DWM for the visible frame
    if info.rect.overlap_area(&z) * 2 < z.area() {
        return false;
    }
    resnap::fills_zone(&crate::plans::visible_rect(hwnd, info.rect), &z)
}

/// Displays changed (resolution, scale, taskbar, plug/unplug): move the
/// geometry the tracker holds onto the new displays and put snapped
/// windows back in their zones.
fn display_changed(ctx: &mut Ctx, old: &[Monitor], new: &[Monitor]) {
    let cfg = ctx.shared.config.read().unwrap().clone();
    for h in ctx.tracker.hwnds() {
        let Some(WinState::Locked(mut l)) = ctx.tracker.get(h).cloned() else { continue };
        let (Some(om), Some(nm)) = (
            old.iter().find(|m| m.name == l.normal.monitor),
            new.iter().find(|m| m.name == l.normal.monitor),
        ) else {
            continue;
        };
        // the rect to restore on release, same relative spot
        l.normal.rect = resnap::rescale(&l.normal.rect, &om.work, &nm.work);
        let Some(frac) = cfg
            .layouts
            .for_monitor(&nm.name, nm.index)
            .and_then(|lay| lay.zone_frac(l.normal.zone).or_else(|| lay.zone_frac(0)))
        else {
            ctx.tracker.set(h, WinState::Locked(l));
            continue;
        };
        let target = frac.to_pixels(&nm.work, 0);
        let moved = target != l.target;
        l.target = target;
        let kind = l.kind;
        ctx.tracker.set(h, WinState::Locked(l));
        if !moved {
            continue;
        }
        let hwnd = HWND(h as *mut _);
        let t = target.inflate(-cfg.zone_gap);
        let ok = match kind {
            LockKind::ZoneFsMoved => place::hold_fullscreen(hwnd, &t),
            LockKind::ZoneFsBorderless => place::move_exact(hwnd, &t),
            // Maximize: re-placed by the snap pass; exit-sent: in flight,
            // it parks on the new target when the app drops
            _ => continue,
        };
        log::line(format!("display-change refit hwnd={h:#x} {kind:?} -> {t:?} ok={ok}"));
    }
    ctx.resnap_until = Some(Instant::now() + RESNAP_WINDOW);
    resnap_pass(ctx, new);
}

/// Follow the display setup: remember a new one, and once the screens
/// have settled on a setup linked to a plan, ask the UI thread to
/// arrange that plan (`plans::arrange`).
fn setup_tick(ctx: &mut Ctx, mons: &[Monitor]) {
    let key = crate::setups::key_of(mons);
    let now = Instant::now();
    if key != ctx.setup_key {
        log::line(format!("display setup: {} -> {}", ctx.setup_key, key));
        crate::setups::record_current(mons);
        let from = match ctx.setup_due.take() {
            Some((from, _)) => from,
            None => std::mem::take(&mut ctx.setup_key),
        };
        ctx.setup_key = key;
        ctx.setup_due = Some((from, now + SETUP_SETTLE));
        crate::tray::post_setup(None);
        return;
    }
    let Some((from, at)) = ctx.setup_due.clone() else { return };
    if now < at {
        return;
    }
    ctx.setup_due = None;
    let cfg = ctx.shared.config.read().unwrap().clone();
    if !cfg.enabled {
        return;
    }
    if let Some(slot) = crate::setups::plan_to_apply(&cfg, &from, &ctx.setup_key) {
        // the plan decides where windows go now, not the re-snap pass
        ctx.resnap_until = None;
        log::line(format!("display setup {}: arranging plan {slot}", ctx.setup_key));
        crate::tray::post_setup(Some(slot));
    }
}

/// Follow the virtual desktop shown: once a switch has settled on a
/// desktop linked to a plan, ask the UI thread to arrange that plan
/// (`plans::arrange`, which only sees the windows of that desktop).
/// Runs on every watcher wake-up: a switch fires a burst of WinEvents.
fn desktop_tick(ctx: &mut Ctx) {
    let cur = crate::vdesk::current().unwrap_or_default();
    let now = Instant::now();
    if cur != ctx.desk {
        log::line(format!("virtual desktop: {} -> {}", ctx.desk, cur));
        // from "" = a second desktop was just created, or at start: the
        // windows are where the user put them
        let switched = !ctx.desk.is_empty() && !cur.is_empty();
        ctx.desk = cur;
        ctx.desk_due = switched.then_some(now + DESK_SETTLE);
        crate::tray::post_desktop(None);
        return;
    }
    let Some(at) = ctx.desk_due else { return };
    if now < at {
        return;
    }
    ctx.desk_due = None;
    let cfg = ctx.shared.config.read().unwrap().clone();
    if !cfg.enabled {
        return;
    }
    if let Some(&slot) = cfg.desktop_plans.get(&ctx.desk) {
        if cfg.plans.contains_key(&slot.to_string()) {
            log::line(format!("virtual desktop {}: arranging plan {slot}", ctx.desk));
            crate::tray::post_desktop(Some(slot));
        }
    }
}

/// Put every window that sat in a zone before the display change back in
/// that zone, as computed on the new geometry. Idempotent: windows
/// already there are left alone, so it can run on every tick.
fn resnap_pass(ctx: &mut Ctx, mons: &[Monitor]) {
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow};
    let cfg = ctx.shared.config.read().unwrap().clone();
    if !cfg.enabled {
        return;
    }
    let present: Vec<String> = mons.iter().map(|m| m.name.clone()).collect();
    for (h, snap) in ctx.snaps.on_display_change(&present, Instant::now()) {
        let hwnd = HWND(h as *mut _);
        if unsafe { !IsWindow(Some(hwnd)).as_bool() || IsIconic(hwnd).as_bool() } {
            continue;
        }
        let Some(mon) = mons.iter().find(|m| m.name == snap.monitor) else { continue };
        let Some(frac) = cfg
            .layouts
            .for_monitor(&mon.name, mon.index)
            .and_then(|lay| lay.zone_frac(snap.zone))
        else {
            continue;
        };
        let target = frac.to_pixels(&mon.work, 0);
        let t = target.inflate(-cfg.zone_gap);
        let info = wininfo::inspect(hwnd);
        // a fullscreen-held window is refit by display_changed, not here
        if ctx.tracker.get(h).is_some_and(|s| s.is_zone_fs()) {
            continue;
        }
        let there = !info.is_zoomed()
            && resnap::fills_zone(&crate::plans::visible_rect(hwnd, info.rect), &t);
        if !there {
            let ok = place::apply_zone(hwnd, &t);
            log::line(format!(
                "display-change resnap hwnd={h:#x} {} zone {} of {} -> {t:?} ok={ok}",
                info.exe, snap.zone, mon.name
            ));
            if !ok {
                continue;
            }
        }
        let info = wininfo::inspect(hwnd);
        // still "snapped" for unsnap-on-drag, at its new spot
        crate::unsnap::with(|b| b.after_snap(h, &info.rect));
        let prev_rect = ctx.tracker.get(h).and_then(|s| s.normal()).map(|n| n.rect);
        let normal = NormalState {
            rect: if snap.maximize { prev_rect.unwrap_or(info.rect) } else { info.rect },
            monitor: mon.name.clone(),
            zone: snap.zone,
        };
        let state = if snap.maximize {
            engine::next_state(
                &Action::ContainMaximize(target),
                &info,
                Some(&WinState::Normal(normal)),
                &mon.name,
                None,
            )
        } else {
            Some(WinState::Normal(normal))
        };
        if let Some(s) = state {
            ctx.tracker.set(h, s);
        }
        ctx.snaps.settled(h);
    }
}

// The Ctx lives on the watcher thread; the hook proc runs on that same
// thread, so a thread-local is the clean way to reach it.
thread_local! {
    static CTX: std::cell::RefCell<Option<Ctx>> = const { std::cell::RefCell::new(None) };
}

fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> Option<R> {
    CTX.with(|c| c.borrow_mut().as_mut().map(f))
}

/// Evaluate one window against the engine and apply the result.
/// Returns the action taken (used by integration tests).
pub fn evaluate(hwnd: HWND) -> Action {
    let info = wininfo::inspect(hwnd);
    evaluate_info(&info)
}

pub fn evaluate_info(info: &WindowInfo) -> Action {
    let mut taken = Action::Nothing;
    with_ctx(|ctx| {
        taken = evaluate_in_ctx(ctx, info);
    });
    taken
}

fn evaluate_in_ctx(ctx: &mut Ctx, info: &WindowInfo) -> Action {
    let cfg = ctx.shared.config.read().unwrap().clone();
    let mons = ctx.shared.monitors.read().unwrap().clone();

    if !cfg.enabled {
        return Action::Nothing;
    }
    if cfg.is_excluded_process(&info.exe)
        || info.excludable(&cfg.exclude_class_prefixes)
    {
        ctx.tracker.remove(info.hwnd);
        return Action::Forget;
    }

    let mon = monitor::monitor_at(&info.rect, &mons).cloned();
    let layout: Option<Layout> = mon.as_ref().and_then(|m| {
        cfg.layouts.for_monitor(&m.name, m.index).cloned()
    });

    let policy = Policy {
        enabled: cfg.enabled,
        zone_gap: cfg.zone_gap,
        fullscreen_tolerance: cfg.fullscreen_tolerance,
        max_refights: cfg.max_refights,
        refight_window: Duration::from_millis(cfg.refight_window_ms),
        fs_exit_key: cfg.fs_exit_vk(&info.exe),
        fs_stable: cfg.is_fs_stable(&info.exe),
        exit_retry: Duration::from_millis(cfg.exit_retry_ms),
        max_exit_sends: cfg.max_exit_sends,
    };

    let prev = ctx.tracker.get(info.hwnd).cloned();
    let desktop = monitor::union_rect(&mons);
    let action = engine::decide(
        info,
        prev.as_ref(),
        mon.as_ref(),
        layout.as_ref(),
        desktop.as_ref(),
        &policy,
    );
    // Track over an already-Normal window is a no-op state-wise and
    // fires on every LOCATIONCHANGE - skip it or the log floods during
    // any drag.
    let quiet = matches!(action, Action::Nothing)
        || (matches!(action, Action::Forget) && prev.is_none())
        || (matches!(action, Action::Track(_))
            && matches!(prev, Some(WinState::Normal(_))));
    // logged when this function returns, i.e. AFTER the action is applied:
    // file IO before SetWindowPos lengthened every visible correction
    struct Deferred(Option<String>);
    impl Drop for Deferred {
        fn drop(&mut self) {
            if let Some(s) = self.0.take() {
                log::line(s);
            }
        }
    }
    let mut _eval_log = Deferred(None);
    if !quiet {
        _eval_log.0 = Some(format!(
            "eval hwnd={:#x} {} rect={:?} zoomed={} prev={:?} -> {action:?}",
            info.hwnd,
            info.exe,
            info.rect,
            info.zoomed,
            prev.as_ref().map(|s| match s {
                WinState::Normal(_) => "normal",
                WinState::Locked(l) => match l.kind {
                    LockKind::Maximize => "lock:max",
                    LockKind::ZoneFsMoved => "lock:moved",
                    LockKind::ZoneFsExitSent => "lock:exitsent",
                    LockKind::ZoneFsBorderless => "lock:borderless",
                },
                WinState::Ignored => "ignored",
            })
        ));
    }

    let hwnd = HWND(info.hwnd as *mut _);
    let mon_name = mon.as_ref().map(|m| m.name.as_str()).unwrap_or("");
    let mut strip_result = None;

    match &action {
        Action::ContainFsMove(t) => {
            let t = t.inflate(-cfg.zone_gap);
            let ok = place::hold_fullscreen(hwnd, &t);
            log::line(format!(
                "hold-fullscreen hwnd={:#x} {} -> {:?} ok={ok}",
                info.hwnd, info.exe, t
            ));
            if !ok {
                return action;
            }
        }
        Action::KeepOnScreen(r) => {
            let ok = place::move_exact(hwnd, r);
            log::line(format!(
                "keep-on-screen hwnd={:#x} {} -> {:?} ok={ok}",
                info.hwnd, info.exe, r
            ));
            if !ok {
                return action;
            }
        }
        Action::RestoreNormal(r) => {
            let ok = place::apply(hwnd, r);
            log::line(format!(
                "restore-normal hwnd={:#x} {} -> {:?} ok={ok}",
                info.hwnd, info.exe, r
            ));
            if !ok {
                return action;
            }
        }
        Action::ContainMaximize(t) => {
            let t = t.inflate(-cfg.zone_gap);
            // dragging it later gives back the size it had before the
            // maximize, like a real maximized window
            if let Some(n) = prev.as_ref().and_then(|s| s.normal()) {
                crate::unsnap::with(|b| b.before_snap(info.hwnd, &n.rect));
            }
            let ok = place::apply_zone(hwnd, &t);
            if ok {
                let placed = wininfo::inspect(hwnd).rect;
                crate::unsnap::with(|b| b.after_snap(info.hwnd, &placed));
            }
            log::line(format!(
                "contain hwnd={:#x} {} -> {:?} ok={ok}",
                info.hwnd, info.exe, t
            ));
            if !ok {
                return action;
            }
        }
        Action::SendFsExitKey { vk, .. } => {
            place::post_key(hwnd, *vk);
            log::line(format!("fs-exit-key {vk:#x} -> hwnd={:#x} {}", info.hwnd, info.exe));
        }
        Action::LockFsBorderless(t) => {
            let t = t.inflate(-cfg.zone_gap);
            strip_result = place::lock_borderless(hwnd, &t);
            log::line(format!(
                "lock-borderless hwnd={:#x} {} -> {:?} ok={}",
                info.hwnd, info.exe, t, strip_result.is_some()
            ));
            if strip_result.is_none() {
                return action;
            }
        }
        Action::ReleaseFs {
            place_at,
            orig_style,
            orig_exstyle,
        } => {
            let ok = place::release_borderless(hwnd, place_at, *orig_style, *orig_exstyle);
            log::line(format!(
                "release hwnd={:#x} {} -> {:?} ok={ok}",
                info.hwnd, info.exe, place_at
            ));
            if !ok {
                return action;
            }
        }
        _ => {}
    }

    let ns = engine::next_state(&action, info, prev.as_ref(), mon_name, strip_result);
    // a plain or maximize-contained window: remember where it sits, so
    // the app reopens there (fullscreen / ignored states are not spots)
    let remember = matches!(
        &ns,
        Some(WinState::Normal(_))
    ) || matches!(&ns, Some(WinState::Locked(l)) if l.kind == LockKind::Maximize);
    // Just left a held fullscreen: keep it where it was for a moment, in
    // case the app re-applies a size of its own right after (Firefox does,
    // off its zone and even off screen).
    let now = Instant::now();
    if let Some(WinState::Locked(l)) = prev.as_ref() {
        if l.kind == LockKind::ZoneFsMoved && matches!(ns, Some(WinState::Normal(_))) {
            ctx.settle.insert(info.hwnd, (l.normal.rect, now, 0));
        }
    }
    if matches!(action, Action::Track(_)) {
        if let Some((keep, since, n)) = ctx.settle.get(&info.hwnd).copied() {
            let dragging = ctx.drag.is_some_and(|(d, _, _)| d == info.hwnd);
            if now.duration_since(since) > SETTLE_WINDOW || dragging || n >= 3 {
                ctx.settle.remove(&info.hwnd);
            } else if !info.iconic && !info.is_zoomed() && !info.rect.approx_eq(&keep, 4) {
                let ok = place::apply(hwnd, &keep);
                log::line(format!(
                    "fs-exit settle hwnd={:#x} {} {:?} -> {keep:?} ok={ok}",
                    info.hwnd, info.exe, info.rect
                ));
                ctx.settle.insert(info.hwnd, (keep, since, n + 1));
            }
        }
    }
    let seen = match (&ns, mon.as_ref()) {
        (None, _) => Seen::Gone,
        (Some(WinState::Locked(l)), Some(m)) if l.kind == LockKind::Maximize => {
            Seen::Contained { monitor: m.name.clone(), zone: l.normal.zone }
        }
        (Some(WinState::Normal(n)), Some(m)) => {
            if in_zone(hwnd, info, m, layout.as_ref(), n.zone, cfg.zone_gap) {
                Seen::InZone { monitor: m.name.clone(), zone: n.zone }
            } else {
                Seen::Elsewhere { monitor: m.name.clone() }
            }
        }
        _ => Seen::Other,
    };
    ctx.snaps.observe(info.hwnd, seen, Instant::now());
    match ns {
        Some(s) => ctx.tracker.set(info.hwnd, s),
        None => ctx.tracker.remove(info.hwnd),
    }
    if remember && cfg.remember_positions {
        crate::memory::remember_window(hwnd, info, &cfg, &mons);
    }
    action
}

const VK_F11_CODE: u32 = 0x7A;
const VK_ESCAPE_CODE: u32 = 0x1B;

/// Low-level keyboard hook: F11/Esc pressed on a zone-fullscreen-locked
/// foreground window releases the lock (and F11 is swallowed so the app
/// does not re-enter its own fullscreen in the same tick).
unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if kb.vkCode == VK_F11_CODE || kb.vkCode == VK_ESCAPE_CODE {
                if handle_zone_fs_key(kb.vkCode) {
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn handle_zone_fs_key(vk: u32) -> bool {
    let mut swallow = false;
    with_ctx(|ctx| {
        // the key may land on an owned popup (Chrome bubbles, menus) that
        // forwards it to its browser window: look at the root owner too
        let top = unsafe { GetForegroundWindow() };
        let owner = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                top,
                windows::Win32::UI::WindowsAndMessaging::GA_ROOTOWNER,
            )
        };
        let Some(fg) = [top, owner]
            .into_iter()
            .find(|w| ctx.tracker.get(w.0 as isize).is_some_and(|s| s.is_zone_fs()))
        else {
            return;
        };
        let h = fg.0 as isize;
        let Some(state) = ctx.tracker.get(h).cloned() else { return };
        let Some(l) = state.locked() else { return };
        if !state.is_zone_fs() {
            return;
        }
        match (vk, l.kind) {
            (VK_F11_CODE, LockKind::ZoneFsMoved) => {
                if l.exit_vk != 0 {
                    // we post the exit key deterministically and swallow
                    // the user's F11 so it is not toggled twice
                    place::post_key(fg, l.exit_vk);
                    log::line(format!("key-release (exit-key) hwnd={h:#x}"));
                    swallow = true;
                    ctx.tracker
                        .set(h, WinState::Normal(l.normal.clone()));
                }
                // exit_vk == 0: let F11 through, the app toggles itself
                // out and the next event re-tracks it
            }
            (VK_F11_CODE, LockKind::ZoneFsBorderless) => {
                place::release_borderless(fg, &l.normal.rect, l.orig_style, l.orig_exstyle);
                log::line(format!("key-release (F11) hwnd={h:#x} -> {:?}", l.normal.rect));
                ctx.tracker.set(h, WinState::Normal(l.normal.clone()));
                swallow = true;
            }
            (VK_F11_CODE, LockKind::ZoneFsExitSent) => {
                // transition in flight: just keep the app from toggling
                swallow = true;
            }
            (VK_ESCAPE_CODE, LockKind::ZoneFsBorderless) => {
                place::release_borderless(fg, &l.normal.rect, l.orig_style, l.orig_exstyle);
                log::line(format!("key-release (Esc) hwnd={h:#x} -> {:?}", l.normal.rect));
                ctx.tracker.set(h, WinState::Normal(l.normal.clone()));
                // pass through: harmless to a normal-mode app
            }
            _ => {}
        }
    });
    swallow
}

/// OS thread id of the watcher, set when `run` starts.
pub static THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Blocking watcher loop. Owns the thread.
pub fn run(shared: Arc<Shared>) {
    unsafe {
        THREAD_ID.store(
            windows::Win32::System::Threading::GetCurrentThreadId(),
            Ordering::SeqCst,
        );
    }
    let start_mons = shared.monitors.read().unwrap().clone();
    crate::setups::record_current(&start_mons);
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            shared: shared.clone(),
            tracker: Tracker::default(),
            snaps: Snaps::default(),
            resnap_until: None,
            drag: None,
            settle: Default::default(),
            released_fs: Default::default(),
            setup_key: crate::setups::key_of(&start_mons),
            setup_due: None,
            desk: crate::vdesk::current().unwrap_or_default(),
            desk_due: None,
        });
    });

    unsafe {
        // Two hooks: system range (movesize/minimize) and object range
        // (show/hide/destroy/locationchange).
        let h1 = SetWinEventHook(
            EVENT_SYSTEM_MOVESIZESTART,
            EVENT_SYSTEM_MINIMIZEEND,
            None,
            Some(hook_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let h2 = SetWinEventHook(
            EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(hook_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0)
            .unwrap_or_default();
        log::line(format!(
            "watcher hooks installed: system={:?} object={:?} kb={:?}",
            h1.0, h2.0, kb.0
        ));

        // Seed tracker with the windows already open.
        for_each_top_level(|hwnd| {
            evaluate_hwnd(hwnd);
        });
        // Learn the minimum size of every app already running, tray apps
        // with a hidden main window included (minsizes.json).
        crate::minsize::probe_all();

        let tid = windows::Win32::System::Threading::GetCurrentThreadId();
        let mut msg = MSG::default();
        loop {
            if !shared.running.load(Ordering::Relaxed) {
                break;
            }
            // Wait for messages; time out every TICK_MS to re-poll config,
            // monitor layout and prune dead hwnds. During a drag we tick
            // fast to track the cursor for the zone highlight.
            let desk_wait = with_ctx(|c| c.desk_due.is_some()).unwrap_or(false);
            let wait_ms = if dragzones::dragging() {
                30
            } else if desk_wait {
                50
            } else {
                TICK_MS
            };
            let wait = MsgWaitForMultipleObjectsEx(
                None,
                wait_ms,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT || msg.message == WM_STOP {
                    shared.running.store(false, Ordering::Relaxed);
                    break;
                }
                if msg.message == WM_FS_TOGGLE && msg.hwnd.0.is_null() {
                    fs_toggle(HWND(msg.wParam.0 as *mut _));
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if !shared.running.load(Ordering::Relaxed) {
                break;
            }
            if wait.0 == 0xFFFFFFFF {
                continue; // WAIT_FAILED
            }
            periodic_tick(&shared, tid);
        }
        dragzones::end();
        let _ = UnhookWinEvent(h1);
        let _ = UnhookWinEvent(h2);
        if !kb.0.is_null() {
            let _ = UnhookWindowsHookEx(kb);
        }
    }
    log::line("watcher stopped");
}

/// Drag-snap lifecycle: every eligible MOVESIZESTART is tracked; the
/// overlay appears whenever the configured modifier is held (before or
/// during the drag) and MOVESIZEEND snaps the window into the zone under
/// the cursor.
fn on_drag_start(hwnd: HWND) {
    with_ctx(|ctx| {
        let cfg = ctx.shared.config.read().unwrap().clone();
        if !cfg.enabled || !cfg.shift_snap {
            return;
        }
        let info = wininfo::inspect(hwnd);
        if !info.visible
            || cfg.is_excluded_process(&info.exe)
            || info.excludable(&cfg.exclude_class_prefixes)
        {
            return;
        }
        // empty = no key needed; an invalid value falls back to Shift
        let trigger = match cfg.hotkeys.drag_snap.trim() {
            "" => None,
            s => crate::hotkeys::parse_mods(s)
                .or_else(|| crate::hotkeys::parse_mods("Shift")),
        };
        let mons = ctx.shared.monitors.read().unwrap().clone();
        dragzones::begin(hwnd.0 as isize, &mons, &cfg.layouts, trigger);
    });
}

fn on_drag_end(hwnd: HWND) {
    if !dragzones::dragging() {
        return;
    }
    let target = dragzones::target_at_cursor();
    dragzones::end();
    if let Some(t) = target {
        let gap = with_ctx(|ctx| ctx.shared.config.read().unwrap().zone_gap)
            .unwrap_or(0);
        let t = t.inflate(-gap);
        let h = hwnd.0 as isize;
        let cur = wininfo::inspect(hwnd).rect;
        crate::unsnap::with(|b| b.before_snap(h, &cur));
        let ok = place::apply_zone(hwnd, &t);
        let placed = wininfo::inspect(hwnd).rect;
        crate::unsnap::with(|b| b.after_snap(h, &placed));
        log::line(format!(
            "drag-snap hwnd={:#x} -> {:?} ok={ok}",
            hwnd.0 as isize, t
        ));
    }
}

fn periodic_tick(shared: &Arc<Shared>, _tid: u32) {
    // Show/hide the drag overlay with the live trigger state and keep the
    // highlight in sync (the modifier may go down mid-drag).
    if dragzones::dragging() {
        dragzones::poll();
    }
    // Refresh monitors (hot-plug, resolution, scale, taskbar moves).
    let mons = monitor::enumerate();
    let changed_from = {
        let mut guard = shared.monitors.write().unwrap();
        if guard.len() != mons.len()
            || guard.iter().zip(mons.iter()).any(|(a, b)| {
                a.rect != b.rect || a.work != b.work || a.name != b.name || a.dpi_x != b.dpi_x
            })
        {
            log::line(format!(
                "monitor topology changed: {}",
                mons.iter()
                    .map(|m| format!("{} {}x{}", m.name, m.rect.width(), m.rect.height()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            let old = std::mem::replace(&mut *guard, mons.clone());
            shared.bump();
            Some(old)
        } else {
            None
        }
    };
    if let Some(old) = changed_from {
        with_ctx(|ctx| display_changed(ctx, &old, &mons));
    }
    with_ctx(|ctx| setup_tick(ctx, &mons));
    with_ctx(desktop_tick);
    with_ctx(|ctx| {
        if ctx.resnap_until.is_some_and(|t| Instant::now() < t) {
            resnap_pass(ctx, &mons);
        } else {
            ctx.resnap_until = None;
        }
    });
    // Drop states of dead windows.
    with_ctx(|ctx| {
        let alive = |h: isize| unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(h as *mut _))).as_bool()
        };
        ctx.tracker.retain(alive);
        ctx.snaps.prune(Instant::now(), alive);
    });
    // Re-evaluate windows waiting on the fullscreen-exit transition:
    // their retry clock only advances on evaluation, and a posted key
    // can be swallowed while the app is still entering fullscreen.
    let pending = with_ctx(|ctx| ctx.tracker.pending_exit()).unwrap_or_default();
    for h in pending {
        evaluate_hwnd(HWND(h as *mut _));
    }
}

/// Ask the watcher thread to stop (called from the UI thread on exit).
/// From the UI thread (hotkey): toggle the fullscreen of the active
/// window between its zone and the whole display.
pub fn post_fs_toggle(hwnd: HWND) {
    let tid = THREAD_ID.load(Ordering::Relaxed);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_FS_TOGGLE, WPARAM(hwnd.0 as usize), LPARAM(0));
        }
    }
}

/// A fullscreen window held in its zone goes to the whole display (state
/// Ignored, so nothing pulls it back; its lock is kept aside), and the
/// same keys put it back in its zone while it is still fullscreen.
/// Frameless "stable" windows are stretched / shrunk the same way.
fn fs_toggle(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_ROOTOWNER};
    with_ctx(|ctx| {
        // the key may land on an owned popup: use the managed root
        let owner = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
        let h = [hwnd, owner]
            .into_iter()
            .map(|w| w.0 as isize)
            .find(|h| ctx.tracker.get(*h).is_some() || ctx.released_fs.contains_key(h))
            .unwrap_or(hwnd.0 as isize);
        let win = HWND(h as *mut _);
        let mons = ctx.shared.monitors.read().unwrap().clone();
        let gap = ctx.shared.config.read().unwrap().zone_gap;
        let info = wininfo::inspect(win);
        let Some(mon) = monitor::monitor_at(&info.rect, &mons).cloned() else { return };
        match ctx.tracker.get(h).cloned() {
            Some(WinState::Locked(l)) if l.kind == LockKind::ZoneFsMoved => {
                let ok = place::move_exact(win, &mon.rect);
                ctx.released_fs.insert(h, l);
                ctx.tracker.set(h, WinState::Ignored);
                log::line(format!("fs-toggle hwnd={h:#x} {} -> whole display ok={ok}", info.exe));
            }
            Some(WinState::Locked(mut l)) if l.kind == LockKind::ZoneFsBorderless => {
                if l.target == mon.rect {
                    // stretched by us before: back to the zone
                    if let Some(orig) = ctx.released_fs.remove(&h) {
                        let ok = place::move_exact(win, &orig.target.inflate(-gap));
                        ctx.tracker.set(h, WinState::Locked(orig));
                        log::line(format!("fs-toggle hwnd={h:#x} {} -> zone ok={ok}", info.exe));
                    }
                } else {
                    ctx.released_fs.insert(h, l.clone());
                    l.target = mon.rect;
                    let ok = place::move_exact(win, &mon.rect);
                    ctx.tracker.set(h, WinState::Locked(l));
                    log::line(format!("fs-toggle hwnd={h:#x} {} -> whole display ok={ok}", info.exe));
                }
            }
            _ => {
                // released earlier and still fullscreen: back into its zone
                let Some(mut l) = ctx.released_fs.remove(&h) else { return };
                if !info.rect.approx_eq(&mon.rect, 8) {
                    return; // left fullscreen meanwhile: nothing to put back
                }
                l.refights = 0;
                l.first_refight = Instant::now();
                let ok = place::hold_fullscreen(win, &l.target.inflate(-gap));
                ctx.tracker.set(h, WinState::Locked(l));
                log::line(format!("fs-toggle hwnd={h:#x} {} -> zone ok={ok}", info.exe));
            }
        }
    });
}

pub fn stop(thread_id: u32) {
    unsafe {
        let _ = PostThreadMessageW(thread_id, WM_STOP, Default::default(), LPARAM(0));
    }
}

fn for_each_top_level(mut f: impl FnMut(HWND)) {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let f = &mut *(lparam.0 as *mut &mut dyn FnMut(HWND));
        f(hwnd);
        windows::core::BOOL(1)
    }
    let mut cb_ref: &mut dyn FnMut(HWND) = &mut f;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(cb),
            LPARAM(&mut cb_ref as *mut _ as isize),
        );
    }
}

/// Evaluate synchronously from any thread that has installed CTX via
/// `install_for_test` — used by integration tests.
pub fn install_for_test(shared: Arc<Shared>) {
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            shared,
            tracker: Tracker::default(),
            snaps: Snaps::default(),
            resnap_until: None,
            drag: None,
            settle: Default::default(),
            released_fs: Default::default(),
            setup_key: String::new(),
            setup_due: None,
            desk: String::new(),
            desk_due: None,
        });
    });
}

pub fn evaluate_hwnd(hwnd: HWND) {
    let _ = evaluate(hwnd);
}
