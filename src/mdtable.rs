//! Markdown (GFM) tables ⇄ table blocks ⇄ databases.
//!
//! * [`extract`] / [`restore`] let every Markdown parser of the app (paste, import, AI tools, MCP) understand tables without each of
//!   them having to: the table lines are swapped for a placeholder line before the normal line parser runs, and the placeholder block
//!   becomes a real `Table` block afterwards.
//! * A `Table` block stores its cells in `Block::text` (rows separated by [`ROW`], cells by [`CELL`]), the column alignment as `l`/`c`/`r`
//!   letters in `Block::url` and "first row is a header" in `Block::checked`.
//! * [`to_database`] turns a table into a real database: the header row becomes the properties (types are inferred from the data),
//!   every other row becomes a database row, and the block is replaced by a linked view of the new database.

use crate::model::*;

pub const CELL: char = '\u{1f}';
pub const ROW: char = '\u{1e}';
const MARK: char = '\u{1}';

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    /// One letter per column: `l`, `c` or `r`.
    pub align: String,
}

pub fn encode(rows: &[Vec<String>]) -> String {
    rows.iter().map(|r| r.iter().map(|c| c.replace([CELL, ROW], " ")).collect::<Vec<_>>().join(&CELL.to_string())).collect::<Vec<_>>().join(&ROW.to_string())
}

pub fn decode(s: &str) -> Vec<Vec<String>> {
    if s.is_empty() {
        return vec![];
    }
    s.split(ROW).map(|r| r.split(CELL).map(str::to_owned).collect()).collect()
}

/// Rows of a table block, padded to a rectangle (at least 1 × 1).
pub fn grid(b: &Block) -> Vec<Vec<String>> {
    let mut rows = decode(&b.text);
    let n = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
    if rows.is_empty() {
        rows.push(vec![]);
    }
    for r in &mut rows {
        r.resize(n, String::new());
    }
    rows
}

pub fn set_grid(b: &mut Block, rows: &[Vec<String>]) {
    b.text = encode(rows);
    let n = rows.first().map_or(0, |r| r.len());
    let mut a: Vec<char> = b.url.chars().collect();
    a.resize(n, 'l');
    b.url = a.into_iter().collect();
}

pub fn new_block(ws: &mut Workspace, t: &Table) -> Block {
    let mut b = ws.blk(BlockKind::Table, "", 0);
    fill(&mut b, t);
    b
}

fn fill(b: &mut Block, t: &Table) {
    b.kind = BlockKind::Table;
    b.text = encode(&t.rows);
    b.url = t.align.clone();
    b.checked = true;
}

/// A fresh 3 × 3 table (what `/table` inserts).
pub fn blank() -> Table {
    Table { rows: vec![vec!["Name".into(), "Status".into(), "Notes".into()], vec![String::new(); 3], vec![String::new(); 3]], align: "lll".into() }
}

// ───────────────────────────── parsing ─────────────────────────────

/// Splits one table line into trimmed cells. Honours `\|` and pipes inside `code spans`.
pub fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let mut cells: Vec<String> = vec![];
    let mut cur = String::new();
    let mut in_code = false;
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' if it.peek() == Some(&'|') => {
                cur.push('|');
                it.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cur).trim().to_owned()),
            _ => cur.push(c),
        }
    }
    // a trailing pipe closes the last cell; text after the last pipe is one more cell
    if !(cur.trim().is_empty() && t.trim_end().ends_with('|')) {
        cells.push(cur.trim().to_owned());
    }
    for c in &mut cells {
        *c = c.replace("<br>", " ").replace("<br/>", " ").replace("<br />", " ");
    }
    cells
}

/// `|:--|:-:|--:|` → `Some("lcr")`.
pub fn separator(line: &str) -> Option<String> {
    let t = line.trim();
    if !t.contains('-') || !t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t')) {
        return None;
    }
    let cells = split_row(t);
    if cells.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in cells {
        let c = c.trim();
        let (l, r) = (c.starts_with(':'), c.ends_with(':'));
        let core = c.trim_matches(':');
        if core.is_empty() || !core.chars().all(|ch| ch == '-') {
            return None;
        }
        out.push(match (l, r) {
            (true, true) => 'c',
            (false, true) => 'r',
            _ => 'l',
        });
    }
    Some(out)
}

