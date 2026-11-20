# Swift UI style parity — the desktop shell carries the Mac app's look

Reference: `Sources/DigUpApp/**` on branch `swift-reference` (the original Swift app, Abrahim's
styling). Target: the Tauri desktop shell (`shell/ui/**`) on Windows, Linux and macOS — one
consistent look, the Mac app's. The docker server UI is explicitly OUT of scope (user decision
2026-11-20: "the compose server already looks how I want it"). Icon: the existing chain
(`shell/src-tauri/icons/`: `icon.icns`, `icon.ico`, `icon.png`, 32/128 pngs) stays as-is; the
bundlers (`nsis` / `deb+rpm+appimage` / `.app`+`.dmg`) already wire it.

Everything below is copied digit-for-digit from the Swift source at `swift-reference`. A shell
value is *parity* when it maps to these; the gap list (bottom) is what U2 closes.

## 1. Brand palette — ALREADY EXACT in `shell/ui/window.css`

| Token | Swift (`Components.swift:9-13`) | Hex | Shell (`window.css:5-9`) |
|---|---|---|---|
| amber | `Color(red: 0.949, green: 0.635, blue: 0.165)` | `#F2A22A` "the D" | `--amber: #f2a22a` ✓ |
| ember | `Color(red: 0.851, green: 0.482, blue: 0.102)` | `#D97B1A` | `--ember: #d97b1a` ✓ |
| moss | `Color(red: 0.208, green: 0.400, blue: 0.322)` | `#356652` darkest tile | `--moss: #356652` ✓ |
| leaf | `Color(red: 0.310, green: 0.502, blue: 0.345)` | `#4F8058` | `--leaf: #4f8058` ✓ |
| sprout | `Color(red: 0.635, green: 0.780, blue: 0.557)` | `#A2C78E` light tile | `--sprout: #a2c78e` ✓ |

Brand meaning (`Components.swift:5-6`): amber marks what DigUp finds (accent icons); moss is
brand moments (onboarding glow, tiles); selection/controls follow the system accent.
Highlight yellow (previews): text `Color(red: 1, green: 0.86, blue: 0.2)` = `#DDB`-family —
shell `--mark: #ffdb33` (check exact usage, see gaps).

## 2. Type scale (Swift `.system(size:, weight:)` — the sizes the app actually uses)

| Size | Weight(s) | Where |
|---|---|---|
| 30 | bold | onboarding hero title (`Onboarding.swift:358`) |
| 20 | bold, semibold | empty-state/hero ("Choose folders to search" 20 bold, `SearchWindow.swift:424`); panel title 20 semibold (code 17, `SearchPanelView.swift:30`) |
| 18 | regular | onboarding feature line (`Onboarding.swift:375`) |
| 17 | semibold | search field, code mode (15 in code mode, `SearchWindow.swift:371`); panel title 17 |
| 16 | regular, medium | onboarding body 16 (`:361`), row title 16 medium (`:401`) |
| 15 | semibold, regular | "No good matches" 15 semibold (`SearchPanelView.swift:66-79`); field/rows 15 |
| 14 | semibold | onboarding row title 14 (`Onboarding.swift:406`) |
| 13 | regular, semibold | body 13 (`SearchPanelView.swift:79`, `Components.swift:204`); status line 13 semibold (`SettingsWindow.swift:436`) |
| 12 | regular, medium | onboarding example/caption 12 (`:381`, `:258`); row example 12 |
| 11 | regular, medium, semibold | captions, chips, meta 11 (`SearchPanelView.swift:114`, `Components.swift:107`); badges 11 semibold |
| 11.5 | medium | accent chip text (`SearchPanelView.swift:178`) |
| 10 | semibold | badges (`Components.swift:53`, `SearchPanelView.swift:224`); `.monospacedDigit()` |

Monospaced digits: every numeric count/chip uses `.monospacedDigit()` (`Components.swift:53,107`,
`SearchPanelView.swift:46`) → CSS `font-variant-numeric: tabular-nums` on counts.

## 3. Shapes and fills

- **Window**: `RoundedRectangle(cornerRadius: 18, .continuous)` (`SearchPanelView.swift:23`,
  `Snapshot… cornerRadius: 18`) → 18 px, the window radius; panel glass uses the same radius
  (`SearchPanel.swift:535-543`, `NSVisualEffectView` + layer cornerRadius).
- **Search field**: radius 10, fill `Color.primary.opacity(0.06)`, focus ring
  `Color.accentColor.opacity(0.6)` stroke width 2, padding h12 (`SearchWindow.swift:386-391`).
- **Pill/chip (the brand chip)** (`Components.swift:28-38`): base `size` — font size `size`
  medium **design: .rounded**; pad h `size*0.45` / v `size*0.28`; radius `size*0.42`; fill
  `primary.opacity(0.07)`; border `primary.opacity(0.14)` width 0.5.
- **Capsule badges**: 10 semibold monospaced, pad h6 v2, fill quaternary / on-image
  `black.opacity(0.6)` (`Components.swift:53-57`); progress capsule fill
  `primary.opacity(0.12)`, on-image `white.opacity(0.3)` (`:76`).
- **Field-ish panel**: radius 9, fill `primary.opacity(0.05)` (recording 0.03)
  (`Components.swift:204-209`); onboarding row tile radius 9 fill `Brand.tile` (`Onboarding.swift:404`).
- **Onboarding glow**: `shadow(color: Brand.moss.opacity(0.35), radius: 14, y: 6)`
  (`Onboarding.swift:356`).
- **Onboarding dots**: active `primary.opacity(0.55)`, inactive `0.15` (`:307`).
- **Empty-state block**: fill `primary.opacity(0.025)`, padding 16, inner spacing 14
  (`SearchWindow.swift:347-356`).

