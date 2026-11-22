//! Data contract between the settings page (WebView2) and the daemon.
//! Pure logic: builds the JSON state the page renders and turns the
//! messages the page posts into a new `Config`. Every change is applied
//! immediately (no Save button), so each op validates on its own and
//! reports the offending field. Unit-tested in-file.

use serde::{Deserialize, Serialize};

use crate::config::{Config, HotkeyConfig};
use crate::monitor::Monitor;
use crate::zones::Layout;

const MAX_PRESET_NAME: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyRow {
    pub exe: String,
    pub key: String,
    /// "Stable" fullscreen for this app (see `Config::fs_stable`).
    #[serde(default)]
    pub stable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ZoneUi {
    pub name: String,
    pub rect: [f64; 4],
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutUi {
    pub name: String,
    /// true for built-in presets (cannot be deleted or overwritten)
    pub builtin: bool,
    pub zones: Vec<ZoneUi>,
}

/// One saved window of a plan, as the page draws it.
#[derive(Debug, Clone, Serialize)]
pub struct PlanWinUi {
    pub exe: String,
    pub title: String,
    pub monitor: String,
    pub rect: [f64; 4],
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanUi {
    pub slot: u8,
    pub windows: Vec<PlanWinUi>,
    /// Display setup key it was saved on ("" for older plans).
    pub setup: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorUi {
    /// GDI device name, the layout key.
    pub id: String,
    /// Hardware id ("" when Windows does not tell).
    pub hw_id: String,
    /// "Built-in display", the model name, else "Display N".
    pub label: String,
    /// Left-to-right number, as in "Display 2".
    pub number: usize,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
    pub internal: bool,
    /// Labels of screens showing the same picture (duplicate mode).
    pub clones: Vec<String>,
    pub layout: LayoutUi,
}

/// A remembered display setup, as the Displays page lists it.
#[derive(Debug, Clone, Serialize)]
pub struct SetupUi {
    pub key: String,
    pub label: String,
    /// Connected right now.
    pub current: bool,
    /// Plan arranged when this setup gets connected.
    pub plan: Option<u8>,
    /// The plan comes from the link of the same screens before one was
    /// replaced by the same model (no link of its own yet).
    pub inherited: bool,
    /// No plan, but it looks like `hint_from` with one screen replaced by
    /// another model: offer to keep plan `hint_plan`.
    pub hint_plan: Option<u8>,
    pub hint_from: String,
    pub last_seen: u64,
    pub displays: Vec<crate::setups::SetupDisplay>,
}

/// Everything the page needs to render.
#[derive(Debug, Clone, Serialize)]
pub struct UiState {
    pub version: String,
    pub enabled: bool,
    pub shift_snap: bool,
    pub remember_positions: bool,
    pub check_updates: bool,
    /// Update check / install status (filled by the host).
    pub update: crate::update::UpdateView,
    pub autostart: bool,
    pub log_enabled: bool,
    pub zone_gap: i32,
    pub hotkeys: HotkeyConfig,
    /// Hotkey fields the OS refused (already taken by another app).
    pub hotkeys_rejected: Vec<String>,
    pub fs_exit_keys: Vec<KeyRow>,
    pub exclude_processes: Vec<String>,
    pub exclude_class_prefixes: Vec<String>,
    pub monitors: Vec<MonitorUi>,
    /// Built-in presets first, then the user's.
    pub presets: Vec<LayoutUi>,
    /// Saved plans (slots 1-9 that have one).
    pub plans: Vec<PlanUi>,
    /// Screens seen before and not connected now, most recent first
    /// (filled by the host): what a new screen can replace.
    pub old_screens: Vec<OldScreenUi>,
    /// Remembered display setups, the connected one first (filled by the
    /// host from setups.json, see `setups_ui`).
    pub setups: Vec<SetupUi>,
    /// Display label of every key binding shown (hotkeys + exit keys) on
    /// the current keyboard layout, e.g. "Ctrl+Alt+VK_DE" -> "Ctrl+Alt+Ù".
    /// Filled by the host (needs Win32).
    pub key_labels: std::collections::BTreeMap<String, String>,
}

/// The editable settings, posted whole on every change.
#[derive(Debug, Clone, Deserialize)]
pub struct UiSettings {
    pub enabled: bool,
    pub shift_snap: bool,
    #[serde(default = "yes")]
    pub remember_positions: bool,
    #[serde(default = "yes")]
    pub check_updates: bool,
    pub autostart: bool,
    pub log_enabled: bool,
    pub zone_gap: i32,
    pub hotkeys: HotkeyConfig,
    #[serde(default)]
    pub fs_exit_keys: Vec<KeyRow>,
    #[serde(default)]
    pub exclude_processes: Vec<String>,
    #[serde(default)]
    pub exclude_class_prefixes: Vec<String>,
}

/// Messages the page can post (`{"type": "...", ...}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PageMsg {
    Ready,
    Apply { data: UiSettings },
    /// Put a preset (built-in or user) on one display.
    UsePreset { monitor: String, name: String },
    /// Save the display's current layout as a named user preset.
    SavePreset { monitor: String, name: String },
    /// Name a new preset, then draw it in the editor on that display.
    NewPreset { monitor: String, name: String },
    /// Redraw an existing user preset in the editor on that display.
    EditPreset { monitor: String, name: String },
    DeletePreset { name: String },
    /// Link a display setup to a plan (None = no plan).
    SetupPlan { key: String, slot: Option<u8> },
    /// Forget a remembered display setup (and its plan link).
    ForgetSetup { key: String },
    /// The connected screen `monitor` (device name) replaces the old
    /// screen `old` (hardware id): plans, setups, zones move over.
    ReplaceScreen { old: String, monitor: String },
    /// Plans: save the current windows into slot, go to it, or clear it.
    PlanSave { slot: u8 },
    PlanGo { slot: u8 },
    PlanClear { slot: u8 },
    /// Key capture started/stopped in a shortcut field: our own global
    /// hotkeys are suspended meanwhile so they can be recorded.
    Capture { on: bool },
    Close,
    Quit,
    OpenEditor,
    OpenConfigDir,
    OpenCredit,
    /// Show the first-run intro card again.
    Welcome,
    /// Updates: ask GitHub now, install the found version, open its page.
    UpdateCheck,
    UpdateInstall,
    UpdatePage,
}

/// Validation failure: `field` lets the page mark the right input.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiError {
    pub field: String,
    pub message: String,
}

fn yes() -> bool {
    true
}

fn err(field: &str, message: impl Into<String>) -> UiError {
    UiError {
        field: field.to_string(),
        message: message.into(),
    }
}

fn layout_ui(l: &Layout, builtin: bool) -> LayoutUi {
    LayoutUi {
        name: l.name.clone(),
        builtin,
        zones: l
            .zones
            .iter()
            .map(|z| ZoneUi {
                name: z.name.clone(),
                rect: [z.rect.left, z.rect.top, z.rect.right, z.rect.bottom],
            })
            .collect(),
    }
}

fn is_builtin(name: &str) -> bool {
    crate::presets::presets().iter().any(|p| p.name == name)
}

pub fn build_state(cfg: &Config, autostart: bool, monitors: &[Monitor]) -> UiState {
    let mut mons: Vec<&Monitor> = monitors.iter().collect();
    mons.sort_by_key(|m| m.index);
    let empty = Layout {
        name: "(none)".into(),
        zones: vec![],
    };
    let mut presets: Vec<LayoutUi> = crate::presets::presets()
        .iter()
        .map(|l| layout_ui(l, true))
        .collect();
    presets.extend(cfg.user_presets.iter().map(|l| layout_ui(l, false)));
    UiState {
        version: env!("CARGO_PKG_VERSION").to_string(),
        enabled: cfg.enabled,
        shift_snap: cfg.shift_snap,
        remember_positions: cfg.remember_positions,
        check_updates: cfg.check_updates,
        update: Default::default(),
        autostart,
        log_enabled: cfg.log_enabled,
        zone_gap: cfg.zone_gap,
        hotkeys: cfg.hotkeys.clone(),
        hotkeys_rejected: vec![],
        fs_exit_keys: cfg
            .fs_exit_keys
            .iter()
            .map(|(k, v)| KeyRow {
                exe: k.clone(),
                key: v.clone(),
                stable: cfg.is_fs_stable(k),
            })
            .collect(),
        exclude_processes: cfg.exclude_processes.clone(),
        exclude_class_prefixes: cfg.exclude_class_prefixes.clone(),
        monitors: mons
            .iter()
            .map(|m| {
                let l = cfg.layouts.for_monitor(&m.name, m.index).unwrap_or(&empty);
                MonitorUi {
                    id: m.name.clone(),
                    hw_id: m.hw_id.clone(),
                    label: crate::monitor::display_label(m),
                    number: m.index + 1,
                    x: m.rect.left,
                    y: m.rect.top,
                    width: m.rect.right - m.rect.left,
                    height: m.rect.bottom - m.rect.top,
                    primary: m.primary,
                    internal: m.internal,
                    clones: m.clones.iter().map(|c| c.1.clone()).collect(),
                    layout: layout_ui(l, is_builtin(&l.name)),
                }
            })
            .collect(),
        presets,
        plans: crate::plans::SLOTS
            .filter_map(|n| {
                cfg.plans.get(&n.to_string()).map(|p| PlanUi {
                    slot: n,
                    windows: p
                        .windows
                        .iter()
                        .map(|w| PlanWinUi {
                            exe: w.exe.clone(),
                            title: w.title.clone(),
                            // the display it would go to now
                            monitor: crate::plans::resolve_monitor(
                                w,
                                monitors,
                                Some(
                                    &crate::plans::plan_screens(p)
                                        .into_iter()
                                        .filter(|s| *s != w.monitor_hw)
                                        .collect::<Vec<_>>(),
                                ),
                            )
                            .map(|m| m.name.clone())
                                .unwrap_or_else(|| w.monitor.clone()),
                            rect: [w.rect.left, w.rect.top, w.rect.right, w.rect.bottom],
                        })
                        .collect(),
                    setup: p.setup.clone(),
                })
            })
            .collect(),
        key_labels: Default::default(),
        setups: vec![],
        old_screens: vec![],
    }
}

/// The remembered setups for the page: the connected one first (listed
/// even before it was saved), then the others, most recent first. Pure.
pub fn setups_ui(cfg: &Config, known: &[crate::setups::Setup], current: &crate::setups::Setup) -> Vec<SetupUi> {
    let ui = |s: &crate::setups::Setup, is_current: bool| {
        let hint = crate::setups::replacement_hint(cfg, known, &s.key);
        SetupUi {
        hint_plan: hint.map(|h| h.0),
        hint_from: hint.map(|h| crate::setups::label(h.1)).unwrap_or_default(),
        key: s.key.clone(),
        label: crate::setups::label(s),
        current: is_current,
        plan: crate::setups::linked_plan(cfg, &s.key).map(|p| p.0),
        inherited: crate::setups::linked_plan(cfg, &s.key).is_some_and(|p| p.1),
        last_seen: s.last_seen,
        displays: s.displays.clone(),
    }
    };
    std::iter::once(ui(current, true))
        .chain(known.iter().filter(|s| s.key != current.key).map(|s| ui(s, false)))
        .collect()
}

/// A screen seen before, not connected now (the "replaces" list).
#[derive(Debug, Clone, Serialize)]
pub struct OldScreenUi {
    pub id: String,
    pub label: String,
    pub last_seen: u64,
    /// Screens it was connected with: none of them can replace it.
    pub with: Vec<String>,
}

/// Screen `old_id` (its device name was `old_gdi`) was replaced by `new`:
/// plan windows, setup links and its zones move to the new screen. Pure.
pub fn replace_screen(cfg: &Config, old_id: &str, old_gdi: &str, new: &Monitor) -> Result<Config, UiError> {
    if new.hw_id.is_empty() {
        return Err(err("screens", "Windows does not identify this screen, so nothing can be moved to it."));
    }
    if old_id.is_empty() || old_id == new.hw_id {
        return Err(err("screens", "Pick the old screen this one replaces."));
    }
    let mut out = cfg.clone();
    for p in out.plans.values_mut() {
        for w in p.windows.iter_mut().filter(|w| w.monitor_hw == old_id) {
            w.monitor_hw = new.hw_id.clone();
            w.monitor = new.name.clone();
            w.monitor_index = new.index;
        }
        let mut ids: Vec<String> = p.setup.split('+').map(String::from).collect();
        if let Some(i) = ids.iter().position(|x| x == old_id) {
            ids[i] = new.hw_id.clone();
            ids.sort();
            p.setup = ids.join("+");
        }
    }
    let links: Vec<(String, u8)> = out.setup_plans.iter().map(|(k, v)| (k.clone(), *v)).collect();
    for (k, slot) in links {
        let mut ids: Vec<String> = k.split('+').map(String::from).collect();
        let Some(i) = ids.iter().position(|x| x == old_id) else { continue };
        ids[i] = new.hw_id.clone();
        ids.sort();
        out.setup_plans.remove(&k);
        // a link the new setup already has wins
        out.setup_plans.entry(ids.join("+")).or_insert(slot);
    }
    if !old_gdi.is_empty() && old_gdi != new.name {
        if let Some(l) = cfg.layouts.by_monitor.get(old_gdi).cloned() {
            out.layouts.set_for_monitor(&new.name, l);
        }
    }
    Ok(out)
}

/// Link setup `key` to plan `slot` (None = unlink).
pub fn set_setup_plan(cfg: &Config, key: &str, slot: Option<u8>) -> Result<Config, UiError> {
    let mut out = cfg.clone();
    match slot {
        None => {
            out.setup_plans.remove(key);
        }
        Some(n) => {
            if !cfg.plans.contains_key(&n.to_string()) {
                return Err(err("setups", &format!("Plan {n} is empty. Save it first on the Plans page.")));
            }
            if key.is_empty() {
                return Err(err("setups", "Unknown display setup."));
            }
            out.setup_plans.insert(key.to_string(), n);
        }
    }
    Ok(out)
}

fn clean_list(v: &[String], lower: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        let t = if lower {
            s.trim().to_lowercase()
        } else {
            s.trim().to_string()
        };
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Validate the editable settings and produce the new Config. Autostart
/// is NOT applied here (registry side effect, host's job).
pub fn apply_settings(cfg: &Config, s: &UiSettings) -> Result<Config, UiError> {
    if !(0..=200).contains(&s.zone_gap) {
        return Err(err("zone_gap", "Zone gap must be between 0 and 200 px."));
    }
    let hk = [
        ("toggle", "Pause / resume", &s.hotkeys.toggle),
        ("editor", "Zone editor", &s.hotkeys.editor),
        ("next_preset", "Next preset", &s.hotkeys.next_preset),
        ("next_zone", "Window to next zone", &s.hotkeys.next_zone),
        ("prev_zone", "Window to previous zone", &s.hotkeys.prev_zone),
        ("fs_toggle", "Fullscreen: zone or whole screen", &s.hotkeys.fs_toggle),
        ("cheatsheet", "Show all shortcuts", &s.hotkeys.cheatsheet),
    ];
    let mut seen: Vec<(&str, _)> = Vec::new();
    for (field, name, v) in hk {
        let Some(parsed) = crate::hotkeys::parse(v.trim()) else {
            return Err(err(
                &format!("hotkeys.{field}"),
                format!("'{}' is not a valid shortcut for {name}.", v.trim()),
            ));
        };
        if let Some((other, _)) = seen.iter().find(|(_, p)| *p == parsed) {
            return Err(err(
                &format!("hotkeys.{field}"),
                format!("{name} uses the same keys as {other}."),
            ));
        }
        seen.push((name, parsed));
    }
    let go = s.hotkeys.plan_go.trim();
    let save = s.hotkeys.plan_save.trim();
    for (field, v) in [("plan_go", go), ("plan_save", save)] {
        if crate::hotkeys::parse_mods(v).is_none() {
            return Err(err(
                &format!("hotkeys.{field}"),
                "Plan shortcuts need modifiers only (e.g. Alt+Shift); the digit 1-9 is added.",
            ));
        }
    }
    if crate::hotkeys::parse_mods(go) == crate::hotkeys::parse_mods(save) {
        return Err(err(
            "hotkeys.plan_save",
            "Saving a plan needs different keys than going to a plan.",
        ));
    }
    let drag = s.hotkeys.drag_snap.trim();
    if !drag.is_empty() && crate::hotkeys::parse_mods(drag).is_none() {
        return Err(err(
            "hotkeys.drag_snap",
            "Snap while dragging needs modifiers only (e.g. Shift, Ctrl+Alt), or nothing for no key.",
        ));
    }
    let mut keys = std::collections::BTreeMap::new();
    let mut stable = Vec::new();
    for (i, row) in s.fs_exit_keys.iter().enumerate() {
        let (exe, key) = (row.exe.trim(), row.key.trim());
        if exe.is_empty() && key.is_empty() {
            continue;
        }
        if exe.is_empty() {
            return Err(err(&format!("fs_exit_keys.{i}"), "Program name is missing."));
        }
        if crate::hotkeys::vk_from_name(key).is_none() {
            return Err(err(
                &format!("fs_exit_keys.{i}"),
                format!("Unknown key '{key}' for {exe}."),
            ));
        }
        keys.insert(exe.to_string(), key.to_string());
        if row.stable {
            stable.push(exe.to_string());
        }
    }
    let mut out = cfg.clone();
    out.enabled = s.enabled;
    out.shift_snap = s.shift_snap;
    out.remember_positions = s.remember_positions;
    out.check_updates = s.check_updates;
    out.log_enabled = s.log_enabled;
    out.zone_gap = s.zone_gap;
    out.hotkeys = HotkeyConfig {
        toggle: s.hotkeys.toggle.trim().to_string(),
        editor: s.hotkeys.editor.trim().to_string(),
        next_preset: s.hotkeys.next_preset.trim().to_string(),
        next_zone: s.hotkeys.next_zone.trim().to_string(),
        prev_zone: s.hotkeys.prev_zone.trim().to_string(),
        fs_toggle: s.hotkeys.fs_toggle.trim().to_string(),
        cheatsheet: s.hotkeys.cheatsheet.trim().to_string(),
        plan_go: go.to_string(),
        plan_save: save.to_string(),
        drag_snap: drag.to_string(),
    };
    out.fs_exit_keys = keys;
    out.fs_stable = stable;
    out.exclude_processes = clean_list(&s.exclude_processes, true);
    out.exclude_class_prefixes = clean_list(&s.exclude_class_prefixes, false);
    Ok(out)
}

fn find_monitor<'a>(monitors: &'a [Monitor], id: &str) -> Result<&'a Monitor, UiError> {
    monitors
        .iter()
        .find(|m| m.name == id)
        .ok_or_else(|| err("monitors", "That display is no longer connected."))
}

/// Put preset `name` (built-in or user) on display `monitor`.
pub fn use_preset(
    cfg: &Config,
    monitors: &[Monitor],
    monitor: &str,
    name: &str,
) -> Result<Config, UiError> {
    let m = find_monitor(monitors, monitor)?;
    let all = crate::presets::all(&cfg.user_presets);
    let l = all
        .iter()
        .find(|l| l.name == name)
        .ok_or_else(|| err("presets", format!("Preset '{name}' does not exist.")))?;
    let mut out = cfg.clone();
    out.layouts.set_for_monitor(&m.name, l.clone());
    Ok(out)
}

/// Save the current layout of `monitor` as user preset `name`. An
/// existing user preset with that name is replaced; built-in names are
/// reserved. The display then shows the preset by its new name.
pub fn save_preset(
    cfg: &Config,
    monitors: &[Monitor],
    monitor: &str,
    name: &str,
) -> Result<Config, UiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err("preset_name", "Give the preset a name."));
    }
    if name.chars().count() > MAX_PRESET_NAME {
        return Err(err(
            "preset_name",
            format!("Keep the name under {MAX_PRESET_NAME} characters."),
        ));
    }
    if is_builtin(name) {
        return Err(err("preset_name", format!("'{name}' is a built-in preset name.")));
    }
    let m = find_monitor(monitors, monitor)?;
    let Some(cur) = cfg.layouts.for_monitor(&m.name, m.index) else {
        return Err(err("preset_name", "This display has no layout to save."));
    };
    if cur.zones.is_empty() {
        return Err(err("preset_name", "This display has no zones to save."));
    }
    let mut layout = cur.clone();
    layout.name = name.to_string();
    let mut out = cfg.clone();
    match out.user_presets.iter_mut().find(|p| p.name == name) {
        Some(p) => *p = layout.clone(),
        None => out.user_presets.push(layout.clone()),
    }
    out.layouts.set_for_monitor(&m.name, layout);
    Ok(out)
}

