//! A UDP relay of the "TURN" kind for peers that cannot punch through their NATs (symmetric NATs: many mobile carriers, some offices).
//!
//! Both devices send the packets of their reliable stream (`punch::Rudp`) to this relay instead of to each other. The relay routes by the
//! 64-bit **cookie** every packet carries in bytes `1..9` (the one the two devices already agreed on through the rendezvous of the site):
//! the first two distinct source addresses seen for a cookie become the two ends of an *allocation*, and every datagram of one end is sent
//! on to the other. Nothing else is looked at, nothing is stored, and the payload is sealed by the end-to-end Noise channel anyway — the
//! relay only ever moves opaque bytes.
//!
//! Abuse limits: ≤ 4 096 allocations, a third address can never join an allocation, datagrams must look like a stream packet
//! (17 ≤ length ≤ 1 400, type ≤ 3, cookie ≠ 0), ≈ 256 KiB/s per allocation (token bucket, 512 KiB burst), idle allocations vanish after 60 s.
//!
//! Run it on any machine with a public UDP port: `lumen-turn --bind 0.0.0.0:3478`; give the address to the site (`LUMEN_TURN_ADDR`, which the
//! rendezvous announces to both devices) or to a device (`LUMEN_TURN`). The routing core ([`Relay`]) is a plain state machine and is tested
//! without sockets; [`serve`] wraps it in one thread.
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const HEADER: usize = 17;
const MAX_DATAGRAM: usize = 1400;
const MAX_ALLOCS: usize = 4096;
const IDLE: Duration = Duration::from_secs(60);
/// Bytes per second added to a bucket, and its capacity.
const RATE: f64 = 256.0 * 1024.0;
const BURST: f64 = 512.0 * 1024.0;

struct Alloc {
    a: SocketAddr,
    b: Option<SocketAddr>,
    last: Instant,
    tokens: f64,
    refilled: Instant,
}

/// The routing table. `route` says where a datagram has to go; the datagram itself is forwarded unchanged.
#[derive(Default)]
pub struct Relay {
    allocs: HashMap<u64, Alloc>,
    /// Datagrams refused (shape, third address, full table, rate) — for the operator's log line.
    pub dropped: u64,
    pub forwarded: u64,
    swept: Option<Instant>,
}

impl Relay {
    pub fn new() -> Relay {
        Relay::default()
    }

    #[cfg(test)]
    pub fn allocations(&self) -> usize {
        self.allocs.len()
    }

    /// The destination of `pkt` received from `from`, or `None` (dropped, or the other end has not shown up yet).
    pub fn route(&mut self, from: SocketAddr, pkt: &[u8], now: Instant) -> Option<SocketAddr> {
        if pkt.len() < HEADER || pkt.len() > MAX_DATAGRAM || pkt[0] > 3 || from.port() == 0 || from.ip().is_unspecified() || from.ip().is_multicast() {
            self.dropped += 1;
            return None;
        }
        let cookie = u64::from_be_bytes(pkt[1..9].try_into().ok()?);
        if cookie == 0 {
            self.dropped += 1;
            return None;
        }
        if self.swept.map_or(true, |t| now.duration_since(t) > Duration::from_secs(5)) {
            self.swept = Some(now);
            self.allocs.retain(|_, a| now.duration_since(a.last) < IDLE);
        }
        if !self.allocs.contains_key(&cookie) {
            if self.allocs.len() >= MAX_ALLOCS {
                self.dropped += 1;
                return None;
            }
            self.allocs.insert(cookie, Alloc { a: from, b: None, last: now, tokens: BURST, refilled: now });
        }
        let al = self.allocs.get_mut(&cookie)?;
        let to = if from == al.a {
            al.b
        } else if al.b == Some(from) {
            Some(al.a)
        } else if al.b.is_none() {
            al.b = Some(from);
            Some(al.a)
        } else {
            // a third address: the allocation belongs to two devices
            self.dropped += 1;
            return None;
        };
        al.tokens = (al.tokens + now.duration_since(al.refilled).as_secs_f64() * RATE).min(BURST);
        al.refilled = now;
        al.last = now;
        if al.tokens < pkt.len() as f64 {
            self.dropped += 1;
            return None;
        }
        al.tokens -= pkt.len() as f64;
        if to.is_some() {
            self.forwarded += 1;
        }
        to
    }
}

/// Binds `bind` and relays on a thread until `stop` is set. Returns the bound address (port 0 picks a free one).
pub fn serve(bind: &str, stop: Arc<AtomicBool>) -> std::io::Result<SocketAddr> {
    let sock = UdpSocket::bind(bind)?;
    let addr = sock.local_addr()?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    std::thread::Builder::new().name("lumen-turn".into()).spawn(move || {
        let mut relay = Relay::new();
        let mut buf = [0u8; 2048];
        while !stop.load(Ordering::Relaxed) {
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                if let Some(to) = relay.route(from, &buf[..n], Instant::now()) {
                    let _ = sock.send_to(&buf[..n], to);
                }
            }
        }
    })?;
    Ok(addr)
}

