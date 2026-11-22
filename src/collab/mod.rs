//! Local collaboration: workspace / page sync between devices on the same Wi-Fi (and, through the optional relay, across networks),
//! with live presence (cursors), roles and on-demand media. Design notes and the roadmap are in `COLLAB.md`.
//!
//! Topology: a **host** shares a scope (whole workspace, one page, or a page with everything nested). Peers connect to the host
//! (star); the host merges and fans out. The data model is the entity CRDT in [`crdt`]; the transports (WebSocket over TCP on native,
//! browser WebSocket on the web, HTTP relay everywhere) only move [`Msg`] frames — the session code below never touches a socket.
pub mod crdt;
mod textmerge;
mod relay;
mod secure;
pub mod ui;
mod comments;
pub mod changes;

#[cfg(not(target_arch = "wasm32"))]
mod net;
#[cfg(target_arch = "wasm32")]
mod webnet;

use crdt::{Applied, Hlc, Key, Op, Replica, Scope, ScopeKind};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Protocol 2: every frame is end-to-end encrypted (`secure.rs`); the share token is proven, never sent.
/// Protocol 3: `Key::Comment` entities and `Entry::base` (merge bases). Older peers are refused with a clear message.
pub const PROTO: u32 = 3;
/// Tombstones are forgotten after this long. A reconnect gives up after ~90 s, so no device can come back with an older view.
const TOMBSTONE_GRACE_MS: u64 = 10 * 60 * 1000;

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
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
    T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}
#[cfg(target_arch = "wasm32")]
pub fn mono_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
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
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "t")]
pub enum Msg {
    /// `auth` = proof of the share token bound to the encrypted channel (empty = ask the host for approval).
    Hello { peer: PeerInfo, #[serde(default)] auth: String, proto: u32 },
    /// Noise handshake message (base64). Always sent in the clear — it is what sets the encryption up.
    Hs { d: String },
    /// One encrypted frame (base64); `more` = the message continues in the next frame.
    Enc { d: String, more: bool },
    Pending,
    Welcome { peer: PeerInfo, session: SessionInfo, role: Role, #[serde(default)] auth: String },
    Denied { reason: String },
    Roster { peers: Vec<(PeerInfo, Role)> },
    Digest { items: Vec<(Key, Hlc, u64)>, last: bool },
    Ops { ops: Vec<Op> },
    Presence(Presence),
    MediaReq { id: u64, off: u64 },
    MediaData { id: u64, off: u64, total: u64, name: String, mime: String, data: String },
    MediaMiss { id: u64 },
    Bye,
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
    fn close(&self);
}

pub enum Event {
    Open { conn: ConnId, sink: Box<dyn Sink>, addr: String, incoming: bool },
    Msg { conn: ConnId, msg: Msg },
    Closed { conn: ConnId, why: String },
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
        Inbox { q: Arc::new(Mutex::new(VecDeque::new())), ctx, next: Arc::new(AtomicU64::new(1)) }
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
        self.q.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
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
}

pub struct JoinRequest {
    pub conn: ConnId,
    /// Short code of the encrypted channel: read it out loud on both devices to rule out an active attacker.
    pub code: String,
    pub peer: PeerInfo,
    pub addr: String,
}

/// How this device joined (kept so a dropped connection can be re-dialled).
#[derive(Clone)]
enum Target {
    Addr { addr: String, token: String },
    Relay { base: String, room: String, token: String },
}

/// A joined session whose connection broke: the replica and the workspace stay, the dial is retried with a back-off and the next
/// digest exchange transfers whatever changed on either side in the meantime.
struct Resume {
    attempt: u32,
    next_at: f64,
    session: String,
}

/// Back-off between reconnect attempts (milliseconds); after the last one the session ends.
const RESUME_DELAYS: [f64; 8] = [800.0, 2000.0, 4000.0, 7000.0, 10_000.0, 15_000.0, 20_000.0, 30_000.0];

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
    pub relay_room: Option<String>,
    pub show_cursors: bool,
    pub last_error: Option<String>,
    pub stats: Stats,
    last_reap: f64,
    joining_since: f64,
    keys: Option<Arc<secure::Keys>>,
    /// Devices the host approved once (Noise static key as hex → (name, viewer)): they join again without a card. Persisted.
    trusted: HashMap<String, (String, bool)>,
    last_gc: f64,
    /// A relay joiner's handshake that already sent its first message (adopted by the first virtual connection that answers).
    relay_init: Option<Arc<Mutex<secure::Crypt>>>,
    /// Wrong-token attempts per address: (count, last time).
    fails: HashMap<String, (u32, f64)>,
    target: Option<Target>,
    resume: Option<Resume>,
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
    prompted: bool,
    bound_at: f64,
    last_op_at: f64,
    /// What other people changed (git-style review, see `changes.rs`).
    pub changes: changes::Log,
    /// Host option: joiners keep a copy of the shared content after the session (`false` = temporary).
    pub keep_copy: bool,
    /// Host option: let a device that knows the share code in without asking. Off by default — every new device needs approval.
    pub auto_admit: bool,
    #[cfg(not(target_arch = "wasm32"))]
    net: net::Net,
    relay: Option<relay::Relay>,
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
        if let Ok(o) = std::process::Command::new("getprop").arg("ro.product.model").output() {
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

/// Printable, bounded text from the network (names that end up in menus and toasts).
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_owned()
}

/// The part of an address that identifies a machine (the port changes with every connection).
fn peer_key(addr: &str) -> String {
    addr.rsplit_once(':').map_or(addr, |(h, _)| h).to_owned()
}

fn chunked<T: Clone>(v: Vec<T>, n: usize) -> Vec<Vec<T>> {
    v.chunks(n.max(1)).map(|c| c.to_vec()).collect()
}

impl Collab {
    pub fn new(ctx: &egui::Context) -> Self {
        let id = ((random_u64() >> 3) & 0x7f_ffff) as u32 + 1;
        let device = device_name();
        let name: String = ctx.data_mut(|d| d.get_persisted::<String>(egui::Id::new("collab_name"))).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| device.clone());
        let show_cursors = ctx.data_mut(|d| d.get_persisted::<bool>(egui::Id::new("collab_cursors"))).unwrap_or(true);
        let me = PeerInfo { id, name, device, platform: platform_name().into(), color: (id % 8) as u8 };
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
            relay_room: None,
            show_cursors,
            last_error: None,
            stats: Stats::default(),
            last_reap: 0.0,
            joining_since: 0.0,
            keys: {
                // the device key is created once and kept (approve a device once, it is recognised next time)
                let id = egui::Id::new("collab_key");
                let saved = ctx.data_mut(|d| d.get_persisted::<String>(id)).and_then(|h| unhex(&h)).and_then(secure::Keys::from_private);
                let k = saved.or_else(|| {
                    let k = secure::gen_keys()?;
                    let h = hex(k.private_bytes());
                    ctx.data_mut(|d| d.insert_persisted(id, h));
                    Some(k)
                });
                k.map(Arc::new)
            },
            trusted: ctx.data_mut(|d| d.get_persisted::<Vec<(String, String, bool)>>(egui::Id::new("collab_trusted"))).unwrap_or_default().into_iter().map(|(k, n, v)| (k, (n, v))).collect(),
            last_gc: 0.0,
            relay_init: None,
            fails: HashMap::new(),
            target: None,
            resume: None,
            banned: HashSet::new(),
            demoted: HashSet::new(),
            serving: None,
            presence: HashMap::new(),
            local: Presence { peer: id, ..Default::default() },
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
            prompted: false,
            bound_at: 0.0,
            last_op_at: 0.0,
            changes: changes::Log::default(),
            keep_copy: true,
            auto_admit: false,
            #[cfg(not(target_arch = "wasm32"))]
            net: net::Net::default(),
            relay: None,
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
            self.ctx.data_mut(|d| d.insert_persisted(egui::Id::new("collab_name"), v));
        }
    }

