# Canvas (infinite whiteboard)

A `canvas` page holds **nodes** at world coordinates (x → right, y → down, 1 unit ≈ 1 px at 100 %) and **edges**.

## Node kinds
`note` (card, markdown inside), `text` (frameless label), `todo` (check-list card with progress), `page` (live link card to a page: set `page`),
`link` (bookmark: `url`), `image` (`url`), `group` (labelled frame — also a slide in presentation mode).
Colours: `1` red, `2` orange, `3` yellow, `4` green, `5` cyan, `6` purple (omit for none).

## Tools
* `create_canvas` {title, parent?, icon?, nodes?, edges?} · `canvas_add` {page, nodes, edges} · `read_page` lists nodes.
* node: `{ "id":"a", "kind":"note", "text":"# Idea\n- point", "x":0, "y":0, "w":260, "h":140, "color":"4" }`
* edge: `{ "from":"a", "to":"b", "label":"depends on", "dashed":false }` — `from`/`to` are the *local ids you chose in this call*.
* Layout: cards 240–320 wide, ≥ 40 gap. Mind maps: root at (0,0), children in columns 360 apart.
  Kanban-like: groups as columns at x = 0, 340, 680 … with notes stacked 160 apart inside.

## When to use a canvas
Brainstorms, architecture diagrams, roadmaps, user journeys, mind maps, relationship maps — anything spatial.
For data use a database; for process use a flow.
