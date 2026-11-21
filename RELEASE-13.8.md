# Lumen 13.8 — crisp widgets, true circles, real equations in HTML / PDF

## 1. Dashboard widgets looked like a blurry picture
**Cause.** Widgets placed on the whiteboard (`src/canvasdash.rs`) are painted once at 100 % and the resulting shapes are then scaled with a
`TSTransform`. egui's `Shape::transform` scales the *already rasterised* glyph quads of every text — i.e. it stretches a bitmap. Zoom in, or use a
3× phone screen, and the text and Lucide icons of a widget are as soft as an image.

**Fix.** `vendor/epaint/src/shapes/shape.rs`: when a text shape is transformed with a scale ≠ 1, the galley is **laid out again at the final font
size** (`crisp_text`), so glyphs are rasterised for the pixel grid they end up on. The fonts are registered per frame by `vendor/egui/src/context.rs`
(layer transforms of interactive layers) and by `canvasdash` (zoomed cards). Widgets are therefore live, crisp content at any zoom level.

## 2. Circles were polygons
**Why.** A GPU only draws triangles, so every curve is a polyline. egui used a *fixed* number of corners (8 / 16 / 32 / 64 / 128 by radius in points,
irrespective of pixel density) and Lumen's own ring code (`dashcharts::sector`) used one chord per 6 px of arc and no anti-aliasing; whiteboard ellipses
were 56-gons. Large on screen — big monitor, 3× phone, zoomed card — the chords were visible. The card zoom also multiplied the chord length.

**Fix — a circle's corner count follows its size in *physical pixels*** (`src/crisp.rs`): `Δ = 2·acos(1 − s/R)` with the sagitta `s` = 0.06 px.
* donut / pie / gauge / timer rings (`dashcharts::sector`): exact arcs + a one-pixel feathered edge (anti-aliased), zoom-aware;
* whiteboard ellipses, pills and rounded corners (`canvas::geo_points_px`); SVG export writes real `<ellipse>`;
* egui itself (`vendor/epaint/src/tessellator.rs`): circles above 40 px radius are generated with as many corners as needed instead of at most 128.

## 3. LaTeX in the HTML / PDF export
The print export flattened every formula to one line of italic Unicode. `math::to_mathml` now converts the same AST that the on-screen typesetter uses
(fractions, roots, scripts, big operators with limits, stretchy delimiters, matrices / cases / aligned, accents, over/under-set) to **MathML Core**.
Block equations and inline `$…$` (Pandoc rules: "costs $5 and $10" stays text; code spans untouched) are typeset natively by Chrome, Edge, Firefox,
Safari and the Android WebView — in the HTML file *and* in the PDF printed from it. A subset of XITS Math (OFL, with the OpenType MATH table,
`assets/xits-math-print.woff2`, 200 KB) is embedded **only when a page contains formulas**, so Android (which has no math font) looks like desktop.
Also: Markdown table cells keep their formatting and formulas; dashboard widgets print as a labelled card instead of a bare placeholder.

## Builds
`./build-all.sh` → WebAssembly (`../public/webapp`), signed arm64 Android APK and Windows x64 exe (`../public/downloads`), `SHA256SUMS.txt`.
The Next.js site (`../src`) serves `/app` (WebAssembly), `/downloads/*` and the `/api/preview`, `/api/ai-proxy`, `/api/flow-proxy` helpers (SSRF-guarded).
