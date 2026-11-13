# Lumen 13.1 — scrolling, typesetting, outline

## 1. Scrolling (`src/smooth.rs`)
**Bug:** one mouse-wheel notch made the page run away (a tiny scroll → a huge jump), most visibly in the browser.
**Cause:** Chrome / Edge report a wheel notch as one *pixel* delta (≈100). `take_wheel` treated every pixel delta as "touchpad", so the notch
became a finger velocity of `100 px / frame-time ≈ 6000 px/s`, and the touchpad-fling code then glided `v · 0.38 s ≈ 2300 px`.
**Fix:**
* Classification: small pixel deltas (< 40) or fractional lines = touchpad; a big pixel delta or a whole number of lines = wheel notch (spring).
  Big deltas *inside* a running touchpad gesture (a fast flick, 180 ms window) stay touchpad.
* A synthetic fling needs a real swipe (≥ 4 events), is capped at 2600 px/s and decays faster; **never** on WebAssembly (browsers send their own inertia).
* A wheel notch is capped at 420 px per frame. Regression test: `one_mouse_wheel_notch_is_never_a_touchpad_swipe`.
* Jumps to a heading (table of contents / outline rail) leave 20 px of air above the heading.
Structure review: the page scroll area, the dashboard and every nested area were checked — only the page and the dashboard use the spring;
horizontal areas (tables, boards, calendars) never see the vertical wheel; popups sit on a higher layer so `rect_contains_pointer` keeps them scrollable.

## 2. LaTeX (`src/math.rs`, `assets/XITSMath-Regular.otf`)
* Formulas are set in **XITS Math** (STIX-derived, OFL): real Mathematical Italic / Bold alphabets instead of a skewed sans font.
* Every box gets its **exact ink ascent / descent and advance** from the font (`ttf-parser`) instead of guessed proportions — scripts, fractions,
  radicals and big operators no longer collide; big operators are centred on the math axis by their real ink box; italic correction before scripts.
* Ordinary-size brackets are real glyphs (stroked curves only for tall delimiters). Fraction bars / rules are snapped to whole device pixels.
* Equation block: no box and no border while reading; the shaded surface appears **only while the source is being edited** (the blue outline is gone).

## 3. Table of contents (`src/editor.rs`)
* Nested headings have a toggle (chevron) – fold / unfold sub-sections, `+N` shows how many are hidden; the state is saved in the block.
* Tools: expand all, collapse all, text-colour picker (default + 9 colours); the heading currently in view is highlighted.

## 4. Outline rail (`src/outline.rs`) — Notion's "page outline" at the side
Slim bars on the page edge, one per heading (length = level), the current section highlighted. Hover opens the outline card (touch: tap),
click jumps, the card scrolls to the current section and closes 0.3 s after the pointer leaves.

## 5. Seamless top bar
`TopBottomPanel::show_separator_line(false)` + no stroke: the bar shares the page surface like Notion.

# Lumen 13.0 — math, universal text menus, file import, multi-block menu

## 1. Table block: ghost buttons fixed (`src/tableblock.rs`)
The toolbar below a table (`+ Row`, `Header row ✓`, `Convert to database` …) fades out when the pointer is away and no cell is selected.
The two *highlighted* buttons ("on" state: `Header row ✓`, `Convert to database`) painted their accent background **without** the fade alpha,
so two blue pills stayed visible under every table. `btn()` now multiplies every fill by the alpha, returns early when the toolbar is
invisible, and is inert (no hover, no click, no cursor) until it is mostly visible.

## 2. LaTeX / math (`src/math.rs`, new)
* **Equation block** (`BlockKind::Equation`, `/equation`, or typing `$$ ` at the start of a block): live 2-D typeset preview + TeX source field
  while focused. Wide formulas are scaled down to fit the page (phones).
* **Inline** `$…$` in any text block or table cell is shown typeset in Unicode (`x^2` → `x²`, `\alpha` → `α`, `\sqrt{x}` → `√x`,
  `\frac{a}{b}` → `a⁄b`); the stored text is never changed. Pandoc rules: `costs $5 and $10` stays text, `\$` is a literal dollar.
* **Engine**: tokenizer → parser → box layout → painter. Supports fractions (`\frac \dfrac \binom`), roots (`\sqrt[n]{}`), sub/superscripts, big operators with
  limits (`\sum \prod \int \oint \bigcup \lim \max …`), stretchy `\left … \right` delimiters, `matrix / pmatrix / bmatrix / vmatrix / cases / aligned / array`,
  accents (`\hat \bar \vec \tilde \dot \overline \underline \underbrace`), `\text`, `\mathrm \mathbf \mathbb \mathcal \mathtt`, Greek, ~150 symbols, spacing commands.
* **Markdown**: `$$ … $$` (one or several lines) imports as Equation blocks and exports back (also in clipboard copy and print/PDF).
* Shortcut `Ctrl+Shift+M` wraps the selection in `$…$` (again to unwrap).

## 3. Formatting shortcuts everywhere
* Page blocks: unchanged + `Ctrl+Shift+M`.
* **Every multi-line text field** (table cells, notes, prompts, descriptions, import box …): `Ctrl+B` bold, `Ctrl+I` italic, `Ctrl+U` underline, `Ctrl+E` code,
  `Ctrl+Shift+S` strikethrough, `Ctrl+Shift+H` highlight, `Ctrl+Shift+M` math — implemented once inside the vendored egui `TextEdit`
  (`vendor/egui/src/widgets/text_edit/builder.rs`, "Lumen patch"); pressing again removes the markers. Code / equation sources opt out.
* **Block selection**: the same shortcuts format *all* selected blocks.

## 4. Right-click / long-press menu in every text input
All text fields register themselves (vendored egui patch) → `widgets::universal_text_menu` opens **Undo · Redo · Cut · Copy · Paste · Select all**
(+ formatting entries in multi-line fields) on right-click, or on a long press on touch screens. Commands are executed by feeding the field the
standard egui events, so they behave exactly like the keyboard. Fields with their own richer menu (page blocks, titles, database cells) keep it.

## 5. Import from files
The Import dialog (new page / database) has **Markdown / text file…**, **CSV / TSV file…** and **Backup (.json)…** buttons using the native dialog
(Windows), the browser file picker (web) or the Storage Access Framework (Android), in addition to paste and drag & drop.

## 6. Multi-block context menu
Right-click or long-press on a block that belongs to a selection → one menu for the whole selection: copy / cut / duplicate / turn into / colour /
bold · italic · underline · strikethrough · code · highlight / indent / outdent / move / undo / clear / delete.

## Build outputs
`./build-all.sh` → `../public/webapp` (WebAssembly), `../public/downloads/Lumen-android-arm64.apk`, `Lumen-windows-x64.exe|zip`, `SHA256SUMS.txt`.
