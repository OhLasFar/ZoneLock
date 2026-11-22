//! Monitor enumeration: GDI device name, pixel bounds, work area, DPI,
//! plus the hardware identity of each screen (model, built-in panel,
//! virtual display) from Windows' display configuration.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::rect::Rect;

#[derive(Debug, Clone, Default)]
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
    /// Hardware identity of the screen ("10AC-40F9", "#2" appended for a
    /// second identical model), stable across ports, docks and GDI
    /// renumbering. Empty when Windows does not tell.
    pub hw_id: String,
    /// Model name from the EDID ("DELL U2720Q"); empty for most built-in
    /// laptop panels.
    pub model: String,
    /// The laptop's own panel (internal / embedded DisplayPort).
    pub internal: bool,
    /// A virtual display (Parsec, remote desktop, streaming tools...):
    /// never part of a display setup.
    pub virt: bool,
    /// Other screens showing this same picture ("Duplicate these
    /// displays"): hardware id and label of each.
    pub clones: Vec<(String, String)>,
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
            ..Default::default()
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
    hw::fill(&mut v);
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

/// One active display path as Windows' display configuration reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// GDI source name (`\\\\.\\DISPLAY1`).
    pub gdi: String,
    /// EDID manufacturer id, as the API returns it (byte-swapped).
    pub mfg: u16,
    pub product: u16,
    pub friendly: String,
    /// DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY value.
    pub tech: i32,
    pub device_path: String,
    /// Serial number from the screen's EDID ("" when it has none): tells
    /// apart two screens of the same model.
    pub serial: String,
}

/// Registry key (under HKLM) holding the EDID of the screen at
/// `device_path` (`\\?\DISPLAY#HPN3688#5&3a57&0&UID4611#{guid}`). Pure.
pub fn edid_key(device_path: &str) -> Option<String> {
    let parts: Vec<&str> = device_path.split('#').collect();
    if parts.len() < 3 || parts[1].is_empty() || parts[2].is_empty() {
        return None;
    }
    Some(format!(
        "SYSTEM\\CurrentControlSet\\Enum\\DISPLAY\\{}\\{}\\Device Parameters",
        parts[1], parts[2]
    ))
}

/// Serial number of an EDID block: the text serial descriptor (0xFF) if
/// any, else the 32-bit serial when it is a real one (0 and 0x01010101
/// are placeholders). Pure.
pub fn edid_serial(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 {
        return None;
    }
    for o in [54usize, 72, 90, 108] {
        let d = &edid[o..o + 18];
        if d[0] == 0 && d[1] == 0 && d[3] == 0xFF {
            let s: String = d[5..]
                .iter()
                .take_while(|b| **b != 0x0A && **b != 0)
                .map(|b| *b as char)
                .filter(|c| c.is_ascii_graphic())
                .collect();
            if !s.is_empty() && s.chars().any(|c| c != '0') {
                return Some(s);
            }
        }
    }
    let n = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
    (n != 0 && n != 0x0101_0101).then(|| n.to_string())
}

