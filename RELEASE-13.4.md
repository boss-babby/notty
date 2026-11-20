# Lumen 13.4 — resizable page tables, intuitive column / row controls, Markdown paste that lands correctly

## Page tables (`src/tableblock.rs`)
* **Resize** — drag the border between two columns (double-click = fit to the widest cell), drag a row's bottom border for a minimum height (double-click = automatic),
  drag the grabber in the bottom-right corner to scale the whole table (proportions kept; double-click = fit to page width), or use **Fit width** / **Equal columns** in the toolbar.
  Sizes are stored in the block (`Block::cols`, `"w1,w2|h1,h2"`), survive copy / history / sync of the workspace, and never change the Markdown export.
* **Handles** (inspired by Affine / Notion tables) — a grip above every column and left of every row. *Click* → menu: insert left / right or above / below, move, duplicate,
  align left / centre / right, sort A→Z / Z→A, fit to contents, equal widths, fit to page, header row, automatic height, clear contents, delete. *Drag* → reorder, with a live insertion line.
  Hovering a grip highlights the whole column / row; the column / row a menu belongs to stays tinted.
* **`+` strips** at the right and bottom edge add a column / row in one click. The existing toolbar (+ Row, + Column, − Row, − Column, align, sort, header row, copy Markdown, convert to database) is unchanged.
* **Touch** — grips, borders and the corner grabber get larger hit areas and are always visible on touch screens.

## Markdown paste (`src/clip.rs`, `editor.rs`)
* Text pasted from anywhere is classified by its *syntax*: headings (#‥######, setext), bullets / numbers / to-dos (any nesting: 2 spaces, 4 spaces or tabs), quotes, GitHub callouts (`> [!NOTE]`), fenced code (``` and ~~~),
  `$$ … $$` equations, images, tables (with alignment), horizontal rules, front matter (kept as a code block), `<details><summary>` toggles, HTML inline tags (`<b> <i> <u> <s> <mark> <code> <a href> <br>`), `:emoji:` shortcodes, entities.
* CommonMark meaning is respected: `__bold__` and `_italic_` (not Lumen's own underline), soft line breaks join wrapped paragraphs, two trailing spaces / a backslash keep a hard break, list items keep their continuation lines;
  `snake_case_words` and `2 * 3 = 6` are left alone.
* Placement: caret at the start → blocks go above; at the end → below; in the middle → the paragraph is split (head, pasted blocks, tail); an empty block is replaced; plain lines pasted into a list continue the list.
  A single line of inline Markdown (`**bold** text`) is inserted at the caret with its formatting. A lone URL keeps its "turn into bookmark" behaviour; plain text stays plain.

Tests: `tableblock` (10, including mouse-driven drags through the real widget), `clip` (8), `tests::pasting_markdown_splits_the_paragraph_and_formats` (editor level).