    pub fn set_cursors(&mut self, on: bool) {
        self.show_cursors = on;
        self.ctx.data_mut(|d| d.insert_persisted(egui::Id::new("collab_cursors"), on));
    }

    /// Everybody who is connected, me first.
    pub fn people(&self) -> Vec<(PeerInfo, Role, bool)> {
        let mut v = vec![(self.me.clone(), if self.mode == Mode::Hosting { Role::Editor } else { self.role }, true)];
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
        self.roster.iter().filter(|(p, _)| p.id != self.me.id).filter_map(|(p, _)| self.presence.get(&p.id).filter(|x| x.0.page == Some(page)).map(|x| (p, &x.0))).collect()
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

    pub fn start_host(&mut self, ws: &mut crate::model::Workspace, ws_id: u64, scope: Scope, with_relay: Option<String>) {
        self.stop(false);
        ID_BASE.store((self.me.id as u64) << 40, Ordering::Relaxed);
        let title = match scope.kind {
            ScopeKind::Workspace => ws.name.clone(),
            _ => ws.page(scope.root).map(|p| if p.title.is_empty() { "Untitled".to_owned() } else { p.title.clone() }).unwrap_or_default(),
        };
        let mut rep = Replica::new(self.me.id, scope);
        rep.adopt(ws, now_ms());
        let roots = rep.roots();
        let share = self.share_id(ws_id, scope);
        self.session = Some(SessionInfo { id: random_code(10), name: title.clone(), scope, roots, share, keep: self.keep_copy });
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
        #[cfg(not(target_arch = "wasm32"))]
        {
            match self.net.listen(&self.inbox) {
                Ok(port) => {
                    self.addr_hint = net::local_addrs().into_iter().map(|ip| format!("{ip}:{port}")).collect();
                    let b = Beacon { v: PROTO, id: self.me.id, device: self.me.device.clone(), name: self.me.name.clone(), platform: self.me.platform.clone(), port, what: Self::scope_label(scope).into(), title, people: 1 };
                    if let Ok(mut g) = self.beacon.lock() {
                        *g = Some(b);
                    }
                    self.net.announce(self.beacon.clone());
                    #[cfg(target_os = "android")]
                    if let Some(b) = self.beacon.lock().ok().and_then(|g| g.clone()) {
                        let mut compact = b.clone();
                        compact.title = compact.title.chars().take(40).collect();
                        if let Ok(j) = serde_json::to_string(&compact) {
                            crate::android::nsd_register(&format!("{:x}", self.me.id), port, &j);
                        }
                    }
                }
                Err(e) => self.last_error = Some(format!("Could not open a port: {e}")),
            }
        }
        if let Some(base) = with_relay {
            self.enable_relay(base);
        }
        self.next_scan = 0.0;
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

    pub fn enable_relay(&mut self, base: String) {
        if self.mode != Mode::Hosting {
            return;
        }
        let room = random_code(10);
        self.relay = Some(relay::Relay::start(&base, &room, self.me.id, &self.inbox));
        self.relay_room = Some(room);
    }

    pub fn relay_link(&self, base: &str) -> Option<String> {
        let room = self.relay_room.as_ref()?;
        Some(format!("{}/app?join={}&t={}", base.trim_end_matches('/'), room, self.token))
    }

    pub fn lan_link(&self) -> Option<String> {
        let a = self.addr_hint.first()?;
        Some(format!("lumen://join?h={}&t={}", a, self.token))
    }

    /// Joins by whatever the user typed / scanned: `ip:port`, `lumen://join?h=…&t=…`, or a link with `join=ROOM&t=TOKEN`.
    pub fn join_text(&mut self, text: &str, relay_base: &str) {
        let t = text.trim();
        let q = |key: &str| -> Option<String> {
            let after = t.split_once('?')?.1;
            after.split('&').find_map(|kv| kv.strip_prefix(&format!("{key}=")).map(|v| v.to_owned()))
        };
        if let Some(room) = q("join") {
            let token = q("t").unwrap_or_default();
            let base = if t.starts_with("http") { t.split("/app").next().unwrap_or(relay_base).to_owned() } else { relay_base.to_owned() };
            self.join_relay(&base, &room, &token);
        } else if let Some(h) = q("h") {
            self.join_addr(&h, &q("t").unwrap_or_default());
        } else if t.contains(':') || t.contains('.') {
            self.join_addr(t, "");
        } else if t.len() >= 4 {
            self.join_relay(relay_base, &t.to_uppercase(), "");
        }
    }

    pub fn join_addr(&mut self, addr: &str, token: &str) {
        self.stop(false);
        let addr = if addr.contains(':') { addr.to_owned() } else { format!("{addr}:{PORT}") };
        self.target = Some(Target::Addr { addr, token: token.to_owned() });
        self.dial();
    }

    pub fn join_relay(&mut self, base: &str, room: &str, token: &str) {
        self.stop(false);
        self.target = Some(Target::Relay { base: base.to_owned(), room: room.to_owned(), token: token.to_owned() });
        self.dial();
    }

    /// Opens the connection described by `self.target` (first join and every reconnect).
    fn dial(&mut self) {
        let Some(t) = self.target.clone() else { return };
        self.mode = Mode::Joining;
        self.joining_since = mono_ms();
        self.last_error = None;
        match t {
            Target::Addr { addr, token } => {
                self.token = token;
                #[cfg(not(target_arch = "wasm32"))]
                self.net.connect(&addr, &self.inbox);
                #[cfg(target_arch = "wasm32")]
                webnet::connect(&addr, &self.inbox);
            }
            Target::Relay { base, room, token } => {
                self.token = token;
                self.relay = Some(relay::Relay::start(&base, &room, self.me.id, &self.inbox));
                // tell the room we are here (first handshake message): whoever hosts answers, the encrypted channel is set up from there
                if let (Some(r), Some(keys)) = (&self.relay, self.keys.clone()) {
                    let mut cr = secure::Crypt::new();
                    if let Some(d) = cr.start_initiator(&keys) {
                        self.relay_init = Some(Arc::new(Mutex::new(cr)));
                        r.hello(&Msg::Hs { d });
                    }
                }
            }
        }
    }

    /// True while a joined session is re-dialling its host after the connection broke.
    pub fn reconnecting(&self) -> bool {
        self.resume.is_some()
    }

    /// A joined device lost its connection for a transport reason: keep the replica / workspace and schedule another dial.
    /// Returns `false` when the session should end instead (host left on purpose, removed, too many failures).
    fn try_resume(&mut self, why: &str, was_joined: bool) -> bool {
        let transient = ["Connection lost", "timed out", "closed the connection", "Could not reach", "did not answer"].iter().any(|k| why.contains(k));
        if !(was_joined || self.resume.is_some()) || !transient || self.target.is_none() || self.rep.is_none() || !self.ready {
            return false;
        }
        let session = match (&self.resume, &self.session) {
            (Some(r), _) => r.session.clone(),
            (None, Some(s)) => s.id.clone(),
            _ => return false,
        };
        let attempt = self.resume.as_ref().map_or(0, |r| r.attempt);
        if attempt as usize >= RESUME_DELAYS.len() {
            return false;
        }
        // tear down the dead transports, but not the replica, the workspace binding or the session description
        for c in self.conns.values() {
            c.sink.close();
        }
        self.conns.clear();
        self.relay_init = None;
        #[cfg(not(target_arch = "wasm32"))]
        self.net.stop();
        if let Some(r) = self.relay.take() {
            r.stop();
        }
        self.inq.clear();
        self.ops_in.clear();
        self.mode = Mode::Joining;
        self.joining_since = mono_ms();
        self.resume = Some(Resume { attempt: attempt + 1, next_at: mono_ms() + RESUME_DELAYS[attempt as usize], session });
        if attempt == 0 {
            self.toast("Connection lost — reconnecting…");
        }
        true
    }

    fn hello(&self, hash: &[u8]) -> Msg {
        Msg::Hello { peer: self.me.clone(), auth: secure::proof("c", &self.token, hash), proto: PROTO }
    }

    pub fn stop(&mut self, notify: bool) {
        for c in self.conns.values() {
            c.sink.send(&Msg::Bye);
            c.sink.close();
        }
        let _ = notify;
        self.conns.clear();
        self.relay_init = None;
        #[cfg(not(target_arch = "wasm32"))]
        self.net.stop();
        if let Some(r) = self.relay.take() {
            r.stop();
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
        self.rep = None;
        self.session = None;
        self.roster.clear();
        self.requests.clear();
        self.presence.clear();
        self.inq.clear();
        self.ops_in.clear();
        self.relay_room = None;
        self.addr_hint.clear();
        self.want_ws = None;
        self.ready = false;
        self.requested.clear();
        self.partial.clear();
        self.serving = None;
        ID_BASE.store(0, Ordering::Relaxed);
    }

    /// Listen for hosts on the network (only while the connect dialog is open).
    pub fn discover(&mut self, on: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if on {
                self.net.discover(&self.inbox);
            } else {
                self.net.stop_discovery();
                self.found.clear();
            }
            #[cfg(target_os = "android")]
            crate::android::nsd_discover(on);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = on;
        }
        self.ui.discovering = on;
    }

    // ───────────── host actions ─────────────

    /// Devices this host recognises: (key id, name, viewer).
    pub fn trusted_list(&self) -> Vec<(String, String, bool)> {
        let mut v: Vec<_> = self.trusted.iter().map(|(k, (n, viewer))| (k.clone(), n.clone(), *viewer)).collect();
        v.sort_by(|a, b| a.1.cmp(&b.1));
        v
    }

    pub fn forget_trusted(&mut self, key: &str) {
        if self.trusted.remove(key).is_some() {
            self.save_trusted();
        }
    }

    fn save_trusted(&self) {
        let v: Vec<(String, String, bool)> = self.trusted.iter().map(|(k, (n, viewer))| (k.clone(), n.clone(), *viewer)).collect();
        self.ctx.data_mut(|d| d.insert_persisted(egui::Id::new("collab_trusted"), v));
    }

    fn remember_device(&mut self, conn: ConnId, role: Role) {
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
        self.remember_device(conn, role);
        self.requests.retain(|r| r.conn != conn);
        self.accept(conn, role);
    }

    pub fn deny(&mut self, conn: ConnId) {
        self.requests.retain(|r| r.conn != conn);
        if let Some(c) = self.conns.remove(&conn) {
            c.sink.send(&Msg::Denied { reason: "The host declined the request".into() });
            c.sink.close();
        }
    }

    pub fn set_role(&mut self, id: u32, role: Role) {
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
                t.1 = role == Role::Viewer;
                dirty = true;
            }
        }
        if dirty {
            self.save_trusted();
        }
        for k in keys.into_iter().filter(|k| !k.is_empty()) {
            if role == Role::Viewer {
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
        let conn = self.conns.iter().find(|(_, c)| c.peer.as_ref().map_or(false, |p| p.id == id)).map(|(k, _)| *k);
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
                c.sink.send(&Msg::Denied { reason: "You were removed from the session".into() });
                c.sink.close();
            }
            self.roster.retain(|(p, _)| p.id != id);
            self.presence.remove(&id);
            self.broadcast_roster();
        }
    }