/// Short fingerprint of a serial number (FNV-1a, 8 hex digits): enough to
/// tell screens apart without writing the serial itself into the config.
fn fingerprint(s: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// Output technologies of a laptop's own panel: INTERNAL (0x80000000),
/// DISPLAYPORT_EMBEDDED (11), UDI_EMBEDDED (13).
fn is_internal_tech(tech: i32) -> bool {
    tech == i32::MIN || tech == 11 || tech == 13
}

/// Virtual / indirect displays: Parsec, RDP, streaming and capture tools.
fn is_virtual(t: &Target) -> bool {
    let f = t.friendly.to_ascii_lowercase();
    let p = t.device_path.to_ascii_lowercase();
    // INDIRECT_VIRTUAL (17); OTHER (-1) is what most virtual display
    // drivers report; no EDID at all on an external output; and a few
    // well-known names as a safety net
    t.tech == 17
        || t.tech == -1
        || (t.mfg == 0 && t.product == 0 && !is_internal_tech(t.tech))
        || ["parsec", "virtual", "spacedesk", "sunshine", "remote display"]
            .iter()
            .any(|k| f.contains(k) || p.contains(k))
}

/// Hardware id of a target: maker-product ("10AC-40F9"), plus a
/// fingerprint of the serial number when the screen has one
/// ("10AC-40F9-1a2b3c4d"), so two screens of the same model keep their own
/// id whatever their order, port or dock. The "#n" suffix is only added
/// for identical screens without a serial.
fn base_id(t: &Target) -> String {
    let model = if t.mfg == 0 && t.product == 0 {
        if is_internal_tech(t.tech) {
            "INTERNAL".to_string()
        } else {
            t.friendly.trim().replace(' ', "_")
        }
    } else {
        format!("{:04X}-{:04X}", t.mfg.swap_bytes(), t.product)
    };
    if model.is_empty() || t.serial.is_empty() {
        model
    } else {
        format!("{model}~{}", fingerprint(&t.serial))
    }
}

/// The model part of a hardware id ("220E-3688~bdb428c8" -> "220E-3688",
/// "10AC-40F9#2" -> "10AC-40F9"): what a replacement screen of the same
/// model shares with the old one. Pure.
pub fn model_of(hw_id: &str) -> &str {
    let s = hw_id.split('~').next().unwrap_or(hw_id);
    s.split('#').next().unwrap_or(s)
}

fn target_label(t: &Target) -> String {
    if is_internal_tech(t.tech) {
        "Built-in display".into()
    } else {
        t.friendly.trim().to_string()
    }
}

/// Give every monitor its hardware identity from the targets. Pure.
/// Identical models get "#2", "#3"... in left-to-right order, so two
/// same screens still make two different ids. A duplicated picture
/// (several targets on one source) is described by its first target
/// (the built-in panel first), the others go to `clones`.
pub fn apply_targets(mons: &mut [Monitor], targets: &[Target]) {
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    let mut uniq = |base: String| -> String {
        let n = seen.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 { base } else { format!("{base}#{n}") }
    };
    for m in mons.iter_mut() {
        let mut mine: Vec<&Target> = targets.iter().filter(|t| t.gdi.eq_ignore_ascii_case(&m.name)).collect();
        if mine.is_empty() {
            continue;
        }
        mine.sort_by_key(|t| !is_internal_tech(t.tech));
        let t = mine[0];
        m.internal = is_internal_tech(t.tech);
        m.virt = is_virtual(t);
        m.model = t.friendly.trim().to_string();
        let base = base_id(t);
        if !base.is_empty() {
            m.hw_id = uniq(base);
        }
        m.clones = mine[1..]
            .iter()
            .filter(|c| !is_virtual(c))
            .map(|c| (uniq(base_id(c)), target_label(c)))
            .collect();
    }
}

/// Human name of a screen: "Built-in display", "DELL U2720Q", else
/// "Display N".
pub fn display_label(m: &Monitor) -> String {
    if m.internal {
        "Built-in display".into()
    } else if !m.model.is_empty() {
        m.model.clone()
    } else {
        format!("Display {}", m.index + 1)
    }
}

pub mod hw {
    use std::sync::Mutex;

    use windows::Win32::Devices::Display::*;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY};

    /// The screen's serial number, read from the EDID Windows keeps in
    /// the registry ("" when unknown).
    fn serial(device_path: &str) -> String {
        let Some(key) = super::edid_key(device_path) else { return String::new() };
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (k, v) = (wide(&key), wide("EDID"));
        let mut buf = vec![0u8; 512];
        let mut len = buf.len() as u32;
        let ok = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(k.as_ptr()),
                PCWSTR(v.as_ptr()),
                RRF_RT_REG_BINARY,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            )
        } == ERROR_SUCCESS;
        if !ok {
            return String::new();
        }
        super::edid_serial(&buf[..len as usize]).unwrap_or_default()
    }

    use super::{apply_targets, Monitor, Target};

    /// Last query, keyed by the GDI names + rects it was made for: the
    /// display configuration is only asked again when they change.
    static CACHE: Mutex<Option<(String, Vec<Target>)>> = Mutex::new(None);

    fn wstr(b: &[u16]) -> String {
        String::from_utf16_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())])
    }

    pub fn targets() -> Vec<Target> {
        let mut out = Vec::new();
        unsafe {
            let (mut np, mut nm) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
                return out;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            if QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                None,
            ) != ERROR_SUCCESS
            {
                return out;
            }
            paths.truncate(np as usize);
            for p in &paths {
                let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
                src.header.adapterId = p.sourceInfo.adapterId;
                src.header.id = p.sourceInfo.id;
                if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                    continue;
                }
                let mut tn = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
                tn.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
                tn.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
                tn.header.adapterId = p.targetInfo.adapterId;
                tn.header.id = p.targetInfo.id;
                if DisplayConfigGetDeviceInfo(&mut tn.header) != 0 {
                    continue;
                }
                out.push(Target {
                    gdi: wstr(&src.viewGdiDeviceName),
                    mfg: tn.edidManufactureId,
                    product: tn.edidProductCodeId,
                    friendly: wstr(&tn.monitorFriendlyDeviceName),
                    tech: tn.outputTechnology.0,
                    serial: serial(&wstr(&tn.monitorDevicePath)),
                    device_path: wstr(&tn.monitorDevicePath),
                });
            }
        }
        out
    }

    pub fn fill(mons: &mut [Monitor]) {
        let sig: String = mons
            .iter()
            .map(|m| format!("{}:{:?};", m.name, m.rect))
            .collect();
        let mut c = CACHE.lock().unwrap();
        if c.as_ref().map(|(s, _)| s != &sig).unwrap_or(true) {
            *c = Some((sig, targets()));
        }
        apply_targets(mons, &c.as_ref().unwrap().1);
    }
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

    fn t(gdi: &str, mfg: u16, product: u16, friendly: &str, tech: i32) -> Target {
        Target {
            gdi: gdi.into(),
            mfg,
            product,
            friendly: friendly.into(),
            tech,
            device_path: String::new(),
            serial: String::new(),
        }
    }

    #[test]
    fn edid_serials_and_registry_key() {
        assert_eq!(
            edid_key(r"\\?\DISPLAY#HPN3688#5&3a57b82e&0&UID4611#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}").as_deref(),
            Some(r"SYSTEM\CurrentControlSet\Enum\DISPLAY\HPN3688\5&3a57b82e&0&UID4611\Device Parameters")
        );
        assert_eq!(edid_key("garbage"), None);
        let mut e = vec![0u8; 128];
        assert_eq!(edid_serial(&e), None, "no serial at all");
        e[12..16].copy_from_slice(&[1, 1, 1, 1]);
        assert_eq!(edid_serial(&e), None, "0x01010101 placeholder");
        e[12..16].copy_from_slice(&[0x39, 0x30, 0, 0]);
        assert_eq!(edid_serial(&e).as_deref(), Some("12345"));
        // text serial descriptor wins
        e[72..77].copy_from_slice(&[0, 0, 0, 0xFF, 0]);
        e[77..90].copy_from_slice(b"CN0ABC123\n   ");
        assert_eq!(edid_serial(&e).as_deref(), Some("CN0ABC123"));
        assert_eq!(edid_serial(&e[..60]), None);
    }

    #[test]
    fn hardware_ids_labels_and_virtual_displays() {
        let m = |name: &str, index| Monitor { name: name.into(), index, ..Default::default() };
        let mut mons = vec![m("D1", 0), m("D2", 1), m("D3", 2), m("D4", 3)];
        apply_targets(
            &mut mons,
            &[
                t("D1", 0xE430, 0x0A1B, "", i32::MIN),      // laptop panel
                t("D2", 0xAC10, 0x40F9, "DELL U2720Q", 10), // DisplayPort
                t("D3", 0xAC10, 0x40F9, "DELL U2720Q", 5),  // same model, HDMI
                t("D4", 0, 0, "Parsec Virtual Display", -1),
            ],
        );
        assert_eq!(mons[0].hw_id, "30E4-0A1B");
        assert!(mons[0].internal && !mons[0].virt);
        assert_eq!(display_label(&mons[0]), "Built-in display");
        assert_eq!(mons[1].hw_id, "10AC-40F9");
        assert_eq!(mons[2].hw_id, "10AC-40F9#2");
        assert_eq!(display_label(&mons[2]), "DELL U2720Q");
        assert!(mons[3].virt && !mons[1].virt);
        // no target info: no id, generic label
        let mut lone = vec![m("D9", 1)];
        apply_targets(&mut lone, &[]);
        assert!(lone[0].hw_id.is_empty());
        assert_eq!(display_label(&lone[0]), "Display 2");
        // two screens of one model WITH serials: their own id whatever the
        // order, so unplugging the left one does not rename the right one
        let ts = |gdi: &str, serial: &str| Target { serial: serial.into(), ..t(gdi, 0xAC10, 0x40F9, "DELL U2720Q", 10) };
        let mut pair = vec![m("L", 0), m("R", 1)];
        apply_targets(&mut pair, &[ts("L", "CN0AAA"), ts("R", "CN0BBB")]);
        let (left, right) = (pair[0].hw_id.clone(), pair[1].hw_id.clone());
        assert!(left.starts_with("10AC-40F9~") && left != right && !right.contains('#'));
        assert_eq!(model_of(&left), "10AC-40F9");
        assert_eq!(model_of("10AC-40F9#2"), "10AC-40F9");
        assert_eq!(model_of("INTERNAL"), "INTERNAL");
        assert!(!left.contains("CN0AAA"), "the serial itself is not stored");
        let mut only_right = vec![m("R", 0)];
        apply_targets(&mut only_right, &[ts("R", "CN0BBB")]);
        assert_eq!(only_right[0].hw_id, right);
        // swapped on the desk: each keeps its id
        let mut swapped = vec![m("R", 0), m("L", 1)];
        apply_targets(&mut swapped, &[ts("R", "CN0BBB"), ts("L", "CN0AAA")]);
        assert_eq!((swapped[0].hw_id.clone(), swapped[1].hw_id.clone()), (right, left));

        // duplicated picture: the built-in panel describes it, the other
        // screen is a clone
        let mut dup = vec![m("D1", 0)];
        apply_targets(&mut dup, &[t("D1", 0x7022, 0x3688, "HP E24 G4", 5), t("D1", 0xE430, 0x0A1B, "", i32::MIN)]);
        assert!(dup[0].internal);
        assert_eq!(dup[0].clones, vec![("2270-3688".to_string(), "HP E24 G4".to_string())]);
    }
}
