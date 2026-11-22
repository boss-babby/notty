# Lumen 14.8 — collaboration pass 9

See COLLAB.md §15 for the design. Headlines:
* Big tables (`lumen-store`) are part of a session: bucket digests, chunk transfer, tombstones, live edits, role gates, collision-free row ids.
* Automatic host fail-over: the editors elect the next host when the host vanishes (`Standby` plan, per-device caps, secret rotation on kick).
* Faster reconnects (a refused dial counts immediately), symmetric-NAT detection skips the doomed direct punch.
* Pairing core for Bluetooth hand-off (`collab/pair.rs`, tested); push relay on the site (`/api/collab/push`).
* The website (Next.js, `../src`) serves the WebAssembly app at `/app` and `/webapp`, the Android APK and the Windows build at `/downloads`, and the
  relay / rendezvous / signalling / push / stats routes under `/api/collab`.

## Verification
```
cargo test --lib                       # 318 pass; 1 pre-existing UI failure (right_click_keeps_selection…, also fails on the untouched checkout)
cargo test --lib soak -- --ignored     # 1 M rows, see below
bash build-all.sh                      # web (wasm32, profile web) + Android (arm64 APK, signed) + Windows (x86_64-pc-windows-gnu)
```
