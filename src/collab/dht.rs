//! "Anywhere in the world without our site": peer discovery through the public BitTorrent mainline DHT (BEP-5), IPv4, UDP, no account.
//!
//! The share code is hashed into a 20-byte *infohash* (`SHA-256("lumen-dht-v1" ‖ code)[..20]`, so the DHT learns nothing about the code).
//! The host **announces** `(infohash, TCP port)` to the nodes closest to it; a joiner who knows the code **looks the infohash up** and gets
//! the host's public `ip:port` back, then dials it exactly like any other direct address — the Noise handshake and the token proof still
//! decide who gets in, the DHT only says where to knock. A host that is behind a NAT needs the public address it already gets from UPnP /
//! the hole punch (`wan_address`): the DHT node records the source address of the `announce_peer` packet.
//!
//! The client is a few hundred lines: bencode, `get_peers` iterations towards the target, `announce_peer` with the returned write token.
//! It runs on its own thread, never blocks the UI and stops with a flag. Bootstrap nodes are parameters so the tests run against a fake node.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BOOTSTRAP: [&str; 3] = [
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "router.utorrent.com:6881",
];

pub fn infohash(code: &str) -> [u8; 20] {
    let mut h = Sha256::new();
    h.update(b"lumen-dht-v1");
    h.update(code.trim().to_ascii_uppercase().as_bytes());
    let d = h.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&d[..20]);
    out
}

// ───────────── bencode ─────────────

#[derive(Clone, Debug, PartialEq)]
pub enum B {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<B>),
    Dict(BTreeMap<Vec<u8>, B>),
}

impl B {
    pub fn s(s: &str) -> B {
        B::Bytes(s.as_bytes().to_vec())
    }

    pub fn dict(items: Vec<(&str, B)>) -> B {
        B::Dict(
            items
                .into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    }

    pub fn get(&self, k: &str) -> Option<&B> {
        match self {
            B::Dict(d) => d.get(k.as_bytes()),
            _ => None,
        }
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            B::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            B::Int(i) => out.extend_from_slice(format!("i{i}e").as_bytes()),
            B::Bytes(b) => {
                out.extend_from_slice(format!("{}:", b.len()).as_bytes());
                out.extend_from_slice(b);
            }
            B::List(l) => {
                out.push(b'l');
                for x in l {
                    x.encode(out);
                }
                out.push(b'e');
            }
            B::Dict(d) => {
                out.push(b'd');
                for (k, v) in d {
                    B::Bytes(k.clone()).encode(out);
                    v.encode(out);
                }
                out.push(b'e');
            }
        }
    }

    pub fn decode(data: &[u8]) -> Option<B> {
        let mut i = 0;
        let v = Self::parse(data, &mut i, 0)?;
        (i == data.len()).then_some(v)
    }

    fn parse(d: &[u8], i: &mut usize, depth: u32) -> Option<B> {
        if depth > 8 {
            return None;
        }
        match *d.get(*i)? {
            b'i' => {
                let end = d[*i..].iter().position(|c| *c == b'e')? + *i;
                let n: i64 = std::str::from_utf8(&d[*i + 1..end]).ok()?.parse().ok()?;
                *i = end + 1;
                Some(B::Int(n))
            }
            b'l' => {
                *i += 1;
                let mut l = vec![];
                while *d.get(*i)? != b'e' {
                    l.push(Self::parse(d, i, depth + 1)?);
                    if l.len() > 4096 {
                        return None;
                    }
                }
                *i += 1;
                Some(B::List(l))
            }
            b'd' => {
                *i += 1;
                let mut m = BTreeMap::new();
                while *d.get(*i)? != b'e' {
                    let Some(B::Bytes(k)) = Self::parse(d, i, depth + 1) else {
                        return None;
                    };
                    m.insert(k, Self::parse(d, i, depth + 1)?);
                    if m.len() > 4096 {
                        return None;
                    }
                }
                *i += 1;
                Some(B::Dict(m))
            }
            b'0'..=b'9' => {
                let colon = d[*i..].iter().position(|c| *c == b':')? + *i;
                let n: usize = std::str::from_utf8(&d[*i..colon]).ok()?.parse().ok()?;
                let start = colon + 1;
                let end = start.checked_add(n)?;
                if end > d.len() {
                    return None;
                }
                *i = end;
                Some(B::Bytes(d[start..end].to_vec()))
            }
            _ => None,
        }
    }
}

fn node_id() -> [u8; 20] {
    let mut id = [0u8; 20];
    for c in id.chunks_mut(8) {
        let r = super::random_u64().to_le_bytes();
        let n = c.len();
        c.copy_from_slice(&r[..n]);
    }
    id
}

