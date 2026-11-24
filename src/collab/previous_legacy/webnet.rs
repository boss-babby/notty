//! Browser transport: a plain `WebSocket` to a Lumen host on the local network (`ws://ip:port`).
//!
//! Browsers refuse `ws://` from an `https://` page to anything but localhost (mixed content). The web build therefore offers two
//! ways in: this direct socket when the page itself is served over http / from localhost, and the relay (`relay.rs`) otherwise.
use super::*;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{CloseEvent, MessageEvent, WebSocket};

struct WsSink(WebSocket);

// SAFETY: wasm32-unknown-unknown (without the atomics feature) has exactly one thread; the value can never cross threads.
unsafe impl Send for WsSink {}

impl Sink for WsSink {
    fn send(&self, m: &Msg) {
        if self.0.ready_state() == 1 {
            match m {
                // protocol 5 on a native host: encrypted frames are binary — [more flag][Noise message]
                Msg::Bin { d, more } => {
                    let mut v = Vec::with_capacity(d.len() + 1);
                    v.push(u8::from(*more));
                    v.extend_from_slice(d);
                    let _ = self.0.send_with_u8_array(&v);
                }
                m => {
                    if let Some(s) = m.to_text() {
                        let _ = self.0.send_with_str(&s);
                    }
                }
            }
        }
    }
    fn close(&self) {
        let _ = self.0.close();
    }
}

pub fn connect(addr: &str, inbox: &Inbox) {
    let conn = inbox.conn_id();
    let ws = match WebSocket::new(&format!("ws://{addr}/")) {
        Ok(w) => w,
        Err(_) => {
            inbox.push(Event::Closed { conn, why: "The browser blocked that address — an https page cannot open ws:// to another device. Use the link option, or open Lumen over http.".into() });
            return;
        }
    };
    let (i1, a1, w1) = (inbox.clone(), addr.to_owned(), ws.clone());
    let onopen = Closure::<dyn FnMut()>::new(move || {
        i1.push(Event::Open {
            conn,
            sink: Box::new(WsSink(w1.clone())),
            addr: a1.clone(),
            incoming: false,
        });
    });
    let _ = js_sys::Reflect::set(
        &ws,
        &JsValue::from_str("binaryType"),
        &JsValue::from_str("arraybuffer"),
    );
    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();
    let i2 = inbox.clone();
    let onmsg = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let data = e.data();
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
    ws.set_onmessage(Some(onmsg.as_ref().unchecked_ref()));
    onmsg.forget();
    let i3 = inbox.clone();
    let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_e: CloseEvent| {
        i3.push(Event::Closed {
            conn,
            why: "Could not reach that device — are you on the same Wi-Fi?".into(),
        });
    });
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();
}
