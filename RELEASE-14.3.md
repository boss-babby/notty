# Lumen 14.3 — collaboration pass 4 + delivery

* **Concurrent text merge**: two people typing in one paragraph keep both edits (three-way merge with per-field rules, symmetric ⇒ convergent).
* **Trusted devices**: persistent device key, approve once, *Forget* to revoke.
* **Comments & @mentions** on pages and blocks, threads, resolve, toasts; synced as `Key::Comment`. Protocol 3.
* **Tombstone GC** (10 min grace; memory of long sessions stays flat).
* **Android**: camera QR scan (`CollabKit`, pure-Rust `rqrr` decoder), NSD discovery/registration (`_lumen._tcp`), CAMERA permission is optional and asked on tap.
* **Website** (`../src`): landing page with downloads (+ SHA-256), the WebAssembly app embedded at `/app`, the collaboration relay at `/api/collab`.
* Build everything: `./build-all.sh` (web → Android APK → Windows exe) → `../public`. Missing binary assets are produced by `tools/fetch-assets.sh`
  (emoji pack from `emoji-datasource-google`, XITS Math from aliftype/xits, `fonttools` for the woff2).
* Remaining work and the review: COLLAB.md §10.
