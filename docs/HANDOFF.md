# ZoneLock — project state & handoff

Last updated: 2026-11-19 (WebView2 settings, packaging validated, git init).
Purpose: full context for the next working session.

## What ZoneLock is

Lightweight native Windows 10/11 utility: keeps maximized windows and
windowed (F11-style) fullscreen inside logical monitor zones instead of
expanding across a whole ultrawide — the FancyZones maximize problem.

Hard invariants (do not break):
- No DLL injection, ever. `WINEVENT_OUTOFCONTEXT` only.
- No admin rights required. Per-user installable, portable.
- No account, no telemetry, no runtime network. Local config only in
  `%APPDATA%\ZoneLock` (`config.json`, `zonelock.log`).
- Never steal focus: `SWP_NOACTIVATE`/`SWP_NOZORDER` on every placement.
- Exclusive fullscreen games are ignored (born-covering rule).
- Corrupt config must fail loud, never silently rewritten.
- Single static exe (~452 KB, `opt-level="z"`, lto, strip, crt-static).
  Idle private RAM ~1.7 MB.

## Tech

- Rust 2021, `windows` crate 0.62.x, `serde`/`serde_json` only other deps.
- Settings window = WebView2 (`webview2-com` 0.39, loader linked
  statically) hosting `src/web/settings.html`; contract in `uimodel.rs`.
  Editor overlay, tray, drag-snap stay hand-drawn Win32/GDI. GDI settings
  panel (`settings.rs`) is the fallback when WebView2 is unavailable.
- Git: branch `main`, first commit = pre-WebView2 state.

## Threads

- **Watcher thread**: WinEvent hooks (out-of-context) + message loop +
  periodic tick (~30 ms). Owns the drag-snap overlay windows and the
  window tracker state (`Ctx` thread-local).
- **UI thread**: tray icon/menu, zone editor overlays, settings window,
  global hotkeys.

## Module map

| File | Role |
|---|---|
| `src/rect.rs` `zones.rs` `config.rs` `presets.rs` `state.rs` `engine.rs` | Pure logic (unit-tested in-file) |
| `src/monitor.rs` `wininfo.rs` `place.rs` `watcher.rs` | Win32 layer |
| `src/dragzones.rs` | Shift-drag snap overlay + poll |
| `src/overlay.rs` | Zone editor (draw/move/resize/delete, magnetic snap) |
| `src/webui.rs` | Settings window host (WebView2, postMessage JSON) |
| `src/web/settings.html` | Settings page (HTML/CSS/JS, embedded) |
| `src/uimodel.rs` | Page <-> Config contract + validation (pure, tested) |
| `src/settings.rs` | GDI settings fallback (no WebView2 runtime) |
| `src/tray.rs` | Tray icon + menu + left-click → settings |
| `src/uidraw.rs` | Shared dark palette + GDI helpers |
| `src/hotkeys.rs` `autostart.rs` `icon.rs` `logo.rs` | Hotkeys, Run key, app icon/logo |
| `src/main.rs` | DPI awareness, singleton, UiCmd dispatch |
| `examples/probe.rs` | Diagnostics CLI (list/eval/max/min/drag/traycmd/...) |
| `tests/win_integration.rs` | Real-Win32 integration tests (serialized) |

## Done & validated on real windows

- **Maximize containment** — a window maximizing lands in the best-overlap
  zone with DWM invisible-border compensation (visible frame flush on
  zone/screen edges, like Aero Snap).
- **F11 windowed fullscreen** — full cycle on Chrome+Firefox real: cover →
  posted exit key (with retry on swallowed keys) → drop detected → strip
  `WS_CAPTION|WS_THICKFRAME` → borderless lock in zone → F11/Esc restores
  frame + exact normal rect. Per-exe exit keys (`fs_exit_keys`).
- **Shift-drag snap** — both directions (Shift held before OR pressed
  mid-drag), click-through zone overlay per monitor, live highlight,
  drop = snap, release Shift = cancel.
