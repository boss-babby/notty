//! "AirDrop"-style pairing core (Bluetooth LE hand-off of a join link) — the part that needs no radio, so it can be tested anywhere.
//!
//! Flow (what the Android glue has to do, see COLLAB.md §15.4):
//! 1. The host advertises only [`session_tag`] — 16 bytes, a one-way hash of the share id; nothing about the document, the person or the
//!    address.
//! 2. A nearby device that wants in connects over GATT and writes its ephemeral X25519 public key ([`Keypair::public`]); the host answers
//!    with its own.
//! 3. Both compute the same shared secret and show the same six digits ([`code`]). The people compare them (this is what defeats a
//!    man in the middle: an attacker's two separate secrets give two different codes), the host taps *Accept*.
//! 4. The host writes the join link sealed with [`seal`] (ChaCha20-Poly1305, key = hash of the shared secret). The guest [`open`]s it and
//!    joins the normal way; the link's share token never travels in the clear.
//! 5. Frames are cut to the GATT MTU with [`fragment`] and put together with [`Reassembler`] (length + CRC checked).
use sha2::{Digest, Sha256};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

/// What a host advertises: identifies the session to a device that already knows its share id, and nobody else.
pub fn session_tag(share: &str) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(b"lumen-ble-tag-v1");
    h.update(share.as_bytes());
    let mut t = [0u8; 16];
    t.copy_from_slice(&h.finalize()[..16]);
    t
}

/// An ephemeral X25519 key pair (one per pairing attempt).
pub struct Keypair {
    secret: [u8; 32],
    public: [u8; 32],
}

impl Keypair {
    pub fn generate() -> Option<Keypair> {
        let mut secret = [0u8; 32];
        getrandom::getrandom(&mut secret).ok()?;
        Some(Self::from_secret(secret))
    }

    pub fn from_secret(secret: [u8; 32]) -> Keypair {
        let public = x25519_dalek::x25519(secret, x25519_dalek::X25519_BASEPOINT_BYTES);
        Keypair { secret, public }
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// Shared secret with the other side's public key; `None` for a degenerate key (all-zero output, a small-order point).
    pub fn shared(&self, theirs: [u8; 32]) -> Option<[u8; 32]> {
        let s = x25519_dalek::x25519(self.secret, theirs);
        (s.iter().any(|b| *b != 0)).then_some(s)
    }
}

/// The six digits both people compare. Bound to both public keys (in a fixed order), so a relayed / substituted key changes it.
pub fn code(shared: &[u8; 32], a: &[u8; 32], b: &[u8; 32]) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut h = Sha256::new();
    h.update(b"lumen-ble-code-v1");
    h.update(shared);
    h.update(lo);
    h.update(hi);
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{n:06}")
}

fn cipher(shared: &[u8; 32]) -> ChaCha20Poly1305 {
    let mut h = Sha256::new();
    h.update(b"lumen-ble-key-v1");
    h.update(shared);
    ChaCha20Poly1305::new(Key::from_slice(&h.finalize()))
}

/// `nonce(12) ‖ ciphertext ‖ tag(16)`.
pub fn seal(shared: &[u8; 32], link: &str) -> Option<Vec<u8>> {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).ok()?;
    let ct = cipher(shared).encrypt(Nonce::from_slice(&nonce), link.as_bytes()).ok()?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Some(out)
}

pub fn open(shared: &[u8; 32], sealed: &[u8]) -> Option<String> {
    if sealed.len() < 12 + 16 || sealed.len() > 4096 {
        return None;
    }
    let (n, ct) = sealed.split_at(12);
    let pt = cipher(shared).decrypt(Nonce::from_slice(n), ct).ok()?;
    let s = String::from_utf8(pt).ok()?;
    (s.starts_with("lumen://join?") || s.starts_with("https://") || s.starts_with("http://")).then_some(s)
}

fn crc(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// Cuts a message into GATT writes of at most `mtu` bytes: `[index, count]` + payload; the first packet's payload starts with `len(u16) crc(u32)`.
pub fn fragment(data: &[u8], mtu: usize) -> Vec<Vec<u8>> {
    let room = mtu.saturating_sub(2).max(4);
    let mut framed = (data.len() as u16).to_be_bytes().to_vec();
    framed.extend_from_slice(&crc(data).to_be_bytes());
    framed.extend_from_slice(data);
    let parts: Vec<&[u8]> = framed.chunks(room).collect();
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut v = vec![i as u8, parts.len() as u8];
            v.extend_from_slice(p);
            v
        })
        .collect()
}

