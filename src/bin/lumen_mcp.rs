//! `lumen-mcp` — headless Model Context Protocol server for Lumen.
//!
//! * `lumen-mcp --workspace backup.json [--access read|edit|full] [--new]` — MCP over stdio (Claude Desktop, Cursor, scripts).
//! * `lumen-mcp --relay 7979 --token lmn_… --origin https://your-site` — a **loopback relay** that lets the WebAssembly app (which cannot
//!   open a listening socket) serve a desktop AI client without anything leaving the computer:
//!
//!   ```text
//!   AI client --POST http://127.0.0.1:7979/mcp (Bearer)--> relay <--long-poll /api/bridge/poll (Bearer, CORS)-- Lumen in the browser
//!   ```
//!
//!   Security: binds to 127.0.0.1 only; every request needs the bearer token (constant-time compare); a request that carries an `Origin`
//!   header must come from `--origin` (or from a loopback page) and its `Host` must be a loopback name (DNS-rebinding guard); CORS and
//!   Chrome's Private-Network-Access preflight are answered for exactly that origin; bodies are capped at 4 MB.
#![cfg_attr(not(debug_assertions), allow(unused))]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--relay") {
        std::process::exit(relay::run(&args));
    }
    std::process::exit(lumen::mcp_cli(args));
}

#[cfg(not(target_arch = "wasm32"))]
mod relay {
    use std::collections::{HashMap, VecDeque};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct State {
        queue: VecDeque<(String, String)>,
        results: HashMap<String, Option<String>>,
        last_poll: Option<Instant>,
        next: u64,
    }
    struct Shared {
        st: Mutex<State>,
        cv: Condvar,
        token: String,
        origin: String,
        port: u16,
    }

