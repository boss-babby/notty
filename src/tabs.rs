//! Workspace tabs (NEXT-PASS §3). Pure state — the tab bar lives in `app.rs`.
//!
//! The list is persisted in the app storage (`tabs` key), never inside the workspace, so it is not synced to collaborators.

use serde::{Deserialize, Serialize};

pub const STORAGE_KEY: &str = "lumen_tabs";
pub const MAX_TABS: usize = 24;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tabs {
    pub list: Vec<u64>,
    pub active: usize,
}

impl Tabs {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn active_page(&self) -> Option<u64> {
        self.list.get(self.active).copied()
    }

    /// Activates the tab of `page`, or opens a new one after the active tab.
    pub fn open(&mut self, page: u64) {
        if let Some(i) = self.list.iter().position(|p| *p == page) {
            self.active = i;
            return;
        }
        if self.list.len() >= MAX_TABS {
            self.list.remove(0);
            self.active = self.active.saturating_sub(1);
        }
        let at = if self.list.is_empty() {
            0
        } else {
            (self.active + 1).min(self.list.len())
        };
        self.list.insert(at, page);
        self.active = at;
    }

    /// A second tab showing the same page, right after the active one (the next navigation then fills it).
    pub fn duplicate_active(&mut self) {
        if let Some(p) = self.active_page() {
            if self.list.len() >= MAX_TABS {
                self.list.remove(0);
                self.active = self.active.saturating_sub(1);
            }
            self.list.insert(self.active + 1, p);
            self.active += 1;
        }
    }

    /// Called every frame with the page that is open: the active tab follows navigation (or a tab is created when there is none).
    pub fn follow(&mut self, page: u64) {
        if self.list.is_empty() {
            self.list.push(page);
            self.active = 0;
        } else if self.active_page() != Some(page) {
            if let Some(i) = self.list.iter().position(|p| *p == page) {
                self.active = i; // navigating to a page that already has a tab selects it
            } else {
                self.active = self.active.min(self.list.len() - 1);
                self.list[self.active] = page;
            }
        }
    }

    /// Closes tab `i`; returns the page that should be shown afterwards (a neighbour), `None` when no tab is left.
    pub fn close(&mut self, i: usize) -> Option<u64> {
        if i >= self.list.len() {
            return self.active_page();
        }
        self.list.remove(i);
        if self.list.is_empty() {
            self.active = 0;
            return None;
        }
        if i < self.active || self.active >= self.list.len() {
            self.active = self.active.saturating_sub(1).min(self.list.len() - 1);
        }
        self.active_page()
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab.
    pub fn cycle(&mut self, dir: i32) -> Option<u64> {
        let n = self.list.len();
        if n < 2 {
            return None;
        }
        self.active = (self.active as i64 + dir as i64).rem_euclid(n as i64) as usize;
        self.active_page()
    }

    /// Drag to reorder. The active tab stays the same tab.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.list.len() || to >= self.list.len() || from == to {
            return;
        }
        let page = self.list.remove(from);
        self.list.insert(to, page);
        if self.active == from {
            self.active = to;
        } else if from < self.active && to >= self.active {
            self.active -= 1;
        } else if from > self.active && to <= self.active {
            self.active += 1;
        }
    }

    /// Drops tabs of pages that no longer exist.
    pub fn prune(&mut self, exists: impl Fn(u64) -> bool) {
        let cur = self.active_page();
        self.list.retain(|p| exists(*p));
        self.active = cur
            .and_then(|c| self.list.iter().position(|p| *p == c))
            .unwrap_or(self.active.min(self.list.len().saturating_sub(1)));
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Tabs {
        let mut t: Tabs = serde_json::from_str(s).unwrap_or_default();
        t.list.truncate(MAX_TABS);
        t.active = t.active.min(t.list.len().saturating_sub(1));
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_persist_round_trip() {
        let mut t = Tabs::default();
        for p in [1, 2, 3] {
            t.open(p);
        }
        t.active = 1;
        assert_eq!(Tabs::from_json(&t.to_json()), t);
        assert_eq!(Tabs::from_json("garbage"), Tabs::default());
        assert_eq!(Tabs::from_json(r#"{"list":[7],"active":9}"#).active, 0);
    }

    #[test]
    fn closing_the_active_tab_selects_a_neighbour() {
        let mut t = Tabs::default();
        for p in [10, 20, 30] {
            t.open(p);
        }
        assert_eq!(t.active_page(), Some(30));
        assert_eq!(t.close(2), Some(20));
        assert_eq!(t.close(0), Some(20));
        assert_eq!(t.close(0), None);
        assert!(t.is_empty());
    }

    #[test]
    fn opening_activates_existing_and_inserts_next_to_active() {
        let mut t = Tabs::default();
        t.open(1);
        t.open(2);
        t.open(3);
        t.active = 0;
        t.open(9);
        assert_eq!(t.list, vec![1, 9, 2, 3]);
        t.open(3);
        assert_eq!(t.active_page(), Some(3));
        assert_eq!(t.list.len(), 4);
    }

    #[test]
    fn navigation_replaces_the_active_tab_and_cycling_wraps() {
        let mut t = Tabs::default();
        t.follow(1);
        t.open(2);
        t.follow(5);
        assert_eq!(t.list, vec![1, 5]);
        assert_eq!(t.cycle(1), Some(1));
        assert_eq!(t.cycle(-1), Some(5));
    }

    #[test]
    fn reordering_keeps_the_active_tab() {
        let mut t = Tabs {
            list: vec![1, 2, 3, 4],
            active: 1,
        };
        t.move_tab(0, 3);
        assert_eq!(t.list, vec![2, 3, 4, 1]);
        assert_eq!(t.active_page(), Some(2));
        t.move_tab(3, 0);
        assert_eq!(t.active_page(), Some(2));
        t.prune(|p| p != 3);
        assert_eq!(t.active_page(), Some(2));
    }
}
