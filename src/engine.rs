//! Containment engine: pure decision logic.
//!
//! Two containment paths share one tracker:
//! - **Maximize** (`WS_MAXIMIZE`): restore + SetWindowPos into the zone.
//! - **Windowed fullscreen** (window covering the monitor, not zoomed):
//!   the app STAYS in its own fullscreen mode and we resize it into the
//!   zone with `SWP_NOSENDCHANGING` (a plain SetWindowPos is undone by
//!   Chromium in WM_WINDOWPOSCHANGING; skipping that message makes the
//!   resize hold - verified 2026-11-19 on Chrome F11 and element
//!   fullscreen: page/video fill the zone, no browser UI). If the app
//!   keeps re-asserting past the refight budget, apps with a known exit
//!   key fall back to the old path (post key, strip frame, park
//!   borderless); others are ignored. A held window that leaves
//!   fullscreen or lands on another display is released, never pulled
//!   back. Exclusive fullscreen is always ignored (born-covering rule).

use std::time::{Duration, Instant};

use crate::monitor::Monitor;
use crate::rect::Rect;
use crate::state::{LockKind, LockedState, NormalState, WinState};
use crate::wininfo::WindowInfo;
use crate::zones::Layout;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Restore-if-zoomed + SetWindowPos to this rect (frame kept).
    ContainMaximize(Rect),
    /// Hold the app in its own fullscreen mode, resized to this zone rect
    /// (SWP_NOSENDCHANGING).
    ContainFsMove(Rect),
    /// Post this virtual key to the window so the app leaves its internal
    /// fullscreen mode; `target` is remembered for the relock step.
    SendFsExitKey { vk: u32, target: Rect },
    /// Strip WS_CAPTION/WS_THICKFRAME and place at this rect.
    LockFsBorderless(Rect),
    /// A locked window grew past the display edge by itself (minimum
    /// size larger than the zone): slide it back to this same-size rect.
    KeepOnScreen(Rect),
    /// A held fullscreen app left fullscreen somewhere odd (Firefox
    /// restores to the zone size): put it back on its pre-fullscreen rect.
    RestoreNormal(Rect),
    /// Restore original styles and place at this rect (user release, or a
    /// locked borderless window that moved on its own).
    ReleaseFs {
        place_at: Rect,
        orig_style: i64,
        orig_exstyle: i64,
    },
    Track(NormalState),
    Ignore,
    Nothing,
    Forget,
}

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub enabled: bool,
    pub zone_gap: i32,
    pub fullscreen_tolerance: i32,
    pub max_refights: u32,
    pub refight_window: Duration,
    /// Virtual key to post so the app exits internal fullscreen
    /// (resolved per-process from config; None = no known key).
    pub fs_exit_key: Option<u32>,
    /// The process is set to "stable" fullscreen: leave its fullscreen
    /// (exit key) and park it frameless instead of holding it.
    pub fs_stable: bool,
    /// Delay before re-posting the exit key while waiting for the drop.
    pub exit_retry: Duration,
    /// Max exit-key posts before giving up and ignoring the window.
    pub max_exit_sends: u32,
}

const WS_CAPTION_BITS: i64 = 0x00C0_0000;

fn caption_bits(style: i64) -> bool {
    style & WS_CAPTION_BITS == WS_CAPTION_BITS
}

fn covers_monitor(rect: &Rect, mon: &Monitor, tol: i32) -> bool {
    rect.approx_eq(&mon.rect, tol) || rect.approx_eq(&mon.work, tol)
}

fn zone_target(layout: &Layout, mon: &Monitor, normal: &NormalState) -> Option<Rect> {
    let frac = layout
        .zone_frac(normal.zone)
        .or_else(|| layout.zone_frac(0))?;
    Some(frac.to_pixels(&mon.work, 0))
}

