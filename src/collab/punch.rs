//! NAT traversal without a relay in the data path: STUN, UDP hole punching and a tiny reliable-datagram stream.
//!
//! *Why*: two phones on different mobile / home networks cannot dial each other, and UPnP is often missing (CGNAT, guest Wi-Fi).
//! Both sides learn their public UDP address from a STUN server, swap it through a rendezvous (`/api/collab/punch` on the site — it only
//! ever sees two addresses and a random cookie, never session data), and send packets at each other until the NAT mappings line up.
//! The path is then an ordinary [`Sink`] for the session: the Noise channel, the share-token proof and the host's approval card run
//! on top unchanged, so a hostile network can only break the connection, not read or alter it.
//!
//! [`Rudp`] is the stream: cumulative acks, a sliding window, retransmission with back-off, reassembly of out-of-order segments. It is a
//! pure state machine (no sockets, no clock) so the tests can run it over a lossy, reordering simulated link.
use super::*;
use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// Public STUN servers (RFC 5389 binding requests only; nothing but the source address is learnt from them).
pub const STUN: [&str; 4] = ["stun.l.google.com:19302", "stun1.l.google.com:19302", "stun.cloudflare.com:3478", "global.stun.twilio.com:3478"];

const MAGIC: u32 = 0x2112_A442;
const MTU: usize = 1100;
const WINDOW: usize = 64;
const RTO_MS: u64 = 220;
const MAX_MSG: usize = 512 * 1024;
const HEADER: usize = 17;

// ───────────── STUN ─────────────

pub fn stun_request(tx: &[u8; 12]) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[0..2].copy_from_slice(&1u16.to_be_bytes()); // Binding Request
    b[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    b[8..20].copy_from_slice(tx);
    b
}

/// The reflexive IPv4 address in a Binding Success Response for transaction `tx`.
pub fn parse_stun(buf: &[u8], tx: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < 20 || u16::from_be_bytes([buf[0], buf[1]]) != 0x0101 || buf[4..8] != MAGIC.to_be_bytes() || buf[8..20] != tx[..] {
        return None;
    }
    let end = (20 + u16::from_be_bytes([buf[2], buf[3]]) as usize).min(buf.len());
    let mut i = 20;
    while i + 4 <= end {
        let (t, l) = (u16::from_be_bytes([buf[i], buf[i + 1]]), u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize);
        let v = buf.get(i + 4..i + 4 + l)?;
        if (t == 0x0020 || t == 0x0001) && l >= 8 && v[1] == 1 {
            let (mut port, mut ip) = (u16::from_be_bytes([v[2], v[3]]), [v[4], v[5], v[6], v[7]]);
            if t == 0x0020 {
                port ^= (MAGIC >> 16) as u16;
                for (k, x) in ip.iter_mut().enumerate() {
                    *x ^= MAGIC.to_be_bytes()[k];
                }
            }
            return Some(SocketAddr::from((ip, port)));
        }
        i += 4 + l.div_ceil(4) * 4;
    }
    None
}

/// Asks the STUN servers (in order) for the public address of `sock`.
pub fn stun_query(sock: &UdpSocket) -> Option<SocketAddr> {
    let old = sock.read_timeout().ok().flatten();
    let _ = sock.set_read_timeout(Some(Duration::from_millis(600)));
    let mut found = None;
    'servers: for name in STUN {
        let Some(server) = name.to_socket_addrs().ok().and_then(|mut a| a.find(|a| a.is_ipv4())) else { continue };
        let mut tx = [0u8; 12];
        tx[..8].copy_from_slice(&random_u64().to_le_bytes());
        tx[8..].copy_from_slice(&(random_u64() as u32).to_le_bytes());
        for _ in 0..2 {
            if sock.send_to(&stun_request(&tx), server).is_err() {
                continue 'servers;
            }
            let mut buf = [0u8; 512];
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                if from.ip() == server.ip() {
                    if let Some(a) = parse_stun(&buf[..n], &tx) {
                        found = Some(a);
                        break 'servers;
                    }
                }
            }
        }
    }
    let _ = sock.set_read_timeout(old);
    found
}