- **Zone editor** — draw zones, right-click delete, center dot move,
  8 resize dots with hover cursors, magnetic snapping (work-area edges,
  fractions 1/2..1/6, other zones' edges) with visible guides,
  double-buffered (no flicker), presets 1-7, Enter=save, Esc=cancel.
- **Settings window** — native chrome (`WS_CAPTION|SYSMENU|MINIMIZEBOX`,
  fixed size) + `DWMWA_USE_IMMERSIVE_DARK_MODE` dark titlebar, dark
  content: toggles, styled fields, owner-drawn buttons, Save → validate +
  write config + live hotkey rebind. Footer credit `@LasFar on X`
  (clickable, opens x.com/LasFar). PerMonitorV2 (manifest + API),
  WM_DPICHANGED relayout, AdjustWindowRectExForDpi for client metrics.
- **Tray** — left click opens settings/dashboard, right click menu
  (enable, edit zones, presets, hotkeys, autostart, config folder, exit).
- **Minimize respect** — placement functions refuse to touch `IsIconic`
  windows (daemon used to un-minimize snapped windows).
- **Work-area zones** — zones map onto `mon.work`, never under the taskbar.
- **Multi-monitor** — per-monitor layouts keyed by device name/index,
  hot-plug topology refresh, DPI-aware metrics.
- **First-seen maximized** windows contained (not misclassified as games);
  transient giant rects rejected as normal-state (`union(monitors)+64`).
- **WebView2 settings, Dispattern style** (`docs/UI.md`) — sidebar groups
  (Zones / Input / Apps / App) with colour-coded counts, instant apply +
  toasts, signal theme only, flat 2D logo matching the app icon. Per-display layouts,
  user presets (save current layout of a display under a name, use on any
  display, delete; included in the Next preset cycle). Shortcuts and exit
  keys recorded by pressing them; stored as virtual keys so any layout
  works (validated: CMS keyboard `VK_DD` shown as `Ç`); our own hotkeys are
  suspended while recording (validated: Win+Alt+E recorded, editor did not
  open). Quit button validated. Panel open = ~101 MB (see AGENTS.md).
- **Tray menu reduced** to Settings (default), Active (check), Quit.
- **Real fullscreen inside the zone** (2026-11-19): F11 and video
  (element) fullscreen stay real fullscreen, resized into the zone with
  SWP_NOSENDCHANGING. Validated on Chrome and Firefox (Store build):
  hold, exit back to the exact pre-fullscreen rect, Firefox's ~516px
  minimum width accepted. Replaces the exit-key + borderless path, which
  showed browser UI and fought user moves (kept as fallback only).
- **Plans 1-9** (2026-11-19): Alt+Shift+N go, Ctrl+Alt+Shift+N save
  (configurable, modifiers-only capture). Validated live: Alt+Shift+7 on
  a plan whose app was closed launched Character Map and placed it on the
  left half, other windows minimized, keyboard layout untouched;
  Ctrl+Alt+Shift+8 saved Terminal/Explorer/Chrome, Win+D, Alt+Shift+8
  brought all three back to the exact same rects with the right one in
  front. Not covered: UWP apps hosted in ApplicationFrameHost (Settings,
  Calculator) are skipped; launching relies on the exe path / AUMID, no
  command-line arguments are restored.
- **Reopen apps where you left them** (memory.rs, positions.json):
  validated with System Information (opens at [57,50,1863,975] by
  itself; moved, killed, reopened -> back at the moved spot).
- **Save plan from the keys with overwrite check**: validated live,
  focus never moved: free slot saved + card; taken slot asked (config
  untouched); same keys again overwrote; another number saved there;
  Esc cancelled.
- **Rightmost-zone overflow fixed**: apps with a minimum size larger
  than the zone (Chrome on a 480px quarter) slide back on screen and
  overlap the neighbour zone instead. Validated: Shift+drag snap to the
  last zone, Chrome F11, Firefox F11. Next-zone hotkey now uses the work
  area + apply_zone like the rest.
- **New preset / Edit preset** from the Displays page through the zone
  editor; drawings without a name are labelled "Custom" instead of
  keeping a preset name with different zones.
- **Packaging** — `package.ps1` -> portable zip + Inno `setup.exe` +
  SHA256SUMS. Silent install (with autostart task) / launch / uninstall
  cycle verified: Run key removed, WebView2 cache removed, config kept.
  `install.ps1` non-interactive flags (`-Autostart -NoLaunch -Uninstall`).
- **Child windows ignored** — `WS_CHILD` excluded from management.
- **Tests**: 40 unit + 7 integration, all green (as of this doc).

## Known limitations

- Apps with 100% custom titlebars that never enter the system move/size
  loop (Notepad Win11 style) don't emit `MOVESIZE*` → no Shift-drag
  overlay for them.
- `Win+Alt+arrows` are held by Windows 11 on the user's box (keyboard
  snap), so zone moves default to `Win+Ctrl+Alt+Right/Left` (free;
  Ctrl+Alt+letter avoided: AltGr on AZERTY). Two shortcuts with the same
  keys are refused by `uimodel::apply_settings`.
- Only one active layout per monitor at a time (switch via
  preset/hotkey).

## Remaining work

### P2 (leftover)
- Real video-player validation (VLC/mpv not installed): check they hold
  with SWP_NOSENDCHANGING like the browsers; if one fights past the
  budget it falls back to its exit key.
- Multi-display fullscreen release (Win+Shift+Arrow while held) is unit
  tested only: the test box had one display connected.
