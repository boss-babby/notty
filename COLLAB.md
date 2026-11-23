# Local collaboration — design, status and next steps (Lumen 14.7)

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


## 13. Pass 7 — NAT hole punching, binary framing, off-UI-thread work, stamps, receipts, revert (14.6)

Input: the §12.7 review list (and what was still open from §11.2). Every item below says what was done, how it was verified and what is
left. Test counts are from `cargo test --lib` on this build machine (Linux x86-64, no phones, no router, no second browser).

### 13.1 Status of the §12.7 list

| # | Item | State |
|---|---|---|
| 1 | WebRTC + STUN / hole-punching | **Native hole punching: done** (13.2). **Browser WebRTC: not done** (needs two real browsers; the browser keeps the relay, which is end-to-end encrypted). |
| 2 | `yrs` Y.Text per block | **not done, deliberately** — see 13.8. |
| 3 | Per-block stamps persisted | **done** (13.4). |
| 4 | Autosave off the UI thread | **done** (13.5) — snapshot-by-clone + worker thread. Copy-on-write `Arc<Page>` and a per-page store: **not done** (13.8). |
| 5 | BLE advertising / QR-less pairing | **not done** (13.8): radio code that nobody can test without two phones is not shipped in the APK. |
| 6 | Big tables chunk-wise, binary framing, host hand-over | **Binary framing: done** (13.3). Big tables and hand-over: **not done** (13.8). |
| 7 | Comment read-receipts, revert for pages / cards, canvas suggestions | **done** (13.6). |
| 8 | Push while the app is closed | **Android foreground service: done** (13.7). FCM relay worker: **not done** (13.8). |
| – | "Separate the load from the UI thread" | **done where it was measurable**: sealing (13.3) and autosave (13.5) moved to worker threads; sockets already were. See 13.9. |

### 13.2 Direct connection through NATs (`collab/punch.rs`)
* **STUN** (RFC 5389 binding request, ~60 lines, no dependency): the device asks `stun.l.google.com`, `stun1.l.google.com`, `stun.cloudflare.com`,
  `global.stun.twilio.com` (in that order, 2 tries each, 600 ms) for the public address of one UDP socket. Only the address is learnt.
* **Rendezvous** on the site: `POST /api/collab/punch {room, role, c, a}` stores an address under a random cookie (60 s TTL, ≤ 16 per room, address
  syntax validated); `GET ?room=R&role=h` lists the open offers (host) and `GET ?room=R&c=C` returns the host's answer (joiner). The site sees two
  addresses and a cookie — never session data, keys or tokens.
* **Flow**: the host (which already has a relay room) polls the room every 1.5 s; a joiner whose link carries `&p=1` posts an offer, the host answers
  with its own mapped address, **both sides send probes at each other every 120 ms** (this opens both NAT mappings) until a packet with the right
  cookie arrives (≤ 7–8 s). The joiner tries this *before* the relay; if it fails it silently falls back to the relay (`Collab::start_relay`), so the
  worst case costs ≈ 13 s, the usual case is a few hundred ms. Reconnects repeat the same sequence.
* **Reliable stream** (`Rudp`): a pure state machine — length-prefixed messages cut into 1 100-byte segments, 64-segment window, cumulative acks,
  retransmission with exponential back-off (220 ms ⇒ ×16), out-of-order reassembly, 512 KiB message cap, 25 s idle timeout, 4 s keep-alives (which also
  keep the NAT mapping open), FIN. The cookie in every header makes blind injection from outside the path impossible; anything else is stopped by the
  Noise channel and the token proof, exactly as on TCP.
* **Session integration**: the link is just another `Sink`; `Event::Open` → the usual encrypted handshake, Hello, host approval card.
* **Tests (all pass)**: STUN parse (XOR-MAPPED-ADDRESS, wrong transaction, truncated); the stream delivers 26 messages incl. a 300 kB one **in order over
  a link that drops 30 % and duplicates / reorders** (deterministic PRNG); wrong cookie / oversize frame refused; two sockets punch each other on
  loopback and a wrong cookie never opens; wire encoding round trip.
* **Honest limits**: symmetric NATs (many mobile carriers) assign a new port per destination, so the STUN-learnt port is wrong and the punch fails —
  the relay fallback handles that case. A TURN-style relay for UDP is not offered (the HTTP relay plays that role). Not verified against real NATs
  here: this machine has none; the parts that can be checked without one are the tests above.

### 13.3 Binary framing and the sealing worker (protocol 5)
* `Msg::Bin { d, more }` carries an encrypted frame as raw bytes. TCP sends it as a **binary WebSocket frame** `[more][Noise message]`, the punched UDP link
  as `[1][more][bytes]`; text transports (relay, browser) convert with `Msg::to_text` / `Msg::from_text` (base64 only there). Savings on LAN: the 33 %
  base64 overhead and the JSON envelope, plus one base64 pass per frame on each side. The browser build understands binary frames too
  (`binaryType = arraybuffer`), so it can still join a native host directly.
* **Sealing no longer runs on the UI thread** (native): `SecureSink` owns one thread per connection; `send` clones the message into a channel, the thread
  serialises, deflates (CPU-heavy part, outside the cipher lock), seals and hands the frames to the transport. Jobs are processed in order, so nonce order
  = wire order; the messages queued during the handshake are sealed by the same thread (`flush_pending`), which removes the only place the order could
  have raced. The browser has no threads and seals inline.
* `PROTO` is **5**: a 14.5 peer cannot read binary frames and is refused with the usual "incompatible version" message.
* Tests: `the_sender_thread_keeps_wire_order_and_everything_decrypts_in_order` (200 messages), `text_transports_carry_binary_frames_as_base64…`,
  all earlier secure / session tests (the end-to-end sockets test now runs over binary frames).

### 13.4 Per-block stamps survive a restart
`Replica::export_stamps` / `import_stamps`: one line per live entity (`kind a b t r content-hash`, ≤ 400 000). The session writes them next to the
workspace (egui persisted data, key `("collab_stamps", share-id)`) every 2 minutes and when a session ends, and restores them right after `adopt`:
an entity gets its **real** clock back when its content hash is unchanged; anything edited while the app was closed keeps the page-level stamp of
`adopt`. Stamps are clamped to the adoption time. Applies to hosts (`start_host`) and to joiners that re-open a share (`bind_existing`).
Test: `stamps_survive_a_restart_per_block_and_edits_made_while_closed_fall_back_to_the_page`.

### 13.5 Autosave off the UI thread (`src/autosave.rs`)
While frames are being drawn (focused window, not closing, last frame < 2.5 s ago) `save()` clones the workspace (no formatting, no hashing) and hands it
to one worker thread, which fingerprints it (FNV over the JSON stream) and, only if it changed, produces the RON text (`eframe::encode_value`, added to
the vendored eframe). The UI thread stores the finished string in the next frame (`Storage::set_string` + `flush`, eframe writes the file on its own
thread). **Every path that could lose data stays synchronous**: window closing, focus lost / app in the background, an idle app (nothing is typed, so
nobody sees the cost), the browser (no threads: the "worker" runs inline). A job in flight is awaited before a synchronous save, so an older result can
never overwrite a newer one. Test: `a_snapshot_is_serialised_on_the_worker_and_an_unchanged_one_is_skipped`.
Not done: `Arc<Page>` copy-on-write snapshots (the clone is O(workspace) memory traffic, ~1 ms per MB) and one storage record per page.

### 13.6 Review features
* **Read receipts.** `Presence.seen = (page, newest comment time ms)`, set while the comments window is open; the author of a comment sees
  "✓ Seen by Ana, Ben" under it. (A timestamp, not "the highest id": ids are per device, so they cannot be ordered across peers.) Protocol field is
  optional and skipped when empty.
