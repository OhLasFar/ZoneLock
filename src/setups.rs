//! Display setups: the set of screens connected at a time ("built-in
//! display only", "built-in + HP E24 G4"). Each setup the watcher sees is
//! remembered in %APPDATA%\ZoneLock\setups.json (a cache, like
//! positions.json: config.json is only written by the user), and the user
//! can link a plan to a setup (`config.setup_plans`, key -> slot). When
//! the connected screens change to a linked setup, the watcher asks the UI
//! thread to arrange that plan's open windows (`plans::arrange`).
//!
//! Screens are told apart by their hardware id (EDID maker + product,
//! `monitor::Monitor::hw_id`), not by the GDI name, which changes with
//! ports and docks. Virtual displays (Parsec, remote desktop...) never
//! count. The pure part is tested in-file.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::monitor::{display_label, Monitor};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupDisplay {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub primary: bool,
    /// Position and size in pixels, for the arrangement drawing.
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Labels of screens duplicating this one.
    #[serde(default)]
    pub clones: Vec<String>,
    /// GDI device name it had (to carry its zones over on a replacement).
    #[serde(default)]
    pub gdi: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    pub key: String,
    pub displays: Vec<SetupDisplay>,
    /// Unix seconds of the last time this setup was connected.
    #[serde(default)]
    pub last_seen: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Known {
    #[serde(default)]
    pub setups: Vec<Setup>,
}

/// How many setups are remembered (the oldest one is dropped).
pub const MAX_SETUPS: usize = 8;

fn real(mons: &[Monitor]) -> impl Iterator<Item = &Monitor> {
    mons.iter().filter(|m| !m.virt)
}

/// Id of a screen: its hardware id, else its GDI name (older drivers).
fn screen_id(m: &Monitor) -> String {
    if m.hw_id.is_empty() {
        format!("gdi:{}", m.name)
    } else {
        m.hw_id.clone()
    }
}

/// The setup key of the connected screens: sorted ids joined by "+".
/// Virtual displays are left out. Pure.
pub fn key_of(mons: &[Monitor]) -> String {
    let mut ids: Vec<String> = real(mons)
        .flat_map(|m| std::iter::once(screen_id(m)).chain(m.clones.iter().map(|c| c.0.clone())))
        .collect();
    ids.sort();
    ids.join("+")
}

/// What the screens look like now. Pure.
pub fn snapshot(mons: &[Monitor], now: u64) -> Setup {
    Setup {
        key: key_of(mons),
        displays: real(mons)
            .map(|m| SetupDisplay {
                id: screen_id(m),
                label: display_label(m),
                internal: m.internal,
                primary: m.primary,
                x: m.rect.left,
                y: m.rect.top,
                w: m.rect.width(),
                h: m.rect.height(),
                clones: m.clones.iter().map(|c| c.1.clone()).collect(),
                gdi: m.name.clone(),
            })
            .collect(),
        last_seen: now,
    }
}

/// "Built-in display only", "Built-in display + HP E24 G4",
/// "Built-in display = HP E24 G4" (duplicated). Pure.
pub fn label(s: &Setup) -> String {
    let parts: Vec<String> = s
        .displays
        .iter()
        .map(|d| {
            std::iter::once(d.label.as_str())
                .chain(d.clones.iter().map(|c| c.as_str()))
                .collect::<Vec<_>>()
                .join(" = ")
        })
        .collect();
    match parts.len() {
        0 => "No display".into(),
        1 if s.displays[0].clones.is_empty() => format!("{} only", parts[0]),
        _ => parts.join(" + "),
    }
}

impl Known {
    /// Remember `s` (new, or seen again with a new arrangement); true
    /// when something changed. Keeps the MAX_SETUPS most recent.
    pub fn record(&mut self, s: Setup) -> bool {
        if s.displays.is_empty() {
            return false;
        }
        if let Some(old) = self.setups.iter_mut().find(|x| x.key == s.key) {
            if old.displays == s.displays && s.last_seen.saturating_sub(old.last_seen) < 86_400 {
                return false;
            }
            *old = s;
        } else {
            self.setups.push(s);
        }
        self.setups.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        self.setups.truncate(MAX_SETUPS);
        true
    }

