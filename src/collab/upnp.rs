//! Minimal UPnP Internet-Gateway-Device client (native only, no dependencies): asks the home router to forward the collaboration port so a
//! device on *another* network can dial this one directly ("direct internet connect"). Everything that crosses that port is still the
//! Noise-encrypted, token-authenticated, approval-gated protocol — the mapping only makes the TCP port reachable.
//!
//! Flow: SSDP `M-SEARCH` → `LOCATION` → device description XML → `controlURL` of `WANIPConnection` / `WANPPPConnection` → SOAP
//! `AddPortMapping` / `GetExternalIPAddress` / `DeletePortMapping`. Parsing is split into pure functions that are unit-tested.
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const SSDP: &str = "239.255.255.250:1900";

#[derive(Clone, Debug, PartialEq)]
pub struct Gateway {
    /// `host:port` of the router's control endpoint.
    pub host: String,
    pub path: String,
    /// Service URN, e.g. `urn:schemas-upnp-org:service:WANIPConnection:1`.
    pub service: String,
}

/// `LOCATION:` header of an SSDP answer.
pub fn parse_location(resp: &str) -> Option<String> {
    resp.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("location")
            .then(|| v.trim().to_owned())
    })
}

/// `http://host:port/path` → (`host:port`, `/path`).
pub fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (hp, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let hp = if hp.contains(':') {
        hp.to_owned()
    } else {
        format!("{hp}:80")
    };
    Some((hp, path.to_owned()))
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(s[a..b].trim())
}

/// (service URN, control URL) of the first WAN connection service in a device description.
pub fn parse_control(desc: &str) -> Option<(String, String)> {
    for seg in desc.split("<service>").skip(1) {
        let seg = seg.split("</service>").next().unwrap_or(seg);
        let Some(ty) = between(seg, "<serviceType>", "</serviceType>") else {
            continue;
        };
        if ty.contains("WANIPConnection") || ty.contains("WANPPPConnection") {
            if let Some(ctl) = between(seg, "<controlURL>", "</controlURL>") {
                return Some((ty.to_owned(), ctl.to_owned()));
            }
        }
    }
    None
}