// ───────────── reliable datagram stream ─────────────

const T_PUNCH: u8 = 0;
const T_DATA: u8 = 1;
const T_ACK: u8 = 2;
const T_FIN: u8 = 3;

fn packet(t: u8, cookie: u64, seq: u32, ack: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(HEADER + payload.len());
    p.push(t);
    p.extend_from_slice(&cookie.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&ack.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

struct Seg {
    seq: u32,
    data: Vec<u8>,
    sent: u64,
    tries: u32,
}

/// What [`Rudp::on_packet`] found.
#[derive(Default)]
pub struct Rx {
    pub messages: Vec<Vec<u8>>,
    pub fin: bool,
    /// A framing violation (message larger than the cap): the link must be dropped.
    pub broken: bool,
    /// The packet belonged to this link (right cookie, well-formed).
    pub valid: bool,
}

pub struct Rudp {
    cookie: u64,
    next_seq: u32,
    inflight: Vec<Seg>,
    queue: std::collections::VecDeque<Vec<u8>>,
    recv_next: u32,
    ooo: BTreeMap<u32, Vec<u8>>,
    buf: Vec<u8>,
    ack_due: bool,
    pub dead: bool,
}

impl Rudp {
    pub fn new(cookie: u64) -> Rudp {
        Rudp { cookie, next_seq: 0, inflight: vec![], queue: Default::default(), recv_next: 0, ooo: BTreeMap::new(), buf: vec![], ack_due: false, dead: false }
    }

    /// Queues one message (length-prefixed, cut into segments).
    pub fn send(&mut self, msg: &[u8]) {
        if msg.len() > MAX_MSG {
            return;
        }
        let mut framed = Vec::with_capacity(msg.len() + 4);
        framed.extend_from_slice(&(msg.len() as u32).to_be_bytes());
        framed.extend_from_slice(msg);
        for c in framed.chunks(MTU) {
            self.queue.push_back(c.to_vec());
        }
    }

    pub fn backlog(&self) -> usize {
        self.queue.len() + self.inflight.len()
    }

    pub fn on_packet(&mut self, pkt: &[u8]) -> Rx {
        let mut rx = Rx::default();
        if pkt.len() < HEADER || u64::from_be_bytes(pkt[1..9].try_into().unwrap_or([0; 8])) != self.cookie {
            return rx;
        }
        rx.valid = true;
        let seq = u32::from_be_bytes(pkt[9..13].try_into().unwrap_or([0; 4]));
        let ack = u32::from_be_bytes(pkt[13..17].try_into().unwrap_or([0; 4]));
        if pkt[0] == T_PUNCH {
            return rx;
        }
        if pkt[0] == T_FIN {
            rx.fin = true;
            return rx;
        }
        self.inflight.retain(|s| (s.seq.wrapping_sub(ack) as i32) >= 0); // cumulative ack: everything below `ack` arrived
        if pkt[0] != T_DATA {
            return rx;
        }
        self.ack_due = true;
        let ahead = seq.wrapping_sub(self.recv_next) as i32;
        if ahead == 0 {
            self.buf.extend_from_slice(&pkt[HEADER..]);
            self.recv_next = self.recv_next.wrapping_add(1);
            while let Some(d) = self.ooo.remove(&self.recv_next) {
                self.buf.extend_from_slice(&d);
                self.recv_next = self.recv_next.wrapping_add(1);
            }
        } else if ahead > 0 && (ahead as usize) < WINDOW * 2 && self.ooo.len() < WINDOW * 2 {
            self.ooo.entry(seq).or_insert_with(|| pkt[HEADER..].to_vec());
        }
        while self.buf.len() >= 4 {
            let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            if len > MAX_MSG {
                rx.broken = true;
                self.buf.clear();
                break;
            }
            if self.buf.len() < 4 + len {
                break;
            }
            rx.messages.push(self.buf[4..4 + len].to_vec());
            self.buf.drain(..4 + len);
        }
        rx
    }

    /// Packets to put on the wire now: new segments while the window has room, timed-out segments again, an ack if one is owed.
    pub fn poll(&mut self, now: u64, out: &mut Vec<Vec<u8>>) {
        for s in &mut self.inflight {
            if now.saturating_sub(s.sent) >= RTO_MS << s.tries.min(4) {
                s.tries += 1;
                s.sent = now;
                if s.tries > 14 {
                    self.dead = true;
                }
                out.push(packet(T_DATA, self.cookie, s.seq, self.recv_next, &s.data));
                self.ack_due = false;
            }
        }
        while self.inflight.len() < WINDOW {
            let Some(d) = self.queue.pop_front() else { break };
            let seq = self.next_seq;
            self.next_seq = seq.wrapping_add(1);
            out.push(packet(T_DATA, self.cookie, seq, self.recv_next, &d));
            self.inflight.push(Seg { seq, data: d, sent: now, tries: 0 });
            self.ack_due = false;
        }
        if self.ack_due {
            self.ack_due = false;
            out.push(self.ack());
        }
    }

    pub fn ack(&self) -> Vec<u8> {
        packet(T_ACK, self.cookie, 0, self.recv_next, &[])
    }
    pub fn fin(&self) -> Vec<u8> {
        packet(T_FIN, self.cookie, 0, self.recv_next, &[])
    }
    pub fn probe(cookie: u64) -> Vec<u8> {
        packet(T_PUNCH, cookie, 0, 0, &[])
    }
}

// ───────────── the link as a session transport ─────────────

struct PunchSink {
    tx: Sender<Option<Msg>>,
}

impl Sink for PunchSink {
    fn send(&self, m: &Msg) {
        let _ = self.tx.send(Some(m.clone()));
    }
    fn close(&self) {
        let _ = self.tx.send(None);
    }
}

fn encode(m: &Msg) -> Option<Vec<u8>> {
    Some(match m {
        Msg::Bin { d, more } => {
            let mut v = Vec::with_capacity(d.len() + 2);
            v.extend_from_slice(&[1, u8::from(*more)]);
            v.extend_from_slice(d);
            v
        }
        m => {
            let mut v = vec![0u8];
            v.extend_from_slice(serde_json::to_string(m).ok()?.as_bytes());
            v
        }
    })
}

fn decode(b: &[u8]) -> Option<Msg> {
    match b.split_first()? {
        (1, rest) => {
            let (flag, body) = rest.split_first()?;
            Some(Msg::Bin { d: body.to_vec(), more: *flag != 0 })
        }
        (0, rest) => match serde_json::from_slice::<Msg>(rest).ok()? {
            Msg::Enc { .. } => None, // encrypted frames only travel as `Bin` on this transport
            m => Some(m),
        },
        _ => None,
    }
}

/// Sends probes at `peer` until one of its packets arrives (both sides do this at the same time: that is what opens the NAT mappings).
fn punch(sock: &UdpSocket, peer: SocketAddr, cookie: u64, secs: u64, stop: &AtomicBool) -> bool {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(40)));
    let end = Instant::now() + Duration::from_secs(secs);
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut buf = [0u8; 2048];
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        if last.elapsed() >= Duration::from_millis(120) {
            last = Instant::now();
            let _ = sock.send_to(&Rudp::probe(cookie), peer);
        }
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            if from == peer && n >= HEADER && u64::from_be_bytes(buf[1..9].try_into().unwrap_or([0; 8])) == cookie {
                for _ in 0..4 {
                    // the other side may still be waiting for a first packet of ours
                    let _ = sock.send_to(&Rudp::probe(cookie), peer);
                    std::thread::sleep(Duration::from_millis(30));
                }
                return true;
            }
        }
    }
    false
}

