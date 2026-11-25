//! User templates and daily notes (NEXT-PASS §5 + §8).
//!
//! * "Save page as template" stores a copy of the page body ([`UserTemplate`]); the gallery, the `/template` command and the
//!   "daily note template" setting use them. New pages get fresh block ids.
//! * Daily notes: [`Workspace::open_day`] opens (creating) the entry of *today + offset* inside the "Journal" page; the page itself
//!   shows Yesterday / Tomorrow chevrons ([`Workspace::journal_day`]).

use serde::{Deserialize, Serialize};

use crate::dates;
use crate::model::{Block, BlockKind, PageKind, Workspace};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct UserTemplate {
    pub id: u64,
    pub name: String,
    pub icon: String,
    pub blocks: Vec<Block>,
}

/// "March 5, 2024" for a day number (the title of a journal entry).
pub fn long_date(day: i64) -> String {
    let (y, m, d) = dates::civil_from_days(day);
    format!("{} {d}, {y}", dates::MONTHS[(m as usize - 1).min(11)])
}

/// Inverse of [`long_date`].
pub fn parse_long_date(s: &str) -> Option<i64> {
    let (month, rest) = s.trim().split_once(' ')?;
    let (day, year) = rest.split_once(", ")?;
    let m = dates::MONTHS
        .iter()
        .position(|n| n.eq_ignore_ascii_case(month))? as u32
        + 1;
    let (d, y): (u32, i32) = (day.trim().parse().ok()?, year.trim().parse().ok()?);
    if !(1..=31).contains(&d) {
        return None;
    }
    let days = dates::days_from_civil(y, m, d);
    (long_date(days) == s.trim()).then_some(days)
}

impl Workspace {
    fn journal_root(&self) -> Option<u64> {
        self.pages
            .iter()
            .find(|p| p.title == "Journal" && p.parent.is_none() && !p.trashed && !p.row)
            .map(|p| p.id)
    }

    /// The same blocks with brand-new ids (a template can be used any number of times).
    fn fresh_blocks(&mut self, blocks: &[Block]) -> Vec<Block> {
        blocks
            .iter()
            .map(|b| {
                let mut n = b.clone();
                n.id = self.id();
                n
            })
            .collect()
    }

    /// Stores the page body as a user template. `None` for unknown pages.
    pub fn save_as_template(&mut self, page: u64) -> Option<u64> {
        let p = self.page(page)?;
        let name = if p.title.trim().is_empty() {
            "Untitled template".to_owned()
        } else {
            p.title.clone()
        };
        let icon = if p.icon.is_empty() {
            "📄".to_owned()
        } else {
            p.icon.clone()
        };
        let blocks = p.blocks.clone();
        let id = self.id();
        self.user_templates.push(UserTemplate {
            id,
            name,
            icon,
            blocks,
        });
        Some(id)
    }

    pub fn delete_user_template(&mut self, id: u64) {
        self.user_templates.retain(|t| t.id != id);
        if self.daily_template == Some(id) {
            self.daily_template = None;
        }
    }

    /// New page from a user template (title and icon are the template's).
    pub fn add_from_user_template(&mut self, tid: u64, parent: Option<u64>) -> Option<u64> {
        let t = self.user_templates.iter().find(|t| t.id == tid)?.clone();
        let id = self.add_page(parent, PageKind::Doc);
        let blocks = self.fresh_blocks(&t.blocks);
        if let Some(p) = self.page_mut(id) {
            p.title = t.name;
            p.icon = t.icon;
            if !blocks.is_empty() {
                p.blocks = blocks;
            }
        }
        Some(id)
    }

    /// Replaces the body of `page` with a fresh copy of the user template (used by daily notes).
    fn fill_from_user_template(&mut self, page: u64, tid: u64) -> bool {
        let Some(t) = self.user_templates.iter().find(|t| t.id == tid).cloned() else {
            return false;
        };
        let blocks = self.fresh_blocks(&t.blocks);
        match self.page_mut(page) {
            Some(p) if !blocks.is_empty() => {
                p.blocks = blocks;
                true
            }
            _ => false,
        }
    }