fn http(hp: &str, request: &str, timeout: Duration) -> Option<String> {
    let sa = hp.to_socket_addrs().ok()?.next()?;
    let mut s = TcpStream::connect_timeout(&sa, timeout).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    s.write_all(request.as_bytes()).ok()?;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let t0 = Instant::now();
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
        if out.len() > 256 * 1024 || t0.elapsed() > timeout * 2 {
            break;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn soap_body(service: &str, action: &str, args: &[(&str, &str)]) -> String {
    let mut a = String::new();
    for (k, v) in args {
        a.push_str(&format!("<{k}>{v}</{k}>"));
    }
    format!("<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service}\">{a}</u:{action}></s:Body></s:Envelope>")
}

/// Finds the router (≈ 2 s at most).
pub fn discover() -> Option<Gateway> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    for st in [
        "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
        "urn:schemas-upnp-org:service:WANIPConnection:1",
        "upnp:rootdevice",
    ] {
        let msg = format!("M-SEARCH * HTTP/1.1\r\nHOST: {SSDP}\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {st}\r\n\r\n");
        let _ = sock.send_to(msg.as_bytes(), SSDP);
    }
    let t0 = Instant::now();
    let mut buf = [0u8; 2048];
    let mut tried: Vec<String> = vec![];
    while t0.elapsed() < Duration::from_millis(2200) {
        let Ok((n, _)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let resp = String::from_utf8_lossy(&buf[..n]).into_owned();
        let Some(loc) = parse_location(&resp) else {
            continue;
        };
        if tried.contains(&loc) {
            continue;
        }
        tried.push(loc.clone());
        let Some((hp, path)) = split_url(&loc) else {
            continue;
        };
        let Some(r) = http(
            &hp,
            &format!("GET {path} HTTP/1.1\r\nHost: {hp}\r\nConnection: close\r\n\r\n"),
            Duration::from_secs(2),
        ) else {
            continue;
        };
        if let Some((service, ctl)) = parse_control(&r) {
            let (host, path) = match split_url(&ctl) {
                Some(x) => x,
                None => (
                    hp.clone(),
                    if ctl.starts_with('/') {
                        ctl
                    } else {
                        format!("/{ctl}")
                    },
                ),
            };
            return Some(Gateway {
                host,
                path,
                service,
            });
        }
    }
    None
}

impl Gateway {
    fn call(&self, action: &str, args: &[(&str, &str)]) -> Option<String> {
        let body = soap_body(&self.service, action, args);
        let req = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nSOAPAction: \"{}#{}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.path,
            self.host,
            self.service,
            action,
            body.len(),
            body
        );
        let r = http(&self.host, &req, Duration::from_secs(3))?;
        (r.starts_with("HTTP/1.1 200") || r.starts_with("HTTP/1.0 200")).then_some(r)
    }

    /// The address of this device that faces the router (the mapping's internal client).
    pub fn local_ip(&self) -> Option<Ipv4Addr> {
        let sa = self.host.to_socket_addrs().ok()?.next()?;
        let s = UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect(sa).ok()?;
        match s.local_addr().ok()?.ip() {
            IpAddr::V4(v4) => Some(v4),
            _ => None,
        }
    }

    /// Forwards TCP `external` → this device's `internal` port. Some routers refuse timed leases (or a busy external port): fall back.
    pub fn add(&self, internal: u16, external: u16) -> Result<u16, String> {
        let ip = self.local_ip().ok_or("no local address")?.to_string();
        let mut last = String::from("the router refused the mapping");
        for (ext, lease) in [
            (external, "7200"),
            (external, "0"),
            (external.wrapping_add(1).max(1025), "0"),
        ] {
            let e = ext.to_string();
            let i = internal.to_string();
            let args = [
                ("NewRemoteHost", ""),
                ("NewExternalPort", e.as_str()),
                ("NewProtocol", "TCP"),
                ("NewInternalPort", i.as_str()),
                ("NewInternalClient", ip.as_str()),
                ("NewEnabled", "1"),
                ("NewPortMappingDescription", "Lumen collaboration"),
                ("NewLeaseDuration", lease),
            ];
            if self.call("AddPortMapping", &args).is_some() {
                return Ok(ext);
            }
            last = format!("the router refused port {ext}");
        }
        Err(last)
    }

    pub fn external_ip(&self) -> Option<String> {
        let r = self.call("GetExternalIPAddress", &[])?;
        between(&r, "<NewExternalIPAddress>", "</NewExternalIPAddress>")
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    }

    pub fn remove(&self, external: u16) {
        let e = external.to_string();
        let _ = self.call(
            "DeletePortMapping",
            &[
                ("NewRemoteHost", ""),
                ("NewExternalPort", e.as_str()),
                ("NewProtocol", "TCP"),
            ],
        );
    }
}

/// `true` for addresses that are not reachable from the internet (private, loopback, link-local, carrier-grade NAT 100.64/10).
pub fn is_private_v4(ip: &str) -> bool {
    match ip.parse::<Ipv4Addr>() {
        Ok(a) => {
            let o = a.octets();
            a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_unspecified()
                || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        Err(_) => true,
    }
}

#[allow(dead_code)]
fn _assert_sa(_: SocketAddr) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssdp_location_and_description() {
        let resp = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nLocation: http://192.168.1.1:5000/rootDesc.xml\r\nST: upnp:rootdevice\r\n\r\n";
        assert_eq!(
            parse_location(resp).as_deref(),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
        assert_eq!(
            split_url("http://192.168.1.1:5000/rootDesc.xml"),
            Some(("192.168.1.1:5000".into(), "/rootDesc.xml".into()))
        );
        assert_eq!(
            split_url("http://router/x"),
            Some(("router:80".into(), "/x".into()))
        );
        let desc = "<root><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><controlURL>/l3f</controlURL></service>\
            <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><controlURL>/ctl/IPConn</controlURL></service></serviceList></device></root>";
        assert_eq!(
            parse_control(desc),
            Some((
                "urn:schemas-upnp-org:service:WANIPConnection:1".into(),
                "/ctl/IPConn".into()
            ))
        );
        assert!(soap_body("urn:x", "AddPortMapping", &[("NewEnabled", "1")])
            .contains("<u:AddPortMapping xmlns:u=\"urn:x\"><NewEnabled>1</NewEnabled>"));
    }

    #[test]
    fn private_and_cgnat_addresses_are_not_public() {
        for ip in [
            "192.168.0.4",
            "10.1.2.3",
            "172.16.5.5",
            "100.64.0.9",
            "100.127.255.1",
            "127.0.0.1",
            "169.254.1.1",
            "nonsense",
        ] {
            assert!(is_private_v4(ip), "{ip}");
        }
        for ip in ["8.8.8.8", "203.0.113.9", "100.63.0.1", "100.128.0.1"] {
            assert!(!is_private_v4(ip), "{ip}");
        }
    }
}