/// Slack (px) a candidate "normal" rect may exceed the virtual screen
/// before we treat it as a transient fullscreen-entry artifact. Only
/// applies when overwriting an existing Normal record; first sightings
/// and Ignored->Track transitions are taken as-is so a window whose
/// saved position is genuinely huge does not stay ignored forever.
const NORMAL_DESKTOP_SLACK: i32 = 64;

fn tracked_zone(layout: Option<&Layout>, mon: &Monitor, info: &WindowInfo) -> usize {
    layout
        .and_then(|l| l.zone_for_rect(&mon.work, &info.rect))
        .unwrap_or(0)
}

fn make_normal(info: &WindowInfo, mon: &Monitor, layout: Option<&Layout>) -> NormalState {
    NormalState {
        rect: info.rect,
        monitor: mon.name.clone(),
        zone: tracked_zone(layout, mon, info),
    }
}

/// Window-rect tolerance for "sits in its zone": covers the invisible
/// resize borders (~7 px at 100 %, ~11 px at 150 %).
const ZONE_FIT_TOL: i32 = 16;

/// Entry into the F11 path for a window that has a remembered normal state.
/// `contained` = it was maximize-contained in its zone just before.
/// Only a window that was IN its zone keeps its fullscreen there: a
/// free-floating window (never snapped, or moved out of its zone) going
/// fullscreen (Parsec connecting, a game, a player) gets the whole display.
fn fs_entry(
    normal: &NormalState,
    mon: &Monitor,
    layout: Option<&Layout>,
    pol: &Policy,
    contained: bool,
) -> Action {
    // the user put this window into fullscreen on another display than
    // its home: respect it, do not drag it back
    if normal.monitor != mon.name {
        return Action::Ignore;
    }
    let Some(t) = layout.and_then(|l| zone_target(l, mon, normal)) else {
        return Action::Ignore;
    };
    let gap_t = t.inflate(-pol.zone_gap);
    if !contained && !crate::resnap::fills_zone_within(&normal.rect, &gap_t, ZONE_FIT_TOL) {
        return Action::Ignore;
    }
    match pol.fs_exit_key {
        Some(vk) if pol.fs_stable => Action::SendFsExitKey { vk, target: t },
        _ => Action::ContainFsMove(t),
    }
}

pub fn decide(
    info: &WindowInfo,
    prev: Option<&WinState>,
    mon: Option<&Monitor>,
    layout: Option<&Layout>,
    desktop: Option<&Rect>,
    pol: &Policy,
) -> Action {
    if !pol.enabled {
        return Action::Nothing;
    }
    if !info.valid || info.rect.is_empty() {
        return Action::Forget;
    }
    let Some(mon) = mon else {
        return Action::Forget;
    };

    let covers = covers_monitor(&info.rect, mon, pol.fullscreen_tolerance);
    let zoomed = info.is_zoomed();

    // Maximize is the user's: a maximized window really fills its display
    // and is not managed meanwhile (state dropped: restoring it starts
    // fresh, and F11 from maximized is a plain fullscreen). Exception: the
    // frameless "stable" fullscreen states, which own the window's frame
    // and must give it back.
    let owns_frame = matches!(
        prev,
        Some(WinState::Locked(l))
            if matches!(l.kind, LockKind::ZoneFsExitSent | LockKind::ZoneFsBorderless)
    );
    if zoomed && !owns_frame {
        return Action::Forget;
    }

    match prev {
        Some(WinState::Locked(l)) => decide_locked(info, l, mon, layout, covers, zoomed, pol),
        Some(WinState::Ignored) => {
            if covers {
                Action::Nothing
            } else {
                Action::Track(make_normal(info, mon, layout))
            }
        }
        prev_state => {
            let normal = prev_state.and_then(|s| s.normal().cloned());
            if covers {
                match &normal {
                    Some(n) => fs_entry(n, mon, layout, pol, false),
                    None => Action::Ignore, // born covering: games, splashes
                }
            } else if matches!(prev_state, Some(WinState::Normal(_)))
                && desktop.is_some_and(|d| {
                    !info.rect.is_within(d, NORMAL_DESKTOP_SLACK)
                })
            {
                // transient fullscreen-entry artifact: keep the good rect
                Action::Nothing
            } else {
                Action::Track(make_normal(info, mon, layout))
            }
        }
    }
}

