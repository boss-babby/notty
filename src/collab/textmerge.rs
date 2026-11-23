//! Three-way merge for concurrent edits of one block (the "text CRDT" of the collaboration layer).
//!
//! Two people typing in the same paragraph used to be resolved by last-writer-wins: the newer *whole block* replaced the older one and
//! the other person's keystrokes vanished. Every operation now carries the content hash of the value it was derived from (`Entry::base`);
//! when an incoming operation is *concurrent* with the local state (its base is not what we hold) and the base text is still in the
//! replica's short history, the block is merged field by field:
//!
//! * a field only one side changed → that change wins;
//! * a text field both sides changed → merge through the text CRDT ([`merge3`]): edits in different places are both kept,
//!   an identical edit is applied once, overlapping edits are ordered by the RGA rule (identical on every device);
//! * any other field both sides changed → the side with the newer clock wins (as before).
//!
//! The merge is symmetric for non-overlapping edits, so every peer computes the same merged value and the system converges without
//! an extra round of negotiation. A ping-pong limiter in the replica stops a pathological case from looping.
use serde_json::{Map, Value};

/// Character-level three-way merge of `a` and `b`, both derived from `base` — now computed by the text CRDT ([`super::textcrdt`]): the
/// result is the same on every device and keeps both people's edits, also when they overlap.
pub fn merge3(base: &str, a: &str, b: &str) -> String {
    if a == b || b == base {
        return a.to_owned();
    }
    if a == base {
        return b.to_owned();
    }
    super::textcrdt::merge3(base, a, b)
}

/// Field-wise merge of three JSON objects (one entity each). `remote_wins` decides conflicts on non-text fields.
/// Returns `None` when any of them is not an object.
pub fn merge_json(base: &str, local: &str, remote: &str, remote_wins: bool) -> Option<String> {
    let b: Value = serde_json::from_str(base).ok()?;
    let l: Value = serde_json::from_str(local).ok()?;
    let r: Value = serde_json::from_str(remote).ok()?;
    let (Value::Object(b), Value::Object(l), Value::Object(r)) = (b, l, r) else {
        return None;
    };
    let mut keys: Vec<&String> = b.keys().chain(l.keys()).chain(r.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Map::new();
    for k in keys {
        let (bv, lv, rv) = (b.get(k), l.get(k), r.get(k));
        let v = if lv == rv {
            lv
        } else if lv == bv {
            rv
        } else if rv == bv {
            lv
        } else if let (Some(Value::String(bs)), Some(Value::String(ls)), Some(Value::String(rs))) =
            (bv, lv, rv)
        {
            out.insert(k.clone(), Value::String(merge3(bs, ls, rs)));
            continue;
        } else if remote_wins {
            rv
        } else {
            lv
        };
        if let Some(v) = v {
            out.insert(k.clone(), v.clone());
        }
    }
    serde_json::to_string(&Value::Object(out)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_in_different_places_are_both_kept() {
        assert_eq!(
            merge3("hello world", "Hello world", "hello world!"),
            "Hello world!"
        );
        assert_eq!(
            merge3("the cat sat", "the big cat sat", "the cat sat down"),
            "the big cat sat down"
        );
    }

    #[test]
    fn merge_is_symmetric() {
        let cases = [
            ("abc def", "abcX def", "abc Ydef"),
            ("one", "one two", "zero one"),
            ("same", "same!", "same!"),
            ("abcdef", "abXef", "abYef"),
            ("", "a", "b"),
        ];
        for (b, x, y) in cases {
            assert_eq!(merge3(b, x, y), merge3(b, y, x), "{b:?} {x:?} {y:?}");
        }
    }

    #[test]
    fn identical_edits_apply_once_and_deletes_win_over_nothing() {
        assert_eq!(merge3("abc", "abXc", "abXc"), "abXc");
        assert_eq!(merge3("a b c", "a c", "a b c!"), "a c!");
    }

    #[test]
    fn json_fields_merge_independently() {
        let base = r#"{"text":"todo","checked":false,"color":0}"#;
        let local = r#"{"text":"todo list","checked":false,"color":0}"#;
        let remote = r#"{"text":"my todo","checked":true,"color":0}"#;
        let m: Value =
            serde_json::from_str(&merge_json(base, local, remote, true).unwrap()).unwrap();
        assert_eq!(m["text"], "my todo list");
        assert_eq!(m["checked"], true);
    }
}
