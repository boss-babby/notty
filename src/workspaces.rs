//! Multiple workspaces.
//!
//! The *active* workspace always lives in `LumenApp::ws` and under the classic storage key (so every older install keeps working and the
//! data-safety slots keep protecting it). The others are kept as serialised JSON next to it (`lumen_ws_<id>`): switching serialises the
//! active workspace, parses the target and swaps — no storage access is needed at that moment, and an inactive workspace costs only its
//! text in memory. The registry (`lumen_workspaces`) lists them in order.
use serde::{Deserialize, Serialize};

pub const REG_KEY: &str = "lumen_workspaces";

pub fn key(id: u64) -> String {
    format!("lumen_ws_{id}")
}

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Meta {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub struct Registry {
    pub active: u64,
    pub next: u64,
    pub list: Vec<Meta>,
    /// Deleted workspaces whose stored text still has to be blanked on the next save.
    #[serde(default)]
    pub removed: Vec<u64>,
}

impl Registry {
    /// Makes the registry consistent: at least one entry, and `active` points at an existing one.
    pub fn ensure(&mut self, active_name: &str) {
        if self.list.is_empty() {
            self.list.push(Meta { id: 1, name: active_name.to_owned() });
            self.active = 1;
        }
        if !self.list.iter().any(|m| m.id == self.active) {
            self.active = self.list[0].id;
        }
        let max = self.list.iter().map(|m| m.id).max().unwrap_or(0);
        self.next = self.next.max(max + 1);
    }

    pub fn sync_active_name(&mut self, name: &str) {
        let a = self.active;
        if let Some(m) = self.list.iter_mut().find(|m| m.id == a) {
            if m.name != name {
                m.name = name.to_owned();
            }
        }
    }

    pub fn add(&mut self, name: &str) -> u64 {
        let id = self.next.max(1);
        self.next = id + 1;
        self.list.push(Meta { id, name: name.to_owned() });
        id
    }

    pub fn remove(&mut self, id: u64) {
        self.list.retain(|m| m.id != id);
        self.removed.push(id);
    }

    /// "Workspace 2", "Workspace 3" … the first name that is not taken.
    pub fn fresh_name(&self) -> String {
        (self.list.len() + 1..)
            .map(|n| format!("Workspace {n}"))
            .find(|n| !self.list.iter().any(|m| &m.name == n))
            .unwrap_or_else(|| "Workspace".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_repaired_and_ids_are_unique() {
        let mut r = Registry::default();
        r.ensure("My Workspace");
        assert_eq!((r.active, r.list.len()), (1, 1));
        let b = r.add("Work");
        let c = r.add("Home");
        assert!(b != c && b != 1 && c != 1);
        r.remove(b);
        assert_eq!(r.list.len(), 2);
        assert_eq!(r.removed, vec![b]);
        r.active = 999;
        r.ensure("x");
        assert_eq!(r.active, 1);
        assert!(r.fresh_name().starts_with("Workspace "));
    }

    #[test]
    fn blank_workspace_round_trips_through_json() {
        let ws = crate::model::Workspace::blank("Side project", true, 2);
        let text = serde_json::to_string(&ws).unwrap();
        let back: crate::model::Workspace = serde_json::from_str(&text).unwrap();
        assert_eq!(back.name, "Side project");
        assert!(back.dark && back.accent == 2);
        assert_eq!(back.pages.iter().filter(|p| !p.trashed).count(), 1);
    }
}