fn decide_locked(
    info: &WindowInfo,
    l: &LockedState,
    mon: &Monitor,
    layout: Option<&Layout>,
    covers: bool,
    _zoomed: bool,
    pol: &Policy,
) -> Action {
    match l.kind {
        // (a maximized window never reaches here: `decide` drops it)
        LockKind::Maximize => {
            if covers {
                // F11 pressed while maximize-locked.
                return fs_entry(&l.normal, mon, layout, pol, true);
            }
            if info.rect.approx_eq(&l.target, pol.fullscreen_tolerance) {
                return Action::Nothing;
            }
            Action::Track(make_normal(info, mon, layout))
        }
        LockKind::ZoneFsMoved => {
            // `orig_style` = style while fullscreen (captured at hold time).
            // Getting a caption back means the app left fullscreen, even
            // if it is still sitting on the zone rect (Chrome sometimes
            // keeps the zone size when F11 is released).
            if caption_bits(info.style) && !caption_bits(l.orig_style) {
                if mon.name == l.normal.monitor
                    && !info.rect.approx_eq(&l.normal.rect, pol.fullscreen_tolerance)
                {
                    return Action::RestoreNormal(l.normal.rect);
                }
                return Action::Track(make_normal(info, mon, layout));
            }
            if info.rect.approx_eq(&l.target, pol.fullscreen_tolerance) {
                return Action::Nothing;
            }
            if covers {
                if mon.name != l.normal.monitor {
                    // moved to another display while fullscreen (e.g.
                    // Win+Shift+Arrow): the user's call, release it
                    return Action::Ignore;
                }
                // app re-asserted its fullscreen bounds: hold again,
                // then fall back (exit key) or yield once over budget
                if !over_budget(l, pol) {
                    return Action::ContainFsMove(l.target);
                }
                return match pol.fs_exit_key {
                    Some(vk) => Action::SendFsExitKey { vk, target: l.target },
                    None => Action::Ignore,
                };
            }
            if info.rect.overlap_area(&l.target) * 2 >= info.rect.area() {
                // still fullscreen, the app nudged its size inside the
                // zone (Firefox enforces a ~516px minimum width): keep it,
                // on screen
                return keep_on_screen(info, mon);
            }
            // moved away by something else: released where it is
            Action::Track(make_normal(info, mon, layout))
        }
        LockKind::ZoneFsExitSent => {
            if covers {
                let waited = l
                    .exit_sent_at
                    .map(|t| t.elapsed() > pol.exit_retry)
                    .unwrap_or(true);
                if waited && l.refights < pol.max_exit_sends {
                    return match pol.fs_exit_key {
                        Some(vk) => Action::SendFsExitKey {
                            vk,
                            target: l.target,
                        },
                        None => Action::Ignore,
                    };
                }
                if waited && l.refights >= pol.max_exit_sends {
                    return Action::Ignore;
                }
                return Action::Nothing;
            }
            // dropped to normal: strip the frame and park it in the zone
            Action::LockFsBorderless(l.target)
        }
        LockKind::ZoneFsBorderless => {
            if info.rect.approx_eq(&l.target, pol.fullscreen_tolerance) {
                return Action::Nothing;
            }
            if covers {
                // app re-entered fullscreen on its own
                return match pol.fs_exit_key {
                    Some(vk) => Action::SendFsExitKey {
                        vk,
                        target: l.target,
                    },
                    None => Action::Ignore,
                };
            }
            if info.rect.overlap_area(&l.target) * 2 >= info.rect.area() {
                // the app nudged its size inside the zone (Chrome's ~514px
                // minimum width): still parked, keep it, on screen
                return keep_on_screen(info, mon);
            }
            // moved away (user drag): put the frame back and re-track it
            // where it now sits
            Action::ReleaseFs {
                place_at: info.rect,
                orig_style: l.orig_style,
                orig_exstyle: l.orig_exstyle,
            }
        }
    }
}

