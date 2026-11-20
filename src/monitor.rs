//! Monitor enumeration: GDI device name, pixel bounds, work area, DPI.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::rect::Rect;

#[derive(Debug, Clone)]
pub struct Monitor {
    /// GDI device name, e.g. `\\\\.\\DISPLAY1`.
    pub name: String,
    /// `rcMonitor` in virtual-screen coordinates (physical pixels when the
    /// process is per-monitor DPI aware, which we set at startup).
    pub rect: Rect,
    /// `rcWork` (monitor minus taskbars/appbars).
    pub work: Rect,
    /// Left-to-right ordering index (0 = leftmost).
    pub index: usize,
    pub dpi_x: u32,
    pub dpi_y: u32,
    pub primary: bool,
    pub handle: isize,
}

fn to_rect(r: &RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

struct EnumCtx {
    out: Vec<Monitor>,
}

unsafe extern "system" fn enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumCtx);
    let mut info: MONITORINFOEXW = std::mem::zeroed();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
        let name_u16: Vec<u16> = info
            .szDevice
            .iter()
            .take_while(|c| **c != 0)
            .copied()
            .collect();
        let name = String::from_utf16_lossy(&name_u16);
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        ctx.out.push(Monitor {
            name,
            rect: to_rect(&info.monitorInfo.rcMonitor),
            work: to_rect(&info.monitorInfo.rcWork),
            index: 0,
            dpi_x,
            dpi_y,
            primary: (info.monitorInfo.dwFlags & 1) != 0, // MONITORINFOF_PRIMARY
            handle: hmon.0 as isize,
        });
    }
    BOOL(1)
}

/// All connected monitors, sorted left-to-right then top-to-bottom, with a
/// stable `index` matching the LayoutSet `__index_N` fallback.
pub fn enumerate() -> Vec<Monitor> {
    let mut ctx = EnumCtx { out: Vec::new() };
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM(&mut ctx as *mut _ as isize),
        );
    }
    let mut v = ctx.out;
    v.sort_by_key(|m| (m.rect.left, m.rect.top));
    for (i, m) in v.iter_mut().enumerate() {
        m.index = i;
    }
    v
}

/// Bounding box of every monitor (the virtual screen in pixel coords).
/// Transient rects that overflow it wildly (some apps pass through a
/// garbage geometry while entering fullscreen) must never be recorded
/// as a "normal" window position.
pub fn union_rect(mons: &[Monitor]) -> Option<Rect> {
    let mut it = mons.iter();
    let first = it.next()?;
    Some(it.fold(first.rect, |acc, m| acc.union(&m.rect)))
}

/// Monitor containing point or, failing that, nearest by rect overlap.
pub fn monitor_at<'a>(rect: &Rect, monitors: &'a [Monitor]) -> Option<&'a Monitor> {
    monitors
        .iter()
        .map(|m| (m, m.rect.overlap_area(rect)))
        .max_by_key(|(_, a)| *a)
        .filter(|(_, a)| *a > 0)
        .map(|(m, _)| m)
        .or_else(|| {
            let (cx, cy) = rect.center();
            monitors.iter().find(|m| m.rect.contains_point(cx, cy))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_returns_at_least_one() {
        // Requires an interactive session; skipped silently on headless CI.
        let mons = enumerate();
        if mons.is_empty() {
            return;
        }
        assert!(mons.iter().any(|m| m.primary));
        assert!(mons[0].rect.width() > 0);
    }
}
