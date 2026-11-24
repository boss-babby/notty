//! Access control for a shared document: signed fail-over plans, revocable / expiring share links ("grants") and the election quorum.
//!
//! * **Host key.** Every device derives a long-term Ed25519 signing key from its device key (`HostKey::from_device`). A host signs the
//!   fail-over plan (`Msg::Standby`) with it; joiners pin the host's public key from `Welcome` and ignore a plan that is unsigned or
//!   signed by somebody else — a device that only knows the share code (or sits between the peers) cannot forge a successor.
//! * **Grants.** A grant is a link of its own: its token, the role it gives (Editor / Suggester / Viewer), an optional expiry, an optional
//!   number of uses and a label. The host accepts a `Hello` whose proof matches an active grant and gives exactly that role without a
//!   request card. A grant is revoked with one click; the live connections that used it are dropped. The list travels, signed, inside the
//!   fail-over plan to the editors (they may have to enforce it after a hand-over), never to viewers.
//! * **Quorum.** With three or more editors in the plan a successor may only act as the authority of the session (approve people, change
//!   roles, mint links) once a majority of the editors has reconnected to it; otherwise it steps down again. This is what stops two halves
//!   of a partitioned network from both becoming "the" host.
use super::{hex, random_code, secure, unhex, Role};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub struct HostKey(SigningKey);

impl HostKey {
    /// The signing key is a hash of the device's X25519 private key and a label — nothing new to store or back up.
    pub fn from_device(private: &[u8]) -> HostKey {
        let mut h = Sha256::new();
        h.update(b"lumen/host-signing-key/v1");
        h.update(private);
        let seed: [u8; 32] = h.finalize().into();
        HostKey(SigningKey::from_bytes(&seed))
    }

    pub fn public_hex(&self) -> String {
        hex(self.0.verifying_key().as_bytes())
    }

    pub fn sign_hex(&self, msg: &[u8]) -> String {
        hex(&self.0.sign(msg).to_bytes())
    }
}

/// `true` when `sig` (hex) is a valid signature of `msg` by the public key `pk` (hex).
pub fn verify(pk: &str, msg: &[u8], sig: &str) -> bool {
    let (Some(pk), Some(sig)) = (unhex(pk), unhex(sig)) else {
        return false;
    };
    let (Ok(pk), Ok(sig)) = (
        <[u8; 32]>::try_from(pk.as_slice()),
        <[u8; 64]>::try_from(sig.as_slice()),
    ) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    vk.verify(msg, &Signature::from_bytes(&sig)).is_ok()
}

/// The bytes the host signs: everything in the plan except the signature itself (a length-prefixed encoding, no ambiguity).
#[allow(clippy::too_many_arguments)]
pub fn plan_bytes(
    host: u32,
    order: &[u32],
    addrs: &[(u32, Vec<String>)],
    secret: &str,
    cap: &str,
    relay: &Option<(String, String)>,
    grants: &str,
    seq: u64,
) -> Vec<u8> {
    let mut out = b"lumen/standby/v2".to_vec();
    let mut put = |s: &[u8]| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s);
    };
    put(&host.to_le_bytes());
    put(&seq.to_le_bytes());
    for o in order {
        put(&o.to_le_bytes());
    }
    for (id, a) in addrs {
        put(&id.to_le_bytes());
        for x in a {
            put(x.as_bytes());
        }
        put(b"|");
    }
    put(secret.as_bytes());
    put(cap.as_bytes());
    if let Some((b, r)) = relay {
        put(b.as_bytes());
        put(r.as_bytes());
    }
    put(grants.as_bytes());
    out
}

/// Majority of `n` voters.
pub fn quorum(n: usize) -> usize {
    n / 2 + 1
}

/// The quorum rule only applies from three editors on (with two, a majority is both of them and a single failure would block the session).
pub fn needs_quorum(editors: usize) -> bool {
    editors >= 3
}

pub const MAX_GRANTS: usize = 32;

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Grant {
    pub id: String,
    pub token: String,
    pub role: Role,
    pub label: String,
    /// ms since the epoch; 0 = never expires.
    pub expires: u64,
    /// 0 = unlimited.
    pub max_uses: u32,
    pub uses: u32,
    pub revoked: bool,
    pub made: u64,
}