/// Store a captured plan in `slot` (1-9).
pub fn set_plan(cfg: &Config, slot: u8, plan: crate::plans::Plan) -> Result<Config, UiError> {
    if !crate::plans::SLOTS.contains(&slot) {
        return Err(err("plans", "Plans go from 1 to 9."));
    }
    if plan.windows.is_empty() {
        return Err(err("plans", "No app window is open to save."));
    }
    let mut out = cfg.clone();
    out.plans.insert(slot.to_string(), plan);
    Ok(out)
}

pub fn clear_plan(cfg: &Config, slot: u8) -> Config {
    let mut out = cfg.clone();
    out.plans.remove(&slot.to_string());
    // a cleared plan is no longer arranged for any setup
    out.setup_plans.retain(|_, n| *n != slot);
    out
}

/// Name of a layout drawn without a pending preset name.
pub const CUSTOM: &str = "Custom";

/// Check the name typed for "New preset" before the editor opens.
pub fn check_new_preset_name(cfg: &Config, name: &str) -> Result<String, UiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(err("preset_name", "Give the preset a name."));
    }
    if name.chars().count() > MAX_PRESET_NAME {
        return Err(err(
            "preset_name",
            format!("Keep the name under {MAX_PRESET_NAME} characters."),
        ));
    }
    if is_builtin(name) || name.eq_ignore_ascii_case(CUSTOM) {
        return Err(err("preset_name", format!("'{name}' is a reserved preset name.")));
    }
    if cfg.user_presets.iter().any(|p| p.name == name) {
        return Err(err(
            "preset_name",
            format!("'{name}' already exists. Use Edit on it instead."),
        ));
    }
    Ok(name.to_string())
}

