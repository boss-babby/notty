//! OS key store (NEXT-PASS §6): "Remember on this device".
//!
//! The vault password is never stored. When the user chooses *Remember this device*, a random 32-byte **device token** is put into the
//! OS credential store (Windows Credential Manager / Android Keystore-wrapped file) and the workspace file keeps only
//! `seal(token, Argon2id hash)`. A copied workspace file is useless without the user profile / phone; on this device the lock screen opens
//! by itself. The browser has no key store, so the password stays the only way in there.
//!
//! `seal` / `open` are pure (ChaCha20-Poly1305, random 96-bit nonce prepended) and unit-tested; the OS calls hide behind [`KeyStore`].

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

const NONCE: usize = 12;

/// Random bytes from the OS.
pub fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    if getrandom::getrandom(&mut k).is_err() {
        // never reached on supported platforms; refuse to hand out a predictable key
        k = [0u8; 32];
    }
    k
}

/// `nonce(12) ‖ ciphertext+tag`.
pub fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE];
    let _ = getrandom::getrandom(&mut nonce);
    seal_with_nonce(key, &nonce, plain)
}

fn seal_with_nonce(key: &[u8; 32], nonce: &[u8; NONCE], plain: &[u8]) -> Vec<u8> {
    let c = ChaCha20Poly1305::new(Key::from_slice(key));
    let ct = c
        .encrypt(Nonce::from_slice(nonce), plain)
        .unwrap_or_default();
    let mut out = Vec::with_capacity(NONCE + ct.len());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    out
}

/// `None` for a wrong key or damaged data.
pub fn open(key: &[u8; 32], blob: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < NONCE + 16 {
        return None;
    }
    let (n, ct) = blob.split_at(NONCE);
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(n), ct)
        .ok()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// A place the OS protects (per user profile / per device).
pub trait KeyStore {
    fn load(&self, name: &str) -> Option<Vec<u8>>;
    fn store(&self, name: &str, secret: &[u8]) -> bool;
    fn forget(&self, name: &str);
}

/// Name of the device token of this app's vault.
pub const VAULT: &str = "Lumen/vault";

#[cfg(windows)]
pub struct CredMan;

