# Databases

A database is a page with `kind: database`. It has typed **properties** (columns), **views** (table, board, list, gallery,
calendar, timeline) and **rows** (pages). The row *title* is the built-in "Name" column.

## Property types
| type | value you send | notes |
|---|---|---|
| `text` | `"string"` | |
| `number` | `12.5` | |
| `select` | `"Option name"` | options are created on demand, so a new name is fine |
| `multi_select` | `["A","B"]` | |
| `checkbox` | `true/false` | |
| `date` | `"2025-03-31"` or range `"2025-03-01/2025-03-31"` | ISO dates only |
| `url` | `"https://…"` | |
| `relation` | `["Row title", …]` or row ids | `relation_to`: database id/title (defaults to itself) |
| `formula` | — | not settable; give `formula` expression on the property: Notion-style, e.g. `prop("Price") * prop("Qty")` |

## Tools
* `create_database` {title, parent?, icon?, view?: table|board|list|gallery|calendar|timeline,
  properties:[{name,type,options?,formula?,relation_to?}], rows?:[{title, values:{PropName: value}}]}
  The database starts **empty** (no default columns). Always include a `select` Status for boards and a `date` for calendars/timelines.
* `add_property` {database, name, type, options?, formula?} · `add_rows` {database, rows:[…]} (≤ 200 per call)
* `update_row` {row, title?, values?} · `query_database` {database, where?:{Prop:"text"}, search?, limit?}
* `add_view` {database, kind, name?, group_by?, filters?:[{prop,op,value}], sorts?:[{prop,asc}]}
  filter ops: contains, is, is_not, gt, lt, is_empty, not_empty, checked, unchecked, today, this_week, this_month, next_7, past_7, overdue.
* Rows are pages: `trash_page {page: rowId}` removes one; `read_page` on a row shows its body.

## Design tips
* Name the database by plural noun (Tasks, Contacts, Invoices). Put a Select for lifecycle (Status/Stage) first.
* Use relations instead of repeating text (Task → Project).
* Add a board view grouped by status, a calendar by date, and a table sorted by priority — one data set, several lenses.
* Show a database on a dashboard (`table`, `board`, `calendar` widgets) rather than copying data.