    /// Opens (creating when needed) the journal entry of `today + offset` days.
    pub fn open_day(&mut self, offset: i64) {
        let root = match self.journal_root() {
            Some(r) => r,
            None => {
                let r = self.add_page(None, PageKind::Doc);
                let intro = self.blk(
                    BlockKind::Callout,
                    "Your daily journal. Press Ctrl+J or Alt+D any time to open today's entry.",
                    0,
                );
                if let Some(p) = self.page_mut(r) {
                    p.title = "Journal".into();
                    p.icon = "📖".into();
                    p.blocks.insert(0, intro);
                }
                r
            }
        };
        let title = long_date(dates::today_day().saturating_add(offset));
        if let Some(e) = self
            .pages
            .iter()
            .find(|p| p.parent == Some(root) && p.title == title && !p.trashed)
        {
            self.current = Some(e.id);
            return;
        }
        let e = self.add_page(Some(root), PageKind::Doc);
        let custom = self
            .daily_template
            .is_some_and(|t| self.fill_from_user_template(e, t));
        if !custom {
            self.fill_template_doc(e, 5);
        }
        if let Some(p) = self.page_mut(e) {
            p.title = title;
            p.icon = "📖".into();
        }
        self.current = Some(e);
    }

    /// Day number of a journal entry page (a child of "Journal" titled like "March 5, 2024").
    pub fn journal_day(&self, page: u64) -> Option<i64> {
        let p = self.page(page)?;
        let root = self.journal_root()?;
        if p.parent != Some(root) || p.trashed {
            return None;
        }
        parse_long_date(&p.title)
    }

    /// Opens the journal entry `delta` days away from the entry that is open now.
    pub fn open_journal_neighbour(&mut self, page: u64, delta: i64) {
        if let Some(day) = self.journal_day(page) {
            let offset = day + delta - dates::today_day();
            self.open_day(offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_dates_round_trip() {
        let d = dates::days_from_civil(2024, 3, 5);
        assert_eq!(long_date(d), "March 5, 2024");
        assert_eq!(parse_long_date("March 5, 2024"), Some(d));
        assert_eq!(parse_long_date("March 32, 2024"), None);
        assert_eq!(parse_long_date("Journal"), None);
        assert_eq!(parse_long_date("Febuary 3, 2024"), None);
    }

    #[test]
    fn template_round_trip_gives_fresh_ids() {
        let mut ws = Workspace::seed();
        let p = ws.add_page(None, PageKind::Doc);
        let b = ws.blk(BlockKind::H2, "Agenda", 0);
        ws.page_mut(p).unwrap().blocks = vec![b];
        ws.page_mut(p).unwrap().title = "Weekly".into();
        let t = ws.save_as_template(p).unwrap();
        let a = ws.add_from_user_template(t, None).unwrap();
        let c = ws.add_from_user_template(t, None).unwrap();
        let (pa, pc) = (ws.page(a).unwrap(), ws.page(c).unwrap());
        assert_eq!(pa.title, "Weekly");
        assert_eq!(pa.blocks[0].text, "Agenda");
        assert_ne!(pa.blocks[0].id, pc.blocks[0].id);
        ws.delete_user_template(t);
        assert!(ws.user_templates.is_empty());
        assert!(ws.add_from_user_template(t, None).is_none());
    }

    #[test]
    fn daily_notes_open_neighbours_and_use_the_chosen_template() {
        let mut ws = Workspace::seed();
        let p = ws.add_page(None, PageKind::Doc);
        let b = ws.blk(BlockKind::Bullet, "Gratitude", 0);
        ws.page_mut(p).unwrap().blocks = vec![b];
        let t = ws.save_as_template(p).unwrap();
        ws.daily_template = Some(t);
        ws.open_day(0);
        let today = ws.current.unwrap();
        assert_eq!(ws.journal_day(today), Some(dates::today_day()));
        assert_eq!(ws.page(today).unwrap().blocks[0].text, "Gratitude");
        ws.open_journal_neighbour(today, -1);
        let yesterday = ws.current.unwrap();
        assert_ne!(yesterday, today);
        assert_eq!(ws.journal_day(yesterday), Some(dates::today_day() - 1));
        ws.open_journal_neighbour(yesterday, 1);
        assert_eq!(
            ws.current,
            Some(today),
            "reopening a day must not duplicate it"
        );
    }
}