    fn ct_eq(a: &str, b: &str) -> bool {
        a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    pub fn run(args: &[String]) -> i32 {
        let (mut port, mut token, mut origin) = (7979u16, String::new(), String::new());
        let mut it = args.iter().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--relay" => port = it.next().and_then(|p| p.parse().ok()).unwrap_or(port),
                "--token" => token = it.next().cloned().unwrap_or_default(),
                "--origin" => origin = it.next().cloned().unwrap_or_default().trim_end_matches('/').to_owned(),
                _ => {}
            }
        }
        if token.len() < 16 {
            eprintln!("lumen-mcp --relay <port> --token <at least 16 characters, e.g. the token shown in Lumen → AI → Connect> [--origin https://your-site]");
            return 2;
        }
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("lumen-mcp: cannot listen on 127.0.0.1:{port}: {e}");
                return 2;
            }
        };
        eprintln!("lumen-mcp relay on http://127.0.0.1:{port}  (agents: POST /mcp · Lumen app bridge URL: http://127.0.0.1:{port} · allowed web origin: {})", if origin.is_empty() { "loopback pages only" } else { &origin });
        let sh = Arc::new(Shared { st: Mutex::new(State::default()), cv: Condvar::new(), token, origin, port });
        for conn in listener.incoming().flatten() {
            let sh = sh.clone();
            std::thread::spawn(move || {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(20)));
                let _ = handle(conn, &sh);
            });
        }
        0
    }

    struct Http {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    fn read_request(s: &mut TcpStream) -> std::io::Result<Http> {
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];
        let end = loop {
            let n = s.read(&mut tmp)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            if buf.len() > 32 * 1024 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        let mut lines = head.split("\r\n");
        let mut first = lines.next().unwrap_or("").split(' ');
        let (method, path) = (first.next().unwrap_or("").to_owned(), first.next().unwrap_or("").to_owned());
        let headers: HashMap<String, String> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))).collect();
        let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        if len > 4 * 1024 * 1024 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut body = buf[end..].to_vec();
        while body.len() < len {
            let n = s.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(len);
        Ok(Http { method, path, headers, body })
    }

    fn allowed_origin(sh: &Shared, origin: &str) -> bool {
        let o = origin.trim_end_matches('/');
        (!sh.origin.is_empty() && o == sh.origin) || o.starts_with("http://127.0.0.1:") || o.starts_with("http://localhost:") || o == "http://127.0.0.1" || o == "http://localhost"
    }

    fn reply(s: &mut TcpStream, code: u16, reason: &str, cors: Option<&str>, ctype: &str, body: &str) -> std::io::Result<()> {
        let mut h = format!("HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n", body.len());
        if let Some(o) = cors {
            h.push_str(&format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Allow-Headers: authorization, content-type\r\nAccess-Control-Allow-Methods: POST, GET, OPTIONS\r\nAccess-Control-Max-Age: 600\r\n"));
        }
        h.push_str("\r\n");
        s.write_all(h.as_bytes())?;
        s.write_all(body.as_bytes())?;
        s.flush()
    }

    fn handle(mut s: TcpStream, sh: &Shared) -> std::io::Result<()> {
        let req = read_request(&mut s)?;
        // DNS-rebinding guard: only loopback host names
        let host = req.headers.get("host").map(|h| h.to_lowercase()).unwrap_or_default();
        let host_ok = host == format!("127.0.0.1:{}", sh.port) || host == format!("localhost:{}", sh.port) || host == "127.0.0.1" || host == "localhost";
        if !host_ok {
            return reply(&mut s, 403, "Forbidden", None, "application/json", "{\"error\":\"bad host\"}");
        }
        let origin = req.headers.get("origin").cloned();
        if let Some(o) = &origin {
            if !allowed_origin(sh, o) {
                return reply(&mut s, 403, "Forbidden", None, "application/json", "{\"error\":\"origin not allowed\"}");
            }
        }
        let cors = origin.as_deref();
        if req.method == "OPTIONS" {
            return reply(&mut s, 204, "No Content", cors, "text/plain", "");
        }
        let bearer = req.headers.get("authorization").and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer "))).unwrap_or("");
        if !ct_eq(bearer, &sh.token) {
            return reply(&mut s, 401, "Unauthorized", cors, "application/json", "{\"error\":\"bearer token required\"}");
        }
        let path = req.path.split('?').next().unwrap_or("");
        match (req.method.as_str(), path) {
            ("POST", "/api/bridge/poll") => {
                let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
                let mut st = sh.st.lock().unwrap();
                st.last_poll = Some(Instant::now());
                if let Some(rs) = v.get("results").and_then(|r| r.as_array()) {
                    for r in rs {
                        if let Some(id) = r.get("id").and_then(|i| i.as_str()) {
                            st.results.insert(id.to_owned(), r.get("response").and_then(|x| x.as_str()).map(str::to_owned));
                        }
                    }
                    sh.cv.notify_all();
                }
                if st.queue.is_empty() {
                    let (g, _) = sh.cv.wait_timeout_while(st, Duration::from_secs(25), |s| s.queue.is_empty()).unwrap();
                    st = g;
                }
                let reqs: Vec<serde_json::Value> = st.queue.drain(..).take(8).map(|(id, payload)| serde_json::json!({"id": id, "payload": payload})).collect();
                drop(st);
                reply(&mut s, 200, "OK", cors, "application/json", &serde_json::json!({ "requests": reqs }).to_string())
            }
            ("POST", "/mcp") | ("POST", "/api/mcp") => {
                let text = String::from_utf8_lossy(&req.body).to_string();
                if serde_json::from_str::<serde_json::Value>(&text).is_err() {
                    return reply(&mut s, 400, "Bad Request", cors, "application/json", "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"Parse error\"}}");
                }
                let mut st = sh.st.lock().unwrap();
                let online = st.last_poll.map_or(false, |t| t.elapsed() < Duration::from_secs(70));
                if !online {
                    drop(st);
                    return reply(&mut s, 503, "Service Unavailable", cors, "application/json", "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32000,\"message\":\"The Lumen app is not connected to this relay (AI → Connect → bridge URL http://127.0.0.1)\"}}");
                }
                st.next += 1;
                let id = format!("r{}", st.next);
                st.queue.push_back((id.clone(), text));
                sh.cv.notify_all();
                let (mut st, _) = sh.cv.wait_timeout_while(st, Duration::from_secs(55), |s| !s.results.contains_key(&id)).unwrap();
                let got = st.results.remove(&id);
                drop(st);
                match got {
                    Some(Some(body)) => reply(&mut s, 200, "OK", cors, "application/json", &body),
                    Some(None) => reply(&mut s, 202, "Accepted", cors, "text/plain", ""),
                    None => reply(&mut s, 504, "Gateway Timeout", cors, "application/json", "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32003,\"message\":\"The Lumen app did not answer in time\"}}"),
                }
            }
            ("GET", "/api/bridge/status") => {
                let st = sh.st.lock().unwrap();
                let online = st.last_poll.map_or(false, |t| t.elapsed() < Duration::from_secs(70));
                drop(st);
                reply(&mut s, 200, "OK", cors, "application/json", &format!("{{\"online\":{online}}}"))
            }
            _ => reply(&mut s, 404, "Not Found", cors, "application/json", "{\"error\":\"not found\"}"),
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod relay {
    pub fn run(_: &[String]) -> i32 {
        2
    }
}