/// Replaces every GFM table in `md` (outside code fences) by a one-line placeholder; the tables are returned in order.
pub fn extract(md: &str) -> (String, Vec<Table>) {
    let lines: Vec<&str> = md.lines().collect();
    let mut out = String::with_capacity(md.len());
    let mut tables = vec![];
    let mut fence = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            fence = !fence;
        }
        if !fence && line.contains('|') && i + 1 < lines.len() {
            if let Some(al) = separator(lines[i + 1]) {
                let head = split_row(line);
                if head.len() == al.chars().count() && !head.is_empty() {
                    let n = head.len();
                    let mut rows = vec![head];
                    let mut j = i + 2;
                    while j < lines.len() && !lines[j].trim().is_empty() && lines[j].contains('|') {
                        let mut r = split_row(lines[j]);
                        r.resize(n, String::new());
                        rows.push(r);
                        j += 1;
                    }
                    out.push_str(&format!("{MARK}TBL{}{MARK}\n", tables.len()));
                    tables.push(Table { rows, align: al });
                    i = j;
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
        i += 1;
    }
    (out, tables)
}

fn placeholder_index(text: &str) -> Option<usize> {
    text.strip_prefix(MARK)?.strip_prefix("TBL")?.strip_suffix(MARK)?.parse().ok()
}

/// Turns the placeholder blocks produced by [`extract`] into `Table` blocks.
pub fn restore(blocks: &mut [Block], tables: &[Table]) {
    for b in blocks.iter_mut() {
        if let Some(t) = placeholder_index(b.text.trim()).and_then(|i| tables.get(i)) {
            b.indent = 0;
            fill(b, t);
        }
    }
}

/// Spreadsheet / CSV-like clipboard text (tabs between cells, ≥ 2 rows, ≥ 2 columns) → table.
pub fn from_tsv(text: &str) -> Option<Table> {
    let norm = text.replace("\r\n", "\n");
    let lines: Vec<&str> = norm.trim_matches('\n').split('\n').collect();
    if lines.len() < 2 || !lines.iter().all(|l| l.contains('\t')) {
        return None;
    }
    let mut rows: Vec<Vec<String>> = lines.iter().map(|l| l.split('\t').map(|c| c.trim().trim_matches('"').to_owned()).collect()).collect();
    let n = rows.iter().map(|r| r.len()).max()?;
    if n < 2 {
        return None;
    }
    for r in &mut rows {
        r.resize(n, String::new());
    }
    Some(Table { rows, align: "l".repeat(n) })
}

// ───────────────────────────── export ─────────────────────────────

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Table block → GFM table (without trailing newline).
pub fn to_markdown(b: &Block) -> String {
    let rows = grid(b);
    let n = rows[0].len();
    let al: Vec<char> = b.url.chars().chain(std::iter::repeat('l')).take(n).collect();
    let line = |r: &Vec<String>| format!("| {} |", r.iter().map(|c| md_cell(c)).collect::<Vec<_>>().join(" | "));
    let mut out = vec![];
    let (head, body): (Vec<String>, &[Vec<String>]) = if b.checked { (rows[0].clone(), &rows[1..]) } else { (vec![String::new(); n], &rows[..]) };
    out.push(line(&head));
    out.push(format!(
        "|{}|",
        al.iter()
            .map(|a| match a {
                'c' => ":---:",
                'r' => "---:",
                _ => "---",
            })
            .collect::<Vec<_>>()
            .join("|")
    ));
    for r in body {
        out.push(line(r));
    }
    out.join("\n")
}

// ───────────────────────────── table → database ─────────────────────────────

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && s.char_indices().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn as_num(s: &str) -> Option<f64> {
    let c: String = s.trim().trim_start_matches(['$', '€', '£']).trim_end_matches('%').replace(',', "");
    if c.is_empty() || !c.chars().any(|ch| ch.is_ascii_digit()) {
        return None;
    }
    c.parse::<f64>().ok()
}

fn is_bool(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "true" | "false" | "yes" | "no" | "[x]" | "[ ]" | "✓" | "✔" | "x" | "done")
}