fn xor_dist(a: &[u8], b: &[u8; 20]) -> [u8; 20] {
    let mut d = [0u8; 20];
    for i in 0..20 {
        d[i] = a.get(i).copied().unwrap_or(0) ^ b[i];
    }
    d
}

fn parse_peers(v: &B) -> Vec<SocketAddr> {
    let mut out = vec![];
    if let B::List(l) = v {
        for p in l {
            if let Some(b) = p.bytes().filter(|b| b.len() == 6) {
                let port = u16::from_be_bytes([b[4], b[5]]);
                if port != 0 {
                    out.push(SocketAddr::V4(SocketAddrV4::new(
                        Ipv4Addr::new(b[0], b[1], b[2], b[3]),
                        port,
                    )));
                }
            }
        }
    }
    out
}

fn parse_nodes(b: &[u8]) -> Vec<([u8; 20], SocketAddr)> {
    b.chunks_exact(26)
        .filter_map(|c| {
            let port = u16::from_be_bytes([c[24], c[25]]);
            (port != 0).then(|| {
                let mut id = [0u8; 20];
                id.copy_from_slice(&c[..20]);
                (
                    id,
                    SocketAddr::V4(SocketAddrV4::new(
                        Ipv4Addr::new(c[20], c[21], c[22], c[23]),
                        port,
                    )),
                )
            })
        })
        .collect()
}

pub struct Found {
    pub peers: Vec<SocketAddr>,
    /// Nodes that answered with a write token (closest first) — where an announce goes.
    pub tokens: Vec<(SocketAddr, Vec<u8>)>,
}

/// Iterative `get_peers` towards `target`: up to `rounds` rounds of the 8 closest unqueried nodes, `budget` in total.
pub fn get_peers(
    sock: &UdpSocket,
    id: &[u8; 20],
    target: &[u8; 20],
    boot: &[SocketAddr],
    budget: Duration,
    stop: &AtomicBool,
) -> Found {
    let start = Instant::now();
    let mut known: Vec<([u8; 20], SocketAddr)> = boot.iter().map(|a| ([0u8; 20], *a)).collect();
    let mut asked: Vec<SocketAddr> = vec![];
    let mut found = Found {
        peers: vec![],
        tokens: vec![],
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(250)));
    let mut buf = vec![0u8; 4096];
    let mut tid = 0u16;
    while start.elapsed() < budget && !stop.load(Ordering::Relaxed) {
        known.sort_by_key(|(nid, _)| xor_dist(nid, target));
        let batch: Vec<SocketAddr> = known
            .iter()
            .map(|(_, a)| *a)
            .filter(|a| !asked.contains(a))
            .take(8)
            .collect();
        if batch.is_empty() {
            break;
        }
        for a in &batch {
            tid = tid.wrapping_add(1);
            let q = B::dict(vec![
                ("t", B::Bytes(tid.to_be_bytes().to_vec())),
                ("y", B::s("q")),
                ("q", B::s("get_peers")),
                (
                    "a",
                    B::dict(vec![
                        ("id", B::Bytes(id.to_vec())),
                        ("info_hash", B::Bytes(target.to_vec())),
                    ]),
                ),
            ]);
            let mut out = vec![];
            q.encode(&mut out);
            let _ = sock.send_to(&out, a);
            asked.push(*a);
        }
        let round_end = Instant::now() + Duration::from_millis(1200);
        let mut replies = 0;
        while Instant::now() < round_end && replies < batch.len() && !stop.load(Ordering::Relaxed) {
            let Ok((n, from)) = sock.recv_from(&mut buf) else {
                continue;
            };
            let Some(msg) = B::decode(&buf[..n]) else {
                continue;
            };
            if msg.get("y").and_then(B::bytes) != Some(b"r") {
                continue;
            }
            replies += 1;
            let Some(r) = msg.get("r") else { continue };
            if let Some(v) = r.get("values") {
                for p in parse_peers(v) {
                    if !found.peers.contains(&p) {
                        found.peers.push(p);
                    }
                }
            }
            if let Some(nodes) = r.get("nodes").and_then(B::bytes) {
                for n in parse_nodes(nodes) {
                    if !known.iter().any(|(_, a)| *a == n.1) && known.len() < 256 {
                        known.push(n);
                    }
                }
            }
            if let Some(t) = r.get("token").and_then(B::bytes) {
                if found.tokens.len() < 16 && !found.tokens.iter().any(|(a, _)| *a == from) {
                    found.tokens.push((from, t.to_vec()));
                }
            }
        }
        if !found.peers.is_empty() && found.tokens.len() >= 3 {
            break;
        }
    }
    found.tokens.sort_by_key(|(a, _)| {
        known
            .iter()
            .find(|(_, k)| k == a)
            .map(|(nid, _)| xor_dist(nid, target))
            .unwrap_or([255u8; 20])
    });
    found
}

