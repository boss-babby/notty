//! Browser ⇄ browser transport: a WebRTC data channel (peer-to-peer, no relay round trips once it is open).
//!
//! The browser has no sockets; until now two browsers talked through the site's HTTP long-poll relay. A host *in a browser* now also
//! answers offers at `/api/collab/rtc` (signalling only — an SDP offer / answer per joiner, 60 s TTL, no session data), and a joiner whose
//! link says `r=1` first tries to open a data channel; if that does not work within ~12 s (no WebRTC, a strict firewall, a native host) the
//! relay takes over exactly as before (`PunchJoin` is the same fall-back machinery as for native hole punching).
//!
//! The channel is just another `Sink`: encrypted frames go as binary messages `[more flag][Noise message]` (the format of the direct
//! WebSocket), other messages as JSON text — so the handshake, the share token proof and the host's approval are unchanged. The ICE
//! agent uses public STUN only (no TURN): peers behind symmetric NATs fall back to the relay.
//!
//! The RTCPeerConnection plumbing is a few dozen lines of JavaScript (`inline_js`) instead of a long list of `web-sys` features.
use super::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen(inline_js = r#"
const ICE = { iceServers: [{ urls: ['stun:stun.l.google.com:19302', 'stun:stun.cloudflare.com:3478'] }] };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const enc = encodeURIComponent;
const api = (base, q, body) =>
  fetch(base.replace(/\/+$/, '') + '/api/collab/rtc' + q, body ? { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) } : { cache: 'no-store' })
    .then((r) => (r.ok ? r.json() : null), () => null);
const gathered = (pc) =>
  new Promise((res) => {
    if (pc.iceGatheringState === 'complete') return res();
    const t = setTimeout(res, 3500);
    pc.addEventListener('icegatheringstatechange', () => { if (pc.iceGatheringState === 'complete') { clearTimeout(t); res(); } });
  });
function wire(dc, open, msg, close, opened) {
  dc.binaryType = 'arraybuffer';
  let conn = null;
  const go = () => {
    if (conn !== null) return;
    conn = open((d) => { if (dc.readyState === 'open') dc.send(d); }, () => { try { dc.close(); } catch (e) {} });
    if (opened) opened();
  };
  dc.onopen = go;
  if (dc.readyState === 'open') go();
  dc.onmessage = (e) => { if (conn !== null) msg(conn, e.data); };
  dc.onclose = () => { if (conn !== null) close(conn); };
}
const hex = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, '0')).join('');

export async function lumen_rtc_dial(base, room, open, msg, close) {
  try {
    if (typeof RTCPeerConnection === 'undefined') return false;
    const cap = await api(base, '?room=' + enc(room) + '&cap=1');
    if (!cap || !cap.rtc) return false;
    const c = hex();
    const pc = new RTCPeerConnection(ICE);
    const dc = pc.createDataChannel('lumen', { ordered: true });
    let done;
    const opened = new Promise((res) => { done = res; setTimeout(() => res(false), 12000); });
    wire(dc, open, msg, close, () => done(true));
    await pc.setLocalDescription(await pc.createOffer());
    await gathered(pc);
    await api(base, '', { room, role: 'j', c, sdp: pc.localDescription.sdp });
    for (let i = 0; i < 24; i++) {
      const v = await api(base, '?room=' + enc(room) + '&c=' + c);
      if (v && v.sdp) { await pc.setRemoteDescription({ type: 'answer', sdp: v.sdp }); break; }
      await sleep(400);
    }
    const ok = await opened;
    if (!ok) pc.close();
    return ok;
  } catch (e) {
    return false;
  }
}

export async function lumen_rtc_host(base, room, open, msg, close, stopped) {
  const seen = new Set();
  const answer = async (o) => {
    const pc = new RTCPeerConnection(ICE);
    pc.ondatachannel = (e) => wire(e.channel, open, msg, close, null);
    await pc.setRemoteDescription({ type: 'offer', sdp: o.sdp });
    await pc.setLocalDescription(await pc.createAnswer());
    await gathered(pc);
    await api(base, '', { room, role: 'h', c: o.c, sdp: pc.localDescription.sdp });
    setTimeout(() => { if (pc.connectionState !== 'connected') pc.close(); }, 20000);
  };
  try {
    while (!stopped()) {
      const v = await api(base, '?room=' + enc(room) + '&role=h');
      for (const o of (v && v.offers) || []) {
        if (seen.has(o.c) || seen.size > 256 || typeof o.sdp !== 'string') continue;
        seen.add(o.c);
        answer(o).catch(() => {});
      }
      await sleep(1500);
    }
  } catch (e) {}
}
"#)]
extern "C" {
    fn lumen_rtc_dial(
        base: &str,
        room: &str,
        open: &js_sys::Function,
        msg: &js_sys::Function,
        close: &js_sys::Function,
    ) -> js_sys::Promise;
    fn lumen_rtc_host(
        base: &str,
        room: &str,
        open: &js_sys::Function,
        msg: &js_sys::Function,
        close: &js_sys::Function,
        stopped: &js_sys::Function,
    ) -> js_sys::Promise;
}

