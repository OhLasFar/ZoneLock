# P0 — Note de faisabilité ZoneLock

Date : 2026-11-17. Verdict : **faisable sans admin et sans injection DLL.**
La voie est prouvée par des tests d'intégration exécutés sur la machine réelle
(`cargo test --test win_integration` : un `SetWinEventHook` out-of-context
capture un vrai `SW_MAXIMIZE` et la fenêtre est reconfinée dans sa zone).

## API retenues

- `SetWinEventHook` avec `WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS`
  (thread dédié, message loop obligatoire pour la livraison des events).
  Events : `EVENT_OBJECT_LOCATIONCHANGE`, `EVENT_SYSTEM_MOVESIZEEND`,
  `EVENT_OBJECT_SHOW/HIDE/DESTROY`, `EVENT_SYSTEM_MINIMIZEEND`.
- Inspection : `GetWindowRect`, `IsZoomed`, `GetWindowLongPtr` (style/exstyle),
  `GetClassName`, `QueryFullProcessImageName`, `DwmGetWindowAttribute(CLOAKED)`.
- Correction : `ShowWindow(SW_RESTORE)` si zoomée, puis
  `SetWindowPos(SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER)`.
- Moniteurs : `EnumDisplayMonitors` + `MONITORINFOEXW` + `GetDpiForMonitor`.

## API écartées (et pourquoi)

- `WH_CBT` global (`SetWindowsHookEx`) : exige une DLL chargée dans chaque
  processus = injection. Exclu.
- `WM_GETMINMAXINFO`, `WM_SYSCOMMAND/SC_MAXIMIZE` : messages par fenêtre,
  inaccessibles sans sous-classement = injection. Exclus.
- `SPI_SETWORKAREA` : ne marche que sur l'écran primaire, une seule zone,
  effets de bord sur la barre des tâches. Écarté.

## Conséquence de la voie non-injection

On ne peut pas veto le maximize avant qu'il ne peigne : on corrige juste
après l'event, dans le même tick. Sur machine normale c'est sous la frame ;
le pire cas documenté est un flash d'une frame. C'est le prix honnête de
« pas d'injection », assumé dans le README.

## Détection F11 / plein écran fenêtré — validée en conditions réelles

Heuristique géométrique : `rect ≈ rcMonitor` **ou** `rect ≈ rcWork`
(tolérance 8 px par défaut, configurable), sans `WS_MAXIMIZE`. La machine à
états exige que la fenêtre ait été vue « normale » avant : une fenêtre qui
naît en recouvrant l'écran est classée `Ignored` pour toujours tant qu'elle
couvre un moniteur. C'est ce qui rend l'outil passif sur les jeux en
exclusive fullscreen lancés directement.

### Le mécanisme de confinement (prouvé sur Chrome et Firefox réels)

Constat clé : `SetWindowPos` seul ne tient **pas** sur un Chromium en F11
(l'app ré-affirme son plein écran à chaque fois — ping-pong infini). La
voie non-injection qui fonctionne, démontrée de bout en bout :

1. Fenêtre trackée en état normal → F11 → elle recouvre son moniteur.
2. `PostMessage(WM_KEYDOWN/UP, exit_key)` postée à la fenêtre : la touche
   de sortie est résolue par processus (`chrome.exe` → F11, `vlc.exe` → f…,
   table `fs_exit_keys` de la config). Prouvé : Chrome et Firefox quittent
   leur plein écran interne sur un simple `PostMessage`, sans vol de focus.
3. L'app retombe en fenêtré → on retire `WS_CAPTION | WS_THICKFRAME` et on
   la cale en plein écran dans sa zone (`ZoneFsBorderless`), styles
   d'origine sauvegardés.
4. Sortie : hook clavier bas niveau `WH_KEYBOARD_LL` (out-of-process, pas
   d'injection — le callback tourne dans notre thread). F11 ou Esc sur la
   fenêtre verrouillée → restauration des styles + du rect normal exact,
   F11 avalé pour ne pas re-basculer l'app en plein écran.

Robustesse ajoutée après essais réels :

- Le premier `PostMessage` peut être avalé par l'app pendant sa transition
  d'entrée en fullscreen → `ZoneFsExitSent` ré-évalue périodiquement et
  reposte (délai configurable, 3 essais max puis `Ignore`).
- Certaines apps traversent une géométrie transitoire plus grande que le
  bureau virtuel entier (observé sur Firefox : 4864 px > 4480 px) : un
  rect hors de `union(monitors) + 64 px` ne remplace jamais un `NormalState`
  existant.
- Apps sans touche de sortie connue : un essai de déplacement coopératif
  (`ZoneFsMoved`) ; si l'app ré-affirme, `Ignore` — même résultat passif
  qu'un jeu exclusif.

Évidence : log `%APPDATA%\ZoneLock\zonelock.log` du 17/11/2026 —
`fs-exit-key → lock-borderless → key-release` sur `chrome.exe` et
`firefox.exe`, avec retry effectif quand le 1er post a été avalé.

## DPI et multi-écrans

- `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` au démarrage.
- Zones stockées en fractions 0..1 du moniteur : le layout survit aux
  changements de DPI et de résolution.
- Un layout par moniteur (clé = nom de device GDI, fallback = index
  gauche→droite, fallback final = layout `default`).
- Re-énumération toutes les 2 s : hot-plug et changement de résolution
  gérés sans `WM_DISPLAYCHANGE`.

## Coexistence FancyZones

PowerToys FancyZones n'intercepte ni Agrandir ni F11 (issue #279 ouverte
depuis 2019). ZoneLock corrige la géométrie post-event ; FZ agit au drag et
sur ses propres raccourcis. Pas de conflit attendu : les deux peuvent
tourner ensemble. Si un conflit apparaît sur une app, `exclude_processes`
dans la config la retire du périmètre ZoneLock.

## Exclusive fullscreen (limite documentée)

Hors scope par design. Impossible à distinguer d'un F11 borderless de façon
fiable sans sonde plus profonde ; la règle « né en plein écran = ignoré »
couvre le cas réel (le jeu démarre déjà en fullscreen). Si un jeu repasse
en fenêtré puis refait un F11 borderless, il peut être confiné comme un
lecteur : le retirer via `exclude_processes`. Pas d'injection pour
« contourner » l'exclusif : antivirus/anti-cheat.

## Dépendances

`windows-rs` (bindings officiels Microsoft), `serde` + `serde_json`. CRT
statique (`+crt-static`). **Zéro runtime à installer**, exe unique.

## SmartScreen / signature

Binaire non signé → SmartScreen « Windows a protégé votre PC » au premier
lancement (More info → Run anyway). Mitigations appliquées : version
resource (ProductName/FileDescription via `winresource`), nom stable,
distribution reproductible. Réputation = temps. Un certificat OV/EV est une
décision LasFar (coût, identité).

## Recommandation langage

**Rust + windows-rs** (retenu et implémenté). Exe release : **~452 Ko**,
RAM privée au repos : **~1,7 Mo** (mesuré le 17/11/2026, debug du tray +
hooks actifs, `enabled=false`). vs C++ : même perf, plus de surface
d'erreur mémoire et plus de code colle. vs C# : self-contained ~15 Mo+ ou
runtime requis ; NativeAOT possible mais écosystème Win32 moins direct.

## Config

Un seul fichier : `%APPDATA%\ZoneLock\config.json` (créé au 1er lancement,
`config.example.json` dans le repo). Pas de compte, pas de cloud, pas de
télémétrie, aucune requête réseau au runtime.