/// Tells the closest nodes that `port` on this host serves `target`. Returns how many nodes were told.
pub fn announce(
    sock: &UdpSocket,
    id: &[u8; 20],
    target: &[u8; 20],
    port: u16,
    tokens: &[(SocketAddr, Vec<u8>)],
) -> usize {
    let mut n = 0;
    for (i, (addr, token)) in tokens.iter().take(8).enumerate() {
        let q = B::dict(vec![
            ("t", B::Bytes(vec![0xA0, i as u8])),
            ("y", B::s("q")),
            ("q", B::s("announce_peer")),
            (
                "a",
                B::dict(vec![
                    ("id", B::Bytes(id.to_vec())),
                    ("info_hash", B::Bytes(target.to_vec())),
                    ("port", B::Int(port as i64)),
                    ("token", B::Bytes(token.clone())),
                    ("implied_port", B::Int(0)),
                ]),
            ),
        ]);
        let mut out = vec![];
        q.encode(&mut out);
        if sock.send_to(&out, addr).is_ok() {
            n += 1;
        }
    }
    n
}

pub fn resolve_bootstrap() -> Vec<SocketAddr> {
    BOOTSTRAP
        .iter()
        .filter_map(|h| h.to_socket_addrs().ok())
        .flat_map(|i| i.filter(|a| a.is_ipv4()))
        .collect()
}

/// A background announcer: re-announces every 9 minutes until dropped / stopped.
pub struct Announcer {
    stop: Arc<AtomicBool>,
    pub status: Arc<std::sync::Mutex<String>>,
}

