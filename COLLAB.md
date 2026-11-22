# Local collaboration — design, status and next steps (Lumen 14.5)

> **14.1 update:** the security review and hardening pass is documented in §8 (protocol 2: end-to-end encrypted, authenticated, rate-limited);
> §4 and §7 below describe the 14.0 state and are annotated with what 14.1 closed.

Sharing a workspace, one page, or a page **with everything nested** between devices on the same Wi-Fi, with live cursors, roles, PDF
annotations and on-demand attachments. No account, no cloud. Code: `src/collab/` (≈2 000 lines), UI hooks in `src/app.rs`,
`src/canvasui.rs`, relay in the website (`src/app/api/collab/route.ts`).

## 1. What works in this pass (verified)

| Area | State |
|---|---|
| Entity CRDT (`crdt.rs`) | HLC + LWW element map, fractional order (LIS-minimal), tombstones, digest/diff anti-entropy. 3 unit tests (convergence, no echo, ordering). |
| Session (`mod.rs`) | Host / joiner, approval flow, roles (Editor / Viewer), roster, presence, flow-controlled sends, bounded per-frame apply (4 ms), on-demand media, id spaces. |
| Native transport (`net.rs`) | WebSocket over TCP (tungstenite) + UDP discovery (broadcast, /24 broadcast, multicast). **End-to-end test over a real loopback socket passes** (`collab::tests::two_devices_sync_over_a_real_socket`: initial sync, host→joiner edit, joiner→host edit + new block). |
| Browser transport (`webnet.rs`) | `WebSocket` to a host (`ws://ip:port`). Works when the page is http or localhost; https pages need the relay (mixed content). |
| Relay (`relay.rs` + `/api/collab`) | HTTP long-poll rooms; one virtual connection per remote device. Works in every build, incl. https web. Opaque frames, memory only, 30 min idle TTL. |
| UI (`ui.rs`) | People chip (avatar stack) next to the breadcrumb → drop-down (avatars, `…`, `+`, roles, follow, cursor switch, leave); Share / Nearby dialog; QR; four-dot "finding" animation; remote cursors on canvas pages; toast. |
| PDF cards | Borderless page (edge to edge, aspect-locked card), slim caption bar (name, `n / total`, first / prev / next), annotations from the reader drawn on the card; annotations sync as entities. |

## 2. Architecture

```
            ┌────────────┐ Msg frames   ┌──────────────┐
 egui UI ◄──┤  Collab     ├────────────►│ Sink (trait)  │── WebSocket/TCP (native)   net.rs
 (app.rs)   │  session    │◄── Inbox ───│               │── WebSocket (browser)      webnet.rs
            │  + Replica  │             │               │── HTTP relay (everywhere)  relay.rs
            └─────┬──────┘              └──────────────┘
                  │ apply / tick
            ┌─────▼──────┐
            │ Workspace   │  (pages, blocks, canvas nodes/edges, pdf_annots)
            └────────────┘
```
* **Star topology.** The host merges and fans out; joiners only talk to the host. Simple, O(n) traffic, trivially authoritative for
  roles and scope. Cost: the session ends when the host leaves (the joiner keeps its copy). Mesh is on the roadmap.
* **Transport-agnostic.** The session never touches a socket: it consumes `Event`s from a thread-safe `Inbox` (every push wakes egui)
  and writes through `Sink`. A new transport (WebRTC, QUIC, Bluetooth, Wi-Fi Direct) = one new file.
* **Joiner lands in its own workspace** ("Shared · name") created through the existing workspace registry. The host's page ids are kept,
  so a remote edit maps 1:1 — no id translation layer. The shared copy stays after the session ends.

### Data model (`crdt.rs`)
* **Entities**: `Page` (meta without blocks / history / view state), `Block`, canvas `Node`, `Edge`, `PdfAnnot`. One JSON value each.
* **Merge**: `(HLC, hash(value,pos))` greater wins; deterministic on every peer, so replicas converge regardless of delivery order.
  Delete = tombstone with its own HLC (undo / re-create wins again with a newer clock).
* **Order** (block order, z-order of cards, sidebar order) = per-entity `pos: f64`. A local reorder keeps the longest increasing run
  of existing positions and gives fresh midpoints to the rest ⇒ moving one block is **one** op; two concurrent inserts never collide;
  duplicates are fixed identically on both sides; if a gap collapses below 1e-9 the list is renumbered.
* **Replica memory**: `(hlc, pos, hash)` per entity ≈ 48 B — no second copy of the content (values are serialised from the workspace only
  when sent). 100 000 blocks ≈ 5 MB.
* **Change detection**: hashing, no dirty flags in the editor. The open page every tick; other pages when `updated` changed; the rest
  round-robin inside a 2.5 ms budget. The tick interval adapts (`cost × 25`, 160 ms … 1.5 s ⇒ ≤ 4 % CPU) and drops to 90 ms right after
  input (`Collab::poke`). Annotation lists are scanned every tick (tiny).
* **Per-user state is not synced**: sidebar `expanded`, `favorite`, canvas pan/zoom, undo history, page history.
* **Scope**: `Workspace`, `Page` (one page), `Tree` (page + descendants + database rows). The host enforces it: ops for pages outside the
  scope are rejected; new child pages created by a joiner under a shared page are accepted for `Tree`/`Workspace`. The scope root keeps
  each side's own `parent`, so a shared sub-page appears at the top of the joiner's tree.
* **Id spaces**: while a session runs every device allocates ids from `(replica_id << 40) + n` (`collab::id_base()`; `Workspace::id`,
  `Canvas::new_id`, `Notes::next_id`). Simultaneous creations on two devices can never collide, and foreign ids never push your
  counter into their space.

### Protocol (`Msg`, JSON text frames, `PROTO = 1`)
`Hello{peer,token,proto}` → host: token match ⇒ auto-approve as Editor, else `Pending` + a request card in the host's UI
(Edit / View / No). `Welcome{peer,session,role}` → both send `Digest` (key, hlc, hash) in 1 500-entry chunks; each side answers with the
entities the other lacks or has older (pages first) as `Ops` chunks of 120, sent a few frames per frame per connection (back-pressure
without blocking the UI). Live: `Ops`, `Presence` (≤ 16 Hz, 3 s heartbeat), `Roster`, `MediaReq/MediaData/MediaMiss`, `Bye`.

### Media on demand
Attachment ids are content hashes, identical on every device. When the open page references a media id that is missing locally
(`referenced_media`) the session requests it chunk by chunk (192 KiB, pull-based: the next chunk is requested when the previous one
arrives). The sender reads the file once per transfer (`Collab::serving`). PDF cards show "Rendering page…" and re-render the moment the
bytes land (`canvasx::service` no longer gives up on a document that is still arriving). Nothing is sent until a page needs it.

