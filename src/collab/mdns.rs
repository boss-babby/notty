//! mDNS / DNS-SD (RFC 6762 / 6763) for `_lumen._tcp.local`: the *universal* "Nearby" channel.
//!
//! Android has always published and browsed `_lumen._tcp` through `NsdManager`; the desktop builds only had Lumen's own UDP beacons, so a
//! phone could see a laptop but not the other way round on networks that filter broadcast. This module speaks the same DNS-SD records
//! natively (pure Rust, no Bonjour / Avahi needed): a desktop host is now visible to Android's NSD, to Apple Bonjour browsers and to
//! `avahi-browse -r _lumen._tcp`, and a desktop that is looking sees Android hosts. The TXT layout is the one `CollabKit.nsdStart` writes
//! (`i0`, `i1`, … = the beacon JSON in chunks of ≤ 180 bytes), so both sides read each other's records.
//!
//! Everything here is bounded: names are decoded with a jump limit, packets are capped, at most 64 services are remembered, and the
//! parser never allocates more than the packet it was given.
use super::*;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub const SERVICE: &str = "_lumen._tcp.local";
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const MAX_PACKET: usize = 9000;
const T_A: u16 = 1;
const T_PTR: u16 = 12;
const T_TXT: u16 = 16;
const T_AAAA: u16 = 28;
const T_SRV: u16 = 33;
const T_ANY: u16 = 255;

// ───────────── wire format ─────────────

fn put_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.').filter(|l| !l.is_empty()) {
        let b = label.as_bytes();
        let n = b.len().min(63);
        out.push(n as u8);
        out.extend_from_slice(&b[..n]);
    }
    out.push(0);
}

fn put_rr(out: &mut Vec<u8>, name: &str, ty: u16, flush: bool, ttl: u32, rdata: &[u8]) {
    put_name(out, name);
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&(if flush { 0x8001u16 } else { 1u16 }).to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
}

/// `info` split into TXT strings `i0=…`, `i1=…` (≤ 180 bytes each, on character boundaries, at most 6 — what `nsdStart` does).
fn txt_chunks(info: &str) -> Vec<Vec<u8>> {
    let mut out = vec![];
    let mut cur = String::new();
    for ch in info.chars() {
        if cur.len() + ch.len_utf8() > 180 {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.truncate(6);
    out.iter()
        .enumerate()
        .map(|(i, c)| format!("i{i}={c}").into_bytes())
        .collect()
}

/// An unsolicited / answering mDNS response announcing one instance (PTR + SRV + TXT + addresses). `ttl` 0 = goodbye.
pub fn build_response(instance: &str, port: u16, info: &str, ips: &[IpAddr], ttl: u32) -> Vec<u8> {
    let fqdn = format!("{instance}.{SERVICE}");
    let host = format!("{instance}.local");
    let mut body = Vec::with_capacity(512);
    let mut an = 0u16;
    // PTR is a shared record: no cache-flush bit
    let mut r = vec![];
    put_name(&mut r, &fqdn);
    put_rr(&mut body, SERVICE, T_PTR, false, ttl, &r);
    an += 1;
    let mut r = vec![0, 0, 0, 0];
    r.extend_from_slice(&port.to_be_bytes());
    put_name(&mut r, &host);
    put_rr(&mut body, &fqdn, T_SRV, true, ttl, &r);
    an += 1;
    let mut r = vec![];
    for c in txt_chunks(info) {
        r.push(c.len() as u8);
        r.extend_from_slice(&c);
    }
    if r.is_empty() {
        r.push(0);
    }
    put_rr(&mut body, &fqdn, T_TXT, true, ttl, &r);
    an += 1;
    for ip in ips.iter().take(8) {
        match ip {
            IpAddr::V4(v4) => put_rr(&mut body, &host, T_A, true, ttl, &v4.octets()),
            IpAddr::V6(v6) => put_rr(&mut body, &host, T_AAAA, true, ttl, &v6.octets()),
        }
        an += 1;
    }
    let mut out = Vec::with_capacity(12 + body.len());
    out.extend_from_slice(&[0, 0, 0x84, 0]); // id 0, response + authoritative
    out.extend_from_slice(&[0, 0]); // no questions
    out.extend_from_slice(&an.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&body);
    out
}

/// A one-question query (`PTR _lumen._tcp.local`).
pub fn build_query(name: &str, ty: u16) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    put_name(&mut out, name);
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}

#[derive(Debug, PartialEq)]
pub enum Rd {
    Ptr(String),
    Srv { port: u16, target: String },
    Txt(Vec<Vec<u8>>),
    A(Ipv4Addr),
    Aaaa(std::net::Ipv6Addr),
    Other,
}

#[derive(Debug, Default)]
pub struct Packet {
    pub questions: Vec<(String, u16)>,
    pub records: Vec<(String, u32, Rd)>,
}

fn name_at(buf: &[u8], mut p: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *buf.get(p)? as usize;
        if len == 0 {
            p += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            let lo = *buf.get(p + 1)? as usize;
            if end.is_none() {
                end = Some(p + 2);
            }
            jumps += 1;
            if jumps > 12 {
                return None;
            }
            p = ((len & 0x3F) << 8) | lo;
            continue;
        }
        if len > 63 {
            return None;
        }
        let label = buf.get(p + 1..p + 1 + len)?;
        if out.len() + len + 1 > 255 {
            return None;
        }
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(label));
        p += 1 + len;
    }
    Some((out, end.unwrap_or(p)))
}

