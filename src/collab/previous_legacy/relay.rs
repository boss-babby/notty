//! HTTP long-poll relay transport (works on every platform, including the browser over https, and through firewalls).
//!
//! The site hosts `/api/collab` (see `src/app/api/collab/route.ts`): a room is a tiny in-memory message log. Every remote device in a
//! room becomes one *virtual connection*, so the session code treats it exactly like a socket. The relay only sees opaque frames
//! (never parsed), keeps nothing on disk and forgets a room after 30 idle minutes. Use it for "share via link" and for the web build;
//! devices on the same Wi-Fi should prefer the direct transport (lower latency, nothing leaves the network).
use super::*;
use serde::Deserialize;
use std::sync::atomic::AtomicBool;

struct Out {
    q: VecDeque<(u32, String)>,
    busy: bool,
}

struct St {
    base: String,
    room: String,
    me: u32,
    /// Random per-session secret: the relay binds it to our device id, so nobody else in the room can speak as this device.
    key: String,
    stop: AtomicBool,
    since: AtomicU64,
    inflight: AtomicBool,
    last_try: Mutex<f64>,
    inbox: Inbox,
    peers: Mutex<HashMap<u32, ConnId>>,
    out: Mutex<Out>,
}

pub struct Relay {
    st: Arc<St>,
}

#[derive(Deserialize)]
struct Item {
    from: u32,
    m: String,
}

#[derive(Deserialize)]
struct Polled {
    next: u64,
    items: Vec<Item>,
}

struct RelaySink {
    st: Arc<St>,
    to: u32,
}

impl Sink for RelaySink {
    fn send(&self, m: &Msg) {
        if let Some(s) = m.to_text() {
            push_out(&self.st, self.to, s);
        }
    }
    fn close(&self) {
        // forget the virtual connection so a device that comes back gets a fresh one
        if let Ok(mut p) = self.st.peers.lock() {
            p.remove(&self.to);
        }
    }
}

fn push_out(st: &Arc<St>, to: u32, m: String) {
    let start = {
        let Ok(mut o) = st.out.lock() else { return };
        o.q.push_back((to, m));
        if o.busy {
            false
        } else {
            o.busy = true;
            true
        }
    };
    if start {
        pump(st.clone());
    }
}

/// One POST at a time keeps the frames of a room in order (a second request in flight could overtake the first).
fn pump(st: Arc<St>) {
    let msgs: Vec<serde_json::Value> = {
        let Ok(mut o) = st.out.lock() else { return };
        let n = o.q.len().min(40);
        let v: Vec<_> =
            o.q.drain(..n)
                .map(|(to, m)| serde_json::json!({ "to": to, "m": m }))
                .collect();
        if v.is_empty() {
            o.busy = false;
        }
        v
    };
    if msgs.is_empty() || st.stop.load(Ordering::Relaxed) {
        return;
    }
    let body = serde_json::json!({ "room": st.room, "from": st.me, "k": st.key, "msgs": msgs })
        .to_string();
    let url = format!("{}/api/collab", st.base);
    let st2 = st.clone();
    ehttp::fetch(ehttp::Request::post(url, body.into_bytes()), move |_| {
        pump(st2)
    });
}

fn poll_once(st: &Arc<St>) {
    if st.stop.load(Ordering::Relaxed) || st.inflight.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut t) = st.last_try.lock() {
        *t = mono_ms();
    }
    let url = format!(
        "{}/api/collab?room={}&me={}&k={}&since={}",
        st.base,
        st.room,
        st.me,
        st.key,
        st.since.load(Ordering::Relaxed)
    );
    let st2 = st.clone();
    ehttp::fetch(ehttp::Request::get(url), move |res| {
        st2.inflight.store(false, Ordering::Release);
        if st2.stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(r) = res else { return };
        if !r.ok {
            return;
        }
        let Ok(p) = serde_json::from_slice::<Polled>(&r.bytes) else {
            return;
        };
        for it in p.items {
            let Some(msg) = Msg::from_text(&it.m) else {
                continue;
            };
            let conn = {
                let Ok(mut peers) = st2.peers.lock() else {
                    continue;
                };
                *peers.entry(it.from).or_insert_with(|| {
                    let c = st2.inbox.conn_id();
                    st2.inbox.push(Event::Open {
                        conn: c,
                        sink: Box::new(RelaySink {
                            st: st2.clone(),
                            to: it.from,
                        }),
                        addr: "link".into(),
                        incoming: true,
                    });
                    c
                })
            };
            if matches!(msg, Msg::Bye) {
                if let Ok(mut peers) = st2.peers.lock() {
                    peers.remove(&it.from);
                }
            }
            st2.inbox.push(Event::Msg { conn, msg });
        }
        st2.since.store(p.next, Ordering::Relaxed);
        poll_once(&st2);
    });
}

impl Relay {
    pub fn start(base: &str, room: &str, me: u32, inbox: &Inbox) -> Relay {
        let st = Arc::new(St {
            base: base.trim_end_matches('/').to_owned(),
            room: room.to_owned(),
            me,
            key: random_code(24),
            stop: AtomicBool::new(false),
            since: AtomicU64::new(0),
            inflight: AtomicBool::new(false),
            last_try: Mutex::new(0.0),
            inbox: inbox.clone(),
            peers: Mutex::new(HashMap::new()),
            out: Mutex::new(Out {
                q: VecDeque::new(),
                busy: false,
            }),
        });
        poll_once(&st);
        Relay { st }
    }

    /// Frame-driven restart after a failed request (no timers needed, so the same code runs in the browser).
    pub fn tick(&self) {
        let idle = !self.st.inflight.load(Ordering::Acquire);
        let waited = self.st.last_try.lock().map_or(0.0, |t| mono_ms() - *t);
        if idle && waited > 1500.0 {
            poll_once(&self.st);
        }
    }

    pub fn hello(&self, m: &Msg) {
        if let Some(s) = m.to_text() {
            push_out(&self.st, 0, s);
        }
    }

    pub fn stop(&self) {
        self.st.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Deserialize)]
struct NearItem {
    room: String,
    beacon: Beacon,
}

#[derive(Deserialize)]
struct NearList {
    items: Vec<NearItem>,
}

/// Publishes this host's beacon under the caller's NAT group on the relay site so browsers and LAN-isolated devices behind the same
/// public address can discover it in the Nearby tab (no share token travels — joining still requires host approval).
pub fn near_announce(base: &str, room: &str, beacon: &Beacon) {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() || room.is_empty() {
        return;
    }
    let body = serde_json::json!({ "room": room, "beacon": beacon }).to_string();
    let url = format!("{base}/api/collab/near");
    ehttp::fetch(ehttp::Request::post(url, body.into_bytes()), |_| {});
}

/// Polls the relay site for hosts announced from the same public address (gives WebAssembly and isolated Wi-Fi clients a real Nearby list).
pub fn near_poll(base: &str, inbox: &Inbox) {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return;
    }
    let url = format!("{base}/api/collab/near");
    let inbox = inbox.clone();
    ehttp::fetch(ehttp::Request::get(url), move |res| {
        let Ok(r) = res else { return };
        if !r.ok {
            return;
        }
        let Ok(list) = serde_json::from_slice::<NearList>(&r.bytes) else {
            return;
        };
        for it in list.items.into_iter().take(32) {
            let room = it.room.trim();
            if room.len() < 4 || room.len() > 32 || !room.chars().all(|c| c.is_ascii_alphanumeric())
            {
                continue;
            }
            inbox.push(Event::Found(it.beacon, format!("relay:{room}")));
        }
    });
}
