//! End-to-end encryption of every collaboration connection (LAN socket, browser socket and HTTP relay alike).
//!
//! * Handshake: `Noise_XX_25519_ChaChaPoly_SHA256` (crate `snow`, pure Rust, also on wasm32). Both sides get forward secrecy and
//!   confidentiality against anyone sniffing the Wi-Fi or the relay; the joiner always initiates.
//! * Authentication: the share token never travels. After the handshake each side proves it knows the token with
//!   `SHA-256(label ‖ token ‖ handshake-hash)`; the proof is bound to *this* channel, so a man in the middle cannot replay it
//!   (see [`proof`]). Devices without a token fall back to the host's approval card, which shows the channel's short code
//!   ([`short_code`]) — read it out loud on both screens to rule out an active attacker.
//! * Framing: JSON → (deflate when it pays) → split in ≤ 48 000 byte pieces (Noise messages are ≤ 65 535 bytes) → one AEAD
//!   message each → `Msg::Enc` frames. A decompression limit and a reassembly cap bound what a hostile peer can make us allocate.
use super::*;
use sha2::{Digest, Sha256};
use snow::{Builder, HandshakeState, TransportState};

const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
const FRAG: usize = 48_000;
/// Largest message we reassemble / inflate.
pub const MAX_PLAIN: usize = 24 << 20;
const MAX_PENDING: usize = 512;

pub struct Keys {
    private: Vec<u8>,
}

pub fn gen_keys() -> Option<Keys> {
    let b = Builder::new(PATTERN.parse().ok()?);
    let kp = b.generate_keypair().ok()?;
    Some(Keys { private: kp.private })
}

enum Stage {
    Idle,
    Handshake(Box<HandshakeState>),
    Open(Box<TransportState>),
    Dead,
}

pub struct Crypt {
    stage: Stage,
    /// Channel binding (Noise handshake hash) once the handshake finished.
    pub hash: Vec<u8>,
    /// The other side's static public key.
    pub remote: Vec<u8>,
    pending: Vec<String>,
    rx: Vec<u8>,
}

pub struct HsOut {
    pub reply: Option<String>,
    pub done: bool,
    pub failed: bool,
    /// Frames that were queued while the handshake ran.
    pub flushed: Vec<Msg>,
}

impl Crypt {
    pub fn new() -> Crypt {
        Crypt { stage: Stage::Idle, hash: vec![], remote: vec![], pending: vec![], rx: vec![] }
    }

    pub fn is_open(&self) -> bool {
        matches!(self.stage, Stage::Open(_))
    }