impl Announcer {
    pub fn start(code: &str, port: u16, boot: Vec<SocketAddr>) -> Announcer {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(std::sync::Mutex::new("Looking for DHT nodes…".to_owned()));
        let (s2, st2, target) = (stop.clone(), status.clone(), infohash(code));
        let _ = std::thread::Builder::new()
            .name("lumen-dht-announce".into())
            .spawn(move || {
                let boot = if boot.is_empty() {
                    resolve_bootstrap()
                } else {
                    boot
                };
                let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
                    return;
                };
                let id = node_id();
                while !s2.load(Ordering::Relaxed) {
                    let f = get_peers(&sock, &id, &target, &boot, Duration::from_secs(12), &s2);
                    let told = announce(&sock, &id, &target, port, &f.tokens);
                    if let Ok(mut g) = st2.lock() {
                        *g = if told > 0 {
                            format!("Announced to {told} DHT nodes")
                        } else {
                            "No DHT node answered (offline?)".to_owned()
                        };
                    }
                    for _ in 0..(if told > 0 { 5400 } else { 300 }) {
                        if s2.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            });
        Announcer { stop, status }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One-shot lookup on a worker thread; the receiver yields the peers once (possibly empty).
pub fn lookup(code: &str, boot: Vec<SocketAddr>) -> Receiver<Vec<SocketAddr>> {
    let (tx, rx) = channel();
    let target = infohash(code);
    let _ = std::thread::Builder::new()
        .name("lumen-dht-lookup".into())
        .spawn(move || {
            let boot = if boot.is_empty() {
                resolve_bootstrap()
            } else {
                boot
            };
            let found = UdpSocket::bind("0.0.0.0:0")
                .map(|sock| {
                    get_peers(
                        &sock,
                        &node_id(),
                        &target,
                        &boot,
                        Duration::from_secs(15),
                        &AtomicBool::new(false),
                    )
                    .peers
                })
                .unwrap_or_default();
            let _ = tx.send(found);
        });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A one-node DHT: stores announced peers per infohash, answers `get_peers` with `values` (or no values) and a token.
    fn fake_node(stop: Arc<AtomicBool>) -> (SocketAddr, Arc<Mutex<Vec<([u8; 20], SocketAddr)>>>) {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let addr = sock.local_addr().unwrap();
        let store: Arc<Mutex<Vec<([u8; 20], SocketAddr)>>> = Arc::new(Mutex::new(vec![]));
        let st = store.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 4096];
            while !stop.load(Ordering::Relaxed) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else {
                    continue;
                };
                let Some(q) = B::decode(&buf[..n]) else {
                    continue;
                };
                let (Some(t), Some(name), Some(a)) =
                    (q.get("t"), q.get("q").and_then(B::bytes), q.get("a"))
                else {
                    continue;
                };
                let Some(ih) = a
                    .get("info_hash")
                    .and_then(B::bytes)
                    .filter(|b| b.len() == 20)
                else {
                    continue;
                };
                let mut key = [0u8; 20];
                key.copy_from_slice(ih);
                let reply = match name {
                    b"get_peers" => {
                        let values: Vec<B> = st
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|(k, _)| *k == key)
                            .filter_map(|(_, p)| match p {
                                SocketAddr::V4(v) => {
                                    let mut b = v.ip().octets().to_vec();
                                    b.extend_from_slice(&v.port().to_be_bytes());
                                    Some(B::Bytes(b))
                                }
                                _ => None,
                            })
                            .collect();
                        let mut r =
                            vec![("id", B::Bytes(vec![7u8; 20])), ("token", B::s("tok123"))];
                        if values.is_empty() {
                            r.push(("nodes", B::Bytes(vec![])));
                        } else {
                            r.push(("values", B::List(values)));
                        }
                        B::dict(vec![("t", t.clone()), ("y", B::s("r")), ("r", B::dict(r))])
                    }
                    b"announce_peer" => {
                        if a.get("token").and_then(B::bytes) == Some(b"tok123") {
                            let port = match a.get("port") {
                                Some(B::Int(p)) => *p as u16,
                                _ => 0,
                            };
                            st.lock()
                                .unwrap()
                                .push((key, SocketAddr::new(from.ip(), port)));
                        }
                        B::dict(vec![
                            ("t", t.clone()),
                            ("y", B::s("r")),
                            ("r", B::dict(vec![("id", B::Bytes(vec![7u8; 20]))])),
                        ])
                    }
                    _ => continue,
                };
                let mut out = vec![];
                reply.encode(&mut out);
                let _ = sock.send_to(&out, from);
            }
        });
        (addr, store)
    }

    #[test]
    fn bencode_round_trips_and_rejects_garbage() {
        let v = B::dict(vec![
            ("a", B::Int(-5)),
            ("b", B::List(vec![B::s("x"), B::Int(0)])),
            ("c", B::Bytes(vec![0, 255])),
        ]);
        let mut out = vec![];
        v.encode(&mut out);
        assert_eq!(B::decode(&out), Some(v));
        for bad in [&b"i12"[..], b"5:ab", b"d1:ae", b"x", b"l", b"i1eextra"] {
            assert!(B::decode(bad).is_none(), "{bad:?}");
        }
        let deep = "l".repeat(100);
        assert!(B::decode(deep.as_bytes()).is_none());
    }

    #[test]
    fn the_infohash_hides_the_code_and_ignores_case() {
        assert_eq!(infohash("abcde23456"), infohash(" ABCDE23456 "));
        assert_ne!(infohash("ABCDE23456"), infohash("ABCDE23457"));
        assert_eq!(infohash("X").len(), 20);
    }

    #[test]
    fn announce_then_lookup_through_a_node() {
        let stop = Arc::new(AtomicBool::new(false));
        let (node, store) = fake_node(stop.clone());
        let target = infohash("HOSTCODE12");
        // the host announces
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let f = get_peers(
            &sock,
            &node_id(),
            &target,
            &[node],
            Duration::from_secs(3),
            &AtomicBool::new(false),
        );
        assert!(f.peers.is_empty() && f.tokens.len() == 1);
        assert_eq!(announce(&sock, &node_id(), &target, 47615, &f.tokens), 1);
        let t0 = Instant::now();
        while store.lock().unwrap().is_empty() && t0.elapsed().as_secs() < 3 {
            std::thread::sleep(Duration::from_millis(20));
        }
        // a joiner knows only the code
        let found = lookup("hostcode12", vec![node])
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert_eq!(
            found,
            vec![SocketAddr::new("127.0.0.1".parse().unwrap(), 47615)]
        );
        // another code finds nothing
        assert!(lookup("OTHERCODE1", vec![node])
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .is_empty());
        // the background announcer does the same job
        let a = Announcer::start("BGCODE1234", 4242, vec![node]);
        let t0 = Instant::now();
        while !a.status.lock().unwrap().starts_with("Announced") && t0.elapsed().as_secs() < 15 {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(a.status.lock().unwrap().starts_with("Announced"));
        a.stop();
        stop.store(true, Ordering::Relaxed);
    }
}
