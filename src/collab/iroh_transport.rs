//! Unified QUIC P2P + relay transport powered by `iroh` 1.x.
//!
//! Replaces the six overlapping custom networking layers (UPnP, STUN/UDP hole punch, TURN,
//! BitTorrent DHT, HTTP long-poll relay, and raw WebSocket/WebRTC) with a single authenticated,
//! TLS 1.3-encrypted QUIC transport behind Lumen's existing [`Inbox`] and [`Sink`] contract.
//!
//! - **Native & Android (`not(wasm32)`)**: spawns one lightweight background OS thread running a
//!   current-thread Tokio runtime only while a session is active, shutting it down on `stop()`.
//! - **WebAssembly (`wasm32`)**: drives the exact same `iroh::Endpoint` via
//!   `wasm_bindgen_futures::spawn_local`, enabling browsers to both host and join via tickets.

use super::{ConnId, Event, Inbox, Msg, Sink};
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Application-Layer Protocol Negotiation identifier for Lumen Iroh sessions.
pub const ALPN: &[u8] = b"lumen/collab/9";

/// Hard ceiling on any single framed `Msg` (4 MiB) to guard against memory exhaustion.
const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Compress JSON payloads larger than this threshold using `miniz_oxide` deflate.
const COMPRESS_THRESHOLD: usize = 400;

/// Connection path classification exposed to the Notion-style status badges in the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum PathKind {
    #[default]
    Unknown = 0,
    DirectLan = 1,
    DirectP2p = 2,
    Relayed = 3,
}

impl PathKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::DirectLan,
            2 => Self::DirectP2p,
            3 => Self::Relayed,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::DirectLan => "Direct LAN",
            Self::DirectP2p => "Direct P2P",
            Self::Relayed => "Relayed",
            Self::Unknown => "Connecting",
        }
    }
}

/// Derives a deterministic `iroh::SecretKey` from a 32-byte device seed.
pub fn secret_key_from_seed(seed: [u8; 32]) -> SecretKey {
    SecretKey::from_bytes(&seed)
}

/// Parses either a full `EndpointTicket` (`endpoint...`) or a bare `EndpointId`.
pub fn parse_endpoint_addr(s: &str) -> Result<EndpointAddr, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("Empty Iroh ticket".into());
    }
    if let Ok(ticket) = EndpointTicket::from_str(trimmed) {
        return Ok(ticket.endpoint_addr().clone());
    }
    if let Ok(id) = EndpointId::from_str(trimmed) {
        return Ok(EndpointAddr::new(id));
    }
    Err("Not a valid Iroh ticket or endpoint ID".into())
}

/// Builds a compact `EndpointTicket` for a standby successor carrying its deterministic `EndpointId`
/// plus optional direct IPv4/IPv6 socket addresses (so failover works both offline on LAN and over the internet).
pub fn standby_ticket(seed: [u8; 32], direct_addrs: &[std::net::SocketAddr]) -> String {
    let id = secret_key_from_seed(seed).public();
    let mut addr = EndpointAddr::new(id);
    for &sa in direct_addrs {
        addr.addrs.insert(iroh::TransportAddr::Ip(sa));
    }
    EndpointTicket::new(addr).to_string()
}

/// Builds a `RelayMode` from an optional user-specified custom relay URL.
/// Empty or blank input uses n0's free default public relay network.
pub fn build_relay_mode(custom_relay: Option<&str>) -> Result<RelayMode, String> {
    let Some(raw) = custom_relay.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(RelayMode::Default);
    };
    let url = RelayUrl::from_str(raw).map_err(|e| format!("Invalid relay URL: {e}"))?;
    Ok(RelayMode::Custom(RelayMap::from(url)))
}

