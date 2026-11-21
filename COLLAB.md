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