    /// Screen `old` was replaced by `new` (settings page): every setup
    /// holding it now holds the new one. Setups that become the same are
    /// merged, the most recent kept.
    pub fn replace_screen(&mut self, old: &str, new: &crate::monitor::Monitor) -> bool {
        let mut changed = false;
        for s in &mut self.setups {
            let mut ids: Vec<String> = s.key.split('+').map(String::from).collect();
            let Some(i) = ids.iter().position(|x| x == old) else { continue };
            ids[i] = new.hw_id.clone();
            ids.sort();
            s.key = ids.join("+");
            for d in s.displays.iter_mut().filter(|d| d.id == old) {
                d.id = new.hw_id.clone();
                d.label = crate::monitor::display_label(new);
                d.internal = new.internal;
                d.gdi = new.name.clone();
            }
            changed = true;
        }
        self.setups.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        let mut seen = std::collections::HashSet::new();
        self.setups.retain(|s| seen.insert(s.key.clone()));
        changed
    }

    pub fn forget(&mut self, key: &str) -> bool {
        let n = self.setups.len();
        self.setups.retain(|s| s.key != key);
        n != self.setups.len()
    }
}

/// A setup key with every screen reduced to its model (sorted): what a
/// setup keeps when a screen is replaced by the same model. Pure.
pub fn model_key(key: &str) -> String {
    let mut v: Vec<&str> = key.split('+').map(crate::monitor::model_of).collect();
    v.sort();
    v.join("+")
}

/// The plan linked to setup `key`: its own link, else the link of the
/// setup it was before a screen got replaced by the same model (same
/// models, all links agreeing on one plan). `true` = inherited. Pure.
pub fn linked_plan(cfg: &crate::config::Config, key: &str) -> Option<(u8, bool)> {
    let exists = |n: &u8| cfg.plans.contains_key(&n.to_string());
    if let Some(n) = cfg.setup_plans.get(key) {
        return exists(n).then_some((*n, false));
    }
    let mk = model_key(key);
    let mut slots: Vec<u8> = cfg
        .setup_plans
        .iter()
        .filter(|(k, n)| model_key(k) == mk && exists(n))
        .map(|(_, n)| *n)
        .collect();
    slots.sort();
    slots.dedup();
    (slots.len() == 1).then(|| (slots[0], true))
}

/// A setup with no plan of its own that looks like a linked setup with
/// ONE screen changed (same number of screens, all others identical): a
/// screen probably replaced by another model. Returns that plan and the
/// old setup, for a "Keep plan N" suggestion - never applied on its own,
/// since a different desk with another screen looks the same. Pure.
pub fn replacement_hint<'a>(
    cfg: &crate::config::Config,
    known: &'a [Setup],
    key: &str,
) -> Option<(u8, &'a Setup)> {
    if linked_plan(cfg, key).is_some() {
        return None;
    }
    let ids: Vec<&str> = key.split('+').collect();
    let mut hits: Vec<(u8, &Setup)> = known
        .iter()
        .filter(|s| s.key != key)
        .filter_map(|s| {
            let n = *cfg.setup_plans.get(&s.key)?;
            if !cfg.plans.contains_key(&n.to_string()) {
                return None;
            }
            let old: Vec<&str> = s.key.split('+').collect();
            let gone = old.iter().filter(|x| !ids.contains(x)).count();
            let new = ids.iter().filter(|x| !old.contains(x)).count();
            (old.len() == ids.len() && gone == 1 && new == 1).then_some((n, s))
        })
        .collect();
    hits.sort_by(|a, b| b.1.last_seen.cmp(&a.1.last_seen));
    let first = hits.first()?.0;
    hits.iter().all(|h| h.0 == first).then(|| hits[0])
}

/// The plan to arrange after the screens changed from `prev` to `now`:
/// only on a real change of setup, only a linked plan that exists. Pure.
pub fn plan_to_apply(cfg: &crate::config::Config, prev: &str, now: &str) -> Option<u8> {
    if prev == now || now.is_empty() {
        return None;
    }
    linked_plan(cfg, now).map(|(n, _)| n)
}

// ---------- cache file ----------

static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
/// setups.json existed but could not be read: never overwrite it.
static BROKEN: AtomicBool = AtomicBool::new(false);

fn path() -> std::path::PathBuf {
    crate::config::config_dir().join("setups.json")
}

fn with_known<R>(f: impl FnOnce(&mut Known) -> R) -> R {
    let mut g = KNOWN.lock().unwrap();
    if g.is_none() {
        *g = Some(match std::fs::read_to_string(path()) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_else(|e| {
                crate::log::line(format!("setups.json unreadable ({e}); not saving setups"));
                BROKEN.store(true, Ordering::Relaxed);
                Known::default()
            }),
            Err(_) => Known::default(),
        });
    }
    f(g.as_mut().unwrap())
}