/// Encodes a `Msg` into a length-prefixed frame: `[u32 LE body_len][u8 flag][payload]`.
/// Flag `0x00` = raw UTF-8 JSON, `0x01` = deflate-compressed UTF-8 JSON.
pub fn encode_frame(m: &Msg) -> Option<Vec<u8>> {
    let json = m.to_text()?;
    let raw = json.as_bytes();
    if raw.len() + 1 > MAX_FRAME {
        return None;
    }
    let (flag, payload): (u8, Vec<u8>) = if raw.len() > COMPRESS_THRESHOLD {
        let comp = miniz_oxide::deflate::compress_to_vec(raw, 6);
        if comp.len() < raw.len() {
            (1u8, comp)
        } else {
            (0u8, raw.to_vec())
        }
    } else {
        (0u8, raw.to_vec())
    };
    let body_len = (1 + payload.len()) as u32;
    let mut out = Vec::with_capacity(4 + body_len as usize);
    out.extend_from_slice(&body_len.to_le_bytes());
    out.push(flag);
    out.extend_from_slice(&payload);
    Some(out)
}

/// Decodes a framed payload (`[u8 flag][payload]`) back into a `Msg`.
pub fn decode_frame_body(body: &[u8]) -> Option<Msg> {
    let (&flag, payload) = body.split_first()?;
    let raw = match flag {
        0 => payload.to_vec(),
        1 => miniz_oxide::inflate::decompress_to_vec_with_limit(payload, MAX_FRAME).ok()?,
        _ => return None,
    };
    let text = std::str::from_utf8(&raw).ok()?;
    Msg::from_text(text)
}

async fn write_msg(send: &mut SendStream, m: &Msg) -> Result<(), String> {
    let Some(frame) = encode_frame(m) else {
        return Err("Frame exceeds MAX_FRAME".into());
    };
    send.write_all(&frame).await.map_err(|e| e.to_string())
}

async fn read_msg(recv: &mut RecvStream) -> Result<Option<Msg>, String> {
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(_)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    let body_len = u32::from_le_bytes(len_buf) as usize;
    if body_len == 0 || body_len > MAX_FRAME {
        return Err(format!("Frame out of bounds ({body_len} bytes)"));
    }
    let mut body = vec![0u8; body_len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(decode_frame_body(&body))
}

fn classify_conn_path(conn: &Connection) -> PathKind {
    let paths = conn.paths();
    let mut best = PathKind::Unknown;
    for p in paths.iter() {
        if let iroh::TransportAddr::Ip(ip_addr) = p.remote_addr() {
            let is_lan = match ip_addr.ip() {
                std::net::IpAddr::V4(v4) => {
                    v4.is_private() || v4.is_loopback() || v4.is_link_local()
                }
                std::net::IpAddr::V6(v6) => {
                    v6.is_loopback()
                        || (v6.segments()[0] & 0xfe00) == 0xfc00
                        || (v6.segments()[0] & 0xffc0) == 0xfe80
                }
            };
            if is_lan {
                return PathKind::DirectLan;
            }
            best = PathKind::DirectP2p;
        } else if p.is_relay() && best == PathKind::Unknown {
            best = PathKind::Relayed;
        }
    }
    best
}

/// Sink implementation backed by an async QUIC bidirectional stream.
pub struct IrohSink {
    tx: mpsc::UnboundedSender<Option<Msg>>,
    path: Arc<AtomicU8>,
}

impl Sink for IrohSink {
    fn send(&self, m: &Msg) {
        let _ = self.tx.send(Some(m.clone()));
    }

    fn close(&self) {
        let _ = self.tx.send(None);
    }

    fn path_kind(&self) -> PathKind {
        PathKind::from_u8(self.path.load(Ordering::Relaxed))
    }
}

/// Drives one accepted or dialed QUIC connection and bridges it into `Inbox`.
async fn drive_connection(conn: Connection, incoming: bool, inbox: Inbox, stop: Arc<AtomicBool>) {
    let remote = conn.remote_id().to_string();
    let cid: ConnId = inbox.conn_id();
    let (mut send, mut recv) = if incoming {
        match conn.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return,
        }
    } else {
        match conn.open_bi().await {
            Ok(pair) => pair,
            Err(e) => {
                inbox.push(Event::Closed {
                    conn: cid,
                    why: format!("Could not open stream: {e}"),
                });
                return;
            }
        }
    };

    let path = Arc::new(AtomicU8::new(classify_conn_path(&conn) as u8));
    let (tx, mut rx) = mpsc::unbounded_channel::<Option<Msg>>();
    let sink = Box::new(IrohSink {
        tx,
        path: path.clone(),
    });
    inbox.push(Event::Open {
        conn: cid,
        sink,
        addr: format!("iroh:{remote}"),
        incoming,
    });

    let writer_stop = stop.clone();
    let writer_conn = conn.clone();
    let writer_fut = async move {
        while let Some(item) = rx.recv().await {
            match item {
                Some(msg) => {
                    if write_msg(&mut send, &msg).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = send.finish();
                    sleep_ms(80).await;
                    writer_conn.close(0u32.into(), b"bye");
                    break;
                }
            }
            if writer_stop.load(Ordering::Relaxed) && rx.is_empty() {
                let _ = send.finish();
                sleep_ms(60).await;
                writer_conn.close(0u32.into(), b"bye");
                break;
            }
        }
    };

    let reader_path = path.clone();
    let reader_conn = conn.clone();
    let reader_inbox = inbox.clone();
    let reader_stop = stop.clone();
    let reader_fut = async move {
        loop {
            if reader_stop.load(Ordering::Relaxed) {
                return "stopped".to_owned();
            }
            // Enforce the 4,096-event back-pressure ceiling before pulling more frames from QUIC.
            while reader_inbox.pending() >= 4096 {
                if reader_stop.load(Ordering::Relaxed) {
                    return "stopped".to_owned();
                }
                sleep_ms(10).await;
            }
            let pk = classify_conn_path(&reader_conn);
            reader_path.store(pk as u8, Ordering::Relaxed);
            match read_msg(&mut recv).await {
                Ok(Some(msg)) => {
                    reader_inbox.push(Event::Msg { conn: cid, msg });
                }
                Ok(None) => return "closed the connection".to_owned(),
                Err(e) => return format!("Connection lost ({e})"),
            }
        }
    };

    // Drive reader and writer concurrently; when either finishes, close the connection.
    #[cfg(not(target_arch = "wasm32"))]
    let why = {
        let w = tokio::spawn(writer_fut);
        let why = reader_fut.await;
        w.abort();
        why
    };

    #[cfg(target_arch = "wasm32")]
    let why = {
        wasm_bindgen_futures::spawn_local(writer_fut);
        reader_fut.await
    };

    conn.close(0u32.into(), b"done");
    inbox.push(Event::Closed { conn: cid, why });
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u64) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Manages the lifecycle of the active `iroh::Endpoint` for hosting or joining.
#[derive(Default)]
pub struct IrohNet {
    stop: Option<Arc<AtomicBool>>,
    prev_stop: Option<Arc<AtomicBool>>,
    ticket: Arc<Mutex<Option<String>>>,
    endpoint_id: Arc<Mutex<Option<String>>>,
    status: Arc<Mutex<String>>,
}

