//! Local collaboration: workspace / page sync between devices on the same Wi-Fi (and, through the optional relay, across networks),
//! with live presence (cursors), roles and on-demand media. Design notes and the roadmap are in `COLLAB.md`.
//!
//! Topology: a **host** shares a scope (whole workspace, one page, or a page with everything nested). Peers connect to the host
//! (star); the host merges and fans out. The data model is the entity CRDT in [`crdt`]; the transports (WebSocket over TCP on native,
//! browser WebSocket on the web, HTTP relay everywhere) only move [`Msg`] frames — the session code below never touches a socket.
pub mod access;
mod bigsync;
pub mod changes;
mod comments;
pub mod crdt;
pub mod iroh_transport;
pub mod pair;
mod secure;
pub mod textcrdt;
mod textmerge;
pub mod ui;

#[cfg(not(target_arch = "wasm32"))]
mod lan;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod mdns;

use crdt::{Applied, Hlc, Key, Op, Replica, Scope, ScopeKind};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Protocol 2: every frame is end-to-end encrypted (`secure.rs`); the share token is proven, never sent.
/// Protocol 3: `Key::Comment` entities and `Entry::base` (merge bases).
/// Protocol 4: `Role::Suggester`, `Presence::{typing, len}`. Older peers are refused with a clear message.
pub const PROTO: u32 = 8;
/// Tombstones are forgotten after this long. A reconnect gives up after ~90 s, so no device can come back with an older view.
const TOMBSTONE_GRACE_MS: u64 = 10 * 60 * 1000;

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
/// Hard limits (denial-of-service protection of a host): connections, signed-in people, waiting approval cards.
const MAX_CONNS: usize = 40;
const MAX_PEERS: usize = 16;
const MAX_REQUESTS: usize = 8;
const MAX_OPS_QUEUE: usize = 300_000;
const MAX_MEDIA: u64 = 256 << 20;

pub const PORT: u16 = 47615;
pub const DISCOVERY_PORT: u16 = 47616;

// ───────────── clocks, ids ─────────────

static ID_BASE: AtomicU64 = AtomicU64::new(0);

/// First id of this device's private id space while a session is active (0 otherwise): `ws.id()` / canvas ids / annotation ids
/// allocated here can never collide with ids created at the same moment on another device.
pub fn id_base() -> u64 {
    ID_BASE.load(Ordering::Relaxed)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn mono_ms() -> f64 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}
#[cfg(target_arch = "wasm32")]
pub fn mono_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// 64 random bits from the operating system (`getrandom`; `crypto.getRandomValues` in the browser).
pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    if getrandom::getrandom(&mut b).is_ok() {
        return u64::from_le_bytes(b);
    }
    // the OS generator is unavailable (should never happen): fall back to a time-seeded value rather than a fixed one
    let n = now_ms() ^ (mono_ms().to_bits());
    n.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (n >> 29)
}

pub fn random_code(n: usize) -> String {
    // 32 symbols: 256 % 32 == 0, so a random byte maps to a symbol without modulo bias; every symbol is fresh OS randomness
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut raw = vec![0u8; n];
    if getrandom::getrandom(&mut raw).is_err() {
        for b in raw.iter_mut() {
            *b = random_u64() as u8;
        }
    }
    raw.iter().map(|b| A[(*b & 31) as usize] as char).collect()
}

// ───────────── protocol ─────────────

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct PeerInfo {
    pub id: u32,
    pub name: String,
    pub device: String,
    pub platform: String,
    pub color: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Role {
    Editor,
    Viewer,
    /// Can edit, but the host sees every change as a suggestion (accept / refuse in the Changes window) before it takes effect.
    Suggester,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub roots: Vec<u64>,
    /// Stable identity of what is shared (same device + workspace + scope ⇒ same value after a restart): a joiner that shared-in
    /// before re-opens the workspace it already has instead of creating another one.
    #[serde(default)]
    pub share: String,
    /// `true`: joiners keep a copy after the session; `false`: the shared workspace is temporary and removed when it ends.
    #[serde(default = "yes")]
    pub keep: bool,
}

fn yes() -> bool {
    true
}

/// A pen / highlight stroke being drawn on a PDF right now (ephemeral — the finished mark travels as an annotation entity).
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, Debug)]
pub struct LiveDraft {
    pub doc: u64,
    pub page: u32,
    /// 0 highlight, 1 underline, 2 strike, 3 pen.
    pub tool: u8,
    pub color: u8,
    /// Pen: the stroke (page fractions, ≤ 96 points). Other tools: `[start, current]`.
    pub pts: Vec<[f32; 2]>,
}

/// A joined session asks the app for a workspace to put the shared content in.
#[derive(Clone, Debug)]
pub struct NewWs {
    pub name: String,
    pub share: String,
    pub keep: bool,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize, Debug)]
pub struct Presence {
    pub peer: u32,
    /// Page the peer is looking at.
    pub page: Option<u64>,
    /// Pointer in canvas world coordinates (canvas pages).
    pub cur: Option<[f32; 2]>,
    /// Block being edited (document pages).
    pub block: Option<u64>,
    /// PDF being read: (document key, page).
    pub pdf: Option<(u64, u32)>,
    /// Text caret / selection inside `block`: (selection start, caret) in characters of the plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caret: Option<(u32, u32)>,
    /// A PDF mark being drawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<LiveDraft>,
    /// Typed in the last two seconds ("typing…" badge).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub typing: bool,
    /// Length (characters) of the text of `block` when `caret` was taken: lets the receiver line the caret up with text that has not
    /// fully arrived yet (0 = unknown).
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub len: u32,
    /// Read receipt: (page, time in ms of the newest comment on that page this device has displayed). Comments are per page and ids are
    /// per device, so a timestamp — not an id — is the only value every peer can order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<(u64, u64)>,
}

fn is_zero32(x: &u32) -> bool {
    *x == 0
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "t")]
pub enum Msg {
    /// `auth` = proof of the share token bound to the encrypted channel (empty = ask the host for approval).
    Hello {
        peer: PeerInfo,
        #[serde(default)]
        auth: String,
        proto: u32,
    },
    /// Noise handshake message (base64). Always sent in the clear — it is what sets the encryption up.
    Hs {
        d: String,
    },
    /// One encrypted frame (base64 text — only used on text transports: the relay and the browser); `more` = continues in the next frame.
    Enc {
        d: String,
        more: bool,
    },
    /// One encrypted frame as raw bytes (protocol 5: a binary WebSocket frame on TCP — no base64, no JSON envelope). Never serialised
    /// as JSON; text transports convert with [`Msg::to_text`] / [`Msg::from_text`].
    #[serde(skip)]
    Bin {
        d: Vec<u8>,
        more: bool,
    },
    Pending,
    Welcome {
        peer: PeerInfo,
        session: SessionInfo,
        role: Role,
        #[serde(default)]
        auth: String,
        /// Protocol 8: the host's long-term public signing key (hex) — joiners pin it and only believe fail-over plans signed with it.
        #[serde(default)]
        spk: String,
    },
    Denied {
        reason: String,
    },
    Roster {
        peers: Vec<(PeerInfo, Role)>,
    },
    Digest {
        items: Vec<(Key, Hlc, u64)>,
        last: bool,
    },
    Ops {
        ops: Vec<Op>,
    },
    Presence(Presence),
    MediaReq {
        id: u64,
        off: u64,
    },
    MediaData {
        id: u64,
        off: u64,
        total: u64,
        name: String,
        mime: String,
        data: String,
    },
    MediaMiss {
        id: u64,
    },
    /// Host → the chosen successor (protocol 6): take over hosting. `leave` = the old host goes away instead of staying as a participant.
    Baton {
        #[serde(default)]
        leave: bool,
    },
    /// Successor → old host: I listen now. Addresses to dial, an optional relay room `(site, room)` and the **new** share token (the old one
    /// is retired, so a device that was removed earlier cannot walk back in with it).
    BatonAck {
        addrs: Vec<String>,
        #[serde(default)]
        relay: Option<(String, String)>,
        #[serde(default)]
        token: String,
    },
    /// Old host → everybody else: the session continues there (same fields as `BatonAck`).
    Moved {
        addrs: Vec<String>,
        #[serde(default)]
        relay: Option<(String, String)>,
        #[serde(default)]
        token: String,
    },
    /// Protocol 7 — big tables (`bigsync.rs`): per-bucket digest of the rows of one table, a request for the buckets that differ, and the rows
    /// of a bucket (or a live edit, `b == u64::MAX`) as a `lumen-store` chunk (base64) plus tombstones `(row id, deleted at ms)`.
    BigDigest {
        table: u64,
        items: Vec<(u64, u32, u64)>,
        last: bool,
    },
    BigGet {
        table: u64,
        buckets: Vec<u64>,
        /// Protocol 8 (delta): for a bucket the requester already holds, `(row id, row signature)` of every row it has — the answer then
        /// carries only the rows that differ or are missing, not the whole bucket.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        have: Vec<(u64, Vec<(u64, u64)>)>,
    },
    BigChunk {
        table: u64,
        b: u64,
        rows: String,
        #[serde(default)]
        dead: Vec<(u64, u64)>,
        /// Protocol 8 (cell merge): live edits name the properties (0 = title) they changed per row id, so a concurrent edit of another cell
        /// of the same row is merged instead of overwritten.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cells: Vec<(u64, Vec<u32>)>,
    },
    /// Protocol 7 — automatic fail-over. Host → each device: who takes over when the host disappears (`order`: editors by id), where they\n    /// can be reached, the shared `secret` (editors only) and this device's own `cap` (the token it proves to the successor), plus the\n    /// relay room the successor would host in.
    Standby {
        host: u32,
        order: Vec<u32>,
        addrs: Vec<(u32, Vec<String>)>,
        #[serde(default)]
        secret: String,
        cap: String,
        #[serde(default)]
        relay: Option<(String, String)>,
        /// Protocol 8: signed by the host's long-term key (`access::plan_bytes`); `seq` grows with every plan, `grants` = the live share
        /// links (editors only) so a successor keeps honouring — and revoking — them.
        #[serde(default)]
        spk: String,
        #[serde(default)]
        sig: String,
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        grants: String,
    },
    /// Device → host: where I would listen if I had to take over.
    StandbyAddr {
        addrs: Vec<String>,
    },
    Bye,
}

impl Msg {
    /// JSON text for a text transport (a `Bin` frame becomes an `Enc` frame with base64).
    pub fn to_text(&self) -> Option<String> {
        match self {
            Msg::Bin { d, more } => serde_json::to_string(&Msg::Enc {
                d: crate::media::b64_encode(d),
                more: *more,
            })
            .ok(),
            m => serde_json::to_string(m).ok(),
        }
    }

    /// Parses a text frame; encrypted frames come out as `Bin`, so the session only ever sees one form.
    pub fn from_text(s: &str) -> Option<Msg> {
        match serde_json::from_str::<Msg>(s).ok()? {
            Msg::Enc { d, more } => Some(Msg::Bin {
                d: crate::media::b64_decode(&d)?,
                more,
            }),
            m => Some(m),
        }
    }
}

/// What a device that offers a session announces on the local network (UDP broadcast, every 2 s).
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Beacon {
    pub v: u32,
    pub id: u32,
    pub device: String,
    pub name: String,
    pub platform: String,
    pub port: u16,
    /// "Workspace", "Page" or "Page + nested".
    pub what: String,
    pub title: String,
    pub people: u32,
    /// Host's Iroh EndpointId or compact ticket for direct local QUIC dialing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub iroh: String,
}

#[derive(Clone, Debug)]
pub struct Found {
    pub beacon: Beacon,
    pub ip: String,
    pub seen: f64,
}

pub type ConnId = u64;

pub trait Sink: Send {
    fn send(&self, m: &Msg);
    /// Bypasses the cipher (handshake frames only).
    fn raw(&self, m: &Msg) {
        self.send(m)
    }
    /// Seals messages that were queued while the handshake ran (only the cipher wrapper has anything to do).
    fn flush_pending(&self) {}
    fn close(&self);
    /// Active transport path classification (Direct LAN, Direct P2P, or Relayed).
    fn path_kind(&self) -> iroh_transport::PathKind {
        iroh_transport::PathKind::Unknown
    }
}

pub enum Event {
    Open {
        conn: ConnId,
        sink: Box<dyn Sink>,
        addr: String,
        incoming: bool,
    },
    Msg {
        conn: ConnId,
        msg: Msg,
    },
    Closed {
        conn: ConnId,
        why: String,
    },
    Found(Beacon, String),
}

/// Thread-safe mailbox between the transports and the UI thread; every push wakes the UI.
#[derive(Clone)]
pub struct Inbox {
    q: Arc<Mutex<VecDeque<Event>>>,
    ctx: egui::Context,
    next: Arc<AtomicU64>,
}

impl Inbox {
    pub fn new(ctx: egui::Context) -> Self {
        Inbox {
            q: Arc::new(Mutex::new(VecDeque::new())),
            ctx,
            next: Arc::new(AtomicU64::new(1)),
        }
    }
    pub fn push(&self, e: Event) {
        if let Ok(mut q) = self.q.lock() {
            q.push_back(e);
        }
        self.ctx.request_repaint();
    }
    /// Events waiting for the UI thread (transports stop reading their socket while this is large: TCP back-pressure instead of memory).
    pub fn pending(&self) -> usize {
        self.q.lock().map_or(0, |q| q.len())
    }
    pub fn conn_id(&self) -> ConnId {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
    fn drain(&self) -> Vec<Event> {
        self.q
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }
}

// ───────────── session state ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Off,
    Hosting,
    /// Connecting / waiting for the host's approval.
    Joining,
    Joined,
}

struct Conn {
    sink: Box<dyn Sink>,
    addr: String,
    peer: Option<PeerInfo>,
    role: Role,
    active: bool,
    digest: HashMap<Key, (Hlc, u64)>,
    backlog: VecDeque<Msg>,
    crypt: Arc<Mutex<secure::Crypt>>,
    opened: f64,
    last_rx: f64,
    /// Token bucket against message floods.
    bucket: f64,
    /// Four digits derived from the encrypted channel (shown on both sides).
    code: String,
    /// The device proved itself with its fail-over `cap` (this device took over after the host vanished).
    via_standby: bool,
    /// The share link (grant token) this device came in with: its `Welcome` proof is made with it, and revoking the grant drops the connection.
    grant_token: Option<String>,
}

pub struct JoinRequest {
    pub conn: ConnId,
    /// Short code of the encrypted channel: read it out loud on both devices to rule out an active attacker.
    pub code: String,
    pub peer: PeerInfo,
    pub addr: String,
}

/// How this device joined (kept so a dropped connection can be re-dialled).
#[derive(Clone, Debug)]
enum Target {
    Iroh { ticket: String, token: String },
}

/// A joined session whose connection broke: the replica and the workspace stay, the dial is retried with a back-off and the next
/// digest exchange transfers whatever changed on either side in the meantime.
struct Resume {
    attempt: u32,
    next_at: f64,
    session: String,
}

/// A host hand-over in flight: the connection of the successor, when it started and whether the old host leaves afterwards.
struct BatonWait {
    conn: ConnId,
    at: f64,
    leave: bool,
}

/// Back-off between reconnect attempts (milliseconds); after the last one the session ends.
/// What the host told this device about the fail-over plan (`Msg::Standby`).
#[derive(Clone, Debug)]
struct Standby {
    host: u32,
    order: Vec<u32>,
    addrs: Vec<(u32, Vec<String>)>,
    secret: String,
    cap: String,
    relay: Option<(String, String)>,
    spk: String,
    sig: String,
    seq: u64,
    grants: String,
}

/// A successor that has not yet been confirmed by a majority of the editors.
struct Quorum {
    need: usize,
    since: f64,
    plan: Standby,
}

const QUORUM_MS: f64 = 14_000.0;

/// An election in progress on a device that lost its host.
struct Elect {
    tried: Vec<u32>,
    cand: u32,
    since: f64,
}

/// How long a candidate gets to come up before the next one is tried.
const ELECT_WINDOW_MS: f64 = 14_000.0;

/// Token a device proves to a fail-over host: a one-way function of the session secret and the device id.
fn standby_cap(secret: &str, id: u32) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"lumen-standby-cap-v1");
    h.update(secret.as_bytes());
    h.update(id.to_le_bytes());
    hex(&h.finalize()[..10])
}

/// Relay room of the fail-over host (the same on every device that knows the secret; the site only sees an opaque code).
fn standby_room(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"lumen-standby-room-v1");
    h.update(secret.as_bytes());
    hex(&h.finalize()[..8]).to_uppercase()
}

/// Port a device listens on when it becomes the host (derived from its id, so several devices on one machine do not clash).
fn standby_port(id: u32) -> u16 {
    47_700 + (id % 250) as u16
}

const RESUME_DELAYS: [f64; 8] = [
    800.0, 2000.0, 4000.0, 7000.0, 10_000.0, 15_000.0, 20_000.0, 30_000.0,
];

#[derive(Default)]
pub struct Ui {
    /// Top-left people menu.
    pub menu: bool,
    /// Share / connect dialog.
    pub dialog: bool,
    pub tab: u8,
    pub scope_choice: u8,
    pub join_text: String,
    pub name_edit: String,
    pub show_cursors: Option<bool>,
    pub copied_at: f64,
    pub qr_cache: Option<(String, Vec<bool>, usize)>,
    pub person_menu: Option<u32>,
    pub follow: Option<u32>,
    /// While following: stay on the person's page only (the default also scrolls / pans to wherever their caret or pointer is).
    pub follow_cursor_off: bool,
    /// New share link in the dialog: role / lifetime / use-count choices (indices).
    pub grant_role: u8,
    pub grant_ttl: u8,
    pub grant_uses: u8,
    pub toast: Option<(String, f64)>,
    pub discovering: bool,
    pub details: bool,
    pub chip: Option<egui::Rect>,
    pub relay_url: String,
    pub relay_loaded: bool,
    pub use_relay: bool,
    pub show_relay: bool,
    pub qr_pick: u8,
    /// Comments window.
    pub comments: bool,
    pub comment_draft: String,
    pub comment_on_block: bool,
    pub comment_resolved: bool,
    pub comment_reply: u64,
    pub comment_known: HashSet<u64>,
    pub comment_primed: bool,
    /// "Changes" review window.
    pub changes: bool,
    pub comment_pick: usize,
    pub hs_poll: f64,
}

/// Optional custom Iroh relay configuration (empty = automatic n0 public relays).

/// A link that carries the credentials of a nearby phone's hotspot: connect to that Wi-Fi first, then dial.
struct PendingWifi {
    ssid: String,
    hosts: String,
    token: String,
    since: f64,
    next_check: f64,
}

