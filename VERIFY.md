# Verification log

Machine: Linux x86-64 (Debian 12), 4 vCPU, 3.9 GB RAM, no phones, no Bluetooth, outbound UDP limited. Rust 1.100.0, JDK 17,
Android SDK 34 + NDK 26.1.10909125, mingw-w64, wasm-bindgen 0.2.130.

## Rust

```
rustfmt --check src/collab/{textcrdt,access,dht,bigsync,mod,net}.rs   # the files of this pass: clean
cargo fmt --check                             # NOT clean at HEAD: the repository is written in a wider hand style and was never rustfmt-clean
                                              # (e.g. src/agent_build.rs); the 17 files this pass touched are formatted with rustfmt, the
                                              # rest of the tree is left untouched to keep the diff reviewable
cargo check --lib --tests                    # Finished, warnings only (pre-existing)
cargo test --lib                             # results below
cargo clippy --lib                           # no errors; the warnings are the pre-existing ones (266 map_or, 51 f32 fallback, …)
```

New tests in this pass (17):

* `collab::textcrdt::tests` — 9: convergence while typing at the same place, reversed delivery + duplicates, insert into a deleted range,
  causal waiting, multi-hunk adapter + Unicode, caret anchors, `merge3` symmetry/overlap, 30 000-operation three-replica convergence,
  `Hub` round-trip + compaction.
* `collab::access::tests` — 4: plan signature (and five tampered plans), grants (expiry / uses / revoke / channel binding / JSON / cap),
  quorum rule, grant limit.
* `collab::dht::tests` — 3: bencode round-trip + hostile input, infohash normalisation, announce → lookup through a fake DHT node
  (including the background announcer).
* `collab::bigsync::tests` — 6 (2 rewritten, 3 new): merge rules, tombstones, two stores converge, edited bucket recomputation, hostile
  chunks, **two cells edited on two devices**, **tombstones survive a restart + checkpoint validity**, **delta request**, **mmap reads**.
* `collab::bigsync::tests::soak_one_million_rows` stays `#[ignore]` (needs ~2 GB).

Existing integration tests that cover the touched code: `two_devices_sync_over_a_real_socket`,
`a_big_table_is_shared_edited_and_deleted_live`, `the_editors_elect_a_new_host_when_the_host_vanishes`,
`a_new_device_needs_the_hosts_approval`, `punch::two_sockets_punch_through_and_exchange_messages`, the secure-channel tests.

### Full suite (`cargo test --lib`, 341 tests)

```
test result: FAILED. 338 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out; finished in 31.29s
failures:
    tests::right_click_keeps_selection_and_opens_menu
```

That one failure is **pre-existing and unrelated**: with the tree stashed (`git stash -u`, baseline = 320 tests) it fails the same way —
the test sweeps pointer coordinates until the editor context menu opens, and the headless font metrics of this machine place the glyphs
outside the swept area. Everything this pass added passes (the 17 tests listed above), as do the existing socket, big-table, election,
approval, punch and secure-channel integration tests.

## Builds

```
./build-web.sh        → ../public/webapp        lumen_bg.wasm 26.3 MB, lumen.js 93 KB, index.html, sw.js, manifest, icons, version.json
./build-android.sh    → ../public/downloads      Lumen-android-arm64.apk 17,302,208 B, signed (META-INF/LUMEN.RSA, .SF), classes.dex 69 KB,
                                                lib/arm64-v8a/liblumen.so 33.3 MB, apksigner verify OK
./build-windows.sh    → ../public/downloads      Lumen-windows-x64.exe 39,209,984 B (+ zip 19,707,824 B)
./make-sums.sh        → ../public/downloads/SHA256SUMS.txt
```

Checks run on the artefacts: `unzip -t` on the zip (OK), `unzip -l` on the APK (classes.dex, `lib/arm64-v8a/liblumen.so`, the v1 signature
files), `strings classes.dex` contains `LumenActivity`, `CollabService`, `BlePair`, `FcmKit`, SHA-256 of every file recorded above.

## Download links (fixed after the first delivery)

`/api/download/<file>` used to answer `302 → /downloads/<file>` built from `req.url`. On this machine that is `http://localhost:3000`, so the
browser behind the preview proxy was sent to an address it cannot reach — the buttons looked dead. The route now streams the artefact itself:

```
GET /api/download/Lumen-android-arm64.apk
  200  content-type: application/vnd.android.package-archive   content-length: 17302208   accept-ranges: bytes
       sha256 = bd4c2de820f1d166dc5e8afba984adaf8d7733be2cdec1be71dd5d7108399cfd  (identical to the file on disk)
GET … (Range: bytes=100-199)   206  content-range: bytes 100-199/17302208  (100 bytes)
HEAD …                         200  content-length: 17302208
/api/download/Lumen-windows-x64.{exe,zip}, SHA256SUMS.txt   200 with their real sizes
/api/download/nope.apk         404 {"error":"unknown file", …}
```

The landing page always renders the three buttons (the size is decoration; a missing file shows “Download” and the route explains what to
build), plus a `direct link` to `/downloads/<file>` and a `Get the APK` button in the header.

## Website

```
npx next typegen                     ✓ Types generated successfully
npm exec tsc -- --noEmit             clean
npx drizzle-kit push                 download_counts created
npm run build                        ✓ compiled, routes: / , /api/collab , /api/collab/push , /api/download/[file] , /api/health
```

`build_and_start` runs the production server and checks `/api/health` (PostgreSQL round-trip).
