# Dashboards in Lumen 5.1

## 1. Research: what is a "dashboard" in Notion?

Notion has **no dashboard object**. A "Notion dashboard" is an ordinary *page* on which people place

* linked database views (table, board, calendar, gallery …) — with their own filters and sorts,
* charts (a chart is just another view type of a database),
* columns, callouts, buttons, embeds, to-do lists — and normal text,

and it behaves like every other page: it sits in the page tree, can be nested, favourited, shared, has an icon and a cover,
and pages are created *from* it through the database views it contains.

## 2. What was wrong in Lumen 5.0

| Problem | Cause |
|---|---|
| A page could not be a dashboard | `Workspace.dash` was one global list of boards, drawn only by the Home screen. Widgets had no way to belong to a page. |
| No pages could be created from a dashboard | The only creation entry points were two hard-coded Home buttons and the *Quick add* widget. |
| Dashboards were invisible to the workspace | Nothing in the page tree / favourites / backlinks knew that a dashboard existed. |
| Plots were 14 separate widgets | Each plot type was its own widget kind, with its own gallery card; changing a type lost the context of the settings. |
| Icons missing | See section 4. |

## 3. What 5.1 does

* **Any page can be a dashboard** — `Board.page` (page id, `0` = a Home tab). The editor draws the board's widget grid between the
  page header and the blocks (`dashboard::embedded`). Entry points: page menu → *Make this page a dashboard*, the `/dashboard`
  slash command, and the *New dashboard* card on Home. The page keeps its icon, cover, text, sub-pages and history; removing the
  dashboard (*Layouts → Remove dashboard*) leaves the page untouched.
* **Create pages from the dashboard** — the new **Pages** widget lists the children of any page (or the top level), with
  *New page* and *New dashboard* buttons. Dashboards created from it become real sub-pages (`cfg::new_dashboard_page`).
* **One Chart widget** — kind `"chart"` with the plot type stored in `cfg.extra["plot"]`
  (bar, h-bar, stacked, line, area, pie, donut, funnel, radar, histogram, scatter, heat-map, gauge, number).
  The inspector's **Plot type** picker switches the type and shows exactly the settings that plot understands
  (`dashwidgets::def_for` resolves name / icon / options / draw function from the current plot). Old workspaces are migrated by
  `dashcfg::migrate_plots`; `Cfg` is unchanged. The gallery shows one *Chart* card.
* **Polish** — hero header, quick-start cards, tab + toolbar on one row, icon chips and soft shadows on cards, hover outline,
  designed empty state, edit-mode banner; everything uses the theme palette (light, dark, accent colours) and adapts to 1 / 6 / 12 columns.

## 4. Icons

Three independent causes:

1. The dashboard drew pictograms (full-width plus, white square, sun, four-pointed star, triangles, ballot boxes, check mark, vulgar
   fractions) with the text font. Inter has no such glyphs, so they rendered as boxes or not at all. They are now Lucide glyphs
   (`l::PLUS`, `l::DATABASE` …), drawn in-line with the label.
2. `lucide::FIGMA` and `lucide::YOUTUBE` pointed at glyphs that Lucide 1.x removed (brand icons). They now map to `frame` / `circle-play`.
3. `assets/*.otf|ttf` were not in the repository. `tools/build-fonts.py` rebuilds them: Inter without its private-use alternates (they
   would shadow Lucide) and a Lucide font containing exactly the 268 referenced glyphs (100 KB instead of 850 KB). The script **fails** if a
   constant points to a missing glyph; the tests `every_lucide_icon_exists_in_the_embedded_font`,
   `dashboard_code_uses_icon_font_not_symbol_characters` and `icons_render_in_proportional_text` keep it that way.

## 5. Extending

```rust
register(Def { kind: "invoices", name: "Open invoices", desc: "…", cat: Cat::Data, glyph: l::WALLET,
               span: 6, rows: 3, opts: &[Opt::Db, Opt::Filters, Opt::Value, Opt::Range], draw: draw_invoices });
```

The gallery card, settings panel, drag & drop, resizing, persistence, pinning and *page dashboards* work for it automatically.