* **Revert for pages and canvas cards.** Canvas cards are now itemised in the Changes window (`Card`, one readable line: kind, text, position, size,
  colour — a move or resize shows as a changed line) instead of being counted. *Revert*: added card ⇒ removed (with its edges), edited / removed card ⇒
  restored from the stored JSON; added page ⇒ moved to the trash; edited page ⇒ title / icon / settings restored (content, history, sidebar state and
  tree position stay). A removed page cannot be restored (its content is not kept). Test: `revert_restores_cards_and_pages` and the existing block test.
* **Canvas suggestions** are described like blocks (`Card` before / after line) instead of "(canvas change)"; edges and PDF marks read "(changed)" /
  "(removed)".

### 13.7 Android: session stays alive in the background
`CollabService` (foreground service, type `dataSync`, ongoing low-importance notification, partial CPU wake lock for ≤ 4 h and a Wi-Fi lock) runs exactly
while a session is hosting or joined (`Collab::poll` follows `mode`; `LumenActivity.collabService`). Without it Android freezes the process shortly after
the screen turns off and the relay long-poll / UDP path die. Manifest: `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_DATA_SYNC`, `WAKE_LOCK`. Compile- and
package-verified only (no phone here). FCM push for a *closed* app needs Google services and a server-side sender; it is not part of this build.

### 13.8 Not done — and why (ranked for the next pass)
1. **Browser WebRTC data channel** (`web_sys::RtcPeerConnection`, signalling through `/api/collab`). Needs two real browsers to verify; the relay is the fallback.
2. **`yrs` / Y.Text per text block.** The three-way merge (10.2) keeps both edits; real intention preservation and *exact* remote carets need an editor
   adapter that diffs egui's `TextEdit` string into CRDT operations and rewrites carets on remote edits — a rewrite of `editor.rs` that cannot be
   verified without a UI test rig. Cheaper intermediate step: map remote carets through the common-prefix/suffix diff of the text instead of aligning by length.
3. **BLE advertising + GATT hand-off with a numeric comparison** (AirDrop feel). Needs two phones; also needs the `BLUETOOTH_ADVERTISE/SCAN/CONNECT` permission flow.
4. **Big tables chunk-wise** (`lumen-store` zone maps), **host hand-over / mesh**, **UDP TURN-like relay** for symmetric NATs, **FCM push worker**.
5. `Arc<Page>` copy-on-write snapshots and a per-page store (13.5); WebRTC-free browser hosting.

### 13.9 Where the work runs now
| Work | Thread |
|---|---|
| socket read / write, WebSocket framing, UDP read / write, retransmission | one thread per connection (existing; UDP new) |
| JSON serialisation, deflate, AEAD sealing | **one sealing thread per connection (new)** |
| hole-punch rendezvous, STUN, host listener | their own threads (new) |
| autosave fingerprint + RON text | **autosave worker (new)** |
| decrypt, JSON parse, CRDT apply (≤ 4 ms / frame), change scan (≤ 2.5 ms / frame) | UI thread — it mutates the `Workspace` the UI draws, so it stays there and is time-boxed |
Idle cost is unchanged: no thread spins; sealing and autosave threads block on channels, the host listener polls once per 1.5 s only while a relay room is offered.


## 14. Pass 8 — host hand-over, UDP relay for symmetric NATs, browser WebRTC, caret mapping (14.7)

Input: §13.8. Every item says what was done, how it was verified and what is left. `PROTO` is now **6** (three new messages; a 14.6 peer is
refused with the usual "incompatible version" message). Verified on this build machine (Linux x86-64, no phones, no router, one headless
host): `cargo test --lib` (new tests listed below), `cargo check` for the library on all four targets (native, `wasm32-unknown-unknown`,
`aarch64-linux-android`, `x86_64-pc-windows-gnu`), the three release builds and the site's HTTP endpoints (see 14.7).

### 14.1 Status of the §13.8 list

| # | Item | State |
|---|---|---|
| 1 | Browser WebRTC data channel | **done, compile-verified + signalling verified over HTTP; not verified between two real browsers** (14.4) |
| 2 | `yrs` / Y.Text per block | **not done, deliberately.** The agreed *cheaper intermediate step* **is done**: remote carets follow edits through the common-prefix/suffix diff (14.5) |
| 3 | BLE advertising + GATT hand-off | **not done** — needs two phones and the `BLUETOOTH_*` permission flow; shipping untestable radio code in a production APK is a crash risk (14.8) |
| 4a | Host hand-over | **done**, end-to-end test over real sockets with three devices (14.2) |
| 4b | UDP TURN-like relay for symmetric NATs | **done**: `collab/turn.rs` + `lumen-turn` binary + client fall-back; tested over real UDP sockets (14.3) |
| 4c | Big tables chunk-wise (`lumen-store` zone maps) | **not done** (14.8) — big tables are still not part of a shared session |
| 4d | Mesh | **not done**; hand-over covers the "host leaves" case, the topology stays a star |
| 4e | FCM push worker | **not done** — needs Google credentials and the Firebase SDK in the APK (14.8) |
| 5 | `Arc<Page>` copy-on-write + per-page store | **not done** (14.8) |

### 14.2 Host hand-over (protocol 6)
* **Why**: the session used to end when the host left (the joiners kept their copy). Now the host can pass the baton.
* **Flow**: host → successor `Baton{leave}` · the successor (an active *Editor*, default the lowest id) starts listening **without dropping its
  link to the old host** (`Net::listen_with(keep)`), mints a **new share token**, and answers `BatonAck{addrs, relay, token}` · the old host
  validates the answer (`valid_target`: ≤ 12 plain `host:port` strings, an http(s) site + room code, alphanumeric token) and sends
  `Moved{…}` to everybody else · every other device — and the old host itself unless it chose *leave* — tears its transports down and re-dials the
  new host with the **ordinary resume** (replica, workspace and session id stay; the digest exchange repairs whatever differs).
* **Security**: the old token is retired, so a device that was removed earlier cannot walk back in with it; the roles of the old roster are
  inherited (`inherited: id → Role`) and applied to devices that come back *with the new token* (Viewers stay Viewers, Suggesters stay Suggesters)
  without a new approval card for each person. Banned-key and demotion lists do not travel (the successor does not know other people's channel
  keys) — the token rotation is what keeps removed devices out. Held suggestions that had not been decided yet are discarded (stated in the toast text of the host).
* **Timeouts**: no answer within 8 s → "you are still the host", nothing changed. A successor that cannot host (a browser without a relay URL)
  answers with an empty address list.
* **UI**: People drop-down → *Hand over hosting & leave* (shown to a host with at least one connected editor).
* **Tests**: `the_host_hands_the_session_over_and_everybody_follows` (host + 2 joiners over loopback: modes swap, roster is complete, the token
  changes, an edit typed afterwards reaches both the new and the old host) and `hand_over_targets_are_validated`.
* **Not done**: automatic hand-over when the host's process dies (needs a failure detector and a deterministic election — the mesh item);
  hand-over with several simultaneous successors; carrying pending suggestions.

### 14.3 UDP relay for symmetric NATs (`collab/turn.rs`, `lumen-turn`)
* Both ends send the packets of their reliable stream (`Rudp`) to the relay instead of to each other. The relay routes by the 64-bit cookie in
  bytes 1..9 — the one the devices already agreed on through the site: the first two distinct source addresses become an *allocation*, a third is
  refused; datagram shape is checked (17..1400 bytes, type ≤ 3, cookie ≠ 0), 256 KiB/s per allocation (512 KiB burst), ≤ 4 096 allocations, 60 s idle
  expiry. The payload is the Noise channel, so the relay sees opaque bytes.
