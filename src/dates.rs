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

pub fn parse(s: &str) -> Option<(i32, u32, u32)> {
    let mut it = s.trim().split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

/// "Jan 5, 2026" style label; falls back to the raw string.
pub fn pretty(s: &str) -> String {
    match parse(s) {
        Some((y, m, d)) => format!("{} {d}, {y}", &MONTHS[m as usize - 1][..3]),
        None => s.to_owned(),
    }
}

pub fn long_today() -> String {
    let (y, m, d) = today();
    format!("{} {d}, {y}", MONTHS[m as usize - 1])
}
