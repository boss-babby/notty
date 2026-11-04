//! Tiny calendar helpers (proleptic Gregorian) so we need no date crate on native or wasm.

use crate::auth::now_ms;

pub const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

/// Today's local date (browser local time on the web, UTC on native).
pub fn today() -> (i32, u32, u32) {
    #[allow(unused_mut)]
    let mut ms = now_ms();
    #[cfg(target_arch = "wasm32")]
    {
        ms -= js_sys::Date::new_0().get_timezone_offset() * 60_000.0;
    }
    civil_from_days((ms / 86_400_000.0).floor() as i64)
}

/// 0 = Monday … 6 = Sunday.
pub fn weekday(y: i32, m: u32, d: u32) -> u32 {
    (days_from_civil(y, m, d) + 3).rem_euclid(7) as u32
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

pub fn iso(y: i32, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn today_iso() -> String {
    let (y, m, d) = today();
    iso(y, m, d)
}

/// Date values are `YYYY-MM-DD` or a range `YYYY-MM-DD/YYYY-MM-DD` (start/end, inclusive).
pub fn parse_one(s: &str) -> Option<(i32, u32, u32)> {
    let mut it = s.trim().split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

/// Start date of a (possibly ranged) date value.
pub fn parse(s: &str) -> Option<(i32, u32, u32)> {
    parse_one(s.split('/').next().unwrap_or(""))
}

/// End date of a ranged value (`None` for single dates).
pub fn parse_end(s: &str) -> Option<(i32, u32, u32)> {
    s.split_once('/').and_then(|(_, e)| parse_one(e))
}

pub fn day_of(t: (i32, u32, u32)) -> i64 {
    days_from_civil(t.0, t.1, t.2)
}

pub fn iso_of_day(n: i64) -> String {
    let (y, m, d) = civil_from_days(n);
    iso(y, m, d)
}

/// Inclusive `(start_day, end_day)` numbers of a date value; single dates have `end == start`.
pub fn span(s: &str) -> Option<(i64, i64)> {
    let a = day_of(parse(s)?);
    let b = parse_end(s).map(day_of).unwrap_or(a).max(a);
    Some((a, b))
}

/// Builds a value from day numbers (collapses to a single date when `start == end`).
pub fn span_value(a: i64, b: i64) -> String {
    let (a, b) = if b < a { (b, a) } else { (a, b) };
    if a == b {
        iso_of_day(a)
    } else {
        format!("{}/{}", iso_of_day(a), iso_of_day(b))
    }
}

fn pretty_one(t: (i32, u32, u32)) -> String {
    format!("{} {}, {}", &MONTHS[t.1 as usize - 1][..3], t.2, t.0)
}

/// "Jan 5, 2026" or "Jan 5 → Jan 9, 2026" style label; falls back to the raw string.
pub fn pretty(s: &str) -> String {
    match (parse(s), parse_end(s)) {
        (Some(a), Some(b)) if a.0 == b.0 => format!("{} {} → {} {}, {}", &MONTHS[a.1 as usize - 1][..3], a.2, &MONTHS[b.1 as usize - 1][..3], b.2, a.0),
        (Some(a), Some(b)) => format!("{} → {}", pretty_one(a), pretty_one(b)),
        (Some(a), None) => pretty_one(a),
        _ => s.to_owned(),
    }
}

pub fn today_day() -> i64 {
    day_of(today())
}

pub fn long_today() -> String {
    let (y, m, d) = today();
    format!("{} {d}, {y}", MONTHS[m as usize - 1])
}