/// Frameless / fullscreen windows have no invisible border, so the
/// window rect is what the user sees: keep it inside the work area.
fn keep_on_screen(info: &WindowInfo, mon: &Monitor) -> Action {
    match info.rect.shifted_into(&mon.work) {
        Some(r) => Action::KeepOnScreen(r),
        None => Action::Nothing,
    }
}

fn over_budget(l: &LockedState, pol: &Policy) -> bool {
    l.refights >= pol.max_refights && l.first_refight.elapsed() < pol.refight_window
}

/// Fold an applied `Action` back into tracker state.
/// `strip_result` carries (orig_style, orig_exstyle) when the applier
/// actually stripped a frame (LockFsBorderless only).
pub fn next_state(
    action: &Action,
    info: &WindowInfo,
    prev: Option<&WinState>,
    mon_name: &str,
    strip_result: Option<(i64, i64)>,
) -> Option<WinState> {
    let base_normal = || -> NormalState {
        prev.and_then(|s| s.normal().cloned()).unwrap_or(NormalState {
            rect: info.rect,
            monitor: mon_name.to_string(),
            zone: 0,
        })
    };
    match action {
        Action::Forget => None,
        Action::Nothing | Action::KeepOnScreen(_) => prev.cloned(),
        Action::Ignore => Some(WinState::Ignored),
        Action::Track(n) => {
            let mut n = n.clone();
            n.monitor = mon_name.to_string();
            Some(WinState::Normal(n))
        }
        Action::RestoreNormal(r) => {
            let mut n = base_normal();
            n.rect = *r;
            n.monitor = mon_name.to_string();
            Some(WinState::Normal(n))
        }
        Action::ReleaseFs { place_at, .. } => {
            let mut n = base_normal();
            n.rect = *place_at;
            n.monitor = mon_name.to_string();
            Some(WinState::Normal(n))
        }
        Action::ContainMaximize(target) => Some(
            bump_lock(prev, base_normal(), *target, LockKind::Maximize).into(),
        ),
        Action::ContainFsMove(target) => {
            let mut l = bump_lock(prev, base_normal(), *target, LockKind::ZoneFsMoved);
            // remember the fullscreen style: its caption coming back is
            // how we see the app leave fullscreen
            l.orig_style = info.style;
            Some(l.into())
        }
        Action::SendFsExitKey { vk, target } => {
            let mut l = bump_lock(prev, base_normal(), *target, LockKind::ZoneFsExitSent);
            l.internal_fs = true;
            l.exit_sent_at = Some(Instant::now());
            l.exit_vk = *vk;
            Some(l.into())
        }
        Action::LockFsBorderless(target) => {
            let (style, exstyle) = strip_result.unwrap_or((0, 0));
            Some(LockedState {
                normal: base_normal(),
                target: *target,
                kind: LockKind::ZoneFsBorderless,
                refights: 0,
                first_refight: Instant::now(),
                orig_style: style,
                orig_exstyle: exstyle,
                internal_fs: false,
                exit_sent_at: None,
                exit_vk: 0,
            }
            .into())
        }
    }
}

fn bump_lock(
    prev: Option<&WinState>,
    normal: NormalState,
    target: Rect,
    kind: LockKind,
) -> LockedState {
    let (refights, first) = match prev.and_then(|s| s.locked()) {
        Some(l) if l.kind == kind => (l.refights + 1, l.first_refight),
        _ => (0, Instant::now()),
    };
    LockedState {
        normal,
        target,
        kind,
        refights,
        first_refight: first,
        orig_style: 0,
        orig_exstyle: 0,
        internal_fs: kind != LockKind::Maximize,
        exit_sent_at: None,
        exit_vk: 0,
    }
}