fn save(k: &Known) {
    if BROKEN.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(t) = serde_json::to_string_pretty(k) {
        let _ = std::fs::create_dir_all(crate::config::config_dir());
        if let Err(e) = std::fs::write(path(), t) {
            crate::log::line(format!("setups.json not written: {e}"));
        }
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Remember the screens connected now (watcher: start + every change).
pub fn record_current(mons: &[Monitor]) {
    let s = snapshot(mons, now_secs());
    with_known(|k| {
        if k.record(s) {
            save(k);
        }
    });
}

/// Every remembered setup, most recent first.
pub fn known() -> Vec<Setup> {
    with_known(|k| k.setups.clone())
}

/// Screens seen before but not connected now: (display, last seen), the
/// most recent sighting of each. Candidates for "replaces an old
/// screen". Pure.
pub fn old_screens(known: &[Setup], mons: &[Monitor]) -> Vec<(SetupDisplay, u64)> {
    let now: Vec<String> = real(mons).map(screen_id).collect();
    let mut out: Vec<(SetupDisplay, u64)> = Vec::new();
    for s in known {
        for d in &s.displays {
            if now.contains(&d.id) || d.id.starts_with("gdi:") {
                continue;
            }
            match out.iter_mut().find(|(x, _)| x.id == d.id) {
                Some(o) if o.1 >= s.last_seen => {}
                Some(o) => *o = (d.clone(), s.last_seen),
                None => out.push((d.clone(), s.last_seen)),
            }
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// Screens `a` and `b` were connected at the same time at least once:
/// then one cannot be the replacement of the other. Pure.
pub fn seen_together(known: &[Setup], a: &str, b: &str) -> bool {
    known.iter().any(|s| {
        let ids: Vec<&str> = s.key.split('+').collect();
        ids.contains(&a) && ids.contains(&b)
    })
}

/// Settings page: screen `old` was replaced by `new`, in setups.json.
pub fn replace_screen(old: &str, new: &Monitor) {
    with_known(|k| {
        if k.replace_screen(old, new) {
            save(k);
        }
    });
}

/// Drop a remembered setup (settings page).
pub fn forget(key: &str) {
    with_known(|k| {
        if k.forget(key) {
            save(k);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rect::Rect;

    fn mon(name: &str, hw: &str, x: i32, internal: bool) -> Monitor {
        Monitor {
            name: name.into(),
            rect: Rect::new(x, 0, x + 1920, 1080),
            work: Rect::new(x, 0, x + 1920, 1040),
            hw_id: hw.into(),
            model: if internal { String::new() } else { "HP E24 G4".into() },
            internal,
            primary: internal,
            ..Default::default()
        }
    }

    #[test]
    fn keys_ignore_order_gdi_names_and_virtual_displays() {
        let laptop = mon("\\\\.\\DISPLAY1", "4D10-1590", 0, true);
        let hp = mon("\\\\.\\DISPLAY2", "2270-3688", 1920, false);
        let both = key_of(&[laptop.clone(), hp.clone()]);
        assert_eq!(both, "2270-3688+4D10-1590");
        // other port / dock: other GDI name, same setup
        let mut hp3 = hp.clone();
        hp3.name = "\\\\.\\DISPLAY5".into();
        assert_eq!(key_of(&[hp3, laptop.clone()]), both);
        // a Parsec display does not make a new setup
        let mut parsec = mon("\\\\.\\DISPLAY9", "", 3840, false);
        parsec.virt = true;
        assert_eq!(key_of(&[laptop.clone(), hp.clone(), parsec]), both);
        assert_eq!(key_of(&[laptop.clone()]), "4D10-1590");
        // duplicated: still both screens
        let mut dup = laptop.clone();
        dup.clones = vec![("2270-3688".into(), "HP E24 G4".into())];
        assert_eq!(key_of(&[dup.clone()]), both);
        assert_eq!(label(&snapshot(&[dup], 0)), "Built-in display = HP E24 G4");
        assert_eq!(label(&snapshot(&[laptop.clone()], 0)), "Built-in display only");
        assert_eq!(label(&snapshot(&[laptop, hp], 0)), "Built-in display + HP E24 G4");
    }

    #[test]
    fn record_keeps_recent_setups_and_forgets() {
        let mut k = Known::default();
        let laptop = mon("D1", "L", 0, true);
        assert!(k.record(snapshot(&[laptop.clone()], 100)));
        assert!(!k.record(snapshot(&[laptop.clone()], 200)), "same setup, same day: no write");
        assert!(k.record(snapshot(&[laptop.clone()], 100 + 90_000)), "a day later: last_seen refreshed");
        for i in 0..10 {
            k.record(snapshot(&[laptop.clone(), mon("D2", &format!("X{i}"), 1920, false)], 100_000 + i));
        }
        assert_eq!(k.setups.len(), MAX_SETUPS);
        assert_eq!(k.setups[0].key, "L+X9");
        assert!(k.forget("L+X9") && !k.forget("L+X9"));
    }

    #[test]
    fn replacing_a_screen_renames_it_in_every_setup() {
        let laptop = mon("D1", "L", 0, true);
        let old = mon("D2", "GAME-1~a", 1920, false);
        let mut k = Known::default();
        k.record(snapshot(&[laptop.clone(), old.clone()], 10));
        k.record(snapshot(&[laptop.clone()], 20));
        let mut new = mon("D3", "GAME-2~b", 1920, false);
        new.model = "LG 27GR".into();
        // the old one is a candidate while the new one is connected
        let cands = old_screens(&k.setups, &[laptop.clone(), new.clone()]);
        assert_eq!(cands.iter().map(|c| c.0.id.as_str()).collect::<Vec<_>>(), vec!["GAME-1~a"]);
        assert_eq!(cands[0].0.gdi, "D2");
        assert!(seen_together(&k.setups, "GAME-1~a", "L"), "the laptop sat next to it");
        assert!(!seen_together(&k.setups, "GAME-1~a", "GAME-2~b"));
        assert!(k.replace_screen("GAME-1~a", &new));
        assert_eq!(k.setups.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), vec!["L", "GAME-2~b+L"]);
        assert_eq!(k.setups[1].displays[1].label, "LG 27GR");
        // seen as connected now: merged with the setup recorded since
        k.record(snapshot(&[laptop.clone(), new.clone()], 30));
        assert!(k.replace_screen("nothing", &new) == false);
        assert_eq!(k.setups.iter().filter(|s| s.key == "GAME-2~b+L").count(), 1);
    }

    #[test]
    fn plan_applies_only_on_a_change_to_a_linked_setup() {
        let mut cfg = crate::config::Config::default();
        cfg.setup_plans.insert("L".into(), 2);
        assert_eq!(plan_to_apply(&cfg, "L+X", "L"), None, "plan 2 is empty");
        cfg.plans.insert("2".into(), crate::plans::Plan::default());
        assert_eq!(plan_to_apply(&cfg, "L+X", "L"), Some(2));
        assert_eq!(plan_to_apply(&cfg, "L", "L"), None, "same setup");
        assert_eq!(plan_to_apply(&cfg, "L", "L+X"), None, "not linked");

        // the HP was replaced by another HP: same plan, marked inherited
        cfg.plans.insert("3".into(), crate::plans::Plan::default());
        cfg.setup_plans.insert("220E-3688~old+L".into(), 3);
        assert_eq!(linked_plan(&cfg, "220E-3688~new+L"), Some((3, true)));
        assert_eq!(plan_to_apply(&cfg, "L", "220E-3688~new+L"), Some(3));
        // its own link wins
        cfg.setup_plans.insert("220E-3688~new+L".into(), 2);
        assert_eq!(linked_plan(&cfg, "220E-3688~new+L"), Some((2, false)));
        // home and office, same model, different plans: ambiguous
        cfg.setup_plans.remove("220E-3688~new+L");
        cfg.setup_plans.insert("220E-3688~office+L".into(), 2);
        assert_eq!(linked_plan(&cfg, "220E-3688~new+L"), None);
        // another model is not a replacement
        assert_eq!(linked_plan(&cfg, "10AC-40F9~x+L"), None);

        // ...but it gets a "Keep plan" hint when one screen changed
        let s = |key: &str, seen| Setup { key: key.into(), displays: vec![], last_seen: seen };
        let mut c2 = crate::config::Config::default();
        c2.plans.insert("4".into(), crate::plans::Plan::default());
        c2.setup_plans.insert("GAME-1~a+L".into(), 4);
        let known = vec![s("GAME-1~a+L", 10), s("L", 5)];
        assert_eq!(replacement_hint(&c2, &known, "GAME-2~b+L").map(|h| (h.0, h.1.key.as_str())), Some((4, "GAME-1~a+L")));
        assert_eq!(replacement_hint(&c2, &known, "GAME-2~b+X+L"), None, "a screen added, not replaced");
        assert_eq!(replacement_hint(&c2, &known, "L"), None, "a screen removed");
        c2.setup_plans.insert("GAME-2~b+L".into(), 4);
        assert_eq!(replacement_hint(&c2, &known, "GAME-2~b+L"), None, "already linked");
    }
}
