//! Zone editor overlay. One topmost translucent window per monitor; draw
//! zones with the mouse (or click the "+" shown in free space to fill
//! it), drag handle dots to move/resize them (edges shared with
//! neighbours move together, Ctrl = this zone only), right-click
//! deletes, number keys apply presets, Enter commits to the shared
//! config (a new preset is named first in an in-overlay card), Esc
//! cancels. Painting is double-buffered and edges snap magnetically to
//! monitor fractions and other zones' edges.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::monitor::Monitor;
use crate::rect::{FracRect, Rect};
use crate::uidraw as ui;
use crate::watcher::Shared;
use crate::zones::{Layout, Zone};

const CLASS: PCWSTR = w!("ZoneLockOverlay");
const ALPHA: u8 = 235; // editor is nearly opaque so zones read clearly

const SNAP_PX: i32 = 14;
const MIN_ZONE_PX: i32 = 48;
const HANDLE_PX: i32 = 11;
const DOT_R: i32 = 5;

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NW,
    NE,
    SW,
    SE,
}

#[derive(Clone)]
struct EditDrag {
    zone: usize,
    handle: Handle,
    start: POINT,
    orig: Rect, // client-px rect at drag start
    /// Other zones' edges on the vertical line being dragged (zone,
    /// far = right edge, orig rect): they follow the dragged edge.
    links_v: Vec<(usize, bool, Rect)>,
    /// Same for the horizontal line (far = bottom edge).
    links_h: Vec<(usize, bool, Rect)>,
}

/// In-overlay "name this layout" prompt (new preset drawn from the
/// settings panel's "Draw my own").
struct Naming {
    hwnd: isize,
    text: String,
    /// Suggested name still untouched: the first typed character
    /// replaces it.
    fresh: bool,
    error: Option<String>,
}

/// Edges closer than this are one shared line (client px).
const LINK_PX: i32 = 2;
/// Radius of the "+" button shown in free space.
const PLUS_R: i32 = 24;
/// Radius of the side "+" that splits a zone in two.
const SPLIT_R: i32 = 10;

struct OvWin {
    mon: Monitor,
    layout: Layout,
    drag_start: Option<POINT>,
    drag_cur: Option<POINT>,
    edit: Option<EditDrag>,
    /// Positions of snapped edges to highlight, (vertical?, client px).
    guides: Vec<(bool, i32)>,
    hover: Option<(usize, Handle)>,
    plus_hover: bool,
    grid_hover: bool,
    /// Crossed-out app icon under the mouse: (zone, icon index).
    icon_hover: Option<(usize, usize)>,
    /// Zone under the mouse (its side "+" split buttons are shown).
    zone_hover: Option<usize>,
    /// Side "+" under the mouse: (zone, side).
    split_hover: Option<(usize, Side)>,
    /// One-line message shown in the hint pill (e.g. "draw a zone first").
    notice: Option<String>,
    dpi: u32,
}