impl From<LockedState> for WinState {
    fn from(l: LockedState) -> Self {
        WinState::Locked(l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rect::FracRect;
    use crate::zones::{Layout, Zone};

    const F11: u32 = 0x7A;

    fn mon() -> Monitor {
        Monitor {
            name: "\\\\.\\DISPLAY1".into(),
            rect: Rect::new(0, 0, 3440, 1440),
            work: Rect::new(0, 0, 3440, 1400),
            index: 0,
            dpi_x: 96,
            dpi_y: 96,
            primary: true,
            handle: 1,
        }
    }

    fn layout() -> Layout {
        Layout {
            name: "halves".into(),
            zones: vec![
                Zone {
                    name: "l".into(),
                    rect: FracRect::new(0.0, 0.0, 0.5, 1.0),
                },
                Zone {
                    name: "r".into(),
                    rect: FracRect::new(0.5, 0.0, 1.0, 1.0),
                },
            ],
        }
    }

    fn info(rect: Rect, zoomed: bool) -> WindowInfo {
        WindowInfo {
            hwnd: 1,
            rect,
            style: 0,
            ex_style: 0,
            class: "AppWindow".into(),
            exe: "chrome.exe".into(),
            pid: 1,
            visible: true,
            iconic: false,
            zoomed,
            cloaked: false,
            valid: true,
        }
    }

    fn pol(key: Option<u32>) -> Policy {
        Policy {
            enabled: true,
            zone_gap: 0,
            fullscreen_tolerance: 8,
            max_refights: 5,
            refight_window: Duration::from_millis(2000),
            fs_exit_key: key,
            fs_stable: false,
            exit_retry: Duration::from_millis(800),
            max_exit_sends: 3,
        }
    }

    /// Snapped in zone 0: window rect = zone + invisible borders.
    fn snapped_zone0() -> WinState {
        WinState::Normal(NormalState {
            rect: Rect::new(-7, 0, 1727, 1407),
            monitor: "\\\\.\\DISPLAY1".into(),
            zone: 0,
        })
    }

    fn normal_zone0() -> WinState {
        WinState::Normal(NormalState {
            rect: Rect::new(100, 100, 900, 700),
            monitor: "\\\\.\\DISPLAY1".into(),
            zone: 0,
        })
    }

    #[test]
    fn normal_window_is_tracked_with_zone() {
        let i = info(Rect::new(100, 100, 900, 700), false);
        match decide(&i, None, Some(&mon()), Some(&layout()), None, &pol(Some(F11))) {
            Action::Track(n) => assert_eq!(n.zone, 0),
            a => panic!("expected Track, got {a:?}"),
        }
    }

    #[test]
    fn maximize_is_a_real_maximize() {
        // snapped, free, never seen, ignored, held fullscreen: the
        // maximized window is left alone and its state dropped
        let maxed = info(Rect::new(-8, -8, 3448, 1408), true);
        for st in [Some(snapped_zone0()), Some(normal_zone0()), None, Some(WinState::Ignored), Some(held(0, HOME))] {
            assert_eq!(
                decide(&maxed, st.as_ref(), Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
                Action::Forget
            );
        }
        // restored afterwards: tracked fresh where it lands
        let back = info(Rect::new(-7, 0, 1727, 1407), false);
        assert!(matches!(
            decide(&back, None, Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::Track(_)
        ));
        // F11 from maximized (no state any more): plain fullscreen
        let covering = info(Rect::new(0, 0, 3440, 1440), false);
        assert_eq!(
            decide(&covering, None, Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::Ignore
        );
    }

    const HOME: &str = "\\\\.\\DISPLAY1";

    fn held(refights: u32, home: &str) -> WinState {
        WinState::Locked(LockedState {
            normal: NormalState {
                rect: Rect::new(100, 100, 900, 700),
                monitor: home.into(),
                zone: 0,
            },
            target: Rect::new(0, 0, 1720, 1400),
            kind: LockKind::ZoneFsMoved,
            refights,
            first_refight: Instant::now(),
            orig_style: 0,
            orig_exstyle: 0,
            internal_fs: true,
            exit_sent_at: None,
            exit_vk: 0,
        })
    }

    #[test]
    fn f11_holds_fullscreen_in_zone_even_with_exit_key() {
        let st = snapped_zone0();
        let f11 = info(Rect::new(0, 0, 3440, 1440), false);
        match decide(&f11, Some(&st), Some(&mon()), Some(&layout()), None, &pol(Some(F11))) {
            Action::ContainFsMove(r) => assert_eq!(r, Rect::new(0, 0, 1720, 1400)),
            a => panic!("expected ContainFsMove (hold), got {a:?}"),
        }
    }

    #[test]
    fn stable_app_takes_the_exit_key_path() {
        let st = snapped_zone0();
        let f11 = info(Rect::new(0, 0, 3440, 1440), false);
        let mut p = pol(Some(F11));
        p.fs_stable = true;
        match decide(&f11, Some(&st), Some(&mon()), Some(&layout()), None, &p) {
            Action::SendFsExitKey { vk, target } => {
                assert_eq!(vk, F11);
                assert_eq!(target, Rect::new(0, 0, 1720, 1400));
            }
            a => panic!("expected SendFsExitKey, got {a:?}"),
        }
        // stable without a known key cannot leave fullscreen: hold instead
        let mut p = pol(None);
        p.fs_stable = true;
        assert!(matches!(
            decide(&f11, Some(&st), Some(&mon()), Some(&layout()), None, &p),
            Action::ContainFsMove(_)
        ));
    }

    #[test]
    fn held_reassert_rehold_then_fallback() {
        let covering = info(Rect::new(0, 0, 3440, 1440), false);
        // within budget: hold again
        assert_eq!(
            decide(&covering, Some(&held(1, HOME)), Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::ContainFsMove(Rect::new(0, 0, 1720, 1400))
        );
        // over budget: exit-key fallback when a key is known, else yield
        match decide(&covering, Some(&held(5, HOME)), Some(&mon()), Some(&layout()), None, &pol(Some(F11))) {
            Action::SendFsExitKey { vk, .. } => assert_eq!(vk, F11),
            a => panic!("expected SendFsExitKey fallback, got {a:?}"),
        }
        assert_eq!(
            decide(&covering, Some(&held(5, HOME)), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::Ignore
        );
    }

    #[test]
    fn held_fullscreen_released_on_exit_or_other_display() {
        // Esc: app restores its normal rect (caption back) -> released
        let mut restored = info(Rect::new(100, 100, 900, 700), false);
        restored.style = WS_CAPTION_BITS;
        assert!(matches!(
            decide(&restored, Some(&held(0, HOME)), Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::Track(_)
        ));
        // moved to another display while fullscreen: never pulled back
        let covering = info(Rect::new(0, 0, 3440, 1440), false);
        assert_eq!(
            decide(&covering, Some(&held(0, "\\\\.\\DISPLAY2")), Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::Ignore
        );
        // fullscreen entered on another display than home: not held
        let other_home = WinState::Normal(NormalState {
            rect: Rect::new(100, 100, 900, 700),
            monitor: "\\\\.\\DISPLAY2".into(),
            zone: 0,
        });
        assert_eq!(
            decide(&covering, Some(&other_home), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::Ignore
        );
        // caption back while still on the zone rect (Chrome): restored
        let mut on_zone = info(Rect::new(0, 0, 1720, 1400), false);
        on_zone.style = WS_CAPTION_BITS;
        assert_eq!(
            decide(&on_zone, Some(&held(0, HOME)), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::RestoreNormal(Rect::new(100, 100, 900, 700))
        );
        // an app that covers WITH its frame keeps it while held: not a release
        let mut framed = held(0, HOME);
        if let WinState::Locked(l) = &mut framed {
            l.orig_style = WS_CAPTION_BITS;
        }
        assert_eq!(
            decide(&on_zone, Some(&framed), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::Nothing
        );
        // app restored somewhere else (Firefox): put back on the old rect
        let mut odd = info(Rect::new(-9, 4, 507, 898), false);
        odd.style = WS_CAPTION_BITS;
        assert_eq!(
            decide(&odd, Some(&held(0, HOME)), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::RestoreNormal(Rect::new(100, 100, 900, 700))
        );
        // still fullscreen but widened by the app inside the zone: kept
        let nudged = info(Rect::new(0, 0, 1756, 1400), false);
        assert_eq!(
            decide(&nudged, Some(&held(0, HOME)), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::Nothing
        );
        // widened past the right edge of the display: slid back on screen
        let mut right = held(0, HOME);
        if let WinState::Locked(l) = &mut right {
            l.target = Rect::new(1720, 0, 3440, 1400);
        }
        let past = info(Rect::new(1720, 0, 3476, 1400), false);
        assert_eq!(
            decide(&past, Some(&right), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::KeepOnScreen(Rect::new(1684, 0, 3440, 1400))
        );
        // left fullscreen into maximized: a real maximize, let go
        let mut zoomed = info(Rect::new(-8, -8, 3448, 1408), true);
        zoomed.style = WS_CAPTION_BITS;
        assert_eq!(
            decide(&zoomed, Some(&held(0, HOME)), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::Forget
        );
        // the held state keeps counting refights across re-holds
        let st = held(2, HOME);
        let ns = next_state(&Action::ContainFsMove(Rect::new(0, 0, 1720, 1400)), &covering, Some(&st), HOME, None);
        assert_eq!(ns.unwrap().locked().unwrap().refights, 3);
    }

    #[test]
    fn fullscreen_of_a_window_outside_its_zone_gets_the_whole_display() {
        // never snapped / moved out of its zone (Parsec connecting, a
        // player, a game): real fullscreen, not held in a zone
        let covering = info(Rect::new(0, 0, 3440, 1440), false);
        for key in [Some(F11), None] {
            assert_eq!(
                decide(&covering, Some(&normal_zone0()), Some(&mon()), Some(&layout()), None, &pol(key)),
                Action::Ignore
            );
        }
        // maximize-contained in its zone, then F11: held in the zone
        let mut maxed = held(0, HOME);
        if let WinState::Locked(l) = &mut maxed {
            l.kind = LockKind::Maximize;
        }
        assert!(matches!(
            decide(&covering, Some(&maxed), Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::ContainFsMove(_)
        ));
        // snapped, but a bit wider than the zone (app minimum size): held
        let wide = WinState::Normal(NormalState {
            rect: Rect::new(-7, 0, 1900, 1407),
            monitor: HOME.into(),
            zone: 0,
        });
        assert!(matches!(
            decide(&covering, Some(&wide), Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::ContainFsMove(_)
        ));
    }
    #[test]
    fn f11_without_key_tries_plain_move() {
        let st = snapped_zone0();
        let f11 = info(Rect::new(0, 0, 3440, 1440), false);
        match decide(&f11, Some(&st), Some(&mon()), Some(&layout()), None, &pol(None)) {
            Action::ContainFsMove(r) => assert_eq!(r, Rect::new(0, 0, 1720, 1400)),
            a => panic!("expected ContainFsMove, got {a:?}"),
        }
    }

    #[test]
    fn reassert_without_key_is_ignored() {
        let l = WinState::Locked(LockedState {
            normal: NormalState {
                rect: Rect::new(0, 0, 100, 100),
                monitor: "m".into(),
                zone: 0,
            },
            target: Rect::new(0, 0, 1720, 1400),
            kind: LockKind::ZoneFsMoved,
            refights: 5,
            first_refight: Instant::now(),
            orig_style: 0,
            orig_exstyle: 0,
            internal_fs: true,
            exit_sent_at: None,
            exit_vk: 0,
        });
        let covering = info(Rect::new(0, 0, 3440, 1440), false);
        assert_eq!(
            decide(&covering, Some(&l), Some(&mon()), Some(&layout()), None, &pol(None)),
            Action::Ignore
        );
    }

    #[test]
    fn exit_sent_then_drop_locks_borderless() {
        let l = WinState::Locked(LockedState {
            normal: NormalState {
                rect: Rect::new(0, 0, 100, 100),
                monitor: "m".into(),
                zone: 0,
            },
            target: Rect::new(0, 0, 1720, 1400),
            kind: LockKind::ZoneFsExitSent,
            refights: 1,
            first_refight: Instant::now(),
            orig_style: 0,
            orig_exstyle: 0,
            internal_fs: true,
            exit_sent_at: Some(Instant::now()),
            exit_vk: 0,
        });
        let dropped = info(Rect::new(300, 200, 1200, 900), false);
        match decide(&dropped, Some(&l), Some(&mon()), Some(&layout()), None, &pol(Some(F11))) {
            Action::LockFsBorderless(r) => assert_eq!(r, Rect::new(0, 0, 1720, 1400)),
            a => panic!("expected LockFsBorderless, got {a:?}"),
        }
    }

    #[test]
    fn borderless_moved_gets_frame_back() {
        let l = WinState::Locked(LockedState {
            normal: NormalState {
                rect: Rect::new(0, 0, 100, 100),
                monitor: "m".into(),
                zone: 0,
            },
            target: Rect::new(0, 0, 1720, 1400),
            kind: LockKind::ZoneFsBorderless,
            refights: 0,
            first_refight: Instant::now(),
            orig_style: 0x16CA0000,
            orig_exstyle: 0x100,
            internal_fs: false,
            exit_sent_at: None,
            exit_vk: 0,
        });
        // nudged by the app inside the zone: still parked
        let nudged = info(Rect::new(0, 0, 1760, 1400), false);
        assert_eq!(
            decide(&nudged, Some(&l), Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::Nothing
        );
        // dragged away by the user: frame back
        let moved = info(Rect::new(2000, 500, 2400, 900), false);
        match decide(&moved, Some(&l), Some(&mon()), Some(&layout()), None, &pol(Some(F11))) {
            Action::ReleaseFs { orig_style, .. } => assert_eq!(orig_style, 0x16CA0000),
            a => panic!("expected ReleaseFs, got {a:?}"),
        }
    }

    #[test]
    fn born_fullscreen_is_ignored() {
        let game = info(Rect::new(0, 0, 3440, 1440), false);
        assert_eq!(
            decide(&game, None, Some(&mon()), Some(&layout()), None, &pol(Some(F11))),
            Action::Ignore
        );
        assert_eq!(
            decide(
                &game,
                Some(&WinState::Ignored),
                Some(&mon()),
                Some(&layout()),
                None,
                &pol(Some(F11))
            ),
            Action::Nothing
        );
    }

    #[test]
    fn transient_giant_rect_does_not_overwrite_normal() {
        // While entering fullscreen some apps pass through a geometry
        // larger than the whole virtual screen (observed on Firefox).
        // It must not pollute the remembered normal rect.
        let desktop = Rect::new(0, 0, 4480, 2369);
        let st = normal_zone0();
        let giant = info(Rect::new(256, 256, 4864, 2304), false);
        assert_eq!(
            decide(
                &giant,
                Some(&st),
                Some(&mon()),
                Some(&layout()),
                Some(&desktop),
                &pol(Some(F11))
            ),
            Action::Nothing
        );
        // a legit off-screen hang (within slack) still tracks fine
        let hanging = info(Rect::new(-40, 100, 900, 700), false);
        assert!(matches!(
            decide(
                &hanging,
                Some(&st),
                Some(&mon()),
                Some(&layout()),
                Some(&desktop),
                &pol(Some(F11))
            ),
            Action::Track(_)
        ));
    }

    #[test]
    fn disabled_policy_does_nothing() {
        let mut p = pol(Some(F11));
        p.enabled = false;
        let i = info(Rect::new(0, 0, 3440, 1440), true);
        assert_eq!(
            decide(&i, None, Some(&mon()), Some(&layout()), None, &p),
            Action::Nothing
        );
    }
}
