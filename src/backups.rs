//! Hourly rotating backups (NEXT-PASS §11.1).
//!
//! Every time the workspace state is written ([`on_state`]) and the wall-clock hour differs from the last backup, the same text is stored
//! as `backups/YYYYMMDD-HH` (UTC). Rotation keeps the newest 24 hourly files plus the newest file of each of the 7 days before that.
//! Native / Android: files next to the app storage (`$LUMEN_DATA_DIR/backups`, desktop: the eframe storage folder). Browser: `localStorage`
//! (quota is small, so only the newest 3 are kept there). The storage manager lists them and the page-history dialog can compare a page with
//! any backup.

use std::sync::Mutex;

use crate::dates;

pub const KEEP_HOURLY: usize = 24;
pub const KEEP_DAILY: usize = 7;
#[cfg(target_arch = "wasm32")]
const WEB_KEEP: usize = 3;

/// One stored backup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// `YYYYMMDD-HH`
    pub name: String,
    pub bytes: usize,
}

/// `YYYYMMDD-HH` (UTC) for a wall-clock time in milliseconds.
pub fn name_for(ts_ms: f64) -> String {
    let ms = if ts_ms.is_finite() {
        ts_ms.max(0.0)
    } else {
        0.0
    };
    let days = (ms / 86_400_000.0).floor() as i64;
    let (y, m, d) = dates::civil_from_days(days);
    let hour = (((ms - days as f64 * 86_400_000.0) / 3_600_000.0) as u32).min(23);
    format!("{y:04}{m:02}{d:02}-{hour:02}")
}

/// `(year, month, day, hour)` of a backup name.
pub fn parse_name(name: &str) -> Option<(i32, u32, u32, u32)> {
    let (date, hour) = name.split_once('-')?;
    if date.len() != 8
        || hour.len() != 2
        || !date.bytes().all(|b| b.is_ascii_digit())
        || !hour.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (y, m, d) = (
        date[..4].parse().ok()?,
        date[4..6].parse().ok()?,
        date[6..].parse().ok()?,
    );
    let h: u32 = hour.parse().ok()?;
    ((1..=12).contains(&m) && (1..=31).contains(&d) && h < 24).then_some((y, m, d, h))
}

/// "Mar 5, 14:00 UTC" (the hour the backup was taken, in UTC).
pub fn label(name: &str) -> String {
    match parse_name(name) {
        Some((_, m, d, h)) => format!(
            "{} {d}, {h:02}:00 UTC",
            &dates::MONTHS[(m as usize - 1).min(11)][..3]
        ),
        None => name.to_owned(),
    }
}

/// Names that rotation deletes: keep the newest `hourly` entries and, of the older ones, the newest entry of each of the `daily` most
/// recent days. Unparseable names are never touched.
pub fn retain_plan(names: &[String], hourly: usize, daily: usize) -> Vec<String> {
    let mut v: Vec<&String> = names.iter().filter(|n| parse_name(n).is_some()).collect();
    v.sort();
    v.reverse(); // newest first (the names sort chronologically)
    let mut doomed = Vec::new();
    let mut days_kept: Vec<&str> = Vec::new();
    for (i, n) in v.iter().enumerate() {
        if i < hourly {
            continue;
        }
        let day = &n[..8];
        if days_kept.contains(&day) || days_kept.len() >= daily {
            doomed.push((*n).clone());
        } else {
            days_kept.push(day);
        }
    }
    doomed
}

static LAST: Mutex<String> = Mutex::new(String::new());