impl IrohNet {
    /// Current shareable `EndpointTicket` string (updates automatically once online).
    pub fn ticket(&self) -> Option<String> {
        self.ticket.lock().ok().and_then(|g| g.clone())
    }

    /// Current 32-byte Ed25519 `EndpointId` string.
    pub fn endpoint_id(&self) -> Option<String> {
        self.endpoint_id.lock().ok().and_then(|g| g.clone())
    }

    /// Human-readable status note (e.g. `"Ready"`, `"Connecting to relay…"`, or bind error).
    pub fn status(&self) -> String {
        self.status.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Stops any active endpoint and tears down its background runtime cleanly.
    pub fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        if let Some(s) = self.prev_stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        if let Ok(mut g) = self.ticket.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.endpoint_id.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.status.lock() {
            g.clear();
        }
    }

    /// Starts hosting on a new `iroh::Endpoint` bound with the device's deterministic key.
    pub fn start_host(
        &mut self,
        seed: [u8; 32],
        custom_relay: Option<String>,
        keep_existing: bool,
        bind_port: Option<u16>,
        inbox: Inbox,
        ctx: egui::Context,
    ) {
        if keep_existing {
            if let Some(s) = self.prev_stop.take() {
                s.store(true, Ordering::Relaxed);
            }
            self.prev_stop = self.stop.take();
            if let Ok(mut g) = self.ticket.lock() {
                *g = None;
            }
        } else {
            self.stop();
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(stop.clone());
        let ticket_slot = self.ticket.clone();
        let id_slot = self.endpoint_id.clone();
        if let Ok(mut g) = id_slot.lock() {
            *g = Some(secret_key_from_seed(seed).public().to_string());
        }
        let status_slot = self.status.clone();
        if let Ok(mut g) = status_slot.lock() {
            *g = "Starting Iroh endpoint…".into();
        }

        let fut = async move {
            let relay_mode = match build_relay_mode(custom_relay.as_deref()) {
                Ok(m) => m,
                Err(e) => {
                    if let Ok(mut g) = status_slot.lock() {
                        *g = e;
                    }
                    ctx.request_repaint();
                    return;
                }
            };
            let mut builder = Endpoint::builder(presets::N0)
                .secret_key(secret_key_from_seed(seed))
                .alpns(vec![ALPN.to_vec()])
                .relay_mode(relay_mode);
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(port) = bind_port.filter(|p| *p != 0) {
                let addr = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                    std::net::Ipv4Addr::UNSPECIFIED,
                    port,
                ));
                if let Ok(b) = builder.bind_addr(addr) {
                    builder = b;
                } else {
                    builder = Endpoint::builder(presets::N0)
                        .secret_key(secret_key_from_seed(seed))
                        .alpns(vec![ALPN.to_vec()])
                        .relay_mode(
                            build_relay_mode(custom_relay.as_deref()).unwrap_or(RelayMode::Default),
                        );
                }
            }
            let _ = bind_port;
            let ep = match builder.bind().await {
                Ok(ep) => ep,
                Err(e) => {
                    if let Ok(mut g) = status_slot.lock() {
                        *g = format!("Could not start endpoint: {e}");
                    }
                    ctx.request_repaint();
                    return;
                }
            };

            let eid = ep.id().to_string();
            if let Ok(mut g) = id_slot.lock() {
                *g = Some(eid);
            }
            // Publish initial ticket immediately (with local addresses), then refresh once online.
            if let Ok(mut g) = ticket_slot.lock() {
                *g = Some(EndpointTicket::new(ep.addr()).to_string());
            }
            if let Ok(mut g) = status_slot.lock() {
                *g = "Ready (local · connecting relay…)".into();
            }
            ctx.request_repaint();

            // Background task waits for online() so the ticket gains the home relay URL without blocking accept().
            let ep_online = ep.clone();
            let ticket_online = ticket_slot.clone();
            let status_online = status_slot.clone();
            let ctx_online = ctx.clone();
            let stop_online = stop.clone();
            let online_task = async move {
                ep_online.online().await;
                if stop_online.load(Ordering::Relaxed) {
                    return;
                }
                let t = EndpointTicket::new(ep_online.addr()).to_string();
                if let Ok(mut g) = ticket_online.lock() {
                    *g = Some(t);
                }
                if let Ok(mut g) = status_online.lock() {
                    *g = "Ready (LAN + Internet)".into();
                }
                ctx_online.request_repaint();
            };

            #[cfg(not(target_arch = "wasm32"))]
            tokio::spawn(online_task);
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(online_task);

            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                #[cfg(not(target_arch = "wasm32"))]
                let incoming = tokio::select! {
                    inc = ep.accept() => inc,
                    _ = async {
                        while !stop.load(Ordering::Relaxed) {
                            sleep_ms(120).await;
                        }
                    } => None,
                };
                #[cfg(target_arch = "wasm32")]
                let incoming = ep.accept().await;

                let Some(inc) = incoming else { break };
                let inbox2 = inbox.clone();
                let stop2 = stop.clone();
                let conn_task = async move {
                    if let Ok(conn) = inc.await {
                        drive_connection(conn, true, inbox2, stop2).await;
                    }
                };
                #[cfg(not(target_arch = "wasm32"))]
                tokio::spawn(conn_task);
                #[cfg(target_arch = "wasm32")]
                wasm_bindgen_futures::spawn_local(conn_task);
            }
            sleep_ms(120).await;
            ep.close().await;
        };

        spawn_runtime("lumen-iroh-host", fut);
    }

    /// Dials a host by its `EndpointTicket` or bare `EndpointId`.
    pub fn dial(
        &mut self,
        seed: [u8; 32],
        ticket_or_id: String,
        custom_relay: Option<String>,
        inbox: Inbox,
        ctx: egui::Context,
    ) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(stop.clone());
        let status_slot = self.status.clone();
        if let Ok(mut g) = status_slot.lock() {
            *g = "Connecting via Iroh…".into();
        }

        let fut = async move {
            let addr = match parse_endpoint_addr(&ticket_or_id) {
                Ok(a) => a,
                Err(e) => {
                    inbox.push(Event::Closed {
                        conn: inbox.conn_id(),
                        why: e,
                    });
                    return;
                }
            };
            let relay_mode = match build_relay_mode(custom_relay.as_deref()) {
                Ok(m) => m,
                Err(e) => {
                    inbox.push(Event::Closed {
                        conn: inbox.conn_id(),
                        why: e,
                    });
                    return;
                }
            };
            let ep = match Endpoint::builder(presets::N0)
                .secret_key(secret_key_from_seed(seed))
                .relay_mode(relay_mode)
                .bind()
                .await
            {
                Ok(ep) => ep,
                Err(e) => {
                    inbox.push(Event::Closed {
                        conn: inbox.conn_id(),
                        why: format!("Could not start endpoint: {e}"),
                    });
                    return;
                }
            };

            let conn = match ep.connect(addr, ALPN).await {
                Ok(c) => c,
                Err(e) => {
                    ep.close().await;
                    inbox.push(Event::Closed {
                        conn: inbox.conn_id(),
                        why: format!("Could not reach host: {e}"),
                    });
                    return;
                }
            };

            if let Ok(mut g) = status_slot.lock() {
                *g = "Connected".into();
            }
            ctx.request_repaint();
            drive_connection(conn, false, inbox, stop).await;
            ep.close().await;
        };

        spawn_runtime("lumen-iroh-dial", fut);
    }
}