### Discovery
Hosts broadcast a `Beacon` (device, what is shared, people) every 2 s to `255.255.255.255`, the /24 broadcast of their own address and
`239.255.76.77` on UDP 47616; a device listens only while the Nearby tab is open. Beacons carry no document content.

## 3. Platforms

* **Windows / Linux / macOS**: full (host + join + discovery). Windows Firewall prompts once for the listening port (47615).
* **Android**: same code. Manifest has `INTERNET`, `ACCESS_WIFI_STATE`, `ACCESS_NETWORK_STATE`, `CHANGE_WIFI_MULTICAST_STATE`,
  `NEARBY_WIFI_DEVICES`. Device name from `getprop ro.product.model`. Some OEM Wi-Fi stacks drop UDP broadcast while the screen is off —
  see roadmap (MulticastLock, NSD). Cleartext sockets from native code are not subject to the network-security policy.
* **Web (WASM)** — *can it use local collaboration?* Partly, by browser rules: no UDP, no listening sockets, so it can **join** but not
  **host or discover**. Join through (a) the link relay (`/app?join=ROOM&t=TOKEN`, works from https), or (b) a direct `ws://ip:port`
  when the page itself is served over http / localhost. Hosting from a browser tab needs WebRTC (roadmap).
  The wasm build has no threads: the relay and `WebSocket` are callback-driven and everything else runs inside `update()`.

## 4. Security model (honest status)
* LAN traffic is **plain WebSocket** in this pass. Join needs the host's approval (or the 6-character token, shown only to the host).
  Roles are enforced on the host (viewer ops are dropped); a viewer's own client is not yet read-only (it can type locally, the host will
  not accept it — show "view only" and lock the editor next).
* Relay frames pass through your server unencrypted but unparsed and unstored. Treat the room code + token as the secret.
* **Next**: Noise `XXpsk0` (crate `snow`) with the token as PSK on every transport ⇒ confidentiality + authentication without PKI;
  per-device keys remembered after the first approval ("trusted devices").

## 5. Performance notes
* No work while idle: `poll` returns immediately when no session is active and nothing is queued.
* UI thread cost per frame while syncing: drain events (cheap), ≤ 4 ms of op apply, ≤ 2.5 ms of scan (open page excluded), 6 messages per connection.
* Sockets never touch the UI thread (one small thread per connection, 15 ms read timeout ⇒ ≤ 15 ms added latency; replace with a
  readiness-based loop (`mio`) when more than ~10 peers matter).
* Large workspaces: replica ≈ 48 B / entity; initial sync is streamed in chunks and applied progressively; media is pull-based.
  `bigs` (disk-backed million-row tables, `lumen-store`) are **not** synced yet (see roadmap).

## 6. Research notes (libraries considered)
| Option | Verdict |
|---|---|
| `yrs` (Yjs in Rust) | Best fit for **text inside a block** (Y.Text, rich-text marks, awareness protocol, wasm-ready). Plan: keep the entity map for structure, store a Y.Text doc per text block, send Y updates inside `Op.val`. Needs an editor adapter (diff the egui `TextEdit` string → Y ops; map remote ops → cursor-stable edits). |
| `automerge` | Whole-document CRDT with history; heavier (memory ≈ ×3–10 of content), no benefit over entity map + Y.Text here. |
| `diamond-types` | Fastest text CRDT; young API. Alternative to `yrs` for per-block text. |
| `mdns-sd` | Standards-based discovery (Bonjour/Avahi interop). Worth adding next to the UDP beacon for networks that filter broadcast. |
| `snow` | Noise protocol for the encrypted transport (see §4). |
| `quinn` / `webrtc-rs` / `matchbox` | QUIC for fast desktop↔desktop; WebRTC data channels are the only way for a browser tab to **host** or reach a LAN device without a relay (signalling can reuse `/api/collab`). |
| `tungstenite` (used) | Small, sync, no TLS needed on a LAN, interoperable with browser `WebSocket`. |
| `qrcode` (used) | Pure Rust QR generation. |

## 7. Next pass — task list (in priority order)
1. **Text CRDT inside blocks** (`yrs`): register per text block; `Op.val` carries the Y update; adapter in `editor.rs` (the focused block
   must never be overwritten by a remote LWW — today the remote value wins on a same-block conflict). Show remote carets/selections (block id
   is already in `Presence::block`).
2. **Encrypted transport** (Noise PSK) + trusted devices list; viewer clients made truly read-only (editor `locked` flag).
3. **Scan QR** on Android (camera via the Java activity → `join_text`); `lumen://join` intent filter → `collab::VIEW.launch`.
   Android `MulticastLock` + NSD (`NsdManager`) discovery for networks that filter broadcast; battery: stop the 120 ms repaint when idle.
4. **PDF reader realtime**: `Presence::pdf` is already carried — draw other readers' page/position in `pdf.rs` and show live strokes
   (annotation drafts) before they are committed.
5. **Mesh / host hand-over** (any peer can relay; session survives the host leaving); reconnect with digest resync (the replica is
   rebuilt from the workspace, so a re-join is a normal anti-entropy round).
6. **Big tables** (`bigs`): sync chunk-wise through `lumen-store` zone maps; permission scopes per database.
7. **Compression** of `Ops` (zstd) and binary framing (postcard) — JSON is ~2× larger than needed; irrelevant on Wi-Fi, matters on the relay.
8. **Tombstone GC** (all peers acknowledged), snapshot checkpoints for very long sessions.
9. WebRTC transport for browser hosting / LAN-from-browser; Wi-Fi Direct / Quick Share hand-off for devices on different networks.
10. Comments / @mentions as entities (`Key::Comment`) — the entity model takes new kinds without protocol changes (bump `PROTO` only for breaking changes).

## 8. How to test by hand
* Two desktops on one Wi-Fi: A → people icon → *Start sharing*; B → people icon → *Nearby* → click A → A approves.
* One machine: start the app twice is not possible (UDP port reuse) — use the automated test: `cargo test --lib collab`.
* Phone ↔ desktop: install the APK, same Wi-Fi, same steps. Type the 6-letter code if discovery is blocked by the router (AP isolation).
* Web: open `/app?join=ROOM&t=TOKEN` (host: *Settings ⚙ → relay server = your site*, switch on *Also create a link*).


## 8. Pass 2 (14.1) — security & performance review, and what changed

Method: read every path that handles bytes from the network (`net.rs`, `webnet.rs`, `relay.rs`, `mod.rs::on_msg/handle_ready`, the site's
relay) with the question "what can a stranger on the Wi-Fi / who knows only the room code make this device do?".

