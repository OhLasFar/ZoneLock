# ZoneLock UI system

The settings window (`src/web/settings.html`) follows the **Dispattern V2
design system** (source: `DispatternV2/plans/DISPATTERN_V2_DESIGN_SYSTEM.md`
+ `src/views/globals.css`). This file is the ZoneLock subset: every new
screen or control uses these tokens and components. If something does not
fit, extend this file first, then the page.

## Theme and tokens

Dark only, **signal theme only** (owner call 2026-11-19: no theme picker).

| Token | Value | Job |
|---|---|---|
| `--dp-bg` | `#08090c` | window |
| `--dp-void` | `#050608` | drawings, mini layouts |
| `--dp-card` | `#101218` | cards, inputs |
| `--dp-elevated` | `#171a21` | hover, kbd, toasts |
| `--dp-muted-bg` | `#1c2029` | switch off, muted badge |
| `--dp-fg` | `#eceef2` | text |
| `--dp-muted` | `#8b909c` | hints, labels |
| `--dp-primary` | `#3b82f6` | accent, on-state, focus |
| `--dp-primary-fg` | `#0b1222` | text on primary |
| `--dp-accent` | primary 16% | active nav, selected wash |
| `--dp-ice` | `#93c5fd` | info badges, links |
| `--dp-border` / `--dp-input` | white 8% / 10% | hairlines, field borders |
| `--dp-ok` `--dp-warn` `--dp-danger` | `#34d399` `#fbbf24` `#f43f5e` | status |

Logo: flat 2D mark, no tile or glow: dark navy rounded square
(`#0b1222`), tall left zone and bottom-right zone in `--dp-primary`,
top-right zone in `--dp-ice`. Same shape and colours as the app/tray icon
(`src/logo.rs`).

Type: Figtree (body), Syne (titles, brand), JetBrains Mono (eyebrows,
badges, keys, numbers). Embedded as woff2 data URIs at runtime by
`webui.rs` (no network); licences in `src/web/fonts/OFL.txt`.

Radius: 6 (badge, kbd) · 8 (controls) · 12 (cards). Controls are 44px
high (`--ctl`), table cells 36px. Card ring = `0 0 0 1px white/8%`.

Background: Dispattern body recipe only (top glow + 48px grid), static.
Not used on purpose: fog drift, grain, blur, sheen. They cost CPU on
low-end PCs for no gain in a settings window.

## Colour coding

- **Status badges** (tinted bg 15% + coloured mono text): `ok` ready /
  active / yours, `warn` taken / paused, `bad` errors, `ice` counts and
  primary display, `promo` built-in preset, `muted` neutral facts.
- **Zones**: each zone index has a fixed colour on every drawing and mini
  layout: blue, emerald, amber, violet, rose, cyan, orange, fuchsia.
- **Nav counts** carry the same badge kinds (e.g. `1 TAKEN` in amber on
  Shortcuts).

## Components (templates in the page)

- **Page head**: eyebrow (mono caps, primary) = nav group, Syne title,
  muted sentence.
- **Card**: `t-card`, header (Syne 16 title + badges, muted desc,
  actions right) and body (padded or `flush` for rows/tables).
- **Tile**: bordered switch tile, two per row (Dispeek recipe).
- **Row**: label + hint left, controls right, hairline between rows.
- **Switch**: 44×24 pill, primary when on.
- **Key box**: click to record, shows keys as `kbd` caps. Stores the
  Windows virtual key, so any layout works (`VK_DD` shows as `Ç` on CMS).
  While recording a global shortcut, ZoneLock's own hotkeys are
  suspended so they can be recorded too. Esc cancels.
- **Buttons**: `primary` (filled + glow), `outline`, `ghost`, `danger`;
  `sm` (34px) in card headers; `.press` scale on click.
- **Fullscreen apps table**: Program | Exit key (key box) | Stable
  (switch) | remove. Stable = frameless, no flash, browser bars visible.
- **Table**: mono caps heads, borderless cells that show a field on hover
  or focus, `×` removes, `+ Add …` in the card header.
- **Display card**: header = name + size/primary/layout-kind badges +
  `+ New preset` (primary). Body: preview on top (max 440px), then preset
  chips below: "Your presets" first, then "Built-in". A "Custom" layout
  (drawn, unnamed) shows a note with `Save as preset`.
- **New preset flow**: name field + `Draw zones` -> editor opens with
  `New preset "name"` in its hint -> Enter saves the preset and applies it
  to that display. `Edit` on a user preset reopens the editor on it.
- **Preset chip**: mini layout + name; current one has a primary ring;
  user presets have their name in `ok` and a star tile in the top-right
  corner.
- **Toast**: bottom right, dot coloured by result. Every change that is
  applied gets one (`Saved`) or an error toast and a red field.

## Behaviour

- Every change applies at once. Text fields apply on Enter or blur.
- A refused change keeps the field red and toasts the reason; switches
  flip back to the saved value.
- Esc closes the window (unless recording a key or naming a preset).
- The tray menu only holds Settings, Active and Quit. Everything else is
  in the panel.