/// Registers the open link with the session and runs it until it ends. Blocks (it is the link's own thread).
fn run(sock: UdpSocket, peer: SocketAddr, cookie: u64, incoming: bool, inbox: Inbox, stop: Arc<AtomicBool>) {
    let conn = inbox.conn_id();
    let (tx, rx) = channel::<Option<Msg>>();
    inbox.push(Event::Open { conn, sink: Box::new(PunchSink { tx }), addr: peer.to_string(), incoming });
    let t0 = Instant::now();
    let ms = move || t0.elapsed().as_millis() as u64;
    let mut r = Rudp::new(cookie);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(12)));
    let (mut last_rx, mut last_tx, mut last_busy) = (Instant::now(), Instant::now(), Instant::now());
    let mut buf = vec![0u8; 2048];
    let mut out: Vec<Vec<u8>> = vec![];
    let mut closing = false;
    let why = 'outer: loop {
        if stop.load(Ordering::Relaxed) {
            break "stopped";
        }
        loop {
            match rx.try_recv() {
                Ok(Some(m)) => {
                    last_busy = Instant::now();
                    if let Some(b) = encode(&m) {
                        r.send(&b);
                    }
                }
                Ok(None) => {
                    closing = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    closing = true;
                    break;
                }
            }
        }
        if inbox.pending() > 4096 {
            std::thread::sleep(Duration::from_millis(10)); // back-pressure: unread datagrams are dropped, the peer retransmits
            last_rx = Instant::now();
        } else {
            match sock.recv_from(&mut buf) {
                Ok((n, from)) if from == peer => {
                    let x = r.on_packet(&buf[..n]);
                    if x.valid {
                        last_rx = Instant::now();
                    }
                    if x.messages.len() + usize::from(x.fin) > 0 {
                        last_busy = last_rx;
                    }
                    for b in x.messages {
                        if let Some(msg) = decode(&b) {
                            inbox.push(Event::Msg { conn, msg });
                        }
                    }
                    if x.broken {
                        break "Protocol error";
                    }
                    if x.fin {
                        break "The other device closed the connection";
                    }
                }
                _ => {}
            }
        }
        out.clear();
        r.poll(ms(), &mut out);
        for p in &out {
            let _ = sock.send_to(p, peer);
            last_tx = Instant::now();
        }
        if r.dead {
            break "Connection lost";
        }
        if closing && r.backlog() == 0 {
            for _ in 0..3 {
                let _ = sock.send_to(&r.fin(), peer);
            }
            break 'outer "closed";
        }
        if last_tx.elapsed() > Duration::from_secs(4) {
            last_tx = Instant::now();
            let _ = sock.send_to(&r.ack(), peer); // keeps the NAT mapping alive
        }
        if last_rx.elapsed() > Duration::from_secs(25) {
            break "The connection timed out";
        }
        if last_busy.elapsed() > Duration::from_secs(2) {
            let _ = sock.set_read_timeout(Some(Duration::from_millis(60)));
        } else {
            let _ = sock.set_read_timeout(Some(Duration::from_millis(12)));
        }
    };
    inbox.push(Event::Closed { conn, why: why.to_owned() });
}

