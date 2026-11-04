# Lumen

A Notion-style workspace written entirely in Rust with [egui](https://github.com/emilk/egui) — no web engine.
The same code runs as a native desktop app (Windows/macOS/Linux) and in the browser via WebAssembly.

## Features
- Block editor: text, headings, to-do, bullet/numbered lists, quote, callout, code, divider
- Markdown shortcuts (`# `, `- `, `[] `, `1. `, `> `, `---`), `/` slash menu, drag-to-reorder, block menu
- Page tree with favorites, sub-pages, trash/restore, covers and icons
- Kanban board pages
- Command palette (Ctrl/Cmd+K) with full-text search
- Password lock window (salted + iterated SHA-256), strength meter, idle auto-lock
- Animated light/dark theme, popups, sidebar, toasts
- Autosave (disk on native, localStorage on web)

## Shortcuts
`Ctrl/Cmd+K` palette · `Ctrl/Cmd+N` new page · `Ctrl/Cmd+\` sidebar · `Ctrl/Cmd+L` lock · `Ctrl/Cmd+Shift+L` theme

## Build
```bash
cargo run --release                         # native
cargo zigbuild --release --target x86_64-pc-windows-gnu   # Windows .exe from any OS
./build-web.sh                              # WebAssembly -> ../public/webapp
```