impl Grant {
    pub fn live(&self, now: u64) -> bool {
        !self.revoked
            && (self.expires == 0 || now < self.expires)
            && (self.max_uses == 0 || self.uses < self.max_uses)
    }

    /// Human text for the list: "Edit · expires in 2 h · 1 / 3 uses".
    pub fn describe(&self, now: u64) -> String {
        let role = match self.role {
            Role::Editor => "edit",
            Role::Viewer => "view",
            Role::Suggester => "suggest",
        };
        let state = if self.revoked {
            "revoked".to_owned()
        } else if self.expires != 0 && now >= self.expires {
            "expired".to_owned()
        } else if self.expires != 0 {
            let s = (self.expires - now) / 1000;
            if s >= 86_400 {
                format!("{} d left", s / 86_400)
            } else if s >= 3600 {
                format!("{} h left", s / 3600)
            } else {
                format!("{} min left", (s / 60).max(1))
            }
        } else {
            "no expiry".to_owned()
        };
        let uses = if self.max_uses == 0 {
            String::new()
        } else {
            format!(" · {} / {} uses", self.uses, self.max_uses)
        };
        format!(
            "{} · {role} · {state}{uses}",
            if self.label.is_empty() {
                "Link"
            } else {
                &self.label
            }
        )
    }
}

#[derive(Default, Clone, Debug)]
pub struct Grants {
    pub list: Vec<Grant>,
}

impl Grants {
    pub fn create(
        &mut self,
        role: Role,
        label: &str,
        ttl_ms: u64,
        max_uses: u32,
        now: u64,
    ) -> Option<Grant> {
        self.prune(now);
        if self.list.len() >= MAX_GRANTS {
            return None;
        }
        let g = Grant {
            id: random_code(6),
            token: random_code(10),
            role,
            label: label.chars().filter(|c| !c.is_control()).take(40).collect(),
            expires: if ttl_ms == 0 {
                0
            } else {
                now.saturating_add(ttl_ms)
            },
            max_uses,
            uses: 0,
            revoked: false,
            made: now,
        };
        self.list.push(g.clone());
        Some(g)
    }

    /// Revokes the grant; returns its token so the caller can drop the connections that came in with it.
    pub fn revoke(&mut self, id: &str) -> Option<String> {
        let g = self.list.iter_mut().find(|g| g.id == id)?;
        g.revoked = true;
        Some(g.token.clone())
    }

    /// The live grant a `Hello` proof belongs to (`hash` = the channel hash the proof is bound to).
    pub fn matching(&self, auth: &str, hash: &[u8], now: u64) -> Option<usize> {
        if auth.is_empty() {
            return None;
        }
        self.list
            .iter()
            .position(|g| g.live(now) && secure::ct_eq(auth, &secure::proof("c", &g.token, hash)))
    }

    pub fn consume(&mut self, idx: usize) {
        if let Some(g) = self.list.get_mut(idx) {
            g.uses = g.uses.saturating_add(1);
        }
    }

    pub fn token_live(&self, token: &str, now: u64) -> bool {
        self.list.iter().any(|g| g.token == token && g.live(now))
    }

    /// Forgets grants that ended more than a day ago.
    pub fn prune(&mut self, now: u64) {
        self.list.retain(|g| {
            g.live(now)
                || now.saturating_sub(g.expires.max(g.made)) < 86_400_000
                || (g.revoked && now.saturating_sub(g.made) < 86_400_000)
        });
    }