fn be16(b: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]))
}

pub fn parse(buf: &[u8]) -> Option<Packet> {
    if buf.len() < 12 || buf.len() > MAX_PACKET {
        return None;
    }
    let (qd, an, ns, ar) = (be16(buf, 4)?, be16(buf, 6)?, be16(buf, 8)?, be16(buf, 10)?);
    let mut p = 12usize;
    let mut pk = Packet::default();
    for _ in 0..qd.min(32) {
        let (n, e) = name_at(buf, p)?;
        pk.questions.push((n, be16(buf, e)?));
        p = e + 4;
    }
    for _ in 0..(an as usize + ns as usize + ar as usize).min(64) {
        let Some((name, e)) = name_at(buf, p) else {
            break;
        };
        let (Some(ty), Some(rl), Some(t0), Some(t1)) = (
            be16(buf, e),
            be16(buf, e + 8),
            be16(buf, e + 4),
            be16(buf, e + 6),
        ) else {
            break;
        };
        let ttl = ((t0 as u32) << 16) | t1 as u32;
        let ds = e + 10;
        let de = ds + rl as usize;
        let Some(rd) = buf.get(ds..de) else { break };
        let rec = match ty {
            T_PTR => name_at(buf, ds)
                .map(|(n, _)| Rd::Ptr(n))
                .unwrap_or(Rd::Other),
            T_SRV if rd.len() >= 7 => name_at(buf, ds + 6)
                .map(|(t, _)| Rd::Srv {
                    port: u16::from_be_bytes([rd[4], rd[5]]),
                    target: t,
                })
                .unwrap_or(Rd::Other),
            T_TXT => {
                let mut v = vec![];
                let mut i = 0;
                while i < rd.len() {
                    let l = rd[i] as usize;
                    let Some(s) = rd.get(i + 1..i + 1 + l) else {
                        break;
                    };
                    v.push(s.to_vec());
                    i += 1 + l;
                }
                Rd::Txt(v)
            }
            T_A if rd.len() == 4 => Rd::A(Ipv4Addr::new(rd[0], rd[1], rd[2], rd[3])),
            T_AAAA if rd.len() == 16 => {
                let mut o = [0u8; 16];
                o.copy_from_slice(rd);
                Rd::Aaaa(o.into())
            }
            _ => Rd::Other,
        };
        pk.records.push((name, ttl, rec));
        p = de;
    }
    Some(pk)
}

/// The beacon JSON out of the `iN=` TXT strings.
fn txt_info(strings: &[Vec<u8>]) -> Option<String> {
    let mut parts: Vec<(usize, &[u8])> = vec![];
    for s in strings {
        if s.first() == Some(&b'i') {
            if let Some(eq) = s.iter().position(|b| *b == b'=') {
                if let Ok(n) = std::str::from_utf8(&s[1..eq])
                    .unwrap_or("x")
                    .parse::<usize>()
                {
                    parts.push((n, &s[eq + 1..]));
                }
            }
        }
    }
    parts.sort_by_key(|p| p.0);
    let bytes: Vec<u8> = parts
        .into_iter()
        .take(6)
        .flat_map(|p| p.1.iter().copied())
        .collect();
    String::from_utf8(bytes).ok().filter(|s| !s.is_empty())
}