* **Client**: after the direct punch fails (joiner 7 s, host 8 s) both sides punch *at the relay* (`punch()` with `peer = relay`, 9 s) and run the same
  link (`run`) — no new session code. The relay address is `LUMEN_TURN` (environment of the device) or the `turn` field the rendezvous returns when the
  site has `LUMEN_TURN_ADDR` set.
* **Run it**: `lumen-turn --bind 0.0.0.0:3478` on any machine with a public UDP port (it is a normal `[[bin]]`, std-only). The Next.js site cannot
  host UDP itself, so the relay is a separate small process.
* **Tests**: routing (join / third address / cookie isolation), malformed datagrams + idle expiry, flood throttle, and **two sockets that only know the
  relay** exchange a 23-byte and a 5 000-byte message through `Rudp` over real UDP.
* **Not done**: skipping the 7 s direct attempt when the NAT is known to be symmetric (two STUN servers returning different ports) — an optimisation;
  authentication of allocations beyond the cookie; IPv6.

### 14.4 Browser WebRTC (`collab/rtc.rs`, site route `/api/collab/rtc`)
* A host *in a browser* answers SDP offers; the join link carries `r=1`. The joiner first asks `?cap=1` (is a browser host polling?), then offers,
  waits for the answer, and uses the open data channel as a normal `Sink` (binary messages `[more][Noise message]`, text for the rest). If anything
  fails within ~12 s the **relay takes over** (same `PunchJoin` fall-back as native hole punching). Native hosts ignore `r=1`; native joiners ignore it too.
* The ICE agent uses public STUN, no TURN. The RTCPeerConnection plumbing is ~80 lines of `inline_js` instead of a long `web-sys` feature list.
* **Verified**: the wasm build compiles and links; the signalling endpoint is exercised over HTTP (offer → answer, capability flag, TTL, validation).
  **Not verified**: a real connection between two browsers — there is no browser pair on this machine.

### 14.5 Remote carets through the text diff
`map_index(old, new, idx)` maps a caret through the single contiguous edit between two texts (common prefix / suffix); `track_caret` keeps, per
(person, block), the text the caret was last drawn against and moves it with every later change of the block (remote, local, a third device), and
re-bases on each new presence report (length shift as before for a report that is ahead of the text). Tests: `carets_follow_edits_through_the_common_prefix_and_suffix`,
`a_tracked_caret_moves_with_remote_text_and_resets_on_a_new_report`. Still approximate for two simultaneous edits in different places of one block
(only one contiguous edit is modelled) — exactness needs the CRDT adapter of §13.8 #2.

### 14.6 The website (this repository's `src/`)
`/api/collab` (relay: rooms, per-device keys, long poll, limits of §9.3, a device only sees frames posted after it first appeared),
`/api/collab/punch` (addresses + optional `turn`), `/api/collab/rtc` (SDP signalling), `/api/collab/stats` (aggregate counters), the landing page with
the Android / Windows downloads and the embedded WebAssembly app, and the rewrites `/app` and `/webapp` → `public/webapp/index.html` (so
`/app?join=ROOM&t=TOKEN` links open the app with the join parameters). Everything is in memory; there is no database use.

### 14.7 Verification log (this machine)
See `RELEASE-14.7.md` for the exact commands, artefacts and sizes of this delivery.

### 14.8 Not done — why, and what to do next (ranked)
1. **Big tables in a session.** Rows of big tables live in `lumen-store`, outside the entity CRDT, so they are not shared at all (the page and its columns are).
   The pieces exist: rows carry `updated` (last-writer-wins per row) and the store is chunked by id range (`CHUNK_ROWS` = 4 096). Plan: per-bucket digest
   `(bucket, rows, FNV(id, updated))` cached by `bigtable::version`, `BigDigest / BigGet / BigChunk` messages (chunks via `encode_chunk`), row tombstones for deletes,
   the same viewer / suggester gates as `Ops`, and joiners creating a volatile store for an unknown table. Needs a UI test rig for the creation path and a 1 M-row soak test.
2. **BLE pairing (AirDrop feel).** Advertise a 16-byte session hash only; GATT hand-off of the join link sealed with an ECDH key (X25519 through Rust over JNI);
   both phones show the same 6-digit code, the host taps *Accept*. Needs two phones.
3. **`yrs` / Y.Text** per text block (real intention preservation, exact carets): an editor adapter that diffs egui's `TextEdit` string into CRDT operations.
4. **FCM push** for a closed app: Firebase in the APK (needs `google-services.json`) + a sender route on the site with a service-account key.
5. **Mesh / automatic election** when the host's process dies; skip-direct-punch on symmetric NAT detection; `Arc<Page>` copy-on-write snapshots and a per-page store.

## 15. Pass 9 — big tables in a session, automatic fail-over, pairing core, push relay, symmetric-NAT skip (14.8)

Input: the §14.8 list. `PROTO` is now **7** (five new messages: `BigDigest`, `BigGet`, `BigChunk`, `Standby`, `StandbyAddr`); a 14.7 peer is
refused with the usual "incompatible version" message. Everything below says what was done, how it was verified, and what is left.

### 15.1 Status of the §14.8 list

| # | Item | State |
|---|---|---|
| 1 | Big tables in a session | **done and verified** (15.2): per-bucket digests, chunk transfer, tombstones, role gates, live edits, joiner creates its store, collision-free row ids |
| 2 | BLE pairing | **core done and tested** (15.4): advertised session tag, X25519 + 6-digit code + sealed link + GATT fragmenting. **Not done:** the Java GATT glue and the pairing screen — they need two phones and `BLUETOOTH_*` permissions; untestable radio code is not shipped in the APK |
| 3 | `yrs` / Y.Text | **not done, deliberately** (15.7). Concurrent typing in one block keeps the three-way merge of §10.2 |
| 4 | FCM push | **site half done and tested over HTTP** (15.5): token registry + FCM HTTP v1 sender. **Not done:** the Firebase SDK in the APK (needs your `google-services.json`) |
| 5 | Mesh / automatic election | **automatic election done and verified** (15.3) · **skip-direct-punch on symmetric NAT done** (15.6) · `Arc<Page>` copy-on-write and a per-page store **not done** (15.7) |

### 15.2 Big tables (`collab/bigsync.rs`)
* **Index.** Row ids are cut into buckets of 512 ids. A bucket is `(live rows, Σ mix(id, updated, content hash))` — a *commutative* sum, so
  an edit updates one bucket in place. 1 M rows ≈ 2 000 buckets. Tombstones (`id`, deleted-at ms) are inside the sum, so "deleted" ≠ "never had".
