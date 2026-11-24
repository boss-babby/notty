//! Ultra-lightweight on-demand LAN discovery over UDP broadcast (port 47616).
//!
//! Runs alongside mDNS (`mdns.rs` on desktop) and `NsdManager` (Android) so devices on the same
//! Wi-Fi discover each other within ~1–2 seconds even when Windows Firewall or Wi-Fi routers
//! filter mDNS multicast (`224.0.0.251:5353`).
//!
//! - **Zero background overhead:** `advertise()` only runs while actively hosting (`Mode::Hosting`),
//!   sending one small (<350 B) UDP datagram every 2 seconds. `browse()` only runs while the user
//!   has the **Nearby** tab open (`discover(true)`), and exits within 250 ms when closed.

use super::{Beacon, Event, Inbox, DISCOVERY_PORT};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAGIC: &[u8] = b"LMN1";

fn bind_broadcast_socket(bind_port: u16) -> Option<UdpSocket> {
    #[cfg(not(target_os = "android"))]
    {
        use socket2::{Domain, Protocol, Socket, Type};
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).ok()?;
        let _ = sock.set_reuse_address(true);
        let _ = sock.set_broadcast(true);
        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, bind_port);
        sock.bind(&addr.into()).ok()?;
        let udp: UdpSocket = sock.into();
        let _ = udp.set_broadcast(true);
        Some(udp)
    }
    #[cfg(target_os = "android")]
    {
        let udp = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, bind_port)).ok()?;
        let _ = udp.set_broadcast(true);
        Some(udp)
    }
}

fn broadcast_targets() -> Vec<SocketAddr> {
    let mut targets = vec![SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::BROADCAST,
        DISCOVERY_PORT,
    ))];
    // Also send to each local /24 subnet broadcast (e.g. 192.168.1.255) for routers/OSes that drop 255.255.255.255.
    for probe in [
        "192.168.1.1:80",
        "10.0.0.1:80",
        "172.16.0.1:80",
        "8.8.8.8:80",
    ] {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            if s.connect(probe).is_ok() {
                if let Ok(SocketAddr::V4(v4)) = s.local_addr() {
                    let [a, b, c, _] = v4.ip().octets();
                    let sub = SocketAddr::V4(SocketAddrV4::new(
                        Ipv4Addr::new(a, b, c, 255),
                        DISCOVERY_PORT,
                    ));
                    if !targets.contains(&sub) {
                        targets.push(sub);
                    }
                }
            }
        }
    }
    targets
}

/// Encodes a `Beacon` into a compact `LMN1<json>` UDP datagram.
pub fn encode_beacon(b: &Beacon) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(b).ok()?;
    if json.len() > 1400 {
        return None;
    }
    let mut pkt = Vec::with_capacity(MAGIC.len() + json.len());
    pkt.extend_from_slice(MAGIC);
    pkt.extend_from_slice(&json);
    Some(pkt)
}

/// Decodes a `LMN1<json>` UDP datagram back into a `Beacon`.
pub fn decode_beacon(pkt: &[u8]) -> Option<Beacon> {
    let body = pkt.strip_prefix(MAGIC)?;
    serde_json::from_slice(body).ok()
}

/// Broadcasts the current host `Beacon` every 2 s while `stop` is false (only while hosting).
pub fn advertise(beacon: Arc<Mutex<Option<Beacon>>>, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new()
        .name("lumen-lan-adv".into())
        .spawn(move || {
            let Ok(udp) = UdpSocket::bind("0.0.0.0:0") else {
                return;
            };
            let _ = udp.set_broadcast(true);
            let mut last_send = Instant::now() - Duration::from_secs(10);
            // The target list needs up to four throw-away sockets to learn the local subnet: build it once and refresh it every 20 s
            // (the phone may roam to another Wi-Fi) instead of on every 2 s beacon.
            let mut targets = broadcast_targets();
            let mut targets_at = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                if last_send.elapsed() >= Duration::from_secs(2) {
                    last_send = Instant::now();
                    if targets_at.elapsed() >= Duration::from_secs(20) {
                        targets = broadcast_targets();
                        targets_at = Instant::now();
                    }
                    if let Ok(Some(b)) = beacon.lock().map(|g| g.clone()) {
                        if let Some(pkt) = encode_beacon(&b) {
                            for dst in &targets {
                                let _ = udp.send_to(&pkt, dst);
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
}

/// Listens for host beacons on UDP port `47616` while `stop` is false (only while Nearby tab is open).
pub fn browse(inbox: Inbox, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new()
        .name("lumen-lan-scan".into())
        .spawn(move || {
            let Some(udp) = bind_broadcast_socket(DISCOVERY_PORT) else {
                log::warn!("LAN discovery: UDP port {DISCOVERY_PORT} is busy or blocked; relying on mDNS / NSD only");
                return;
            };
            let _ = udp.set_read_timeout(Some(Duration::from_millis(250)));
            let mut buf = [0u8; 2048];
            while !stop.load(Ordering::Relaxed) {
                if let Ok((n, from)) = udp.recv_from(&mut buf) {
                    if let Some(b) = decode_beacon(&buf[..n]) {
                        let addr = format!("{}:{}", from.ip().to_canonical(), b.port);
                        inbox.push(Event::Found(b, addr));
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::PROTO;

    #[test]
    fn broadcast_targets_always_include_the_limited_broadcast() {
        let t = broadcast_targets();
        assert!(t.contains(&SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::BROADCAST,
            DISCOVERY_PORT
        ))));
        let mut d = t.clone();
        d.dedup();
        assert_eq!(d.len(), t.len(), "no duplicate targets");
    }

    #[test]
    fn foreign_datagrams_are_ignored() {
        assert!(decode_beacon(b"GET / HTTP/1.1").is_none());
        assert!(decode_beacon(b"LMN1{not json").is_none());
    }

    #[test]
    fn lan_beacon_roundtrips_with_iroh_id() {
        let b = Beacon {
            v: PROTO,
            id: 42,
            device: "Laptop".into(),
            name: "Ada".into(),
            platform: "windows".into(),
            port: 47615,
            what: "Workspace".into(),
            title: "My Notes".into(),
            people: 1,
            iroh: "abcd1234iroh".into(),
        };
        let pkt = encode_beacon(&b).expect("encode");
        assert!(pkt.starts_with(MAGIC));
        let dec = decode_beacon(&pkt).expect("decode");
        assert_eq!(dec, b);
    }
}
