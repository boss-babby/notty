# Pages and blocks

A doc page is a flat list of **blocks**; nesting is expressed with `indent` (0‑6): a block belongs to the closest
previous block with a smaller indent.

## Block types
`text`, `h1`, `h2`, `h3`, `todo` (has `checked`), `bullet`, `numbered`, `toggle` (children = indented blocks), `quote`,
`callout`, `code`, `divider`, `toc` (auto table of contents), `image` (`url`), `bookmark` (`url`), `embed` (`url`), `page_link` (`link` = page id).

## Markdown dialect accepted by `create_page.markdown`, `append_markdown`, `replace_page_content`
```
# Title            (first H1 of create_page becomes the page title)
## Heading 2       ### Heading 3
- bullet           * bullet            (two spaces of indentation = one nesting level)
1. numbered
- [ ] to-do        - [x] done to-do
+ toggle           (children indented below)
> quote            !> callout          ---  divider        [toc]  table of contents
![alt](https://…)  image               <https://…>  bookmark card      [[Page title]] link to a page (resolved by title)
```
Fenced code blocks (```` ```lang ````) become code blocks.
Inline formatting inside any text: `**bold**`, `*italic*`, `~~strike~~`, `` `code` ``, `==highlight==`, `[label](https://url)`.

## Tools
* `create_page` {title, parent?, icon?, markdown?, favorite?} → new doc page. `parent` accepts an id or a title.
* `append_markdown` {page, markdown, after_block?} · `append_blocks` {page, blocks:[{type,text,checked?,indent?,url?}]}
* `edit_block` {page, block, text?, type?, checked?} · `delete_block` {page, block}
* `replace_page_content` {page, markdown} — **destructive**, history keeps a version.
* `update_page` {page, title?, icon?, parent?, favorite?, full_width?}
* `trash_page` / `restore_page` — trash is recoverable.

## Good page patterns
* Meeting note: H1 title, callout with the goal, `## Agenda` bullets, `## Notes`, `## Decisions`, `## Action items` as to-dos.
* Project hub: callout (goal/owner/deadline), `[toc]`, linked database (see `databases`), `## Risks`, `## Links`.
* Wiki / docs: one parent page with a `toc`, children for each topic, `[[links]]` between related pages.


## Tables (v12)
GitHub-style Markdown tables are understood everywhere markdown is accepted (`create_page`, `append_markdown`, `replace_page_content`, pasted text):

```
| Task | Owner | Done |
|:-----|:-----:|-----:|
| Ship | Ann   | yes  |
```
They become a `table` block (cells are plain text with inline markdown). The user can convert a table block into a database with *Convert to database*.
For structured data prefer `create_database` (typed properties, relations, views); use tables for small, static, presentational grids.