// ───────────── browsing ─────────────

#[derive(Default)]
pub struct Browser {
    /// instance FQDN → (port, host name, beacon JSON)
    services: HashMap<String, (u16, String, String)>,
    hosts: HashMap<String, Vec<IpAddr>>,
}

impl Browser {
    /// Folds one packet into the picture and returns the hosts that are now complete (address + port + beacon).
    pub fn absorb(&mut self, pk: &Packet, from: IpAddr) -> Vec<(Beacon, String)> {
        let mut touched: Vec<String> = vec![];
        for (name, ttl, rd) in &pk.records {
            let key = name.to_ascii_lowercase();
            match rd {
                Rd::Srv { port, target } if key.ends_with(SERVICE) => {
                    if *ttl == 0 {
                        self.services.remove(&key);
                        continue;
                    }
                    if self.services.len() >= 64 && !self.services.contains_key(&key) {
                        continue;
                    }
                    let e = self.services.entry(key.clone()).or_default();
                    e.0 = *port;
                    e.1 = target.to_ascii_lowercase();
                    touched.push(key);
                }
                Rd::Txt(v) if key.ends_with(SERVICE) && *ttl > 0 => {
                    if let Some(info) = txt_info(v) {
                        if self.services.len() < 64 || self.services.contains_key(&key) {
                            self.services.entry(key.clone()).or_default().2 = info;
                            touched.push(key);
                        }
                    }
                }
                Rd::A(a) => self.host_ip(&key, IpAddr::V4(*a)),
                Rd::Aaaa(a) => {
                    // link-local IPv6 needs a zone to be dialled; the sender's own address is used instead
                    if (a.segments()[0] & 0xffc0) != 0xfe80 {
                        self.host_ip(&key, IpAddr::V6(*a));
                    }
                }
                _ => {}
            }
        }
        if self.hosts.len() > 128 {
            self.hosts.clear();
        }
        touched.sort();
        touched.dedup();
        let mut out = vec![];
        for k in touched {
            let Some((port, host, info)) = self.services.get(&k) else {
                continue;
            };
            if *port == 0 || info.is_empty() {
                continue;
            }
            let Ok(mut b) = serde_json::from_str::<Beacon>(info) else {
                continue;
            };
            b.port = *port;
            if b.v != PROTO {
                continue;
            }
            // prefer the advertised IPv4 (reachable on this LAN), then the packet's source, then IPv6
            let ip = self
                .hosts
                .get(host)
                .and_then(|v| v.iter().find(|i| i.is_ipv4()).or_else(|| v.first()))
                .copied()
                .unwrap_or(from);
            let addr = match ip {
                IpAddr::V6(v6) => format!("[{v6}]:{port}"),
                v4 => format!("{v4}:{port}"),
            };
            out.push((b, addr));
        }
        out
    }

    fn host_ip(&mut self, host: &str, ip: IpAddr) {
        let v = self.hosts.entry(host.to_owned()).or_default();
        if !v.contains(&ip) && v.len() < 8 {
            v.push(ip);
        }
    }
}

// ───────────── sockets ─────────────

fn open() -> Option<(UdpSocket, Socket)> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).ok()?;
    s.set_reuse_address(true).ok()?;
    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    let _ = s.set_reuse_port(true);
    s.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT)).into())
        .ok()?;
    let _ = s.set_multicast_ttl_v4(255);
    let _ = s.set_multicast_loop_v4(true);
    let mut joined = false;
    for ip in &v4_interfaces() {
        joined |= s.join_multicast_v4(&GROUP, ip).is_ok();
    }
    if !joined {
        s.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED).ok()?;
    }
    let udp: UdpSocket = s.try_clone().ok()?.into();
    let _ = udp.set_read_timeout(Some(Duration::from_millis(250)));
    Some((udp, s))
}

fn v4_interfaces() -> Vec<Ipv4Addr> {
    local_addrs()
        .iter()
        .filter_map(|a| a.parse::<Ipv4Addr>().ok())
        .collect()
}

/// Sends `bytes` to the mDNS group out of every interface we know (multi-homed machines: Wi-Fi + VPN + virtual switches).
fn multicast(ctl: &Socket, udp: &UdpSocket, bytes: &[u8]) {
    let dst = SocketAddr::new(IpAddr::V4(GROUP), PORT);
    let ifs = v4_interfaces();
    if ifs.is_empty() {
        let _ = udp.send_to(bytes, dst);
        return;
    }
    for ip in ifs {
        let _ = ctl.set_multicast_if_v4(&ip);
        let _ = udp.send_to(bytes, dst);
    }
}