#[cfg(windows)]
impl CredMan {
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(windows)]
impl KeyStore for CredMan {
    fn load(&self, name: &str) -> Option<Vec<u8>> {
        use windows_sys::Win32::Security::Credentials::{
            CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        };
        let target = Self::wide(name);
        let mut p: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target` is NUL-terminated; on success `p` points to a credential owned by the OS that is released with CredFree.
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 || p.is_null() {
                return None;
            }
            let c = &*p;
            let v = if c.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            };
            CredFree(p as *const _);
            Some(v)
        }
    }

    fn store(&self, name: &str, secret: &[u8]) -> bool {
        use windows_sys::Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        };
        let mut target = Self::wide(name);
        let mut user = Self::wide("Lumen");
        let mut blob = secret.to_vec();
        // SAFETY: an all-zero CREDENTIALW is a valid "empty" value; the pointers stay valid for the call (CredWriteW copies the data).
        unsafe {
            let mut c: CREDENTIALW = std::mem::zeroed();
            c.Type = CRED_TYPE_GENERIC;
            c.TargetName = target.as_mut_ptr();
            c.UserName = user.as_mut_ptr();
            c.CredentialBlobSize = blob.len() as u32;
            c.CredentialBlob = blob.as_mut_ptr();
            c.Persist = CRED_PERSIST_LOCAL_MACHINE;
            CredWriteW(&c, 0) != 0
        }
    }

    fn forget(&self, name: &str) {
        use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
        let target = Self::wide(name);
        // SAFETY: `target` is NUL-terminated.
        unsafe {
            CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0);
        }
    }
}

/// Android: the token is wrapped by an AES-GCM key that lives in the Android Keystore (never leaves the secure hardware / TEE) and the
/// wrapped bytes are kept in a private file.
#[cfg(target_os = "android")]
pub struct AndroidKeys;

#[cfg(target_os = "android")]
impl AndroidKeys {
    fn path(name: &str) -> Option<std::path::PathBuf> {
        let d = std::env::var_os("LUMEN_DATA_DIR").map(std::path::PathBuf::from)?;
        Some(d.join("keys").join(name.replace('/', "_")))
    }
}

#[cfg(target_os = "android")]
impl KeyStore for AndroidKeys {
    fn load(&self, name: &str) -> Option<Vec<u8>> {
        let wrapped = std::fs::read(Self::path(name)?).ok()?;
        crate::android::key_unwrap(&wrapped)
    }
    fn store(&self, name: &str, secret: &[u8]) -> bool {
        let (Some(p), Some(w)) = (Self::path(name), crate::android::key_wrap(secret)) else {
            return false;
        };
        p.parent()
            .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(p, w).is_ok()
    }
    fn forget(&self, name: &str) {
        if let Some(p) = Self::path(name) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// The key store of this platform, if there is one.
pub fn os() -> Option<Box<dyn KeyStore>> {
    #[cfg(windows)]
    {
        Some(Box::new(CredMan))
    }
    #[cfg(target_os = "android")]
    {
        Some(Box::new(AndroidKeys))
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        None
    }
}

/// Whether "Remember on this device" can work here.
pub fn available() -> bool {
    os().is_some()
}

/// Creates a device token, stores it in the OS store and returns what the workspace file keeps (`hex(seal(token, secret))`).
pub fn enroll(store: &dyn KeyStore, name: &str, secret: &[u8]) -> Option<String> {
    let token = random_key();
    if token == [0u8; 32] || !store.store(name, &token) {
        return None;
    }
    Some(to_hex(&seal(&token, secret)))
}

/// The protected secret, if this device holds the token.
pub fn unlock(store: &dyn KeyStore, name: &str, sealed_hex: &str) -> Option<Vec<u8>> {
    let token: [u8; 32] = store.load(name)?.try_into().ok()?;
    open(&token, &from_hex(sealed_hex)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Mem(RefCell<HashMap<String, Vec<u8>>>);
    impl KeyStore for Mem {
        fn load(&self, n: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(n).cloned()
        }
        fn store(&self, n: &str, s: &[u8]) -> bool {
            self.0.borrow_mut().insert(n.into(), s.to_vec());
            true
        }
        fn forget(&self, n: &str) {
            self.0.borrow_mut().remove(n);
        }
    }

    #[test]
    fn seal_open_round_trip_with_a_fixed_key() {
        let key = [7u8; 32];
        let blob = seal_with_nonce(&key, &[1u8; 12], b"argon2id hash");
        assert_eq!(&blob[..12], &[1u8; 12]);
        assert_eq!(open(&key, &blob).as_deref(), Some(&b"argon2id hash"[..]));
        assert!(open(&[8u8; 32], &blob).is_none(), "wrong key");
        let mut bad = blob.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open(&key, &bad).is_none(), "tampered");
        assert!(open(&key, &blob[..10]).is_none(), "truncated");
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(
            from_hex(&to_hex(&[0, 1, 254, 255])),
            Some(vec![0, 1, 254, 255])
        );
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
    }

    #[test]
    fn enroll_then_unlock_on_the_same_device_only() {
        let here = Mem::default();
        let sealed = enroll(&here, VAULT, b"secret-hash").unwrap();
        assert_eq!(
            unlock(&here, VAULT, &sealed).as_deref(),
            Some(&b"secret-hash"[..])
        );
        let elsewhere = Mem::default();
        assert!(
            unlock(&elsewhere, VAULT, &sealed).is_none(),
            "a copied workspace file is useless without the token"
        );
        here.forget(VAULT);
        assert!(unlock(&here, VAULT, &sealed).is_none());
    }
}
