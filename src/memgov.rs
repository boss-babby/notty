//! Memory governor: one place that owns the byte budgets of every cache and reacts to memory pressure.
//!
//! * the row cache of large datasets (`bigtable`, see `DATA-ARCHITECTURE.md`) gets a fixed budget per platform;
//! * Android `onTrimMemory` (JNI, `LumenActivity.nativeTrim`) and "app was in the background for 20 s" shrink the caches:
//!   level 1 = row caches to 1/8 for a minute and cached dashboard results dropped, level 2 = everything incl. decoded images;
//! * `rss_mb()` reports the process' resident memory (Linux / Android) for the status line of large tables.

use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

use egui::Context;

#[cfg(target_os = "android")]
const DEFAULT_ROWS: usize = 24 << 20;
#[cfg(target_arch = "wasm32")]
const DEFAULT_ROWS: usize = 48 << 20;
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
const DEFAULT_ROWS: usize = 96 << 20;

static ROWS: AtomicUsize = AtomicUsize::new(DEFAULT_ROWS);
static TRIM: AtomicU8 = AtomicU8::new(0);
static REDUCED_UNTIL: AtomicU64 = AtomicU64::new(0);
static UNFOCUSED_SINCE: AtomicU64 = AtomicU64::new(0);

/// Configured budget of one table's chunk cache.
pub fn row_cache_bytes() -> usize {
    ROWS.load(Ordering::Relaxed)
}

pub fn set_row_cache_bytes(b: usize) {
    ROWS.store(b, Ordering::Relaxed);
}

/// Asks for a trim on the next frame (callable from any thread, e.g. the JNI callback). 1 = soft, 2 = hard.
pub fn request_trim(level: u8) {
    TRIM.fetch_max(level, Ordering::Relaxed);
}

/// Once per frame.
pub fn tick(ctx: &Context) {
    let now = ctx.input(|i| i.time);
    let focused = ctx.input(|i| i.focused);
    let mut level = TRIM.swap(0, Ordering::Relaxed);
    if focused {
        UNFOCUSED_SINCE.store(0, Ordering::Relaxed);
    } else {
        let since = f64::from_bits(UNFOCUSED_SINCE.load(Ordering::Relaxed));
        if since == 0.0 {
            UNFOCUSED_SINCE.store(now.to_bits(), Ordering::Relaxed);
        } else if now - since > 20.0 {
            level = level.max(1);
            UNFOCUSED_SINCE.store((now + 1.0e6).to_bits(), Ordering::Relaxed); // once per background period
        }
    }
    if level > 0 {
        REDUCED_UNTIL.store((now + 60.0).to_bits(), Ordering::Relaxed);
        crate::bigdata::clear_cache();
        if level >= 2 {
            ctx.forget_all_images();
        }
        crate::bigtable::set_budget_all(0);
        ctx.request_repaint();
    }
    let reduced = now < f64::from_bits(REDUCED_UNTIL.load(Ordering::Relaxed));
    crate::bigtable::set_budget_all(if reduced { row_cache_bytes() / 8 } else { row_cache_bytes() });
}

/// Resident memory of this process in MB (Linux / Android only).
pub fn rss_mb() -> Option<f64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let s = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: f64 = s.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096.0 / 1048576.0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        None
    }
}
