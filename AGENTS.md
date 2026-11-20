# ZoneLock agent notes

Windows tool: keep maximize + windowed fullscreen (F11) inside monitor
zones. Rust + windows-rs, single static exe, no admin, no DLL injection.

## Layout of the code

- `src/rect.rs`, `src/zones.rs`, `src/config.rs`, `src/presets.rs`,
  `src/state.rs`, `src/engine.rs` — pure logic, unit-tested in-file.
- `src/dragzones.rs` — Shift+drag snap overlay (FancyZones-style):
  click-through zone windows shown between EVENT_SYSTEM_MOVESIZESTART
  and MOVESIZEEND, owned by the watcher thread. Note: apps with fully
  custom titlebars that bypass the system move loop (e.g. Win11
  Notepad) never fire MOVESIZE — no overlay for them.
- `src/monitor.rs`, `src/wininfo.rs`, `src/place.rs`, `src/watcher.rs` —
  Win32 layer. The watcher runs on its own thread with a message loop
  (required for `WINEVENT_OUTOFCONTEXT` delivery) and a thread-local Ctx.
- `src/tray.rs`, `src/overlay.rs`, `src/hotkeys.rs`, `src/autostart.rs`,
  `src/settings.rs`, `src/icon.rs`, `src/logo.rs`, `src/uidraw.rs`,
  `src/main.rs` — UI thread, tray menu, overlay editor (draw / move /
  resize via handle dots, magnetic snap to fractions + zone edges with
  visible guides, double-buffered paint), custom dark settings panel
  (borderless, pill toggles, owner-drawn buttons), Run key. `logo.rs`
  is `include!`d by build.rs to emit the .ico — keep it std-only and
  free of `//!` docs. `uidraw.rs` holds the shared dark palette and
  GDI helpers.
- `src/webui.rs` + `src/web/settings.html` + `src/uimodel.rs` — the
  Settings window (tray left click) is a WebView2 page compiled into the
  exe (`include_str!` + `NavigateToString`, fonts injected as data URIs).
  Visual system = Dispattern V2, spec in `docs/UI.md` (read it before
  touching the page). Every change applies immediately: page ->
  `uimodel::PageMsg` (apply / use_preset / save_preset / new_preset /
  edit_preset / delete_preset / capture / close / quit / open_*), host -> `{type: state|saved|sync|error}`
  (`error` carries `{field, message}`). `uimodel` is pure + tested
  (`build_state`, `apply_settings`, `use_preset`, `save_preset`,
  `delete_preset`); all validation lives there. WebView2 exists only while
  the window is open (Controller.Close on WM_DESTROY). `settings.rs` (GDI)
  is the fallback when the runtime is missing or init fails — keep it
  compiling.
- New / Edit preset: the page names the preset, `overlay::open_for_preset`
  opens the editor with a pending (monitor, name); Enter stores the
  drawing through `uimodel::commit_editor_layout` (pending name -> user
  preset; free drawing -> name of the identical preset or "Custom") and
  `close_all` calls `webui::refresh()` so the panel updates.
- Fullscreen is held in a zone ONLY when the window sat in its zone
  just before (`engine::fs_entry`: normal rect fills the zone within
  `ZONE_FIT_TOL` = 16 px of invisible borders via
  `resnap::fills_zone_within`, or it was maximize-contained). A
  free-floating window going fullscreen (Parsec / RDP connecting, games,
  players moved out of their zone) -> `Ignore` = whole display.
- Fullscreen (engine `ContainFsMove` / `LockKind::ZoneFsMoved`): the app
  STAYS in its own fullscreen and `place::hold_fullscreen` resizes it into
  the zone with `SWP_NOSENDCHANGING` (plain SetWindowPos is undone by
  Chromium in WM_WINDOWPOSCHANGING). Release = caption comes back vs the
  style captured at hold time (`orig_style`), then `RestoreNormal` to the
  pre-fullscreen rect. Other display -> released. Chromium apps flash on
  every click while held (they re-apply the monitor rect on any
  activation/z-order change; ~5-7 ms, not fixable without injection), so
  per-app `config.fs_stable` (Stable switch in Fullscreen apps) sends
  them down the exit-key + frameless path instead: no flash, browser
  bars visible. That path is also the fallback past the refight budget.
  The F11/Esc keyboard hook resolves the foreground window's root owner
  (Chrome bubbles forward F11 to the browser window).
