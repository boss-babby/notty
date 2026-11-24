# Lumen 14.9 — mobile pass: bottom sheets, AI panel, keyboard toolbar, system bars

Scope of this round: Android first (the same Rust code also drives the WebAssembly and Windows builds).
Every item below was traced to a root cause in the source before it was changed; the changes are small and local.

| # | Area | Verdict before | Now |
|---|------|----------------|-----|
| 1 | Bottom sheet (⋯ page menu and every phone pop-up) | **Broken** — never slid, no exit animation, no re-open animation | Slides up 0.30 s (ease-out), down 0.22 s (ease-in), scrim fades with it |
| 2 | AI panel on phones | **Broken** — same clamp bug, backdrop and surface stopped short of the system bars | Travels from the bottom edge and back, covers the system bars, follows the keyboard smoothly |
| 3 | Keyboard toolbar → formatting tools (Bold, Italic …) | **Broken** — worked on the wrong text positions | Same engine as Ctrl+B: selection toggle or a real typing mode |
| 4 | Keyboard collapses when the options are used | **Bug** | Keyboard stays up while a sheet / the toolbar is used |
| 5 | Gap at the bottom of the screen | **Bug** (floating sheet, un-dimmed strips, mismatching strips under the drawer) | Edge-to-edge sheets, dimmed and filled system bars |
| 6 | Arrows | Up / Down fine; Left / Right stopped at block edges; no `->` typography | Left / Right cross blocks; `->` and `<-` become → and ← |
| 7 | Local discovery | Works (UDP broadcast + mDNS + Android NSD); three weaknesses found | Hardened (cached targets, logged failures); limitations documented |

---

## 1. Bottom sheet — appearing and closing

Where: `popup_area_on` in `src/widgets.rs`. On phones (`screen < 600 pt`, pop-ups ≥ 190 pt wide) every pop-up becomes a bottom sheet. The page ⋯ menu from your screenshot is one of them.

### What was wrong (deep analysis)

1. **The slide never happened.** The sheet was anchored with an off-screen offset `(1 - t) * 28`, but an egui `Area` keeps itself inside the screen by default (`constrain(true)`). The offset was clamped away, so the sheet only faded in for 0.16 s. Small as it looks, this is why it felt "okay but not smooth".
2. **No closing animation at all.** Almost every owner closes a menu by simply no longer calling `popup_area` (an item was tapped, `self.page_menu = None`). The sheet was removed in the same frame.
3. **The opening animation only ran once.** The `appear` timer is reset by `popup_area` only on its own close paths (scrim tap, Esc). When the owner closed the menu, the stale timer made the *next* opening skip the animation (`t = 1` immediately).
4. **The scrim stopped at `screen_rect`.** On Android `screen_rect` is shrunk by the status-bar, navigation-bar and keyboard insets, so the strips outside it were never dimmed. In your screenshot the status bar and the bottom band are bright while the page is dim.
5. **The sheet floated**: 6 px above the bottom, 10 px bottom corners, 12 px side gap. That is the visible gap under it; Notion's sheet is flush with the bottom and covers the gesture bar area.

### What changed

* `constrain(false)` on the scrim and the sheet; the sheet starts completely below the screen and travels up with `cubic_out` (0.30 s). The scrim fades with the same curve.
* **Exit animation for every close path**, including owner-driven ones: each frame the sheet's last paint list is remembered (`remember_sheet`); `play_sheet_ghosts` (called once at the end of `update`) replays a sheet that was not drawn in this pass sliding down with `cubic_in` (0.22 s) while the scrim fades, and resets the open timer so the next opening animates again. No caller had to change.
* The sheet is edge-to-edge (full width, 20 px top corners, square bottom) and extends under the navigation bar; its bottom padding includes the nav-bar height so the last row clears the gesture bar. The scrim covers status bar, content and navigation bar.
* A tap on the grab handle closes the sheet. (Drag-to-dismiss was deliberately not added: `touch.rs` turns a one-finger swipe into a scroll, so a drag on the handle would never arrive.)
* The block-type sheet above the keyboard toolbar (`Turn into` / `Insert below`) now slides 36 px and fades in (0.22 s) and resets properly.

---

## 2. AI panel on phones

Where: `aiui::fullscreen` and the `ai_mob_t` driver in `src/app.rs`.