### 8.1 Findings (14.0) and fixes (14.1)
| # | Finding (14.0) | Severity | Fix in 14.1 |
|---|---|---|---|
| 1 | All traffic in clear text (LAN socket, relay) — sniffable, forgeable | High | **Noise XX** (`secure.rs`): X25519 / ChaCha20-Poly1305 / SHA-256, forward secrecy, every frame AEAD-sealed, nonce counter ⇒ replay / reorder / tamper fails. Applies to LAN, browser WebSocket and relay alike. Protocol 2; protocol-1 peers are refused. |
| 2 | Token sent in `Hello` (a sniffer / relay operator learns it) and compared with `==` | High | The token **never travels**: both sides send `SHA-256(label ‖ token ‖ handshake-hash)` (bound to the channel ⇒ not replayable by a man in the middle), compared in constant time. The same proof authenticates the *host* to the joiner (`Welcome.auth`), so knowing only the room code cannot impersonate a host. |
| 3 | Tokens / room codes from a time-seeded xorshift (predictable), 6 characters | High | `getrandom` (OS CSPRNG; `crypto.getRandomValues` in the browser), unbiased 32-symbol alphabet, **10 characters (50 bit)**; session / room ids too. Noise static keys are per process. |
| 4 | Token brute force unlimited | High | 5 wrong proofs per address ⇒ 5 min lock-out (auto-join disabled, approval still possible); a wrong proof disconnects. |
| 5 | **Kick by id**: an unauthenticated socket could send `Hello{peer.id = victim}` and the host dropped the victim's real connection | High | A stale connection is replaced only by a peer that proves the token or presents the same channel key; otherwise "id already connected". Host's own id and id 0 are refused. |
| 6 | Presence spoofing: the host trusted `Presence.peer` from the client | Medium | The host overwrites it with the connection's peer id; non-finite / absurd coordinates dropped; ≤ 64 presence entries. |
| 7 | Unbounded resources per stranger: threads (one per connection), pending approval cards, digest, op queue, media requests | High (DoS) | ≤ 40 connections / 6 per address / 16 signed-in peers / 8 pending approvals; `Hello` due within 20 s, approvals expire after 5 min; digest ≤ 3 M entries; ≤ 1 000 ops per message and ≤ 300 000 queued; ≤ 64 media requests queued; token-bucket flood control (≈ 500 frames/s) drops the connection. |
| 8 | WebSocket frames up to 64 MiB accepted | High (memory) | `max_message_size = max_frame_size = 256 KiB`; reassembled plaintext ≤ 24 MiB; inflate is limited (decompression bomb test). |
| 9 | Unsolicited `MediaData` accepted and stored, unlimited size, no integrity check | High | Only for ids this device requested, ≤ 256 MiB, ≤ 8 transfers, chunk offsets checked, **content id (FNV of the bytes) verified** before the file is used; names / mime sanitised. |
| 10 | Names from the network drawn unescaped (control chars, 10 kB names) | Low | `clean()`: printable, ≤ 40 chars; beacons sanitised, ≤ 64 discovered hosts. |
| 11 | Relay: anyone could post/poll any room as any device id; no limits | High | Per-session random key bound to (room, device id) on first use (403 afterwards), room/member/byte/message caps, IP token bucket, 30 min idle TTL, nothing on disk, stale hellos not replayed. The relay only ever sees sealed frames. |
| 12 | `lumen://` links joined immediately | Medium | A link from another app only pre-fills the dialog; the user taps Join. |
| 13 | Android: `allowBackup=true`, cleartext allowed | Medium | Backup off, cleartext off (LAN sockets are native and unaffected). |
| 14 | Join request cards could be raised by anyone | Low | Cards show a **4-digit channel code** derived from the handshake: if both screens show the same code, nobody sits in the middle. |

Known limits (unchanged, documented honestly): a *viewer* client's UI is not locked (its edits stay local and the host drops them);
the host is the single authority; FNV is not collision-resistant (attachment ids are content ids, not signatures — the AEAD already
protects them on the wire); a malicious *approved Editor* can of course edit or delete content in scope (use View / history).

### 8.2 Performance
* **Compression**: payloads > 400 B are deflated before sealing (JSON ops typically 4–10× smaller; relay bandwidth ÷ 5).
* **Battery**: socket threads poll every 15 ms while data moves and every 90 ms after 2 idle seconds (was a fixed 15 ms ⇒ 66 wake-ups/s per
  connection); the collab repaint timer follows the adaptive scan interval (≤ 1 s) instead of a fixed 120 ms.
* **No work for strangers**: failed handshakes cost one X25519 operation and a thread slot that is released at once.
* Android holds a `MulticastLock` only while the app is in the foreground (discovery works with Wi-Fi power-save, battery is spared in the background).

### 8.3 Task list (§7) — status
| # | Task | State |
|---|---|---|
| 1 | Text CRDT (`yrs`) inside blocks | open — large editor adapter; entity LWW stays |
| 2 | Encrypted transport (Noise) | **done** (XX + token proof; "trusted devices" persistence still open) |
| 3 | Android deep link + MulticastLock | **done** (`lumen://join` intent filter, `nativeLink`, `MulticastLock`); camera QR scan & NSD still open |
| 4 | PDF realtime readers | open |
| 5 | Mesh / host hand-over | open |
| 6 | Big tables sync | open |
| 7 | Compression | **done** (deflate; binary framing still open) |
| 8 | Tombstone GC | open |
| 9 | WebRTC for browser hosting | open |
| 10 | Comments / @mentions | open |

## 9. Pass 3 — scope audit, reconnect, relay on the site (this delivery)

Method: second review of everything an *approved* guest (Editor) or a stranger can still do after pass 2, plus a look at failure modes
(network drops, screen off, slow peers). Every fix below has a test (`cargo test --lib collab`, 12 tests).