- Plans (`src/plans.rs`, page "Plans", config `plans` "1".."9",
  hotkeys `plan_go` / `plan_save` = modifiers only, digit 1-9 added,
  registered as ids 101-109 / 201-209): save = visible app windows
  (Alt+Tab-like filter, ours and excluded programs out) with exe, full
  path, AUMID for Store apps, class, title, display + work-area
  fraction, z-order. Go = `plans::assign` (pure, tested: same exe, class
  then title preferred) -> restore + apply_zone + z-order, top one
  focused; missing programs launched (AUMID via shell:AppsFolder, else
  path) and placed by `plans::claim_new_window` from the watcher's
  EVENT_OBJECT_SHOW (20 s window); every other app window minimized.
  Save from the keys goes through `planprompt.rs` (native GDI card,
  bottom-centre, topmost, WS_EX_NOACTIVATE + MA_NOACTIVATE: never takes
  focus): free slot -> "Plan N saved"; taken slot -> question holding the
  plan captured on the first press (`plans::save_step`, pure/tested):
  same keys again = overwrite, another number = save there, Esc
  (temporary hotkey owned by the card) / click / 7 s = cancel.
- Reopen where left (`src/memory.rs`, config `remember_positions`,
  default on): the watcher remembers the visible rect (work-area
  fraction) of every Normal / maximize-contained resizable app window,
  keyed `exe|class`, in `%APPDATA%\ZoneLock\positions.json` (cache,
  separate from config.json; flushed on window destroy + exit, only if
  dirty; unreadable file -> never overwritten). On EVENT_OBJECT_SHOW of a
  window the tracker has not seen, unless a plan launch claimed it, it is
  put back there - skipped if another window of the same exe|class is
  open, not resizable, zoomed, covering a display or excluded.
  Plans and remembered spots keep their display by GDI name;
  `plans::target_rect` falls back to the PRIMARY display when that
  display is gone (never "same index", which is another screen).
- Display changes (`src/resnap.rs`, pure/tested + watcher
  `display_changed` / `resnap_pass`): Windows squeezes/moves windows
  BEFORE the watcher sees the new geometry, so those moves look like
  user moves. Every window in a zone (`resnap::fills_zone`: exact, or
  min-size overflow <= 1/3 on one axis) keeps a Snap (display + zone);
  leaving the zone only marks it `lost`. The tick compares rect + work +
  name + dpi; on a change, snaps lost < 3 s ago are re-applied to the
  same zone on the new geometry (re-checked every tick for 2.5 s),
  fullscreen/frameless locks get their target recomputed (+ restore
  rect rescaled). Display gone -> orphan snaps, re-applied when it
  returns. Validated 2026-11-19 with live resolution changes on both
  displays (test via ChangeDisplaySettingsEx flags 0, then reset with
  ChangeDisplaySettingsEx(NULL,...) - a per-device revert does NOT
  restore the display arrangement).
- Unsnap on drag (`src/unsnap.rs`, pure/tested `Book` behind a Mutex,
  used by the UI thread hotkey and the watcher): Shift+drag snap,
  next-zone hotkey and maximize-contain record the pre-snap size + the
  rect placed on (zone -> zone keeps the first size). The first real
  move of a drag that started on that rect (same size, new position; a
  resize drag changes size and is ignored) resizes the window to the old
  size under the cursor. The system move loop re-applies the size it
  captured at drag start, so the watcher sends WM_CANCELMODE (ends the
  loop), SetWindowPos, then posts WM_SYSCOMMAND 0xF012 (SC_DRAGMOVE) to
  restart the move while the button is still down. Validated with
  Firefox + a WinForms window.
- Hotkeys: `Win+Alt+arrows` are held by Windows 11 keyboard snap (it
  answers them with halves/thirds) - never test ZoneLock with them. Zone
  moves default to `Win+Ctrl+Alt+Right/Left` (next/prev). Avoid
  Ctrl+Alt+letter defaults (AltGr on AZERTY etc.). Probing availability
  with RegisterHotKey must run IN the interactive session (a child
  `pwsh -File` saw everything as free) and mind PowerShell's
  case-insensitive variables (`$m` is `$M`).
- Min-size overflow: an app whose minimum width/height is bigger than
  the zone (Chrome ~500px, Firefox ~516px) grows right/down off the
  display on the last zone. `place::keep_on_screen` slides it back
  inside the work area after every placement (apply_zone,
  hold_fullscreen, lock_borderless), and the engine emits
  `KeepOnScreen` when a held/frameless window grows later by itself.
- Shift+drag snap: `dragzones.rs` tracks every eligible MOVESIZESTART;
  the overlay opens/closes live with Shift (held before OR pressed
  mid-drag) and drops snap only while the overlay is visible.

## Commands

- `cargo build` / `cargo build --release`
- `cargo test` — needs an interactive desktop session: integration tests
  create real windows (serialized via a static mutex).
- Hotkeys/exit keys are stored as names or `VK_XX` (any layout):
  `hotkeys::vk_from_name` / `vk_to_name` / `binding_label`.