// ───────────── rendezvous (the site) ─────────────

fn http(req: ehttp::Request) -> Option<serde_json::Value> {
    let r = ehttp::fetch_blocking(&req).ok()?;
    if !r.ok {
        return None;
    }
    serde_json::from_slice(&r.bytes).ok()
}

fn post(base: &str, body: serde_json::Value) {
    let _ = http(ehttp::Request::post(format!("{}/api/collab/punch", base.trim_end_matches('/')), body.to_string().into_bytes()));
}

fn bound() -> Option<(UdpSocket, SocketAddr)> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    let mapped = stun_query(&sock)?;
    Some((sock, mapped))
}

fn parse_addr(v: &serde_json::Value, key: &str) -> Option<SocketAddr> {
    v.get(key)?.as_str()?.parse::<SocketAddr>().ok().filter(|a| a.is_ipv4() && !a.ip().is_unspecified() && !a.ip().is_multicast())
}

/// Joiner: offers a punch through the room, waits for the host's answer, punches. `true` once the link is open (the session got its
/// `Event::Open`); the caller falls back to the relay otherwise. Runs on its own thread.
pub fn dial(base: String, room: String, inbox: Inbox, stop: Arc<AtomicBool>) -> bool {
    let Some((sock, mine)) = bound() else { return false };
    let cookie = random_u64() | 1;
    let c = format!("{cookie:x}");
    post(&base, serde_json::json!({ "room": room, "role": "j", "c": c, "a": mine.to_string() }));
    let url = format!("{}/api/collab/punch?room={}&c={}", base.trim_end_matches('/'), room, c);
    let end = Instant::now() + Duration::from_secs(6);
    let mut host = None;
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        if let Some(a) = http(ehttp::Request::get(&url)).and_then(|v| parse_addr(&v, "a")) {
            host = Some(a);
            break;
        }
        std::thread::sleep(Duration::from_millis(350));
    }
    let Some(host) = host else { return false };
    if !punch(&sock, host, cookie, 7, &stop) {
        return false;
    }
    let st = stop.clone();
    let _ = std::thread::Builder::new().name("lumen-collab-punch".into()).spawn(move || run(sock, host, cookie, false, inbox, st));
    true
}