    pub fn started(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    /// First handshake message (joiner).
    pub fn start_initiator(&mut self, keys: &Keys) -> Option<String> {
        let mut hs = Builder::new(PATTERN.parse().ok()?).local_private_key(&keys.private).build_initiator().ok()?;
        let mut buf = vec![0u8; 512];
        let n = hs.write_message(&[], &mut buf).ok()?;
        self.stage = Stage::Handshake(Box::new(hs));
        Some(crate::media::b64_encode(&buf[..n]))
    }

    /// Feeds one handshake message; becomes the responder when it is the first one.
    pub fn on_hs(&mut self, keys: &Keys, d: &str) -> HsOut {
        let fail = HsOut { reply: None, done: false, failed: true, flushed: vec![] };
        let Some(bytes) = crate::media::b64_decode(d) else { return fail };
        if bytes.len() > 512 {
            self.stage = Stage::Dead;
            return fail;
        }
        if matches!(self.stage, Stage::Idle) {
            let Ok(pat) = PATTERN.parse() else { return fail };
            let Ok(hs) = Builder::new(pat).local_private_key(&keys.private).build_responder() else { return fail };
            self.stage = Stage::Handshake(Box::new(hs));
        }
        let Stage::Handshake(mut hs) = std::mem::replace(&mut self.stage, Stage::Dead) else { return fail };
        let mut buf = vec![0u8; 512];
        if hs.read_message(&bytes, &mut buf).is_err() {
            return fail;
        }
        let mut reply = None;
        if !hs.is_handshake_finished() {
            match hs.write_message(&[], &mut buf) {
                Ok(n) => reply = Some(crate::media::b64_encode(&buf[..n])),
                Err(_) => return fail,
            }
        }
        if hs.is_handshake_finished() {
            self.hash = hs.get_handshake_hash().to_vec();
            self.remote = hs.get_remote_static().map(|k| k.to_vec()).unwrap_or_default();
            match hs.into_transport_mode() {
                Ok(ts) => self.stage = Stage::Open(Box::new(ts)),
                Err(_) => return fail,
            }
            let pending = std::mem::take(&mut self.pending);
            let mut flushed = vec![];
            for p in pending {
                flushed.extend(self.seal(&p));
            }
            HsOut { reply, done: true, failed: false, flushed }
        } else {
            self.stage = Stage::Handshake(hs);
            HsOut { reply, done: false, failed: false, flushed: vec![] }
        }
    }

    /// Encrypts one JSON message; before the handshake ends it is queued.
    pub fn seal(&mut self, json: &str) -> Vec<Msg> {
        let Stage::Open(ts) = &mut self.stage else {
            if !matches!(self.stage, Stage::Dead) && self.pending.len() < MAX_PENDING {
                self.pending.push(json.to_owned());
            }
            return vec![];
        };
        let raw = json.as_bytes();
        let mut payload = Vec::with_capacity(raw.len() + 1);
        if raw.len() > 400 {
            let z = miniz_oxide::deflate::compress_to_vec(raw, 3);
            if z.len() + 8 < raw.len() {
                payload.push(1);
                payload.extend_from_slice(&z);
            }
        }
        if payload.is_empty() {
            payload.push(0);
            payload.extend_from_slice(raw);
        }
        let n = payload.len().div_ceil(FRAG);
        let mut out = Vec::with_capacity(n);
        for (i, chunk) in payload.chunks(FRAG).enumerate() {
            let mut buf = vec![0u8; chunk.len() + 64];
            match ts.write_message(chunk, &mut buf) {
                Ok(len) => out.push(Msg::Enc { d: crate::media::b64_encode(&buf[..len]), more: i + 1 < n }),
                Err(_) => {
                    self.stage = Stage::Dead;
                    return vec![];
                }
            }
        }
        out
    }

    /// Decrypts one frame; `Ok(Some(json))` when a whole message is complete, `Err` when the connection must be dropped.
    pub fn open(&mut self, d: &str, more: bool) -> Result<Option<String>, ()> {
        let Stage::Open(ts) = &mut self.stage else { return Err(()) };
        let bytes = crate::media::b64_decode(d).ok_or(())?;
        if bytes.len() > 65_535 {
            return Err(());
        }
        let mut buf = vec![0u8; bytes.len()];
        let n = ts.read_message(&bytes, &mut buf).map_err(|_| ())?;
        if self.rx.len() + n > MAX_PLAIN {
            self.rx.clear();
            return Err(());
        }
        self.rx.extend_from_slice(&buf[..n]);
        if more {
            return Ok(None);
        }
        let all = std::mem::take(&mut self.rx);
        let (flag, body) = all.split_first().ok_or(())?;
        let text = match flag {
            0 => body.to_vec(),
            1 => miniz_oxide::inflate::decompress_to_vec_with_limit(body, MAX_PLAIN).map_err(|_| ())?,
            _ => return Err(()),
        };
        String::from_utf8(text).map(Some).map_err(|_| ())
    }
}

/// Wraps a transport: everything sent through it is encrypted (or queued until the handshake ends); `raw` bypasses the cipher.
pub struct SecureSink {
    pub inner: Box<dyn Sink>,
    pub crypt: Arc<Mutex<Crypt>>,
}

impl Sink for SecureSink {
    fn send(&self, m: &Msg) {
        let Ok(json) = serde_json::to_string(m) else { return };
        let frames = match self.crypt.lock() {
            Ok(mut c) => c.seal(&json),
            Err(_) => return,
        };
        for f in frames {
            self.inner.send(&f);
        }
    }
    fn raw(&self, m: &Msg) {
        self.inner.send(m);
    }
    fn close(&self) {
        self.inner.close();
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `SHA-256("lumen-collab-v2" ‖ label ‖ len(token) ‖ token ‖ handshake-hash)` — proves knowledge of the token for *this* channel only.
pub fn proof(label: &str, token: &str, hash: &[u8]) -> String {
    if token.is_empty() || hash.is_empty() {
        return String::new();
    }
    let mut h = Sha256::new();
    h.update(b"lumen-collab-v2");
    h.update(label.as_bytes());
    h.update((token.len() as u32).to_le_bytes());
    h.update(token.as_bytes());
    h.update(hash);
    hex(&h.finalize())
}

/// Constant-time comparison (no early exit on the first differing byte).
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Four digits both devices can read aloud to confirm they talk to each other (derived from the channel binding).
pub fn short_code(hash: &[u8]) -> String {
    if hash.len() < 4 {
        return String::new();
    }
    let n = u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) % 10_000;
    format!("{n:04}")
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn pair() -> (Crypt, Crypt) {
        let (ka, kb) = (gen_keys().unwrap(), gen_keys().unwrap());
        let (mut a, mut b) = (Crypt::new(), Crypt::new());
        let m1 = a.start_initiator(&ka).unwrap();
        let o = b.on_hs(&kb, &m1);
        assert!(!o.failed && !o.done);
        let o2 = a.on_hs(&ka, &o.reply.unwrap());
        assert!(!o2.failed && o2.done);
        let o3 = b.on_hs(&kb, &o2.reply.unwrap());
        assert!(!o3.failed && o3.done);
        assert_eq!(a.hash, b.hash);
        (a, b)
    }

    fn deliver(from: &mut Crypt, to: &mut Crypt, json: &str) -> Option<String> {
        let mut out = None;
        for f in from.seal(json) {
            let Msg::Enc { d, more } = f else { panic!("not an Enc frame") };
            out = to.open(&d, more).unwrap();
        }
        out
    }

    #[test]
    fn handshake_encrypts_and_binds_the_channel() {
        let (mut a, mut b) = pair();
        assert_eq!(deliver(&mut a, &mut b, r#"{"t":"Bye"}"#).as_deref(), Some(r#"{"t":"Bye"}"#));
        assert_eq!(deliver(&mut b, &mut a, "héllo ✓").as_deref(), Some("héllo ✓"));
        // the token proof is bound to the channel: the same token on another channel gives another proof
        let (c, _) = pair();
        assert_ne!(proof("c", "ABCDE23456", &a.hash), proof("c", "ABCDE23456", &c.hash));
        assert_ne!(proof("c", "ABCDE23456", &a.hash), proof("s", "ABCDE23456", &a.hash));
        assert!(ct_eq(&proof("c", "ABCDE23456", &a.hash), &proof("c", "ABCDE23456", &b.hash)));
        assert!(!ct_eq("", ""));
        assert_eq!(short_code(&a.hash), short_code(&b.hash));
    }

    #[test]
    fn big_messages_are_fragmented_compressed_and_survive() {
        let (mut a, mut b) = pair();
        let big: String = (0..400_000).map(|i| char::from(b'a' + (i % 23) as u8)).collect();
        // high-entropy text (6 bits per character) cannot be compressed below ~75 %: 300 kB needs several 48 kB frames
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let noise: String = (0..300_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                char::from(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[(x >> 20) as usize & 63])
            })
            .collect();
        assert_eq!(deliver(&mut a, &mut b, &big).as_deref(), Some(big.as_str()));
        let frames = a.seal(&noise);
        assert!(frames.len() > 4, "got {} frames", frames.len());
        let mut out = None;
        for f in frames {
            let Msg::Enc { d, more } = f else { panic!() };
            out = b.open(&d, more).unwrap();
        }
        assert_eq!(out.as_deref(), Some(noise.as_str()));
    }

    #[test]
    fn tampering_and_replay_are_rejected() {
        let (mut a, mut b) = pair();
        let Msg::Enc { d, more } = a.seal("secret").remove(0) else { panic!() };
        let mut bytes = crate::media::b64_decode(&d).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(b.open(&crate::media::b64_encode(&bytes), more).is_err());
        // a plaintext frame, or garbage, never decrypts
        let (mut a2, mut b2) = pair();
        assert!(b2.open("AAAA", false).is_err());
        let frames = a2.seal("one");
        let Msg::Enc { d, more } = frames[0].clone() else { panic!() };
        assert!(b2.open(&d, more).is_ok());
        assert!(b2.open(&d, more).is_err(), "replayed frame must fail (nonce counter)");
    }

    #[test]
    fn decompression_bombs_are_bounded() {
        let (mut a, mut b) = pair();
        // 40 MB of zeros deflates to ~40 kB; the receiver must refuse to inflate it
        let bomb = vec![0u8; 40 << 20];
        let z = miniz_oxide::deflate::compress_to_vec(&bomb, 1);
        let mut payload = vec![1u8];
        payload.extend_from_slice(&z);
        // build a valid encrypted frame around the hostile payload using the sender's cipher
        let frames = {
            let Stage::Open(ts) = &mut a.stage else { panic!() };
            let mut out = vec![];
            let n = payload.len().div_ceil(FRAG);
            for (i, chunk) in payload.chunks(FRAG).enumerate() {
                let mut buf = vec![0u8; chunk.len() + 64];
                let len = ts.write_message(chunk, &mut buf).unwrap();
                out.push((crate::media::b64_encode(&buf[..len]), i + 1 < n));
            }
            out
        };
        let mut res = Ok(None);
        for (d, more) in frames {
            res = b.open(&d, more);
        }
        assert!(res.is_err());
    }
}
