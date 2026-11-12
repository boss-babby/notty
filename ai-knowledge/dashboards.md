# Dashboards

Any page can become a dashboard: it keeps its blocks and gains a 12‑column grid of **widgets** (like Notion dashboards,
but with real charts). Multi-page systems = several dashboard pages under one parent, linked from a hub page.

## Tools
* `create_dashboard` {title, parent?, icon?, intro_markdown?, widgets:[…]} → creates a page *and* its dashboard in one call.
* `add_widgets` {page, widgets:[…]} · `list_widget_kinds` · `remove_widget` {page, widget} · `read_page` shows the widgets.

## Widget spec
```
{ "kind": "bar", "title": "Tasks by status", "span": 6, "rows": 3,
  "database": "Tasks", "group": "Status", "value": "Hours", "agg": "sum",
  "bucket": "month", "date": "Due", "range": "last30", "stack": "Owner", "goal": 100, "limit": 8,
  "cols": ["Status","Owner"], "sort": "Due", "asc": true, "text": "…", "items": ["a","b"], "target": "2026-01-01", "palette": 0 }
```
* `span` 1‑12 columns (12 = full width), `rows` 1‑8. Defaults come from the kind. Fill a row to 12 (e.g. 3+3+3+3 or 6+6 or 4+4+4).
* `database`/`group`/`value`/`date`/`x`/`y`/`stack`/`sort`/`cols` accept **names** (database title or id; property names).
  Anything omitted is auto-filled from the database (first select = group, first number = value, first date = date).
* `agg`: count, filled, unique, sum, avg, min, max, median, percent. `bucket`: day, week, month, quarter, year.
* `range`: all, today, last7, last30, last90, last365, this_week, this_month, this_quarter, this_year, next7, next30.

## Kinds
* **Overview**: `stats`, `tasks`, `upcoming`, `recent`, `favorites`, `activity`, `subpages`
* **Charts**: `number`, `gauge`, `bar`, `hbar`, `stacked`, `line`, `area`, `pie`, `donut`, `funnel`, `radar`, `histogram`, `scatter`, `heatmap`
* **Data**: `table`, `list`, `board`, `calendar`, `progress`
* **Personal**: `notes`, `checklist`, `countdown`, `timer`, `today`, `quicklinks`, `pagepreview`, `quickadd`

## Layout recipe
Row 1: 3–4 `number` KPIs (span 3 or 4, rows 1). Row 2: one or two charts (span 6, rows 3). Row 3: a `table` or `board`
(span 12, rows 4). End with `notes` for how to use the dashboard. KPIs need `agg` + `value`; a `goal` turns them into progress.