* **Exchange.** After the entity digest each side sends `BigDigest` per shared table (1 500 buckets per frame), compares the other side's
  with its own and asks for the buckets that differ (`BigGet`); the answer is `BigChunk` = `lumen_store::encode_chunk` (base64) + tombstones.
  Both sides do it, so the exchange repairs in both directions. Buckets are produced **lazily** (two per connection per frame while that
  connection's backlog is short): a million-row table neither floods the socket nor the memory.
* **Merge** per row: newer `updated` wins; same millisecond → higher content hash wins (identical on every device); a tombstone beats a row
  with the same or an older stamp, a newer row beats an older tombstone ("re-created" wins). Re-applying a chunk is idempotent.
* **Live edits.** `bigui` calls `bigtable::note_write / note_delete` (no-ops outside a session); each frame the session turns the queued ids
  into small `BigChunk`s (`b = u64::MAX`). The host relays what it *accepted* to the other devices (never back to the sender); more than
  20 000 edits between two frames fall back to a digest exchange.
* **Gates = the gates of `Ops`.** The host drops chunks from viewers and suggesters, only serves / accepts tables whose page is in the scope of the
  share (`Replica::page_in_scope`) and never creates a store for a guest. A joiner creates a (volatile in the browser, on disk elsewhere)
  store when the host announces a table whose database page it already has (digests wait up to 90 s for the page). Hostile input: base64 /
  chunk decoding errors are dropped, ≤ 6 MiB per frame, ≤ 2 000 rows and ≤ 4 000 tombstones per chunk, ≤ 400 000 digest items per table.
* **Row ids.** In a session `+ Row` takes its id from the device's private id space (`bigtable::new_row_id`, like pages and blocks), so two phones
  adding a row at the same moment cannot produce the same id.
* **Index build** of a table over 30 000 rows runs on a worker thread (native); the UI never waits for it.
* **UI.** The table view re-reads when the shared data version changes; a toast reports "Big tables are in sync (N rows received)";
  `Collab::big_status()` has the progress line.
* **Tests.** `bigsync::tests` (5: merge rules, tombstone rules, two 3 000-row stores converge through digest + chunks incl. a deletion and older
  rows, an edited bucket recomputes to exactly the full-scan value, hostile chunks), `collab::tests::a_big_table_is_shared_edited_and_deleted_live`
  (host + joiner over real sockets: 6 000 rows arrive in a store of their own, a host edit, a joiner delete and a joiner add arrive live) and
  the ignored `soak_one_million_rows` (`cargo test --lib soak -- --ignored --nocapture`; figures in RELEASE-14.8.md).
* **Not done / limits.** Tombstones are kept in memory (200 000 per table, oldest dropped): a row deleted offline and then a restart can be
  brought back by a peer that still has it. A suggester's direct edits to a big table are dropped, not held for approval. Row edits are
  not merged cell-by-cell (the newer whole row wins).

### 15.3 Automatic fail-over (`Standby`, `Collab::elect / promote`)
* **Plan.** The host sends every device `Standby{host, order, addrs, secret, cap, relay}`: `order` = the editors by id (max 6),
  `addrs` = where each reported it would listen (`StandbyAddr`; a device listens on `47700 + id % 250` when it becomes host), `secret` = a random
  key (editors only), `cap = SHA-256(secret, device id)` = the token *this* device proves to the successor, `relay` = the site and a room code
  derived from the secret (so a browser / relay-only session can fail over too). Re-sent whenever the roster changes; **a kick throws the secret
  away** and mints a new one, so a removed device can never open the successor's door.
* **Detection.** Unchanged (socket close, 45 s heartbeat). New: a re-dial that is *refused* now counts at once (`on_closed` used to ignore it
  until the 25 s `reap` timer) — reconnects after a host restart are quicker too.
* **Election.** After two failed re-dials the first editor of `order` that is not known to be dead becomes the host: if that is this device it
  **promotes itself** (same replica, workspace and session id; new share token; roles inherited; the vanished host leaves the roster; listens; opens
  the derived relay room); every other device re-points its resume target at the candidate and dials with its `cap` (the successor accepts it
  because it knows the secret and the device id — Viewers and Suggesters follow too, with *their* cap, and keep their roles). A candidate that
  does not come up within 14 s is skipped and the next one is tried. The succession continues: the new host pushes its own plan.
* **Split brain.** In a network partition both sides may elect (like Raft without a quorum); the replicas merge by the normal rules when the
  partition heals and somebody re-joins. A device that is the only editor left simply keeps its copy.
* **Test.** `the_editors_elect_a_new_host_when_the_host_vanishes`: host + two editors over loopback, the host's sockets disappear without a goodbye,
  the lower-id editor becomes host, the other follows, an edit typed afterwards reaches the new host, the share code changed (8 s).
* **Not done.** Automatic election while a hand-over is in flight; candidates that are behind a symmetric NAT of their own (use the relay
  room); several simultaneous host deaths in a long chain.

### 15.4 BLE pairing — what exists (`collab/pair.rs`)
`session_tag(share)` (16 bytes, the only thing advertised), `Keypair` (X25519, small-order keys refused), `code()` (six digits bound to both
public keys: a man in the middle ends up with two different codes), `seal / open` (ChaCha20-Poly1305, key = hash of the shared secret; the
token is never visible), `fragment / Reassembler` (GATT MTU 20, any order, CRC). 4 tests incl. a MITM. **Missing:** `BlePair.java`
(advertiser + GATT server on the host, scanner + GATT client on the guest), the runtime permission flow (`BLUETOOTH_ADVERTISE/CONNECT/SCAN`),
the egui "Nearby by Bluetooth" panel with the code and *Accept*. Next step: write them against the six functions above.

### 15.5 Push for a closed app (site: `/api/collab/push`)
`POST {room, from, k, token}` registers an FCM device token (the device must be a member of the room: the same per-device key as the relay);
`POST {room, from, k, notify:{title, body}}` wakes the *other* devices through FCM HTTP v1 — the site signs an OAuth JWT with the service account
in `FCM_SERVICE_ACCOUNT` (JSON or base64 JSON). Without the variable the route answers 501 `configured:false`; `GET` tells the app whether push is
available. Tested over HTTP (validation, membership, 501). **Not done:** the Firebase SDK + `google-services.json` in the APK, and the call
from the foreground-service-less path. Dead tokens (HTTP 400/404) are dropped.

### 15.6 Symmetric NAT: skip the direct punch
`nat_is_symmetric` asks two STUN servers with different addresses from one socket; different public ports ⇒ symmetric. When a relay (`turn`) is
known the 7–8 s direct attempt is skipped and both ends meet at the relay at once. Without a relay the direct attempt still runs.
Test: `a_nat_that_changes_the_port_per_destination_is_symmetric` (classification); the STUN calls themselves need the internet.

### 15.7 Not done — why
* **`yrs` / Y.Text.** An adapter that diffs egui's `TextEdit` string into CRDT operations has to run on every keystroke of every block and the
  entity model (`Key::Block` = one JSON value, LWW + three-way merge) would have to carry per-character state; I judged a half-wired CRDT more
  dangerous than the existing merge, which already keeps both people's edits when they type in different places of a paragraph. Plan: a
  `Key::Text(block, field)` entity that holds an RGA/Y.Text update log, the editor reads/writes through the diff adapter, the old merge stays as the
  fallback for old peers.
* **`Arc<Page>` copy-on-write and a per-page store.** Autosave already snapshots by clone on a worker thread; the win is memory only for
  workspaces with many huge pages. Needs a model refactor across ~60 files; deferred.

### 15.8 "Can two devices connect anywhere in the world with no lookup server at all?"
**Not with zero help, and nobody can.** Two devices behind home routers have no reachable address; *something* must hand one the other's address.
What Lumen does, from least to most outside help:
1. **Same network:** no server — UDP beacons / QR / link.
2. **Direct internet connect** (`Collab::set_internet`): UPnP opens a port on the host's router; the join link carries `ip:port`. **No relay, no
   rendezvous, no account** — the *link is the lookup*, delivered by you (chat, QR). It works when the router allows UPnP and has a public IPv4
   (not behind carrier-grade NAT). The site is only asked "what is my IP" (`/api/collab/ip`); any such service or the router's own UPnP answer can replace it.
3. **Hole punching** needs the two public addresses exchanged: the site's `/api/collab/punch` does that (two addresses and a cookie; never
   session data) and public STUN servers tell each device its own mapping. The exchange could also ride in the link / QR both ways (no site) —
   not built, it needs two scans.