* **Same root cause as the sheet**: the panel's `Area`s were not `constrain(false)`, so the off-screen start / end positions were clamped. Opening and closing therefore jumped through the last part of the travel instead of gliding. Fixed on the backdrop and the panel.
* The backdrop dims the whole window including the status and navigation bars; the panel surface continues under the navigation bar (flush with the bottom), top corners round off while it settles.
* **Keyboard**: Android changes `screen_rect` in one step when the keyboard opens or closes, so the chat visibly jumped. The panel height now follows it through `animate_value_with_time` (0.16 s).
* Driver kept: `animate_bool_with_time_and_easing(…, 0.28, cubic_in_out)`; closing no longer pops because the panel is rendered until `t` reaches 0.

---

## 3. Keyboard toolbar → formatting tools (Aa)

### Symptoms
Pressing Bold / Italic / … gave unpredictable results: stray `****` characters, tools that looked active when they were not, formatting that landed on the wrong words, and the text after the caret changing.

### Root cause
The editor never shows Markdown markers. While editing, a block is a `Rich` value (plain text + one `Fmt` per character, `src/rich.rs`) and is stored as Markdown only on change. The mobile toolbar (`apply_mobile_format`) ignored that model: it took the caret index of the *plain text* field and used it to splice `**`, `*`, `__`, `~~`, `` ` ``, `==` into the **stored Markdown string**. As soon as the block contained any other format, an escape (`\*`) or a link, plain index ≠ Markdown index and the markers landed in the wrong place. The "active" highlight (`caret_inside_mark`) tested `text.contains(mark)` anywhere before the caret, so `*` was "active" inside any `**bold**` and a closed format stayed "active" forever.

### Fix
* The toolbar now calls the same code as Ctrl+B / Ctrl+I on the desktop: `toggle_format` on the `Rich` model.
  * **With a selection** → toggled on exactly that text (off again if all of it already has the format; other formats are preserved).
  * **Caret only** → *typing mode*: nothing is inserted. The next characters are typed in that format and the following ones inherit it, until the tool is tapped again or the caret moves elsewhere. This is exactly "until that tool is off, everything I type has the format".
* `fmt_flag_active` drives the highlighted state from the real per-character format (selection: all characters; caret: pending mode, otherwise the character before the caret). Several tools can be on at once.
* Tapping a tool briefly takes focus (and with it the live cursor) from the text field, so the selection is recorded while the field still has it (`mobile_last_sel`) and restored afterwards.
* Code and equation blocks ignore the tools (they have no inline formatting).
* Regression tests: `mobile_format_tests` (typing mode never inserts markers; mode stays on across characters and switches off; selection toggles keep other formats).

---

## 4. "Option menu drops the keyboard"

Android's keyboard policy (`android::after_frame`) hides the keyboard half a second after no text field has focus. Two things made the field lose focus although the user was still editing:

1. Tapping any toolbar button or its sheet — egui clears the text focus on a press elsewhere.
2. Opening the ⋯ sheet (the field loses focus, the keyboard collapses, `screen_rect` grows and the page re-flows under the sheet).

Fix:

* While the toolbar or its sheet is being used, focus is handed straight back to the block being edited (`mobile_bar`), so the keyboard never notices.
* While a phone bottom sheet is on screen, the keyboard policy does not hide the keyboard (`widgets::sheet_active`); the usual half-second timer starts once the sheet has gone. The page therefore no longer jumps when the sheet opens.

---

## 5. The gap at the bottom of the phone

Cause (three parts):

1. The window is edge-to-edge but egui only owns `screen_rect` (inside the insets); the strip below was just the clear colour and was **not** dimmed by scrims.
2. The sheet floated 6 px above the bottom with rounded bottom corners (see §1).
3. With the navigation drawer open, the strip under it kept the page colour instead of the drawer colour.

Fix: scrims dim the full window (status bar and navigation bar included); sheets and the AI panel are flush with the physical bottom; `paint_system_bars` fills the strips with the page colour and with the drawer colour under the open drawer. `clear_color` already equals the page background, so the closed state is a single surface.

---

## 6. Arrows

Audit of the arrow handling in `src/editor.rs`:

* **Up / Down** in a text block: move inside the block's rows and cross into the previous / next block at the first / last row — correct; the slash menu and `[[` link suggestions take Up / Down first (and wrap) — correct.
* **Left / Right** stopped at the block edges (the caret never left the block with the keyboard). Fixed: ← at the start continues at the end of the previous block, → at the end continues at the start of the next one (skipping hidden and non-text blocks).
* **Typography**: typing `->` now becomes `→`, `<-` becomes `←` (not in code blocks, only when a character was typed, so deleting never re-triggers it; markers and formats are preserved because the replacement goes through `Rich::replace`). Not implemented on purpose: `<->` and `=>` (they collide with code-like prose).
* Test: `arrows_are_typed_as_symbols`.

---

## 7. Local discovery ("Nearby")

How it works (`src/collab`):

| Layer | Used on | Notes |
|-------|---------|-------|
| UDP broadcast, port 47616, `LMN1<json>` beacons every 2 s while hosting (`lan.rs`) | all | Works when routers / firewalls drop mDNS |
| mDNS / DNS-SD (`mdns.rs`) | Windows, Linux, macOS | |
| `NsdManager` `_lumen._tcp` (`CollabKit.java`) | Android | plus a `MulticastLock` held by the activity so Wi-Fi power-save does not drop broadcasts |
| Session transport | all | TCP 47615 / iroh; Noise_XX encrypted, so a spoofed beacon cannot read anything |

Verdict: **works**, and it is cheap when idle (the advertiser only runs while hosting, the scanner only while the *Nearby* tab is open). Findings:

1. `advertise()` rebuilt the target list — up to four throw-away UDP sockets — every 2 s. **Fixed**: built once, refreshed every 20 s (still follows a phone that roams to another Wi-Fi).
2. If port 47616 could not be bound the scanner thread returned silently, so *Nearby* simply stayed empty. **Fixed**: it is logged (`LAN discovery: UDP port … busy`); mDNS / NSD keep working.
3. **Known limitation (not changed)**: the directed-broadcast address assumes a /24 (`a.b.c.255`). On a /16 or /23 network the limited broadcast `255.255.255.255` is what finds peers; guest Wi-Fi with client isolation blocks all of it (use the join link / QR code, which goes through iroh). Reading the real netmask needs `getifaddrs`, a larger change than this round warranted.
* Tests: `broadcast_targets_always_include_the_limited_broadcast`, `foreign_datagrams_are_ignored`, plus the existing beacon round-trip.

---

## Comparison with the Notion mobile "Actions" sheet (your screenshots)

| | Notion | Lumen before | Lumen now |
|---|--------|--------------|-----------|
| Dimming | whole window, incl. status + nav bar | page only | whole window |
| Sheet edge | flush with the bottom, 24 px top corners | floating, 6 px gap, 10 px bottom corners | flush, 20 px top corners, nav-bar padding |
| Motion | slides up, slides down | fade only, vanishes | slide up / down, scrim fades |
| Title | centred "Actions" | none | centred "Actions" |
| Structure | rounded cards with hairline separators | one flat list with upper-case section labels | **still missing** |
| Typeface picker | Default / Serif / Mono tiles on top | – | **still missing** |
| Row height | ≈ 56 dp, large touch rows | compact | **still missing** (next pass) |
| Toggles | switch on the row (Available offline) | checkmark rows | **still missing** |
| Destructive action | red "Move to Trash" | – | – |

These remaining items are visual restructuring of the page menu itself (grouping, title, bigger rows, font tiles, switches) and are the recommended content of the next round.

---

## Files touched

`src/widgets.rs` (sheet engine, ghosts, system bars) · `src/aiui.rs` (phone panel) · `src/app.rs` (drawer backdrop, ghost playback) · `src/editor.rs` (toolbar formatting, arrows, focus retention, tests) · `src/android.rs` (insets, keyboard policy) · `src/collab/lan.rs` · `Cargo.toml` (14.9.0).

## Deliverables

* WebAssembly build — `/app` (alias `/webapp/`), served from `public/webapp`.
* Android APK — `/downloads/Lumen-android-arm64.apk`, arm64-v8a, `android` profile (release, thin LTO, stripped), v2 + v3 signed, 16 KB aligned.
* Windows x64 — `/downloads/Lumen-windows-x64.exe` and `.zip`.
* `SHA256SUMS.txt` next to the downloads.
* Reproduce: `notty/setup-android-env.sh` once, then `notty/build-all.sh`.

## Verification

VERIFICATION_PLACEHOLDER