pub struct Collab {
    pub me: PeerInfo,
    pub mode: Mode,
    pub session: Option<SessionInfo>,
    pub role: Role,
    pub ui: Ui,
    pub found: Vec<Found>,
    pub requests: Vec<JoinRequest>,
    pub roster: Vec<(PeerInfo, Role)>,
    pub token: String,
    pub addr_hint: Vec<String>,
    pub show_cursors: bool,
    pub last_error: Option<String>,
    pub stats: Stats,
    last_reap: f64,
    joining_since: f64,
    keys: Option<Arc<secure::Keys>>,
    /// Devices the host approved once (Noise static key as hex → (name, viewer)): they join again without a card. Persisted.
    trusted: HashMap<String, (String, bool)>,
    last_gc: f64,
    /// Big tables in the session (digests, pending buckets, per-table index).
    big: bigsync::BigState,
    big_tracking: bool,
    /// Joiner: the fail-over plan the host sent.
    standby: Option<Standby>,
    /// Host (original or promoted): the secret the fail-over caps derive from.
    standby_key: Option<String>,
    /// Set once this device took over after a failure: the host's secret, so joiners can prove their `cap`.
    standby_secret: Option<String>,
    /// Host: where each device said it would listen.
    peer_addrs: HashMap<u32, Vec<String>>,
    /// Joiner: the address list was sent for this host id.
    standby_sent: u32,
    electing: Option<Elect>,
    /// Revocable / expiring share links of this document (host side; editors hold a copy from the plan).
    pub grants: access::Grants,
    host_key: Option<access::HostKey>,
    /// Joiner: the host's public signing key (pinned from the first `Welcome`).
    host_pin: Option<String>,
    plan_seq: u64,
    quorum: Option<Quorum>,
    listen_pref: u16,
    /// Wrong-token attempts per address: (count, last time).
    fails: HashMap<String, (u32, f64)>,
    target: Option<Target>,
    resume: Option<Resume>,
    /// A hand-over this host started and is waiting for the successor to confirm.
    baton: Option<BatonWait>,
    pending_baton_ack: Option<(ConnId, f64)>,
    /// Roles of the people of the session a hand-over left us with: a device that comes back with the new token keeps its role.
    inherited: HashMap<u32, Role>,
    /// Devices (by Noise static key) the host removed: they cannot walk back in with the share token.
    banned: HashSet<Vec<u8>>,
    /// Devices (by Noise static key) that were demoted to viewers: reconnecting with the token does not make them editors again.
    demoted: HashSet<Vec<u8>>,
    /// The attachment currently being streamed to a peer (so a big PDF is read from disk once, not once per chunk).
    pub serving: Option<(u64, Arc<[u8]>)>,
    presence: HashMap<u32, (Presence, f64)>,
    local: Presence,
    sent_local: Presence,
    last_presence_tx: f64,
    rep: Option<Replica>,
    conns: HashMap<ConnId, Conn>,
    inbox: Inbox,
    inq: VecDeque<(ConnId, Msg)>,
    ops_in: VecDeque<(ConnId, Op)>,
    bound: u64,
    want_ws: Option<NewWs>,
    ready: bool,
    next_scan: f64,
    requested: HashMap<u64, f64>,
    partial: HashMap<u64, (Vec<u8>, String, String)>,
    media_done: Vec<(u64, String, String, Vec<u8>)>,
    media_reqs: Vec<(ConnId, u64, u64)>,
    check_media: bool,
    last_page: Option<u64>,
    /// Remote edits are recorded for the review window.
    track: bool,
    /// The joiner re-opened a workspace it already had: the first sync is reviewed as "what changed while you were away".
    reused: bool,
    /// When the per-entity clocks were last written (ms, monotonic).
    stamps_at: f64,
    /// Android: the foreground service is running (it follows `mode`).
    svc_on: bool,
    /// Custom Iroh relay URL (empty = use n0's free default public relays automatically).
    pub custom_iroh_relay: String,
    prompted: bool,
    bound_at: f64,
    last_op_at: f64,
    /// What other people changed (git-style review, see `changes.rs`).
    pub changes: changes::Log,
    /// Host option: joiners keep a copy of the shared content after the session (`false` = temporary).
    pub keep_copy: bool,
    /// Host option: let a device that knows the share code in without asking. Off by default — every new device needs approval.
    pub auto_admit: bool,
    /// Changes of *suggesting* devices, held until the host decides.
    pub suggestions: Vec<changes::Suggestion>,
    held_in: VecDeque<(ConnId, Op)>,
    reassert: Vec<(Key, Hlc)>,
    /// Pages edited on this device while it was away from the host (listed in the Changes window).
    pub away: Vec<(u64, String)>,
    /// Nearby hotspot of this phone: (state, ssid, passphrase) as last reported by Android.
    pub hs: (String, String, String),
    pending_wifi: Option<PendingWifi>,
    /// Unified Iroh QUIC transport (P2P hole-punching + n0 public relays on native, Android, Windows, and WASM).
    pub iroh: iroh_transport::IrohNet,
    #[cfg(not(target_arch = "wasm32"))]
    lan_adv_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(not(target_arch = "wasm32"))]
    lan_disc_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    mdns_adv_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    mdns_disc_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    beacon: Arc<Mutex<Option<Beacon>>>,
    ctx: egui::Context,
}

#[derive(Default, Clone)]
pub struct Stats {
    /// Tombstones forgotten so far.
    pub gc: u64,
    pub ops_in: u64,
    pub ops_out: u64,
    pub scan_ms: f64,
    pub entities: usize,
}

pub fn platform_name() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "web"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn device_name() -> String {
    #[cfg(target_os = "android")]
    {
        if let Ok(o) = std::process::Command::new("getprop")
            .arg("ro.product.model")
            .output()
        {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if !s.is_empty() {
                return s;
            }
        }
        return "Android device".into();
    }
    #[allow(unreachable_code)]
    {
        for k in ["COMPUTERNAME", "HOSTNAME", "HOST"] {
            if let Ok(v) = std::env::var(k) {
                if !v.trim().is_empty() {
                    return v.trim().to_owned();
                }
            }
        }
        for p in ["/etc/hostname", "/proc/sys/kernel/hostname"] {
            if let Ok(v) = std::fs::read_to_string(p) {
                if !v.trim().is_empty() {
                    return v.trim().to_owned();
                }
            }
        }
        "This computer".into()
    }
}
#[cfg(target_arch = "wasm32")]
pub fn device_name() -> String {
    "Web browser".into()
}

/// Seals / opens the device key with the Android Keystore (other platforms: the key stays in the app-private store as before).
fn vault_seal(b: &[u8]) -> Option<Vec<u8>> {
    #[cfg(target_os = "android")]
    {
        crate::android::vault_seal(b)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = b;
        None
    }
}

fn vault_open(b: &[u8]) -> Option<Vec<u8>> {
    #[cfg(target_os = "android")]
    {
        crate::android::vault_open(b)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = b;
        None
    }
}

/// The device key is created once and kept (approve a device once, it is recognised next time). Stored as `ks1:<hex>` when it is wrapped by
/// the Android Keystore (the raw key never touches storage there), as plain hex elsewhere; an older plain key is wrapped on first start.
fn load_device_key(ctx: &egui::Context) -> Option<Arc<secure::Keys>> {
    let id = egui::Id::new("collab_key");
    let stored = ctx.data_mut(|d| d.get_persisted::<String>(id));
    let saved = stored
        .as_deref()
        .and_then(|s| match s.strip_prefix("ks1:") {
            Some(w) => unhex(w)
                .and_then(|b| vault_open(&b))
                .and_then(secure::Keys::from_private),
            None => unhex(s).and_then(secure::Keys::from_private),
        });
    let remember = |k: &secure::Keys| {
        let v = match vault_seal(k.private_bytes()) {
            Some(w) => format!("ks1:{}", hex(&w)),
            None => hex(k.private_bytes()),
        };
        ctx.data_mut(|d| d.insert_persisted(id, v));
    };
    match saved {
        Some(k) => {
            if stored.as_deref().is_some_and(|s| !s.starts_with("ks1:")) {
                remember(&k); // upgrade a plain key to a wrapped one
            }
            Some(Arc::new(k))
        }
        None => {
            let k = secure::gen_keys()?;
            remember(&k);
            Some(Arc::new(k))
        }
    }
}

fn with_port(a: &str) -> String {
    if a.starts_with('[') {
        if a.contains("]:") {
            a.to_owned()
        } else {
            format!("{a}:{PORT}")
        }
    } else if a.contains(':') {
        a.to_owned()
    } else {
        format!("{a}:{PORT}")
    }
}

/// Percent-encoding for the values inside a join link (SSIDs and passphrases may contain anything).
pub fn pct_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a hand-over message may tell a device to dial: Iroh tickets/IDs (`iroh:...`) or plain `host:port` strings.
fn valid_target(addrs: &[String], relay: &Option<(String, String)>, token: &str) -> bool {
    let plain = |a: &String| {
        !a.is_empty()
            && a.len() <= 512
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || ".:-[]_".contains(c))
    };
    let room = relay.as_ref().map_or(true, |(b, r)| {
        (b.starts_with("https://") || b.starts_with("http://"))
            && b.len() <= 200
            && !b.contains(char::is_whitespace)
            && (4..=32).contains(&r.len())
            && r.chars().all(|c| c.is_ascii_alphanumeric())
    });
    addrs.len() <= 12
        && addrs.iter().all(plain)
        && room
        && (addrs.len() + relay.iter().count()) > 0
        && (4..=64).contains(&token.len())
        && token.chars().all(|c| c.is_ascii_alphanumeric())
}

fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The part of an address that identifies a machine (the port changes with every connection).
fn peer_key(addr: &str) -> String {
    if addr.starts_with("iroh:") {
        return addr.to_owned();
    }
    addr.rsplit_once(':').map_or(addr, |(h, _)| h).to_owned()
}

fn chunked<T: Clone>(v: Vec<T>, n: usize) -> Vec<Vec<T>> {
    v.chunks(n.max(1)).map(|c| c.to_vec()).collect()
}