/// Best property type for a column of cell texts.
pub fn infer(cells: &[&str]) -> PropKind {
    let filled: Vec<&str> = cells.iter().copied().filter(|c| !c.trim().is_empty()).collect();
    if filled.is_empty() {
        return PropKind::Text;
    }
    if filled.iter().all(|c| is_bool(c)) {
        return PropKind::Checkbox;
    }
    if filled.iter().all(|c| is_date(c.trim())) {
        return PropKind::Date;
    }
    if filled.iter().all(|c| as_num(c).is_some()) {
        return PropKind::Number;
    }
    if filled.iter().all(|c| c.starts_with("http://") || c.starts_with("https://")) {
        return PropKind::Url;
    }
    let mut uniq: Vec<&str> = filled.clone();
    uniq.sort_unstable();
    uniq.dedup();
    if filled.len() >= 4 && uniq.len() <= 8 && uniq.len() * 2 <= filled.len() && filled.iter().all(|c| c.chars().count() <= 28) {
        return PropKind::Select;
    }
    PropKind::Text
}

/// Converts table block `block` of page `page` into a database (a child of that page) and swaps the block for a linked view of it.
/// Returns the id of the new database.
pub fn to_database(ws: &mut Workspace, page: u64, block: u64) -> Option<u64> {
    let (rows, header, name) = {
        let p = ws.page(page)?;
        let bi = p.blocks.iter().position(|b| b.id == block && b.kind == BlockKind::Table)?;
        let b = &p.blocks[bi];
        // the closest heading above the table names the database
        let name = p.blocks[..bi].iter().rev().find(|x| matches!(x.kind, BlockKind::H1 | BlockKind::H2 | BlockKind::H3) && !x.text.trim().is_empty()).map(|x| x.text.trim().to_owned());
        (grid(b), b.checked, name)
    };
    let ncols = rows[0].len();
    let (head, body): (Vec<String>, &[Vec<String>]) = if header {
        (rows[0].iter().enumerate().map(|(i, h)| if h.trim().is_empty() { format!("Column {}", i + 1) } else { h.trim().to_owned() }).collect(), &rows[1..])
    } else {
        ((0..ncols).map(|i| if i == 0 { "Name".to_owned() } else { format!("Column {}", i + 1) }).collect(), &rows[..])
    };
    let db = ws.add_database(Some(page), ViewKind::Table);
    ws.pages.retain(|p| !(p.row && p.parent == Some(db)));
    if let Some(d) = ws.db_mut(db) {
        d.props.clear();
    }
    if let Some(p) = ws.page_mut(db) {
        p.title = name.unwrap_or_else(|| "Table".into());
    }
    // column 0 is the title; the others become typed properties
    let mut props: Vec<(usize, u64)> = vec![];
    let mut used: Vec<String> = vec!["name".into()];
    for c in 1..ncols {
        let cells: Vec<&str> = body.iter().map(|r| r[c].as_str()).collect();
        let kind = infer(&cells);
        let mut name = head[c].clone();
        while used.contains(&name.to_lowercase()) {
            name.push('_');
        }
        used.push(name.to_lowercase());
        let pid = ws.add_prop(db, &name, kind, &[]);
        props.push((c, pid));
    }
    for r in body {
        let id = ws.add_row(db, None);
        if let Some(p) = ws.page_mut(id) {
            p.title = r[0].clone();
        }
        for (c, pid) in &props {
            let cell = r[*c].trim();
            if cell.is_empty() {
                continue;
            }
            if let Ok(Some(v)) = crate::agent_tools::to_val(ws, db, *pid, &serde_json::Value::String(cell.to_owned())) {
                if let Some(p) = ws.page_mut(id) {
                    p.props.insert(*pid, v);
                }
            }
        }
    }
    if let Some(d) = ws.db_mut(db) {
        d.active = 0;
    }
    ws.ensure_linked_view(db, block, ViewKind::Table);
    if let Some(b) = ws.page_mut(page).and_then(|p| p.blocks.iter_mut().find(|b| b.id == block)) {
        b.kind = BlockKind::DbView;
        b.link = Some(db);
        b.text.clear();
        b.url.clear();
        b.checked = false;
    }
    let _ = crate::formula::recompute(ws);
    ws.touch(page);
    Some(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "# Plan\n\nIntro text\n\n| Task | Owner | Done | Due | Cost |\n|:-----|:-----:|-----:|-----|------|\n| Write `a|b` | Ann | yes | 2025-01-03 | $12 |\n| Ship \\| it | Bob | no | 2025-02-01 | 1,5 |\n| Rest | Ann | no | | 3 |\n| More | Bob | yes | 2025-02-02 | 4 |\n\nAfter\n\n```\n| not | a table |\n|---|---|\n```\n";

    #[test]
    fn extracts_gfm_tables_and_skips_code_fences() {
        let (rest, tables) = extract(MD);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows.len(), 5);
        assert_eq!(tables[0].rows[0], ["Task", "Owner", "Done", "Due", "Cost"]);
        assert_eq!(tables[0].rows[1][0], "Write `a|b`");
        assert_eq!(tables[0].rows[2][0], "Ship | it");
        assert_eq!(tables[0].align, "lcrll");
        assert!(rest.contains("TBL0") && rest.contains("| not | a table |"));
    }

    #[test]
    fn restore_and_export_round_trip() {
        let (rest, tables) = extract(MD);
        let mut blocks: Vec<Block> = rest.lines().filter(|l| !l.trim().is_empty()).map(|l| Block::new(0, BlockKind::Text, l, 0)).collect();
        restore(&mut blocks, &tables);
        let t = blocks.iter().find(|b| b.kind == BlockKind::Table).expect("table block");
        let md = to_markdown(t);
        let (_, again) = extract(&md);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].rows, tables[0].rows);
        assert_eq!(again[0].align, "lcrll");
    }

    #[test]
    fn table_becomes_database_with_inferred_types() {
        let (_, tables) = extract(MD);
        let mut ws = Workspace::seed();
        let page = ws.add_page(None, PageKind::Doc);
        let b = new_block(&mut ws, &tables[0]);
        let bid = b.id;
        ws.page_mut(page).unwrap().blocks.push(b);
        let db = to_database(&mut ws, page, bid).expect("database");
        let d = ws.db(db).unwrap();
        let kinds: Vec<(String, PropKind)> = d.props.iter().map(|p| (p.name.clone(), p.kind)).collect();
        assert_eq!(kinds, vec![("Owner".into(), PropKind::Select), ("Done".into(), PropKind::Checkbox), ("Due".into(), PropKind::Date), ("Cost".into(), PropKind::Number)]);
        assert_eq!(ws.rows(db).len(), 4);
        let blk = ws.page(page).unwrap().blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(blk.kind, BlockKind::DbView);
        assert_eq!(blk.link, Some(db));
    }

    #[test]
    fn spreadsheet_paste_is_a_table() {
        let t = from_tsv("a\tb\n1\t2\n3\t4").unwrap();
        assert_eq!(t.rows.len(), 3);
        assert!(from_tsv("no tabs here\nat all").is_none());
    }

    #[test]
    fn relation_backlinks_show_assigned_work() {
        let mut ws = Workspace::seed();
        let people = ws.add_database(None, ViewKind::Table);
        let tasks = ws.add_database(None, ViewKind::Table);
        let ann = ws.add_row(people, None);
        let bob = ws.add_row(people, None);
        let assignee = ws.add_prop(tasks, "Assignee", PropKind::Relation, &[]);
        ws.db_mut(tasks).unwrap().props.iter_mut().find(|p| p.id == assignee).unwrap().cfg.relation = people;
        let (t1, t2) = (ws.add_row(tasks, None), ws.add_row(tasks, None));
        ws.page_mut(t1).unwrap().props.insert(assignee, Val::Multi(vec![ann]));
        ws.page_mut(t2).unwrap().props.insert(assignee, Val::Multi(vec![ann, bob]));
        assert_eq!(ws.relation_backlinks(ann), vec![(tasks, assignee, vec![t1, t2])]);
        assert_eq!(ws.relation_backlinks(bob), vec![(tasks, assignee, vec![t2])]);
        // a trashed task no longer shows up
        ws.page_mut(t1).unwrap().trashed = true;
        assert_eq!(ws.relation_backlinks(ann), vec![(tasks, assignee, vec![t2])]);
    }
}