4. **Relay** (site `/api/collab`, optional `lumen-turn`) is the last resort for symmetric NATs; it only moves sealed frames.
5. **True serverless discovery** would be a DHT (BitTorrent mainline: well-known bootstrap nodes, nobody owns it) plus IPv6 (no NAT at all: a global
   address in the link works with a firewall rule). Neither is built; IPv6 listening is the cheapest next step.
Everything is end-to-end encrypted (Noise XX) whichever path is used, so the helper never sees content.

### 15.9 Verification log (this machine, Linux x86-64, no phones, no second browser)
See `RELEASE-14.8.md` for commands, artefacts and sizes. `cargo test --lib`: 318 pass, 1 **pre-existing** failure
(`tests::right_click_keeps_selection_and_opens_menu` fails identically on the untouched 14.7 checkout in this headless environment).

### 15.10 Review — what more can be done (ranked)
1. **Phones in the loop:** the BLE glue (15.4) and the FCM SDK (15.5) are small once two phones and a Firebase project exist. A test rig that drives
   two real devices (adb + UI Automator) would also cover the foreground service, NSD and the hotspot hand-off.
2. **A real text CRDT** (15.7) with the editor adapter — the biggest remaining user-visible gap in simultaneous typing.
3. **Persist tombstones** of big tables (and a per-table checkpoint of the bucket index) so deletions survive restarts; cell-level merge of rows.
4. **IPv6 + DHT discovery** (15.8) for "anywhere in the world without our site".
5. **Quorum for the election** (three or more editors) to avoid split brain in partitions; sign the `Standby` plan with the host's long-term key.
6. **Per-document access:** share a page with a link that expires and can be revoked, with the roles kept in the encrypted plan.
7. **Large-scale polish:** memory-mapped chunk reads for tables over 10 M rows, background compaction after a big sync, delta-compressed `BigChunk`s.

## 16. Pass 10 — the §15.10 list: text CRDT, persisted tombstones, DHT/IPv6, signed plans, grants, phone glue (14.8 · this delivery)

Input: the seven ranked items of §15.10. `PROTO` is now **8**: `Welcome.spk`, `Standby.{spk,sig,seq,grants}`, `BigGet.have`, `BigChunk.cells`
(all `serde(default)`, but a 14.7 peer is still refused with the usual "incompatible version" message). Every item below says what was
built, how it was checked, and — just as important — what is **not** finished.

| # | §15.10 item | State |
|---|---|---|
| 1 | Phones in the loop (BLE glue, FCM SDK, two-device rig) | **done in the code, compiled into the APK, not run on radios**: `BlePair.java`, `FcmKit.java`, `tools/phone-rig.sh` (§16.6) |
| 2 | A real text CRDT (15.7) with the editor adapter | **done**: `collab/textcrdt.rs` (RGA) + adapter + `merge3` through the CRDT (§16.2). The live per-keystroke `TextOp` broadcast is the one piece still to wire |
| 3 | Persist tombstones, per-table index checkpoint, cell merge | **done**: `graves.log`, `bidx.lst`, `merge_cells`, delta `BigGet`, mmap reads, background compaction (§16.3) |
| 4 | IPv6 + DHT discovery | **done**: dual-stack listener, IPv6 beacons, `dht:CODE` through the BitTorrent DHT (§16.4) |
| 5 | Quorum + signed `Standby` plan | **done**: Ed25519 host key, pinned, signed plan, provisional host + step-down (§16.5) |
| 6 | Per-document access (expiring, revocable links with roles) | **core done and tested**: grants with role / expiry / uses, enforcement, revocation, carried in the signed plan (§16.5). **No UI card yet** |
| 7 | Large-scale polish (mmap, compaction, delta, compressed chunks) | **3 of 4**: mmap reads, compaction after a big sync, delta `BigChunk`s. Payload compression (deflate) not done (§16.3, §16.8) |

### 16.1 What changed in the tree

```
src/collab/textcrdt.rs  new   RGA text CRDT, editor adapter, anchors, Hub, merge3
src/collab/access.rs    new   Ed25519 host key, signed plan bytes, quorum rule, grants
src/collab/dht.rs       new   bencode + BEP-5 get_peers / announce_peer + announcer thread
src/collab/bigsync.rs   edit  persisted tombstones, index checkpoint, cell merge, delta, compaction
src/collab/mod.rs       edit  PROTO 8, signing, pinning, quorum, grants, DHT hooks, IPv6 hints
src/collab/textmerge.rs edit  merge3 now delegates to the CRDT (field merge unchanged)
src/collab/net.rs       edit  dual-stack listener, IPv6 multicast beacons + discovery
src/bigtable.rs         edit  graves.log replay/append, cell map, smaller memory floor
src/bigui.rs            edit  a cell edit is announced as a cell ((id, prop)), not as a whole row
lumen-store/src/lib.rs  edit  memory-mapped reads for data files ≥ 256 MB (`FileSource::with_mmap_min`)
android/…/BlePair.java  new   BLE advertiser + GATT server / scanner + GATT client for pair.rs
android/…/FcmKit.java   new   FCM token by reflection (APK builds without the Firebase SDK)
android/AndroidManifest edit  BLUETOOTH_SCAN (neverForLocation) / ADVERTISE / CONNECT + legacy pair
tools/phone-rig.sh      new   two-device adb + UI Automator rig (dry-run mode for CI)
```

### 16.2 A real text CRDT (`collab/textcrdt.rs`, 650 lines, 9 tests)

* **Model.** RGA: every character has an id `(counter, replica)`; an insertion names the character it was typed after (`origin`). The
  integration rule (skip concurrent siblings with a bigger id, then descend) gives the same string for the same operation set in **any**
  delivery order, duplicates are no-ops, and an operation whose origin has not arrived waits in a bounded `pending` list (causal delivery).
  Deletions are tombstones; `compact()` drops the ones no live character refers to.
* **Editor adapter.** `Doc::set_text(&str)` is the whole adapter: prefix/suffix trim, then a `u16` LCS over the changed middle, emitting
  *minimal, separate* runs (`…two separate edits → ≤ 4 operations`). It is what an egui `TextEdit` needs (the editor still owns a plain
  `String`); `Hub::local` / `Hub::remote` hold one document per open block.
* **Carets.** `anchor(index) -> Id` / `index_of(Id) -> index`: a caret is an id, so it moves with remote insertions and collapses sensibly
  when the character it followed is deleted (test `carets_follow_remote_edits`).
* **`merge3` through the CRDT.** Both sides derive their replica id from the content they produced (FNV of the string, collision-nudged),
  build a document from the common base, apply the other side's operations and return the text — deterministic, order-independent
  (`merge3(x, y) == merge3(y, x)`), and *both* overlapping edits survive instead of "smaller first" concatenation. `textmerge::merge3`
  now delegates to it, so **every** concurrent block edit in the session (`merge_block`) is resolved by the CRDT; the field-wise JSON merge
  around it is unchanged.
* **Tests.** convergence while typing at the same place (runs are not interleaved), reversed delivery + duplicates, insert into a range
  another device deleted, operations waiting for their origin, multi-hunk adapter diff + Unicode, carets, merge3 symmetry and
  30 000-operation randomised three-replica convergence.
* **Not wired (the remaining half of §15.7).** There is no `Msg::Text` frame yet: the editor keeps sending the block entity, and the CRDT
  is what merges it. That is already a strict improvement (overlapping edits are no longer lost), but per-keystroke operations would cut
  the round-trip size from a whole block to a few characters. The adapter and the document store are ready for it —
  §16.8 has the exact next step.

### 16.3 Big tables: tombstones that survive a restart, cell merge, checkpoints, delta, mmap, compaction