impl Collab {
    pub fn new(ctx: &egui::Context) -> Self {
        let id = ((random_u64() >> 3) & 0x7f_ffff) as u32 + 1;
        let device = device_name();
        let name: String = ctx
            .data_mut(|d| d.get_persisted::<String>(egui::Id::new("collab_name")))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| device.clone());
        let show_cursors = ctx
            .data_mut(|d| d.get_persisted::<bool>(egui::Id::new("collab_cursors")))
            .unwrap_or(true);
        let me = PeerInfo {
            id,
            name,
            device,
            platform: platform_name().into(),
            color: (id % 8) as u8,
        };
        Collab {
            me,
            mode: Mode::Off,
            session: None,
            role: Role::Editor,
            ui: Ui::default(),
            found: vec![],
            requests: vec![],
            roster: vec![],
            token: String::new(),
            addr_hint: vec![],
            show_cursors,
            last_error: None,
            stats: Stats::default(),
            last_reap: 0.0,
            joining_since: 0.0,
            keys: load_device_key(ctx),
            trusted: ctx
                .data_mut(|d| {
                    d.get_persisted::<Vec<(String, String, bool)>>(egui::Id::new("collab_trusted"))
                })
                .unwrap_or_default()
                .into_iter()
                .map(|(k, n, v)| (k, (n, v)))
                .collect(),
            last_gc: 0.0,
            big: Default::default(),
            big_tracking: false,
            standby: None,
            standby_key: None,
            standby_secret: None,
            peer_addrs: HashMap::new(),
            standby_sent: 0,
            electing: None,
            grants: access::Grants::from_json(
                &ctx.data_mut(|d| d.get_persisted::<String>(egui::Id::new("collab_grants")))
                    .unwrap_or_default(),
            ),
            host_key: None,
            host_pin: None,
            plan_seq: 0,
            quorum: None,
            listen_pref: PORT,
            suggestions: vec![],
            held_in: VecDeque::new(),
            reassert: vec![],
            away: vec![],
            hs: ("idle".into(), String::new(), String::new()),
            pending_wifi: None,
            fails: HashMap::new(),
            target: None,
            resume: None,
            baton: None,
            pending_baton_ack: None,
            inherited: HashMap::new(),
            banned: HashSet::new(),
            demoted: HashSet::new(),
            serving: None,
            presence: HashMap::new(),
            local: Presence {
                peer: id,
                ..Default::default()
            },
            sent_local: Presence::default(),
            last_presence_tx: 0.0,
            rep: None,
            conns: HashMap::new(),
            inbox: Inbox::new(ctx.clone()),
            inq: VecDeque::new(),
            ops_in: VecDeque::new(),
            bound: 0,
            want_ws: None,
            ready: false,
            next_scan: 0.0,
            requested: HashMap::new(),
            partial: HashMap::new(),
            media_done: vec![],
            media_reqs: vec![],
            check_media: false,
            last_page: None,
            track: false,
            reused: false,
            stamps_at: 0.0,
            svc_on: false,
            custom_iroh_relay: ctx
                .data_mut(|d| d.get_persisted::<String>(egui::Id::new("collab_iroh_relay")))
                .unwrap_or_default(),
            prompted: false,
            bound_at: 0.0,
            last_op_at: 0.0,
            changes: changes::Log::default(),
            keep_copy: true,
            auto_admit: false,
            iroh: iroh_transport::IrohNet::default(),
            #[cfg(not(target_arch = "wasm32"))]
            lan_adv_stop: None,
            #[cfg(not(target_arch = "wasm32"))]
            lan_disc_stop: None,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            mdns_adv_stop: None,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            mdns_disc_stop: None,
            beacon: Arc::new(Mutex::new(None)),
            ctx: ctx.clone(),
        }
    }

    pub fn active(&self) -> bool {
        self.mode != Mode::Off
    }

    pub fn set_name(&mut self, n: &str) {
        let n = n.trim();
        if !n.is_empty() && n != self.me.name {
            self.me.name = n.chars().take(32).collect();
            let v = self.me.name.clone();
            self.ctx
                .data_mut(|d| d.insert_persisted(egui::Id::new("collab_name"), v));
        }
    }

    pub fn set_cursors(&mut self, on: bool) {
        self.show_cursors = on;
        self.ctx
            .data_mut(|d| d.insert_persisted(egui::Id::new("collab_cursors"), on));
    }

    /// Everybody who is connected, me first.
    pub fn people(&self) -> Vec<(PeerInfo, Role, bool)> {
        let mut v = vec![(
            self.me.clone(),
            if self.mode == Mode::Hosting {
                Role::Editor
            } else {
                self.role
            },
            true,
        )];
        for (p, r) in &self.roster {
            if p.id != self.me.id {
                v.push((p.clone(), *r, false));
            }
        }
        v
    }

    pub fn presence_of(&self, id: u32) -> Option<&Presence> {
        self.presence.get(&id).map(|p| &p.0)
    }

    pub fn remotes_on(&self, page: u64) -> Vec<(&PeerInfo, &Presence)> {
        self.roster
            .iter()
            .filter(|(p, _)| p.id != self.me.id)
            .filter_map(|(p, _)| {
                self.presence
                    .get(&p.id)
                    .filter(|x| x.0.page == Some(page))
                    .map(|x| (p, &x.0))
            })
            .collect()
    }

    pub fn toast(&mut self, s: impl Into<String>) {
        self.ui.toast = Some((s.into(), mono_ms()));
    }

    // ───────────── starting / stopping ─────────────

    fn scope_label(scope: Scope) -> &'static str {
        match scope.kind {
            ScopeKind::Workspace => "Workspace",
            ScopeKind::Page => "Page",
            ScopeKind::Tree => "Page + nested",
        }
    }

    pub fn start_host(
        &mut self,
        ws: &mut crate::model::Workspace,
        ws_id: u64,
        scope: Scope,
        with_relay: Option<String>,
    ) {
        self.stop(false);
        ID_BASE.store((self.me.id as u64) << 40, Ordering::Relaxed);
        let title = match scope.kind {
            ScopeKind::Workspace => ws.name.clone(),
            _ => ws
                .page(scope.root)
                .map(|p| {
                    if p.title.is_empty() {
                        "Untitled".to_owned()
                    } else {
                        p.title.clone()
                    }
                })
                .unwrap_or_default(),
        };
        let mut rep = Replica::new(self.me.id, scope);
        rep.adopt(ws, now_ms());
        let roots = rep.roots();
        let share = self.share_id(ws_id, scope);
        // per-block clocks of the previous run of this share (restart ⇒ no fall-back to page granularity)
        let stamps: Option<String> = self
            .ctx
            .data_mut(|d| d.get_persisted(egui::Id::new(("collab_stamps", share.clone()))));
        if let Some(t) = stamps {
            rep.import_stamps(&t);
        }
        self.session = Some(SessionInfo {
            id: random_code(10),
            name: title.clone(),
            scope,
            roots,
            share,
            keep: self.keep_copy,
        });
        self.changes.clear();
        self.track = true;
        self.reused = false;
        self.rep = Some(rep);
        self.mode = Mode::Hosting;
        self.role = Role::Editor;
        self.bound = ws_id;
        self.ready = true;
        self.token = random_code(10);
        self.roster = vec![(self.me.clone(), Role::Editor)];
        self.last_error = None;
        let _ = with_relay;
        self.open_listener(scope, title, false);
        self.next_scan = 0.0;
    }

    /// Deterministic 32-byte seed for this device's Iroh `SecretKey`, derived from the persisted device key.
    fn iroh_seed(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"lumen-iroh-key-v1");
        if let Some(k) = &self.keys {
            h.update(k.private_bytes());
        } else {
            h.update(self.me.id.to_le_bytes());
        }
        let digest = h.finalize();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&digest[..32]);
        seed
    }

    /// Current Iroh `EndpointTicket` string for this host (updates automatically once the relay connects).
    pub fn iroh_ticket(&self) -> Option<String> {
        self.iroh.ticket()
    }

    /// Current 32-byte Ed25519 Iroh `EndpointId` string.
    pub fn iroh_endpoint_id(&self) -> Option<String> {
        self.iroh.endpoint_id()
    }

    /// Human-readable transport status string for the Share dialog.
    pub fn iroh_status(&self) -> String {
        self.iroh.status()
    }

    /// Active connection path classification for a specific peer (`Direct LAN`, `Direct P2P`, or `Relayed`).
    pub fn peer_path(&self, peer_id: u32) -> iroh_transport::PathKind {
        self.conns
            .values()
            .find(|c| c.active && c.peer.as_ref().map_or(false, |p| p.id == peer_id))
            .map(|c| c.sink.path_kind())
            .unwrap_or_default()
    }

    /// Best active connection path classification for this session (used by joiners and status indicators).
    pub fn primary_path(&self) -> iroh_transport::PathKind {
        let mut best = iroh_transport::PathKind::Unknown;
        for c in self.conns.values().filter(|c| c.active) {
            let k = c.sink.path_kind();
            match (best, k) {
                (_, iroh_transport::PathKind::DirectLan) => return k,
                (
                    iroh_transport::PathKind::Unknown | iroh_transport::PathKind::Relayed,
                    iroh_transport::PathKind::DirectP2p,
                ) => best = k,
                (iroh_transport::PathKind::Unknown, iroh_transport::PathKind::Relayed) => best = k,
                _ => {}
            }
        }
        best
    }

    /// Opens the Iroh QUIC endpoint and LAN mDNS/NSD advertisement for a hosting session (works on native, Android, Windows, and WASM).
    fn open_listener(&mut self, scope: Scope, title: String, keep: bool) {
        let custom = (!self.custom_iroh_relay.trim().is_empty())
            .then(|| self.custom_iroh_relay.trim().to_owned());
        let bind_port = (self.listen_pref != PORT).then_some(self.listen_pref);
        self.iroh.start_host(
            self.iroh_seed(),
            custom,
            keep,
            bind_port,
            self.inbox.clone(),
            self.ctx.clone(),
        );
        let b = Beacon {
            v: PROTO,
            id: self.me.id,
            device: self.me.device.clone(),
            name: self.me.name.clone(),
            platform: self.me.platform.clone(),
            port: PORT,
            what: Self::scope_label(scope).into(),
            title,
            people: 1,
            iroh: self.iroh.endpoint_id().unwrap_or_default(),
        };
        if let Ok(mut g) = self.beacon.lock() {
            *g = Some(b);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(s) = self.lan_adv_stop.take() {
                s.store(true, Ordering::Relaxed);
            }
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            lan::advertise(self.beacon.clone(), stop.clone());
            self.lan_adv_stop = Some(stop);
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            if let Some(s) = self.mdns_adv_stop.take() {
                s.store(true, Ordering::Relaxed);
            }
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            mdns::advertise(self.beacon.clone(), stop.clone());
            self.mdns_adv_stop = Some(stop);
        }
        #[cfg(target_os = "android")]
        if let Some(b) = self.beacon.lock().ok().and_then(|g| g.clone()) {
            let mut compact = b.clone();
            compact.title = compact.title.chars().take(40).collect();
            if let Ok(j) = serde_json::to_string(&compact) {
                crate::android::nsd_register(&format!("{:x}", self.me.id), PORT, &j);
            }
        }
    }

    /// Stable identifier of "this workspace / page shared from this device" (one-way hash of the device key — it reveals nothing).
    fn share_id(&self, ws_id: u64, scope: Scope) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"lumen-share-v1");
        if let Some(k) = &self.keys {
            h.update(k.private_bytes());
        }
        h.update(ws_id.to_le_bytes());
        h.update([scope.kind as u8]);
        h.update(scope.root.to_le_bytes());
        hex(&h.finalize()[..10])
    }

    /// Universal share link (`lumen://join?iroh=<ticket>&t=<token>`) that works on LAN, Wi-Fi, 5G, CGNAT, and browsers.
    pub fn iroh_link(&self) -> Option<String> {
        let ticket = self.iroh.ticket()?;
        let mut s = format!("lumen://join?iroh={}&t={}", pct_encode(&ticket), self.token);
        if self.hs.0 == "up" && !self.hs.1.is_empty() {
            s.push_str(&format!(
                "&ws={}&wp={}",
                pct_encode(&self.hs.1),
                pct_encode(&self.hs.2)
            ));
        }
        Some(s)
    }

    /// Optional web-app URL (`https://.../app?iroh=<ticket>&t=<token>`) for one-click browser joining.
    pub fn iroh_web_link(&self, site_base: &str) -> Option<String> {
        let base = site_base.trim().trim_end_matches('/');
        if base.is_empty() {
            return None;
        }
        let ticket = self.iroh.ticket()?;
        Some(format!(
            "{base}/app?iroh={}&t={}",
            pct_encode(&ticket),
            self.token
        ))
    }

    /// Starts / stops this phone's private hotspot (Android only; elsewhere a no-op).
    pub fn set_hotspot(&mut self, on: bool) {
        #[cfg(target_os = "android")]
        {
            if on {
                crate::android::hotspot_start();
            } else {
                crate::android::hotspot_stop();
                self.hs = ("idle".into(), String::new(), String::new());
            }
        }
        let _ = on;
    }

    pub fn hotspot_supported() -> bool {
        cfg!(target_os = "android")
    }

    /// Refreshes the hotspot state (cheap JNI call, at most 3 times a second while it matters).
    fn poll_hotspot(&mut self, now: f64) {
        #[cfg(target_os = "android")]
        if (self.hs.0 != "idle" || self.ui.dialog) && now - self.ui.hs_poll > 330.0 {
            self.ui.hs_poll = now;
            let st = crate::android::hotspot_state();
            if st != self.hs {
                let came_up = st.0 == "up" && self.hs.0 != "up";
                self.hs = st;
                if came_up && self.mode == Mode::Hosting {
                    self.ui.qr_cache = None;
                }
                self.ctx.request_repaint();
            }
        }
        let _ = now;
    }

    /// Wi-Fi join that precedes a dial (link with `ws` / `wp`): waits for Android, then dials via Iroh.
    fn poll_wifi(&mut self, now: f64) {
        let Some(p) = self.pending_wifi.as_mut() else {
            return;
        };
        if now < p.next_check {
            return;
        }
        p.next_check = now + 300.0;
        let timed_out = now - p.since > 55_000.0;
        #[cfg(target_os = "android")]
        let (state, msg) = crate::android::wifi_state();
        #[cfg(not(target_os = "android"))]
        let (state, msg) = ("failed".to_owned(), String::new());
        if state == "up" {
            if let Some(p) = self.pending_wifi.take() {
                self.join_iroh(&p.hosts, &p.token);
            }
        } else if state == "failed" || timed_out {
            let ssid = self.pending_wifi.take().map(|p| p.ssid).unwrap_or_default();
            self.mode = Mode::Off;
            self.last_error = Some(if msg.is_empty() {
                format!("Could not join the Wi-Fi “{ssid}”")
            } else {
                msg
            });
        }
        self.ctx
            .request_repaint_after(std::time::Duration::from_millis(300));
    }

    /// Joins by whatever the user pasted / scanned: `lumen://join?iroh=...&t=...`, `https://.../app?iroh=...&t=...`,
    /// a bare `endpoint...` Iroh ticket, or a bare Iroh `EndpointId`.
    pub fn join_text(&mut self, text: &str, _relay_base: &str) {
        let t = text.trim();
        if t.is_empty() {
            return;
        }
        let q = |key: &str| -> Option<String> {
            let after = t.split_once('?')?.1;
            after
                .split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{key}=")).map(pct_decode))
        };
        if let Some(ticket) = q("iroh") {
            let token = q("t").unwrap_or_default();
            match (q("ws"), q("wp")) {
                (Some(ssid), pass) if cfg!(target_os = "android") && !ssid.is_empty() => {
                    self.join_via_wifi(&ssid, &pass.unwrap_or_default(), &ticket, &token)
                }
                _ => self.join_iroh(&ticket, &token),
            }
            return;
        }
        let raw = t.strip_prefix("iroh:").unwrap_or(t).trim();
        if iroh_transport::parse_endpoint_addr(raw).is_ok() {
            self.join_iroh(raw, "");
        } else {
            self.last_error = Some(
                "Paste a Lumen Iroh link or ticket (starts with lumen://join?iroh= or endpoint…)"
                    .into(),
            );
        }
    }

    /// Joins a host by its Iroh `EndpointTicket` or `EndpointId`.
    pub fn join_iroh(&mut self, ticket_or_id: &str, token: &str) {
        self.stop(false);
        self.target = Some(Target::Iroh {
            ticket: ticket_or_id.trim().to_owned(),
            token: token.to_owned(),
        });
        self.dial();
    }

    /// A link made by a phone that opened its private hotspot: join that Wi-Fi (Android asks the user once), then dial via Iroh.
    pub fn join_via_wifi(&mut self, ssid: &str, pass: &str, ticket: &str, token: &str) {
        self.stop(false);
        #[cfg(target_os = "android")]
        crate::android::wifi_join(ssid, pass);
        let _ = pass;
        self.mode = Mode::Joining;
        self.joining_since = mono_ms();
        self.last_error = None;
        let now = mono_ms();
        self.pending_wifi = Some(PendingWifi {
            ssid: ssid.to_owned(),
            hosts: ticket.to_owned(),
            token: token.to_owned(),
            since: now,
            next_check: now + 500.0,
        });
        self.ctx
            .request_repaint_after(std::time::Duration::from_millis(300));
    }

    /// Opens the Iroh QUIC connection described by `self.target` (first join and every reconnect).
    fn dial(&mut self) {
        let Some(Target::Iroh { ticket, token }) = self.target.clone() else {
            return;
        };
        self.mode = Mode::Joining;
        self.joining_since = mono_ms();
        self.last_error = None;
        self.token = token;
        let custom = (!self.custom_iroh_relay.trim().is_empty())
            .then(|| self.custom_iroh_relay.trim().to_owned());
        self.iroh.dial(
            self.iroh_seed(),
            ticket,
            custom,
            self.inbox.clone(),
            self.ctx.clone(),
        );
    }

    /// True while a joined session is re-dialling its host after the connection broke.
    pub fn reconnecting(&self) -> bool {
        self.resume.is_some()
    }

    /// A joined device lost its connection for a transport reason: keep the replica / workspace and schedule another dial.
    /// Returns `false` when the session should end instead (host left on purpose, removed, too many failures).
    fn try_resume(&mut self, why: &str, was_joined: bool) -> bool {
        let transient = [
            "Connection lost",
            "timed out",
            "closed the connection",
            "Could not reach",
            "did not answer",
        ]
        .iter()
        .any(|k| why.contains(k));
        if !(was_joined || self.resume.is_some())
            || !transient
            || self.target.is_none()
            || self.rep.is_none()
            || !self.ready
        {
            return false;
        }
        let session = match (&self.resume, &self.session) {
            (Some(r), _) => r.session.clone(),
            (None, Some(s)) => s.id.clone(),
            _ => return false,
        };
        let mut attempt = self.resume.as_ref().map_or(0, |r| r.attempt);
        // the host seems gone for good: the editors of the fail-over plan elect the next host (this device may be it)
        if self.standby.is_some() && (attempt >= 2 || self.electing.is_some()) && self.elect() {
            if self.mode == Mode::Hosting {
                return true;
            }
            attempt = attempt.min(RESUME_DELAYS.len() as u32 - 5);
        }
        if attempt as usize >= RESUME_DELAYS.len() {
            return false;
        }
        // tear down the dead transports, but not the replica, the workspace binding or the session description
        for c in self.conns.values() {
            c.sink.close();
        }
        self.conns.clear();
        self.iroh.stop();
        self.inq.clear();
        self.ops_in.clear();
        self.mode = Mode::Joining;
        self.joining_since = mono_ms();
        self.resume = Some(Resume {
            attempt: attempt + 1,
            next_at: mono_ms() + RESUME_DELAYS[attempt as usize],
            session,
        });
        if attempt == 0 {
            self.toast("Connection lost — reconnecting…");
        }
        true
    }

    fn hello(&self, hash: &[u8]) -> Msg {
        Msg::Hello {
            peer: self.me.clone(),
            auth: secure::proof("c", &self.token, hash),
            proto: PROTO,
        }
    }

    /// Writes the per-entity clocks next to the workspace (egui persisted data, keyed by the share id).
    fn save_stamps(&mut self) {
        let (Some(rep), Some(s)) = (self.rep.as_ref(), self.session.as_ref()) else {
            return;
        };
        if s.share.is_empty() {
            return;
        }
        let text = rep.export_stamps();
        self.ctx.data_mut(|d| {
            d.insert_persisted(egui::Id::new(("collab_stamps", s.share.clone())), text)
        });
        self.stamps_at = mono_ms();
    }

    fn load_stamps(&mut self) {
        let Some(share) = self
            .session
            .as_ref()
            .map(|s| s.share.clone())
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let text: Option<String> = self
            .ctx
            .data_mut(|d| d.get_persisted(egui::Id::new(("collab_stamps", share))));
        if let (Some(t), Some(rep)) = (text, self.rep.as_mut()) {
            rep.import_stamps(&t);
        }
    }

    pub fn stop(&mut self, notify: bool) {
        self.quorum = None;
        self.big = Default::default();
        if std::mem::take(&mut self.big_tracking) {
            crate::bigtable::track(false);
        }
        self.standby = None;
        self.standby_key = None;
        self.standby_secret = None;
        self.peer_addrs.clear();
        self.standby_sent = 0;
        self.electing = None;
        self.listen_pref = PORT;
        self.save_stamps();
        for c in self.conns.values() {
            c.sink.send(&Msg::Bye);
            c.sink.close();
        }
        if notify {
            self.set_hotspot(false);
            self.pending_wifi = None;
            #[cfg(target_os = "android")]
            crate::android::wifi_leave();
        }
        self.suggestions.clear();
        self.held_in.clear();
        self.reassert.clear();
        self.conns.clear();
        self.iroh.stop();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(s) = self.lan_adv_stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        if let Some(s) = self.mdns_adv_stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        if let Ok(mut g) = self.beacon.lock() {
            *g = None;
        }
        #[cfg(target_os = "android")]
        crate::android::nsd_unregister();
        self.mode = Mode::Off;
        self.resume = None;
        self.banned.clear();
        self.demoted.clear();
        self.baton = None;
        self.pending_baton_ack = None;
        self.inherited.clear();
        self.rep = None;
        self.session = None;
        self.roster.clear();
        self.requests.clear();
        self.presence.clear();
        self.inq.clear();
        self.ops_in.clear();

        self.addr_hint.clear();
        self.want_ws = None;
        self.ready = false;
        self.requested.clear();
        self.partial.clear();
        self.serving = None;
        ID_BASE.store(0, Ordering::Relaxed);
    }

    /// Listen for nearby hosts on the LAN via UDP broadcast + mDNS (desktop) and NsdManager (Android).
    /// Only active while the Nearby tab is open.
    pub fn discover(&mut self, on: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(s) = self.lan_disc_stop.take() {
                s.store(true, Ordering::Relaxed);
            }
            if on {
                let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                lan::browse(self.inbox.clone(), stop.clone());
                self.lan_disc_stop = Some(stop);
            } else {
                self.found.clear();
            }
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            if let Some(s) = self.mdns_disc_stop.take() {
                s.store(true, Ordering::Relaxed);
            }
            if on {
                let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                mdns::browse(self.inbox.clone(), stop.clone());
                self.mdns_disc_stop = Some(stop);
            }
        }
        #[cfg(target_os = "android")]
        {
            crate::android::nsd_discover(on);
        }
        #[cfg(target_arch = "wasm32")]
        {
            if !on {
                self.found.clear();
            }
        }
        self.ui.discovering = on;
    }

    // ───────────── host actions ─────────────

    /// Devices this host recognises: (key id, name, viewer).
    pub fn trusted_list(&self) -> Vec<(String, String, bool)> {
        let mut v: Vec<_> = self
            .trusted
            .iter()
            .map(|(k, (n, viewer))| (k.clone(), n.clone(), *viewer))
            .collect();
        v.sort_by(|a, b| a.1.cmp(&b.1));
        v
    }

    pub fn forget_trusted(&mut self, key: &str) {
        if self.trusted.remove(key).is_some() {
            self.save_trusted();
        }
    }

    fn save_trusted(&self) {
        let v: Vec<(String, String, bool)> = self
            .trusted
            .iter()
            .map(|(k, (n, viewer))| (k.clone(), n.clone(), *viewer))
            .collect();
        self.ctx
            .data_mut(|d| d.insert_persisted(egui::Id::new("collab_trusted"), v));
    }

    fn remember_device(&mut self, conn: ConnId, role: Role) {
        if role == Role::Suggester {
            return; // a suggester is asked again next time
        }
        let found = self.conns.get(&conn).and_then(|c| {
            let name = c.peer.as_ref()?.name.clone();
            let key = c.crypt.lock().ok()?.remote.clone();
            (!key.is_empty()).then_some((hex(&key), name))
        });
        if let Some((k, name)) = found {
            if self.trusted.len() < 200 {
                self.trusted.insert(k, (name, role == Role::Viewer));
                self.save_trusted();
            }
        }
    }

    pub fn approve(&mut self, conn: ConnId, role: Role) {
        if self.quorum.is_some() {
            self.toast("Waiting for the other editors to confirm the new host…");
            return;
        }
        self.remember_device(conn, role);
        self.requests.retain(|r| r.conn != conn);
        self.accept(conn, role);
    }

    pub fn deny(&mut self, conn: ConnId) {
        self.requests.retain(|r| r.conn != conn);
        if let Some(c) = self.conns.remove(&conn) {
            c.sink.send(&Msg::Denied {
                reason: "The host declined the request".into(),
            });
            c.sink.close();
        }
    }

    pub fn set_role(&mut self, id: u32, role: Role) {
        if self.quorum.is_some() {
            self.toast("Waiting for the other editors to confirm the new host…");
            return;
        }
        let mut changed = false;
        let mut keys: Vec<Vec<u8>> = vec![];
        for c in self.conns.values_mut() {
            if c.peer.as_ref().map_or(false, |p| p.id == id) {
                c.role = role;
                changed = true;
                if let Ok(x) = c.crypt.lock() {
                    keys.push(x.remote.clone());
                }
            }
        }
        let mut dirty = false;
        for k in keys.iter().filter(|k| !k.is_empty()) {
            if let Some(t) = self.trusted.get_mut(&hex(k)) {
                t.1 = role != Role::Editor;
                dirty = true;
            }
        }
        if dirty {
            self.save_trusted();
        }
        for k in keys.into_iter().filter(|k| !k.is_empty()) {
            if role != Role::Editor {
                self.demoted.insert(k);
            } else {
                self.demoted.remove(&k);
            }
        }
        if changed {
            for (p, r) in self.roster.iter_mut() {
                if p.id == id {
                    *r = role;
                }
            }
            self.broadcast_roster();
        }
    }

    pub fn kick(&mut self, id: u32) {
        if self.quorum.is_some() {
            self.toast("Waiting for the other editors to confirm the new host…");
            return;
        }
        // the fail-over secret the removed device knows must not open the successor's door
        self.standby_key = None;
        self.peer_addrs.remove(&id);
        let conn = self
            .conns
            .iter()
            .find(|(_, c)| c.peer.as_ref().map_or(false, |p| p.id == id))
            .map(|(k, _)| *k);
        if let Some(k) = conn {
            if let Some(c) = self.conns.remove(&k) {
                if let Ok(x) = c.crypt.lock() {
                    if !x.remote.is_empty() {
                        self.banned.insert(x.remote.clone());
                        if self.trusted.remove(&hex(&x.remote)).is_some() {
                            self.save_trusted();
                        }
                    }
                }
                c.sink.send(&Msg::Denied {
                    reason: "You were removed from the session".into(),
                });
                c.sink.close();
            }
            self.roster.retain(|(p, _)| p.id != id);
            self.presence.remove(&id);
            self.broadcast_roster();
        }
    }

    // ───────────── host hand-over (protocol 6) ─────────────

    /// People who could take over hosting: connected editors, by id.
    pub fn successors(&self) -> Vec<PeerInfo> {
        let mut v: Vec<PeerInfo> = self
            .conns
            .values()
            .filter(|c| c.active && c.role == Role::Editor)
            .filter_map(|c| c.peer.clone())
            .collect();
        v.sort_by_key(|p| p.id);
        v
    }

    /// True while a hand-over waits for the successor's answer.
    pub fn handing_over(&self) -> bool {
        self.baton.is_some()
    }

    /// Host: asks `to` (default: the editor with the lowest id) to host the session from now on. The successor starts listening, answers
    /// with its addresses and a fresh share token, and everybody — this device included, unless `leave` — re-connects to it with the usual
    /// resume (replica and workspace stay, the digest exchange repairs whatever differs). Nothing changes if the successor does not answer.
    pub fn hand_over(&mut self, to: Option<u32>, leave: bool) -> bool {
        if self.mode != Mode::Hosting || self.baton.is_some() {
            return false;
        }
        let pick = self
            .conns
            .iter()
            .filter(|(_, c)| c.active && c.role == Role::Editor)
            .filter_map(|(k, c)| c.peer.as_ref().map(|p| (p.id, *k, p.name.clone())))
            .filter(|(id, _, _)| to.map_or(true, |t| *id == t))
            .min_by_key(|(id, _, _)| *id);
        let Some((_, conn, name)) = pick else {
            self.toast("Nobody can take over right now");
            return false;
        };
        if let Some(c) = self.conns.get(&conn) {
            c.sink.send(&Msg::Baton { leave });
        }
        self.baton = Some(BatonWait {
            conn,
            at: mono_ms(),
            leave,
        });
        self.toast(format!(
            "Handing the session over to {}…",
            ui::first_name(&name)
        ));
        true
    }

    /// Successor: the host asks us to take over. Listen first, then tell the host where to send everybody.
    fn on_baton(&mut self, conn: ConnId) {
        if self.mode != Mode::Joined
            || !self.ready
            || self.rep.is_none()
            || !self.conns.get(&conn).map_or(false, |c| c.active)
        {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let can_host = true;
        if !can_host {
            if let Some(c) = self.conns.get(&conn) {
                c.sink.send(&Msg::BatonAck {
                    addrs: vec![],
                    relay: None,
                    token: String::new(),
                });
            }
            return;
        }
        self.inherited = self.roster.iter().map(|(p, r)| (p.id, *r)).collect();
        self.mode = Mode::Hosting;
        self.role = Role::Editor;
        self.token = random_code(10);
        self.track = true;
        self.changes.clear();
        self.resume = None;
        self.target = None;
        self.reused = false;
        self.open_listener(session.scope, session.name.clone(), true);
        // Never block the UI thread waiting for iroh: send BatonAck immediately if a ticket is
        // already ready, otherwise defer it to `poll()` until the ticket arrives (or 1.5 s elapses).
        if let Some(t) = self.iroh.ticket() {
            if let Some(c) = self.conns.get(&conn) {
                c.sink.send(&Msg::BatonAck {
                    addrs: vec![format!("iroh:{t}")],
                    relay: None,
                    token: self.token.clone(),
                });
            }
        } else {
            self.pending_baton_ack = Some((conn, mono_ms() + 1500.0));
        }
        self.update_beacon();
        self.next_scan = 0.0;
        self.toast("You are now hosting this session");
    }

    /// Old host: the successor listens. Tell everybody else, then follow it (or leave).
    fn on_baton_ack(
        &mut self,
        conn: ConnId,
        addrs: Vec<String>,
        relay: Option<(String, String)>,
        token: String,
    ) {
        let Some(b) = self.baton.take_if(|b| b.conn == conn) else {
            return;
        };
        if self.mode != Mode::Hosting {
            return;
        }
        if !valid_target(&addrs, &relay, &token) {
            self.toast("That device cannot host the session");
            return;
        }
        let moved = Msg::Moved {
            addrs: addrs.clone(),
            relay: relay.clone(),
            token: token.clone(),
        };
        for (cid, c) in &self.conns {
            if *cid != conn && c.active {
                c.sink.send(&moved);
            }
        }
        if b.leave {
            self.stop(true);
            self.toast("You handed the session over");
        } else if !self.follow(addrs, relay, token) {
            self.toast("Could not follow the new host");
        }
    }

    /// Participant: the host moved the session to another device.
    fn on_moved(
        &mut self,
        conn: ConnId,
        addrs: Vec<String>,
        relay: Option<(String, String)>,
        token: String,
    ) {
        if self.mode != Mode::Joined
            || !self.conns.get(&conn).map_or(false, |c| c.active)
            || !valid_target(&addrs, &relay, &token)
        {
            return;
        }
        if !self.follow(addrs, relay, token) {
            self.toast("The session moved, but the new host cannot be reached");
        }
    }

    /// Drops every transport and re-dials the new host as a *resumed* join (replica, workspace and session description stay).
    fn follow(
        &mut self,
        addrs: Vec<String>,
        relay: Option<(String, String)>,
        token: String,
    ) -> bool {
        let _ = relay;
        let Some(first) = addrs.first() else {
            return false;
        };
        let ticket = first.strip_prefix("iroh:").unwrap_or(first).to_owned();
        let target = Target::Iroh { ticket, token };
        let Some(session) = self.session.as_ref().map(|s| s.id.clone()) else {
            return false;
        };
        if self.rep.is_none() || !self.ready {
            return false;
        }
        self.host_pin = None;
        for c in self.conns.values() {
            c.sink.close();
        }
        self.conns.clear();
        self.iroh.stop();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(s) = self.lan_adv_stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        if let Some(s) = self.mdns_adv_stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        if let Ok(mut g) = self.beacon.lock() {
            *g = None;
        }
        #[cfg(target_os = "android")]
        crate::android::nsd_unregister();
        self.requests.clear();
        self.suggestions.clear();
        self.held_in.clear();
        self.reassert.clear();
        self.banned.clear();
        self.demoted.clear();
        self.inq.clear();
        self.ops_in.clear();
        self.addr_hint.clear();
        self.baton = None;
        self.standby = None;
        self.standby_sent = 0;
        self.electing = None;
        self.target = Some(target);
        self.mode = Mode::Joining;
        self.role = Role::Editor;
        self.joining_since = mono_ms();
        self.resume = Some(Resume {
            attempt: 0,
            next_at: mono_ms() + 600.0,
            session,
        });
        self.toast("The host changed — reconnecting…");
        true
    }

    fn accept(&mut self, conn: ConnId, role: Role) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.conns.values().filter(|c| c.active).count() >= MAX_PEERS {
            if let Some(c) = self.conns.remove(&conn) {
                c.sink.send(&Msg::Denied {
                    reason: "This session is full".into(),
                });
                c.sink.close();
            }
            return;
        }
        let mut token = self.token.clone();
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        c.active = true;
        c.role = role;
        let Some(peer) = c.peer.clone() else { return };
        if c.via_standby {
            if let Some(sec) = &self.standby_secret {
                token = standby_cap(sec, peer.id);
            }
        } else if let Some(g) = &c.grant_token {
            token = g.clone();
        }
        let hash = c.crypt.lock().map(|x| x.hash.clone()).unwrap_or_default();
        let spk = {
            self.ensure_signer();
            self.host_key
                .as_ref()
                .map(|k| k.public_hex())
                .unwrap_or_default()
        };
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        c.sink.send(&Msg::Welcome {
            peer: self.me.clone(),
            session,
            role,
            auth: secure::proof("s", &token, &hash),
            spk,
        });
        self.roster.retain(|(p, _)| p.id != peer.id);
        self.roster.push((peer.clone(), role));
        self.broadcast_roster();
        self.toast(format!("{} joined", peer.name));
        self.send_digest(conn);
        self.update_beacon();
    }

    fn update_beacon(&self) {
        if let Ok(mut g) = self.beacon.lock() {
            if let Some(b) = g.as_mut() {
                b.people = self.roster.len() as u32;
                b.name = self.me.name.clone();
            }
        }
    }

    fn broadcast_roster(&mut self) {
        let m = Msg::Roster {
            peers: self.roster.clone(),
        };
        for c in self.conns.values().filter(|c| c.active) {
            c.sink.send(&m);
        }
        self.update_beacon();
        self.push_standby();
    }

    /// Host: tells every device who takes over if this host disappears (and how to prove itself to that successor).
    fn push_standby(&mut self) {
        if self.mode != Mode::Hosting || self.session.is_none() {
            return;
        }
        let key = self
            .standby_key
            .get_or_insert_with(|| random_code(16))
            .clone();
        let mut order: Vec<u32> = self
            .roster
            .iter()
            .filter(|(p, r)| *r == Role::Editor && p.id != self.me.id)
            .map(|(p, _)| p.id)
            .collect();
        order.sort_unstable();
        order.truncate(6);
        let relay: Option<(String, String)> = None;
        let addrs: Vec<(u32, Vec<String>)> = order
            .iter()
            .filter_map(|id| self.peer_addrs.get(id).map(|a| (*id, a.clone())))
            .collect();
        self.ensure_signer();
        self.plan_seq += 1;
        let seq = self.plan_seq;
        let now = now_ms();
        self.grants.prune(now);
        let grants_json = self.grants.live_json(now);
        let spk = self
            .host_key
            .as_ref()
            .map(|k| k.public_hex())
            .unwrap_or_default();
        for c in self.conns.values().filter(|c| c.active) {
            let Some(p) = c.peer.as_ref() else { continue };
            let secret = if c.role == Role::Editor {
                key.clone()
            } else {
                String::new()
            };
            let cap = standby_cap(&key, p.id);
            let grants = if c.role == Role::Editor {
                grants_json.clone()
            } else {
                String::new()
            };
            let sig = self
                .host_key
                .as_ref()
                .map(|k| {
                    k.sign_hex(&access::plan_bytes(
                        self.me.id, &order, &addrs, &secret, &cap, &relay, &grants, seq,
                    ))
                })
                .unwrap_or_default();
            c.sink.send(&Msg::Standby {
                host: self.me.id,
                order: order.clone(),
                addrs: addrs.clone(),
                secret,
                cap,
                relay: relay.clone(),
                spk: spk.clone(),
                sig,
                seq,
                grants,
            });
        }
    }

    fn on_standby(&mut self, conn: ConnId, sb: Standby) {
        if self.mode != Mode::Joined
            || !self.conns.get(&conn).map_or(false, |c| c.active)
            || sb.order.len() > 8
            || sb.addrs.len() > 8
            || sb.cap.len() > 40
            || sb.secret.len() > 40
        {
            return;
        }
        if sb
            .addrs
            .iter()
            .any(|(_, a)| a.len() > 12 || (!a.is_empty() && !valid_target(a, &None, &sb.cap)))
        {
            return;
        }
        // protocol 8: only a plan signed by the pinned host key counts (a forged plan could name an attacker as the successor)
        let signed = !sb.sig.is_empty()
            && !sb.spk.is_empty()
            && self.host_pin.as_deref().map_or(true, |p| p == sb.spk)
            && access::verify(
                &sb.spk,
                &access::plan_bytes(
                    sb.host, &sb.order, &sb.addrs, &sb.secret, &sb.cap, &sb.relay, &sb.grants,
                    sb.seq,
                ),
                &sb.sig,
            );
        if !signed
            || self.standby.as_ref().map_or(false, |old| {
                old.host == sb.host && old.spk == sb.spk && sb.seq <= old.seq
            })
        {
            if !signed {
                self.toast("Ignored a fail-over plan that is not signed by the host");
            }
            return;
        }
        if self.host_pin.is_none() {
            self.host_pin = Some(sb.spk.clone());
        }
        let first_for_host = self.standby_sent != sb.host;
        self.standby = Some(sb.clone());
        if first_for_host && sb.order.contains(&self.me.id) && !sb.secret.is_empty() {
            self.standby_sent = sb.host;
            let my_id = iroh_transport::secret_key_from_seed(self.iroh_seed())
                .public()
                .to_string();
            if let Some(c) = self.conns.get(&conn) {
                c.sink.send(&Msg::StandbyAddr {
                    addrs: vec![format!("iroh:{my_id}")],
                });
            }
        }
    }

    fn on_standby_addr(&mut self, conn: ConnId, addrs: Vec<String>) {
        let Some(id) = self
            .conns
            .get(&conn)
            .filter(|c| c.active && c.role == Role::Editor)
            .and_then(|c| c.peer.as_ref().map(|p| p.id))
        else {
            return;
        };
        if self.mode != Mode::Hosting
            || addrs.len() > 12
            || addrs.iter().any(|a| {
                if let Some(raw) = a.strip_prefix("iroh:") {
                    raw.is_empty()
                        || raw.len() > 480
                        || iroh_transport::parse_endpoint_addr(raw).is_err()
                } else {
                    a.len() > 64 || a.parse::<std::net::SocketAddr>().is_err()
                }
            })
            || (self.peer_addrs.len() > 64 && !self.peer_addrs.contains_key(&id))
        {
            return;
        }
        self.peer_addrs.insert(id, addrs);
        self.push_standby();
    }

    /// A device that lost its host: picks the first editor of the plan that is not known to be dead. That device takes over (it is us, or
    /// the target is re-pointed at it). `true` = something to wait for.
    fn elect(&mut self) -> bool {
        let Some(sb) = self.standby.clone() else {
            return false;
        };
        let now = mono_ms();
        let el = self.electing.get_or_insert(Elect {
            tried: vec![],
            cand: 0,
            since: now,
        });
        if el.cand != 0 && now - el.since > ELECT_WINDOW_MS {
            let dead = el.cand;
            el.tried.push(dead);
            el.cand = 0;
        }
        let can_host = true;
        loop {
            let tried = self
                .electing
                .as_ref()
                .map(|e| e.tried.clone())
                .unwrap_or_default();
            let Some(cand) = sb
                .order
                .iter()
                .copied()
                .find(|id| *id != sb.host && !tried.contains(id))
            else {
                return false;
            };
            if cand == self.me.id {
                if sb.secret.is_empty() || !can_host {
                    if let Some(e) = self.electing.as_mut() {
                        e.tried.push(cand);
                    }
                    continue;
                }
                self.promote(sb);
                return true;
            }
            if self.electing.as_ref().map_or(0, |e| e.cand) != cand {
                let addrs = sb
                    .addrs
                    .iter()
                    .find(|(id, _)| *id == cand)
                    .map(|(_, a)| a.clone())
                    .unwrap_or_default();
                let Some(first) = addrs.first() else {
                    if let Some(e) = self.electing.as_mut() {
                        e.tried.push(cand);
                    }
                    continue;
                };
                let target = Target::Iroh {
                    ticket: first.strip_prefix("iroh:").unwrap_or(first).to_owned(),
                    token: sb.cap.clone(),
                };
                self.target = Some(target);
                self.host_pin = None;
                if let Some(e) = self.electing.as_mut() {
                    e.cand = cand;
                    e.since = now;
                }
                self.toast("The host disappeared — switching to another device…");
            }
            return true;
        }
    }

    /// This device is the successor: it becomes the host of the same session (replica, workspace and roles stay).
    fn promote(&mut self, sb: Standby) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.inherited = self.roster.iter().map(|(p, r)| (p.id, *r)).collect();
        self.grants = access::Grants::from_json(&sb.grants);
        // three or more editors: this device is only a *provisional* host until a majority of them has reconnected (`check_quorum`)
        self.quorum = access::needs_quorum(sb.order.len()).then(|| Quorum {
            need: access::quorum(sb.order.len()),
            since: mono_ms(),
            plan: sb.clone(),
        });
        self.roster.retain(|(p, _)| p.id != sb.host);
        self.mode = Mode::Hosting;
        self.role = Role::Editor;
        self.token = random_code(10);
        self.standby_secret = Some(sb.secret.clone());
        self.standby_key = Some(sb.secret.clone());
        self.standby = None;
        self.standby_sent = 0;
        self.electing = None;
        self.peer_addrs.clear();
        self.track = true;
        self.changes.clear();
        self.resume = None;
        self.target = None;
        self.reused = false;
        for c in self.conns.values() {
            c.sink.close();
        }
        self.conns.clear();
        self.listen_pref = standby_port(self.me.id);
        self.open_listener(session.scope, session.name.clone(), false);
        self.update_beacon();
        self.next_scan = 0.0;
        self.toast("The host disappeared — you are now hosting this session");
    }

    fn ensure_signer(&mut self) {
        if self.host_key.is_none() {
            self.host_key = self
                .keys
                .as_ref()
                .map(|k| access::HostKey::from_device(k.private_bytes()));
        }
    }

    /// Provisional host: confirmed once a majority of the plan's editors (this device included) is connected through their caps;
    /// otherwise the device steps down and the election goes on with the next editor.
    fn check_quorum(&mut self, now: f64) {
        let Some(q) = &self.quorum else { return };
        if self.mode != Mode::Hosting {
            self.quorum = None;
            return;
        }
        let voters = 1 + self
            .conns
            .values()
            .filter(|c| {
                c.active
                    && c.via_standby
                    && c.role == Role::Editor
                    && c.peer
                        .as_ref()
                        .map_or(false, |p| q.plan.order.contains(&p.id))
            })
            .count();
        if voters >= q.need {
            self.quorum = None;
            self.toast("A majority of the editors confirmed you as the host");
            self.push_standby();
            return;
        }
        if now - q.since > QUORUM_MS {
            let Some(q) = self.quorum.take() else { return };
            self.step_down(q.plan);
        }
    }

    /// The quorum was not reached (this device is in the minority of a partition): give the host role up and let the election continue.
    fn step_down(&mut self, sb: Standby) {
        self.toast("Not enough editors could be reached — stepping back from hosting");
        for c in self.conns.values() {
            c.sink.close();
        }
        self.conns.clear();
        self.roster
            .retain(|(p, _)| p.id == self.me.id || p.id != sb.host);
        self.mode = Mode::Joined;
        self.token = String::new();
        self.standby_secret = None;
        self.grants = access::Grants::from_json(&self.grants.live_json(now_ms()));
        self.standby = Some(sb.clone());
        let mut tried = vec![sb.host, self.me.id];
        if let Some(e) = self.electing.take() {
            tried.extend(e.tried);
        }
        self.electing = Some(Elect {
            tried,
            cand: 0,
            since: mono_ms(),
        });
        self.iroh.stop();
        let next = self.elect();
        if !next || !self.try_resume("Connection lost", true) {
            self.stop(false);
            self.last_error = Some(
                "Not enough editors to continue the session — the shared copy stays on this device"
                    .into(),
            );
        }
    }

    // ───────────── share links of one document (grants) ─────────────

    fn save_grants(&self) {
        let json = self.grants.live_json(now_ms());
        self.ctx
            .data_mut(|d| d.insert_persisted(egui::Id::new("collab_grants"), json));
    }

    /// Host: mints a link with its own role, lifetime and use count. Returns the token.
    pub fn create_grant(
        &mut self,
        role: Role,
        label: &str,
        ttl_ms: u64,
        max_uses: u32,
    ) -> Option<String> {
        if self.mode != Mode::Hosting || self.quorum.is_some() {
            return None;
        }
        let g = self
            .grants
            .create(role, label, ttl_ms, max_uses, now_ms())?;
        self.save_grants();
        self.push_standby();
        Some(g.token)
    }

    /// Host: revokes a link and drops the devices that came in with it.
    pub fn revoke_grant(&mut self, id: &str) {
        let Some(token) = self.grants.revoke(id) else {
            return;
        };
        self.drop_grant_conns(&token, "This link was revoked");
        self.save_grants();
        self.push_standby();
    }

    fn drop_grant_conns(&mut self, token: &str, why: &str) {
        let ids: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.grant_token.as_deref() == Some(token))
            .map(|(k, _)| *k)
            .collect();
        for id in ids {
            if let Some(c) = self.conns.get(&id) {
                c.sink.send(&Msg::Denied { reason: why.into() });
            }
            self.drop_conn(id, why);
        }
    }

    /// Host: a device that came in through a link that has expired in the meantime is removed.
    fn enforce_grants(&mut self) {
        if self.mode != Mode::Hosting || self.grants.list.is_empty() {
            return;
        }
        let now = now_ms();
        let dead: Vec<String> =
            self.conns
                .values()
                .filter_map(|c| c.grant_token.clone())
                .filter(|t| {
                    // Only drop active connections whose grant was explicitly revoked or time-expired
                    // (never kick a joiner simply because their own join consumed the last max_uses slot).
                    self.grants.list.iter().any(|g| {
                        g.token == *t && (g.revoked || (g.expires != 0 && now >= g.expires))
                    })
                })
                .collect();
        for t in dead {
            self.drop_grant_conns(&t, "This link has expired");
        }
    }

    /// The universal Iroh share link for a grant (same ticket, grant-specific token).
    pub fn grant_link(&self, token: &str, base: &str) -> Option<String> {
        let ticket = self.iroh.ticket()?;
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() {
            Some(format!("{base}/app?iroh={}&t={token}", pct_encode(&ticket)))
        } else {
            Some(format!(
                "lumen://join?iroh={}&t={token}",
                pct_encode(&ticket)
            ))
        }
    }

    pub fn grant_lan_text(&self, token: &str) -> Option<String> {
        let ticket = self.iroh.ticket()?;
        Some(format!(
            "lumen://join?iroh={}&t={token}",
            pct_encode(&ticket)
        ))
    }

    pub fn provisional(&self) -> bool {
        self.quorum.is_some()
    }

    fn send_digest(&mut self, conn: ConnId) {
        let Some(rep) = self.rep.as_ref() else { return };
        let items = rep.digest();
        let parts = chunked(items, 1500);
        let n = parts.len();
        if let Some(c) = self.conns.get_mut(&conn) {
            if n == 0 {
                c.backlog.push_back(Msg::Digest {
                    items: vec![],
                    last: true,
                });
            }
            for (i, p) in parts.into_iter().enumerate() {
                c.backlog.push_back(Msg::Digest {
                    items: p,
                    last: i + 1 == n,
                });
            }
        }
    }

    // ───────────── per-frame work ─────────────

    /// The app asks once per frame. `Some(name)`: create + switch to a fresh workspace for the shared content, then call [`bind`].
    pub fn take_new_workspace(&mut self) -> Option<NewWs> {
        self.want_ws.take()
    }

    /// The app created an empty workspace for this session: the host is asked for everything.
    pub fn bind(&mut self, ws_id: u64) {
        self.bound = ws_id;
        self.ready = true;
        self.next_scan = 0.0;
        self.bound_at = mono_ms();
        self.last_op_at = self.bound_at;
        for c in self.conns.values_mut().filter(|c| c.active) {
            c.backlog.push_back(Msg::Digest {
                items: vec![],
                last: true,
            });
        }
    }

    /// The app re-opened the workspace this device got from the same share before. Its content becomes the replica (stamped with the
    /// time of the last sync, so whatever the host changed since wins and whatever was edited here offline is sent), only the
    /// difference travels, and everything that arrives is recorded for review.
    pub fn bind_existing(
        &mut self,
        ws: &mut crate::model::Workspace,
        ws_id: u64,
        last_sync_ms: u64,
    ) {
        self.bound = ws_id;
        self.ready = true;
        self.next_scan = 0.0;
        self.bound_at = mono_ms();
        self.last_op_at = self.bound_at;
        self.away = ws
            .pages
            .iter()
            .filter(|p| {
                !p.trashed
                    && !p.row
                    && p.updated.is_finite()
                    && p.updated > last_sync_ms as f64 + 1500.0
            })
            .map(|p| (p.id, p.title.clone()))
            .take(200)
            .collect();
        if let Some(rep) = self.rep.as_mut() {
            rep.adopt(ws, last_sync_ms.min(now_ms()));
        }
        self.load_stamps();
        self.track = true;
        self.reused = true;
        self.prompted = false;
        let conns: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.active)
            .map(|(k, _)| *k)
            .collect();
        for c in conns {
            self.send_digest(c);
        }
    }

    /// Opens the review window and tells the user (first time after a re-join).
    fn announce_changes(&mut self) {
        let n = self.changes.len() + self.away.len();
        if n > 0 {
            self.ui.changes = true;
            self.toast(format!(
                "{n} change{} since you were last here",
                if n == 1 { "" } else { "s" }
            ));
        } else {
            self.toast("Up to date — nothing changed since you were last here");
        }
    }

    pub fn take_media(&mut self) -> Vec<(u64, String, String, Vec<u8>)> {
        std::mem::take(&mut self.media_done)
    }

    /// Chunk requests from peers: (connection, media id, offset). The app answers through [`serve_media`].
    pub fn take_media_requests(&mut self) -> Vec<(ConnId, u64, u64)> {
        std::mem::take(&mut self.media_reqs)
    }

    pub fn serve_media(
        &mut self,
        conn: ConnId,
        id: u64,
        off: u64,
        name: &str,
        mime: &str,
        bytes: Option<&[u8]>,
    ) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        match bytes {
            None => c.sink.send(&Msg::MediaMiss { id }),
            Some(b) => {
                const CH: usize = 192 * 1024;
                let s = (off as usize).min(b.len());
                let e = (s + CH).min(b.len());
                c.sink.send(&Msg::MediaData {
                    id,
                    off,
                    total: b.len() as u64,
                    name: name.to_owned(),
                    mime: mime.to_owned(),
                    data: crate::media::b64_encode(&b[s..e]),
                });
            }
        }
    }

    pub fn set_view(&mut self, page: Option<u64>, block: Option<u64>, cur: Option<[f32; 2]>) {
        self.local.page = page;
        self.local.block = block;
        self.local.cur = cur;
        self.local.caret = caret_state()
            .filter(|c| Some(c.0) == block)
            .map(|c| (c.1, c.2));
        self.local.draft = draft_state();
        self.local.typing = typing_active();
        self.local.len = if self.local.caret.is_some() {
            caret_len()
        } else {
            0
        };
    }

    pub fn set_reading(&mut self, pdf: Option<(u64, u32)>) {
        self.local.pdf = pdf;
    }

    /// Typing / dragging happened: look for changes soon.
    pub fn poke(&mut self) {
        let t = mono_ms() + 90.0;
        if self.next_scan > t {
            self.next_scan = t;
        }
    }

    /// Runs the session. Returns `true` when the workspace changed because of a remote edit.
    pub fn poll(
        &mut self,
        ws: &mut crate::model::Workspace,
        ws_id: u64,
        current: Option<u64>,
        has_media: &dyn Fn(u64) -> bool,
    ) -> bool {
        #[cfg(target_os = "android")]
        {
            let want = matches!(self.mode, Mode::Hosting | Mode::Joined);
            if want != self.svc_on {
                self.svc_on = want;
                let text = if self.mode == Mode::Hosting {
                    "Sharing a workspace — tap to open"
                } else {
                    "Connected to a shared workspace — tap to open"
                };
                crate::android::collab_service(want, text);
            }
        }
        #[cfg(not(target_os = "android"))]
        let _ = self.svc_on;
        if self.mode == Mode::Off
            && self.pending_wifi.is_none()
            && self.hs.0 == "idle"
            && !self.ui.dialog
            && self.inbox.q.lock().map_or(true, |q| q.is_empty())
        {
            return false;
        }
        let now = mono_ms();
        if let Some((conn, deadline)) = self.pending_baton_ack {
            let ready = self.iroh.ticket();
            if ready.is_some() || now >= deadline {
                self.pending_baton_ack = None;
                let addrs = ready
                    .or_else(|| self.iroh.endpoint_id())
                    .map(|t| vec![format!("iroh:{t}")])
                    .unwrap_or_default();
                if let Some(c) = self.conns.get(&conn) {
                    c.sink.send(&Msg::BatonAck {
                        addrs,
                        relay: None,
                        token: self.token.clone(),
                    });
                }
            }
        }
        self.poll_wifi(now);
        self.poll_hotspot(now);
        // Keep the LAN UDP/mDNS/NSD beacon's `iroh` field synchronized with the live ticket (includes local IP:port for offline LAN dialing).
        if self.mode == Mode::Hosting {
            if let Some(tkt) = self.iroh.ticket().or_else(|| self.iroh.endpoint_id()) {
                let mut updated = false;
                if let Ok(mut g) = self.beacon.lock() {
                    if let Some(b) = g.as_mut() {
                        if b.iroh != tkt {
                            b.iroh = tkt;
                            updated = true;
                        }
                    }
                }
                #[cfg(target_os = "android")]
                if updated {
                    if let Some(b) = self.beacon.lock().ok().and_then(|g| g.clone()) {
                        let mut compact = b.clone();
                        compact.title = compact.title.chars().take(40).collect();
                        if let Ok(j) = serde_json::to_string(&compact) {
                            crate::android::nsd_register(&format!("{:x}", self.me.id), PORT, &j);
                        }
                    }
                }
                let _ = updated;
            }
        }
        if matches!(self.mode, Mode::Hosting | Mode::Joined) && now - self.stamps_at > 120_000.0 {
            self.save_stamps();
        }
        if self.baton.as_ref().map_or(false, |b| now - b.at > 8000.0) {
            self.baton = None;
            self.toast("The hand-over was not confirmed — you are still the host");
        }
        if self.resume.as_ref().map_or(false, |r| now >= r.next_at) && self.conns.is_empty() {
            if let Some(r) = self.resume.as_mut() {
                r.next_at = f64::MAX; // until this attempt fails (Closed / reap) or succeeds (Welcome)
            }
            self.dial();
        }
        self.handle_events();
        if now - self.last_reap > 2000.0 {
            self.last_reap = now;
            self.reap(now);
        }
        if now - self.last_gc > 60_000.0 {
            self.last_gc = now;
            if let Some(r) = self.rep.as_mut() {
                self.stats.gc += r.gc(now_ms().saturating_sub(TOMBSTONE_GRACE_MS)) as u64;
            }
        }
        let mut changed = false;
        let usable = self.ready && self.rep.is_some() && self.bound == ws_id;
        if usable {
            let t_start = now;
            let mut applied = Applied {
                track: self.track,
                ..Default::default()
            };
            let mut forward: Vec<(ConnId, Op)> = vec![];
            // protocol messages that wait for a workspace
            while let Some((conn, msg)) = self.inq.pop_front() {
                self.handle_ready(ws, conn, msg);
                if mono_ms() - t_start > 3.0 {
                    break;
                }
            }
            if self.big_tick(ws, now) {
                changed = true;
            }
            // suggestions of suggesting devices: described (what was there / what is proposed) and held for the host
            while let Some((conn, op)) = self.held_in.pop_front() {
                self.hold_suggestion(ws, conn, op);
            }
            if !self.reassert.is_empty() {
                let list = std::mem::take(&mut self.reassert);
                let mut ops = vec![];
                if let Some(rep) = self.rep.as_mut() {
                    for (key, min) in list {
                        ops.extend(rep.reassert(ws, key, min, now_ms()));
                    }
                }
                for c in self.conns.values_mut().filter(|c| c.active) {
                    for part in chunked(ops.clone(), 120) {
                        c.backlog.push_back(Msg::Ops { ops: part });
                    }
                }
            }
            // remote operations, bounded per frame so a big initial sync never freezes the UI
            let mut count = 0;
            while let Some((conn, op)) = self.ops_in.pop_front() {
                if let Some(rep) = self.rep.as_mut() {
                    if rep.apply(ws, &op, now_ms(), &mut applied) {
                        forward.push((conn, op));
                    }
                }
                count += 1;
                if count % 32 == 0 && mono_ms() - t_start > 4.0 {
                    break;
                }
            }
            if applied.n > 0 {
                if let Some(rep) = self.rep.as_mut() {
                    rep.finish(ws, &applied);
                }
                changed = true;
                self.last_op_at = now;
                if self.track {
                    let before = self.changes.len();
                    self.changes.other += applied.other;
                    for ch in std::mem::take(&mut applied.log) {
                        self.changes.push(ch);
                    }
                    if self.mode == Mode::Hosting
                        && self.changes.len() > before
                        && !self.ui.changes
                        && now - self.ui.copied_at > 8000.0
                    {
                        self.ui.copied_at = now;
                        self.toast("Someone changed the page — open Changes to review");
                    }
                }
                self.stats.ops_in += applied.n as u64;
                if ws.current.map_or(true, |c| ws.page(c).is_none()) {
                    let root = self
                        .session
                        .as_ref()
                        .and_then(|s| s.roots.first().copied())
                        .filter(|r| ws.page(*r).is_some());
                    ws.current = root.or_else(|| {
                        ws.pages
                            .iter()
                            .find(|p| !p.trashed && !p.row && p.parent.is_none())
                            .map(|p| p.id)
                    });
                }
                if current.map_or(false, |c| applied.pages.contains(&c)) {
                    self.check_media = true;
                }
                // the host fans accepted operations out to everybody else
                if self.mode == Mode::Hosting {
                    let mut by_conn: HashMap<ConnId, Vec<Op>> = HashMap::new();
                    for (src, op) in forward {
                        for (cid, c) in &self.conns {
                            if *cid != src && c.active {
                                by_conn.entry(*cid).or_default().push(op.clone());
                            }
                        }
                    }
                    for (cid, ops) in by_conn {
                        if let Some(c) = self.conns.get_mut(&cid) {
                            for part in chunked(ops, 120) {
                                c.backlog.push_back(Msg::Ops { ops: part });
                            }
                        }
                    }
                }
            }
            // after the first sync has settled: a fresh joiner starts recording, a returning one reviews what happened while away
            if self.mode == Mode::Joined
                && self.ops_in.is_empty()
                && self.inq.is_empty()
                && now - self.last_op_at > 1500.0
                && now - self.bound_at > 2500.0
            {
                if !self.track {
                    self.track = true;
                }
                if self.reused && !self.prompted {
                    self.prompted = true;
                    self.announce_changes();
                }
            }
            // local changes
            let mut hot: Vec<Op> = vec![];
            if self.role != Role::Viewer {
                if let Some(r) = self.rep.as_mut() {
                    hot = r.tick_hot(ws, now_ms());
                    // the block being typed in leaves in the same frame (keeps the other side's text and caret in step)
                    if let (Some(cur), Some(b)) = (current, self.local.block) {
                        hot.extend(r.tick_block(ws, cur, b, now_ms()));
                    }
                }
            }
            if !hot.is_empty() {
                self.stats.ops_out += hot.len() as u64;
                for c in self.conns.values_mut().filter(|c| c.active) {
                    for part in chunked(hot.clone(), 120) {
                        c.backlog.push_back(Msg::Ops { ops: part });
                    }
                }
            }
            if now >= self.next_scan && self.role != Role::Viewer {
                let cost0 = mono_ms();
                let ops = self
                    .rep
                    .as_mut()
                    .map(|r| r.tick(ws, current, now_ms(), 2.5))
                    .unwrap_or_default();
                let cost = mono_ms() - cost0;
                self.stats.scan_ms = cost;
                self.stats.entities = self.rep.as_ref().map_or(0, |r| r.len());
                // spend at most ~4 % of the time scanning, never wait longer than 1.5 s
                self.next_scan = now + (cost * 25.0).clamp(160.0, 1500.0);
                if !ops.is_empty() {
                    self.stats.ops_out += ops.len() as u64;
                    for c in self.conns.values_mut().filter(|c| c.active) {
                        for part in chunked(ops.clone(), 120) {
                            c.backlog.push_back(Msg::Ops { ops: part });
                        }
                    }
                }
            }
            // media this page needs
            if self.last_page != current {
                self.last_page = current;
                self.check_media = true;
            }
            if self.check_media {
                self.check_media = false;
                if let (Some(rep), Some(cur)) = (self.rep.as_ref(), current) {
                    let mut want = HashSet::new();
                    rep.referenced_media(ws, cur, &mut want);
                    for id in want {
                        if !has_media(id) {
                            self.request_media(id, 0);
                        }
                    }
                }
            }
        }
        // presence
        if self.mode != Mode::Off
            && now - self.last_presence_tx > 60.0
            && (self.local != self.sent_local || now - self.last_presence_tx > 3000.0)
        {
            self.last_presence_tx = now;
            self.sent_local = self.local.clone();
            let m = Msg::Presence(self.local.clone());
            for c in self.conns.values().filter(|c| c.active) {
                c.sink.send(&m);
            }
        }
        // flow-controlled sending: a few frames per connection per frame
        for c in self.conns.values_mut() {
            for _ in 0..6 {
                match c.backlog.pop_front() {
                    Some(m) => c.sink.send(&m),
                    None => break,
                }
            }
        }
        // stale presence
        self.presence.retain(|_, (_, t)| now - *t < 12_000.0);
        self.found.retain(|f| {
            let ttl = if f.ip.starts_with("relay:") {
                16_000.0
            } else {
                15_000.0
            };
            now - f.seen < ttl
        });

        if self.mode != Mode::Off {
            self.ctx
                .request_repaint_after(std::time::Duration::from_millis(
                    if self.conns.values().any(|c| !c.backlog.is_empty()) || !self.ops_in.is_empty()
                    {
                        8
                    } else {
                        ((self.next_scan - now).clamp(120.0, 1000.0)) as u64
                    },
                ));
        } else if self.ui.discovering {
            self.ctx
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
        changed
    }

    /// Describes one operation of a suggesting device and keeps it until the host decides (a newer operation on the same thing replaces it).
    fn hold_suggestion(&mut self, ws: &crate::model::Workspace, conn: ConnId, op: Op) {
        let Some(who) = self
            .conns
            .get(&conn)
            .and_then(|c| c.peer.as_ref().map(|p| p.id))
        else {
            return;
        };
        if let Some(r) = self.rep.as_mut() {
            r.clock.observe(op.e.hlc, now_ms());
        }
        let (what, page, block, title, before, after) =
            crdt::describe(ws, &op).unwrap_or_else(|| {
                let page = match op.key {
                    Key::Node(p, _)
                    | Key::Edge(p, _)
                    | Key::Page(p)
                    | Key::Block(p, _)
                    | Key::Comment(p, _)
                    | Key::Annot(p, _) => p,
                };
                let what = match op.key {
                    Key::Edge(..) => "Edge",
                    Key::Annot(..) => "Mark",
                    _ => "Canvas",
                };
                (
                    what,
                    page,
                    0,
                    ws.page(page).map(|p| p.title.clone()).unwrap_or_default(),
                    String::new(),
                    if op.e.val.is_some() {
                        "(changed)"
                    } else {
                        "(removed)"
                    }
                    .to_owned(),
                )
            });
        let at = now_ms();
        if let Some(x) = self
            .suggestions
            .iter_mut()
            .find(|x| x.who == who && x.op.key == op.key)
        {
            x.after = after;
            x.op = op;
            x.at = at;
            return;
        }
        if self.suggestions.len() >= 500 {
            return;
        }
        let first = self.suggestions.is_empty();
        self.suggestions.push(changes::Suggestion {
            conn,
            who,
            op,
            what,
            page,
            block,
            title,
            before,
            after,
            at,
        });
        if first {
            self.ui.changes = true;
            self.toast("A suggestion is waiting — open Changes to accept or refuse it");
        }
    }

    /// Host: apply (`accept`) or undo (`refuse`) held suggestions by index into [`Collab::suggestions`].
    pub fn resolve_suggestions(&mut self, accept: &[usize], refuse: &[usize]) {
        let mut drop: Vec<usize> = accept
            .iter()
            .chain(refuse.iter())
            .copied()
            .filter(|i| *i < self.suggestions.len())
            .collect();
        drop.sort_unstable();
        drop.dedup();
        for i in drop.into_iter().rev() {
            let sg = self.suggestions.remove(i);
            if accept.contains(&i) {
                self.ops_in.push_front((sg.conn, sg.op));
            } else {
                self.reassert.push((sg.op.key, sg.op.e.hlc));
            }
        }
    }

    fn request_media(&mut self, id: u64, off: u64) {
        let now = mono_ms();
        if off == 0
            && self
                .requested
                .get(&id)
                .map_or(false, |t| now - *t < 15_000.0)
        {
            return;
        }
        self.requested.insert(id, now);
        for c in self.conns.values().filter(|c| c.active) {
            c.sink.send(&Msg::MediaReq { id, off });
        }
    }

    // ───────────── message handling ─────────────

    fn handle_events(&mut self) {
        #[cfg(target_os = "android")]
        for (b, ip) in crate::android::nsd_take() {
            self.inbox.push(Event::Found(b, ip));
        }
        for ev in self.inbox.drain() {
            match ev {
                Event::Found(mut b, ip) => {
                    let is_relay = ip.starts_with("relay:");
                    if b.id == self.me.id || (b.port == 0 && !is_relay) {
                        continue;
                    }
                    // beacons are unauthenticated UDP/HTTP: bound and sanitise everything that is drawn
                    b.device = clean(&b.device, 40);
                    b.name = clean(&b.name, 40);
                    b.platform = clean(&b.platform, 12);
                    b.what = clean(&b.what, 20);
                    b.title = clean(&b.title, 60);
                    b.people = b.people.min(999);
                    if self.found.len() >= 64 && !self.found.iter().any(|f| f.beacon.id == b.id) {
                        continue;
                    }
                    let seen = mono_ms();
                    match self.found.iter_mut().find(|f| f.beacon.id == b.id) {
                        Some(f) => {
                            // Never let a relay-discovered entry overwrite a live direct LAN address for the same host.
                            if is_relay && !f.ip.starts_with("relay:") && seen - f.seen < 6_500.0 {
                                f.beacon.people = b.people;
                            } else {
                                f.beacon = b;
                                f.ip = ip;
                                f.seen = seen;
                            }
                        }
                        None => self.found.push(Found {
                            beacon: b,
                            ip,
                            seen,
                        }),
                    }
                }
                Event::Open {
                    conn,
                    sink,
                    addr,
                    incoming,
                } => {
                    if self.mode == Mode::Off
                        || self.conns.len() >= MAX_CONNS
                        || self.keys.is_none()
                    {
                        sink.close();
                        continue;
                    }
                    let now = mono_ms();
                    let crypt = if self.mode == Mode::Joining && !incoming {
                        let mut cr = secure::Crypt::new();
                        if let Some(d) = cr.start_initiator(self.keys.as_ref().unwrap()) {
                            sink.send(&Msg::Hs { d });
                        }
                        Arc::new(Mutex::new(cr))
                    } else {
                        Arc::new(Mutex::new(secure::Crypt::new()))
                    };
                    let sink: Box<dyn Sink> =
                        Box::new(secure::SecureSink::new(sink, crypt.clone()));
                    self.conns.insert(
                        conn,
                        Conn {
                            sink,
                            addr,
                            peer: None,
                            role: Role::Editor,
                            active: false,
                            digest: HashMap::new(),
                            backlog: VecDeque::new(),
                            crypt,
                            opened: now,
                            last_rx: now,
                            bucket: 600.0,
                            code: String::new(),
                            via_standby: false,
                            grant_token: None,
                        },
                    );
                }
                Event::Closed { conn, why } => self.on_closed(conn, &why),
                Event::Msg { conn, msg } => self.on_frame(conn, msg),
            }
        }
    }

    /// Closes a connection that misbehaved (or failed to authenticate) and runs the normal "closed" bookkeeping.
    fn drop_conn(&mut self, conn: ConnId, why: &str) {
        if let Some(c) = self.conns.get(&conn) {
            c.sink.close();
        }
        self.on_closed(conn, why);
    }

    /// Every frame from the network enters here: flood control, handshake, decryption. Nothing in clear text reaches `on_msg`.
    fn on_frame(&mut self, conn: ConnId, msg: Msg) {
        let now = mono_ms();
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        // token bucket: ~500 frames / s sustained, bursts of 800 (a 24 MiB message is ~700 frames, so sync traffic fits)
        c.bucket = (c.bucket + (now - c.last_rx) * 0.5).min(800.0);
        c.last_rx = now;
        if c.bucket < 1.0 {
            self.drop_conn(conn, "Too many messages");
            return;
        }
        c.bucket -= 1.0;
        match msg {
            Msg::Hs { d } => self.on_hs(conn, &d),
            Msg::Bin { d, more } => {
                let r = c
                    .crypt
                    .lock()
                    .map(|mut x| x.open(&d, more))
                    .unwrap_or(Err(()));
                match r {
                    Ok(Some(json)) => match serde_json::from_str::<Msg>(&json) {
                        Ok(Msg::Hs { .. }) | Ok(Msg::Enc { .. }) | Ok(Msg::Bin { .. }) | Err(_) => {
                            self.drop_conn(conn, "Protocol error")
                        }
                        Ok(m) => self.on_msg(conn, m),
                    },
                    Ok(None) => {}
                    Err(()) => self.drop_conn(conn, "Encryption error — the connection was closed"),
                }
            }
            // anything in clear text (an old version, or someone probing the port) is refused
            _ => self.drop_conn(conn, "This device runs an incompatible version"),
        }
    }

    fn on_hs(&mut self, conn: ConnId, d: &str) {
        let Some(keys) = self.keys.clone() else {
            return;
        };
        let joining = self.mode == Mode::Joining;
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        let (out, hash) = {
            let Ok(mut cr) = c.crypt.lock() else { return };
            // a joiner only ever initiates; an unsolicited first message from the other side is not a handshake we asked for
            if joining && !cr.started() {
                drop(cr);
                self.drop_conn(conn, "Unexpected handshake");
                return;
            }
            let out = cr.on_hs(&keys, d);
            (out, cr.hash.clone())
        };
        if out.failed {
            self.drop_conn(conn, "Secure connection failed");
            return;
        }
        if let Some(r) = out.reply {
            c.sink.raw(&Msg::Hs { d: r });
        }
        for f in &out.flushed {
            c.sink.raw(f);
        }
        if out.done {
            c.sink.flush_pending();
            c.code = secure::short_code(&hash);
            if joining {
                let hello = self.hello(&hash);
                if let Some(c) = self.conns.get(&conn) {
                    c.sink.send(&hello);
                }
            }
        }
    }

    /// Housekeeping: connections that never authenticate or never answer are closed, so half-open sockets cannot pile up.
    fn reap(&mut self, now: f64) {
        let mut dead: Vec<(ConnId, &'static str)> = vec![];
        for (id, c) in &self.conns {
            if c.active {
                continue;
            }
            let age = now - c.opened;
            if self.mode == Mode::Hosting {
                if c.peer.is_none() && age > 20_000.0 {
                    dead.push((*id, "No hello"));
                } else if age > 300_000.0 {
                    dead.push((*id, "Request expired"));
                }
            } else if age
                > if self.resume.is_some() {
                    25_000.0
                } else {
                    300_000.0
                }
            {
                dead.push((*id, "The host did not answer"));
            }
        }
        // an authenticated connection that went completely silent (heartbeats are sent every 3 s): unplugged / out of range / host asleep
        for (id, c) in &self.conns {
            if c.active && now - c.last_rx > 45_000.0 {
                dead.push((*id, "The connection timed out"));
            }
        }
        for (id, why) in dead {
            if self.requests.iter().any(|r| r.conn == id) {
                self.requests.retain(|r| r.conn != id);
            }
            if let Some(c) = self.conns.get(&id) {
                c.sink.send(&Msg::Denied { reason: why.into() });
            }
            self.drop_conn(id, why);
        }
        self.fails.retain(|_, (_, t)| now - *t < 600_000.0);
        self.check_quorum(now);
        self.enforce_grants();
        // joining a room that nobody answers (typo in the code, host gone)
        if self
            .resume
            .as_ref()
            .map_or(false, |r| r.next_at == f64::MAX)
            && self.conns.is_empty()
            && now - self.joining_since > 25_000.0
        {
            // a reconnect dial that nobody answered (relay: no Closed event ever comes)
            if !self.try_resume("Could not reach", false) {
                self.stop(false);
                self.last_error =
                    Some("Could not reconnect — the shared copy stays on this device".into());
                self.toast("Could not reconnect — the shared copy stays on this device");
            }
        } else if self.mode == Mode::Joining
            && self.resume.is_none()
            && self.conns.is_empty()
            && now - self.joining_since > 30_000.0
        {
            self.stop(false);
            let msg = "Nobody answered — check the room code and that the host is still sharing";
            self.last_error = Some(msg.into());
            self.toast(msg);
        }
    }

    fn on_closed(&mut self, conn: ConnId, why: &str) {
        let Some(c) = self.conns.remove(&conn) else {
            // a re-dial that never got as far as a connection (refused / unreachable): count the attempt now instead of waiting 25 s for `reap`
            if self.mode == Mode::Joining
                && self.conns.is_empty()
                && why.starts_with("Could not reach")
                && self
                    .resume
                    .as_ref()
                    .map_or(false, |r| r.next_at == f64::MAX)
                && !self.try_resume(why, false)
            {
                self.stop(false);
                self.last_error =
                    Some("Could not reconnect — the shared copy stays on this device".into());
                self.toast("Could not reconnect — the shared copy stays on this device");
            }
            return;
        };
        self.requests.retain(|r| r.conn != conn);
        match self.mode {
            Mode::Hosting => {
                if let Some(p) = c.peer {
                    if c.active {
                        self.roster.retain(|(x, _)| x.id != p.id);
                        self.presence.remove(&p.id);
                        self.broadcast_roster();
                        self.toast(format!("{} left", p.name));
                    }
                }
            }
            Mode::Joining | Mode::Joined => {
                if self.conns.is_empty() {
                    let was_joined = self.mode == Mode::Joined;
                    if self.try_resume(why, was_joined) {
                        return;
                    }
                    let msg = if was_joined {
                        "The host ended the session — the shared copy stays on this device"
                            .to_owned()
                    } else if why.is_empty() {
                        "Could not reach that device".to_owned()
                    } else {
                        why.to_owned()
                    };
                    self.stop(false);
                    self.last_error = Some(msg.clone());
                    self.toast(msg);
                }
            }
            Mode::Off => {}
        }
    }

    fn on_msg(&mut self, conn: ConnId, msg: Msg) {
        match msg {
            Msg::Hello { peer, auth, proto } => {
                if self.mode != Mode::Hosting {
                    return;
                }
                if self
                    .conns
                    .get(&conn)
                    .map_or(true, |c| c.peer.is_some() || c.active)
                {
                    return; // one hello per connection
                }
                if proto != PROTO {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied {
                            reason: "This device runs an incompatible version".into(),
                        });
                    }
                    self.drop_conn(conn, "incompatible");
                    return;
                }
                // the host chooses what everybody sees: bounded, printable, and never the host's own identity
                let peer = PeerInfo {
                    id: peer.id,
                    name: clean(&peer.name, 40),
                    device: clean(&peer.device, 40),
                    platform: clean(&peer.platform, 12),
                    color: (peer.id % 8) as u8,
                };
                let (addr, hash, remote, code) = match self.conns.get(&conn) {
                    Some(c) => {
                        let cr = c.crypt.lock();
                        (
                            c.addr.clone(),
                            cr.as_ref().map(|x| x.hash.clone()).unwrap_or_default(),
                            cr.as_ref().map(|x| x.remote.clone()).unwrap_or_default(),
                            c.code.clone(),
                        )
                    }
                    None => return,
                };
                if !remote.is_empty() && self.banned.contains(&remote) {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied {
                            reason: "You were removed from this session".into(),
                        });
                    }
                    self.drop_conn(conn, "removed");
                    return;
                }
                let now = mono_ms();
                let locked = self
                    .fails
                    .get(&peer_key(&addr))
                    .map_or(false, |(n, t)| *n >= 5 && now - *t < 300_000.0);
                let want = secure::proof("c", &self.token, &hash);
                let via_standby = !locked
                    && !auth.is_empty()
                    && self.standby_secret.as_ref().map_or(false, |sec| {
                        secure::ct_eq(
                            &auth,
                            &secure::proof("c", &standby_cap(sec, peer.id), &hash),
                        )
                    });
                if via_standby {
                    if let Some(c) = self.conns.get_mut(&conn) {
                        c.via_standby = true;
                    }
                }
                // a share link of its own (role, expiry and use count chosen by the host; revocable)
                let grant_idx = if via_standby || locked {
                    None
                } else {
                    self.grants.matching(&auth, &hash, now_ms())
                };
                let authed = via_standby
                    || grant_idx.is_some()
                    || (!locked && !auth.is_empty() && secure::ct_eq(&auth, &want));
                // a device this host approved before is recognised by its channel key (proven by the Noise handshake)
                let known =
                    !locked && !remote.is_empty() && self.trusted.contains_key(&hex(&remote));
                if !auth.is_empty() && !authed && !known {
                    if !locked {
                        let e = self.fails.entry(peer_key(&addr)).or_insert((0, now));
                        e.0 += 1;
                        e.1 = now;
                    }
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied {
                            reason: if locked {
                                "Too many wrong codes — try again in a few minutes".into()
                            } else {
                                "That share code is not valid".into()
                            },
                        });
                    }
                    self.drop_conn(conn, "bad code");
                    return;
                }
                if peer.id == 0 || peer.id == self.me.id {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied {
                            reason: "Device id conflict — restart Lumen on that device".into(),
                        });
                    }
                    self.drop_conn(conn, "id conflict");
                    return;
                }
                // a device that reconnects replaces its stale connection — but only when it proves who it is (token or same channel key);
                // otherwise anybody could kick a person out of the session just by claiming their id
                let existing: Vec<ConnId> = self
                    .conns
                    .iter()
                    .filter(|(k, c)| {
                        **k != conn && c.peer.as_ref().map_or(false, |p| p.id == peer.id)
                    })
                    .map(|(k, _)| *k)
                    .collect();
                for s in existing {
                    let same_key = !remote.is_empty()
                        && self
                            .conns
                            .get(&s)
                            .and_then(|c| c.crypt.lock().ok().map(|x| x.remote == remote))
                            .unwrap_or(false);
                    if authed || known || same_key {
                        if let Some(c) = self.conns.remove(&s) {
                            c.sink.close();
                        }
                        self.roster.retain(|(p, _)| p.id != peer.id);
                    } else {
                        if let Some(c) = self.conns.get(&conn) {
                            c.sink.send(&Msg::Denied {
                                reason: "A device with that id is already connected".into(),
                            });
                        }
                        self.drop_conn(conn, "id in use");
                        return;
                    }
                }
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.peer = Some(peer.clone());
                }
                // after a host hand-over the people of the old session come back with the new token and keep the role they had
                let inherited = if authed {
                    self.inherited.get(&peer.id).copied()
                } else {
                    None
                };
                if self.quorum.is_some() && !via_standby {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied { reason: "The session is re-forming after a host change — try again in a moment".into() });
                    }
                    self.drop_conn(conn, "re-forming");
                    return;
                }
                if let Some(gi) = grant_idx {
                    let (role, token) = (
                        self.grants.list[gi].role,
                        self.grants.list[gi].token.clone(),
                    );
                    self.grants.consume(gi);
                    self.save_grants();
                    if let Some(c) = self.conns.get_mut(&conn) {
                        c.grant_token = Some(token);
                    }
                    self.accept(conn, role);
                } else if known || (authed && (self.auto_admit || inherited.is_some())) {
                    let viewer = !remote.is_empty()
                        && (self.demoted.contains(&remote)
                            || self.trusted.get(&hex(&remote)).map_or(false, |t| t.1));
                    let role =
                        inherited.unwrap_or(if viewer { Role::Viewer } else { Role::Editor });
                    self.accept(conn, role);
                } else if self.requests.len() >= MAX_REQUESTS {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied {
                            reason: "The host has too many pending requests".into(),
                        });
                    }
                    self.drop_conn(conn, "busy");
                } else {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Pending);
                    }
                    self.requests.push(JoinRequest {
                        conn,
                        code,
                        peer,
                        addr,
                    });
                    self.ui.menu = true;
                }
            }
            Msg::Pending => {
                self.toast("Waiting for the host to accept…");
            }
            Msg::Welcome {
                peer,
                session,
                role,
                auth,
                spk,
            } => {
                if self.mode != Mode::Joining {
                    return;
                }
                // the host's signing key is pinned for the life of the session; a successor chosen by the signed plan starts a new pin
                if !spk.is_empty() {
                    if let Some(pin) = &self.host_pin {
                        if *pin != spk {
                            self.last_error =
                                Some("The host's identity changed — connection refused".into());
                            self.toast("The host's identity changed — connection refused");
                            self.drop_conn(conn, "host key changed");
                            return;
                        }
                    }
                    self.host_pin = Some(spk);
                }
                // the share token also authenticates the HOST: somebody who only knows the room code cannot pose as the host
                if !self.token.is_empty() {
                    let hash = self
                        .conns
                        .get(&conn)
                        .and_then(|c| c.crypt.lock().ok().map(|x| x.hash.clone()))
                        .unwrap_or_default();
                    if !secure::ct_eq(&auth, &secure::proof("s", &self.token, &hash)) {
                        self.last_error = Some("That device could not prove it holds the share code — connection refused".into());
                        self.toast("That device could not prove it holds the share code");
                        self.drop_conn(conn, "host not authentic");
                        return;
                    }
                }
                // a reconnect to the SAME session keeps the replica and the workspace: the digest exchange brings both sides up to date
                let resuming = self
                    .resume
                    .take()
                    .map_or(false, |r| r.session == session.id)
                    && self.rep.is_some()
                    && self.ready;
                self.mode = Mode::Joined;
                self.role = role;
                if resuming {
                    if let Some(rep) = self.rep.as_mut() {
                        rep.set_roots(session.roots.iter().copied());
                    }
                } else {
                    let mut rep = Replica::new(self.me.id, Scope::ALL);
                    rep.set_roots(session.roots.iter().copied());
                    self.rep = Some(rep);
                }
                ID_BASE.store((self.me.id as u64) << 40, Ordering::Relaxed);
                self.roster = vec![(peer.clone(), Role::Editor), (self.me.clone(), role)];
                self.fails.clear();
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.peer = Some(peer);
                    c.active = true;
                    c.role = Role::Editor;
                }
                if resuming {
                    self.send_digest(conn);
                    self.session = Some(session);
                    self.ui.dialog = false;
                    self.next_scan = 0.0;
                    self.toast("Reconnected");
                    return;
                }
                self.want_ws = Some(NewWs {
                    name: format!("Shared · {}", session.name),
                    share: session.share.clone(),
                    keep: session.keep,
                });
                self.changes.clear();
                self.track = false;
                self.reused = false;
                self.prompted = false;
                self.session = Some(session);
                self.ready = false;
                self.ui.dialog = false;
            }
            Msg::Denied { reason } => {
                self.stop(false);
                self.last_error = Some(reason.clone());
                self.toast(reason);
            }
            Msg::Roster { peers } => {
                if self.mode == Mode::Joined {
                    if let Some((_, r)) = peers.iter().find(|(p, _)| p.id == self.me.id) {
                        self.role = *r;
                    }
                    self.roster = peers;
                }
            }
            Msg::Standby {
                host,
                order,
                addrs,
                secret,
                cap,
                relay,
                spk,
                sig,
                seq,
                grants,
            } => self.on_standby(
                conn,
                Standby {
                    host,
                    order,
                    addrs,
                    secret,
                    cap,
                    relay,
                    spk,
                    sig,
                    seq,
                    grants,
                },
            ),
            Msg::StandbyAddr { addrs } => self.on_standby_addr(conn, addrs),
            Msg::Baton { .. } => self.on_baton(conn),
            Msg::BatonAck {
                addrs,
                relay,
                token,
            } => self.on_baton_ack(conn, addrs, relay, token),
            Msg::Moved {
                addrs,
                relay,
                token,
            } => self.on_moved(conn, addrs, relay, token),
            Msg::Bye => self.on_closed(conn, "The other device left"),
            other => {
                if self.conns.get(&conn).map_or(false, |c| c.active) {
                    self.inq.push_back((conn, other));
                }
            }
        }
    }

    /// Messages that need a bound workspace.
    fn handle_ready(&mut self, ws: &mut crate::model::Workspace, conn: ConnId, msg: Msg) {
        match msg {
            Msg::Digest { items, last } => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    if c.digest.len() + items.len() > 1_000_000 {
                        self.drop_conn(conn, "Digest too large");
                        return;
                    }
                    c.digest
                        .extend(items.into_iter().map(|(k, h, x)| (k, (h, x))));
                }
                if last {
                    self.big_announce(ws, conn);
                    // everything the other side lacks or has older, pages first; queued and sent a few messages per frame
                    let remote = self
                        .conns
                        .get_mut(&conn)
                        .map(|c| std::mem::take(&mut c.digest))
                        .unwrap_or_default();
                    let ops = self
                        .rep
                        .as_ref()
                        .map(|r| r.diff(ws, &remote))
                        .unwrap_or_default();
                    self.stats.ops_out += ops.len() as u64;
                    if let Some(c) = self.conns.get_mut(&conn) {
                        for part in chunked(ops, 120) {
                            c.backlog.push_back(Msg::Ops { ops: part });
                        }
                    }
                }
            }
            Msg::BigDigest { table, items, last } => {
                self.big_on_digest(ws, conn, table, items, last)
            }
            Msg::BigGet {
                table,
                buckets,
                have,
            } => self.big_on_get(ws, conn, table, buckets, have),
            Msg::BigChunk {
                table,
                b,
                rows,
                dead,
                cells,
            } => self.big_on_chunk(ws, conn, table, b, rows, dead, cells),
            Msg::Ops { ops } => {
                let role = self.conns.get(&conn).map(|c| c.role);
                let from_viewer =
                    self.mode == Mode::Hosting && role.map_or(true, |r| r == Role::Viewer);
                let suggest = self.mode == Mode::Hosting && role == Some(Role::Suggester);
                if ops.len() > 1000 || self.ops_in.len() + ops.len() > MAX_OPS_QUEUE {
                    self.drop_conn(conn, "Too many operations");
                    return;
                }
                if suggest {
                    // comments and PDF marks take effect at once; everything else waits for the host's decision
                    for o in ops {
                        if matches!(o.key, Key::Comment(..) | Key::Annot(..)) {
                            self.ops_in.push_back((conn, o));
                        } else if self.held_in.len() < 2000 {
                            self.held_in.push_back((conn, o));
                        }
                    }
                } else if !from_viewer {
                    self.ops_in.extend(ops.into_iter().map(|o| (conn, o)));
                }
            }
            Msg::Presence(mut p) => {
                // the host knows who is on the other end of the connection: a peer cannot move somebody else's cursor
                if self.mode == Mode::Hosting {
                    match self
                        .conns
                        .get(&conn)
                        .and_then(|c| c.peer.as_ref().map(|x| x.id))
                    {
                        Some(id) => p.peer = id,
                        None => return,
                    }
                } else if p.peer == self.me.id {
                    return;
                }
                if p.cur.map_or(false, |c| {
                    !c[0].is_finite() || !c[1].is_finite() || c[0].abs() > 1e8 || c[1].abs() > 1e8
                }) {
                    p.cur = None;
                }
                if let Some(d) = p.draft.as_mut() {
                    d.pts.truncate(128);
                    if d.tool > 3
                        || d.pts.iter().any(|q| {
                            !q[0].is_finite()
                                || !q[1].is_finite()
                                || q[0].abs() > 4.0
                                || q[1].abs() > 4.0
                        })
                    {
                        p.draft = None;
                    }
                }
                if p.caret
                    .map_or(false, |(a, b)| a > 10_000_000 || b > 10_000_000)
                {
                    p.caret = None;
                }
                if !self.presence.contains_key(&p.peer) && self.presence.len() >= 64 {
                    return;
                }
                let id = p.peer;
                if self.mode == Mode::Hosting {
                    let m = Msg::Presence(p.clone());
                    for (cid, c) in &self.conns {
                        if *cid != conn && c.active {
                            c.sink.send(&m);
                        }
                    }
                }
                self.presence.insert(id, (p, mono_ms()));
            }
            Msg::MediaReq { id, off } => {
                if self.media_reqs.len() < 64 {
                    self.media_reqs.push((conn, id, off));
                }
            }
            Msg::MediaData {
                id,
                off,
                total,
                name,
                mime,
                data,
            } => {
                // only what this device asked for, bounded in size, and verified against its content id before it is used
                if !self.requested.contains_key(&id)
                    || total > MAX_MEDIA
                    || data.len() > 700_000
                    || (self.partial.len() >= 8 && !self.partial.contains_key(&id))
                {
                    return;
                }
                let Some(bytes) = crate::media::b64_decode(&data) else {
                    return;
                };
                let (name, mime) = (clean(&name, 200), clean(&mime, 100));
                let e = self
                    .partial
                    .entry(id)
                    .or_insert_with(|| (Vec::new(), name, mime));
                if e.0.len() as u64 != off || off + bytes.len() as u64 > total {
                    return;
                }
                e.0.extend_from_slice(&bytes);
                let got = e.0.len() as u64;
                if got >= total {
                    if let Some((b, n, m)) = self.partial.remove(&id) {
                        if crate::media::fnv(&b) == id {
                            self.media_done.push((id, n, m, b));
                        } else {
                            self.requested.remove(&id);
                        }
                    }
                } else {
                    self.requested.insert(id, mono_ms());
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::MediaReq { id, off: got });
                    }
                }
            }
            Msg::MediaMiss { id } => {
                self.requested.remove(&id);
            }
            _ => {}
        }
    }
}

