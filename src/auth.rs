//! Password protection: salted, iterated SHA-256 with a constant-time comparison.
//! Works identically on native and WebAssembly (no OS entropy needed).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ROUNDS: u32 = 60_000;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Credential {
    pub salt: String,
    pub hash: String,
}

pub fn now_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0)
    }
}

fn entropy() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&now_ms().to_le_bytes());
    #[cfg(target_arch = "wasm32")]
    {
        for _ in 0..4 {
            out.extend_from_slice(&js_sys::Math::random().to_le_bytes());
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&std::process::id().to_le_bytes());
        let local = 0u8;
        out.extend_from_slice(&(&local as *const u8 as usize).to_le_bytes());
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn derive(password: &str, salt: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(b":");
    h.update(password.as_bytes());
    let mut cur = h.finalize();
    for _ in 0..ROUNDS {
        let mut h = Sha256::new();
        h.update(cur);
        h.update(salt.as_bytes());
        cur = h.finalize();
    }
    hex(&cur)
}

pub fn create(password: &str) -> Credential {
    let salt = hex(&Sha256::digest(entropy())[..16]);
    let hash = derive(password, &salt);
    Credential { salt, hash }
}

pub fn verify(cred: &Credential, password: &str) -> bool {
    let a = derive(password, &cred.salt);
    let (a, b) = (a.as_bytes(), cred.hash.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Returns a 0..=1 score and a label.
pub fn strength(pw: &str) -> (f32, &'static str) {
    let mut score = 0.0f32;
    let len = pw.chars().count();
    score += (len as f32 / 12.0).min(1.0) * 0.5;
    if pw.chars().any(|c| c.is_ascii_lowercase()) && pw.chars().any(|c| c.is_ascii_uppercase()) {
        score += 0.15;
    }
    if pw.chars().any(|c| c.is_ascii_digit()) {
        score += 0.15;
    }
    if pw.chars().any(|c| !c.is_alphanumeric()) {
        score += 0.2;
    }
    let s = score.min(1.0);
    let label = match s {
        x if x < 0.3 => "Weak",
        x if x < 0.6 => "Okay",
        x if x < 0.85 => "Good",
        _ => "Strong",
    };
    if len == 0 {
        (0.0, "")
    } else {
        (s.max(0.08), label)
    }
}