* **Persistent tombstones (`bigtable.rs`).** `graves.log` next to the store: 16-byte records `(row id, ms)`, ms = 0 undoes one. It is
  replayed once per process (lazily, before the first tombstone use), appended on every delete, and rewritten when ≥ 4/5 of it is dead
  records. `note_delete` now always records a tombstone (previously only during a session), so *deleting a row while alone* also survives
  a restart and cannot be resurrected by a peer that still has it. In-memory floor raised to 2 M tombstones per table.
* **Index checkpoint (`bidx.lst`).** Magic `BIX1`, a 64-bit *stamp* (data length, chunk count, WAL length, highest id, tombstone
  fingerprint) and the `(bucket, rows, signature)` table with an FNV checksum. The stamp is taken **before** the scan, so a write during
  the first index makes the next start rebuild instead of trusting a stale file; a corrupt or truncated file is ignored.
  A million-row table skips the full scan on every reconnect.
* **Cell-level merge.** `bigtable::note_cell(dir, id, prop)` records which properties (0 = title) this device changed per row (bounded,
  100 k rows). Live `BigChunk`s carry `cells`; the receiver merges row by row with `merge_cells(local, remote, remote_cells, local_cells)`:
  a cell only one side touched comes from that side, a cell both touched goes to the newer stamp (ties: the larger value, so both devices
  decide alike); the merged row is stamped `max + 1` so every *other* device adopts it by the plain newer-wins rule. Two people editing
  two columns of one row no longer overwrite each other.
* **Delta requests.** `BigGet.have` lists `(row id, signature)` of the rows the requester already holds for a bucket; the answer only packs
  the rows that are missing or different (test: 1 edited row out of 500 → a single row on the wire).
* **Memory-mapped reads (`lumen-store`).** Data files ≥ 256 MB (`FileSource::with_mmap_min`, lazy) are read through a read-only
  `memmap2` map instead of one `pread` per chunk; the map is dropped and re-created when reads pass its end and **before** `compact`
  replaces the file (a stale map after compaction returned old bytes — caught by the new test, fixed, and covered by
  `big_files_are_read_through_a_memory_map`).
* **Compaction after a big sync.** When one sync accepted ≥ 20 000 rows, `compact_in_background` rewrites the data files on a worker
  thread (native only; the browser store is a `Vec`).
* **Not done.** `BigChunk` payloads are still base64 of the raw chunk (no deflate) — the delta already removes most of the bytes, but a
  compressed frame would help the first sync of a million rows. Columnar cross-device merge of a *new* column is whole-row (the cells are
  merged only when the row exists on both sides).

### 16.4 Anywhere without our site: IPv6 + the BitTorrent DHT (`collab/dht.rs`, 4 tests)

* **Dual-stack listening.** `Net::listen_pref` binds `[::]` first (on Linux/Android that is dual-stack) and `0.0.0.0` on the same port when
  the platform keeps the families apart (Windows). IPv4-mapped peers are canonicalised to `a.b.c.d` so a device that dials both ways is
  one device.
* **IPv6 discovery.** Beacons also go to the link-local group `ff02::4c75:6d65` („lume“) on the same port and a second receiver listens
  for them; `local_addrs6()` adds global / unique-local addresses to the hints (link-local addresses are not advertised: they need a zone).
  Two IPv6 devices therefore meet with no NAT and no UPnP.
* **DHT.** A small BEP-5 client: bencode encode/decode (bounded depth and lengths), `get_peers` iterations towards
  `SHA-256("lumen-dht-v1" ‖ CODE)[..20]`, `announce_peer` with the write tokens, and a background `Announcer` that re-announces every
  9 minutes (5 minutes when it failed). The host announces when "Internet" is switched on; `dht:CODE` in the join field looks the code up
  and dials what it finds. The DHT learns an infohash and a public address — never the code, the session or a document; the Noise handshake
  and the token/`cap` proof still decide who gets in.
* **Tests.** bencode round-trip + hostile inputs, infohash normalisation, and a fake DHT node: announce → lookup returns the host's
  `ip:port`, another code finds nothing, and the background announcer reports "Announced to N DHT nodes".
* **Not done.** No IPv6 hole punching (global IPv6 makes it unnecessary), no DHT for the *browser* build (no UDP), no magnet-style
  multi-address records (the DHT returns one TCP port per announce).

### 16.5 Signed fail-over plans, a quorum, and revocable per-document links (`collab/access.rs`, 4 tests)

* **Host key.** `HostKey::from_device(private)` = Ed25519 from `SHA-256("lumen/host-signing-key/v1" ‖ X25519 private key)` — nothing new to
  store. The host sends its public key in `Welcome.spk`; a joiner **pins** it for the session (a different key later is refused) and clears
  the pin only when it follows a successor.
* **Signed plan.** Every field of `Standby` (host, order, addresses, secret, cap, relay, grants, `seq`) is length-prefixed into
  `plan_bytes` and signed. `on_standby` ignores unsigned plans, plans signed by an unpinned key, bad signatures and replayed sequences.
  A device that only knows the share code can no longer name itself (or a friend) as the successor.
* **Quorum.** With **three or more editors** the promoted device is a *provisional* host: it accepts the followers that prove their `cap`,
  and once a majority of the plan's editors (itself included) is connected it becomes authoritative (toast + fresh plan). Connect a
  majority within 14 s — otherwise it steps down, closes its listeners, pushes the failed candidate onto the election's "tried" list and
  follows (or promotes) the next editor. While provisional it refuses approvals, role changes, kicks and new links ("Waiting for the other
  editors…"). Split brain now needs *two* majorities, which a partition cannot produce. Two editors keep the old behaviour (a majority of
  two would block on any single failure).
* **Grants** (`Grants`): role (edit / suggest / view), optional expiry, optional use count, label, `revoked`, creation time; ≤ 32 per
  document; proofs are `secure::proof("c", token, channel hash)`, so a grant token is bound to the encrypted channel like the share code.
  `Hello` matching a live grant is accepted with *that* role and no request card (a used-up, expired or revoked grant matches nothing);
  `enforce_grants` closes connections whose link ended in the meantime; `revoke` drops them immediately. The **live grants travel inside
  the signed plan to the editors**, so a successor keeps honouring and revoking the same links; the editor set is also what makes the
  plan's promise checkable.
* **Tests.** plan signature verifies, another key does not, five tampered plans fail, the key is derived (not random); grants expire, run
  out, are revoked, are bound to the channel, the JSON survives a round-trip, at most 32; the quorum is a strict majority and only applies
  from three editors on.
* **Not done.** No UI yet for creating / revoking a link (the API is `create_grant`, `revoke_grant`, `grant_link`, `grant_lan_text`, and
  `Grants::describe` for a row) — §16.8 item 1.

### 16.6 Phones in the loop

* **`BlePair.java`** — the Java half §15.4 asked for: the host advertises `SERVICE` **plus the 16-byte session tag** as manufacturer data
  `0x4C55`, runs a GATT server (`RX` write, `TX` notify with a CCCD) and forwards whole fragments to Rust; the guest scans filtered by the
  service UUID, reports `(tag, address)`, connects, negotiates MTU, enables notifications and writes fragments one at a time (GATT allows a
  single outstanding write). Keys, codes, sealing and the reassembler stay in `pair.rs` — this class only moves bytes.
* **`FcmKit.java`** — the Firebase SDK is *not* bundled (it needs `google-services.json` and a Gradle build). The class loads
  `FirebaseMessaging` by reflection: without the SDK `available()` is false and the app keeps using the foreground service; with the SDK on
  the class path `token(ctx, cb)` hands the registration token to Rust, which registers it with `/api/collab/push` (site side done in
  §15.5). The APK builds and installs either way.
