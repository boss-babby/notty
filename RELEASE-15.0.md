# Lumen 15.0 — side peek, PDF → note, portable vault, Argon2id

Platforms: Windows x64 · Android arm64 (APK, release-signed, 16 KB page aligned) · WebAssembly (served at `/app`).
Everything below is the same Rust code on all three. Plan for what is still open: **NEXT-PASS.md**.

## What is new

| Area | Change | Code |
|------|--------|------|
| Databases | A row now opens in a **right-hand side peek** (slides in, header: close · open as page `Ctrl+Enter` · previous / next row · breadcrumb · "always full page" toggle, `Esc` closes). Row context menu: *Open in side peek* / *Open as page*. Palette: *Open rows: side peek ⇄ full page*. Windows narrower than 820 pt (phones) still open the full page. | `peek.rs`, `database.rs`, `app.rs` |
| PDF | **→ Note** in the annotation panel: creates "Highlights — <title>" with every highlight / underline / strike quote, comments, sticky notes and drawings, grouped by page, each with a **↩ p. N** deep link (`lumenpdf://…`). Clicking it re-opens the reader on that page with the annotation selected. | `extract.rs`, `pdfnotes.rs`, `pdf.rs` |
| Portability | **Export vault as Markdown ZIP** (palette, storage manager): one `.md` per page, YAML front-matter (title, icon, id, parent, timestamps, database properties), folder tree, CSV per database, `assets/` for pictures and attachments, relative links. **Import → ZIP vault**: Notion "Markdown & CSV" export or an Obsidian vault (folders → nested pages, front-matter titles, `![[img.png]]`, CSV → database, pictures → media). | `vault.rs`, `io.rs` |
| Security | Vault password is derived with **Argon2id** (19 MiB, 2 passes). Older SHA-256 credentials keep working and are re-written as Argon2id the next time the password is typed. | `auth.rs`, `lock.rs` |
| Storage | **Storage manager** (palette): size of pages / page history / attachments, counts, biggest attachments, *Prune history* (keeps named versions and the newest 5 snapshots per page), shortcuts to export and back up. | `vault.rs`, `app.rs` |
| Mobile | Touch rows 50 dp (60 dp with subtitle); **switch rows** for Small text, Full width, Favorite, Dark mode in the ⋯ sheet. | `widgets.rs::menu_switch` |
| Site | Next.js front page with the embedded WebAssembly app (`/app`), APK / EXE / ZIP / source downloads and checksums. | `../src/app/page.tsx`, `../next.config.ts` |

## Tests added

`extract` (deep-link round trip, escaping, grouping), `vault` (CRC-32 vector, ZIP round trip with size limit, Notion ids, front-matter, ISO dates,
export → import round trip, history pruning), `auth` (Argon2id round trip, legacy credential upgrade path, old JSON without `kdf`), `peek` (phones / preference).

## Known limits

See NEXT-PASS.md §1 — notably: the peek shares the editor state of the page behind it (typing there is not in that page's undo stack),
`lumenpdf://` links resolve only for PDFs attached as file blocks, and Notion-specific blocks import as plain Markdown.