// ───────────── shared view state (read by the canvas painter without a reference to the session) ─────────────

#[derive(Default)]
struct ViewState {
    remotes: Vec<(PeerInfo, Presence)>,
    show: bool,
    cursor: Option<(u64, [f32; 2], f64)>,
    launch: Option<String>,
    /// Local text caret: (block, selection start, caret, time).
    caret: Option<(u64, u32, u32, f64)>,
    caret_len: u32,
    typing_at: f64,
    draft: Option<(LiveDraft, f64)>,
    /// Remote carets followed through other people's edits: (peer, block) → [`Track`].
    tracks: HashMap<(u32, u64), Track>,
    /// The person being followed and whether the view also chases their caret / pointer (not only their page).
    follow: Option<u32>,
    follow_cursor: bool,
    /// Last followed caret the view was scrolled to: (peer, block, selection start, caret) — one scroll per move, never per frame.
    follow_seen: Option<(u32, u64, u32, u32)>,
}

/// A remote caret as last drawn: the text it was drawn against and where it was mapped to, so that an edit by anybody (the remote
/// person, me, a third device) moves the caret with the text instead of leaving it at the old offset.
struct Track {
    raw: (u32, u32),
    len: u32,
    text: String,
    ab: (u32, u32),
}

thread_local! {
    static VIEW: std::cell::RefCell<ViewState> = std::cell::RefCell::new(ViewState::default());
}