impl Drop for IrohNet {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_runtime<F>(name: &str, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(fut);
            }
        });
}

#[cfg(target_arch = "wasm32")]
fn spawn_runtime<F>(_name: &str, fut: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(fut);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_raw_and_compressed() {
        let small = Msg::Bye;
        let enc = encode_frame(&small).expect("encode small");
        assert_eq!(enc[4], 0, "small frame stays raw");
        let dec = decode_frame_body(&enc[4..]).expect("decode small");
        assert!(matches!(dec, Msg::Bye));

        // Large payload > 400 bytes should deflate cleanly
        let big = Msg::Denied {
            reason: "Lumen Iroh Sync Payload ".repeat(60),
        };
        let enc_big = encode_frame(&big).expect("encode big");
        assert_eq!(enc_big[4], 1, "large frame uses deflate compression");
        let dec_big = decode_frame_body(&enc_big[4..]).expect("decode big");
        match dec_big {
            Msg::Denied { reason } => {
                assert_eq!(reason.len(), "Lumen Iroh Sync Payload ".len() * 60);
            }
            _ => panic!("expected Denied"),
        }
    }

    #[test]
    fn two_iroh_endpoints_sync_frames_live() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");

        rt.block_on(async {
            let host = Endpoint::builder(presets::Minimal)
                .alpns(vec![ALPN.to_vec()])
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("bind host");
            let guest = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("bind guest");

            let ticket_str = EndpointTicket::new(host.addr()).to_string();
            let parsed = parse_endpoint_addr(&ticket_str).expect("parse ticket");
            assert_eq!(parsed.id, host.id());

            let server = tokio::spawn(async move {
                let inc = host.accept().await.expect("accept incoming");
                let conn = inc.await.expect("handshake");
                let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");
                let m = read_msg(&mut recv).await.expect("read").expect("msg");
                write_msg(&mut send, &m).await.expect("echo");
                let _ = send.finish();
                let _ = conn.closed().await;
                host.close().await;
            });

            let conn = guest.connect(parsed, ALPN).await.expect("connect");
            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
            let payload = Msg::Denied {
                reason: "hello-iroh-quic-".repeat(80),
            };
            write_msg(&mut send, &payload).await.expect("send");
            let echoed = read_msg(&mut recv).await.expect("recv").expect("echoed");
            match echoed {
                Msg::Denied { reason } => {
                    assert_eq!(reason, "hello-iroh-quic-".repeat(80));
                }
                other => panic!("unexpected echoed msg: {other:?}"),
            }
            conn.close(0u32.into(), b"ok");
            guest.close().await;
            let _ = server.await;
        });
    }
}