/// Called with every state text that is written. Cheap unless the hour changed.
pub fn on_state(text: &str, now_ms: f64) {
    if text.len() < 8 {
        return;
    }
    let name = name_for(now_ms);
    {
        let Ok(mut last) = LAST.lock() else { return };
        if *last == name {
            return;
        }
        if last.is_empty() && list().iter().any(|e| e.name == name) {
            *last = name; // a backup for this hour exists from an earlier run
            return;
        }
        *last = name.clone();
    }
    if !write(&name, text) {
        log::warn!("backup {name} could not be written");
        return;
    }
    let names: Vec<String> = list().into_iter().map(|e| e.name).collect();
    for n in retain_plan(&names, KEEP_HOURLY, KEEP_DAILY) {
        remove(&n);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod io {
    use super::Entry;
    use std::path::PathBuf;

    pub fn dir() -> Option<PathBuf> {
        if let Some(d) = std::env::var_os("LUMEN_DATA_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            return Some(d.join("backups"));
        }
        eframe::storage_dir("Lumen").map(|d| d.join("backups"))
    }

    pub fn list() -> Vec<Entry> {
        let Some(d) = dir() else { return vec![] };
        let Ok(rd) = std::fs::read_dir(d) else {
            return vec![];
        };
        let mut v: Vec<Entry> = rd
            .flatten()
            .filter_map(|e| {
                let name = e
                    .file_name()
                    .to_string_lossy()
                    .strip_suffix(".ron")?
                    .to_owned();
                super::parse_name(&name)?;
                Some(Entry {
                    name,
                    bytes: e.metadata().map_or(0, |m| m.len() as usize),
                })
            })
            .collect();
        v.sort_by(|a, b| b.name.cmp(&a.name));
        v
    }

    pub fn read(name: &str) -> Option<String> {
        super::parse_name(name)?;
        std::fs::read_to_string(dir()?.join(format!("{name}.ron"))).ok()
    }

    pub fn write(name: &str, text: &str) -> bool {
        let Some(d) = dir() else { return false };
        if std::fs::create_dir_all(&d).is_err() {
            return false;
        }
        let tmp = d.join(format!("{name}.tmp"));
        std::fs::write(&tmp, text).is_ok()
            && std::fs::rename(&tmp, d.join(format!("{name}.ron"))).is_ok()
    }

    pub fn remove(name: &str) {
        if super::parse_name(name).is_some() {
            if let Some(d) = dir() {
                let _ = std::fs::remove_file(d.join(format!("{name}.ron")));
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod io {
    use super::Entry;
    const PREFIX: &str = "lumen-backup-";

    fn store() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn list() -> Vec<Entry> {
        let Some(s) = store() else { return vec![] };
        let n = s.length().unwrap_or(0);
        let mut v = Vec::new();
        for i in 0..n {
            if let Ok(Some(k)) = s.key(i) {
                if let Some(name) = k.strip_prefix(PREFIX) {
                    if super::parse_name(name).is_some() {
                        let bytes = s.get_item(&k).ok().flatten().map_or(0, |t| t.len());
                        v.push(Entry {
                            name: name.to_owned(),
                            bytes,
                        });
                    }
                }
            }
        }
        v.sort_by(|a, b| b.name.cmp(&a.name));
        v
    }

    pub fn read(name: &str) -> Option<String> {
        store()?.get_item(&format!("{PREFIX}{name}")).ok()?
    }

    pub fn write(name: &str, text: &str) -> bool {
        let Some(s) = store() else { return false };
        // the quota is small: make room first (oldest go), then try
        let mut have = list();
        while have.len() >= super::WEB_KEEP {
            if let Some(old) = have.pop() {
                remove(&old.name);
            }
        }
        s.set_item(&format!("{PREFIX}{name}"), text).is_ok()
    }

    pub fn remove(name: &str) {
        if let Some(s) = store() {
            let _ = s.remove_item(&format!("{PREFIX}{name}"));
        }
    }
}

/// Newest first.
pub fn list() -> Vec<Entry> {
    io::list()
}
pub fn read(name: &str) -> Option<String> {
    io::read(name)
}
fn write(name: &str, text: &str) -> bool {
    io::write(name, text)
}
fn remove(name: &str) {
    io::remove(name)
}

/// `(count, bytes)` of all stored backups.
pub fn totals() -> (usize, usize) {
    let l = list();
    (l.len(), l.iter().map(|e| e.bytes).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        // 2024-03-05 14:30 UTC
        let ts = (dates::days_from_civil(2024, 3, 5) as f64) * 86_400_000.0 + 14.5 * 3_600_000.0;
        let n = name_for(ts);
        assert_eq!(n, "20240305-14");
        assert_eq!(parse_name(&n), Some((2024, 3, 5, 14)));
        assert_eq!(label(&n), "Mar 5, 14:00 UTC");
        assert!(parse_name("20241301-00").is_none());
        assert!(parse_name("junk").is_none());
        assert!(parse_name("20240305-24").is_none());
    }

    #[test]
    fn rotation_keeps_24_hourly_and_one_per_older_day() {
        let mut names = Vec::new();
        // 10 days × 24 hours, 2024-03-01 .. 2024-03-10
        for d in 1..=10u32 {
            for h in 0..24 {
                names.push(format!("202403{d:02}-{h:02}"));
            }
        }
        names.push("keep-me.txt".into());
        let doomed = retain_plan(&names, 24, 7);
        let kept: Vec<&String> = names.iter().filter(|n| !doomed.contains(n)).collect();
        // newest 24 = all of 03-10; then the newest of 03-09 .. 03-03 (7 days); 03-02 and 03-01 go; the junk name stays
        assert_eq!(kept.len(), 24 + 7 + 1);
        assert!(kept.contains(&&"20240309-23".to_owned()));
        assert!(kept.contains(&&"20240303-23".to_owned()));
        assert!(!kept.contains(&&"20240302-23".to_owned()));
        assert!(!doomed.contains(&"keep-me.txt".to_owned()));
    }

    #[test]
    fn nothing_is_deleted_below_the_limits() {
        let names: Vec<String> = (0..10).map(|h| format!("20240305-{h:02}")).collect();
        assert!(retain_plan(&names, 24, 7).is_empty());
    }
}