/// Called by the app after each frame's `poll`: what the painters need.
pub fn publish(c: &Collab) {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        // following somebody always shows their cursor, whatever the "show cursors" switch says
        v.show = c.active() && (c.show_cursors || c.ui.follow.is_some());
        v.follow = c.ui.follow;
        v.follow_cursor = !c.ui.follow_cursor_off;
        if !v.show {
            v.tracks.clear();
        }
        v.remotes = if v.show {
            c.roster
                .iter()
                .filter(|(p, _)| p.id != c.me.id)
                .filter_map(|(p, _)| c.presence.get(&p.id).map(|x| (p.clone(), x.0.clone())))
                .collect()
        } else {
            vec![]
        };
    });
}

/// The text editor reports the caret of the focused block (characters of the plain text) every frame.
pub fn set_caret(block: u64, from: u32, to: u32, len: u32) {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        v.caret = Some((block, from, to, mono_ms()));
        v.caret_len = len;
    });
}

/// The editor of `block` is not focused (any more): its caret is no longer mine.
pub fn clear_caret(block: u64) {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        if matches!(v.caret, Some((b, ..)) if b == block) {
            v.caret = None;
            v.caret_len = 0;
        }
    });
}

fn caret_len() -> u32 {
    VIEW.with(|v| v.borrow().caret_len)
}