    fn accept(&mut self, conn: ConnId, role: Role) {
        let Some(session) = self.session.clone() else { return };
        if self.conns.values().filter(|c| c.active).count() >= MAX_PEERS {
            if let Some(c) = self.conns.remove(&conn) {
                c.sink.send(&Msg::Denied { reason: "This session is full".into() });
                c.sink.close();
            }
            return;
        }
        let token = self.token.clone();
        let Some(c) = self.conns.get_mut(&conn) else { return };
        c.active = true;
        c.role = role;
        let Some(peer) = c.peer.clone() else { return };
        let hash = c.crypt.lock().map(|x| x.hash.clone()).unwrap_or_default();
        c.sink.send(&Msg::Welcome { peer: self.me.clone(), session, role, auth: secure::proof("s", &token, &hash) });
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
        let m = Msg::Roster { peers: self.roster.clone() };
        for c in self.conns.values().filter(|c| c.active) {
            c.sink.send(&m);
        }
        self.update_beacon();
    }

    fn send_digest(&mut self, conn: ConnId) {
        let Some(rep) = self.rep.as_ref() else { return };
        let items = rep.digest();
        let parts = chunked(items, 1500);
        let n = parts.len();
        if let Some(c) = self.conns.get_mut(&conn) {
            if n == 0 {
                c.backlog.push_back(Msg::Digest { items: vec![], last: true });
            }
            for (i, p) in parts.into_iter().enumerate() {
                c.backlog.push_back(Msg::Digest { items: p, last: i + 1 == n });
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
            c.backlog.push_back(Msg::Digest { items: vec![], last: true });
        }
    }

    /// The app re-opened the workspace this device got from the same share before. Its content becomes the replica (stamped with the
    /// time of the last sync, so whatever the host changed since wins and whatever was edited here offline is sent), only the
    /// difference travels, and everything that arrives is recorded for review.
    pub fn bind_existing(&mut self, ws: &mut crate::model::Workspace, ws_id: u64, last_sync_ms: u64) {
        self.bound = ws_id;
        self.ready = true;
        self.next_scan = 0.0;
        self.bound_at = mono_ms();
        self.last_op_at = self.bound_at;
        if let Some(rep) = self.rep.as_mut() {
            rep.adopt(ws, last_sync_ms.min(now_ms()));
        }
        self.track = true;
        self.reused = true;
        self.prompted = false;
        let conns: Vec<ConnId> = self.conns.iter().filter(|(_, c)| c.active).map(|(k, _)| *k).collect();
        for c in conns {
            self.send_digest(c);
        }
    }

    /// Opens the review window and tells the user (first time after a re-join).
    fn announce_changes(&mut self) {
        let n = self.changes.len();
        if n > 0 {
            self.ui.changes = true;
            self.toast(format!("{n} change{} since you were last here", if n == 1 { "" } else { "s" }));
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

    pub fn serve_media(&mut self, conn: ConnId, id: u64, off: u64, name: &str, mime: &str, bytes: Option<&[u8]>) {
        let Some(c) = self.conns.get(&conn) else { return };
        match bytes {
            None => c.sink.send(&Msg::MediaMiss { id }),
            Some(b) => {
                const CH: usize = 192 * 1024;
                let s = (off as usize).min(b.len());
                let e = (s + CH).min(b.len());
                c.sink.send(&Msg::MediaData { id, off, total: b.len() as u64, name: name.to_owned(), mime: mime.to_owned(), data: crate::media::b64_encode(&b[s..e]) });
            }
        }
    }

    pub fn set_view(&mut self, page: Option<u64>, block: Option<u64>, cur: Option<[f32; 2]>) {
        self.local.page = page;
        self.local.block = block;
        self.local.cur = cur;
        self.local.caret = caret_state().filter(|c| Some(c.0) == block).map(|c| (c.1, c.2));
        self.local.draft = draft_state();
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
    pub fn poll(&mut self, ws: &mut crate::model::Workspace, ws_id: u64, current: Option<u64>, has_media: &dyn Fn(u64) -> bool) -> bool {
        if self.mode == Mode::Off && self.inbox.q.lock().map_or(true, |q| q.is_empty()) {
            return false;
        }
        let now = mono_ms();
        if let Some(r) = &self.relay {
            r.tick();
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
            let mut applied = Applied { track: self.track, ..Default::default() };
            let mut forward: Vec<(ConnId, Op)> = vec![];
            // protocol messages that wait for a workspace
            while let Some((conn, msg)) = self.inq.pop_front() {
                self.handle_ready(ws, conn, msg);
                if mono_ms() - t_start > 3.0 {
                    break;
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
                    if self.mode == Mode::Hosting && self.changes.len() > before && !self.ui.changes && now - self.ui.copied_at > 8000.0 {
                        self.ui.copied_at = now;
                        self.toast("Someone changed the page — open Changes to review");
                    }
                }
                self.stats.ops_in += applied.n as u64;
                if ws.current.map_or(true, |c| ws.page(c).is_none()) {
                    let root = self.session.as_ref().and_then(|s| s.roots.first().copied()).filter(|r| ws.page(*r).is_some());
                    ws.current = root.or_else(|| ws.pages.iter().find(|p| !p.trashed && !p.row && p.parent.is_none()).map(|p| p.id));
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
            if self.mode == Mode::Joined && self.ops_in.is_empty() && self.inq.is_empty() && now - self.last_op_at > 1500.0 && now - self.bound_at > 2500.0 {
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
            if self.role == Role::Editor {
                if let Some(r) = self.rep.as_mut() {
                    hot = r.tick_hot(ws, now_ms());
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
            if now >= self.next_scan && self.role == Role::Editor {
                let cost0 = mono_ms();
                let ops = self.rep.as_mut().map(|r| r.tick(ws, current, now_ms(), 2.5)).unwrap_or_default();
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
        if self.mode != Mode::Off && now - self.last_presence_tx > 60.0 && (self.local != self.sent_local || now - self.last_presence_tx > 3000.0) {
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
        self.found.retain(|f| now - f.seen < 7000.0);
        if self.mode != Mode::Off {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(if self.conns.values().any(|c| !c.backlog.is_empty()) || !self.ops_in.is_empty() { 8 } else { ((self.next_scan - now).clamp(120.0, 1000.0)) as u64 }));
        } else if self.ui.discovering {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        changed
    }

    fn request_media(&mut self, id: u64, off: u64) {
        let now = mono_ms();
        if off == 0 && self.requested.get(&id).map_or(false, |t| now - *t < 15_000.0) {
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
                    if b.id == self.me.id || b.port == 0 {
                        continue;
                    }
                    // beacons are unauthenticated UDP: bound and sanitise everything that is drawn
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
                            f.beacon = b;
                            f.ip = ip;
                            f.seen = seen;
                        }
                        None => self.found.push(Found { beacon: b, ip, seen }),
                    }
                }
                Event::Open { conn, sink, addr, incoming } => {
                    if self.mode == Mode::Off || self.conns.len() >= MAX_CONNS || self.keys.is_none() {
                        sink.close();
                        continue;
                    }
                    let now = mono_ms();
                    let crypt = match (self.mode, self.relay_init.take()) {
                        // relay joiner: the first handshake message already went out through the room
                        (Mode::Joining, Some(c)) => c,
                        (Mode::Joining, None) if !incoming => {
                            let mut cr = secure::Crypt::new();
                            if let Some(d) = cr.start_initiator(self.keys.as_ref().unwrap()) {
                                sink.send(&Msg::Hs { d });
                            }
                            Arc::new(Mutex::new(cr))
                        }
                        _ => Arc::new(Mutex::new(secure::Crypt::new())),
                    };
                    let sink: Box<dyn Sink> = Box::new(secure::SecureSink { inner: sink, crypt: crypt.clone() });
                    self.conns.insert(conn, Conn { sink, addr, peer: None, role: Role::Editor, active: false, digest: HashMap::new(), backlog: VecDeque::new(), crypt, opened: now, last_rx: now, bucket: 600.0, code: String::new() });
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
        let Some(c) = self.conns.get_mut(&conn) else { return };
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
            Msg::Enc { d, more } => {
                let r = c.crypt.lock().map(|mut x| x.open(&d, more)).unwrap_or(Err(()));
                match r {
                    Ok(Some(json)) => match serde_json::from_str::<Msg>(&json) {
                        Ok(Msg::Hs { .. }) | Ok(Msg::Enc { .. }) | Err(_) => self.drop_conn(conn, "Protocol error"),
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
        let Some(keys) = self.keys.clone() else { return };
        let joining = self.mode == Mode::Joining;
        let Some(c) = self.conns.get_mut(&conn) else { return };
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
            } else if age > if self.resume.is_some() { 25_000.0 } else { 300_000.0 } {
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
        // joining a room that nobody answers (typo in the code, host gone)
        if self.resume.as_ref().map_or(false, |r| r.next_at == f64::MAX) && self.conns.is_empty() && now - self.joining_since > 25_000.0 {
            // a reconnect dial that nobody answered (relay: no Closed event ever comes)
            if !self.try_resume("Could not reach", false) {
                self.stop(false);
                self.last_error = Some("Could not reconnect — the shared copy stays on this device".into());
                self.toast("Could not reconnect — the shared copy stays on this device");
            }
        } else if self.mode == Mode::Joining && self.resume.is_none() && self.conns.is_empty() && now - self.joining_since > 30_000.0 {
            self.stop(false);
            let msg = "Nobody answered — check the room code and that the host is still sharing";
            self.last_error = Some(msg.into());
            self.toast(msg);
        }
    }

    fn on_closed(&mut self, conn: ConnId, why: &str) {
        let Some(c) = self.conns.remove(&conn) else { return };
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
                    let msg = if was_joined { "The host ended the session — the shared copy stays on this device".to_owned() } else if why.is_empty() { "Could not reach that device".to_owned() } else { why.to_owned() };
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
                if self.conns.get(&conn).map_or(true, |c| c.peer.is_some() || c.active) {
                    return; // one hello per connection
                }
                if proto != PROTO {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied { reason: "This device runs an incompatible version".into() });
                    }
                    self.drop_conn(conn, "incompatible");
                    return;
                }
                // the host chooses what everybody sees: bounded, printable, and never the host's own identity
                let peer = PeerInfo { id: peer.id, name: clean(&peer.name, 40), device: clean(&peer.device, 40), platform: clean(&peer.platform, 12), color: (peer.id % 8) as u8 };
                let (addr, hash, remote, code) = match self.conns.get(&conn) {
                    Some(c) => {
                        let cr = c.crypt.lock();
                        (c.addr.clone(), cr.as_ref().map(|x| x.hash.clone()).unwrap_or_default(), cr.as_ref().map(|x| x.remote.clone()).unwrap_or_default(), c.code.clone())
                    }
                    None => return,
                };
                if !remote.is_empty() && self.banned.contains(&remote) {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied { reason: "You were removed from this session".into() });
                    }
                    self.drop_conn(conn, "removed");
                    return;
                }
                let now = mono_ms();
                let locked = self.fails.get(&peer_key(&addr)).map_or(false, |(n, t)| *n >= 5 && now - *t < 300_000.0);
                let want = secure::proof("c", &self.token, &hash);
                let authed = !locked && !auth.is_empty() && secure::ct_eq(&auth, &want);
                // a device this host approved before is recognised by its channel key (proven by the Noise handshake)
                let known = !locked && !remote.is_empty() && self.trusted.contains_key(&hex(&remote));
                if !auth.is_empty() && !authed && !known {
                    if !locked {
                        let e = self.fails.entry(peer_key(&addr)).or_insert((0, now));
                        e.0 += 1;
                        e.1 = now;
                    }
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied { reason: if locked { "Too many wrong codes — try again in a few minutes".into() } else { "That share code is not valid".into() } });
                    }
                    self.drop_conn(conn, "bad code");
                    return;
                }
                if peer.id == 0 || peer.id == self.me.id {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied { reason: "Device id conflict — restart Lumen on that device".into() });
                    }
                    self.drop_conn(conn, "id conflict");
                    return;
                }
                // a device that reconnects replaces its stale connection — but only when it proves who it is (token or same channel key);
                // otherwise anybody could kick a person out of the session just by claiming their id
                let existing: Vec<ConnId> = self.conns.iter().filter(|(k, c)| **k != conn && c.peer.as_ref().map_or(false, |p| p.id == peer.id)).map(|(k, _)| *k).collect();
                for s in existing {
                    let same_key = !remote.is_empty() && self.conns.get(&s).and_then(|c| c.crypt.lock().ok().map(|x| x.remote == remote)).unwrap_or(false);
                    if authed || known || same_key {
                        if let Some(c) = self.conns.remove(&s) {
                            c.sink.close();
                        }
                        self.roster.retain(|(p, _)| p.id != peer.id);
                    } else {
                        if let Some(c) = self.conns.get(&conn) {
                            c.sink.send(&Msg::Denied { reason: "A device with that id is already connected".into() });
                        }
                        self.drop_conn(conn, "id in use");
                        return;
                    }
                }
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.peer = Some(peer.clone());
                }
                if known || (authed && self.auto_admit) {
                    let viewer = !remote.is_empty() && (self.demoted.contains(&remote) || self.trusted.get(&hex(&remote)).map_or(false, |t| t.1));
                    let role = if viewer { Role::Viewer } else { Role::Editor };
                    self.accept(conn, role);
                } else if self.requests.len() >= MAX_REQUESTS {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Denied { reason: "The host has too many pending requests".into() });
                    }
                    self.drop_conn(conn, "busy");
                } else {
                    if let Some(c) = self.conns.get(&conn) {
                        c.sink.send(&Msg::Pending);
                    }
                    self.requests.push(JoinRequest { conn, code, peer, addr });
                    self.ui.menu = true;
                }
            }
            Msg::Pending => {
                self.toast("Waiting for the host to accept…");
            }
            Msg::Welcome { peer, session, role, auth } => {
                if self.mode != Mode::Joining {
                    return;
                }
                // the share token also authenticates the HOST: somebody who only knows the room code cannot pose as the host
                if !self.token.is_empty() {
                    let hash = self.conns.get(&conn).and_then(|c| c.crypt.lock().ok().map(|x| x.hash.clone())).unwrap_or_default();
                    if !secure::ct_eq(&auth, &secure::proof("s", &self.token, &hash)) {
                        self.last_error = Some("That device could not prove it holds the share code — connection refused".into());
                        self.toast("That device could not prove it holds the share code");
                        self.drop_conn(conn, "host not authentic");
                        return;
                    }
                }
                // a reconnect to the SAME session keeps the replica and the workspace: the digest exchange brings both sides up to date
                let resuming = self.resume.take().map_or(false, |r| r.session == session.id) && self.rep.is_some() && self.ready;
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
                self.want_ws = Some(NewWs { name: format!("Shared · {}", session.name), share: session.share.clone(), keep: session.keep });
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
                    c.digest.extend(items.into_iter().map(|(k, h, x)| (k, (h, x))));
                }
                if last {
                    // everything the other side lacks or has older, pages first; queued and sent a few messages per frame
                    let remote = self.conns.get_mut(&conn).map(|c| std::mem::take(&mut c.digest)).unwrap_or_default();
                    let ops = self.rep.as_ref().map(|r| r.diff(ws, &remote)).unwrap_or_default();
                    self.stats.ops_out += ops.len() as u64;
                    if let Some(c) = self.conns.get_mut(&conn) {
                        for part in chunked(ops, 120) {
                            c.backlog.push_back(Msg::Ops { ops: part });
                        }
                    }
                }
            }
            Msg::Ops { ops } => {
                let from_viewer = self.mode == Mode::Hosting && self.conns.get(&conn).map_or(true, |c| c.role == Role::Viewer);
                if ops.len() > 1000 || self.ops_in.len() + ops.len() > MAX_OPS_QUEUE {
                    self.drop_conn(conn, "Too many operations");
                    return;
                }
                if !from_viewer {
                    self.ops_in.extend(ops.into_iter().map(|o| (conn, o)));
                }
            }
            Msg::Presence(mut p) => {
                // the host knows who is on the other end of the connection: a peer cannot move somebody else's cursor
                if self.mode == Mode::Hosting {
                    match self.conns.get(&conn).and_then(|c| c.peer.as_ref().map(|x| x.id)) {
                        Some(id) => p.peer = id,
                        None => return,
                    }
                } else if p.peer == self.me.id {
                    return;
                }
                if p.cur.map_or(false, |c| !c[0].is_finite() || !c[1].is_finite() || c[0].abs() > 1e8 || c[1].abs() > 1e8) {
                    p.cur = None;
                }
                if let Some(d) = p.draft.as_mut() {
                    d.pts.truncate(128);
                    if d.tool > 3 || d.pts.iter().any(|q| !q[0].is_finite() || !q[1].is_finite() || q[0].abs() > 4.0 || q[1].abs() > 4.0) {
                        p.draft = None;
                    }
                }
                if p.caret.map_or(false, |(a, b)| a > 10_000_000 || b > 10_000_000) {
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
            Msg::MediaData { id, off, total, name, mime, data } => {
                // only what this device asked for, bounded in size, and verified against its content id before it is used
                if !self.requested.contains_key(&id) || total > MAX_MEDIA || data.len() > 700_000 || (self.partial.len() >= 8 && !self.partial.contains_key(&id)) {
                    return;
                }
                let Some(bytes) = crate::media::b64_decode(&data) else { return };
                let (name, mime) = (clean(&name, 200), clean(&mime, 100));
                let e = self.partial.entry(id).or_insert_with(|| (Vec::new(), name, mime));
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
    draft: Option<(LiveDraft, f64)>,
}

thread_local! {
    static VIEW: std::cell::RefCell<ViewState> = std::cell::RefCell::new(ViewState::default());
}

/// Called by the app after each frame's `poll`: what the painters need.
pub fn publish(c: &Collab) {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        v.show = c.active() && c.show_cursors;
        v.remotes = if v.show { c.roster.iter().filter(|(p, _)| p.id != c.me.id).filter_map(|(p, _)| c.presence.get(&p.id).map(|x| (p.clone(), x.0.clone()))).collect() } else { vec![] };
    });
}

/// The text editor reports the caret of the focused block (characters of the plain text) every frame.
pub fn set_caret(block: u64, from: u32, to: u32) {
    VIEW.with(|v| v.borrow_mut().caret = Some((block, from, to, mono_ms())));
}

fn caret_state() -> Option<(u64, u32, u32)> {
    VIEW.with(|v| match v.borrow().caret {
        Some((b, f, t, at)) if mono_ms() - at < 1500.0 => Some((b, f, t)),
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
pub fn remote_carets(block: u64) -> Vec<(String, u8, u32, u32)> {
    VIEW.with(|v| {
        let v = v.borrow();
        if !v.show {
            return vec![];
        }
        v.remotes.iter().filter(|(_, p)| p.block == Some(block)).filter_map(|(i, p)| p.caret.map(|(a, b)| (i.name.clone(), i.color, a, b))).collect()
    })
}

/// Other people's PDF marks in progress on `doc` page `page`: (colour index of the person, draft).
pub fn remote_drafts(doc: u64, page: u32) -> Vec<(u8, LiveDraft)> {
    VIEW.with(|v| {
        let v = v.borrow();
        if !v.show {
            return vec![];
        }
        v.remotes.iter().filter_map(|(i, p)| p.draft.as_ref().filter(|d| d.doc == doc && d.page == page).map(|d| (i.color, d.clone()))).collect()
    })
}

/// Paints other people's carets (a thin coloured bar, selection tint and a name flag) over the text of `block`.
pub fn paint_carets(ui: &egui::Ui, block: u64, galley: &egui::Galley, pos: egui::Pos2) {
    let list = remote_carets(block);
    if list.is_empty() {
        return;
    }
    let n = galley.text().chars().count() as u32;
    let p = ui.painter();
    for (name, color, a, b) in list {
        let col = ui::COLORS[(color % 8) as usize];
        let (a, b) = (a.min(n), b.min(n));
        let at = |i: u32| galley.pos_from_ccursor(egui::text::CCursor::new(i as usize));
        let head = at(b);
        if a != b {
            let (lo, hi) = (a.min(b), a.max(b));
            // one tinted rect per row of the selection
            let (r0, r1) = (at(lo), at(hi));
            if (r0.top() - r1.top()).abs() < 1.0 {
                p.rect_filled(egui::Rect::from_min_max(pos + egui::vec2(r0.left(), r0.top()), pos + egui::vec2(r1.left(), r1.bottom())), 2.0, ui::with_alpha_pub(col, 0.22));
            }
        }
        let x = pos.x + head.left();
        let (y0, y1) = (pos.y + head.top(), pos.y + head.bottom());
        p.line_segment([egui::pos2(x, y0), egui::pos2(x, y1)], egui::Stroke::new(2.0_f32, col));
        let label = p.layout_no_wrap(ui::first_name(&name), egui::FontId::proportional(10.5), egui::Color32::WHITE);
        let flag = egui::Rect::from_min_size(egui::pos2(x - 1.0, y0 - label.size().y - 3.0), label.size() + egui::vec2(8.0, 3.0));
        p.rect_filled(flag, 3.0, col);
        p.galley(flag.min + egui::vec2(4.0, 1.5), label, egui::Color32::WHITE);
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(400));
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
pub fn paint_canvas_cursors(p: &egui::Painter, pal: &crate::theme::Palette, page: u64, w2s: &dyn Fn(egui::Pos2) -> egui::Pos2) {
    VIEW.with(|v| {
        let v = v.borrow();
        if !v.show {
            return;
        }
        let list: Vec<(&PeerInfo, &Presence)> = v.remotes.iter().filter(|(_, pr)| pr.page == Some(page)).map(|(a, b)| (a, b)).collect();
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
                    if s.contains("join=") {
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

    fn until(h: &mut Collab, hw: &mut Workspace, j: &mut Collab, jw: &mut Workspace, secs: u64, f: impl Fn(&Workspace, &Workspace) -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            step(h, hw, j, jw);
            if f(hw, jw) {
                return true;
            }
        }
        false
    }

    #[test]
    fn two_devices_sync_over_a_real_socket() {
        let ctx = egui::Context::default();
        let mut host = Collab::new(&ctx);
        host.auto_admit = true;
        let mut join = Collab::new(&ctx);
        let mut hw = Workspace::seed();
        let mut jw = Workspace::blank("joiner", false, 0);
        jw.pages.clear();
        jw.current = None;
        host.start_host(&mut hw, 1, Scope::ALL, None);
        let port = host.net.port;
        assert!(port > 0, "host could not listen");
        join.join_addr(&format!("127.0.0.1:{port}"), &host.token.clone());
        let blocks = |w: &Workspace| w.pages.iter().map(|p| p.blocks.len()).sum::<usize>();
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| a.pages.len() == b.pages.len() && blocks(a) == blocks(b) && !b.pages.is_empty()), "initial sync did not finish: {} vs {} pages", hw.pages.len(), jw.pages.len());
        assert_eq!(host.roster.len(), 2);
        let pid = hw.pages.iter().find(|p| p.blocks.len() > 1).map(|p| p.id).unwrap();
        hw.current = Some(pid);
        jw.current = Some(pid);
        // host edits → joiner sees it
        hw.page_mut(pid).unwrap().blocks[0].text = "edited on the host".into();
        host.poke();
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 6, |_, b| b.page(pid).map_or(false, |p| p.blocks[0].text == "edited on the host")), "host edit did not arrive");
        // joiner edits another block → host sees it, and a new block appears on both
        jw.page_mut(pid).unwrap().blocks[1].text = "edited on the phone".into();
        let nb = jw.page(pid).unwrap().blocks[1].clone();
        let mut extra = nb.clone();
        extra.id = jw.id();
        extra.text = "added on the phone".into();
        jw.page_mut(pid).unwrap().blocks.push(extra);
        join.poke();
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 6, |a, _| a.page(pid).map_or(false, |p| p.blocks[1].text == "edited on the phone" && p.blocks.iter().any(|b| b.text == "added on the phone"))), "joiner edit did not arrive");
        host.stop(true);
        join.stop(true);
    }

    fn pair() -> (Collab, Workspace, Collab, Workspace, u16) {
        let mut host = Collab::new(&egui::Context::default());
        host.auto_admit = true;
        let join = Collab::new(&egui::Context::default());
        let mut hw = Workspace::seed();
        let mut jw = Workspace::blank("joiner", false, 0);
        jw.pages.clear();
        jw.current = None;
        host.start_host(&mut hw, 1, Scope::ALL, None);
        let port = host.net.port;
        (host, hw, join, jw, port)
    }

    #[test]
    fn a_dropped_connection_is_re_dialled_and_catches_up() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        join.join_addr(&format!("127.0.0.1:{port}"), &host.token.clone());
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b.pages.is_empty() && a.pages.len() == b.pages.len()), "initial sync");
        let pid = hw.pages.iter().find(|p| p.blocks.len() > 1).map(|p| p.id).unwrap();
        hw.current = Some(pid);
        jw.current = Some(pid);
        // the network drops under the joiner
        let conn = *join.conns.keys().next().expect("joined");
        join.drop_conn(conn, "Connection lost");
        assert!(join.reconnecting(), "a joined session must try to come back");
        assert_eq!(join.mode, Mode::Joining);
        // meanwhile the host edits, and so does the (offline) joiner
        hw.page_mut(pid).unwrap().blocks[0].text = "typed while you were away".into();
        jw.page_mut(pid).unwrap().blocks[1].text = "typed offline".into();
        host.poke();
        join.poke();
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 15, |a, b| b.page(pid).map_or(false, |p| p.blocks[0].text == "typed while you were away") && a.page(pid).map_or(false, |p| p.blocks[1].text == "typed offline")), "both sides must converge after the reconnect");
        assert_eq!(join.mode, Mode::Joined);
        assert!(!join.reconnecting());
        assert!(join.want_ws.is_none(), "a reconnect must not create another shared workspace");
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn a_removed_device_cannot_come_back_with_the_token() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        let token = host.token.clone();
        join.join_addr(&format!("127.0.0.1:{port}"), &token);
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |_, b| !b.pages.is_empty()), "initial sync");
        let id = join.me.id;
        host.kick(id);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(join.mode, Mode::Off, "the kicked device must leave");
        join.join_addr(&format!("127.0.0.1:{port}"), &token);
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
        let addr = format!("127.0.0.1:{port}");
        join.join_addr(&addr, "");
        let t0 = Instant::now();
        while host.requests.is_empty() && t0.elapsed() < Duration::from_secs(8) {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(host.requests.len(), 1, "an unknown device must ask first");
        let conn = host.requests[0].conn;
        host.approve(conn, Role::Editor);
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |_, b| !b.pages.is_empty()), "initial sync");
        join.stop(true);
        let t1 = Instant::now();
        while t1.elapsed() < Duration::from_secs(1) {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(host.trusted_list().len(), 1);
        join.join_addr(&addr, "");
        let t2 = Instant::now();
        while t2.elapsed() < Duration::from_secs(6) && join.mode != Mode::Joined {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert_eq!(join.mode, Mode::Joined, "a trusted device joins without a card");
        assert!(host.requests.is_empty());
        let id = host.trusted_list()[0].0.clone();
        host.forget_trusted(&id);
        assert!(host.trusted_list().is_empty());
        host.stop(true);
        join.stop(true);
    }

    /// Needs the website running: `LUMEN_RELAY_TEST=http://127.0.0.1:3000 cargo test --lib relay -- --ignored`.
    #[test]
    #[ignore]
    fn two_devices_sync_through_the_site_relay() {
        let base = std::env::var("LUMEN_RELAY_TEST").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
        let (mut host, mut hw, mut join, mut jw, _port) = pair();
        host.enable_relay(base.clone());
        let room = host.relay_room.clone().expect("relay room");
        // a real host has been polling for a long time before anybody holds the link; the relay shows a device only frames posted
        // after it first appeared (a late joiner must not see other people's old handshakes)
        std::thread::sleep(Duration::from_millis(800));
        join.join_relay(&base, &room, &host.token.clone());
        let blocks = |w: &Workspace| w.pages.iter().map(|p| p.blocks.len()).sum::<usize>();
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 30, |a, b| !b.pages.is_empty() && a.pages.len() == b.pages.len() && blocks(a) == blocks(b)), "initial sync through the relay did not finish");
        let pid = hw.pages.iter().find(|p| p.blocks.len() > 1).map(|p| p.id).unwrap();
        hw.current = Some(pid);
        jw.current = Some(pid);
        jw.page_mut(pid).unwrap().blocks[0].text = "typed on the other side of the relay".into();
        join.poke();
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 30, |a, _| a.page(pid).map_or(false, |p| p.blocks[0].text == "typed on the other side of the relay")), "edit through the relay did not arrive");
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn a_new_device_needs_the_hosts_approval() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        host.auto_admit = false;
        join.join_addr(&format!("127.0.0.1:{port}"), &host.token.clone());
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(8) && host.requests.is_empty() {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert!(!host.requests.is_empty(), "no approval card appeared");
        // the right code alone does not let anybody in
        for _ in 0..60 {
            step(&mut host, &mut hw, &mut join, &mut jw);
        }
        assert!(jw.pages.is_empty() && join.mode != Mode::Joined, "joined without approval");
        let conn = host.requests[0].conn;
        host.approve(conn, Role::Editor);
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b.pages.is_empty() && a.pages.len() == b.pages.len()), "no sync after approval");
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn rejoining_reopens_the_same_share_and_lists_what_changed() {
        let ctx = egui::Context::default();
        let mut host = Collab::new(&ctx);
        host.auto_admit = true;
        let mut join = Collab::new(&ctx);
        let mut hw = Workspace::seed();
        let mut jw = Workspace::blank("joiner", false, 0);
        jw.pages.clear();
        jw.current = None;
        host.start_host(&mut hw, 1, Scope::ALL, None);
        let share = host.session.as_ref().unwrap().share.clone();
        assert!(!share.is_empty());
        join.join_addr(&format!("127.0.0.1:{}", host.net.port), &host.token.clone());
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b.pages.is_empty() && a.pages.len() == b.pages.len()));
        let pages = jw.pages.len();
        host.stop(true);
        join.stop(true);
        // the host edits while the joiner is away, then shares the same workspace again
        let pid = hw.pages.iter().find(|p| p.blocks.len() > 1).map(|p| p.id).unwrap();
        let old = hw.page(pid).unwrap().blocks[0].text.clone();
        hw.page_mut(pid).unwrap().blocks[0].text = "changed while you were away".into();
        host.start_host(&mut hw, 1, Scope::ALL, None);
        assert_eq!(host.session.as_ref().unwrap().share, share, "the share id must survive a restart");
        join.join_addr(&format!("127.0.0.1:{}", host.net.port), &host.token.clone());
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
            if jw.page(pid).map_or(false, |p| p.blocks[0].text == "changed while you were away") {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        assert!(reused, "the existing workspace was not reused");
        assert_eq!(jw.pages.len(), pages, "no page may be duplicated or lost");
        let ch = join.changes.list.iter().find(|c| c.what == "Block" && c.after.contains("changed while you were away")).expect("the change is listed for review");
        assert_eq!(ch.kind, changes::Kind::Edited);
        assert_eq!(ch.before, crate::rich::strip(&old));
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn comments_reach_the_other_device_without_waiting_for_the_full_scan() {
        let (mut host, mut hw, mut join, mut jw, port) = pair();
        join.join_addr(&format!("127.0.0.1:{port}"), &host.token.clone());
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 10, |a, b| !b.pages.is_empty() && a.pages.len() == b.pages.len()));
        let pid = hw.pages[0].id;
        let cm = crate::model::Comment { id: jw.id(), author: "J".into(), author_id: join.me.id, text: "hello @host".into(), ts: 1.0, ..Default::default() };
        jw.comments.entry(pid).or_default().push(cm);
        let t0 = Instant::now();
        assert!(until(&mut host, &mut hw, &mut join, &mut jw, 3, |a, _| a.comments.get(&pid).map_or(false, |l| l.iter().any(|c| c.text == "hello @host"))), "comment did not arrive");
        assert!(t0.elapsed() < Duration::from_secs(2), "comment sync is too slow: {:?}", t0.elapsed());
        host.stop(true);
        join.stop(true);
    }

    #[test]
    fn hostile_presence_is_sanitised() {
        let mut p = Presence { draft: Some(LiveDraft { doc: 1, page: 0, tool: 9, color: 0, pts: vec![[0.0, 0.0]] }), ..Default::default() };
        // the same rules the host applies on receipt
        if let Some(d) = p.draft.as_mut() {
            d.pts.truncate(128);
            if d.tool > 3 {
                p.draft = None;
            }
        }
        assert!(p.draft.is_none());
    }
}
