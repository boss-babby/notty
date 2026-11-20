//! cargo run --release --example bench -- <naive|build|open> [rows] [dir]
//! Measures the same 1 000 000-row table held fully in RAM (the way `Workspace.pages` holds rows today) against the disk-backed store.
use lumen_store::*;
use std::collections::BTreeMap;
use std::time::Instant;

fn rss_mb(key: &str) -> f64 {
    std::fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())).unwrap_or(0.0) / 1024.0
}

fn row(i: u64) -> Row {
    Row {
        id: i,
        title: format!("Task {i} — ship the release"),
        updated: 1_700_000_000_000 + i * 1000,
        props: vec![(1, Value::Sel(i % 5)), (2, Value::Num(((i * 7919) % 10_000) as f64 / 10.0)), (3, Value::Text(format!("team-{}", i % 40))), (4, Value::Check(i % 3 == 0)), (5, Value::Multi(vec![i % 11, (i + 3) % 11]))],
    }
}

// the in-memory shape of a row today: title String + BTreeMap<u64, Val> (Page has ~30 more fields, so this is a lower bound)
#[allow(dead_code)]
struct Naive {
    id: u64,
    title: String,
    updated: f64,
    props: BTreeMap<u64, Value>,
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = a.get(1).map(|s| s.as_str()).unwrap_or("open");
    let n: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let dir = a.get(3).cloned().unwrap_or_else(|| "/tmp/lstore-bench".into());
    let base = rss_mb("VmRSS");
    match mode {
        "naive" => {
            let t = Instant::now();
            let rows: Vec<Naive> = (1..=n).map(|i| { let r = row(i); Naive { id: r.id, title: r.title, updated: r.updated as f64, props: r.props.into_iter().map(|(k, v)| (k as u64, v)).collect() } }).collect();
            println!("naive  : {n} rows built in {:?}; RSS {:.0} MB (+{:.0} MB)", t.elapsed(), rss_mb("VmRSS"), rss_mb("VmRSS") - base);
            let t = Instant::now();
            let s: f64 = rows.iter().filter_map(|r| match r.props.get(&2) { Some(Value::Num(x)) => Some(*x), _ => None }).sum();
            println!("naive  : full-scan sum {s:.0} in {:?}", t.elapsed());
        }
        "build" => {
            let _ = std::fs::remove_dir_all(&dir);
            let mut st = Store::open_dir(&dir, 64 << 20).unwrap();
            let t = Instant::now();
            st.bulk_append((1..=n).map(row)).unwrap();
            println!("build  : {n} rows → {} chunks, {:.1} MB on disk in {:?}; peak RSS {:.0} MB", st.chunk_count(), st.disk_bytes() as f64 / 1048576.0, t.elapsed(), rss_mb("VmHWM"));
        }
        _ => {
            let t = Instant::now();
            let mut st = Store::open_dir(&dir, 64 << 20).unwrap();
            println!("open   : {:?} (index of {} chunks); RSS {:.0} MB", t.elapsed(), st.chunk_count(), rss_mb("VmRSS"));
            let t = Instant::now();
            let len = st.len().unwrap();
            let ag = st.aggregate_num(2, None).unwrap();
            println!("count/sum/min/max from zone maps: {len} rows, sum {:.0} in {:?} (chunk reads: {})", ag.sum, t.elapsed(), st.stats.chunk_reads);
            let t = Instant::now();
            let f = st.aggregate_num(2, Some((100.0, 120.0))).unwrap();
            println!("filtered 100..120: {} rows in {:?}; chunks skipped {}", f.count, t.elapsed(), st.stats.chunks_skipped);
            let t = Instant::now();
            let mut c = [0u64; 5];
            st.scan_sel_values(1, |o| c[o as usize] += 1).unwrap();
            println!("group-by select (full column scan): {:?} in {:?}; RSS {:.0} MB", c, t.elapsed(), rss_mb("VmRSS"));
            let t = Instant::now();
            let mut s = 0.0;
            st.scan_num(2, |_, x| s += x).unwrap();
            println!("numeric column scan sum {s:.0} in {:?}", t.elapsed());
            let t = Instant::now();
            let w = st.window(n / 2, 60).unwrap();
            println!("window(row {} .. +60) = {} rows in {:?} (table scroll position)", n / 2, w.len(), t.elapsed());
            let t = Instant::now();
            let mut x = 12345u64;
            for _ in 0..10_000 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let _ = st.get(1 + (x >> 33) % n).unwrap();
            }
            println!("10 000 random point reads in {:?}; cache {:.1} MB; cache hits {} / reads {}", t.elapsed(), st.cache_bytes() as f64 / 1048576.0, st.stats.cache_hits, st.stats.chunk_reads);
            let t = Instant::now();
            for i in 0..1000u64 {
                let mut r = row(1 + i * 997);
                r.title = format!("edited {i}");
                st.upsert(r).unwrap();
            }
            let te = t.elapsed();
            let t = Instant::now();
            st.checkpoint().unwrap();
            println!("1 000 durable edits (WAL) in {te:?}; checkpoint rewriting the touched chunks in {:?}; disk {:.1} MB", t.elapsed(), st.disk_bytes() as f64 / 1048576.0);
            println!("final RSS {:.0} MB, peak {:.0} MB", rss_mb("VmRSS"), rss_mb("VmHWM"));
        }
    }
}
