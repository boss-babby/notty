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
        if let Ok(s) = serde_json::to_string(m) {
            push_out(&self.st, self.to, s);
        }
    }
    fn close(&self) {}
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
        let v: Vec<_> = o.q.drain(..n).map(|(to, m)| serde_json::json!({ "to": to, "m": m })).collect();
        if v.is_empty() {
            o.busy = false;
        }
        v
    };
    if msgs.is_empty() || st.stop.load(Ordering::Relaxed) {
        return;
    }
    let body = serde_json::json!({ "room": st.room, "from": st.me, "msgs": msgs }).to_string();
    let url = format!("{}/api/collab", st.base);
    let st2 = st.clone();
    ehttp::fetch(ehttp::Request::post(url, body.into_bytes()), move |_| pump(st2));
}

fn poll_once(st: &Arc<St>) {
    if st.stop.load(Ordering::Relaxed) || st.inflight.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut t) = st.last_try.lock() {
        *t = mono_ms();
    }
    let url = format!("{}/api/collab?room={}&me={}&since={}", st.base, st.room, st.me, st.since.load(Ordering::Relaxed));
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
        let Ok(p) = serde_json::from_slice::<Polled>(&r.bytes) else { return };
        for it in p.items {
            let Ok(msg) = serde_json::from_str::<Msg>(&it.m) else { continue };
            let conn = {
                let Ok(mut peers) = st2.peers.lock() else { continue };
                *peers.entry(it.from).or_insert_with(|| {
                    let c = st2.inbox.conn_id();
                    st2.inbox.push(Event::Open { conn: c, sink: Box::new(RelaySink { st: st2.clone(), to: it.from }), addr: "link".into(), incoming: true });
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
            stop: AtomicBool::new(false),
            since: AtomicU64::new(0),
            inflight: AtomicBool::new(false),
            last_try: Mutex::new(0.0),
            inbox: inbox.clone(),
            peers: Mutex::new(HashMap::new()),
            out: Mutex::new(Out { q: VecDeque::new(), busy: false }),
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
        if let Ok(s) = serde_json::to_string(m) {
            push_out(&self.st, 0, s);
        }
    }

    pub fn stop(&self) {
        self.st.stop.store(true, Ordering::Relaxed);
    }
}