- **Chromium flash on click while held in a zone**: Chromium re-applies
  the monitor rect in WM_WINDOWPOSCHANGING on every activation / z-order
  change while fullscreen; we re-hold within ~5-7 ms (measured; bounded
  by Chrome's UI thread, a faster path in the watcher did not help).
  Firefox: no flash at all. Fixing it inside Chrome would need injection
  (forbidden). Owner's choice (2026-11-19): per-app "Stable" switch in
  Fullscreen apps (`config.fs_stable`): exit key + frameless in the zone,
  validated on Chrome (0 flash, F11/video/Esc cycles back to the exact
  rect). Default stays real fullscreen; nothing is Stable by default.

### P3 (leftover)
- Manual/visual validation of editor + settings at other DPI scales
  (125%/150% monitor wasn't connected this session).

### P4 — packaging (leftover)
- Demo GIF (`docs/demo.gif` referenced by README, not recorded yet).
- Clean-machine validation (fresh Win10 VM: SmartScreen, WebView2 present?).
- Pick the license (MIT or GPL-3.0) and add `LICENSE`.
- Exe grew 452 KB -> ~911 KB with WebView2; if size matters, check what
  dominates (loader static lib vs webview2-com generics).

### Missing features / backlog ideas
- Settings: custom zones still need the overlay editor (Draw zones button);
  a later step could edit zones directly in the display drawing. Advanced
  keys (`fullscreen_tolerance`, `max_refights`, ...) are config.json only.
- GDI fallback panel (no WebView2) has no editor/preset/quit buttons; the
  tray menu no longer has them either. Hotkeys still work there.
- No "zone picker on drop without Shift" option (FancyZones secondary
  mouse button style).
- No per-window "remember this app in this zone" rules.
- No startup toast/first-run wizard (editor discoverability).
- README/user docs beyond `docs/FEASIBILITY.md`.

## UI decision (done)

WebView2 chosen and shipped for Settings, styled with the Dispattern V2
system (owner call 2026-11-19). Spec: `docs/UI.md`. The GDI overlay
editor / drag-snap still use `uidraw.rs` (not yet Dispattern colours).

## Config keys (`%APPDATA%\ZoneLock\config.json`)

`enabled`, `shift_snap`, `zone_gap`, `log_enabled`, `autostart`,
`fullscreen_tolerance`, `max_refights`, `refight_window_ms`,
`exit_retry_ms`, `max_exit_sends`, `exclude_processes`,
`exclude_class_prefixes`, `hotkeys{toggle,editor,next_preset,next_zone,prev_zone}`,
`fs_exit_keys` (exe→key name), `layouts` (per monitor).

## Commands

- `cargo build` / `cargo build --release`
- `cargo test` — interactive desktop required; **kill `zonelock.exe`
  first** (the live daemon swallows test windows and poisons results).
- `powershell -File packaging\package.ps1` — dist zip (unverified).
- `examples/probe` helpers: `--traycmd N` post menu cmd, `--max|--min|
  --restore|--set hwnd`, `--drag hwnd x y [late|noshift]`, `--postf11`,
  `--postkey`, `--ovdrag`.

## Traps hit this session (don't re-step on these)

- `HTCLIENT=1`, `HTCAPTION=2` — swapped once (returned 9 = HTMAXBUTTON),
  broke dragging AND the close button.
- `segoe()`: points→pixels is `pt*dpi/72`, not `/96` (text was ~26%
  undersized = the "pixelated" look).
- Measure `DWMWA_EXTENDED_FRAME_BOUNDS` AFTER restoring a zoomed window:
  maximized reports ~8px all sides, restored ~7px L/R/B + 0 top.
- Never place minimized windows (`IsIconic` guard in `place.rs`).
- Zone canvas is `mon.work` (excludes taskbar), overlay origin is
  `mon.rect` — `zone_client_rect(z, work, origin)` handles both; hit
  tests must translate client→screen via `origin`.
- `wininfo::excludable` rejects `WS_EX_TOOLWINDOW` — can't use that style
  to hide test windows from ZoneLock.
- Log: `Track` over an already-Normal prev is skipped (was flooding on
  every LOCATIONCHANGE during drags); crude rotation at 1 MiB.
- `OnceLock<Box<dyn Fn + Send + Sync>>` for wndproc handler statics —
  a raw-pointer slot crashed the daemon once.
- `src/logo.rs` is `include!`d by `build.rs` — std-only, no `//!` inner
  doc comments.
- `webview2_com::*` glob-imports a `Result<T>` alias — spell
  `std::result::Result<_, String>` in `webui.rs`.
- CSS: `.row .t small` (display:block) beats a bare `.taken` class —
  scope state classes as `.row .t small.taken`.
