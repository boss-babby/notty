//! Native transport: WebSocket over TCP (so browsers can connect to a desktop / phone host too) and UDP discovery.
//!
//! One small thread per connection; the UI thread only ever touches an `mpsc` sender and the shared [`Inbox`], so sockets can
//! never block a frame. Discovery: the host broadcasts a [`Beacon`] every 2 s to the limited broadcast address, the /24 broadcast
//! of its own address and a site-local multicast group; a device that is looking binds the discovery port while the dialog is open.
use super::*;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 76, 77);

struct WsSink {
    tx: Sender<Option<Msg>>,
}

impl Sink for WsSink {
    fn send(&self, m: &Msg) {
        let _ = self.tx.send(Some(m.clone()));
    }
    fn close(&self) {
        let _ = self.tx.send(None);
    }
}

#[derive(Default)]
pub struct Net {
    stop: Option<Arc<AtomicBool>>,
    disc_stop: Option<Arc<AtomicBool>>,
    pub port: u16,
}

/// Addresses other devices can reach this one on (private IPv4 first).
pub fn local_addrs() -> Vec<String> {
    let mut out = vec![];
    for probe in ["192.168.1.1:80", "10.0.0.1:80", "172.16.0.1:80", "8.8.8.8:80"] {
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

fn pump(mut ws: WebSocket<TcpStream>, conn: ConnId, inbox: Inbox, rx: Receiver<Option<Msg>>, stop: Arc<AtomicBool>) {
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(15)));
    let _ = ws.get_ref().set_nodelay(true);
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let why: &str;
    'outer: loop {
        if stop.load(Ordering::Relaxed) {
            why = "stopped";
            break;
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                last_rx = Instant::now();
                if let Ok(m) = serde_json::from_str::<Msg>(t.as_str()) {
                    inbox.push(Event::Msg { conn, msg: m });
                }
            }
            Ok(Message::Close(_)) => {
                why = "The other device closed the connection";
                break;
            }
            Ok(_) => last_rx = Instant::now(),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => {
                why = "Connection lost";
                break;
            }
        }
        loop {
            match rx.try_recv() {
                Ok(Some(m)) => {
                    let Ok(s) = serde_json::to_string(&m) else { continue };
                    if ws.send(Message::text(s)).is_err() {
                        why = "Connection lost";
                        break 'outer;
                    }
                }
                Ok(None) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    why = "closed";
                    break 'outer;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    why = "closed";
                    break 'outer;
                }
            }
        }
        if last_ping.elapsed() > Duration::from_secs(5) {
            last_ping = Instant::now();
            let _ = ws.send(Message::Ping(Default::default()));
        }
        if last_rx.elapsed() > Duration::from_secs(25) {
            why = "The connection timed out";
            break;
        }
    }
    inbox.push(Event::Closed { conn, why: why.to_owned() });
}

impl Net {
    pub fn listen(&mut self, inbox: &Inbox) -> Result<u16, String> {
        self.stop();
        let l = TcpListener::bind(("0.0.0.0", PORT)).or_else(|_| TcpListener::bind(("0.0.0.0", 0))).map_err(|e| e.to_string())?;
        let port = l.local_addr().map_err(|e| e.to_string())?.port();
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(stop.clone());
        self.port = port;
        let inbox = inbox.clone();
        std::thread::Builder::new()
            .name("lumen-collab-accept".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match l.accept() {
                        Ok((stream, peer)) => {
                            let inbox = inbox.clone();
                            let stop = stop.clone();
                            let _ = std::thread::Builder::new().name("lumen-collab-conn".into()).spawn(move || {
                                let _ = stream.set_nonblocking(false);
                                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                                let Ok(ws) = tungstenite::accept(stream) else { return };
                                let (tx, rx) = channel();
                                let conn = inbox.conn_id();
                                inbox.push(Event::Open { conn, sink: Box::new(WsSink { tx }), addr: peer.ip().to_string(), incoming: true });
                                pump(ws, conn, inbox, rx, stop);
                            });
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(60)),
                        Err(_) => std::thread::sleep(Duration::from_millis(250)),
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(port)
    }

    pub fn connect(&mut self, addr: &str, inbox: &Inbox) {
        let stop = self.stop.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();
        let inbox = inbox.clone();
        let addr = addr.to_owned();
        let _ = std::thread::Builder::new().name("lumen-collab-dial".into()).spawn(move || {
            let conn = inbox.conn_id();
            let fail = |why: &str| inbox.push(Event::Closed { conn, why: why.to_owned() });
            let Some(sa) = addr.to_socket_addrs().ok().and_then(|mut a| a.next()) else { return fail("That address is not valid") };
            let Ok(stream) = TcpStream::connect_timeout(&sa, Duration::from_secs(4)) else { return fail("Could not reach that device — are you on the same Wi-Fi?") };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let Ok((ws, _)) = tungstenite::client(format!("ws://{addr}/"), stream) else { return fail("That device did not answer as a Lumen host") };
            let (tx, rx) = channel();
            inbox.push(Event::Open { conn, sink: Box::new(WsSink { tx }), addr: sa.ip().to_string(), incoming: false });
            pump(ws, conn, inbox, rx, stop);
        });
    }

    pub fn announce(&mut self, beacon: Arc<Mutex<Option<Beacon>>>) {
        let stop = self.stop.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();
        let _ = std::thread::Builder::new().name("lumen-collab-beacon".into()).spawn(move || {
            let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
            let _ = sock.set_broadcast(true);
            let _ = sock.set_multicast_ttl_v4(2);
            while !stop.load(Ordering::Relaxed) {
                let b = beacon.lock().ok().and_then(|g| g.clone());
                if let Some(b) = b {
                    if let Ok(bytes) = serde_json::to_vec(&b) {
                        let mut targets: Vec<SocketAddr> = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), DISCOVERY_PORT), SocketAddr::new(IpAddr::V4(GROUP), DISCOVERY_PORT)];
                        for ip in local_addrs() {
                            if let Ok(IpAddr::V4(v4)) = ip.parse::<IpAddr>() {
                                let o = v4.octets();
                                targets.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], 255)), DISCOVERY_PORT));
                            }
                        }
                        for t in targets {
                            let _ = sock.send_to(&bytes, t);
                        }
                    }
                }
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }

    pub fn discover(&mut self, inbox: &Inbox) {
        if self.disc_stop.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.disc_stop = Some(stop.clone());
        let inbox = inbox.clone();
        let _ = std::thread::Builder::new().name("lumen-collab-discover".into()).spawn(move || {
            let Ok(sock) = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) else { return };
            let _ = sock.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED);
            let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));
            let mut buf = vec![0u8; 2048];
            while !stop.load(Ordering::Relaxed) {
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    if let Ok(b) = serde_json::from_slice::<Beacon>(&buf[..n]) {
                        if b.v == PROTO {
                            let ip = format!("{}:{}", from.ip(), b.port);
                            inbox.push(Event::Found(b, ip));
                        }
                    }
                }
            }
        });
    }

    pub fn stop_discovery(&mut self) {
        if let Some(s) = self.disc_stop.take() {
            s.store(true, Ordering::Relaxed);
        }
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
    }
}