/// Publishes the beacon that `beacon` holds (kept up to date by the session) until `stop` is set; says goodbye when it ends.
pub fn advertise(beacon: Arc<Mutex<Option<Beacon>>>, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new()
        .name("lumen-collab-mdns-adv".into())
        .spawn(move || {
            let Some((udp, ctl)) = open() else { return };
            let mut buf = vec![0u8; MAX_PACKET];
            let mut last_tx = Instant::now() - Duration::from_secs(60);
            let mut last_ann = Instant::now() - Duration::from_secs(60);
            let mut last: Option<String> = None;
            let make = |ttl: u32| -> Option<(String, Vec<u8>)> {
                let b = beacon.lock().ok()?.clone()?;
                let mut compact = b.clone();
                compact.title = compact.title.chars().take(40).collect();
                let info = serde_json::to_string(&compact).ok()?;
                let inst = format!("lumen-{:x}", b.id);
                let mut ips: Vec<IpAddr> = local_addrs()
                    .iter()
                    .filter_map(|a| a.parse().ok())
                    .collect();
                ips.extend(
                    local_addrs6()
                        .iter()
                        .filter_map(|a| a.parse::<IpAddr>().ok()),
                );
                let pkt = build_response(&inst, b.port, &info, &ips, ttl);
                Some((inst, pkt))
            };
            while !stop.load(Ordering::Relaxed) {
                let mut asked = false;
                if let Ok((n, _)) = udp.recv_from(&mut buf) {
                    if let Some(pk) = parse(&buf[..n]) {
                        asked = pk.questions.iter().any(|(q, t)| {
                            let q = q.to_ascii_lowercase();
                            (q == SERVICE && (*t == T_PTR || *t == T_ANY))
                                || (q.starts_with("lumen-") && q.ends_with(SERVICE))
                        });
                    }
                }
                // answer a question (rate limited) or repeat ourselves now and then: Android's NSD caches records for a couple of minutes
                if (asked && last_tx.elapsed() > Duration::from_millis(300))
                    || last_ann.elapsed() > Duration::from_secs(30)
                {
                    if let Some((inst, pkt)) = make(120) {
                        multicast(&ctl, &udp, &pkt);
                        last = Some(inst);
                        last_tx = Instant::now();
                        last_ann = Instant::now();
                    }
                }
            }
            // goodbye (ttl 0) so browsers drop the entry at once instead of after two minutes
            if let (Some(inst), Some(b)) = (last, beacon.lock().ok().and_then(|g| g.clone())) {
                multicast(&ctl, &udp, &build_response(&inst, b.port, "", &[], 0));
            }
        });
}