    pub fn live_json(&self, now: u64) -> String {
        let live: Vec<&Grant> = self.list.iter().filter(|g| g.live(now)).collect();
        serde_json::to_string(&live).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Grants {
        let mut list: Vec<Grant> = serde_json::from_str(s).unwrap_or_default();
        list.truncate(MAX_GRANTS);
        for g in &mut list {
            g.label = g
                .label
                .chars()
                .filter(|c| !c.is_control())
                .take(40)
                .collect();
        }
        Grants { list }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_signature_binds_every_field() {
        let key = HostKey::from_device(&[7u8; 32]);
        let other = HostKey::from_device(&[8u8; 32]);
        let relay = Some(("https://x".to_owned(), "ROOM".to_owned()));
        let msg = plan_bytes(
            5,
            &[6, 7],
            &[(6, vec!["10.0.0.2:47706".into()])],
            "secret",
            "cap",
            &relay,
            "[]",
            3,
        );
        let sig = key.sign_hex(&msg);
        assert!(verify(&key.public_hex(), &msg, &sig));
        assert!(
            !verify(&other.public_hex(), &msg, &sig),
            "another key must not verify"
        );
        let tampered = [
            plan_bytes(
                5,
                &[7, 6],
                &[(6, vec!["10.0.0.2:47706".into()])],
                "secret",
                "cap",
                &relay,
                "[]",
                3,
            ),
            plan_bytes(
                5,
                &[6, 7],
                &[(6, vec!["10.0.0.9:47706".into()])],
                "secret",
                "cap",
                &relay,
                "[]",
                3,
            ),
            plan_bytes(
                5,
                &[6, 7],
                &[(6, vec!["10.0.0.2:47706".into()])],
                "secret",
                "cap",
                &relay,
                "[]",
                4,
            ),
            plan_bytes(
                5,
                &[6, 7],
                &[(6, vec!["10.0.0.2:47706".into()])],
                "secret",
                "cap",
                &relay,
                "[{\"x\":1}]",
                3,
            ),
            plan_bytes(
                5,
                &[6, 7],
                &[(6, vec!["10.0.0.2:47706".into()])],
                "secret",
                "cap",
                &None,
                "[]",
                3,
            ),
        ];
        for t in tampered {
            assert!(!verify(&key.public_hex(), &t, &sig));
        }
        assert!(!verify("zz", &msg, &sig) && !verify(&key.public_hex(), &msg, "00"));
        assert_eq!(
            key.public_hex(),
            HostKey::from_device(&[7u8; 32]).public_hex(),
            "the key is derived, not random"
        );
    }

    #[test]
    fn grants_expire_run_out_and_can_be_revoked() {
        let mut g = Grants::default();
        let hash = [9u8; 32];
        let a = g
            .create(Role::Viewer, "Reviewer", 60_000, 0, 1_000)
            .unwrap();
        let b = g.create(Role::Editor, "", 0, 2, 1_000).unwrap();
        let proof_a = secure::proof("c", &a.token, &hash);
        let proof_b = secure::proof("c", &b.token, &hash);
        assert_eq!(g.matching(&proof_a, &hash, 2_000), Some(0));
        assert_eq!(g.matching(&proof_a, &hash, 70_000), None, "expired");
        assert_eq!(g.matching(&proof_b, &hash, 2_000), Some(1));
        g.consume(1);
        g.consume(1);
        assert_eq!(g.matching(&proof_b, &hash, 2_000), None, "used up");
        assert_eq!(g.matching("", &hash, 2_000), None);
        let c = g.create(Role::Suggester, "x", 0, 0, 3_000).unwrap();
        let proof_c = secure::proof("c", &c.token, &hash);
        assert!(g.matching(&proof_c, &hash, 4_000).is_some());
        assert_eq!(g.revoke(&c.id), Some(c.token.clone()));
        assert!(g.matching(&proof_c, &hash, 4_000).is_none(), "revoked");
        assert!(!g.token_live(&c.token, 4_000));
        // the proof is bound to the channel: a replay on another channel fails
        assert_eq!(g.matching(&proof_a, &[1u8; 32], 2_000), None);
        // the plan carries only the live ones and round-trips
        let json = g.live_json(2_000);
        let back = Grants::from_json(&json);
        assert_eq!(back.list.len(), 1);
        assert_eq!(back.list[0].role, Role::Viewer);
        assert!(Grants::from_json("not json").list.is_empty());
        assert!(a.describe(2_000).contains("view") && b.describe(2_000).contains("uses"));
    }

    #[test]
    fn the_quorum_is_a_strict_majority_and_only_for_three_or_more() {
        assert_eq!(quorum(3), 2);
        assert_eq!(quorum(4), 3);
        assert_eq!(quorum(5), 3);
        assert!(!needs_quorum(2) && needs_quorum(3));
    }

    #[test]
    fn at_most_32_grants() {
        let mut g = Grants::default();
        for _ in 0..MAX_GRANTS {
            assert!(g.create(Role::Viewer, "", 0, 0, 10).is_some());
        }
        assert!(g.create(Role::Viewer, "", 0, 0, 10).is_none());
    }
}
