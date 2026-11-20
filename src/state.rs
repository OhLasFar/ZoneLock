//! Per-window state machine memory. Pure data, no Win32 calls.

use std::collections::HashMap;
use std::time::Instant;

use crate::rect::Rect;

/// What the window looked like last time it was a plain normal window.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalState {
    pub rect: Rect,
    /// Monitor device name the window was mostly on.
    pub monitor: String,
    /// Zone index (inside the monitor's layout) chosen by overlap.
    pub zone: usize,
}

/// Sub-mode of a zone-locked window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    /// Plain maximize that we pulled back into the zone. The window keeps
    /// its normal frame; it just sits maximized-looking inside the zone.
    Maximize,
    /// Windowed fullscreen (F11, video fullscreen) HELD in the zone: the
    /// app stays in its own fullscreen mode, resized with
    /// SWP_NOSENDCHANGING. Frame belongs to the app, not us.
    ZoneFsMoved,
    /// Fallback when a held app keeps re-asserting: we posted its
    /// fullscreen-exit key and are waiting for the drop to normal.
    ZoneFsExitSent,
    /// Final state after the app exited internal fullscreen: we stripped
    /// WS_CAPTION/WS_THICKFRAME ourselves and parked it in the zone.
    /// orig_style/exstyle are needed to restore the frame on release.
    ZoneFsBorderless,
}

#[derive(Debug, Clone)]
pub struct LockedState {
    pub normal: NormalState,
    /// Zone rect the window must occupy.
    pub target: Rect,
    pub kind: LockKind,
    /// Consecutive re-containments inside the refight window.
    pub refights: u32,
    pub first_refight: Instant,
    /// Style/exstyle captured before we stripped the frame
    /// (ZoneFsBorderless only).
    pub orig_style: i64,
    pub orig_exstyle: i64,
    /// True when the app still believes it is fullscreen
    /// (ZoneFsMoved only): releasing needs its exit key re-posted.
    pub internal_fs: bool,
    /// When we last posted the exit key (ZoneFsExitSent).
    pub exit_sent_at: Option<Instant>,
    /// The vk posted to make this app leave internal fullscreen
    /// (0 = none configured; the app usually toggles on F11 itself then).
    pub exit_vk: u32,
}

#[derive(Debug, Clone)]
pub enum WinState {
    Normal(NormalState),
    Locked(LockedState),
    /// Seen covering a monitor without a prior normal phase: exclusive
    /// fullscreen games, UWP splashes, the shell. Never touched.
    Ignored,
}

impl WinState {
    pub fn normal(&self) -> Option<&NormalState> {
        match self {
            WinState::Normal(n) => Some(n),
            WinState::Locked(l) => Some(&l.normal),
            WinState::Ignored => None,
        }
    }

    pub fn locked(&self) -> Option<&LockedState> {
        match self {
            WinState::Locked(l) => Some(l),
            _ => None,
        }
    }

    /// Foreground-key suppression should apply to any in-flight or final
    /// zone-fullscreen lock.
    pub fn is_zone_fs(&self) -> bool {
        matches!(
            self.locked().map(|l| l.kind),
            Some(
                LockKind::ZoneFsMoved
                    | LockKind::ZoneFsExitSent
                    | LockKind::ZoneFsBorderless
            )
        )
    }
}

/// HWND -> last known state. Entries are pruned when windows die.
#[derive(Default)]
pub struct Tracker {
    map: HashMap<isize, WinState>,
}

impl Tracker {
    pub fn get(&self, hwnd: isize) -> Option<&WinState> {
        self.map.get(&hwnd)
    }

    pub fn set(&mut self, hwnd: isize, s: WinState) {
        self.map.insert(hwnd, s);
    }

    pub fn remove(&mut self, hwnd: isize) {
        self.map.remove(&hwnd);
    }

    pub fn retain(&mut self, mut alive: impl FnMut(isize) -> bool) {
        self.map.retain(|h, _| alive(*h));
    }

    /// HWNDs waiting for their app to leave internal fullscreen:
    /// the only state whose retry clock needs periodic polling
    /// (every other transition is driven by WinEvents).
    pub fn pending_exit(&self) -> Vec<isize> {
        self.map
            .iter()
            .filter(|(_, s)| {
                matches!(
                    s.locked().map(|l| l.kind),
                    Some(LockKind::ZoneFsExitSent)
                )
            })
            .map(|(h, _)| *h)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Every tracked HWND (snapshot, safe to mutate the tracker after).
    pub fn hwnds(&self) -> Vec<isize> {
        self.map.keys().copied().collect()
    }
}