#[derive(Default)]
pub struct Reassembler {
    parts: Vec<Option<Vec<u8>>>,
}

impl Reassembler {
    /// Feeds one packet; returns the message once every part is here and the checksum is right.
    pub fn push(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        let (&i, rest) = packet.split_first()?;
        let (&n, body) = rest.split_first()?;
        if n == 0 || n > 64 || i >= n {
            return None;
        }
        if self.parts.len() != n as usize {
            self.parts = vec![None; n as usize];
        }
        self.parts[i as usize] = Some(body.to_vec());
        if self.parts.iter().any(|p| p.is_none()) {
            return None;
        }
        let all: Vec<u8> = self.parts.drain(..).flatten().flatten().collect();
        if all.len() < 6 {
            return None;
        }
        let len = u16::from_be_bytes([all[0], all[1]]) as usize;
        let sum = u32::from_be_bytes([all[2], all[3], all[4], all[5]]);
        let data = &all[6..];
        (data.len() == len && crc(data) == sum).then(|| data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_see_the_same_code_and_the_link_arrives_sealed() {
        let (host, guest) = (Keypair::generate().unwrap(), Keypair::generate().unwrap());
        let (hs, gs) = (host.shared(guest.public()).unwrap(), guest.shared(host.public()).unwrap());
        assert_eq!(hs, gs);
        let (c1, c2) = (code(&hs, &host.public(), &guest.public()), code(&gs, &guest.public(), &host.public()));
        assert_eq!(c1, c2);
        assert_eq!(c1.len(), 6);
        let link = "lumen://join?h=192.168.1.20:47615&t=ABCD1234";
        let sealed = seal(&hs, link).unwrap();
        assert!(!sealed.windows(8).any(|w| w == b"ABCD1234"), "the token must not be visible");
        assert_eq!(open(&gs, &sealed).as_deref(), Some(link));
        // any bit flipped in transit is detected
        let mut bad = sealed.clone();
        bad[20] ^= 1;
        assert!(open(&gs, &bad).is_none());
    }

    #[test]
    fn a_man_in_the_middle_gets_different_codes_and_cannot_read_the_link() {
        let (host, guest, mallory) = (Keypair::generate().unwrap(), Keypair::generate().unwrap(), Keypair::generate().unwrap());
        // mallory answers the guest with her key and the host with hers: two separate secrets
        let host_side = host.shared(mallory.public()).unwrap();
        let guest_side = guest.shared(mallory.public()).unwrap();
        let ch = code(&host_side, &host.public(), &mallory.public());
        let cg = code(&guest_side, &guest.public(), &mallory.public());
        assert_ne!(ch, cg, "the people would see two different codes and refuse");
        let sealed = seal(&host_side, "lumen://join?h=1.2.3.4:5&t=SECRETTOKEN").unwrap();
        assert!(open(&guest_side, &sealed).is_none());
        // a small-order key is refused
        assert!(host.shared([0u8; 32]).is_none());
    }

    #[test]
    fn the_advertised_tag_is_stable_and_reveals_nothing() {
        assert_eq!(session_tag("share-1"), session_tag("share-1"));
        assert_ne!(session_tag("share-1"), session_tag("share-2"));
        assert_eq!(session_tag("x").len(), 16);
    }

    #[test]
    fn messages_survive_a_20_byte_mtu_in_any_order_and_damage_is_caught() {
        let msg: Vec<u8> = (0..190u32).map(|i| (i * 7) as u8).collect();
        let mut parts = fragment(&msg, 20);
        assert!(parts.len() > 8 && parts.iter().all(|p| p.len() <= 20));
        parts.reverse();
        let mut r = Reassembler::default();
        let mut got = None;
        for p in &parts {
            got = r.push(p);
        }
        assert_eq!(got.as_deref(), Some(&msg[..]));
        let mut parts = fragment(&msg, 20);
        parts[3][5] ^= 0x40;
        let mut r = Reassembler::default();
        assert!(parts.iter().filter_map(|p| r.push(p)).next().is_none());
        assert!(Reassembler::default().push(&[9, 3, 1]).is_none());
    }
}
