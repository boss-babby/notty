//! Browser transport: a plain `WebSocket` to a Lumen host on the local network (`ws://ip:port`).
//!
//! Browsers refuse `ws://` from an `https://` page to anything but localhost (mixed content). The web build therefore offers two
//! ways in: this direct socket when the page itself is served over http / from localhost, and the relay (`relay.rs`) otherwise.
use super::*;
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{CloseEvent, MessageEvent, WebSocket};

struct WsSink(WebSocket);

// SAFETY: wasm32-unknown-unknown (without the atomics feature) has exactly one thread; the value can never cross threads.
unsafe impl Send for WsSink {}

impl Sink for WsSink {
    fn send(&self, m: &Msg) {
        if self.0.ready_state() == 1 {
            if let Ok(s) = serde_json::to_string(m) {
                let _ = self.0.send_with_str(&s);
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
        i1.push(Event::Open { conn, sink: Box::new(WsSink(w1.clone())), addr: a1.clone(), incoming: false });
    });
    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    onopen.forget();
    let i2 = inbox.clone();
    let onmsg = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        if let Some(s) = e.data().as_string() {
            if let Ok(m) = serde_json::from_str::<Msg>(&s) {
                i2.push(Event::Msg { conn, msg: m });
            }
        }
    });
    ws.set_onmessage(Some(onmsg.as_ref().unchecked_ref()));
    onmsg.forget();
    let i3 = inbox.clone();
    let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_e: CloseEvent| {
        i3.push(Event::Closed { conn, why: "Could not reach that device — are you on the same Wi-Fi?".into() });
    });
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();
}