### 9.1 Findings and fixes
| # | Finding | Severity | Fix |
|---|---|---|---|
| 15 | **Page take-over.** In a *Tree* share a guest could send a `Page` op for an *existing, unshared* page id and name a shared page as parent: `page_allowed` accepted it, the page was overwritten and pulled into the shared scope (its blocks then synced out). Ids are small and sequential. | High | `apply_page` refuses a page that exists locally but is not in scope. Test `a_guest_cannot_take_over_delete_or_loop_pages_outside_its_scope`. |
| 16 | **Delete outside scope.** `Key::Page` tombstones were applied without any scope check: an editor of ONE page could delete any page of the host. | High | Deletes need the page to be in scope (`in_scope_page`). |
| 17 | **PDF annotation scope.** `Key::Annot` ops were applied for any document id. | Medium | Only for documents referenced by shared pages (`docs`), workspace shares unchanged. |
| 18 | **Page-tree cycles.** `A.parent = B`, `B.parent = A` from a hostile editor (or a bad merge) loops every sidebar / breadcrumb walk → UI hang on all devices. | High (DoS) | `would_cycle` walk before applying a page; self-parent refused. |
| 19 | **Unbounded entity values / count.** One 20 MB block, millions of tiny ones. | Medium (DoS) | ≤ 4 MiB per entity value, ≤ 3 M entities per replica, non-finite positions refused. Digest cap 3 M → 1 M entries per connection. |
| 20 | **Kick was not a ban.** A removed guest still held the share token and rejoined as Editor at once. | High | Removed devices are banned by their Noise static key for the session (`banned`). |
| 21 | **Role laundering.** A guest demoted to *View* reconnected with the token and was auto-approved as Editor again. | High | Demoted keys are remembered (`demoted`) and re-admitted as viewers. |
| 22 | **Slow-loris on the WebSocket handshake.** The 5 s socket timeout restarts with every byte, so a stranger could hold one of the 48 connection slots forever. | Medium (DoS) | Hard 8 s handshake deadline (`HandshakeGuard`) on accept and on dial. |
| 23 | **Unbounded inbox.** A peer sending at wire speed while the UI thread is not running (screen off, Android background) queued events without limit. | Medium (memory) | Socket threads stop reading while > 4 096 events wait (TCP back-pressure). |
| 24 | **Silent half-open sessions.** On the relay (and on a dead Wi-Fi) an authenticated peer that vanished was never noticed. | Medium (reliability) | Heartbeat watchdog: no frame for 45 s ⇒ connection closed ⇒ reconnect logic (below). |
| 25 | Android build ships the *development* release keystore inside the repository (`android/lumen-release.jks`, password in `Cargo.toml` / `build-android.sh`). | Info | Kept so that updates install over earlier APKs. For your own distribution generate a key (`LUMEN_KEYSTORE`, `LUMEN_KEYSTORE_PASS` are honoured by `build-android.sh`) and keep it out of git. |

### 9.2 Reliability: automatic reconnect with digest resync (roadmap item 5, joiner side)
A joined device that loses its connection (Wi-Fi blip, host asleep, relay hiccup, 45 s of silence) no longer ends the session:
* the **replica, the workspace and the session description are kept**; only the transports are torn down;
* the dial is repeated with back-off 0.8 / 2 / 4 / 7 / 10 / 15 / 20 / 30 s (8 attempts), the UI shows the normal "connecting" state and a toast;
* on `Welcome` for the **same session id** the joiner sends its *real* digest instead of an empty one, so anti-entropy transfers whatever
  changed on either side while it was away (offline edits are merged by the normal LWW rules; deletions stay deleted);
* a different session id at the same address (host restarted) is treated as a fresh join, a `Denied` / `Bye` / kick ends the session at once;
* a viewer stays read-only while reconnecting.
Test: `a_dropped_connection_is_re_dialled_and_catches_up` (real sockets, edits on both sides while disconnected).

### 9.3 The relay lives in this repository's website
`src/app/api/collab/route.ts` + `src/lib/relay.ts` implement the protocol `relay.rs` speaks: `POST {room, from, k, msgs:[{to, m}]}` (`to` 0 = everybody else) and
long-poll `GET ?room&me&k&since` → `{next, items:[{from, m}]}`. Controls: room code is the capability (uppercase, 4–32 chars), each
(room, device id) is bound to a per-session key on first use (403 otherwise), ≤ 400 rooms / 12 per address / 24 members, log ≤ 3 000 frames and 24 MiB,
frame ≤ 512 KiB, body ≤ 6 MiB, token-bucket per address, hello frames older than 60 s are never replayed, 30 min idle expiry, memory only.
Frames are opaque sealed Noise records — losing one (log trimmed) breaks the AEAD nonce sequence, the connection is dropped and the reconnect above repairs it.

### 9.4 Task list status (after pass 3)
| # | Task | State |
|---|---|---|
| 1 | Text CRDT (`yrs`) inside blocks | open — still the biggest gap; entity LWW remains (on a same-block conflict the newer whole-block value wins) |
| 2 | Encrypted transport, viewers read-only, bans | **done** (trusted-device *persistence* across restarts still open: Noise keys are per process) |
| 3 | Android deep link, MulticastLock | **done**; camera QR scan, NSD open |
| 4 | PDF realtime readers | open |
| 5 | Mesh / host hand-over | **joiner reconnect done**; host hand-over open |
| 6 | Big tables sync | open |
| 7 | Compression | **done** |
| 8 | Tombstone GC | open — needs "all peers acknowledged" bookkeeping; tombstones are 48 B each |
| 9 | WebRTC for browser hosting | open |
| 10 | Comments / @mentions | open |

### 9.5 Ideas not done (ranked)
1. `yrs` Y.Text per text block (largest user-visible win: two people typing in the same paragraph).
2. Persist the device key + "trusted devices" (approve once, auto-join later) — pair with Android Keystore.
3. Camera QR scan on Android (Java activity → `join_text`), NSD discovery.
4. Binary framing (postcard) for the relay.
5. Tombstone GC with acknowledgements; snapshot checkpoints.