/// Looks for `_lumen._tcp` hosts until `stop` is set; every complete one arrives as an `Event::Found` (same as a UDP beacon).
pub fn browse(inbox: Inbox, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new()
        .name("lumen-collab-mdns-browse".into())
        .spawn(move || {
            let Some((udp, ctl)) = open() else { return };
            let mut buf = vec![0u8; MAX_PACKET];
            let mut browser = Browser::default();
            let query = build_query(SERVICE, T_PTR);
            let mut last_q = Instant::now() - Duration::from_secs(10);
            while !stop.load(Ordering::Relaxed) {
                if last_q.elapsed() > Duration::from_millis(2500) {
                    multicast(&ctl, &udp, &query);
                    last_q = Instant::now();
                }
                if let Ok((n, from)) = udp.recv_from(&mut buf) {
                    if let Some(pk) = parse(&buf[..n]) {
                        for (b, addr) in browser.absorb(&pk, from.ip().to_canonical()) {
                            inbox.push(Event::Found(b, addr));
                        }
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beacon() -> Beacon {
        Beacon {
            v: PROTO,
            id: 0xabc123,
            device: "Laptop".into(),
            name: "Ada".into(),
            platform: "windows".into(),
            port: 41234,
            what: "Workspace".into(),
            title: "Grüße — notes 📝".into(),
            people: 2,
            iroh: "test_iroh_id".into(),
        }
    }

    #[test]
    fn a_response_survives_a_round_trip_and_completes_a_host() {
        let b = beacon();
        let info = serde_json::to_string(&b).unwrap();
        let ips: Vec<IpAddr> = vec![
            "192.168.1.20".parse().unwrap(),
            "2001:db8::7".parse().unwrap(),
            "fe80::1".parse().unwrap(),
        ];
        let pkt = build_response("lumen-abc123", b.port, &info, &ips, 120);
        let p = parse(&pkt).expect("parses");
        assert!(p.records.iter().any(|(n, _, r)| n == SERVICE
            && matches!(r, Rd::Ptr(x) if x == "lumen-abc123._lumen._tcp.local")));
        let mut br = Browser::default();
        let found = br.absorb(&p, "10.9.9.9".parse().unwrap());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, b);
        assert_eq!(
            found[0].1, "192.168.1.20:41234",
            "the advertised IPv4 wins over the packet source"
        );
    }

    #[test]
    fn a_goodbye_removes_the_service_and_the_source_address_is_the_fallback() {
        let b = beacon();
        let info = serde_json::to_string(&b).unwrap();
        let mut br = Browser::default();
        let hello = parse(&build_response("lumen-1", b.port, &info, &[], 120)).unwrap();
        assert_eq!(
            br.absorb(&hello, "10.1.2.3".parse().unwrap())[0].1,
            "10.1.2.3:41234"
        );
        let bye = parse(&build_response("lumen-1", b.port, "", &[], 0)).unwrap();
        assert!(br.absorb(&bye, "10.1.2.3".parse().unwrap()).is_empty());
        assert!(br.services.is_empty());
    }

    #[test]
    fn long_beacons_are_chunked_like_android_does_and_read_back() {
        let mut b = beacon();
        b.title = "x".repeat(60);
        b.device = "d".repeat(40);
        let info = serde_json::to_string(&b).unwrap();
        let chunks = txt_chunks(&info);
        assert!(chunks.iter().all(|c| c.len() <= 183));
        assert_eq!(txt_info(&chunks).unwrap(), info);
    }

    #[test]
    fn queries_parse_and_hostile_packets_never_panic() {
        let q = parse(&build_query(SERVICE, T_PTR)).unwrap();
        assert_eq!(q.questions, vec![(SERVICE.to_owned(), T_PTR)]);
        // a compression pointer that points at itself, truncated records, random noise
        let evil = vec![0, 0, 0x84, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0xC0, 12, 0, 12, 0, 1];
        assert!(parse(&evil).is_none());
        let mut seed = 0x2545F491u32;
        let good = build_response("lumen-x", 1, "{}", &["10.0.0.1".parse().unwrap()], 60);
        for i in 0..4000 {
            let mut v = good.clone();
            for _ in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let at = seed as usize % v.len();
                v[at] = (seed >> 8) as u8;
            }
            if i % 7 == 0 {
                v.truncate(seed as usize % v.len());
            }
            let _ = parse(&v).map(|p| Browser::default().absorb(&p, "10.0.0.2".parse().unwrap()));
        }
    }
}

/// Local IPv4 addresses used for mDNS advertisements.
pub(super) fn local_addrs() -> Vec<String> {
    let mut out = vec![];
    for probe in [
        "192.168.1.1:80",
        "10.0.0.1:80",
        "172.16.0.1:80",
        "8.8.8.8:80",
    ] {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            if s.connect(probe).is_ok() {
                if let Ok(a) = s.local_addr() {
                    let ip = a.ip().to_string();
                    if !a.ip().is_unspecified() && !out.contains(&ip) {
                        out.push(ip);
                    }
                }
            }
        }
    }
    out
}

/// Local global/ULA IPv6 addresses used for mDNS advertisements.
pub(super) fn local_addrs6() -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for probe in ["[2001:4860:4860::8888]:80", "[fd00::1]:80"] {
        if let Ok(s) = UdpSocket::bind("[::]:0") {
            if s.connect(probe).is_ok() {
                if let Ok(a) = s.local_addr() {
                    if let IpAddr::V6(v6) = a.ip() {
                        let seg = v6.segments()[0];
                        let usable =
                            !v6.is_unspecified() && !v6.is_loopback() && (seg & 0xffc0) != 0xfe80;
                        let ip = format!("[{v6}]");
                        if usable && !out.contains(&ip) {
                            out.push(ip);
                        }
                    }
                }
            }
        }
    }
    out
}