/// `lumen-turn [--bind 0.0.0.0:3478]` — runs until the process is killed; prints one status line a minute.
pub fn run(args: &[String]) -> i32 {
    let mut bind = "0.0.0.0:3478".to_owned();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bind" => match it.next() {
                Some(v) => bind = v.clone(),
                None => {
                    eprintln!("--bind needs an address, e.g. 0.0.0.0:3478");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("lumen-turn — UDP relay for Lumen collaboration through symmetric NATs\n\nUSAGE: lumen-turn [--bind ADDR:PORT]   (default 0.0.0.0:3478)");
                return 0;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return 2;
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    match serve(&bind, stop) {
        Ok(addr) => {
            eprintln!("lumen-turn relaying on udp://{addr}");
            loop {
                std::thread::sleep(Duration::from_secs(60));
                eprintln!("lumen-turn alive");
            }
        }
        Err(e) => {
            eprintln!("cannot bind {bind}: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::punch::Rudp;

    fn sa(p: u16) -> SocketAddr {
        SocketAddr::from(([203, 0, 113, 7], p))
    }

    #[test]
    fn the_first_two_addresses_of_a_cookie_are_joined_and_a_third_is_refused() {
        let mut r = Relay::new();
        let t = Instant::now();
        let probe = Rudp::probe(42);
        assert_eq!(r.route(sa(1000), &probe, t), None, "the other end has not shown up yet");
        assert_eq!(r.route(sa(2000), &probe, t), Some(sa(1000)));
        assert_eq!(r.route(sa(1000), &probe, t), Some(sa(2000)));
        assert_eq!(r.route(sa(3000), &probe, t), None, "third address");
        assert_eq!(r.route(sa(3000), &Rudp::probe(43), t), None, "a new cookie starts its own allocation");
        assert_eq!(r.allocations(), 2);
    }

    #[test]
    fn malformed_datagrams_are_dropped_and_idle_allocations_expire() {
        let mut r = Relay::new();
        let t = Instant::now();
        assert_eq!(r.route(sa(1), &[0u8; 5], t), None);
        let mut bad_type = Rudp::probe(9);
        bad_type[0] = 9;
        assert_eq!(r.route(sa(1), &bad_type, t), None);
        assert_eq!(r.route(sa(1), &Rudp::probe(0), t), None, "cookie 0");
        assert_eq!(r.route(sa(1), &vec![1u8; 2000], t), None, "too big");
        assert_eq!(r.allocations(), 0);
        r.route(sa(1), &Rudp::probe(5), t);
        assert_eq!(r.allocations(), 1);
        r.route(sa(2), &Rudp::probe(6), t + Duration::from_secs(70));
        assert_eq!(r.allocations(), 1, "the idle one is gone");
    }

    #[test]
    fn a_flood_on_one_allocation_is_throttled() {
        let mut r = Relay::new();
        let t = Instant::now();
        r.route(sa(1), &Rudp::probe(8), t);
        let mut big = Rudp::probe(8);
        big.resize(1300, 0);
        let sent = (0..2000).filter(|_| r.route(sa(2), &big, t).is_some()).count();
        assert!(sent < 450 && sent > 300, "burst of about 512 KiB, then nothing: {sent}");
    }

    #[test]
    fn two_devices_behind_symmetric_nats_talk_through_the_relay_over_real_sockets() {
        let stop = Arc::new(AtomicBool::new(false));
        let relay = serve("127.0.0.1:0", stop.clone()).unwrap();
        let (sa_, sb) = (UdpSocket::bind("127.0.0.1:0").unwrap(), UdpSocket::bind("127.0.0.1:0").unwrap());
        // neither knows the other's address: both only know the relay
        let st = stop.clone();
        let h = std::thread::spawn(move || {
            let ok = crate::collab::punch::punch(&sb, relay, 0xfeed, 5, &st);
            (ok, sb)
        });
        assert!(crate::collab::punch::punch(&sa_, relay, 0xfeed, 5, &stop));
        let (ok, sb) = h.join().unwrap();
        assert!(ok);
        // a reliable stream on top, A → B and B → A
        sa_.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
        sb.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
        let (mut ra, mut rb) = (Rudp::new(0xfeed), Rudp::new(0xfeed));
        ra.send(b"hello through the relay");
        rb.send(&vec![7u8; 5000]);
        let t0 = Instant::now();
        let (mut got_a, mut got_b): (Vec<Vec<u8>>, Vec<Vec<u8>>) = (vec![], vec![]);
        let mut buf = [0u8; 2048];
        while t0.elapsed() < Duration::from_secs(5) && (got_a.is_empty() || got_b.is_empty()) {
            let now = t0.elapsed().as_millis() as u64;
            for (r, s) in [(&mut ra, &sa_), (&mut rb, &sb)] {
                let mut out = vec![];
                r.poll(now, &mut out);
                for p in out {
                    let _ = s.send_to(&p, relay);
                }
            }
            for (r, s, got) in [(&mut ra, &sa_, &mut got_a), (&mut rb, &sb, &mut got_b)] {
                while let Ok((n, from)) = s.recv_from(&mut buf) {
                    assert_eq!(from, relay, "only the relay talks to a device");
                    let rx = r.on_packet(&buf[..n]);
                    got.extend(rx.messages);
                    let _ = s.send_to(&r.ack(), relay);
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        assert_eq!(got_b, vec![b"hello through the relay".to_vec()]);
        assert_eq!(got_a.len(), 1);
        assert_eq!(got_a[0].len(), 5000);
    }
}
