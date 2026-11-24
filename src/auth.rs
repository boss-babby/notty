//! Password protection: salted **Argon2id** (memory-hard, RFC 9106) with a constant-time comparison; credentials created by older
//! versions (salted, iterated SHA-256) still verify and are upgraded transparently the next time the password is typed.
//! Pure Rust: identical on Windows, Android and WebAssembly.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ROUNDS: u32 = 60_000;

/// `Credential::kdf` values: 0 = legacy iterated SHA-256, 2 = Argon2id.
pub const KDF_SHA256: u8 = 0;
pub const KDF_ARGON2ID: u8 = 2;

/// Argon2id cost: 19 MiB, 2 passes, 1 lane (OWASP minimum profile — about 0.1–0.3 s on a phone, small enough for WebAssembly).
const ARGON_M_KIB: u32 = 19_456;
const ARGON_T: u32 = 2;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Credential {
    pub salt: String,
    pub hash: String,
    /// Key-derivation function (see `KDF_*`); files written before 15.x have none = 0.
    #[serde(default)]
    pub kdf: u8,
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

fn csprng_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).expect("OS CSPRNG unavailable");
    buf
}

/// `n_bytes` of cryptographically secure random hex (OS CSPRNG / Web Crypto) — API / bridge tokens & salts.
pub fn random_hex(n_bytes: usize) -> String {
    hex(&csprng_bytes(n_bytes))
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

fn derive_argon2id(password: &str, salt: &str) -> String {
    use argon2::{Algorithm, Argon2, Params, Version};
    let mut out = [0u8; 32];
    // constant, valid parameters: Params::new only fails for out-of-range values
    let params = Params::new(ARGON_M_KIB, ARGON_T, 1, Some(out.len())).expect("valid Argon2 parameters");
    let kdf = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    if kdf.hash_password_into(password.as_bytes(), salt.as_bytes(), &mut out).is_err() {
        return String::new();
    }
    hex(&out)
}

pub fn create(password: &str) -> Credential {
    let salt = random_hex(16);
    let hash = derive_argon2id(password, &salt);
    Credential { salt, hash, kdf: KDF_ARGON2ID }
}

/// True when the credential still uses the old SHA-256 chain (re-create it with [`create`] after a successful [`verify`]).
pub fn is_legacy(cred: &Credential) -> bool {
    cred.kdf != KDF_ARGON2ID
}

pub fn verify(cred: &Credential, password: &str) -> bool {
    let a = if cred.kdf == KDF_ARGON2ID { derive_argon2id(password, &cred.salt) } else { derive(password, &cred.salt) };
    let (a, b) = (a.as_bytes(), cred.hash.as_bytes());
    if a.is_empty() || a.len() != b.len() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2id_round_trip() {
        let c = create("correct horse");
        assert_eq!(c.kdf, KDF_ARGON2ID);
        assert!(!is_legacy(&c));
        assert!(verify(&c, "correct horse"));
        assert!(!verify(&c, "correct horsf"));
        assert!(!verify(&c, ""));
    }

    #[test]
    fn legacy_credentials_still_unlock_and_are_flagged() {
        let salt = random_hex(16);
        let old = Credential { hash: derive("hunter22", &salt), salt, kdf: KDF_SHA256 };
        assert!(is_legacy(&old));
        assert!(verify(&old, "hunter22"));
        assert!(!verify(&old, "hunter23"));
        // a credential file written by an older build has no `kdf` key
        let json = format!("{{\"salt\":\"{}\",\"hash\":\"{}\"}}", old.salt, old.hash);
        let parsed: Credential = serde_json::from_str(&json).expect("legacy json");
        assert!(is_legacy(&parsed) && verify(&parsed, "hunter22"));
    }
}