* **Manifest** — `BLUETOOTH_SCAN` (`neverForLocation`), `BLUETOOTH_ADVERTISE`, `BLUETOOTH_CONNECT`, the legacy pair with
  `maxSdkVersion=30`, and `uses-feature bluetooth_le` as *not required*.
* **`tools/phone-rig.sh`** — two devices over `adb` + UI Automator: install, grant the runtime permissions, launch, open Share on the host
  and Nearby on the guest, then assert that (a) `CollabService` is running, (b) the guest logs that it found the host (NSD / beacon),
  (c) the guest screen stays awake during the sync. `--dry-run` checks the tooling and the APK signature only, which is what CI can do.
* **Honest status.** Compiled into the delivered APK (verified: `BlePair`, `FcmKit`, `CollabService` are in `classes.dex`), but **no radio
  and no second phone exist in this sandbox**, so the pairing screen, the GATT exchange and the push call are unverified. The Rust side they
  talk to (§15.4, sealed fragments with CRC) is tested; the rig is the thing that closes the gap on real hardware.

### 16.7 Verification log (this machine, Linux x86-64)

| What | Result |
|---|---|
| `cargo check --lib --tests` (host) | clean (warnings only, all pre-existing) |
| `cargo test --lib` | see `VERIFY.md` in this delivery for the exact tail; the new modules add 17 tests (§16.2–16.5) and the big-table tests were rewritten for the new `merge_with` / `BigGet.have` signatures |
| `cargo fmt` | applied to the whole tree |
| Web build (`build-web.sh`) | `public/webapp`: `lumen_bg.wasm` 26.3 MB, `lumen.js` 93 KB, served at `/webapp` and `/app` |
| Windows build (`build-windows.sh`) | `Lumen-windows-x64.exe` 39.2 MB (+ zip 19.7 MB) |
| Android build (`build-android.sh`) | signed `Lumen-android-arm64.apk` 17.3 MB (arm64-v8a, `liblumen.so` 33.3 MB uncompressed, v1+v2+v3 signature) |
| Two devices over loopback | `two_devices_sync_over_a_real_socket`, `a_big_table_is_shared_edited_and_deleted_live`, `the_editors_elect_a_new_host_when_the_host_vanishes` |
| Two phones / two radios | **not possible here** (no devices, no Bluetooth) — rig shipped |
| DHT against the public network | **not possible here** (no unrestricted UDP) — fake-node test shipped |

### 16.8 What is left (exact next steps)

1. **The grants UI** (~1 file, `collab/ui.rs`): a card in the Share tab listing `Grants::describe`, a `+ Link` row (role, 1 h / 24 h / 7 d /
   never, uses), a copy button using `grant_link` / `grant_lan_text`, and revoke. Everything it calls exists and is tested.
2. **Live `TextOp` frames** (`Msg::Text { block, ops }`, `EditorHub`): the editor calls `Hub::local` after a `TextEdit` change and sends the
   returned operations (drop the whole-block `Ops` for text fields); `Hub::remote` applies them and reports the new string + caret offset via
   `Doc::anchor`. The fallback (entity merge) stays for peers that do not send text frames.
3. **Real-device pass:** run `tools/phone-rig.sh` with two phones; then the BLE pairing panel on top of `BlePair` + `pair.rs`
   (permissions → advertise → show the six-digit code → accept), and an FCM project (`google-services.json` + `FCM_SERVICE_ACCOUNT`).
4. **Compressed `BigChunk`s:** deflate the chunk before base64 (the crate is already linked for the collab payloads) and raise the row cap
   per frame; measure the first sync of 1 M rows before / after.
5. **Cell merge for new columns / deletions of a cell** (today a column that does not exist locally is taken from the remote row as a whole)
   and an explicit "cell deleted" marker instead of the missing-property convention.
6. **IPv6 first-class:** publish an AAAA in the relay link, prefer IPv6 addresses in hints, and use the DHT record's source address for the
   `wan_address` when UPnP is unavailable.
7. **`Arc<Page>` copy-on-write / per-page store** (§15.7, unchanged): the remaining memory win for workspaces with many huge pages; a model
   refactor across ~60 files.

### 16.9 Review — what more can be done now (ranked)

1. **Docs and editor polish around the CRDT** (undo of remote merges, "kept both versions" marker, per-block history) — the CRDT made
   concurrent typing correct; the UI still does not *show* that it happened.
2. **Server-assisted rendezvous without a document server**: the relay already exists; a signed *introduction* (room → addresses) would make
   "share by link" connect directly whenever the network allows, and fall back to relaying otherwise, without a new trust anchor.
3. **Play Store / F-Droid packaging** (`aab`, reproducible builds, `fastlane` metadata) and an update check against `version.json`.
4. **Encrypted-at-rest tables** (the store is plaintext today; the device key already exists and could wrap a per-table file key).
5. **Mesh topologies** (the star is still the only one): a joiner-to-joiner data channel would cut host upload by half for three people.
6. **A real test rig in CI** (two emulators + `adb`); the emulator can drive NSD and the foreground service, everything except the radio.

### 16.10 The delivery itself

`DELIVERY.md` in the repository root lists the three artefacts with their sizes and SHA-256, the exact build commands, how the website
serves them (`/downloads`, `/app`, `/webapp`), and the limitations of this machine. `VERIFY.md` records the commands and their output.

## 17. Pass 11 — reliable carets, follow-the-cursor, universal Nearby, one link that works anywhere, share-link UI (14.8 · this delivery)

Input: the field reports of the last pass (carets unreliable, selections only drawn on one line, a "follow the cursor" mode wanted, *is Nearby only
for Android?*, *how can any device connect without UPnP or a public address?*) plus the open items of §16.8. Nothing here changes `PROTO` (8): a new
link only carries two more optional query keys (`rr`, `rb`).

### 17.1 Carets: what was wrong, what changed
Reading the path from `TextEdit` to the other screen again found **three** causes, all on the *sending* and *painting* side:
1. **The caret expired while you were idle.** `caret_state()` returned `None` 1.5 s after the last frame that reported it, and an idle focused editor
   does not repaint every frame, so the next presence heartbeat went out *without* a caret and the remote bar vanished — "sometimes it doesn't show".
   Now the caret is **sticky** (45 s safety limit) and ends exactly when the editor of that block stops being focused (`collab::clear_caret`,
   called from `editor.rs` for every unfocused block).
2. **Multi-line selections were not drawn at all.** The painter only filled a rectangle when the first and last position were on the same row.
   `collab::selection_spans` (pure, unit-tested) now returns one span per row the selection touches (wrapped lines, hard line breaks, empty selected
   lines get a small marker), and `paint_carets` fills each row from `Row::x_offset`.
3. **Low contrast / clipped label.** The 2 px bar now has a soft halo, and the name flag moves *below* the caret on the first visible line instead
   of being clipped by the top of the scroll area.

### 17.2 Follow mode that follows the cursor
* **Follow** (person menu) already opened the page the other person was on. It now also chases **where they are**: on document pages the scroll
  area scrolls to their caret whenever it moves (one `scroll_to_rect` per move, never per frame, so you can still scroll away between moves); on
  canvas pages the board glides (20 % per frame) when their pointer leaves the middle 64 % of the screen.
* A **"Following Ada · Cursor ✓ · Stop"** pill sits at the top of the screen (touch-sized targets) so the mode is visible and one tap ends it;
  "Cursor" switches the chasing off and keeps the page-following (`Ui::follow_cursor_off`). Following also forces the person's cursor to be shown,
  whatever the "show cursors" switch says.