struct RtcSink {
    send: js_sys::Function,
    close: js_sys::Function,
}

// SAFETY: wasm32-unknown-unknown (without the atomics feature) has exactly one thread; the value can never cross threads.
unsafe impl Send for RtcSink {}

impl Sink for RtcSink {
    fn send(&self, m: &Msg) {
        let data: JsValue = match m {
            Msg::Bin { d, more } => {
                let mut v = Vec::with_capacity(d.len() + 1);
                v.push(u8::from(*more));
                v.extend_from_slice(d);
                js_sys::Uint8Array::from(&v[..]).into()
            }
            m => match m.to_text() {
                Some(s) => JsValue::from_str(&s),
                None => return,
            },
        };
        let _ = self.send.call1(&JsValue::NULL, &data);
    }
    fn close(&self) {
        let _ = self.close.call0(&JsValue::NULL);
    }
}

/// The three callbacks the JavaScript side needs: a channel opened (→ connection id), a message, a channel closed.
fn callbacks(
    inbox: &Inbox,
    incoming: bool,
) -> (js_sys::Function, js_sys::Function, js_sys::Function) {
    let i1 = inbox.clone();
    let open = Closure::<dyn FnMut(js_sys::Function, js_sys::Function) -> f64>::new(
        move |send: js_sys::Function, close: js_sys::Function| {
            let conn = i1.conn_id();
            i1.push(Event::Open {
                conn,
                sink: Box::new(RtcSink { send, close }),
                addr: "webrtc".into(),
                incoming,
            });
            conn as f64
        },
    );
    let i2 = inbox.clone();
    let msg = Closure::<dyn FnMut(f64, JsValue)>::new(move |conn: f64, data: JsValue| {
        let conn = conn as ConnId;
        if let Some(s) = data.as_string() {
            if let Some(m) = Msg::from_text(&s) {
                i2.push(Event::Msg { conn, msg: m });
            }
        } else if let Ok(buf) = data.dyn_into::<js_sys::ArrayBuffer>() {
            let b = js_sys::Uint8Array::new(&buf).to_vec();
            if let Some((flag, body)) = b.split_first() {
                i2.push(Event::Msg {
                    conn,
                    msg: Msg::Bin {
                        d: body.to_vec(),
                        more: *flag != 0,
                    },
                });
            }
        }
    });
    let i3 = inbox.clone();
    let close = Closure::<dyn FnMut(f64)>::new(move |conn: f64| {
        i3.push(Event::Closed {
            conn: conn as ConnId,
            why: "Connection lost".into(),
        });
    });
    let out = (
        open.as_ref().unchecked_ref::<js_sys::Function>().clone(),
        msg.as_ref().unchecked_ref::<js_sys::Function>().clone(),
        close.as_ref().unchecked_ref::<js_sys::Function>().clone(),
    );
    open.forget();
    msg.forget();
    close.forget();
    out
}

/// Joiner: tries to open a data channel to the browser that hosts `room`; `tx` gets `true` once the channel opened (the session already
/// has its `Event::Open`), `false` when the caller should use the relay.
pub fn dial(
    base: &str,
    room: &str,
    inbox: &Inbox,
    stop: Arc<std::sync::atomic::AtomicBool>,
    tx: std::sync::mpsc::Sender<bool>,
    ctx: egui::Context,
) {
    let (open, msg, close) = callbacks(inbox, false);
    let p = lumen_rtc_dial(base, room, &open, &msg, &close);
    wasm_bindgen_futures::spawn_local(async move {
        let ok = wasm_bindgen_futures::JsFuture::from(p)
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && !stop.load(Ordering::Relaxed);
        let _ = tx.send(ok);
        ctx.request_repaint();
    });
}

/// Host: answers offers for `room` until `stop` is set.
pub fn host(base: &str, room: &str, inbox: &Inbox, stop: Arc<std::sync::atomic::AtomicBool>) {
    let (open, msg, close) = callbacks(inbox, true);
    let stopped = Closure::<dyn FnMut() -> bool>::new(move || stop.load(Ordering::Relaxed));
    let f: js_sys::Function = stopped.as_ref().unchecked_ref::<js_sys::Function>().clone();
    stopped.forget();
    let p = lumen_rtc_host(base, room, &open, &msg, &close, &f);
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    });
}