/// The text of a block was edited by this user just now ("typing…" badge for the others; clears itself after two seconds).
pub fn set_typing(ctx: &egui::Context) {
    VIEW.with(|v| v.borrow_mut().typing_at = mono_ms());
    ctx.request_repaint_after(std::time::Duration::from_millis(2100));
}

fn typing_active() -> bool {
    VIEW.with(|v| mono_ms() - v.borrow().typing_at < 2000.0)
}

fn caret_state() -> Option<(u64, u32, u32)> {
    VIEW.with(|v| match v.borrow().caret {
        // sticky: an idle, focused editor does not repaint, so the caret must outlive the frames; `clear_caret` ends it when focus leaves
        Some((b, f, t, at)) if mono_ms() - at < 45_000.0 => Some((b, f, t)),
        _ => None,
    })
}

/// The PDF reader reports the mark being drawn (`None` when the stroke ended).
pub fn set_draft(d: Option<LiveDraft>) {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        match d {
            Some(d) => v.draft = Some((d, mono_ms())),
            None => v.draft = None,
        }
    });
}

fn draft_state() -> Option<LiveDraft> {
    VIEW.with(|v| match &v.borrow().draft {
        Some((d, at)) if mono_ms() - at < 2500.0 => Some(d.clone()),
        _ => None,
    })
}

/// Other people's text carets inside `block`: (name, colour index, selection start, caret).
pub fn remote_carets(block: u64) -> Vec<(String, u8, u32, u32, u32, bool, u32)> {
    VIEW.with(|v| {
        let v = v.borrow();
        if !v.show {
            return vec![];
        }
        v.remotes
            .iter()
            .filter(|(_, p)| p.block == Some(block))
            .filter_map(|(i, p)| {
                p.caret
                    .map(|(a, b)| (i.name.clone(), i.color, a, b, p.len, p.typing, i.id))
            })
            .collect()
    })
}

/// Other people's PDF marks in progress on `doc` page `page`: (colour index of the person, draft).
pub fn remote_drafts(doc: u64, page: u32) -> Vec<(u8, LiveDraft)> {
    VIEW.with(|v| {
        let v = v.borrow();
        if !v.show {
            return vec![];
        }
        v.remotes
            .iter()
            .filter_map(|(i, p)| {
                p.draft
                    .as_ref()
                    .filter(|d| d.doc == doc && d.page == page)
                    .map(|d| (i.color, d.clone()))
            })
            .collect()
    })
}