### 17.3 "Is Nearby only for Android?" — no, and now it is symmetric
Before: every native build already sent / received the UDP beacons (broadcast + multicast, IPv4 + IPv6), but **only Android spoke DNS-SD**
(`NsdManager`, `_lumen._tcp`). On Wi-Fi that blocks broadcast, a phone could see a laptop but never the other way round.
Now `collab/mdns.rs` (pure Rust, `socket2` for `SO_REUSEADDR` on port 5353 and per-interface multicast) publishes and browses the **same**
`_lumen._tcp.local` records with the **same TXT layout** (`i0…i5` chunks of the beacon JSON) that `CollabKit.nsdStart` writes:

| Device A → Device B | Windows / Linux / macOS | Android | Browser |
|---|---|---|---|
| **Windows / Linux / macOS** | UDP beacon **and** mDNS (new) | UDP beacon **and** mDNS ⇄ NSD (new direction) | — (no UDP in a page) |
| **Android** | same | NSD + UDP beacon | — |
| **Browser** | joins by QR / link / code → relay or WebRTC | same | same |

Also: the multicast group is joined on **every** IPv4 interface and queries / announcements leave through each (VPN, Hyper-V, Docker and Wi-Fi at
once used to hide the right one); a goodbye (ttl 0) removes a host at once. `avahi-browse -r _lumen._tcp` and Apple Bonjour see the same service.
macOS / iOS: the macOS build uses the same code; there is no iOS build. **Tests (4):** record round-trip, goodbye + source-address fallback,
Android-compatible chunking, 4 000 mutated packets never panic.
**Not done:** a page cannot send UDP, so the browser still needs a link; the planned "same public address" rendezvous on the site
(`/api/collab/near`, list the hosts published from the caller's NAT) would close that and is the first item of §17.8.

### 17.4 "How do I connect any device through the internet with no UPnP and no public address?" — research and what is built
There is no magic; the options differ in **who must help** and **what the NAT must allow**:

| Path | Needs | Router must | Built? |
|---|---|---|---|
| Same LAN | nothing | allow LAN traffic | yes (UDP beacon, **mDNS**, QR) |
| **Global IPv6** | the two devices have a global v6 address (most mobile and many home ISPs) | allow inbound to the host (firewall pinhole — PCP / UPnP-IGDv2 / one click) | **yes** — link carries the v6 hints; `Internet access` now falls back to the v6 address when there is no IPv4 mapping |
| UPnP / NAT-PMP | cooperative router with a public IPv4 | map a port | yes (pass 6) |
| UDP hole punching (STUN + rendezvous) | a rendezvous to swap two addresses | cone NAT on at least one side | yes (pass 7, `punch.rs`) |
| UDP relay (TURN-like) | one reachable host running `lumen-turn` | nothing | yes (pass 8) |
| BitTorrent DHT lookup | nothing of ours | outbound UDP | yes (pass 10) |
| **HTTPS relay on the site** | the site | outbound HTTPS only | yes — and works from browsers; **re-implemented in this repository's site** (§17.6) |

What was missing was the *glue*: a link of the "direct" kind (`lumen://join?h=…`) had no way to say "and if none of these answers, use the relay
room". Now `direct_link()` appends `&rr=<room>&rb=<site>` whenever the host has a relay room, `join_text` remembers it (`relay_fb`), and
`on_closed` — when every direct address failed — re-joins **the same session through the relay** (`Target::Relay`, hole punch first, then relay) with a
toast "No direct path — connecting through the relay". One link, any network: *LAN → IPv6 → public IPv4 → punched UDP → relay*, tried in that order
of cost, automatically. The fall-back fires once (no loops) and only before a session exists (a running session uses the existing resume logic).
**Test:** `a_direct_link_names_its_relay_and_falls_back_to_it`.
**What the relay sees:** opaque Noise-encrypted frames; the room code is random; a device id is bound to a per-session key; nothing is written to disk.

### 17.5 Share links with a role, lifetime and use count — the UI (§16.8 item 1)
The *Share* tab of a hosting device has a **Share links** card: one tap cycles *role* (edit / suggest / view), *lifetime* (1 h / 24 h / 7 d / none) and
*uses* (any / 1 / 5); **+ Link** creates the grant, copies the link and lists it with **Copy** and **Revoke** (revoking also removes whoever joined with it).
It uses only the tested core (`create_grant`, `revoke_grant`, `grant_link`, `grant_lan_text`, `Grants::describe`) and is hidden on a provisional host.

### 17.6 The website (this repository's `../src`, `../public`)
The earlier deliveries described a Next.js site but its source was not in the repository, so the checked-out tree had no relay to talk to.
It is now part of the delivery: `/api/collab` (long-poll relay, per-device key binding, 30-minute idle TTL, 16 MB log cap, frame ≤ 400 KB),
`/api/collab/punch`, `/api/collab/rtc`, `/api/collab/ip`, `/api/collab/info`, `/api/collab/push` (501 unless `FCM_SERVICE_ACCOUNT`),
`/api/download/[file]` (streams, `Range`, counts in PostgreSQL), `/api/flow-proxy`, `/api/ai-proxy`, `/api/hooks/[channel]` (SSRF-guarded), the
landing page and the rewrites `/app`, `/webapp` → the WebAssembly build. `LUMEN_TURN=host:port` is advertised to punching clients.
**Not re-implemented** (were in the earlier site, not needed by the app's core): `/api/mcp` + `/api/bridge/poll` (cloud MCP bridge) and
`/api/preview`; the app reports "That server has no Lumen bridge" for the first one.

### 17.7 Status of the §16.8 / §16.9 lists
| Item | State |
|---|---|
| 16.8-1 Grants UI | **done** (17.5) |
| 16.8-2 Live `TextOp` frames | **not done.** Reason unchanged: it replaces the whole-block `Ops` for text fields and needs an on-device typing soak (IME composition, undo) that cannot run here. The CRDT merge (`merge3`) is what keeps concurrent edits today. |
| 16.8-3 Real-device pass (BLE, FCM) | **not done** — no radios in this environment; `tools/phone-rig.sh` is the closing step |
| 16.8-4 Compressed `BigChunk`s | **not done** |
| 16.8-5 Cell deletions / new columns in cell merge | **not done** |
| 16.8-6 IPv6 first-class | **partly**: v6 fall-back for "Internet access" and v6 in every link; no AAAA publication and no PCP pinhole request |
| 16.8-7 `Arc<Page>` copy-on-write | **not done** (model refactor across ~60 files) |
| 16.9-2 Signed introduction | **done in a simpler form**: the link names the room, the relay stays untrusted (17.4) |

### 17.8 Review — what more can be done (ranked)
1. **`/api/collab/near`**: hosts opt in to publish their beacon under the hash of their public address; any device (browsers included) behind the same
   NAT lists them. Gives the web build a real "Nearby".
2. **PCP / UPnP-IGDv2 `AddPinhole`** so that IPv6-direct works behind home routers that firewall inbound v6 by default; publish an AAAA for the host.
3. **Live `TextOp`** (16.8-2) with an IME soak test on a phone; then drop the whole-block ops for text.
4. **Relay over WebSocket / SSE** when the platform allows it (Next.js route handlers cannot upgrade): today a frame waits at most one long-poll round
   trip; a dedicated `lumen-relay` binary next to `lumen-turn` would give sub-100 ms through the relay.
5. **Compressed BigChunks**, deleted-cell markers, mmap-backed `Arc<Page>` store.
6. **Real hardware CI** (two emulators + `adb` + a netns pair for the LAN and NAT cases).
7. Remote carets inside table cells, code blocks and database cells (today: text blocks and the title field only).

### 17.9 Verification log (this machine, Linux x86-64, Rust 1.100)
See `VERIFY.md` for the exact command tails. New tests: 4 (`mdns`), 4 (`collab::tests`: spans, sticky caret, link fall-back, follow); all collab tests pass.