/// Host: answers the offers that arrive for `room` while `stop` is false.
pub fn host(base: String, room: String, inbox: Inbox, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("lumen-collab-punchd".into()).spawn(move || {
        let url = format!("{}/api/collab/punch?room={}&role=h", base.trim_end_matches('/'), room);
        let mut seen: HashSet<String> = HashSet::new();
        while !stop.load(Ordering::Relaxed) {
            if let Some(v) = http(ehttp::Request::get(&url)) {
                for o in v.get("offers").and_then(|o| o.as_array()).cloned().unwrap_or_default() {
                    let (Some(c), Some(joiner)) = (o.get("c").and_then(|c| c.as_str()).map(str::to_owned), parse_addr(&o, "a")) else { continue };
                    let Ok(cookie) = u64::from_str_radix(&c, 16) else { continue };
                    if cookie == 0 || seen.len() > 256 || !seen.insert(c.clone()) {
                        continue;
                    }
                    let (base, room, inbox, stop) = (base.clone(), room.clone(), inbox.clone(), stop.clone());
                    let _ = std::thread::Builder::new().name("lumen-collab-punch".into()).spawn(move || {
                        let Some((sock, mine)) = bound() else { return };
                        post(&base, serde_json::json!({ "room": room, "role": "h", "c": c, "a": mine.to_string() }));
                        if punch(&sock, joiner, cookie, 8, &stop) {
                            run(sock, joiner, cookie, true, inbox, stop);
                        }
                    });
                }
            }
            for _ in 0..6 {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift for the lossy link.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn stun_response_is_parsed_and_foreign_transactions_are_refused() {
        let tx = [7u8; 12];
        let req = stun_request(&tx);
        assert_eq!(&req[4..8], &MAGIC.to_be_bytes());
        // XOR-MAPPED-ADDRESS 203.0.113.9:40000
        let (ip, port) = ([203u8, 0, 113, 9], 40000u16);
        let mut r = vec![0x01, 0x01, 0, 12, 0x21, 0x12, 0xA4, 0x42];
        r.extend_from_slice(&tx);
        r.extend_from_slice(&[0, 0x20, 0, 8, 0, 1]);
        r.extend_from_slice(&(port ^ (MAGIC >> 16) as u16).to_be_bytes());
        for (k, x) in ip.iter().enumerate() {
            r.push(x ^ MAGIC.to_be_bytes()[k]);
        }
        assert_eq!(parse_stun(&r, &tx), Some(SocketAddr::from((ip, port))));
        assert_eq!(parse_stun(&r, &[8u8; 12]), None);
        assert_eq!(parse_stun(&r[..10], &tx), None);
    }

    /// Two ends over a link that drops 30 % of the datagrams and reorders / duplicates others.
    #[test]
    fn the_stream_delivers_everything_in_order_over_a_lossy_link() {
        let (mut a, mut b) = (Rudp::new(42), Rudp::new(42));
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut msgs: Vec<Vec<u8>> = (0..24u32).map(|i| vec![i as u8; 1 + (i as usize * 997) % 5000]).collect();
        msgs.push((0..300_000u32).map(|i| (i % 251) as u8).collect());
        msgs.push(vec![]);
        for m in &msgs {
            a.send(m);
        }
        let mut got: Vec<Vec<u8>> = vec![];
        let mut wire_ab: Vec<(u64, Vec<u8>)> = vec![];
        let mut wire_ba: Vec<(u64, Vec<u8>)> = vec![];
        let mut t = 0u64;
        while got.len() < msgs.len() && t < 600_000 {
            t += 5;
            let mut out = vec![];
            a.poll(t, &mut out);
            for p in out {
                if rng.next() % 100 >= 30 {
                    wire_ab.push((t + rng.next() % 40, p.clone()));
                    if rng.next() % 100 < 5 {
                        wire_ab.push((t + rng.next() % 80, p));
                    }
                }
            }
            let mut out = vec![];
            b.poll(t, &mut out);
            for p in out {
                if rng.next() % 100 >= 30 {
                    wire_ba.push((t + rng.next() % 40, p));
                }
            }
            wire_ab.sort_by_key(|x| x.0);
            while wire_ab.first().map_or(false, |x| x.0 <= t) {
                let (_, p) = wire_ab.remove(0);
                got.extend(b.on_packet(&p).messages);
            }
            wire_ba.sort_by_key(|x| x.0);
            while wire_ba.first().map_or(false, |x| x.0 <= t) {
                let (_, p) = wire_ba.remove(0);
                a.on_packet(&p);
            }
        }
        assert_eq!(got.len(), msgs.len(), "delivered {} of {} in {t} ms", got.len(), msgs.len());
        assert!(got == msgs, "content or order differs");
        assert!(!a.dead);
    }

    #[test]
    fn foreign_cookies_and_oversize_messages_are_dropped() {
        let mut b = Rudp::new(1);
        assert!(!b.on_packet(&packet(T_DATA, 2, 0, 0, b"zzzz")).valid);
        let bomb = (MAX_MSG as u32 + 1).to_be_bytes();
        let x = b.on_packet(&packet(T_DATA, 1, 0, 0, &bomb));
        assert!(x.broken);
    }

    #[test]
    fn two_sockets_punch_through_and_exchange_messages() {
        let (sa, sb) = (UdpSocket::bind("127.0.0.1:0").unwrap(), UdpSocket::bind("127.0.0.1:0").unwrap());
        let (pa, pb) = (sa.local_addr().unwrap(), sb.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let h = std::thread::spawn(move || punch(&sb, pa, 77, 5, &st));
        assert!(punch(&sa, pb, 77, 5, &stop));
        assert!(h.join().unwrap());
        // wrong cookie never opens
        let (sc, sd) = (UdpSocket::bind("127.0.0.1:0").unwrap(), UdpSocket::bind("127.0.0.1:0").unwrap());
        let _ = sd.send_to(&Rudp::probe(1), sc.local_addr().unwrap());
        assert!(!punch(&sc, sd.local_addr().unwrap(), 2, 1, &stop));
    }

    #[test]
    fn frames_round_trip_through_the_wire_encoding() {
        let m = Msg::Bin { d: vec![1, 2, 3], more: true };
        assert!(matches!(decode(&encode(&m).unwrap()), Some(Msg::Bin { d, more: true }) if d == [1, 2, 3]));
        assert!(matches!(decode(&encode(&Msg::Pending).unwrap()), Some(Msg::Pending)));
        assert!(decode(&[9, 9]).is_none());
    }
}
