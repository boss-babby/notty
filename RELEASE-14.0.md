# Lumen 14.0 — local collaboration, borderless PDF cards, Android + web + Windows

* **Local collaboration** — share the workspace, one page, or a page with everything nested to devices on the same Wi-Fi: discovery,
  approval, roles, live cursors, annotations, attachments on demand. Entity CRDT (HLC + LWW map + fractional order), WebSocket transport,
  link relay for the web build. Design, protocol, security status and the next-pass task list: **[COLLAB.md](COLLAB.md)**.
* **People chip** next to the breadcrumb (avatars → drop-down with the people, `…`, `+`, follow, cursor switch, leave) and a
  Share / Nearby dialog with QR, code entry and the animated "finding" dots. Details live in hover cards.
* **PDF cards on the canvas are borderless**: the page fills the card edge to edge (the card follows the page's aspect ratio), a slim
  caption bar holds the file name, `page / total` and first / previous / next, and **annotations made in the reader are drawn on the card**.
* **Android** — new network permissions (Wi-Fi state, multicast, nearby devices); release APK signed, 16 KB aligned, thin-LTO profile.
* **Website** — `/` landing + downloads (APK, Windows exe/zip, checksums), `/app` = WebAssembly build, `/api/collab` = relay.
* Build everything: `./build-all.sh` (needs `tools/provision.sh` once; binary assets via `tools/fetch-assets.sh`, `tools/build-emoji-pack.py`).