## 4. Spacing rhythm (Swift `spacing:`/`padding:`)

Field row: HStack 10, inner 9, h-pad 12, window h-pad 14, bottom 10, top pad 18/24
(`SearchWindow.swift:367-434`). Panel: title row h-pad 18; chips h10 v3 (capsule)
(`SearchPanelView.swift:48-54`); blocks spacing 8, meta 11 at h16 (`:114-115`). Onboarding:
h48 top46 hero; rows spacing 14/12/16 at 20-30 (`Onboarding.swift:234-389`). Settings: label
stacks spacing 2, rows spacing 12, caption 11 (`SettingsWindow.swift:212-492`).

## 5. The shell's current state

`shell/ui/window.css` line 1: *"Sizes and wording follow the Mac app's search window
(Sources/DigUpApp/SearchWindow.swift)"* — the palette (1) is already exact; the token names
(`--pane: rgba(0,0,0,0.025)`, `--fill: 0.06`, `--fill-strong: 0.10`, `--line: 0.09`,
`--tile: 0.05`, `--chip: leaf@0.1`) map onto §3. Remaining work is per-rule parity + the
cross-OS chrome, below.

## 6. Gap list (U2 closes these) — measured 2026-11-20 against `shell/ui/*.css`

Measured facts: type scale in use — 10, 11, 11.5, 12, 12.5, 13, 14, 15, 16, 17, 18, 20, 22,
24, 30 (+8.5, +10.5); radii — 2,3,4,5,6,7(×12 window.css),8(×4 panel.css),9,10,12,18,99;
focus ring `window.css:88` = `0 0 0 2px accent@60 %` — **exact §3 parity**; tabular-nums
present on counts (`panel.css:115,209,295`, `settings.css:42`); fills via tokens
`--pane 0.025 · --fill 0.06 · --fill-strong 0.10 · --line 0.09 · --tile 0.05` — §3 parity.

Confirmed parity (no work): §1 palette · §3 field ring · tabular-nums · token fills.
Chip radii 7/8 map onto the §3 chip formula (font 17 → radius 17·0.42 = 7.14; 20 → 8.4):
the dominant 7/8 values ARE the brand chip computed — keep.

Gaps to close:

- [x] **G1 off-scale type** → map onto the §2 scale: 8.5→10, 10.5→11, 12.5→12 (6 sites:
      `panel.css:307,339,351`, `window.css:357,582,659`), 22→20 (`panel.css:103`),
      24→20 (`setup.css:48`, `window.css:341`). Hero stays 30 where used as the app's
      hero title (`panel.css` scale already includes 30 ✓).
- [x] **G2 primary-button color split**: `setup.css:119` + `settings.css:8` primary =
      `--moss`, while `window.css:393,587,642` primary/selected = `--accent`. Swift rule
      (`Components.swift:5-6`): controls follow the **system accent**, moss is brand
      moments. → controls in onboarding/settings move to accent; moss stays on brand
      surfaces (glow, tiles, progress fill `setup.css:109` is a brand moment — keep).
- [x] **G3 fills hard-coded**: `rgba(0,0,0,…)` literals (0.16, 0.18, 0.3, 0.35, 0.42, 0.45,
      0.5, 0.6, 0.78, 0.85) bypass the §3 token ladder — tokenize over `--fill*/--pane/
      --tile` where a spec value exists (0.16 = accent row tint `window.css:542` is a
      literal of `--amber` alpha 0.16 — keep with comment), keep overlays (scrim 0.45) as
      literals with a named token.
- [ ] **G4 window chrome (the cross-OS "consistent" headline)**: no `transparent`/
      `decorations`/`shadow` keys in ANY tauri conf (measured: zero hits in base +
      `.macos/.windows/.linux` confs) → all three OSes ship the native system window today.
      Swift's look is the 18 px rounded **glass** window (`SearchPanel.swift:535-543`
      NSVisualEffectView + radius 18, `SearchPanelView.swift:23`). Plan: macOS
      `transparent: true` + `transparent` CSS surface with `backdrop-filter: blur()`;
      Windows keeps native 11 rounded corners (parity by the OS); Linux native CSD —
      the rounded glass everywhere needs `decorations: false` + a CSS-drawn titlebar,
      which is the largest sub-project (needs its own gate: drag, resize, snap).
- [x] **G5 focus ring consistency**: `settings.css:61` ring = `--moss@50 %` vs the
      §3 parity ring `accent@60 %` (`window.css:88`) → settings adopts the parity ring.
- [x] **G6 onboarding specifics**: glow `moss@0.35 r14 y6` (`Onboarding.swift:356`) and
      dots 0.55/0.15 (`:307`) — `window.css:336` radial (amber→leaf) is the app's
      onboarding glow shape ✓; dot alphas verified where onboarding renders in
      `setup.css` (U2 pass checks each dot site).
- [ ] **G7 dark mode**: Swift `Color.primary/.secondary` auto-adapt; the shell CSS has no
      `prefers-color-scheme` block (measured: zero hits) → add the inverted token block
      (Swift `primary` fills become white-alpha on dark; the app's dark look is the same
      radii/scale with flipped alphas).

## 7. Verification

`cargo test -p digup-shell`-equivalent: the shell's own suite (`shell/tests`); visual proof:
serve `shell/ui` in a headless Chromium (playwright MCP on this box) and screenshot
panel/results/settings/setup on light+dark, side-by-side with rendered Swift facts
(type-scale sheet, radius sheet). Packaging gate: `packages.yml` chain — NSIS exe,
deb/rpm/appimage, `.app`+`.dmg` — icon chain unchanged, so artifact checks ride as-is.