- `powershell -File packaging\package.ps1` — dist zip + Inno setup.exe
  (if ISCC found, per-user install at `%LOCALAPPDATA%\Programs\Inno Setup 6`)
  + SHA256SUMS. Validated 2026-11-19: silent install/uninstall cycle.
- windows-rs 0.62: many handles take `Option<HWND>`/`Option<HDC>`;
  `SetWinEventHook` lives in `Win32::UI::Accessibility`;
  `SetCapture`/`ReleaseCapture` in `Win32::UI::Input::KeyboardAndMouse`;
  `SelectObject`/`DeleteObject` want `HGDIOBJ` (use `.into()`).

## Invariants

- No injection, ever. `WINEVENT_OUTOFCONTEXT` only.
- Never activate or steal focus: `SWP_NOACTIVATE` on every placement.
  Single exception: going to a plan (user hotkey / panel button) brings
  the plan's top window to the front - that is the feature.
- Windows born covering a monitor are ignored forever (exclusive
  fullscreen games).
- Fail loud on corrupt config; never silently rewrite user files.
- Network: ONLY `update.rs` (WinHTTP GET to api.github.com latest release
  of OhLasFar/ZoneLock + that release's github.com asset links; generic
  User-Agent, no user data; daily, `config.check_updates`, or on click).
  Downloads are verified against the release's SHA256SUMS.txt
  (`update::sha256_hex`). Installed copy (unins000.exe beside the exe) ->
  setup `/VERYSILENT /update=1` (Inno `[Run] Check: IsUpdate` relaunches);
  portable -> exe swap (`.old`) + `--after-update <pid>`. Releases must
  keep shipping `zonelock-X-setup.exe`, `zonelock-X-win-x64.exe` and
  SHA256SUMS.txt (package.ps1) or older copies cannot update.
- No telemetry, config under `%APPDATA%\ZoneLock` only
  (WebView2 user data in `%APPDATA%\ZoneLock\WebView2`). The settings
  page must stay self-contained: CSP `default-src 'none'`, every
  navigation away from data:/about: is cancelled, no external assets.
- Only top-level windows are managed: `WS_CHILD` is excluded in
  `wininfo::excludable` (WebView2/Chrome child widgets fire WinEvents).
- Shared handler/statics for wndprocs: use `OnceLock<T>` with
  `Send + Sync` — a raw-pointer `AtomicIsize` handler slot crashed the
  daemon on first WM_COMMAND.

## UI / DPI notes

- The exe is PerMonitorV2 via manifest (`build.rs`) + the API call in
  `main.rs`. Font scaling: `uidraw::segoe(pt, w, dpi)` converts points
  with `pt*dpi/72` - never `/96` (that made text ~26% too small).
- Settings window sizes from `GetDpiForWindow` at WM_CREATE and handles
  `WM_DPICHANGED` (relayout children). Left click on the tray icon opens
  it (`WM_LBUTTONUP` in `TRAY_MSG`); right click shows the menu.
- The settings window uses NATIVE chrome (`WS_CAPTION|WS_SYSMENU|
  WS_MINIMIZEBOX`, no thick frame) + `DWMWA_USE_IMMERSIVE_DARK_MODE`
  (attr 20) for a dark titlebar. Client metrics must go through
  `AdjustWindowRectExForDpi`, never raw `sx(W)`/`sx(H)` on the outer
  rect. Do NOT reintroduce a borderless custom caption: hit-test codes
  are `HTCLIENT=1`/`HTCAPTION=2` (a previous build returned 9/2 swapped,
  which broke dragging and the close button).
- `place::apply_zone` measures `DWMWA_EXTENDED_FRAME_BOUNDS` AFTER
  restoring zoomed windows: a maximized window reports ~8px invisible
  border on all sides, a restored one ~7px L/R/B and 0 top. Measuring
  pre-restore misplaces the visible top edge. Integration tests assert
  on `visible_rect` (extended frame bounds), not `GetWindowRect`.
- Tests: a running `zonelock.exe` daemon intercepts test windows -
  kill it before `cargo test` and restart after.

## Measured (2026-11-19)

- release exe: ~1.05 MB (452 KB before WebView2; +95 KB embedded fonts)
- idle private RAM: ~1.7 MB; ~3.4 MB after the settings window was
  opened once (loader/COM DLLs stay mapped; WebView2 processes exit)
- settings window open: ~101 MB private across our WebView2 processes
  with `--disable-gpu --disable-software-rasterizer` (161 MB default;
  `--disable-gpu` alone spawns a ~70 MB SwiftShader GPU process). Opens
  in ~380 ms warm, ~2.4 s on the first launch after a flag change.
