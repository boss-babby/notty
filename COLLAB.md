# Local collaboration — design, status and next steps (Lumen 14.1)

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