/// Maps a character index of `old` to the matching index of `new` through the one contiguous edit that turns `old` into `new`
/// (common prefix / common suffix). A point before the edit stays, a point after it moves by the length difference, a point inside the
/// replaced range keeps its distance from the start of the edit, clamped to the new text.
pub fn map_index(old: &[char], new: &[char], idx: usize) -> usize {
    let max = old.len().min(new.len());
    let pre = old.iter().zip(new).take_while(|(x, y)| x == y).count();
    let suf = old[pre..]
        .iter()
        .rev()
        .zip(new[pre..].iter().rev())
        .take(max - pre)
        .take_while(|(x, y)| x == y)
        .count();
    let idx = idx.min(old.len());
    if idx <= pre {
        idx
    } else if idx >= old.len() - suf {
        new.len() - (old.len() - idx)
    } else {
        pre + (idx - pre).min(new.len() - suf - pre)
    }
}

/// Follows one remote caret through the edits made to the block since it was drawn the last time. `(a, b)` is what the person sent
/// (selection start, caret) and `len` the length of their text then; `text` / `n` are our text now.
fn track_caret(peer: u32, block: u64, raw: (u32, u32), len: u32, text: &str, n: u32) -> (u32, u32) {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        if v.tracks.len() > 256 {
            v.tracks.clear();
        }
        let t = v.tracks.entry((peer, block)).or_insert(Track {
            raw: (u32::MAX, u32::MAX),
            len: 0,
            text: String::new(),
            ab: (0, 0),
        });
        if t.raw != raw || t.len != len {
            // a fresh report: the caret was taken against a text of `len` characters; ours has `n` (an edit is still on its way): the
            // missing / extra characters lie before the caret when somebody types or deletes at it, so shift instead of jumping
            let adj = if len != 0 && len != n {
                (n as i64 - len as i64).clamp(-64, 64)
            } else {
                0
            };
            let shift = |x: u32| (x as i64 + adj).clamp(0, n as i64) as u32;
            *t = Track {
                raw,
                len,
                text: text.to_owned(),
                ab: (shift(raw.0), shift(raw.1)),
            };
        } else if t.text != text {
            let (old, new): (Vec<char>, Vec<char>) =
                (t.text.chars().collect(), text.chars().collect());
            t.ab = (
                map_index(&old, &new, t.ab.0 as usize) as u32,
                map_index(&old, &new, t.ab.1 as usize) as u32,
            );
            t.text.clear();
            t.text.push_str(text);
        }
        (t.ab.0.min(n), t.ab.1.min(n))
    })
}

/// The rows of a galley that a selection `[lo, hi)` (characters) touches, as `(row, first column, last column)`; the last column is
/// `None` when the selection runs on to the end of the row (and past its line break). `rows` = `(characters without the break, ends with a
/// break)` per row. Pure, so it is tested without a font.
pub fn selection_spans(
    rows: &[(usize, bool)],
    lo: usize,
    hi: usize,
) -> Vec<(usize, usize, Option<usize>)> {
    let mut out = vec![];
    let mut start = 0usize;
    for (i, &(n, nl)) in rows.iter().enumerate() {
        let end = start + n + nl as usize;
        let (a, b) = (lo.max(start), hi.min(end));
        if a < b || (a == b && lo < start && hi > start && n == 0) {
            let first = a - start;
            let last = if b > start + n || (nl && b == end) {
                None
            } else {
                Some(b - start)
            };
            out.push((i, first.min(n), last));
        }
        start = end;
        if start >= hi {
            break;
        }
    }
    out
}

/// Paints other people's carets (a thin coloured bar, selection tint — across every row of a multi-line selection — and a name flag) over
/// the text of `block`. The person being followed also pulls the surrounding scroll area to their caret.
pub fn paint_carets(ui: &egui::Ui, block: u64, galley: &egui::Galley, pos: egui::Pos2) {
    let list = remote_carets(block);
    if list.is_empty() {
        return;
    }
    let text = galley.text();
    let n = text.chars().count() as u32;
    let p = ui.painter();
    let (follow, follow_cursor, follow_seen) = VIEW.with(|v| {
        let v = v.borrow();
        (v.follow, v.follow_cursor, v.follow_seen)
    });
    let rows: Vec<(usize, bool)> = galley
        .rows
        .iter()
        .map(|r| (r.char_count_excluding_newline(), r.ends_with_newline))
        .collect();
    for (name, color, a, b, len, typing, peer) in list {
        let col = ui::COLORS[(color % 8) as usize];
        let (a, b) = track_caret(peer, block, (a, b), len, text, n);
        let at = |i: u32| galley.pos_from_ccursor(egui::text::CCursor::new(i as usize));
        let head = at(b);
        if a != b {
            let (lo, hi) = (a.min(b) as usize, a.max(b) as usize);
            for (ri, c0, c1) in selection_spans(&rows, lo, hi) {
                let Some(row) = galley.rows.get(ri) else {
                    continue;
                };
                let x0 = row.x_offset(c0);
                let x1 = match c1 {
                    // a selected line break is drawn as a short extra bit so an empty selected row is visible
                    None => row.rect.right() + if row.ends_with_newline { 5.0 } else { 0.0 },
                    Some(c) => row.x_offset(c),
                };
                let (x0, x1) = (x0.min(x1), x0.max(x1).max(x0 + 2.0));
                p.rect_filled(
                    egui::Rect::from_min_max(
                        pos + egui::vec2(x0, row.rect.top()),
                        pos + egui::vec2(x1, row.rect.bottom()),
                    ),
                    2.0,
                    ui::with_alpha_pub(col, 0.24),
                );
            }
        }
        let x = pos.x + head.left();
        let (y0, y1) = (pos.y + head.top(), pos.y + head.bottom());
        // a soft halo under the bar keeps it visible on any page colour
        p.line_segment(
            [egui::pos2(x, y0), egui::pos2(x, y1)],
            egui::Stroke::new(4.0_f32, ui::with_alpha_pub(col, 0.18)),
        );
        p.line_segment(
            [egui::pos2(x, y0), egui::pos2(x, y1)],
            egui::Stroke::new(2.0_f32, col),
        );
        let label = p.layout_no_wrap(
            if typing {
                format!("{} · typing…", ui::first_name(&name))
            } else {
                ui::first_name(&name)
            },
            egui::FontId::proportional(10.5),
            egui::Color32::WHITE,
        );
        // the flag sits above the caret, or under it on the first line of a block at the top of the screen
        let above = y0 - label.size().y - 3.0 >= ui.clip_rect().top();
        let fy = if above {
            y0 - label.size().y - 3.0
        } else {
            y1 + 1.0
        };
        let flag =
            egui::Rect::from_min_size(egui::pos2(x - 1.0, fy), label.size() + egui::vec2(8.0, 3.0));
        p.rect_filled(flag, 3.0, col);
        p.galley(flag.min + egui::vec2(4.0, 1.5), label, egui::Color32::WHITE);
        if follow == Some(peer) && follow_cursor && follow_seen != Some((peer, block, a, b)) {
            VIEW.with(|v| v.borrow_mut().follow_seen = Some((peer, block, a, b)));
            ui.scroll_to_rect(
                egui::Rect::from_min_max(
                    egui::pos2(x - 8.0, y0 - 56.0),
                    egui::pos2(x + 8.0, y1 + 96.0),
                ),
                None,
            );
            ui.ctx().request_repaint();
        }
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(400));
}

/// While following with "follow cursor" on: the pointer of the followed person on canvas `page` (world coordinates).
pub fn follow_pointer(page: u64) -> Option<[f32; 2]> {
    VIEW.with(|v| {
        let v = v.borrow();
        if !v.follow_cursor {
            return None;
        }
        let f = v.follow?;
        v.remotes
            .iter()
            .find(|(i, p)| i.id == f && p.page == Some(page))
            .and_then(|(_, p)| p.cur)
    })
}

/// The canvas reports the pointer in world coordinates every frame while it is over the board.
pub fn canvas_cursor(page: u64, world: [f32; 2]) {
    VIEW.with(|v| v.borrow_mut().cursor = Some((page, world, mono_ms())));
}

/// (page of the last pointer report, pointer) — `None` once the pointer has been still / away for a second.
pub fn view_state() -> (Option<u64>, Option<[f32; 2]>) {
    VIEW.with(|v| match v.borrow().cursor {
        Some((p, w, t)) if mono_ms() - t < 1000.0 => (Some(p), Some(w)),
        _ => (None, None),
    })
}

/// Other people's pointers on a canvas page.
pub fn paint_canvas_cursors(
    p: &egui::Painter,
    pal: &crate::theme::Palette,
    page: u64,
    w2s: &dyn Fn(egui::Pos2) -> egui::Pos2,
) {
    VIEW.with(|v| {
        let v = v.borrow();
        if !v.show {
            return;
        }
        let list: Vec<(&PeerInfo, &Presence)> = v
            .remotes
            .iter()
            .filter(|(_, pr)| pr.page == Some(page))
            .map(|(a, b)| (a, b))
            .collect();
        ui::paint_cursor_list(p, pal, &list, w2s);
    });
}

/// A link that should start a join (`?join=ROOM&t=TOKEN` on the web). Consumed once.
pub fn take_launch_link() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        thread_local! { static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
        if !DONE.with(|d| d.replace(true)) {
            if let Some(w) = web_sys::window() {
                if let Ok(s) = w.location().search() {
                    if (s.contains("iroh=") || s.contains("join=")) {
                        return Some(s);
                    }
                }
            }
        }
    }
    if let Ok(mut l) = LAUNCH.lock() {
        if let Some(s) = l.take() {
            return Some(s);
        }
    }
    VIEW.with(|v| v.borrow_mut().launch.take())
}

/// A join link that arrived on another thread (Android intent): picked up by [`take_launch_link`] on the UI thread.
static LAUNCH: Mutex<Option<String>> = Mutex::new(None);