### 9.6 Verified against the website relay
`LUMEN_RELAY_TEST=http://127.0.0.1:3000 cargo test --lib site_relay -- --ignored` runs the real Rust client (host + joiner, `ehttp`) against the
running site: Noise handshake over the room, initial sync, joiner → host edit (stable over repeated runs). Note: the relay shows a device only frames posted
*after* it first appeared in the room (so a late joiner never receives somebody else's old handshake); a host therefore has to be polling before the link is
shared — which is always the case in the app (the link only exists once the room does).

## 10. Pass 4 — concurrent text merge, trusted devices, comments, tombstone GC, Android QR + NSD (14.3)

Everything below is covered by `cargo test --lib collab` (21 tests) unless marked *(device-only)*.

### 10.1 Task list status (after pass 4)
| # | Task | State |
|---|---|---|
| 1 | Text CRDT inside blocks | **done in a different way than planned** — field-wise **three-way merge** (`textmerge.rs`) instead of `yrs`. See 10.2. Remote carets inside a paragraph are *not* drawn (only the block id travels in `Presence`). |
| 2 | Trusted devices / persisted device key | **done** — the Noise static key is created once and stored with the app data; a host remembers every device it approved (name, role) and lets it in without a card next time; *Forget* in the Share dialog revokes (and a kick bans). The key sits in egui's persisted storage (app-private dir on Android); wrapping it in the Android Keystore is still open. |
| 3 | Android camera QR scan + NSD | **done, compile-verified, not exercised on a physical phone** *(device-only)* — `CollabKit.java` (camera preview → Rust `rqrr` decoder → join dialog; `NsdManager` register/discover of `_lumen._tcp` with the beacon in TXT records). Nearby shows NSD hosts next to UDP ones. |
| 4 | PDF reader realtime | open — `Presence::pdf` is carried and stored, the reader does not draw other readers yet. |
| 5 | Mesh / host hand-over | open — joiner-side reconnect (pass 3) stays; a session still ends when the host leaves. Design sketch in 10.5. |
| 6 | Big tables (`bigs`) sync | open. |
| 7 | Binary framing | open (deflate from pass 2 already removes most of the JSON overhead). |
| 8 | Tombstone GC | **done** — see 10.3. |
| 9 | WebRTC for browser hosting | open. |
| 10 | Comments / @mentions | **done** — see 10.4. |

### 10.2 Concurrent edits of one block: three-way merge
* Every `Entry` carries `base` = content hash of the value it replaced (0 = unknown, old behaviour). `Replica` keeps the last four versions of
  the blocks of the open page (and of any block that received a remote edit) in `hist` — the *merge bases*.
* `Replica::apply` → `merge_block`: if an incoming block op is neither identical to nor built on what we hold, and its base is in `hist`,
  the block JSON is merged field by field (`textmerge::merge_json`): a field only one side touched wins; a text field both touched goes
  through a character-level three-way merge (`merge3`: edits in different places are both kept, identical edits apply once, overlapping edits
  keep both versions in a deterministic order); other conflicting fields fall back to the newer clock.
* The merge is **symmetric** for non-overlapping edits, so both peers compute the same result and converge without negotiation; the merged
  value is re-published by the normal local scan with a newer clock. A limiter (4 merges / 3 s / block) rules out ping-pong.
* Tests: `two_people_typing_in_the_same_paragraph_keep_both_edits` (A appends, B prepends, both texts survive on both sides),
  `merge_is_symmetric`, `json_fields_merge_independently`.
* Limits: single-hunk diff per side (two far-apart edits by the *same* person inside one scan interval collapse to one wider hunk — still
  loss-free, only coarser); blocks of pages nobody has open on the merging device have no base and keep last-writer-wins.
  A real sequence CRDT (`yrs`) would also give intention preservation and remote carets — see 10.5.

### 10.3 Tombstone garbage collection
Tombstones (48 B) older than 10 minutes are dropped every minute (`Replica::gc`, `Collab::poll`). This is safe because a dropped connection
gives up after ≈ 90 s (`RESUME_DELAYS`) — no device can come back with a view older than the grace period; a later join is a fresh
workspace anyway. Test: `old_tombstones_are_collected_and_young_ones_stay`. `Stats::gc` counts what was freed.

### 10.4 Comments and @mentions
* Model: `Workspace::comments: BTreeMap<page id, Vec<Comment>>` — `Comment { id, block, reply_to, author, author_id, text, ts, resolved }`.
* Sync: `Key::Comment(page, id)` entities (group 2 of the page), scan / apply / digest / diff like annotations, scope-checked on the host
  (a guest can only comment on pages in the shared scope; ≤ 5 000 comments per page). **`PROTO` is now 3** (old peers are refused with a message).
* UI (`collab/comments.rs`): speech-bubble button with the number of open comments in the top bar (desktop and phone layouts), a window with
  threads, replies, *Resolve / Reopen / Delete*, "resolved" filter, optional attachment to the block being edited, `@name` completion from the
  people in the session, highlighted mentions, and a toast when someone else comments or mentions you. Works offline too.
* Tests: `comments_sync_resolve_and_delete`, `mentions_match_first_names_only_at_word_ends`.

### 10.5 Review — what more can be done (ranked)
1. **Host hand-over.** Cheapest design: host sends `Handover{to, token}` to the longest-connected editor; that device starts listening (it
   already holds the replica and the session id), answers with its port, the host broadcasts `Moved{ip, port, token}` and the others re-dial
   through the existing resume path (same session id ⇒ digest resync). Relay sessions need no hand-over (the room is the host). Needs a 3-device
   test harness.
2. **`yrs` Y.Text per text block** for intention preservation and live remote carets (block id is already in `Presence`); keep the entity map for
   structure and use the merge of 10.2 as the fallback for old peers.
3. **PDF reader realtime**: draw `Presence::pdf` as small avatars on the page thumbnail strip + live stroke drafts.
4. **WebRTC data channels** (signalling over `/api/collab`) so a browser tab can host and LAN devices can reach it without the relay.
5. **Big tables**: sync `lumen-store` zone-map chunks instead of pages; per-database permission scopes.
6. Binary framing (postcard) for the relay; snapshot checkpoints; Android Keystore wrapping of the device key; Wi-Fi Direct / Quick Share hand-off
   across networks; end-to-end tests on real phones (QR + NSD are verified to compile and package only).
7. Product ideas beyond sync: comment notifications while the app is closed (relay push), per-block "suggest edit" mode, presence "typing…"
   badges, session recording / audit log for hosts, read-only share links with expiry, workspace snapshots to roll back a bad merge.


## 11. Pass 5 — field-report fixes, temporary / permanent shares, review of changes, live carets (14.4)

Input: the field report after pass 4 (Android scanner dead, camera behind the app, slow big workspaces, delayed comments, `@` list wrong,
no remote carets, PDF marks not live, browser could not connect across networks, join without acceptance, share always saved, a re-share
created a new workspace, no view of what changed). Status of each item, honestly:

| # | Report | State | What was done |
|---|---|---|---|
| 1 | Android scanner did not work / camera opened **behind** the app | **fixed** (device-only verification pending) | Root cause: `LumenActivity` is a *NativeActivity* — native code owns the window surface, so Java views added to it (a `SurfaceView` hole, buttons) are never composited above egui; the preview ended up behind the opaque egui surface. The scanner is now its **own activity** (`ScanActivity`): normal view hierarchy, `TextureView` preview (no z-order games), centre-crop transform, rotation handling, torch, tap-to-focus, viewfinder, vibrates on success, 140 ms decode cadence with a one-slot queue (a slow decode drops frames instead of queueing). The camera permission is requested by the activity itself. The decoded link is queued through the existing `push_launch_link` and pre-fills the join dialog. Registered in the manifest (`ScanActivity`, `fullSensor`, not exported). |
| 2 | Large workspaces: massive lag | **partly fixed** — the three worst O(n²) / O(n) hot spots found by reading the code | **Sidebar**: `children()` + `has_children()` were O(P) *per row*, per frame ⇒ O(P²) per frame; now a cached flattened tree (signature-guarded, rebuilt only when the tree changes) and **only the visible rows are laid out** (fixed 29 px pitch + two spacers) — cost per frame is independent of the page count. **Sync scan** (`Replica::tick_inner`): `order.contains()` and `ids.position()` were O(P) inside an O(P) loop ⇒ O(P²) per tick, plus an O(entities) "disappeared pages" scan every tick; now a `HashSet`, an id→index map, a page-list signature that skips the O(P) bookkeeping while the page list is unchanged, and a bounded round-robin slice (512). Annotation / comment lists are **fingerprinted** (no allocation) and serialised only when they changed. **Editor**: on pages with > 400 blocks, blocks that were never laid out and are > 1 600 px below the viewport get an estimated height instead of being laid out in the first frames (the measured height replaces it when they come near). **Not done**: autosave still serialises the whole workspace every 30 s on the UI thread (needs a trustworthy dirty flag across all editors), `Workspace::children()` is still O(P) for its other callers, bigs (`lumen-store`) are not synced. |
| 3 | Comments appear late | **fixed** | The full scan is adaptive (160 ms … 1.5 s, more on big workspaces). Comments and PDF annotations now have their own **hot path**: `Replica::tick_hot` runs every frame, is fingerprint-guarded (≈ 0 cost when nothing changed) and sends at once. Test `comments_reach_the_other_device_without_waiting_for_the_full_scan` asserts < 2 s end to end over real sockets. |
| 4 | `@` list not shown correctly | **fixed** | The completion looked only at the *last* `@` of the *whole text*, matched only a name's start, showed ≤ 4 buttons and nothing at all when the exact name was typed. Now: it follows the **caret** (an `@` at a word start before the caret, no whitespace in between), matches the start of any word of a name, shows a proper list (≤ 6) with ↑ ↓ Tab / Enter and tap, puts the caret behind the inserted name, and says why it is empty ("nobody else is in this session yet"). |
| 5 | No cursor of other users while editing a page | **done** | `Presence.caret = (selection start, caret)` in characters of the plain text of `Presence.block`. The editor reports the focused caret every frame; every drawn block paints other people's carets: 2 px bar in the person's colour, a name flag, and a tinted single-row selection. ≤ 16 Hz, only while the caret / block changed (+ 3 s heartbeat). Sanitised on the host (bounded, finite). |
| 6 | PDF annotations not real time | **done** | (a) finished marks use the hot path above; (b) **live strokes**: `Presence.draft = LiveDraft { doc, page, tool, color, pts≤96 }` is sent while a highlight / underline / strike / pen stroke is drawn and painted by every reader (`pdfnotes::paint_remote_drafts`, also on canvas PDF cards) in the drawer's colour; sanitised (tool ≤ 3, ≤ 128 finite points). Not yet: other readers' *page position* avatars in the thumbnail strip (`Presence::pdf` is carried but not drawn). |
| 7 | Browser connect over different networks, QR / link, **with approval** | **done via the relay; WebRTC not done** | The web build can already host and join through `/api/collab` (HTTPS-safe, any network, NAT-friendly): *Start sharing → Also create a link* → QR / link → the other side opens `/app?join=ROOM&t=CODE`. **New**: the code no longer admits anybody by itself — it only proves the link is genuine; **every new device waits for the host's accept card** (4-digit channel code to compare, Edit / View / No). Hosts that want the old behaviour can switch "Ask me before anyone joins" off. Approved devices are remembered (trusted devices) and come back without a card. The relay itself is now part of this repository's website (`src/app/api/collab/route.ts`, `src/lib/relay.ts`) and speaks exactly what `relay.rs` speaks. **Open**: a true browser↔browser WebRTC data channel (no relay in the data path). Design: signalling over the same `/api/collab` room (SDP + ICE as ordinary frames), `web_sys::RtcPeerConnection` + `RtcDataChannel` behind the existing `Sink` trait (one new file `webrtc.rs`, ~300 lines), Noise runs on top unchanged. It needs real browsers to verify, which this environment does not have, so it was not shipped unverified. |
| 8 | "Super fast, efficient, Telegram-level (MTProto-like) encryption" | **already stronger than asked; kept** | Frames are sealed with **Noise XX** (X25519 key agreement, ChaCha20-Poly1305 AEAD, SHA-256) — the construction used by WireGuard / WhatsApp / Lightning; ChaCha20-Poly1305 is as fast as AES-IGE in software and far faster on phones without AES instructions, and unlike MTProto 1/2 (AES-IGE + a bespoke KDF, repeatedly criticised by cryptographers) it is a standard, formally analysed AEAD with forward secrecy. Payloads > 400 B are deflated before sealing. Additional per-frame cost: one AEAD pass, no allocation beyond the output buffer. I deliberately did **not** write a custom MTProto-style cipher: hand-rolled crypto is the one place "custom codebase" is the wrong call. Open: binary framing (postcard) instead of JSON + base64 (≈ −30 % bytes), would be a `PROTO` bump. |
| 9 | Temporary vs permanent share | **done** | Host chooses **Permanent / Temporary** in the Share dialog (`SessionInfo.keep`). Permanent: the joiner keeps the workspace. Temporary: the joiner gets a "Temporary · name" workspace that is removed when the session ends (the app switches back first); a chip "Temporary · removed when the session ends — **Keep a copy**" converts it. A temporary workspace left behind by a crash is removed at the next start (id persisted). |
| 10 | Re-share + reconnect opens the previous workspace | **done** | `SessionInfo.share` = SHA-256 of (device key ‖ workspace id ‖ scope) — stable across restarts, reveals nothing. The joiner stores `share → (workspace id, last-sync time)` (egui persisted storage, refreshed every 30 s and at the end of a session). Joining the same share again **reopens that workspace** (`Collab::bind_existing`): its content is adopted as the replica stamped with the last-sync time, a *real* digest is sent, only the difference travels. Test `rejoining_reopens_the_same_share_and_lists_what_changed`. Limit: if the host *restarted* its session, host entities are stamped "now", so a *conflicting* offline edit of the joiner to the same block loses to the host (non-conflicting edits are all kept; a plain reconnect without a host restart keeps both). Fix idea: stamp the host replica per page with `Page::updated` at adopt time. |
| 11 | "Show what changed like git" | **done** | `collab/changes.rs`: every remote change is recorded (host: always; joiner: after the first sync settles, or from the start when a workspace was reopened) with its before / after text, author and time; later edits of one block fold into one entry (typed-and-reverted disappears). The **Changes** window (history icon in the top bar, badge with the count; opens by itself after a re-join) groups by page and draws a unified diff: red `−` / green `+` rows, 1 line of context, **changed words highlighted inside the line**. Added / removed blocks, pages and comments are listed; canvas / annotation changes are counted. Bounded: 1 000 entries, 8 KiB per side. 3 unit tests (line diff, word diff, folding). |
| 12 | Performance / memory everywhere | **ongoing** | See #2. Memory: the change log is bounded; replica fingerprints are 8 B per list; the sidebar cache is 24 B per visible row. |

### 11.1 Verified
`cargo check --lib --tests`, `cargo test --lib` (collab suite includes the new tests: approval, rejoin + change list, comment latency),
`cargo clippy`, `cargo fmt --check` for the new modules, plus production builds of the Windows exe, the WebAssembly bundle and the signed arm64 APK
(`build-all.sh`). Camera, torch and NSD are *device-only* paths: the APK compiles, packages and the Java activity was compiled with `javac -source 8`,
but nobody has held a phone to a screen in this pass.

### 11.2 Review — what more can be done (ranked)
1. **WebRTC data channels** (see #7) — browser↔browser and browser↔phone without the relay in the data path; reuse `/api/collab` for signalling.
2. **Host restart stamps** (see #10): per-page adopt stamps so offline joiner edits survive a host restart; then "offline edits made while away" can be listed in the Changes window too.
3. **Autosave off the UI thread / dirty tracking** — a `Workspace::rev` counter bumped by every mutation path (or a hash of the changed pages) so idle autosave costs nothing and large workspaces serialise incrementally.
4. **`yrs` text CRDT** for in-paragraph intention preservation (the three-way merge of 10.2 keeps both edits but not intent).
5. Page-level **suggest-edit mode** (changes held until the host accepts them — the review UI already shows the diff), per-person colours in the diff, "revert this change" buttons (the `before` text is already stored).
6. Sync of `lumen-store` big tables chunk-wise; binary framing; host hand-over / mesh; Wi-Fi Direct / Nearby Share hand-off.
7. Android: Keystore-wrapped device key; CameraX instead of the deprecated `android.hardware.Camera` (kept: zero dependencies, works API 26–34); ML-free QR decode is `rqrr` (~30 ms per 1280×720 frame on a mid phone).
8. Presence "typing…" badges, read-receipts for comments, notification push through the relay while the app is closed.


## 12. Pass 6 — direct internet connect, nearby hotspot, suggest-edit, restart stamps, dirty-tracked autosave (14.5)

Input: the §11.2 review list plus the field notes "realtime / cursors still unreliable", "no peer-to-peer option for devices on different Wi-Fi",
"how do two nearby devices without a shared Wi-Fi connect fast, like AirDrop". Everything below is covered by `cargo test --lib collab`
(37 tests, 9 of them new) unless marked *(device-only)*.

### 12.1 Status of the §11.2 list

| # | Item | State |
|---|---|---|
| 1 | WebRTC data channels | **not done** — see 12.6. The same goal for *native* devices is met by 12.2 (direct internet connect) and 12.3 (hotspot); browsers still use the relay. |
| 2 | Host restart stamps | **done** — see 12.4. The "edited while you were away" list is in the Changes window. |
| 3 | Autosave dirty tracking | **done (hash-based, option "or a hash of the changed pages")** — see 12.5. Serialising on a worker thread is still open. |
| 4 | `yrs` text CRDT | **not done** (deliberately): the three-way merge of 10.2 keeps both edits; `yrs` would need an editor adapter that diffs egui's `TextEdit` string into Y operations and rewrites the caret on remote edits — a rewrite of the text editor, which cannot be verified without a UI test rig. Kept as the top item of 12.7. |
| 5 | Suggest-edit mode, per-person diff colours, revert | **done** — see 12.4. |
| 6 | Big-table sync, binary framing, host hand-over, Wi-Fi Direct / Nearby Share | **Wi-Fi hand-off done in another way** (12.3: LocalOnlyHotspot — Nearby Share / Wi-Fi Direct have no public API for third-party apps). Big tables, binary framing, hand-over: open. |
| 7 | Keystore-wrapped device key | **done** — `NearbyKit.keyWrap/keyUnwrap` (AES-256-GCM key that never leaves the Keystore); the device key is stored as `ks1:<hex>`; an older plain key is wrapped on the first start. CameraX: **not done** (kept `android.hardware.Camera`: zero dependencies, API 26–34). *(device-only)* |
| 8 | Typing badges | **done** (12.1a). Read receipts for comments and push while the app is closed: **not done** (push needs a server that can reach FCM / a background service; the relay is stateless by design). |

### 12.1a Realtime and cursors — what was actually wrong
Reading the path from keystroke to the other screen found four causes of "unreliable":
1. **Latency of the text itself.** A typed character waited for the next adaptive full scan (160 ms … 1.5 s, 90 ms after `poke`). Now the block being typed in is scanned **every frame** (`Replica::tick_block`, one serialisation of one block) and leaves in the same frame.
2. **The caret outran the text.** `Presence` (carets) travelled immediately, the text edit a scan later, so the caret was drawn against a text that was a few characters short and jumped. `Presence.len` carries the text length at the time the caret was taken; the painter shifts the caret by `local_len − len` (typing / deleting happens at the caret, so the missing characters are before it) instead of clamping.
3. **No sign of life.** `Presence.typing` (set for 2 s after an edit; the editor schedules the repaint that clears it) drives a "· typing…" flag on the remote caret and "typing…" in the people list.
4. Everything else (heartbeat 3 s, stale after 12 s, host fan-out, sanitising) was already correct and is unchanged.
The pass also fixed that a device's presence is no longer hidden while the app polls: `Collab::poll` now also runs while a Wi-Fi join or the hotspot is pending.

### 12.2 Direct internet connect (peer-to-peer, no relay)
* **Opt-in**: *Share → Internet access*. Off by default; nothing is exposed until the host switches it on.
* **How**: the host maps its TCP port on the router with **UPnP-IGD** (`collab/upnp.rs`, no dependency: SSDP → description XML → SOAP `AddPortMapping`, lease 2 h with a lease-0 fallback, removed again when the session ends) and learns the public address from the router (`GetExternalIPAddress`) and from this site (`GET /api/collab/ip` answers with the caller's address). A router address in 10/8, 172.16/12, 192.168/16, 100.64/10 means CGNAT / double NAT: the UI says so and points to the relay instead of pretending.
* **The link carries every way in**: `lumen://join?h=192.168.1.5:47615,203.0.113.9:40000&t=TOKEN[&ws=SSID&wp=PASS]`. The joiner dials **all candidates at once** (`net::Net::connect`, one thread each, 5 s timeout) and keeps the first TCP connection that completes — same-Wi-Fi joins stay as fast as before, cross-network joins work from the same QR. Typed addresses accept the same comma lists.
* **Security** (unchanged protocol, now reachable from outside): Noise XX with the share token bound to the channel, host approval card with a 4-digit channel code to compare, ≤ 48 connections / 6 per address, 8 s handshake deadline, wrong-token lock-out after 5 tries, banned / demoted keys. A port scanner finds a WebSocket server that closes anything that is not a valid Lumen handshake. The mapping exists only while the session runs.
* **Not verified here**: this build machine has no router. UPnP request / response parsing, the link format and the multi-candidate dial are unit-tested (`upnp::tests`, `a_device_dials_every_candidate_…`); the router conversation itself needs a real router.

### 12.3 "AirDrop for two phones on different networks": private hotspot hand-off *(device-only)*
Android allows an app to open a **LocalOnlyHotspot** (private access point without internet) and to join a specific network with `WifiNetworkSpecifier` — no Wi-Fi settings screen. `NearbyKit.java`:
1. Host: *Share → Nearby device without shared Wi-Fi* starts the hotspot (asks for *Nearby devices* / location once). The Direct QR now contains the join link **and** the hotspot's SSID / password (`ws`, `wp`), and the host's interface addresses (`NetworkInterface`, hotspot included) are added as candidates.
2. Joiner: scans the code with Lumen's scanner (Android 10+) → `join_via_wifi` → Android shows its one-tap "connect to …" sheet → the process is bound to that network → the candidates are dialled. Android 8/9 joiners are told to join the Wi-Fi in Settings and scan again.
3. Hand-off time is dominated by the OS Wi-Fi association (a few seconds); after it, the usual encrypted join + approval. Desktop joiners join the shown SSID manually and use the link.
Limits: one hotspot at a time; some phones refuse a local-only hotspot while tethering or Wi-Fi Direct is active (the reason is shown); while bound to the hotspot the app has no internet (it is released on *Stop sharing*). Compile-verified and packaged; nobody has held two phones to each other in this pass.

### 12.4 Collaboration features
* **Restart stamps.** Entities adopted without operations (`Replica::adopt`) were all stamped with the zero clock, so after a host restart a joiner's offline edit and the host's copy were separated by a hash coin-flip. They are now stamped with their page's `updated` time (capped at "now"), on both sides, so the later edit wins deterministically and untouched pages produce no traffic. Granularity is the page: if both sides edited *different blocks of the same page* while apart, the page with the later `updated` wins for the blocks that differ (an ordinary reconnect without a restart keeps both, as before). Test `restart_stamps_let_the_newer_page_win_…`.
* **Edited while away.** When a joiner re-opens a share, the pages it edited since the last sync are listed in the Changes window (click to open) and counted in the badge.
* **Suggest-edit mode.** New role `Suggester` (host approval card: *Edit / Suggest / View / No*; per person: *Make editor / Suggest only / Make viewer*; protocol 4). The device edits normally; the host does **not** apply its changes: they are described (what was there / what is proposed), folded per block, and shown at the top of the Changes window with *Accept / Refuse* (and *all*). *Accept* applies the operation and fans it out; *Refuse* makes the host **re-assert** its own value with a newer clock (`Replica::reassert`), so the suggester's copy snaps back. Comments and PDF marks are not held. Suggesters are not remembered as trusted devices (they are asked again). Test `a_suggestion_waits_for_the_host_and_a_refusal_restores_the_joiner` (real sockets: held, host unchanged, refused → restored, second suggestion accepted → applied).
* **Revert this change.** Every logged block change keeps the block as it was (JSON ≤ 16 KiB, formatting included); *Revert* restores it, deletes a block that was added, or re-inserts one that was removed (at the end of the page). Test `revert_restores_the_block_…`.
* **Per-person colours** in the Changes window (bar, dot and name in the person's colour, also for suggestions).

### 12.5 Autosave
`save()` used to serialise the whole workspace every 30 s, read the previous copy back for the safety rotation and rewrite the key. Now the state is streamed through an FNV hasher (`serde_json::to_writer` into a `Write` that hashes — no allocation, no I/O); if the fingerprint equals the last saved one the workspace, the rotation logic and the storage write are skipped. An idle app therefore costs one hashing pass per 30 s and **zero** writes (localStorage on the web, flash on phones). A failed serialisation just means "save". Still open: the hashing pass runs on the UI thread, and a truly incremental save (per page) needs a store that is not one JSON value.

### 12.6 What is not done, honestly
* **WebRTC** (browser ↔ browser without the relay). Needs `RtcPeerConnection` / `RtcDataChannel` through `web_sys`, signalling over `/api/collab` and a `Sink` for the channel (~300 lines, design in 11.2 #1). It can only be verified in two real browsers; this environment has none, and an unverified transport inside the shipped wasm bundle is a risk to the working web app. Browsers keep using the relay (which is end-to-end encrypted, so the relay never sees content).
* `yrs`, big-table sync, binary framing, host hand-over, CameraX, comment read-receipts, push while closed (see 12.1).
* Verification gaps: UPnP against a real router, hotspot / `WifiNetworkSpecifier` / Keystore on real phones. `tests::right_click_keeps_selection_and_opens_menu` fails in this headless environment **before and after** these changes (it fails identically on the untouched 14.4 tree; it depends on font metrics); the other 288 library tests pass.

### 12.7 Review — what more can be done (ranked)
1. **WebRTC data channels** + a tiny STUN list: browser↔browser, and the missing piece for hole-punching between two phones behind NATs without UPnP (native: `webrtc-rs` or a hand-rolled UDP punch with the relay as the rendezvous).
2. **`yrs` Y.Text per text block** with a `TextEdit` adapter — real intention preservation and exact remote carets (today's carets are aligned by length).
3. **Per-block stamps on the host** (persist `(hlc)` per entity next to the workspace) so a restart does not fall back to page granularity.
4. **Autosave off the UI thread**: snapshot-by-pages (`Arc<Page>` copy-on-write) and a store with one record per page.
5. **BLE advertising + QR-less pairing** (Android `BluetoothLeAdvertiser` carrying the share id; the hotspot credentials delivered over GATT behind a numeric comparison) — the real AirDrop feel; today a QR scan starts the hand-off.
6. Big tables chunk-wise, binary framing (postcard), host hand-over / mesh.
7. Comment read-receipts (`Presence` carries the highest comment id seen), "revert" for pages and canvas cards, suggestions for canvas edits (today they are listed as "(canvas change)").
8. Push while the app is closed: FCM relay worker; or a foreground service on Android with the relay long-poll.