/// Store what the zone editor drew on `monitor`. With `preset_name` (New
/// / Edit preset flow) the layout becomes that user preset (created or
/// replaced). Without it, the layout keeps the name of the preset it
/// matches exactly, or becomes "Custom" - never a preset name with
/// different zones.
pub fn commit_editor_layout(
    cfg: &Config,
    monitor: &str,
    mut layout: Layout,
    preset_name: Option<&str>,
) -> Config {
    let mut out = cfg.clone();
    match preset_name {
        Some(name) => {
            layout.name = name.to_string();
            match out.user_presets.iter_mut().find(|p| p.name == name) {
                Some(p) => *p = layout.clone(),
                None => out.user_presets.push(layout.clone()),
            }
        }
        None => {
            layout.name = crate::presets::all(&cfg.user_presets)
                .into_iter()
                .find(|p| p.zones == layout.zones)
                .map(|p| p.name)
                .unwrap_or_else(|| CUSTOM.to_string());
        }
    }
    out.layouts.set_for_monitor(monitor, layout);
    out
}

/// Delete a user preset. Displays using it keep their zones.
pub fn delete_preset(cfg: &Config, name: &str) -> Result<Config, UiError> {
    if is_builtin(name) {
        return Err(err("presets", "Built-in presets cannot be deleted."));
    }
    let mut out = cfg.clone();
    let before = out.user_presets.len();
    out.user_presets.retain(|p| p.name != name);
    if out.user_presets.len() == before {
        return Err(err("presets", format!("Preset '{name}' does not exist.")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rect::Rect;

    fn mon(name: &str, index: usize) -> Monitor {
        Monitor {
            name: name.into(),
            rect: Rect::new(0, 0, 3440, 1440),
            work: Rect::new(0, 0, 3440, 1400),
            index,
            dpi_x: 96,
            dpi_y: 96,
            primary: index == 0,
            handle: 0,
            ..Default::default()
        }
    }

    fn settings_from(st: &UiState) -> UiSettings {
        serde_json::from_value(serde_json::to_value(st).unwrap()).unwrap()
    }

    #[test]
    fn state_roundtrips_unchanged() {
        let cfg = Config::default();
        let st = build_state(&cfg, false, &[mon("D1", 0)]);
        assert_eq!(st.monitors.len(), 1);
        let out = apply_settings(&cfg, &settings_from(&st)).unwrap();
        assert_eq!(out.fs_exit_keys, cfg.fs_exit_keys);
        assert_eq!(out.hotkeys.toggle, cfg.hotkeys.toggle);
        assert_eq!(out.zone_gap, cfg.zone_gap);
    }

    #[test]
    fn errors_name_the_field() {
        let cfg = Config::default();
        let st = build_state(&cfg, false, &[]);
        let mut s = settings_from(&st);
        s.hotkeys.editor = "Win+Banana".into();
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "hotkeys.editor");
        let mut s = settings_from(&st);
        s.fs_exit_keys.insert(
            0,
            KeyRow {
                exe: "x.exe".into(),
                key: "Nope".into(),
                stable: false,
            },
        );
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "fs_exit_keys.0");
        let mut s = settings_from(&st);
        s.zone_gap = -3;
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "zone_gap");
        // the same keys twice: the second one is flagged
        let mut s = settings_from(&st);
        s.hotkeys.prev_zone = s.hotkeys.next_zone.clone();
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "hotkeys.prev_zone");
        // defaults are distinct and valid
        assert!(apply_settings(&cfg, &settings_from(&st)).is_ok());
    }

    #[test]
    fn stable_flag_roundtrips() {
        let cfg = Config::default();
        let st = build_state(&cfg, false, &[]);
        let mut s = settings_from(&st);
        let i = s.fs_exit_keys.iter().position(|r| r.exe == "chrome.exe").unwrap();
        s.fs_exit_keys[i].stable = true;
        let out = apply_settings(&cfg, &s).unwrap();
        assert_eq!(out.fs_stable, vec!["chrome.exe".to_string()]);
        assert!(out.is_fs_stable("CHROME.EXE"));
        let st2 = build_state(&out, false, &[]);
        assert!(st2.fs_exit_keys.iter().any(|r| r.exe == "chrome.exe" && r.stable));
        // removing the row drops the flag
        let mut s = settings_from(&st2);
        s.fs_exit_keys.retain(|r| r.exe != "chrome.exe");
        assert!(apply_settings(&out, &s).unwrap().fs_stable.is_empty());
    }

    #[test]
    fn plan_shortcuts_validated_and_plans_listed() {
        let cfg = Config::default();
        let st = build_state(&cfg, false, &[]);
        let mut s = settings_from(&st);
        assert_eq!(s.hotkeys.plan_go, "Alt+Shift");
        s.hotkeys.plan_go = "Alt+Q".into();
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "hotkeys.plan_go");
        let mut s = settings_from(&st);
        s.hotkeys.plan_save = "Shift+Alt".into();
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "hotkeys.plan_save");

        let plan = crate::plans::Plan {
            windows: vec![crate::plans::PlanWindow {
                exe: "discord.exe".into(),
                path: String::new(),
                aumid: String::new(),
                class: String::new(),
                title: "Discord".into(),
                monitor: "D1".into(),
                monitor_index: 0,
                monitor_hw: String::new(),
                rect: crate::rect::FracRect::new(0.0, 0.0, 0.5, 1.0),
            }],
            setup: String::new(),
        };
        assert!(set_plan(&cfg, 0, plan.clone()).is_err());
        assert!(set_plan(&cfg, 3, crate::plans::Plan::default()).is_err());
        let cfg = set_plan(&cfg, 3, plan).unwrap();
        let st = build_state(&cfg, false, &[]);
        assert_eq!(st.plans.len(), 1);
        assert_eq!((st.plans[0].slot, st.plans[0].windows[0].exe.as_str()), (3, "discord.exe"));
        assert!(clear_plan(&cfg, 3).plans.is_empty());

        // setups: link only existing plans; clearing the plan unlinks it
        assert_eq!(set_setup_plan(&cfg, "L", Some(4)).unwrap_err().field, "setups");
        let linked = set_setup_plan(&cfg, "L", Some(3)).unwrap();
        assert_eq!(linked.setup_plans.get("L"), Some(&3));
        assert!(set_setup_plan(&linked, "L", None).unwrap().setup_plans.is_empty());
        assert!(clear_plan(&linked, 3).setup_plans.is_empty());
        // a replaced screen: plan windows, links and zones move over
        let mut c2 = linked.clone();
        let mut p = c2.plans["3"].clone();
        p.windows[0].monitor_hw = "GAME-1~a".into();
        p.windows[0].monitor = "D2".into();
        p.setup = "GAME-1~a+L".into();
        c2.plans.insert("3".into(), p);
        c2.setup_plans.insert("GAME-1~a+L".into(), 3);
        c2.layouts.set_for_monitor("D2", crate::zones::Layout::halves());
        let mut new = mon("D5", 1);
        new.hw_id = "GAME-2~b".into();
        let moved = replace_screen(&c2, "GAME-1~a", "D2", &new).unwrap();
        let w = &moved.plans["3"].windows[0];
        assert_eq!((w.monitor_hw.as_str(), w.monitor.as_str(), w.monitor_index), ("GAME-2~b", "D5", 1));
        assert_eq!(moved.plans["3"].setup, "GAME-2~b+L");
        assert_eq!(moved.setup_plans.get("GAME-2~b+L"), Some(&3));
        assert!(!moved.setup_plans.contains_key("GAME-1~a+L"));
        assert_eq!(moved.layouts.by_monitor.get("D5").map(|l| l.name.as_str()), Some("halves"));
        assert_eq!(replace_screen(&c2, "GAME-2~b", "", &new).unwrap_err().field, "screens");
        new.hw_id.clear();
        assert!(replace_screen(&c2, "GAME-1~a", "D2", &new).is_err());

        let cur = crate::setups::Setup { key: "L".into(), displays: vec![], last_seen: 9 };
        let old = crate::setups::Setup { key: "L+X".into(), displays: vec![], last_seen: 5 };
        let list = setups_ui(&linked, &[old.clone(), cur.clone()], &cur);
        assert_eq!(
            list.iter().map(|s| (s.key.as_str(), s.current, s.plan)).collect::<Vec<_>>(),
            vec![("L", true, Some(3)), ("L+X", false, None)]
        );
    }

    #[test]
    fn drag_snap_modifiers_only_or_empty() {
        let cfg = Config::default();
        let st = build_state(&cfg, false, &[]);
        // default is Shift, other modifier sets pass, empty means no key
        let mut s = settings_from(&st);
        assert_eq!(s.hotkeys.drag_snap, "Shift");
        s.hotkeys.drag_snap = "Ctrl+Alt".into();
        assert_eq!(apply_settings(&cfg, &s).unwrap().hotkeys.drag_snap, "Ctrl+Alt");
        let mut s = settings_from(&st);
        s.hotkeys.drag_snap = "".into();
        assert_eq!(apply_settings(&cfg, &s).unwrap().hotkeys.drag_snap, "");
        // a key or a made-up name is refused
        let mut s = settings_from(&st);
        s.hotkeys.drag_snap = "Alt+Q".into();
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "hotkeys.drag_snap");
        let mut s = settings_from(&st);
        s.hotkeys.drag_snap = "Banana".into();
        assert_eq!(apply_settings(&cfg, &s).unwrap_err().field, "hotkeys.drag_snap");
    }

    #[test]
    fn exclusions_cleaned() {
        let cfg = Config::default();
        let st = build_state(&cfg, false, &[]);
        let mut s = settings_from(&st);
        s.exclude_processes = vec![" Game.EXE ".into(), "".into(), "game.exe".into()];
        let out = apply_settings(&cfg, &s).unwrap();
        assert_eq!(out.exclude_processes, vec!["game.exe".to_string()]);
    }

    #[test]
    fn presets_per_display() {
        let mons = [mon("D1", 0), mon("D2", 1)];
        let mut cfg = Config::default();
        cfg.layouts.ensure(&[("D1".into(), 0), ("D2".into(), 1)]);
        let thirds = crate::presets::presets()[1].name.clone();
        let cfg = use_preset(&cfg, &mons, "D2", &thirds).unwrap();
        assert_eq!(cfg.layouts.for_monitor("D2", 1).unwrap().name, thirds);
        assert_ne!(cfg.layouts.for_monitor("D1", 0).unwrap().name, thirds);
        assert!(use_preset(&cfg, &mons, "D9", &thirds).is_err());
        assert!(use_preset(&cfg, &mons, "D1", "nope").is_err());
    }

    #[test]
    fn save_use_delete_user_preset() {
        let mons = [mon("D1", 0), mon("D2", 1)];
        let mut cfg = Config::default();
        cfg.layouts.ensure(&[("D1".into(), 0), ("D2".into(), 1)]);
        let thirds = crate::presets::presets()[1].clone();
        cfg = use_preset(&cfg, &mons, "D1", &thirds.name).unwrap();

        // built-in names reserved, empty names refused
        assert!(save_preset(&cfg, &mons, "D1", &thirds.name).is_err());
        assert!(save_preset(&cfg, &mons, "D1", "  ").is_err());

        cfg = save_preset(&cfg, &mons, "D1", " Coding ").unwrap();
        assert_eq!(cfg.user_presets.len(), 1);
        assert_eq!(cfg.user_presets[0].name, "Coding");
        assert_eq!(cfg.user_presets[0].zones, thirds.zones);
        assert_eq!(cfg.layouts.for_monitor("D1", 0).unwrap().name, "Coding");

        // same name overwrites instead of duplicating
        cfg = save_preset(&cfg, &mons, "D2", "Coding").unwrap();
        assert_eq!(cfg.user_presets.len(), 1);

        // user preset usable on another display, listed after built-ins
        cfg = use_preset(&cfg, &mons, "D1", "Coding").unwrap();
        let st = build_state(&cfg, false, &mons);
        let last = st.presets.last().unwrap();
        assert_eq!((last.name.as_str(), last.builtin), ("Coding", false));
        assert!(st.presets[0].builtin);

        // delete keeps the display's zones
        cfg = delete_preset(&cfg, "Coding").unwrap();
        assert!(cfg.user_presets.is_empty());
        assert!(!cfg.layouts.for_monitor("D1", 0).unwrap().zones.is_empty());
        assert!(delete_preset(&cfg, &thirds.name).is_err());
        assert!(delete_preset(&cfg, "Coding").is_err());
    }

    #[test]
    fn new_preset_names_and_editor_commit() {
        let mut cfg = Config::default();
        cfg.layouts.ensure(&[("D1".into(), 0)]);
        let thirds = crate::presets::presets()[1].clone();
        assert!(check_new_preset_name(&cfg, " ").is_err());
        assert!(check_new_preset_name(&cfg, &thirds.name).is_err());
        assert!(check_new_preset_name(&cfg, "custom").is_err());
        assert_eq!(check_new_preset_name(&cfg, " Gaming ").unwrap(), "Gaming");

        // new preset flow: drawn layout becomes the user preset + display layout
        let mut drawn = thirds.clone();
        drawn.zones.pop();
        let cfg = commit_editor_layout(&cfg, "D1", drawn.clone(), Some("Gaming"));
        assert_eq!(cfg.user_presets.len(), 1);
        assert_eq!(cfg.user_presets[0].name, "Gaming");
        assert_eq!(cfg.user_presets[0].zones, drawn.zones);
        assert_eq!(cfg.layouts.for_monitor("D1", 0).unwrap().name, "Gaming");
        assert!(check_new_preset_name(&cfg, "Gaming").is_err());

        // edit flow replaces it in place
        let cfg = commit_editor_layout(&cfg, "D1", thirds.clone(), Some("Gaming"));
        assert_eq!(cfg.user_presets.len(), 1);
        assert_eq!(cfg.user_presets[0].zones, thirds.zones);

        // free drawing: exact match keeps a preset name, otherwise Custom
        let mut halves_named_wrong = crate::presets::presets()[0].clone();
        halves_named_wrong.name = "whatever".into();
        let cfg = commit_editor_layout(&cfg, "D1", halves_named_wrong, None);
        assert_eq!(cfg.layouts.for_monitor("D1", 0).unwrap().name, crate::presets::presets()[0].name);
        let mut odd = thirds.clone();
        odd.zones.truncate(1);
        let cfg = commit_editor_layout(&cfg, "D1", odd, None);
        assert_eq!(cfg.layouts.for_monitor("D1", 0).unwrap().name, CUSTOM);
    }

    #[test]
    fn page_messages_parse() {
        let m: PageMsg = serde_json::from_str(r#"{"type":"ready"}"#).unwrap();
        assert!(matches!(m, PageMsg::Ready));
        let m: PageMsg =
            serde_json::from_str(r#"{"type":"save_preset","monitor":"D1","name":"x"}"#).unwrap();
        assert!(matches!(m, PageMsg::SavePreset { .. }));
        let m: PageMsg = serde_json::from_str(r#"{"type":"quit"}"#).unwrap();
        assert!(matches!(m, PageMsg::Quit));
        let m: PageMsg = serde_json::from_str(r#"{"type":"welcome"}"#).unwrap();
        assert!(matches!(m, PageMsg::Welcome));
    }
}