pub fn push_launch_link(s: String) {
    if let Ok(mut l) = LAUNCH.lock() {
        *l = Some(s);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::model::Workspace;
    use std::time::{Duration, Instant};

    fn step(h: &mut Collab, hw: &mut Workspace, j: &mut Collab, jw: &mut Workspace) {
        let hc = hw.current;
        h.poll(hw, 1, hc, &|_| true);
        if j.take_new_workspace().is_some() {
            j.bind(2);
        }
        let jc = jw.current;
        j.poll(jw, 2, jc, &|_| true);
        std::thread::sleep(Duration::from_millis(4));
    }

    fn until(
        h: &mut Collab,
        hw: &mut Workspace,
        j: &mut Collab,
        jw: &mut Workspace,
        secs: u64,
        f: impl Fn(&Workspace, &Workspace) -> bool,
    ) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            step(h, hw, j, jw);
            if f(hw, jw) {
                return true;
            }
        }
        false
    }

    /// One frame of three devices: the host works on workspace 1, the two others on their "Shared" workspace 2.
    fn round3(
        h: &mut Collab,
        hw: &mut Workspace,
        a: &mut Collab,
        aw: &mut Workspace,
        b: &mut Collab,
        bw: &mut Workspace,
    ) {
        let hc = hw.current;
        h.poll(hw, 1, hc, &|_| true);
        for (c, w) in [(a, aw), (b, bw)] {
            if c.take_new_workspace().is_some() {
                c.bind(2);
            }
            let cur = w.current;
            c.poll(w, 2, cur, &|_| true);
        }
        std::thread::sleep(Duration::from_millis(4));
    }

    #[test]
    fn the_editors_elect_a_new_host_when_the_host_vanishes() {
        let (mut host, mut hw, mut a, mut aw, port) = pair();
        let mut b = Collab::new(&egui::Context::default());
        let mut bw = Workspace::blank("b", false, 0);
        bw.pages.clear();
        bw.current = None;
        let token = host.token.clone();
        a.join_iroh(&port, &token);
        b.join_iroh(&port, &token);
        let blocks = |w: &Workspace| w.pages.iter().map(|p| p.blocks.len()).sum::<usize>();
        let t0 = Instant::now();
        let mut synced = false;
        while t0.elapsed().as_secs() < 12 && !synced {
            round3(&mut host, &mut hw, &mut a, &mut aw, &mut b, &mut bw);
            synced = !aw.pages.is_empty()
                && !bw.pages.is_empty()
                && blocks(&hw) == blocks(&aw)
                && blocks(&hw) == blocks(&bw)
                && host.roster.len() == 3
                && a.standby.is_some()
                && b.standby.is_some();
        }
        assert!(
            synced,
            "three devices did not sync / receive the fail-over plan"
        );
        assert!(
            a.standby.as_ref().unwrap().secret.len() >= 16
                && a.standby.as_ref().unwrap().cap != b.standby.as_ref().unwrap().cap,
            "every device gets its own cap"
        );
        // the devices would report their addresses themselves (their LAN address is not known in a sandbox): tell the host where they listen
        for c in [&a, &b] {
            let sa = std::net::SocketAddr::from(([127, 0, 0, 1], standby_port(c.me.id)));
            let tkt = iroh_transport::standby_ticket(c.iroh_seed(), &[sa]);
            host.peer_addrs.insert(c.me.id, vec![format!("iroh:{tkt}")]);
        }
        host.push_standby();
        let t0 = Instant::now();
        while t0.elapsed().as_millis() < 300 {
            round3(&mut host, &mut hw, &mut a, &mut aw, &mut b, &mut bw);
        }
        // the host's process dies: sockets vanish, nobody says goodbye
        host.iroh.stop();
        for c in host.conns.values() {
            c.sink.close();
        }
        host.conns.clear();
        std::thread::sleep(Duration::from_millis(600)); // the accept thread notices the stop flag (a dead process has no listener at all)
        let (succ_is_a, pid) = (
            a.me.id < b.me.id,
            hw.pages
                .iter()
                .find(|p| p.blocks.len() > 1)
                .map(|p| p.id)
                .unwrap(),
        );
        let step2 = |a: &mut Collab, aw: &mut Workspace, b: &mut Collab, bw: &mut Workspace| {
            for (c, w) in [(&mut *a, &mut *aw), (&mut *b, &mut *bw)] {
                let cur = w.current;
                c.poll(w, 2, cur, &|_| true);
            }
            std::thread::sleep(Duration::from_millis(8));
        };
        let t0 = Instant::now();
        let mut done = false;
        while t0.elapsed().as_secs() < 45 && !done {
            step2(&mut a, &mut aw, &mut b, &mut bw);
            let (s, f) = if succ_is_a { (&a, &b) } else { (&b, &a) };
            done = s.mode == Mode::Hosting
                && f.mode == Mode::Joined
                && s.roster.len() >= 2
                && s.conns.values().filter(|c| c.active).count() == 1;
        }
        assert!(
            done,
            "no new host: a {:?} b {:?} (a is successor: {succ_is_a})",
            a.mode, b.mode
        );
        // the follower's edit reaches the new host, and the new host's edit reaches the follower (the old host is gone)
        let (sw, fw, s, f) = if succ_is_a {
            (&mut aw, &mut bw, &mut a, &mut b)
        } else {
            (&mut bw, &mut aw, &mut b, &mut a)
        };
        fw.page_mut(pid).unwrap().blocks[0].text = "typed after the fail-over".into();
        f.poke();
        let t0 = Instant::now();
        let mut ok = false;
        while t0.elapsed().as_secs() < 10 && !ok {
            for (c, w) in [(&mut *s, &mut *sw), (&mut *f, &mut *fw)] {
                let cur = w.current;
                c.poll(w, 2, cur, &|_| true);
            }
            std::thread::sleep(Duration::from_millis(8));
            ok = sw
                .page(pid)
                .map_or(false, |p| p.blocks[0].text == "typed after the fail-over");
        }
        assert!(ok, "edit after the fail-over did not arrive");
        // the dead host cannot walk back in with the old share code (the new host has a new one)
        assert_ne!(s.token, token);
        for c in [&mut a, &mut b] {
            c.stop(true);
        }
    }

    #[test]
    fn the_host_hands_the_session_over_and_everybody_follows() {
        let (mut host, mut hw, mut a, mut aw, port) = pair();
        let mut b = Collab::new(&egui::Context::default());
        let mut bw = Workspace::blank("b", false, 0);
        bw.pages.clear();
        bw.current = None;
        let token = host.token.clone();
        a.join_iroh(&port, &token);
        b.join_iroh(&port, &token);
        let blocks = |w: &Workspace| w.pages.iter().map(|p| p.blocks.len()).sum::<usize>();
        let t0 = std::time::Instant::now();
        let mut synced = false;
        while t0.elapsed().as_secs() < 12 && !synced {
            round3(&mut host, &mut hw, &mut a, &mut aw, &mut b, &mut bw);
            synced = !aw.pages.is_empty()
                && !bw.pages.is_empty()
                && blocks(&hw) == blocks(&aw)
                && blocks(&hw) == blocks(&bw)
                && host.roster.len() == 3;
        }
        assert!(synced, "three devices did not sync first");
        let a_id = a.me.id;
        assert!(host.hand_over(Some(a_id), false));
        let pid = hw
            .pages
            .iter()
            .find(|p| p.blocks.len() > 1)
            .map(|p| p.id)
            .unwrap();
        let t0 = std::time::Instant::now();
        let mut done = false;
        while t0.elapsed().as_secs() < 20 && !done {
            round3(&mut host, &mut hw, &mut a, &mut aw, &mut b, &mut bw);
            done = a.mode == Mode::Hosting
                && host.mode == Mode::Joined
                && b.mode == Mode::Joined
                && a.roster.len() == 3
                && a.token != token;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            done,
            "modes: host {:?} a {:?} b {:?}, roster at a {}",
            host.mode,
            a.mode,
            b.mode,
            a.roster.len()
        );
        // an edit made by the participant who did not move reaches the new host AND the old one through it
        bw.page_mut(pid).unwrap().blocks[0].text = "typed after the hand-over".into();
        b.poke();
        let t0 = std::time::Instant::now();
        let mut ok = false;
        while t0.elapsed().as_secs() < 10 && !ok {
            round3(&mut host, &mut hw, &mut a, &mut aw, &mut b, &mut bw);
            ok = [&hw, &aw].iter().all(|w| {
                w.page(pid)
                    .map_or(false, |p| p.blocks[0].text == "typed after the hand-over")
            });
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ok, "edit after the hand-over did not reach everybody");
        for c in [&mut host, &mut a, &mut b] {
            c.stop(true);
        }
    }

    #[test]
    fn hand_over_targets_are_validated() {
        let ok = |a: &[&str], r: Option<(&str, &str)>, t: &str| {
            valid_target(
                &a.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
                &r.map(|(b, r)| (b.to_string(), r.to_string())),
                t,
            )
        };
        assert!(ok(&["192.168.1.5:7878"], None, "abcd1234"));
        assert!(ok(
            &[],
            Some(("https://site.example", "ROOM123456")),
            "abcd1234"
        ));
        assert!(!ok(&[], None, "abcd1234"), "nothing to dial");
        assert!(!ok(&["a b"], None, "abcd1234"));
        assert!(!ok(&["1.2.3.4:5"], None, "x"));
        assert!(!ok(&[], Some(("ftp://x", "ROOM123456")), "abcd1234"));
        assert!(!ok(&[], Some(("https://x", "RO OM")), "abcd1234"));
    }

    #[test]
    fn carets_follow_edits_through_the_common_prefix_and_suffix() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        // typing before the caret pushes it right, typing after leaves it
        assert_eq!(map_index(&c("hello world"), &c("hello big world"), 8), 12);
        assert_eq!(map_index(&c("hello world"), &c("hello world!!"), 8), 8);
        // a deletion before the caret pulls it left; a caret inside the deleted range lands at the start of the edit
        assert_eq!(map_index(&c("hello big world"), &c("hello world"), 12), 8);
        assert_eq!(map_index(&c("hello big world"), &c("hello world"), 8), 6);
        // identical text, empty text, index beyond the end
        assert_eq!(map_index(&c("abc"), &c("abc"), 2), 2);
        assert_eq!(map_index(&c(""), &c("abc"), 0), 0);
        assert_eq!(map_index(&c("abc"), &c(""), 3), 0);
        assert_eq!(map_index(&c("abc"), &c("abcd"), 99), 3);
        // a caret exactly at the point of an insertion stays in front of it (the person's own next report moves it)
        assert_eq!(map_index(&c("aaaa"), &c("aaaaa"), 4), 4);
    }

    #[test]
    fn a_tracked_caret_moves_with_remote_text_and_resets_on_a_new_report() {
        assert_eq!(track_caret(7, 1, (5, 5), 11, "hello world", 11), (5, 5));
        // somebody typed at the start: same report, longer text ⇒ the caret moved with the text
        assert_eq!(track_caret(7, 1, (5, 5), 11, "oh hello world", 14), (8, 8));
        // a new report replaces the track
        assert_eq!(track_caret(7, 1, (2, 3), 14, "oh hello world", 14), (2, 3));
        // report taken against a longer text than ours (edit still on its way): shift by the difference
        assert_eq!(track_caret(8, 1, (10, 10), 14, "oh hello wor", 12), (8, 8));
    }

    #[test]
    fn two_devices_sync_over_a_real_socket() {
        let ctx = egui::Context::default();
        let mut host = Collab::new(&ctx);
        host.auto_admit = true;
        let mut join = Collab::new(&egui::Context::default());
        let mut hw = Workspace::seed();
        let mut jw = Workspace::blank("joiner", false, 0);
        jw.pages.clear();
        jw.current = None;
        host.start_host(&mut hw, 1, Scope::ALL, None);
        let port = wait_ticket(&host);
        assert!(!port.is_empty(), "host could not listen");
        join.join_iroh(&port, &host.token.clone());
        let blocks = |w: &Workspace| w.pages.iter().map(|p| p.blocks.len()).sum::<usize>();
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| a
                .pages
                .len()
                == b.pages.len()
                && blocks(a) == blocks(b)
                && !b.pages.is_empty()),
            "initial sync did not finish: {} vs {} pages",
            hw.pages.len(),
            jw.pages.len()
        );
        assert_eq!(host.roster.len(), 2);
        let pid = hw
            .pages
            .iter()
            .find(|p| p.blocks.len() > 1)
            .map(|p| p.id)
            .unwrap();
        hw.current = Some(pid);
        jw.current = Some(pid);
        // host edits → joiner sees it
        hw.page_mut(pid).unwrap().blocks[0].text = "edited on the host".into();
        host.poke();
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 6, |_, b| b
                .page(pid)
                .map_or(false, |p| p.blocks[0].text == "edited on the host")),
            "host edit did not arrive"
        );
        // joiner edits another block → host sees it, and a new block appears on both
        jw.page_mut(pid).unwrap().blocks[1].text = "edited on the phone".into();
        let nb = jw.page(pid).unwrap().blocks[1].clone();
        let mut extra = nb.clone();
        extra.id = jw.id();
        extra.text = "added on the phone".into();
        jw.page_mut(pid).unwrap().blocks.push(extra);
        join.poke();
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 6, |a, _| a
                .page(pid)
                .map_or(false, |p| p.blocks[1].text == "edited on the phone"
                    && p.blocks
                        .iter()
                        .any(|b| b.text == "added on the phone"))),
            "joiner edit did not arrive"
        );
        host.stop(true);
        join.stop(true);
    }

    fn wait_ticket(host: &Collab) -> String {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(6) {
            if let Some(t) = host.iroh.ticket() {
                return t;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        panic!("host iroh ticket never became ready");
    }

    fn pair() -> (Collab, Workspace, Collab, Workspace, String) {
        let mut host = Collab::new(&egui::Context::default());
        host.auto_admit = true;
        let join = Collab::new(&egui::Context::default());
        let mut hw = Workspace::seed();
        let mut jw = Workspace::blank("joiner", false, 0);
        jw.pages.clear();
        jw.current = None;
        host.start_host(&mut hw, 1, Scope::ALL, None);
        let ticket = wait_ticket(&host);
        (host, hw, join, jw, ticket)
    }

    #[test]
    fn a_big_table_is_shared_edited_and_deleted_live() {
        use crate::bigtable as bt;
        std::env::set_var(
            "LUMEN_TABLES_DIR",
            std::env::temp_dir().join(format!("lumen-bigtest-{}", std::process::id())),
        );
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        let tid = bt::create_sample(&mut hw, "Big", 6000, None).expect("sample table");
        let rows = |w: &Workspace| bt::store(w, tid).map(|s| s.lock().unwrap().len().unwrap());
        join.join_iroh(&port, &host.token.clone());
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 25, |_, b| rows(b)
                == Some(6000)),
            "the joiner never received the rows: {:?}",
            rows(&jw)
        );
        assert_ne!(
            hw.bigs[&tid].dir, jw.bigs[&tid].dir,
            "the joiner has its own store"
        );
        // host edits one row → the joiner sees it
        let hs = bt::store(&hw, tid).unwrap();
        {
            let mut g = hs.lock().unwrap();
            let mut r = g.get(77).unwrap().unwrap();
            r.title = "edited on the host".into();
            r.updated = now_ms() + 10;
            g.upsert(r).unwrap();
        }
        bt::note_write(&hw.bigs[&tid].dir, 77);
        bt::bump_version(&hw.bigs[&tid].dir);
        let title = |w: &Workspace, id: u64| {
            bt::store(w, tid)
                .and_then(|s| s.lock().unwrap().get(id).unwrap())
                .map(|r| r.title)
        };
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |_, b| title(
                b, 77
            )
            .as_deref()
                == Some("edited on the host")),
            "live edit did not arrive"
        );
        // joiner deletes a row and adds one → the host follows
        let js = bt::store(&jw, tid).unwrap();
        let jdir = jw.bigs[&tid].dir.clone();
        let new_id = {
            let mut g = js.lock().unwrap();
            g.delete(10).unwrap();
            let id = bt::new_row_id(&mut g);
            g.upsert(lumen_store::Row {
                id,
                title: "from the phone".into(),
                updated: now_ms() + 20,
                props: vec![],
            })
            .unwrap();
            id
        };
        bt::note_delete(&jdir, 10);
        bt::note_write(&jdir, new_id);
        assert!(
            new_id > 1 << 40,
            "rows created in a session come from the device's own id space"
        );
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, _| title(
                a, new_id
            )
            .as_deref()
                == Some("from the phone")
                && title(a, 10).is_none()),
            "joiner edit / delete did not arrive"
        );
        assert_eq!(rows(&hw), Some(6000), "one deleted, one added");
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn a_dropped_connection_is_re_dialled_and_catches_up() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        join.join_iroh(&port, &host.token.clone());
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b
                .pages
                .is_empty()
                && a.pages.len() == b.pages.len()),
            "initial sync"
        );
        let pid = hw
            .pages
            .iter()
            .find(|p| p.blocks.len() > 1)
            .map(|p| p.id)
            .unwrap();
        hw.current = Some(pid);
        jw.current = Some(pid);
        // the network drops under the joiner
        let conn = *join.conns.keys().next().expect("joined");
        join.drop_conn(conn, "Connection lost");
        assert!(
            join.reconnecting(),
            "a joined session must try to come back"
        );
        assert_eq!(join.mode, Mode::Joining);
        // meanwhile the host edits, and so does the (offline) joiner
        hw.page_mut(pid).unwrap().blocks[0].text = "typed while you were away".into();
        jw.page_mut(pid).unwrap().blocks[1].text = "typed offline".into();
        host.poke();
        join.poke();
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 15, |a, b| b
                .page(pid)
                .map_or(false, |p| p.blocks[0].text
                    == "typed while you were away")
                && a.page(pid)
                    .map_or(false, |p| p.blocks[1].text == "typed offline")),
            "both sides must converge after the reconnect"
        );
        assert_eq!(join.mode, Mode::Joined);
        assert!(!join.reconnecting());
        assert!(
            join.want_ws.is_none(),
            "a reconnect must not create another shared workspace"
        );
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn a_removed_device_cannot_come_back_with_the_token() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        let token = host.token.clone();
        join.join_iroh(&port, &token);
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |_, b| !b
                .pages
                .is_empty()),
            "initial sync"
        );
        let id = join.me.id;
        host.kick(id);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(join.mode, Mode::Off, "the kicked device must leave");
        join.join_iroh(&port, &token);
        let t1 = Instant::now();
        while t1.elapsed() < Duration::from_secs(4) {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_ne!(join.mode, Mode::Joined, "a removed device must stay out");
        assert_eq!(host.roster.len(), 1);
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn an_approved_device_is_recognised_next_time() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        let addr = port.clone();
        join.join_iroh(&addr, "");
        let t0 = Instant::now();
        while host.requests.is_empty() && t0.elapsed() < Duration::from_secs(8) {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(host.requests.len(), 1, "an unknown device must ask first");
        let conn = host.requests[0].conn;
        host.approve(conn, Role::Editor);
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |_, b| !b
                .pages
                .is_empty()),
            "initial sync"
        );
        join.stop(true);
        let t1 = Instant::now();
        while t1.elapsed() < Duration::from_secs(1) {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(host.trusted_list().len(), 1);
        join.join_iroh(&addr, "");
        let t2 = Instant::now();
        while t2.elapsed() < Duration::from_secs(6) && join.mode != Mode::Joined {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(
            join.mode,
            Mode::Joined,
            "a trusted device joins without a card"
        );
        assert!(host.requests.is_empty());
        let id = host.trusted_list()[0].0.clone();
        host.forget_trusted(&id);
        assert!(host.trusted_list().is_empty());
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn two_devices_sync_through_iroh_universal_link() {
        let (mut host, mut hw, mut join, mut jw, _port) = pair();
        let link = host.iroh_link().expect("iroh link");
        join.join_text(&link, "");
        let blocks = |w: &Workspace| w.pages.iter().map(|p| p.blocks.len()).sum::<usize>();
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 30, |a, b| !b
                .pages
                .is_empty()
                && a.pages.len() == b.pages.len()
                && blocks(a) == blocks(b)),
            "initial sync through the relay did not finish"
        );
        let pid = hw
            .pages
            .iter()
            .find(|p| p.blocks.len() > 1)
            .map(|p| p.id)
            .unwrap();
        hw.current = Some(pid);
        jw.current = Some(pid);
        jw.page_mut(pid).unwrap().blocks[0].text = "typed on the other side of the relay".into();
        join.poke();
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 30, |a, _| a
                .page(pid)
                .map_or(false, |p| p.blocks[0].text
                    == "typed on the other side of the relay")),
            "edit through the relay did not arrive"
        );
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn a_new_device_needs_the_hosts_approval() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        host.auto_admit = false;
        join.join_iroh(&port, &host.token.clone());
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(8) && host.requests.is_empty() {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert!(!host.requests.is_empty(), "no approval card appeared");
        // the right code alone does not let anybody in
        for _ in 0..60 {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert!(
            jw.pages.is_empty() && join.mode != Mode::Joined,
            "joined without approval"
        );
        let conn = host.requests[0].conn;
        host.approve(conn, Role::Editor);
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b
                .pages
                .is_empty()
                && a.pages.len() == b.pages.len()),
            "no sync after approval"
        );
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn rejoining_reopens_the_same_share_and_lists_what_changed() {
        let ctx = egui::Context::default();
        let mut host = Collab::new(&ctx);
        host.auto_admit = true;
        let mut join = Collab::new(&egui::Context::default());
        let mut hw = Workspace::seed();
        let mut jw = Workspace::blank("joiner", false, 0);
        jw.pages.clear();
        jw.current = None;
        host.start_host(&mut hw, 1, Scope::ALL, None);
        let share = host.session.as_ref().unwrap().share.clone();
        assert!(!share.is_empty());
        join.join_iroh(&wait_ticket(&host), &host.token.clone());
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b
            .pages
            .is_empty()
            && a.pages.len() == b.pages.len()));
        let pages = jw.pages.len();
        host.stop(true);
        join.stop(true);
        // the host edits while the joiner is away, then shares the same workspace again
        let pid = hw
            .pages
            .iter()
            .find(|p| p.blocks.len() > 1)
            .map(|p| p.id)
            .unwrap();
        let old = hw.page(pid).unwrap().blocks[0].text.clone();
        hw.page_mut(pid).unwrap().blocks[0].text = "changed while you were away".into();
        host.start_host(&mut hw, 1, Scope::ALL, None);
        assert_eq!(
            host.session.as_ref().unwrap().share,
            share,
            "the share id must survive a restart"
        );
        join.join_iroh(&wait_ticket(&host), &host.token.clone());
        let t0 = Instant::now();
        let mut reused = false;
        while t0.elapsed() < Duration::from_secs(10) {
            let hc = hw.current;
            host.poll(&mut hw, 1, hc, &|_| true);
            if let Some(nw) = join.take_new_workspace() {
                assert_eq!(nw.share, share);
                join.bind_existing(&mut jw, 2, now_ms().saturating_sub(60_000));
                reused = true;
            }
            let jc = jw.current;
            join.poll(&mut jw, 2, jc, &|_| true);
            if jw
                .page(pid)
                .map_or(false, |p| p.blocks[0].text == "changed while you were away")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        assert!(reused, "the existing workspace was not reused");
        assert_eq!(jw.pages.len(), pages, "no page may be duplicated or lost");
        let ch = join
            .changes
            .list
            .iter()
            .find(|c| c.what == "Block" && c.after.contains("changed while you were away"))
            .expect("the change is listed for review");
        assert_eq!(ch.kind, changes::Kind::Edited);
        assert_eq!(ch.before, crate::rich::strip(&old));
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn comments_reach_the_other_device_without_waiting_for_the_full_scan() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        join.join_iroh(&port, &host.token.clone());
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b
            .pages
            .is_empty()
            && a.pages.len() == b.pages.len()));
        let pid = hw.pages[0].id;
        let cm = crate::model::Comment {
            id: jw.id(),
            author: "J".into(),
            author_id: join.me.id,
            text: "hello @host".into(),
            ts: 1.0,
            ..Default::default()
        };
        jw.comments.entry(pid).or_default().push(cm);
        let t0 = Instant::now();
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 3, |a, _| a
                .comments
                .get(&pid)
                .map_or(false, |l| l.iter().any(|c| c.text == "hello @host"))),
            "comment did not arrive"
        );
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "comment sync is too slow: {:?}",
            t0.elapsed()
        );
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn hostile_presence_is_sanitised() {
        let mut p = Presence {
            draft: Some(LiveDraft {
                doc: 1,
                page: 0,
                tool: 9,
                color: 0,
                pts: vec![[0.0, 0.0]],
            }),
            ..Default::default()
        };
        // the same rules the host applies on receipt
        if let Some(d) = p.draft.as_mut() {
            d.pts.truncate(128);
            if d.tool > 3 {
                p.draft = None;
            }
        }
        assert!(p.draft.is_none());
    }

    #[test]
    fn link_values_survive_percent_encoding_and_candidates_get_ports() {
        for v in ["Lumen-AB12", "My Phone's Wi-Fi", "pä$$w0rd=&?%", "日本語"] {
            assert_eq!(pct_decode(&pct_encode(v)), v);
        }
        assert_eq!(with_port("10.0.0.2"), format!("10.0.0.2:{PORT}"));
        assert_eq!(with_port("10.0.0.2:5000"), "10.0.0.2:5000");
        assert_eq!(with_port("[::1]"), format!("[::1]:{PORT}"));
        assert_eq!(with_port("[::1]:7"), "[::1]:7");
    }

    #[test]
    fn one_iroh_link_carries_ticket_and_hotspot() {
        let (mut host, _hw, _join, _jw, _ticket) = pair();
        let l = host.iroh_link().unwrap();
        assert!(l.starts_with("lumen://join?iroh="), "{l}");
        assert!(l.contains(&format!("&t={}", host.token)), "{l}");
        host.hs = ("up".into(), "DIRECT-lumen 1".into(), "pw d/1".into());
        let d = host.iroh_link().unwrap();
        assert!(d.contains("&ws=DIRECT-lumen%201&wp=pw%20d%2F1"), "{d}");
        host.stop(true);
    }

    #[test]
    fn a_device_dials_universal_iroh_link_and_syncs() {
        let (mut host, mut hw, mut join, mut jw, _ticket) = pair();
        let link = host.iroh_link().expect("iroh_link");
        join.join_text(&link, "");
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 12, |a, b| !b
                .pages
                .is_empty()
                && a.pages.len() == b.pages.len()),
            "did not connect through the universal iroh link"
        );
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn a_suggestion_waits_for_the_host_and_a_refusal_restores_the_joiner() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        host.auto_admit = false;
        join.join_iroh(&port, &host.token.clone());
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(8) && host.requests.is_empty() {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        let conn = host.requests[0].conn;
        host.approve(conn, Role::Suggester);
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b
                .pages
                .is_empty()
                && a.pages.len() == b.pages.len()),
            "no sync"
        );
        assert_eq!(join.role, Role::Suggester);
        let pid = hw
            .pages
            .iter()
            .find(|p| p.blocks.len() > 1)
            .map(|p| p.id)
            .unwrap();
        hw.current = Some(pid);
        jw.current = Some(pid);
        let original = hw.page(pid).unwrap().blocks[0].text.clone();
        jw.page_mut(pid).unwrap().blocks[0].text = "my suggestion".into();
        join.poke();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(8) && host.suggestions.is_empty() {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(host.suggestions.len(), 1, "the suggestion was not held");
        assert_eq!(
            hw.page(pid).unwrap().blocks[0].text,
            original,
            "a suggestion must not change the host"
        );
        assert_eq!(host.suggestions[0].after, "my suggestion");
        host.resolve_suggestions(&[], &[0]);
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 8, |_, b| b
                .page(pid)
                .unwrap()
                .blocks[0]
                .text
                == original),
            "refusal did not restore the joiner"
        );
        jw.page_mut(pid).unwrap().blocks[0].text = "second suggestion".into();
        join.poke();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(8) && host.suggestions.is_empty() {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        host.resolve_suggestions(&[0], &[]);
        assert!(
            until(&mut host, &mut hw, &mut join, &mut jw, 8, |a, _| a
                .page(pid)
                .unwrap()
                .blocks[0]
                .text
                == "second suggestion"),
            "accepted suggestion did not apply"
        );
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn selection_spans_cover_every_row_of_a_multi_line_selection() {
        // "abc\n" / "" (an empty line) / "defg"
        let rows = [(3, true), (0, true), (4, false)];
        assert_eq!(
            selection_spans(&rows, 1, 7),
            vec![(0, 1, None), (1, 0, None), (2, 0, Some(2))]
        );
        // inside one row
        assert_eq!(selection_spans(&[(10, false)], 2, 5), vec![(0, 2, Some(5))]);
        // ends exactly where the next row starts: the next row is not touched
        assert_eq!(selection_spans(&rows, 0, 4), vec![(0, 0, None)]);
        // a wrapped paragraph (no line break between the two rows)
        assert_eq!(
            selection_spans(&[(6, false), (5, false)], 4, 9),
            vec![(0, 4, Some(6)), (1, 0, Some(3))]
        );
        assert!(selection_spans(&rows, 3, 3).is_empty());
    }

    #[test]
    fn a_remote_caret_outlives_idle_frames_and_ends_on_blur() {
        set_caret(5, 1, 2, 10);
        assert_eq!(caret_state(), Some((5, 1, 2)));
        clear_caret(6); // another block losing focus does not touch mine
        assert_eq!(caret_state(), Some((5, 1, 2)));
        clear_caret(5);
        assert_eq!(caret_state(), None);
    }

    #[test]
    fn universal_iroh_link_parses_ticket_and_token() {
        let ctx = egui::Context::default();
        let mut join = Collab::new(&ctx);
        let sk = iroh_transport::secret_key_from_seed([7u8; 32]);
        let eid = sk.public().to_string();
        let link = format!("lumen://join?iroh={eid}&t=ABCD123456");
        join.join_text(&link, "");
        assert!(
            matches!(join.target, Some(Target::Iroh { ref ticket, ref token }) if ticket == &eid && token == "ABCD123456")
        );
        join.stop(false);
    }

    #[test]
    fn following_pulls_the_view_to_the_person_only_when_cursor_following_is_on() {
        let ctx = egui::Context::default();
        let mut c = Collab::new(&ctx);
        c.ui.follow = Some(7);
        publish(&c);
        VIEW.with(|v| {
            let v = v.borrow();
            assert_eq!(v.follow, Some(7));
            assert!(v.follow_cursor);
        });
        c.ui.follow_cursor_off = true;
        publish(&c);
        assert!(follow_pointer(1).is_none());
        VIEW.with(|v| assert!(!v.borrow().follow_cursor));
    }

    #[test]
    fn universal_nearby_prefers_lan_over_relay_and_accepts_browser_hosts() {
        let mut c = Collab::new(&egui::Context::default());
        let mut ws = crate::model::Workspace::blank("Test", false, 0);
        // Browser host has port 0 and arrives via relay:ROOM
        c.inbox.push(Event::Found(
            Beacon {
                v: PROTO,
                id: 777,
                device: "Chrome".into(),
                name: "Ada".into(),
                platform: "web".into(),
                port: 0,
                what: "Workspace".into(),
                title: "Notes".into(),
                people: 1,
                iroh: String::new(),
            },
            "relay:ABCD1234".into(),
        ));
        // Native host arrives via LAN first, then via relay
        c.inbox.push(Event::Found(
            Beacon {
                v: PROTO,
                id: 888,
                device: "Laptop".into(),
                name: "Bob".into(),
                platform: "windows".into(),
                port: PORT,
                what: "Page".into(),
                title: "Plan".into(),
                people: 2,
                iroh: String::new(),
            },
            "192.168.1.42:47615".into(),
        ));
        c.inbox.push(Event::Found(
            Beacon {
                v: PROTO,
                id: 888,
                device: "Laptop".into(),
                name: "Bob".into(),
                platform: "windows".into(),
                port: PORT,
                what: "Page".into(),
                title: "Plan".into(),
                people: 3,
                iroh: String::new(),
            },
            "relay:WXYZ9999".into(),
        ));
        c.poll(&mut ws, 1, None, &|_| false);
        assert_eq!(c.found.len(), 2);
        let web = c.found.iter().find(|f| f.beacon.id == 777).unwrap();
        assert_eq!(web.ip, "relay:ABCD1234");
        let lan = c.found.iter().find(|f| f.beacon.id == 888).unwrap();
        assert_eq!(
            lan.ip, "192.168.1.42:47615",
            "direct LAN must win over relay"
        );
        assert_eq!(lan.beacon.people, 3);
    }
}
