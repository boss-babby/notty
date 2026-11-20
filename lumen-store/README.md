# lumen-store
Disk-backed, chunked, columnar row store (std only). Design, measurements and the migration plan: `../DATA-ARCHITECTURE.md`.

```rust
let mut t = lumen_store::Store::open_dir("data/tasks", 64 << 20)?;   // 64 MB cache budget
t.bulk_append(rows)?;                    // import: ascending ids
t.upsert(row)?; t.delete(id)?;           // WAL (durable) + overlay
let page = t.window(first_visible, 60)?; // table scrolling
let sum  = t.aggregate_num(prop, None)?; // zone maps: no I/O
t.scan_sel_values(status, |opt| …)?;     // group-by for charts, one column at a time
```
`cargo test --release` · `cargo run --release --example bench -- build|open|naive 1000000`