thread_local! {
    static WINDOWS: RefCell<HashMap<isize, OvWin>> = RefCell::new(HashMap::new());
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// (monitor, preset name) when the editor was opened by Draw my own /
    /// Edit preset in the settings panel. Name None = a new preset whose
    /// name is asked when Enter is pressed.
    static PENDING: RefCell<Option<(String, Option<String>)>> = const { RefCell::new(None) };
    static NAMING: RefCell<Option<Naming>> = const { RefCell::new(None) };
    /// Drawing grid: index in GRID_STEPS (0 = off). Kept while ZoneLock
    /// runs, shared by every display.
    static GRID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// config.force_small_zones when the editor opened (warning wording).
    static FORCE_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Grid choices cycled by the Grid button / G key: cells per side.
const GRID_STEPS: [u32; 5] = [0, 4, 6, 8, 12];

fn grid_cells() -> u32 {
    GRID_STEPS[GRID.with(|g| g.get()) % GRID_STEPS.len()]
}

/// Next grid size, then repaint every display.
fn cycle_grid() {
    GRID.with(|g| g.set((g.get() + 1) % GRID_STEPS.len()));
    let hwnds: Vec<isize> = WINDOWS.with(|w| w.borrow().keys().copied().collect());
    for h in hwnds {
        unsafe {
            let _ = InvalidateRect(Some(HWND(h as *mut _)), None, false);
        }
    }
}

/// Grid lines along one axis of the work area, client px (inner lines).
fn grid_lines(a: i32, b: i32, cells: u32) -> Vec<i32> {
    (1..cells as i64)
        .map(|k| a + ((b - a) as i64 * k / cells as i64) as i32)
        .collect()
}

/// The Grid toggle pill, bottom left, on the same line as the hint pill
/// (over the taskbar strip, so it never covers a zone).
fn grid_btn_rect(ov: &OvWin) -> Rect {
    let w = ui::sx(148, ov.dpi);
    let h = ui::sx(30, ov.dpi);
    let top = ov.mon.rect.height() - h - ui::sx(22, ov.dpi);
    let left = ui::sx(16, ov.dpi);
    Rect::new(left, top, left + w, top + h)
}

fn on_grid_btn(ov: &OvWin, x: i32, y: i32) -> bool {
    let r = grid_btn_rect(ov);
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn pending_for(mon: &str) -> Option<String> {
    PENDING.with(|p| {
        p.borrow()
            .as_ref()
            .filter(|(m, _)| m == mon)
            .and_then(|(_, n)| n.clone())
    })
}

/// Monitor of a new preset still waiting for its name.
fn pending_unnamed() -> Option<String> {
    PENDING.with(|p| {
        p.borrow()
            .as_ref()
            .filter(|(_, n)| n.is_none())
            .map(|(m, _)| m.clone())
    })
}

fn naming_active() -> bool {
    NAMING.with(|n| n.borrow().is_some())
}

/// Open the editor on display `monitor` for a user preset. `Some(name)`
/// redraws that preset (Enter overwrites it); `None` starts an empty
/// drawing and asks for the name when Enter is pressed.
pub fn open_for_preset(shared: Arc<Shared>, monitor: String, name: Option<String>) {
    if is_open() {
        close_all(false);
    }
    PENDING.with(|p| *p.borrow_mut() = Some((monitor, name)));
    toggle(shared);
}

/// Snap `v` to the nearest target within `tol`; returns (value, target).
fn snap_to(v: i32, targets: &[i32], tol: i32) -> (i32, Option<i32>) {
    targets
        .iter()
        .filter(|t| (v - **t).abs() <= tol)
        .min_by_key(|t| (v - **t).abs())
        .map(|t| (*t, Some(*t)))
        .unwrap_or((v, None))
}

/// Snap targets along one axis: monitor edges, fractions (halves,
/// thirds, quarters, sixths) and every other zone's edges. Client px.
fn axis_targets(ov: &OvWin, vertical: bool, skip: &[usize]) -> Vec<i32> {
    // zone canvas = the work area (excludes the taskbar) in client coords
    let (a, b) = if vertical {
        (
            ov.mon.work.top - ov.mon.rect.top,
            ov.mon.work.bottom - ov.mon.rect.top,
        )
    } else {
        (
            ov.mon.work.left - ov.mon.rect.left,
            ov.mon.work.right - ov.mon.rect.left,
        )
    };
    let total = b - a;
    let mut v = vec![a, b];
    for d in [2i64, 3, 4, 6] {
        for n in 1..d {
            v.push(a + (total as i64 * n / d) as i32);
        }
    }
    v.extend(grid_lines(a, b, grid_cells()));
    for (i, z) in ov.layout.zones.iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        let r = zone_client_rect(z, &ov.mon.work, &ov.mon.rect);
        if vertical {
            v.push(r.top);
            v.push(r.bottom);
        } else {
            v.push(r.left);
            v.push(r.right);
        }
    }
    v
}

fn win_rect_from_drag(a: POINT, b: POINT) -> Rect {
    Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
}

/// `x`,`y` are overlay client coords; zones map onto the work area
/// (`work`) and the overlay window sits at `origin` (monitor rect).
fn point_in_zone(layout: &Layout, work: &Rect, origin: &Rect, x: i32, y: i32) -> Option<usize> {
    layout
        .zones
        .iter()
        .position(|z| z.rect.to_pixels(work, 0).contains_point(x + origin.left, y + origin.top))
}

/// Zone rect in overlay client coordinates: fractions are resolved on
/// the work area, then translated by the overlay's monitor origin.
fn zone_client_rect(z: &Zone, work: &Rect, origin: &Rect) -> Rect {
    let p = z.rect.to_pixels(work, 0);
    Rect::new(
        p.left - origin.left,
        p.top - origin.top,
        p.right - origin.left,
        p.bottom - origin.top,
    )
}

/// Handle anchor points for a zone rect, in client coordinates.
/// Index 0 is the center (Move); the rest are edges and corners.
fn handle_points(r: &Rect) -> [(Handle, i32, i32); 9] {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    [
        (Handle::Move, cx, cy),
        (Handle::NW, r.left, r.top),
        (Handle::N, cx, r.top),
        (Handle::NE, r.right, r.top),
        (Handle::E, r.right, cy),
        (Handle::W, r.left, cy),
        (Handle::SW, r.left, r.bottom),
        (Handle::S, cx, r.bottom),
        (Handle::SE, r.right, r.bottom),
    ]
}

/// Hit-test resize/move handles across zones (topmost zone wins).
fn handle_at(layout: &Layout, work: &Rect, origin: &Rect, x: i32, y: i32) -> Option<(usize, Handle)> {
    for (i, z) in layout.zones.iter().enumerate().rev() {
        let r = zone_client_rect(z, work, origin);
        for (h, hx, hy) in handle_points(&r).into_iter().skip(1) {
            if (x - hx).abs() <= HANDLE_PX && (y - hy).abs() <= HANDLE_PX {
                return Some((i, h));
            }
        }
        let (_, cx, cy) = handle_points(&r)[0];
        if (x - cx).abs() <= HANDLE_PX * 2 && (y - cy).abs() <= HANDLE_PX * 2 {
            return Some((i, Handle::Move));
        }
    }
    // anywhere along an edge grabs that edge (not only its dot)
    let near = HANDLE_PX / 2 + 2;
    for (i, z) in layout.zones.iter().enumerate().rev() {
        let r = zone_client_rect(z, work, origin);
        let in_y = y > r.top && y < r.bottom;
        let in_x = x > r.left && x < r.right;
        if in_y && (x - r.left).abs() <= near {
            return Some((i, Handle::W));
        }
        if in_y && (x - r.right).abs() <= near {
            return Some((i, Handle::E));
        }
        if in_x && (y - r.top).abs() <= near {
            return Some((i, Handle::N));
        }
        if in_x && (y - r.bottom).abs() <= near {
            return Some((i, Handle::S));
        }
    }
    None
}

/// Apply an edit-drag to the zone's client rect with magnetic snapping.
/// Returns the new rect and the guide positions actually snapped to.
/// `Move` snaps the whole rect rigidly (same size), edge/corner handles
/// snap the moved edges.
fn edited_rect(ed: &EditDrag, x: i32, y: i32, ov: &OvWin) -> (Rect, Vec<(bool, i32)>) {
    let tol = ui::sx(SNAP_PX, ov.dpi);
    // the dragged zone and the zones that follow it are not magnets
    let mut skip = vec![ed.zone];
    skip.extend(ed.links_v.iter().chain(&ed.links_h).map(|(j, _, _)| *j));
    let xt = axis_targets(ov, false, &skip);
    let yt = axis_targets(ov, true, &skip);
    let dx = x - ed.start.x;
    let dy = y - ed.start.y;
    let o = ed.orig;
    // keep zones inside the work area (client coords)
    let (mw_l, mw_r) = (
        ov.mon.work.left - ov.mon.rect.left,
        ov.mon.work.right - ov.mon.rect.left,
    );
    let (mh_t, mh_b) = (
        ov.mon.work.top - ov.mon.rect.top,
        ov.mon.work.bottom - ov.mon.rect.top,
    );
    let mut guides = Vec::new();
    let (mut l, mut t, mut r, mut b) = (o.left, o.top, o.right, o.bottom);

    match ed.handle {
        Handle::Move => {
            // keep the size: pick the snapped translation closest to the
            // raw delta, trying leading and trailing edges on each axis
            let snap_axis = |lo: i32, hi: i32, d: i32, targets: &[i32]| {
                let (lo_s, lo_t) = snap_to(lo + d, targets, tol);
                let (hi_s, hi_t) = snap_to(hi + d, targets, tol);
                match (lo_t, hi_t) {
                    (Some(_), None) => lo_s - lo,
                    (None, Some(_)) => hi_s - hi,
                    (Some(lt), Some(ht)) => {
                        if (lt - (lo + d)).abs() <= (ht - (hi + d)).abs() {
                            lo_s - lo
                        } else {
                            hi_s - hi
                        }
                    }
                    _ => d,
                }
            };
            let sdx = snap_axis(o.left, o.right, dx, &xt);
            let sdy = snap_axis(o.top, o.bottom, dy, &yt);
            if sdx != dx {
                guides.push((true, o.left + sdx));
            }
            if sdy != dy {
                guides.push((false, o.top + sdy));
            }
            l += sdx;
            r += sdx;
            t += sdy;
            b += sdy;
            l = l.clamp(mw_l, mw_r - (r - l));
            r = l + (o.right - o.left);
            t = t.clamp(mh_t, mh_b - (b - t));
            b = t + (o.bottom - o.top);
        }
        h => {
            let snap_edge = |v: i32, targets: &[i32], g: &mut Vec<(bool, i32)>, vert: bool| {
                let (s, t) = snap_to(v, targets, tol);
                if let Some(tp) = t {
                    g.push((vert, tp));
                }
                s
            };
            let moves_l = matches!(h, Handle::W | Handle::NW | Handle::SW);
            let moves_r = matches!(h, Handle::E | Handle::NE | Handle::SE);
            let moves_t = matches!(h, Handle::N | Handle::NW | Handle::NE);
            let moves_b = matches!(h, Handle::S | Handle::SW | Handle::SE);
            if moves_l {
                l = snap_edge(o.left + dx, &xt, &mut guides, true).clamp(mw_l, mw_r - MIN_ZONE_PX);
            }
            if moves_r {
                r = snap_edge(o.right + dx, &xt, &mut guides, true).clamp(mw_l + MIN_ZONE_PX, mw_r);
            }
            if moves_t {
                t = snap_edge(o.top + dy, &yt, &mut guides, false).clamp(mh_t, mh_b - MIN_ZONE_PX);
            }
            if moves_b {
                b = snap_edge(o.bottom + dy, &yt, &mut guides, false).clamp(mh_t + MIN_ZONE_PX, mh_b);
            }
            if r - l < MIN_ZONE_PX {
                if moves_l {
                    l = r - MIN_ZONE_PX;
                } else {
                    r = l + MIN_ZONE_PX;
                }
            }
            if b - t < MIN_ZONE_PX {
                if moves_t {
                    t = b - MIN_ZONE_PX;
                } else {
                    b = t + MIN_ZONE_PX;
                }
            }
            // a shared edge stops where a following zone would get too small
            if let Some((lo, hi)) = link_bounds(&ed.links_v, false) {
                if moves_l {
                    l = l.clamp(lo, hi.max(lo));
                } else if moves_r {
                    r = r.clamp(lo, hi.max(lo));
                }
            }
            if let Some((lo, hi)) = link_bounds(&ed.links_h, true) {
                if moves_t {
                    t = t.clamp(lo, hi.max(lo));
                } else if moves_b {
                    b = b.clamp(lo, hi.max(lo));
                }
            }
        }
    }
    (Rect::new(l, t, r, b), guides)
}

/// Range the shared line may take so every following zone keeps
/// `MIN_ZONE_PX`. `horizontal` = the links are on a horizontal line.
fn link_bounds(links: &[(usize, bool, Rect)], horizontal: bool) -> Option<(i32, i32)> {
    if links.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = (i32::MIN, i32::MAX);
    for (_, far, o) in links {
        let (near_v, far_v) = if horizontal { (o.top, o.bottom) } else { (o.left, o.right) };
        if *far {
            lo = lo.max(near_v + MIN_ZONE_PX);
        } else {
            hi = hi.min(far_v - MIN_ZONE_PX);
        }
    }
    Some((lo, hi))
}

/// Edges of other zones lying on the same line as `main`'s edge (left /
/// right when `vertical`, top / bottom otherwise; `far` = right / bottom)
/// and chained to it through overlapping spans: a splitter shared by
/// neighbours, as in FancyZones. Returns (zone, far) without `main`.
fn linked_edges(rects: &[Rect], main: usize, vertical: bool, far: bool, tol: i32) -> Vec<(usize, bool)> {
    let pos = |r: &Rect, f: bool| match (vertical, f) {
        (true, false) => r.left,
        (true, true) => r.right,
        (false, false) => r.top,
        (false, true) => r.bottom,
    };
    let span = |r: &Rect| if vertical { (r.top, r.bottom) } else { (r.left, r.right) };
    let line = pos(&rects[main], far);
    let mut set = vec![(main, far)];
    let mut cands: Vec<(usize, bool)> = (0..rects.len())
        .filter(|j| *j != main)
        .flat_map(|j| [(j, false), (j, true)])
        .filter(|(j, f)| (pos(&rects[*j], *f) - line).abs() <= tol)
        .collect();
    loop {
        let before = cands.len();
        cands.retain(|c| {
            let (a0, a1) = span(&rects[c.0]);
            let joined = set.iter().any(|s| {
                let (b0, b1) = span(&rects[s.0]);
                a1.min(b1) - a0.max(b0) > 0
            });
            if joined {
                set.push(*c);
            }
            !joined
        });
        if cands.len() == before {
            break;
        }
    }
    set.remove(0);
    set
}

/// Empty spots of the screen (fractions), biggest first, at most `max`:
/// each is the largest rectangle touching no zone (nor an earlier spot),
/// bounded by the screen and the zones' edges. A "+" is drawn in each;
/// clicking it fills the spot with a zone.
fn free_spaces(zones: &[FracRect], max: usize) -> Vec<FracRect> {
    const EPS: f64 = 0.002;
    const MIN: f64 = 0.06;
    let mut taken: Vec<FracRect> = zones.to_vec();
    let mut out = Vec::new();
    while out.len() < max {
        let mut xs = vec![0.0, 1.0];
        let mut ys = vec![0.0, 1.0];
        for z in &taken {
            xs.extend([z.left, z.right]);
            ys.extend([z.top, z.bottom]);
        }
        let norm = |v: &mut Vec<f64>| {
            v.retain(|x| (0.0..=1.0).contains(x));
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v.dedup_by(|a, b| (*a - *b).abs() < EPS);
        };
        norm(&mut xs);
        norm(&mut ys);
        let mut best: Option<(f64, FracRect)> = None;
        for (i, &l) in xs.iter().enumerate() {
            for &r in &xs[i + 1..] {
                if r - l < MIN {
                    continue;
                }
                for (k, &t) in ys.iter().enumerate() {
                    for &b in &ys[k + 1..] {
                        if b - t < MIN {
                            continue;
                        }
                        let area = (r - l) * (b - t);
                        if best.as_ref().is_some_and(|(a, _)| *a >= area) {
                            continue;
                        }
                        let free = taken.iter().all(|z| {
                            r.min(z.right) - l.max(z.left) <= EPS || b.min(z.bottom) - t.max(z.top) <= EPS
                        });
                        if free {
                            best = Some((area, FracRect::new(l, t, r, b)));
                        }
                    }
                }
            }
        }
        match best {
            Some((_, f)) => {
                taken.push(f);
                out.push(f);
            }
            None => break,
        }
    }
    out
}

/// Free spots of this overlay with the client-px centre of their "+".
fn plus_spots(ov: &OvWin) -> Vec<(FracRect, i32, i32)> {
    let zones: Vec<FracRect> = ov.layout.zones.iter().map(|z| z.rect).collect();
    free_spaces(&zones, 8)
        .into_iter()
        .filter_map(|f| {
            let r = f.to_pixels(&ov.mon.work, 0);
            let min = ui::sx(PLUS_R * 2 + 16, ov.dpi);
            (r.width() >= min && r.height() >= min).then(|| {
                (
                    f,
                    (r.left + r.right) / 2 - ov.mon.rect.left,
                    (r.top + r.bottom) / 2 - ov.mon.rect.top,
                )
            })
        })
        .collect()
}

fn plus_at(ov: &OvWin, x: i32, y: i32) -> Option<FracRect> {
    let rad = ui::sx(PLUS_R, ov.dpi);
    plus_spots(ov)
        .into_iter()
        .find(|(_, cx, cy)| (x - cx).pow(2) + (y - cy).pow(2) <= rad * rad)
        .map(|(f, _, _)| f)
}

/// Side of a zone a split "+" sits on; the new zone takes that half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// Small "+" on each side of a zone (inside, centred, clear of the edge
/// dots and the centre move dot), in client px. A side is left out when
/// the zone is too small to split that way.
fn split_points(r: &Rect, dpi: u32) -> Vec<(Side, i32, i32)> {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    let inset = ui::sx(34, dpi);
    let min = ui::sx(170, dpi).max(MIN_ZONE_PX * 2);
    let mut v = Vec::new();
    if r.width() >= min {
        v.push((Side::Left, r.left + inset, cy));
        v.push((Side::Right, r.right - inset, cy));
    }
    if r.height() >= min {
        v.push((Side::Top, cx, r.top + inset));
        v.push((Side::Bottom, cx, r.bottom - inset));
    }
    v
}

fn split_at(ov: &OvWin, x: i32, y: i32) -> Option<(usize, Side)> {
    let zi = ov.zone_hover?;
    let z = ov.layout.zones.get(zi)?;
    let r = zone_client_rect(z, &ov.mon.work, &ov.mon.rect);
    let rad = ui::sx(SPLIT_R + 3, ov.dpi);
    split_points(&r, ov.dpi)
        .into_iter()
        .find(|(_, px, py)| (x - px).pow(2) + (y - py).pow(2) <= rad * rad)
        .map(|(side, _, _)| (zi, side))
}

/// Split zone `zi` in two halves; the new zone takes the `side` half and
/// is inserted next to it so the numbers still read left to right / top
/// to bottom.
fn split_zone(layout: &mut Layout, zi: usize, side: Side) {
    let orig = layout.zones[zi].rect;
    let mx = (orig.left + orig.right) / 2.0;
    let my = (orig.top + orig.bottom) / 2.0;
    let mut new = orig;
    let z = &mut layout.zones[zi].rect;
    match side {
        Side::Right => {
            z.right = mx;
            new.left = mx;
        }
        Side::Left => {
            z.left = mx;
            new.right = mx;
        }
        Side::Bottom => {
            z.bottom = my;
            new.top = my;
        }
        Side::Top => {
            z.top = my;
            new.bottom = my;
        }
    }
    let at = if matches!(side, Side::Right | Side::Bottom) { zi + 1 } else { zi };
    layout.zones.insert(
        at,
        Zone {
            name: String::new(),
            rect: new,
        },
    );
    renumber(layout);
}

/// Default zone names follow their position after an insert/delete.
fn renumber(layout: &mut Layout) {
    for (i, z) in layout.zones.iter_mut().enumerate() {
        if z.name.is_empty() || z.name.starts_with("zone ") {
            z.name = format!("zone {}", i + 1);
        }
    }
}

/// Icon tiles of the apps that do not fit zone `zi` (client px), left
/// to right under the badge, as many as fit the zone width.
fn tight_icons(ov: &OvWin, zi: usize) -> Vec<(Rect, crate::minsize::Tight)> {
    let Some(z) = ov.layout.zones.get(zi) else { return Vec::new() };
    let r = zone_client_rect(z, &ov.mon.work, &ov.mon.rect);
    let tight = crate::minsize::too_small(r.width(), r.height(), ov.dpi);
    let size = ui::sx(40, ov.dpi);
    let gap = ui::sx(8, ov.dpi);
    let top = r.top + ui::sx(40, ov.dpi);
    if top + size > r.bottom - ui::sx(8, ov.dpi) {
        return Vec::new();
    }
    let mut x = r.left + ui::sx(12, ov.dpi);
    let mut out = Vec::new();
    for t in tight {
        if x + size > r.right - ui::sx(8, ov.dpi) {
            break;
        }
        out.push((Rect::new(x, top, x + size, top + size), t));
        x += size + gap;
    }
    out
}

fn icon_at(ov: &OvWin, x: i32, y: i32) -> Option<(usize, usize)> {
    (0..ov.layout.zones.len()).rev().find_map(|zi| {
        tight_icons(ov, zi)
            .iter()
            .position(|(r, _)| r.contains_point(x, y))
            .map(|k| (zi, k))
    })
}

/// First "Layout N" not used by a user preset.
fn suggest_name(taken: &[String]) -> String {
    (1..)
        .map(|n| format!("Layout {n}"))
        .find(|n| !taken.iter().any(|t| t.eq_ignore_ascii_case(n)))
        .unwrap()
}

/// Cursor for a handle, used on hover so users discover the dots.
fn handle_cursor(h: Handle) -> PCWSTR {
    match h {
        Handle::Move => IDC_SIZEALL,
        Handle::N | Handle::S => IDC_SIZENS,
        Handle::E | Handle::W => IDC_SIZEWE,
        Handle::NW | Handle::SE => IDC_SIZENWSE,
        Handle::NE | Handle::SW => IDC_SIZENESW,
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let (mw, mh) = WINDOWS
                .with(|w| {
                    w.borrow()
                        .get(&(hwnd.0 as isize))
                        .map(|ov| (ov.mon.rect.width(), ov.mon.rect.height()))
                })
                .unwrap_or((0, 0));
            ui::buffered_paint(hwnd, mw, mh, |hdc| paint(hwnd, hdc));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR => {
            // keep our contextual cursor (SetCursor in mousemove); the
            // class cursor is the crosshair for empty space
            let hit = WINDOWS.with(|w| {
                w.borrow().get(&(hwnd.0 as isize)).and_then(|ov| {
                    if naming_active() {
                        Some(IDC_ARROW)
                    } else if ov.plus_hover || ov.grid_hover || ov.split_hover.is_some() {
                        Some(IDC_HAND)
                    } else {
                        ov.hover.map(|(_, h)| handle_cursor(h))
                    }
                })
            });
            if let Some(c) = hit {
                unsafe {
                    let _ = SetCursor(Some(LoadCursorW(None, c).unwrap_or_default()));
                }
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            if naming_active() {
                return LRESULT(0);
            }
            // MK_CONTROL: resize this zone only, neighbours stay
            let alone = wparam.0 & 0x0008 != 0;
            let grid_click = WINDOWS.with(|w| {
                w.borrow()
                    .get(&(hwnd.0 as isize))
                    .is_some_and(|ov| on_grid_btn(ov, x, y))
            });
            if grid_click {
                cycle_grid();
                return LRESULT(0);
            }
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    ov.notice = None;
                    if let Some((zi, side)) = split_at(ov, x, y) {
                        split_zone(&mut ov.layout, zi, side);
                        ov.split_hover = None;
                        ov.zone_hover = None;
                        ov.hover = None;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if let Some(f) = plus_at(ov, x, y) {
                        ov.layout.zones.push(Zone {
                            name: format!("zone {}", ov.layout.zones.len() + 1),
                            rect: f,
                        });
                        ov.plus_hover = false;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if let Some((zi, h)) = handle_at(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y) {
                        let rects: Vec<Rect> = ov
                            .layout
                            .zones
                            .iter()
                            .map(|z| zone_client_rect(z, &ov.mon.work, &ov.mon.rect))
                            .collect();
                        let r = rects[zi];
                        let link = |vertical: bool, far: bool| -> Vec<(usize, bool, Rect)> {
                            if alone {
                                return Vec::new();
                            }
                            linked_edges(&rects, zi, vertical, far, LINK_PX)
                                .into_iter()
                                .map(|(j, f)| (j, f, rects[j]))
                                .collect()
                        };
                        let links_v = match h {
                            Handle::W | Handle::NW | Handle::SW => link(true, false),
                            Handle::E | Handle::NE | Handle::SE => link(true, true),
                            _ => Vec::new(),
                        };
                        let links_h = match h {
                            Handle::N | Handle::NW | Handle::NE => link(false, false),
                            Handle::S | Handle::SW | Handle::SE => link(false, true),
                            _ => Vec::new(),
                        };
                        ov.edit = Some(EditDrag {
                            zone: zi,
                            handle: h,
                            start: POINT { x, y },
                            orig: r,
                            links_v,
                            links_h,
                        });
                        let _ = SetCapture(hwnd);
                    } else if point_in_zone(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y).is_none() {
                        // start drag only on empty space (not inside a zone)
                        ov.drag_start = Some(POINT { x, y });
                        ov.drag_cur = Some(POINT { x, y });
                        let _ = SetCapture(hwnd);
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            let mut dirty = false;
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    if let Some(ed) = ov.edit.clone() {
                        let (r, guides) = edited_rect(&ed, x, y, ov);
                        let scr = Rect::new(
                            r.left + ov.mon.rect.left,
                            r.top + ov.mon.rect.top,
                            r.right + ov.mon.rect.left,
                            r.bottom + ov.mon.rect.top,
                        );
                        let fr = FracRect::from_pixels(&scr, &ov.mon.work);
                        ov.layout.zones[ed.zone].rect = fr;
                        // shared edges follow: same fraction, other edges untouched
                        let left_side = matches!(ed.handle, Handle::W | Handle::NW | Handle::SW);
                        let top_side = matches!(ed.handle, Handle::N | Handle::NW | Handle::NE);
                        let vx = if left_side { fr.left } else { fr.right };
                        let vy = if top_side { fr.top } else { fr.bottom };
                        for (j, far, _) in &ed.links_v {
                            let z = &mut ov.layout.zones[*j].rect;
                            if *far { z.right = vx } else { z.left = vx }
                        }
                        for (j, far, _) in &ed.links_h {
                            let z = &mut ov.layout.zones[*j].rect;
                            if *far { z.bottom = vy } else { z.top = vy }
                        }
                        ov.guides = guides;
                        dirty = true;
                    } else if ov.drag_start.is_some() {
                        ov.drag_cur = Some(POINT { x, y });
                        dirty = true;
                    } else if !naming_active() {
                        let zh = point_in_zone(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y)
                            .or_else(|| ov.hover.map(|(zi, _)| zi));
                        if zh != ov.zone_hover {
                            ov.zone_hover = zh;
                            dirty = true;
                        }
                        let ic = icon_at(ov, x, y);
                        if ic != ov.icon_hover {
                            ov.icon_hover = ic;
                            dirty = true;
                        }
                        let gb = ic.is_some() || on_grid_btn(ov, x, y);
                        let gb_real = ic.is_none() && gb;
                        let split = if gb { None } else { split_at(ov, x, y) };
                        let plus = !gb && split.is_none() && plus_at(ov, x, y).is_some();
                        let h = if gb || plus || split.is_some() {
                            None
                        } else {
                            handle_at(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y)
                        };
                        if h != ov.hover
                            || plus != ov.plus_hover
                            || split != ov.split_hover
                            || gb_real != ov.grid_hover
                        {
                            ov.hover = h;
                            ov.plus_hover = plus;
                            ov.split_hover = split;
                            ov.grid_hover = gb_real;
                            dirty = true;
                        }
                    }
                }
            });
            if dirty {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    if ov.edit.take().is_some() {
                        ov.guides.clear();
                        ov.zone_hover = None;
                        let _ = ReleaseCapture();
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if let Some(a) = ov.drag_start.take() {
                        ov.drag_cur = None;
                        let _ = ReleaseCapture();
                        let tol = ui::sx(SNAP_PX, ov.dpi);
                        let xt = axis_targets(ov, false, &[]);
                        let yt = axis_targets(ov, true, &[]);
                        let (sx, _) = snap_to(x, &xt, tol);
                        let (sy, _) = snap_to(y, &yt, tol);
                        let mut r = win_rect_from_drag(a, POINT { x: sx, y: sy });
                        r = Rect::new(
                            snap_to(r.left, &xt, tol).0,
                            snap_to(r.top, &yt, tol).0,
                            snap_to(r.right, &xt, tol).0,
                            snap_to(r.bottom, &yt, tol).0,
                        );
                        if r.width() >= MIN_ZONE_PX && r.height() >= MIN_ZONE_PX {
                            let frac = FracRect::from_pixels(&r, &ov.mon.work);
                            if frac.right > frac.left && frac.bottom > frac.top {
                                ov.layout.zones.push(Zone {
                                    name: format!("zone {}", ov.layout.zones.len() + 1),
                                    rect: frac,
                                });
                            }
                        }
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            if naming_active() {
                return LRESULT(0);
            }
            WINDOWS.with(|w| {
                if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                    if let Some(i) = point_in_zone(&ov.layout, &ov.mon.work, &ov.mon.rect, x, y) {
                        ov.layout.zones.remove(i);
                        renumber(&mut ov.layout);
                        ov.hover = None;
                        ov.zone_hover = None;
                        ov.split_hover = None;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            });
            LRESULT(0)
        }
        WM_CHAR if naming_active() => {
            let c = wparam.0 as u32;
            NAMING.with(|n| {
                if let Some(nm) = n.borrow_mut().as_mut() {
                    match c {
                        0x08 => {
                            // Backspace (the untouched suggestion goes whole)
                            if nm.fresh {
                                nm.text.clear();
                            } else {
                                nm.text.pop();
                            }
                        }
                        0x7F => nm.text.clear(), // Ctrl+Backspace
                        c if c >= 0x20 => {
                            if let Some(ch) = char::from_u32(c) {
                                if nm.fresh {
                                    nm.text.clear();
                                }
                                if nm.text.chars().count() < 40 {
                                    nm.text.push(ch);
                                }
                            }
                        }
                        _ => return,
                    }
                    nm.fresh = false;
                    nm.error = None;
                    let _ = InvalidateRect(Some(HWND(nm.hwnd as *mut _)), None, false);
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN if naming_active() => {
            match wparam.0 as u32 {
                0x0D => finish_naming(),
                0x1B => {
                    // back to drawing
                    if let Some(nm) = NAMING.with(|n| n.borrow_mut().take()) {
                        let _ = InvalidateRect(Some(HWND(nm.hwnd as *mut _)), None, false);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u32;
            match vk {
                0x0D => {
                    // Enter: a new preset is named first, else save + close
                    if pending_unnamed().is_some() {
                        start_naming();
                    } else {
                        close_all(true);
                    }
                }
                0x1B => close_all(false),
                // '1'..'7' -> preset on this monitor
                k @ (0x31..=0x37) => {
                    let idx = (k - 0x31) as usize;
                    WINDOWS.with(|w| {
                        if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                            if let Some(p) = crate::presets::presets().get(idx) {
                                ov.layout = p.clone();
                                ov.hover = None;
                                ov.zone_hover = None;
                                ov.split_hover = None;
                                let _ = InvalidateRect(Some(hwnd), None, false);
                            }
                        }
                    });
                }
                0x47 => cycle_grid(), // G
                0x2E => {
                    WINDOWS.with(|w| {
                        if let Some(ov) = w.borrow_mut().get_mut(&(hwnd.0 as isize)) {
                            ov.layout.zones.pop();
                            ov.hover = None;
                            ov.zone_hover = None;
                            ov.split_hover = None;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOWS.with(|w| {
                w.borrow_mut().remove(&(hwnd.0 as isize));
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint(hwnd: HWND, hdc: HDC) {
    WINDOWS.with(|w| {
        let map = w.borrow();
        let Some(ov) = map.get(&(hwnd.0 as isize)) else {
            return;
        };
        let mon = ov.mon.rect;
        let dpi = ov.dpi;
        let rc = RECT {
            left: 0,
            top: 0,
            right: mon.width(),
            bottom: mon.height(),
        };
        ui::fill(hdc, &rc, ui::BG);

        SetBkMode(hdc, TRANSPARENT);
        let font = ui::segoe(11, 600, dpi);
        let mut g = ui::GdiGuard::new(hdc);
        g.font(font);
        SetTextColor(hdc, ui::cr(ui::TXT));

        // zone boxes first, so the grid reads on top of them
        for (i, z) in ov.layout.zones.iter().enumerate() {
            let r = zone_client_rect(z, &ov.mon.work, &mon);
            let zr = RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            };
            let active = ov.hover.map(|(zi, _)| zi) == Some(i)
                || ov.edit.as_ref().map(|e| e.zone) == Some(i);
            ui::round_box(
                hdc,
                &zr,
                ui::sx(10, dpi),
                ui::blend(ui::ACCENT, ui::BG, if active { 46 } else { 30 }),
                Some((ui::blend(ui::ACCENT, ui::BG, if active { 200 } else { 110 }), 1)),
            );
        }

        let cells = grid_cells();
        if cells == 0 {
            // faint fraction grid so the magnet targets read on screen
            let grid = CreatePen(PS_SOLID, 1, ui::cr(ui::blend(ui::EDGE, ui::BG, 160)));
            let old_p = SelectObject(hdc, grid.into());
            for d in [2i64, 3, 4] {
                for n in 1..d {
                    let gx = (mon.width() as i64 * n / d) as i32;
                    let gy = (mon.height() as i64 * n / d) as i32;
                    let _ = MoveToEx(hdc, gx, 0, None);
                    let _ = LineTo(hdc, gx, mon.height());
                    let _ = MoveToEx(hdc, 0, gy, None);
                    let _ = LineTo(hdc, mon.width(), gy);
                }
            }
            SelectObject(hdc, old_p);
            let _ = DeleteObject(grid.into());
        } else {
            // drawing grid on the work area: every line is a magnet; the
            // halves read a little stronger
            let (wl, wt) = (ov.mon.work.left - mon.left, ov.mon.work.top - mon.top);
            let (wr, wb) = (ov.mon.work.right - mon.left, ov.mon.work.bottom - mon.top);
            let thin = CreatePen(PS_SOLID, 1, ui::cr(ui::blend(ui::ACCENT, ui::BG, 95)));
            let half = CreatePen(PS_SOLID, 2, ui::cr(ui::blend(ui::ACCENT, ui::BG, 150)));
            let old_p = SelectObject(hdc, thin.into());
            let mid = cells % 2 == 0;
            for (k, gx) in grid_lines(wl, wr, cells).into_iter().enumerate() {
                SelectObject(hdc, if mid && k + 1 == cells as usize / 2 { half } else { thin }.into());
                let _ = MoveToEx(hdc, gx, wt, None);
                let _ = LineTo(hdc, gx, wb);
            }
            for (k, gy) in grid_lines(wt, wb, cells).into_iter().enumerate() {
                SelectObject(hdc, if mid && k + 1 == cells as usize / 2 { half } else { thin }.into());
                let _ = MoveToEx(hdc, wl, gy, None);
                let _ = LineTo(hdc, wr, gy);
            }
            SelectObject(hdc, old_p);
            let _ = DeleteObject(thin.into());
            let _ = DeleteObject(half.into());
        }

        // zones
        for (i, z) in ov.layout.zones.iter().enumerate() {
            let r = zone_client_rect(z, &ov.mon.work, &mon);
            let zr = RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            };

            // badge + name, top-left inside the zone
            let bx = r.left + ui::sx(16, dpi);
            let by = r.top + ui::sx(16, dpi);
            let rad = ui::sx(11, dpi);
            ui::dot(hdc, bx, by, rad, ui::blend(ui::ACCENT, ui::BG, 230));
            SetTextColor(hdc, ui::cr(ui::BG));
            let mut br = RECT {
                left: bx - rad,
                top: by - rad,
                right: bx + rad,
                bottom: by + rad,
            };
            ui::text(hdc, &mut br, &format!("{}", i + 1), DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SetTextColor(hdc, ui::cr(ui::MUTED));
            let mut nr = RECT {
                left: bx + rad + ui::sx(8, dpi),
                top: by - ui::sx(9, dpi),
                right: r.right - ui::sx(8, dpi),
                bottom: by + ui::sx(9, dpi),
            };
            // the badge already says the number: show the size (and a
            // custom name), never "zone N" a second time
            let size = format!("{} \u{00d7} {} px", r.width(), r.height());
            let label = if z.name.is_empty() || z.name == format!("zone {}", i + 1) {
                size
            } else {
                format!("{}   \u{00b7}   {size}", z.name)
            };
            ui::text(hdc, &mut nr, &label, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);

            // too small for apps whose minimum size we know: amber dashes
            // + each app's icon crossed out in red (hover = why)
            let icons = tight_icons(ov, i);
            if !icons.is_empty() {
                let inset = ui::sx(3, dpi);
                let dr = RECT {
                    left: zr.left + inset,
                    top: zr.top + inset,
                    right: zr.right - inset,
                    bottom: zr.bottom - inset,
                };
                ui::dashed_box(hdc, &dr, ui::sx(8, dpi), ui::WARN, ui::sx(2, dpi).max(2));
                for (k, (ir, t)) in icons.iter().enumerate() {
                    let hot = ov.icon_hover == Some((i, k));
                    let tile = RECT { left: ir.left, top: ir.top, right: ir.right, bottom: ir.bottom };
                    ui::round_box(
                        hdc,
                        &tile,
                        ui::sx(8, dpi),
                        if hot { ui::blend(ui::WARN, ui::CARD, 40) } else { ui::CARD },
                        Some((if hot { ui::WARN } else { ui::blend(ui::WARN, ui::BG, 140) }, 1)),
                    );
                    let pad = ui::sx(5, dpi);
                    let isz = ir.width() - pad * 2;
                    match crate::appicon::get(&t.exe, &t.path, isz) {
                        Some(hi) => {
                            let _ = DrawIconEx(hdc, ir.left + pad, ir.top + pad, hi, isz, isz, 0, None, DI_NORMAL);
                        }
                        None => {
                            // no icon found: the app's initial
                            let f = ui::segoe(13, 700, dpi);
                            let old = SelectObject(hdc, f.into());
                            SetTextColor(hdc, ui::cr(ui::TXT));
                            let mut lr = tile;
                            let initial: String = t.label.chars().take(1).collect();
                            ui::text(hdc, &mut lr, &initial, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                            SelectObject(hdc, old);
                            let _ = DeleteObject(f.into());
                        }
                    }
                    let (cx, cy) = ((ir.left + ir.right) / 2, (ir.top + ir.bottom) / 2);
                    ui::x_mark(hdc, cx, cy, ir.width() * 3 / 10, ui::sx(3, dpi) as f32, ui::DANGER, ui::BG);
                }
            }
            SetTextColor(hdc, ui::cr(ui::TXT));

            // handle dots
            let hr2 = Rect::new(zr.left, zr.top, zr.right, zr.bottom);
            for (h, hx, hy) in handle_points(&hr2) {
                let hovered = ov.hover == Some((i, h));
                let rad = ui::sx(if h == Handle::Move { DOT_R + 1 } else { DOT_R }, dpi);
                let (fill_c, ring_c) = if hovered {
                    (ui::ACCENT, ui::ACCENT)
                } else if h == Handle::Move {
                    (ui::blend(ui::ACCENT, ui::BG, 140), ui::ACCENT)
                } else {
                    (ui::CARD, ui::blend(ui::ACCENT, ui::BG, 200))
                };
                ui::ring_dot(hdc, hx, hy, rad, ring_c, fill_c);
            }

            // side "+" (split in two) on the zone under the mouse
            if ov.zone_hover == Some(i) && ov.edit.is_none() && ov.drag_start.is_none() {
                for (side, px, py) in split_points(&hr2, dpi) {
                    let hot = ov.split_hover == Some((i, side));
                    draw_plus(hdc, px, py, ui::sx(SPLIT_R, dpi), hot, dpi);
                }
            }
        }

        // big "+" in every empty spot: click = a zone filling it
        if ov.edit.is_none() && ov.drag_start.is_none() && NAMING.with(|n| n.borrow().is_none()) {
            for (f, cx, cy) in plus_spots(ov) {
                let hot = ov.plus_hover && {
                    let rad = ui::sx(PLUS_R, dpi);
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    let _ = ScreenToClient(hwnd, &mut p);
                    (p.x - cx).pow(2) + (p.y - cy).pow(2) <= rad * rad
                };
                if hot {
                    let r = zone_client_rect(&Zone { name: String::new(), rect: f }, &ov.mon.work, &mon);
                    let inset = ui::sx(4, dpi);
                    let pr = RECT {
                        left: r.left + inset,
                        top: r.top + inset,
                        right: r.right - inset,
                        bottom: r.bottom - inset,
                    };
                    ui::round_box(
                        hdc,
                        &pr,
                        ui::sx(10, dpi),
                        ui::blend(ui::ACCENT, ui::BG, 16),
                        Some((ui::blend(ui::ACCENT, ui::BG, 150), 1)),
                    );
                }
                draw_plus(hdc, cx, cy, ui::sx(PLUS_R, dpi), hot, dpi);
            }
        }

        // magnetic guides while an edge is snapped
        for (vert, pos) in &ov.guides {
            let gp = CreatePen(PS_SOLID, ui::sx(2, dpi), ui::cr(ui::ACCENT));
            let old = SelectObject(hdc, gp.into());
            if *vert {
                let _ = MoveToEx(hdc, *pos, 0, None);
                let _ = LineTo(hdc, *pos, mon.height());
            } else {
                let _ = MoveToEx(hdc, 0, *pos, None);
                let _ = LineTo(hdc, mon.width(), *pos);
            }
            SelectObject(hdc, old);
            let _ = DeleteObject(gp.into());
        }

        // rubber band for a new zone
        if let (Some(a), Some(b)) = (ov.drag_start, ov.drag_cur) {
            let tol = ui::sx(SNAP_PX, dpi);
            let xt = axis_targets(ov, false, &[]);
            let yt = axis_targets(ov, true, &[]);
            let sb = POINT {
                x: snap_to(b.x, &xt, tol).0,
                y: snap_to(b.y, &yt, tol).0,
            };
            let r = win_rect_from_drag(a, sb);
            let mut br = RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            };
            let bp = CreatePen(PS_DOT, 1, ui::cr(ui::ACCENT));
            let old_p2 = SelectObject(hdc, bp.into());
            let old_b2 = SelectObject(hdc, GetStockObject(HOLLOW_BRUSH));
            let _ = Rectangle(hdc, br.left, br.top, br.right, br.bottom);
            SelectObject(hdc, old_b2);
            SelectObject(hdc, old_p2);
            let _ = DeleteObject(bp.into());
            let pct = FracRect::from_pixels(&r, &mon);
            let info = format!(
                "{:.0}% x {:.0}%",
                (pct.right - pct.left) * 100.0,
                (pct.bottom - pct.top) * 100.0
            );
            SetTextColor(hdc, ui::cr(ui::ACCENT));
            ui::text(hdc, &mut br, &info, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SetTextColor(hdc, ui::cr(ui::TXT));
        }

        // instruction pill, bottom center
        let base = "drag or +: new zone   side +: split   dots / edges: move, resize (Ctrl: this zone only)   right-click: delete   G: grid   1-7: preset   Enter: save   Esc: cancel";
        let hint_owned = if let Some(msg) = &ov.notice {
            msg.clone()
        } else if pending_unnamed().as_deref() == Some(ov.mon.name.as_str()) {
            format!("New layout   \u{00b7}   {base}")
        } else {
            match pending_for(&ov.mon.name) {
                Some(n) => format!("Preset \u{201c}{n}\u{201d}   \u{00b7}   {base}"),
                None => base.to_string(),
            }
        };
        let hint = hint_owned.as_str();
        let hint_font = ui::segoe(10, 400, dpi);
        SelectObject(hdc, hint_font.into());
        let tw = ui::text_w(hdc, hint);
        let th = ui::sx(30, dpi);
        let hx = (mon.width() - tw) / 2;
        let hy = mon.height() - th - ui::sx(22, dpi);
        let mut hr = RECT {
            left: hx - ui::sx(16, dpi),
            top: hy,
            right: hx + tw + ui::sx(16, dpi),
            bottom: hy + th,
        };
        let notice = ov.notice.is_some();
        ui::round_box(
            hdc,
            &hr,
            ui::sx(15, dpi),
            ui::CARD,
            Some((if notice { ui::ACCENT } else { ui::EDGE }, 1)),
        );
        SetTextColor(hdc, ui::cr(if notice { ui::TXT } else { ui::MUTED }));
        ui::text(hdc, &mut hr, hint, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        let _ = DeleteObject(hint_font.into());

        // why an app icon is crossed out (hover)
        if let Some((zi, k)) = ov.icon_hover {
            if let Some((ir, t)) = tight_icons(ov, zi).into_iter().nth(k) {
                let zr = zone_client_rect(&ov.layout.zones[zi], &ov.mon.work, &mon);
                paint_icon_tip(hdc, &ir, &t, zr.width(), zr.height(), mon.width(), mon.height(), dpi);
            }
        }

        // Grid toggle, top centre
        {
            let br = grid_btn_rect(ov);
            let rc = RECT { left: br.left, top: br.top, right: br.right, bottom: br.bottom };
            let on = cells > 0;
            let fill_c = if ov.grid_hover { ui::blend(ui::ACCENT, ui::CARD, 40) } else { ui::CARD };
            let edge = if on { ui::ACCENT } else { ui::EDGE };
            ui::round_box(hdc, &rc, ui::sx(15, dpi), fill_c, Some((edge, 1)));
            let label = if on { format!("Grid  {cells} \u{00d7} {cells}") } else { "Grid  off".to_string() };
            let f = ui::segoe(10, 600, dpi);
            let old = SelectObject(hdc, f.into());
            SetTextColor(hdc, ui::cr(if on { ui::TXT } else { ui::MUTED }));
            let mut lr = rc;
            lr.right -= ui::sx(30, dpi);
            ui::text(hdc, &mut lr, &label, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            // key cap "G" on the right
            let kc = RECT {
                left: rc.right - ui::sx(34, dpi),
                top: rc.top + ui::sx(6, dpi),
                right: rc.right - ui::sx(12, dpi),
                bottom: rc.bottom - ui::sx(6, dpi),
            };
            ui::round_box(hdc, &kc, ui::sx(4, dpi), ui::FIELD, Some((ui::EDGE, 1)));
            let mut kr = kc;
            SetTextColor(hdc, ui::cr(ui::MUTED));
            ui::text(hdc, &mut kr, "G", DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SelectObject(hdc, old);
            let _ = DeleteObject(f.into());
        }

        // name card for a new preset
        NAMING.with(|n| {
            if let Some(nm) = n.borrow().as_ref().filter(|nm| nm.hwnd == hwnd.0 as isize) {
                paint_naming(hdc, nm, mon.width(), mon.height(), dpi);
            }
        });
    });
}

/// Tooltip under a crossed-out app icon: what the app needs, what the
/// zone gives, and what will happen.
#[allow(clippy::too_many_arguments)]
unsafe fn paint_icon_tip(hdc: HDC, ir: &Rect, t: &crate::minsize::Tight, zw: i32, zh: i32, mw: i32, mh: i32, dpi: u32) {
    let need = match (t.w as i32 > zw, t.h as i32 > zh) {
        (true, true) => format!("{} needs at least {} \u{00d7} {} px.", t.label, t.w, t.h),
        (true, false) => format!("{} needs at least {} px of width.", t.label, t.w),
        _ => format!("{} needs at least {} px of height.", t.label, t.h),
    };
    let zone = format!("This zone is {zw} \u{00d7} {zh} px.");
    let what = if FORCE_ON.with(|f| f.get()) {
        format!("Force small zones is on: {} will be squeezed into it anyway and may cut content or draw wrong.", t.label)
    } else {
        format!(
            "{} will stay bigger and spill over the next zone. Make the zone bigger, or turn on Force small zones in the settings (risky).",
            t.label
        )
    };
    let w = ui::sx(320, dpi);
    let pad = ui::sx(14, dpi);
    let title_f = ui::segoe(11, 600, dpi);
    let body_f = ui::segoe(10, 400, dpi);
    let old = SelectObject(hdc, title_f.into());
    let mut m1 = RECT { left: 0, top: 0, right: w - pad * 2, bottom: 0 };
    DrawTextW(hdc, &mut need.encode_utf16().collect::<Vec<_>>(), &mut m1, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
    SelectObject(hdc, body_f.into());
    let body = format!("{zone}\n{what}");
    let mut m2 = RECT { left: 0, top: 0, right: w - pad * 2, bottom: 0 };
    DrawTextW(hdc, &mut body.encode_utf16().collect::<Vec<_>>(), &mut m2, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
    let gap = ui::sx(6, dpi);
    let h = pad * 2 + m1.bottom + gap + m2.bottom;
    let mut left = ir.left;
    let mut top = ir.bottom + ui::sx(8, dpi);
    if left + w > mw - ui::sx(8, dpi) {
        left = mw - ui::sx(8, dpi) - w;
    }
    if top + h > mh - ui::sx(8, dpi) {
        top = ir.top - ui::sx(8, dpi) - h;
    }
    let card = RECT { left, top, right: left + w, bottom: top + h };
    ui::round_box(hdc, &card, ui::sx(10, dpi), ui::CARD, Some((ui::WARN, 1)));
    SelectObject(hdc, title_f.into());
    SetTextColor(hdc, ui::cr(ui::TXT));
    let mut r1 = RECT { left: left + pad, top: top + pad, right: left + w - pad, bottom: top + pad + m1.bottom };
    ui::text(hdc, &mut r1, &need, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
    SelectObject(hdc, body_f.into());
    SetTextColor(hdc, ui::cr(ui::MUTED));
    let mut r2 = RECT { left: left + pad, top: r1.bottom + gap, right: left + w - pad, bottom: top + h - pad };
    ui::text(hdc, &mut r2, &body, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
    SelectObject(hdc, old);
    let _ = DeleteObject(title_f.into());
    let _ = DeleteObject(body_f.into());
}

/// Round "+" button: `hot` = under the mouse.
unsafe fn draw_plus(hdc: HDC, cx: i32, cy: i32, rad: i32, hot: bool, dpi: u32) {
    let (fill_c, ring_c, mark) = if hot {
        (ui::ACCENT, ui::ACCENT, ui::BG)
    } else {
        (ui::CARD, ui::blend(ui::ACCENT, ui::BG, 200), ui::ACCENT)
    };
    ui::ring_dot(hdc, cx, cy, rad, ring_c, fill_c);
    let w = (ui::sx(2, dpi) as f32).max(2.0) * if rad > ui::sx(16, dpi) { 1.25 } else { 1.0 };
    ui::plus_mark(hdc, cx, cy, rad * 9 / 20, w, mark);
}

/// Centred card: title, name field with caret, error, key hints.
unsafe fn paint_naming(hdc: HDC, nm: &Naming, w: i32, h: i32, dpi: u32) {
    let cw = ui::sx(440, dpi);
    let ch = ui::sx(196, dpi);
    let card = RECT {
        left: (w - cw) / 2,
        top: (h - ch) / 2,
        right: (w + cw) / 2,
        bottom: (h + ch) / 2,
    };
    ui::round_box(hdc, &card, ui::sx(16, dpi), ui::CARD, Some((ui::blend(ui::ACCENT, ui::BG, 160), 1)));
    let pad = ui::sx(24, dpi);

    let title_f = ui::segoe(14, 600, dpi);
    let old_f = SelectObject(hdc, title_f.into());
    SetTextColor(hdc, ui::cr(ui::TXT));
    let mut tr = RECT {
        left: card.left + pad,
        top: card.top + pad,
        right: card.right - pad,
        bottom: card.top + pad + ui::sx(26, dpi),
    };
    ui::text(hdc, &mut tr, "Name this layout", DT_LEFT | DT_VCENTER | DT_SINGLELINE);

    let body_f = ui::segoe(11, 400, dpi);
    SelectObject(hdc, body_f.into());
    let field = RECT {
        left: card.left + pad,
        top: tr.bottom + ui::sx(14, dpi),
        right: card.right - pad,
        bottom: tr.bottom + ui::sx(14 + 40, dpi),
    };
    let edge = if nm.error.is_some() { ui::DANGER } else { ui::ACCENT };
    ui::round_box(hdc, &field, ui::sx(8, dpi), ui::FIELD, Some((edge, 1)));
    let ip = ui::sx(12, dpi);
    let tw = ui::text_w(hdc, &nm.text);
    if nm.fresh && !nm.text.is_empty() {
        // untouched suggestion reads as selected: typing replaces it
        let sel = RECT {
            left: field.left + ip - ui::sx(2, dpi),
            top: field.top + ui::sx(9, dpi),
            right: field.left + ip + tw + ui::sx(2, dpi),
            bottom: field.bottom - ui::sx(9, dpi),
        };
        ui::fill(hdc, &sel, ui::blend(ui::ACCENT, ui::FIELD, 90));
    }
    SetTextColor(hdc, ui::cr(ui::TXT));
    let mut fr = RECT {
        left: field.left + ip,
        top: field.top,
        right: field.right - ip,
        bottom: field.bottom,
    };
    ui::text(hdc, &mut fr, &nm.text, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    // caret
    let cx = (field.left + ip + tw + ui::sx(1, dpi)).min(field.right - ip);
    let caret = RECT {
        left: cx,
        top: field.top + ui::sx(10, dpi),
        right: cx + ui::sx(2, dpi).max(1),
        bottom: field.bottom - ui::sx(10, dpi),
    };
    ui::fill(hdc, &caret, ui::ACCENT);

    let mut er = RECT {
        left: card.left + pad,
        top: field.bottom + ui::sx(10, dpi),
        right: card.right - pad,
        bottom: field.bottom + ui::sx(30, dpi),
    };
    match &nm.error {
        Some(e) => {
            SetTextColor(hdc, ui::cr(ui::DANGER));
            ui::text(hdc, &mut er, e, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
        }
        None => {
            SetTextColor(hdc, ui::cr(ui::MUTED));
            ui::text(
                hdc,
                &mut er,
                "Saved as one of your presets and used on this screen.",
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
    }
    let mut kr = RECT {
        left: card.left + pad,
        top: card.bottom - pad - ui::sx(20, dpi),
        right: card.right - pad,
        bottom: card.bottom - pad,
    };
    SetTextColor(hdc, ui::cr(ui::MUTED));
    ui::text(hdc, &mut kr, "Enter: save     Esc: back to drawing", DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc, old_f);
    let _ = DeleteObject(title_f.into());
    let _ = DeleteObject(body_f.into());
}

fn commit(hwnd: HWND) {
    WINDOWS.with(|w| {
        SHARED.with(|s| {
            if let Some(ov) = w.borrow().get(&(hwnd.0 as isize)) {
                if let Some(shared) = s.borrow().as_ref() {
                    let mut cfg = shared.config.write().unwrap();
                    let preset = pending_for(&ov.mon.name);
                    let next = crate::uimodel::commit_editor_layout(
                        &cfg,
                        &ov.mon.name,
                        ov.layout.clone(),
                        preset.as_deref(),
                    );
                    *cfg = next;
                    let _ = cfg.save();
                    shared.bump();
                }
            }
        });
    });
}

/// Enter on a new preset: open the name card on its display (or say
/// that a zone must be drawn first).
fn start_naming() {
    let Some(mon) = pending_unnamed() else { return };
    let target = WINDOWS.with(|w| {
        w.borrow()
            .iter()
            .find(|(_, ov)| ov.mon.name == mon)
            .map(|(h, ov)| (*h, ov.layout.zones.is_empty()))
    });
    let Some((h, empty)) = target else {
        // the display went away: keep the drawings, nothing to name
        close_all(true);
        return;
    };
    let hwnd = HWND(h as *mut _);
    if empty {
        WINDOWS.with(|w| {
            if let Some(ov) = w.borrow_mut().get_mut(&h) {
                ov.notice = Some("Draw at least one zone first (drag, or click a +).".into());
            }
        });
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        return;
    }
    let taken: Vec<String> = SHARED.with(|s| {
        s.borrow()
            .as_ref()
            .map(|sh| sh.config.read().unwrap().user_presets.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default()
    });
    NAMING.with(|n| {
        *n.borrow_mut() = Some(Naming {
            hwnd: h,
            text: suggest_name(&taken),
            fresh: true,
            error: None,
        })
    });
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Enter in the name card: validate, then save everything and close.
fn finish_naming() {
    let Some(text) = NAMING.with(|n| n.borrow().as_ref().map(|nm| nm.text.clone())) else {
        return;
    };
    let checked = SHARED.with(|s| {
        s.borrow()
            .as_ref()
            .map(|sh| crate::uimodel::check_new_preset_name(&sh.config.read().unwrap(), &text))
    });
    match checked {
        Some(Ok(name)) => {
            PENDING.with(|p| {
                if let Some((_, n)) = p.borrow_mut().as_mut() {
                    *n = Some(name);
                }
            });
            NAMING.with(|n| n.borrow_mut().take());
            close_all(true);
        }
        Some(Err(e)) => NAMING.with(|n| {
            if let Some(nm) = n.borrow_mut().as_mut() {
                nm.error = Some(e.message.clone());
                unsafe {
                    let _ = InvalidateRect(Some(HWND(nm.hwnd as *mut _)), None, false);
                }
            }
        }),
        None => {}
    }
}

/// Save the layouts of every open overlay then destroy them.
/// `save` == false cancels without writing.
fn close_all(save: bool) {
    NAMING.with(|n| n.borrow_mut().take());
    let hwnds: Vec<isize> = WINDOWS.with(|w| w.borrow().keys().copied().collect());
    for h in hwnds {
        if save {
            commit(HWND(h as *mut _));
        }
        unsafe {
            let _ = DestroyWindow(HWND(h as *mut _));
        }
    }
    PENDING.with(|p| p.borrow_mut().take());
    // the settings panel (if open) shows the new layouts / presets
    crate::webui::refresh();
}

pub fn is_open() -> bool {
    WINDOWS.with(|w| !w.borrow().is_empty())
}

/// Toggle the overlay on/off. When turning off, edits are committed.
pub fn toggle(shared: Arc<Shared>) {
    if is_open() {
        close_all(true);
        return;
    }
    SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));
    let mons = shared.monitors.read().unwrap().clone();
    let layouts = shared.config.read().unwrap().layouts.clone();
    // ask every open app for its minimum size before showing the zones
    crate::minsize::probe_all();
    FORCE_ON.with(|f| f.set(shared.config.read().unwrap().force_small_zones));

    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let fresh_on = pending_unnamed();
        let mut focus = None;
        for mon in mons {
            let layout = if fresh_on.as_deref() == Some(mon.name.as_str()) {
                // Draw my own: start from a blank screen
                Layout {
                    name: String::new(),
                    zones: Vec::new(),
                }
            } else {
                layouts
                    .for_monitor(&mon.name, mon.index)
                    .cloned()
                    .unwrap_or_else(Layout::halves)
            };
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
                CLASS,
                w!("ZoneLock zones"),
                WS_POPUP | WS_VISIBLE,
                mon.rect.left,
                mon.rect.top,
                mon.rect.width(),
                mon.rect.height(),
                None,
                None,
                Some(hinst.into()),
                None,
            );
            let Ok(hwnd) = hwnd else { continue };
            let _ = SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(0),
                ALPHA,
                LWA_ALPHA,
            );
            WINDOWS.with(|w| {
                w.borrow_mut().insert(
                    hwnd.0 as isize,
                    OvWin {
                        mon: mon.clone(),
                        layout,
                        drag_start: None,
                        drag_cur: None,
                        edit: None,
                        guides: Vec::new(),
                        hover: None,
                        plus_hover: false,
                        grid_hover: false,
                        icon_hover: None,
                        zone_hover: None,
                        split_hover: None,
                        notice: None,
                        dpi: mon.dpi_x.max(96),
                    },
                );
            });
            let _ = ShowWindow(hwnd, SW_SHOW);
            // keys (G, Enter, Esc...) go to the display being drawn on, else
            // the one under the mouse: opened from a hotkey, the editor
            // would otherwise leave the keyboard with the previous window
            let mut cur = POINT::default();
            let _ = GetCursorPos(&mut cur);
            let under = mon.rect.contains_point(cur.x, cur.y);
            if fresh_on.as_deref() == Some(mon.name.as_str()) || pending_for(&mon.name).is_some() {
                focus = Some(hwnd);
            } else if under && focus.is_none() {
                focus = Some(hwnd);
            }
        }
        // keys go to the display being drawn on
        if let Some(h) = focus {
            let _ = SetForegroundWindow(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(l: f64, t: f64, r: f64, b: f64) -> FracRect {
        FracRect::new(l, t, r, b)
    }

    #[test]
    fn shared_edges_follow_the_splitter() {
        // A top-left, C bottom-left, B right (full height): A's right edge,
        // B's left edge and C's right edge are one line
        let a = Rect::new(0, 0, 500, 400);
        let c = Rect::new(0, 400, 500, 800);
        let b = Rect::new(500, 0, 1000, 800);
        let rects = [a, b, c];
        let mut got = linked_edges(&rects, 0, true, true, LINK_PX);
        got.sort();
        assert_eq!(got, vec![(1, false), (2, true)]);
        // A's bottom edge only touches C's top
        assert_eq!(linked_edges(&rects, 0, false, true, LINK_PX), vec![(2, false)]);
        // the outer left edge has nothing on the other side
        assert_eq!(linked_edges(&rects, 0, true, false, LINK_PX), vec![]);
    }

    #[test]
    fn stacked_zones_alone_are_not_linked() {
        // same right edge, but nothing joins them: resize alone
        let rects = [Rect::new(0, 0, 500, 400), Rect::new(0, 400, 500, 800)];
        assert!(linked_edges(&rects, 0, true, true, LINK_PX).is_empty());
    }

    #[test]
    fn link_bounds_keep_min_size() {
        let links = [(1, false, Rect::new(500, 0, 1000, 800))];
        assert_eq!(link_bounds(&links, false), Some((i32::MIN, 1000 - MIN_ZONE_PX)));
        let links = [(2, true, Rect::new(0, 400, 500, 800))];
        assert_eq!(link_bounds(&links, false), Some((MIN_ZONE_PX, i32::MAX)));
        assert_eq!(link_bounds(&[], true), None);
    }

    #[test]
    fn free_spaces_fill_the_gaps() {
        assert_eq!(free_spaces(&[], 8), vec![f(0.0, 0.0, 1.0, 1.0)]);
        // a zone on the left 60%: the gap is exactly the right 40%
        assert_eq!(free_spaces(&[f(0.0, 0.0, 0.6, 1.0)], 8), vec![f(0.6, 0.0, 1.0, 1.0)]);
        // full screen covered: nothing
        assert!(free_spaces(&[f(0.0, 0.0, 0.5, 1.0), f(0.5, 0.0, 1.0, 1.0)], 8).is_empty());
        // two separate gaps around a centre column: one "+" each
        let got = free_spaces(&[f(0.3, 0.0, 0.7, 1.0)], 8);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&f(0.0, 0.0, 0.3, 1.0)) && got.contains(&f(0.7, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn split_gives_half_to_the_new_zone() {
        let mut l = Layout {
            name: String::new(),
            zones: vec![Zone { name: "zone 1".into(), rect: f(0.0, 0.0, 0.5, 1.0) }],
        };
        split_zone(&mut l, 0, Side::Right);
        assert_eq!(l.zones[0].rect, f(0.0, 0.0, 0.25, 1.0));
        assert_eq!(l.zones[1].rect, f(0.25, 0.0, 0.5, 1.0));
        split_zone(&mut l, 0, Side::Left);
        assert_eq!(l.zones[0].rect, f(0.0, 0.0, 0.125, 1.0));
        assert_eq!(l.zones[1].rect, f(0.125, 0.0, 0.25, 1.0));
        // bottom: the new zone is the lower half, right after the old one
        split_zone(&mut l, 2, Side::Bottom);
        assert_eq!(l.zones[2].rect, f(0.25, 0.0, 0.5, 0.5));
        assert_eq!(l.zones[3].rect, f(0.25, 0.5, 0.5, 1.0));
        split_zone(&mut l, 0, Side::Top);
        assert_eq!(l.zones[0].rect, f(0.0, 0.0, 0.125, 0.5));
        assert_eq!(l.zones[1].rect, f(0.0, 0.5, 0.125, 1.0));
        let names: Vec<_> = l.zones.iter().map(|z| z.name.as_str()).collect();
        assert_eq!(names, ["zone 1", "zone 2", "zone 3", "zone 4", "zone 5"]);
    }

    #[test]
    fn grid_lines_split_the_axis() {
        assert_eq!(grid_lines(0, 1200, 4), vec![300, 600, 900]);
        assert_eq!(grid_lines(100, 700, 6), vec![200, 300, 400, 500, 600]);
        assert!(grid_lines(0, 1000, 0).is_empty());
    }

    #[test]
    fn suggested_name_is_free() {
        assert_eq!(suggest_name(&[]), "Layout 1");
        assert_eq!(suggest_name(&["layout 1".into(), "Layout 2".into()]), "Layout 3");
    }
}
