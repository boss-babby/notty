//! Notion-style formulas, relations and rollups.
//!
//! * `prop("Price") * prop("Qty")`, `if(prop("Done"), "✓", "…")`, `dateBetween(prop("Due"), today(), "days")`,
//!   `prop("Name").length()` — a small hand written lexer / parser / evaluator (no dependencies, works on wasm).
//! * Relation properties store the ids of related rows; rollups aggregate a property of those rows.
//! * [`recompute`] evaluates every formula / rollup and caches the result in the row's property map, so filters,
//!   sorts, CSV export and every database view treat computed values like ordinary ones.

use crate::dates;
use crate::model::*;

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Num(f64),
    Str(String),
    Bool(bool),
    /// Inclusive day numbers (start, end).
    Date(i64, i64),
    List(Vec<V>),
    Empty,
}

pub fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "∞".into() } else { "-∞".into() };
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let s = format!("{n:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

impl V {
    pub fn num(&self) -> f64 {
        match self {
            V::Num(n) => *n,
            V::Bool(b) => *b as i32 as f64,
            V::Str(s) => s.trim().parse().unwrap_or(f64::NAN),
            V::Date(a, _) => *a as f64,
            V::List(l) => l.first().map(|v| v.num()).unwrap_or(0.0),
            V::Empty => 0.0,
        }
    }

    pub fn text(&self) -> String {
        match self {
            V::Num(n) => fmt_num(*n),
            V::Str(s) => s.clone(),
            V::Bool(b) => b.to_string(),
            V::Date(a, b) => dates::pretty(&dates::span_value(*a, *b)),
            V::List(l) => l.iter().map(|v| v.text()).collect::<Vec<_>>().join(", "),
            V::Empty => String::new(),
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            V::Bool(b) => *b,
            V::Num(n) => *n != 0.0 && !n.is_nan(),
            V::Str(s) => !s.is_empty(),
            V::List(l) => !l.is_empty(),
            V::Date(..) => true,
            V::Empty => false,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            V::Empty => true,
            V::Str(s) => s.is_empty(),
            V::List(l) => l.is_empty(),
            _ => false,
        }
    }

    /// Value to cache in a row (`None` = empty).
    pub fn to_val(&self) -> Option<Val> {
        match self {
            V::Empty => None,
            V::Num(n) if n.is_nan() => None,
            V::Num(n) => Some(Val::Num(*n)),
            V::Bool(b) => Some(Val::Check(*b)),
            V::Str(s) if s.is_empty() => None,
            V::Str(s) => Some(Val::Text(s.clone())),
            V::Date(a, b) => Some(Val::Text(dates::span_value(*a, *b))),
            V::List(l) if l.is_empty() => None,
            V::List(_) => Some(Val::Text(self.text())),
        }
    }
}

fn veq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Num(x), V::Num(y)) => (x - y).abs() < 1e-12,
        (V::Str(x), V::Str(y)) => x == y,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Date(a1, b1), V::Date(a2, b2)) => a1 == a2 && b1 == b2,
        (V::List(x), V::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| veq(p, q)),
        (V::Empty, o) | (o, V::Empty) => o.is_empty(),
        _ => false,
    }
}

// ───────────────────────────── lexer ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Id(String),
    Op(&'static str),
    LP,
    RP,
    LB,
    RB,
    Comma,
    Q,
    Colon,
    Dot,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && cs.get(i + 1).map_or(false, |d| d.is_ascii_digit())) {
            let st = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || (cs[i] == '.' && cs.get(i + 1).map_or(false, |d| d.is_ascii_digit()))) {
                i += 1;
            }
            let t: String = cs[st..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| format!("Bad number “{t}”"))?));
        } else if c == '"' || c == '\'' || c == '“' {
            let close = if c == '“' { '”' } else { c };
            i += 1;
            let mut s = String::new();
            loop {
                let Some(&ch) = cs.get(i) else { return Err("Unterminated text".into()) };
                i += 1;
                if ch == close {
                    break;
                }
                if ch == '\\' {
                    let Some(&n) = cs.get(i) else { return Err("Unterminated text".into()) };
                    i += 1;
                    s.push(match n {
                        'n' => '\n',
                        't' => '\t',
                        o => o,
                    });
                } else {
                    s.push(ch);
                }
            }
            out.push(Tok::Str(s));
        } else if c.is_alphabetic() || c == '_' {
            let st = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(cs[st..i].iter().collect()));
        } else {
            let two: String = cs[i..(i + 2).min(cs.len())].iter().collect();
            let t2 = match two.as_str() {
                "==" => Some("=="),
                "!=" => Some("!="),
                "<=" => Some("<="),
                ">=" => Some(">="),
                "&&" => Some("&&"),
                "||" => Some("||"),
                _ => None,
            };
            if let Some(op) = t2 {
                out.push(Tok::Op(op));
                i += 2;
                continue;
            }
            i += 1;
            out.push(match c {
                '(' => Tok::LP,
                ')' => Tok::RP,
                '[' => Tok::LB,
                ']' => Tok::RB,
                ',' => Tok::Comma,
                '?' => Tok::Q,
                ':' => Tok::Colon,
                '.' => Tok::Dot,
                '+' => Tok::Op("+"),
                '-' => Tok::Op("-"),
                '*' => Tok::Op("*"),
                '/' => Tok::Op("/"),
                '%' => Tok::Op("%"),
                '^' => Tok::Op("^"),
                '<' => Tok::Op("<"),
                '>' => Tok::Op(">"),
                '!' => Tok::Op("!"),
                '=' => Tok::Op("=="),
                '&' => Tok::Op("&&"),
                '|' => Tok::Op("||"),
                o => return Err(format!("Unexpected “{o}”")),
            });
        }
    }
    Ok(out)
}

// ───────────────────────────── parser ─────────────────────────────

#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Call(String, Vec<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    List(Vec<Expr>),
}

struct Parser {
    t: Vec<Tok>,
    i: usize,
}

type PR = Result<Expr, String>;

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn ternary(&mut self) -> PR {
        let c = self.or()?;
        if self.eat(&Tok::Q) {
            let a = self.ternary()?;
            if !self.eat(&Tok::Colon) {
                return Err("Expected “:” in a ? b : c".into());
            }
            let b = self.ternary()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }
    fn word_op(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Id(x)) if x.eq_ignore_ascii_case(w)) && self.t.get(self.i + 1) != Some(&Tok::LP)
    }
    fn or(&mut self) -> PR {
        let mut l = self.and()?;
        loop {
            if self.eat(&Tok::Op("||")) || self.eat_word("or") {
                let r = self.and()?;
                l = Expr::Bin("||", Box::new(l), Box::new(r));
            } else {
                break;
            }
        }
        Ok(l)
    }
    fn eat_word(&mut self, w: &str) -> bool {
        if self.word_op(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn and(&mut self) -> PR {
        let mut l = self.eq()?;
        loop {
            if self.eat(&Tok::Op("&&")) || self.eat_word("and") {
                let r = self.eq()?;
                l = Expr::Bin("&&", Box::new(l), Box::new(r));
            } else {
                break;
            }
        }
        Ok(l)
    }
    fn bin_level(&mut self, ops: &[&'static str], next: fn(&mut Parser) -> PR) -> PR {
        let mut l = next(self)?;
        loop {
            let Some(Tok::Op(o)) = self.peek().cloned() else { break };
            let Some(op) = ops.iter().find(|x| **x == o) else { break };
            self.i += 1;
            let r = next(self)?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn eq(&mut self) -> PR {
        self.bin_level(&["==", "!="], Parser::cmp)
    }
    fn cmp(&mut self) -> PR {
        self.bin_level(&["<=", ">=", "<", ">"], Parser::add)
    }
    fn add(&mut self) -> PR {
        self.bin_level(&["+", "-"], Parser::mul)
    }
    fn mul(&mut self) -> PR {
        self.bin_level(&["*", "/", "%"], Parser::unary)
    }
    fn unary(&mut self) -> PR {
        if self.eat(&Tok::Op("-")) {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat(&Tok::Op("!")) || self.eat_word("not") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.pow()
    }
    fn pow(&mut self) -> PR {
        let b = self.postfix()?;
        if self.eat(&Tok::Op("^")) {
            let e = self.unary()?;
            return Ok(Expr::Bin("^", Box::new(b), Box::new(e)));
        }
        Ok(b)
    }
    fn args(&mut self) -> Result<Vec<Expr>, String> {
        let mut v = vec![];
        if self.eat(&Tok::RP) {
            return Ok(v);
        }
        loop {
            v.push(self.ternary()?);
            if self.eat(&Tok::Comma) {
                continue;
            }
            if self.eat(&Tok::RP) {
                return Ok(v);
            }
            return Err("Expected “,” or “)”".into());
        }
    }
    fn postfix(&mut self) -> PR {
        let mut e = self.primary()?;
        while self.eat(&Tok::Dot) {
            let Some(Tok::Id(name)) = self.peek().cloned() else { return Err("Expected a function name after “.”".into()) };
            self.i += 1;
            let mut a = vec![e];
            if self.eat(&Tok::LP) {
                a.extend(self.args()?);
            }
            e = Expr::Call(name, a);
        }
        Ok(e)
    }
    fn primary(&mut self) -> PR {
        let Some(t) = self.peek().cloned() else { return Err("Unexpected end of formula".into()) };
        self.i += 1;
        match t {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Id(name) => {
                if self.eat(&Tok::LP) {
                    return Ok(Expr::Call(name, self.args()?));
                }
                match name.to_lowercase().as_str() {
                    "true" => Ok(Expr::Bool(true)),
                    "false" => Ok(Expr::Bool(false)),
                    "pi" => Ok(Expr::Num(std::f64::consts::PI)),
                    "e" => Ok(Expr::Num(std::f64::consts::E)),
                    _ => Err(format!("Unknown name “{name}” — use prop(\"{name}\") for properties")),
                }
            }
            Tok::LP => {
                let e = self.ternary()?;
                if !self.eat(&Tok::RP) {
                    return Err("Missing “)”".into());
                }
                Ok(e)
            }
            Tok::LB => {
                let mut v = vec![];
                if !self.eat(&Tok::RB) {
                    loop {
                        v.push(self.ternary()?);
                        if self.eat(&Tok::Comma) {
                            continue;
                        }
                        if self.eat(&Tok::RB) {
                            break;
                        }
                        return Err("Expected “,” or “]”".into());
                    }
                }
                Ok(Expr::List(v))
            }
            Tok::RP => Err("Unexpected “)”".into()),
            Tok::Op(o) => Err(format!("Unexpected “{o}”")),
            Tok::Comma => Err("Unexpected “,”".into()),
            Tok::Colon => Err("Unexpected “:”".into()),
            Tok::Q => Err("Unexpected “?”".into()),
            _ => Err("Unexpected token".into()),
        }
    }
}

pub fn parse(src: &str) -> Result<Expr, String> {
    if src.trim().is_empty() {
        return Err("Type a formula, e.g. prop(\"Price\") * 2".into());
    }
    let mut p = Parser { t: lex(src)?, i: 0 };
    let e = p.ternary()?;
    if p.i < p.t.len() {
        return Err("Unexpected text after the end of the formula".into());
    }
    Ok(e)
}

// ───────────────────────────── evaluation ─────────────────────────────

#[derive(Clone, Copy)]
struct Cx<'a> {
    ws: &'a Workspace,
    db: &'a Database,
    row: &'a Page,
    depth: u8,
}

fn find_prop<'a>(db: &'a Database, name: &str) -> Option<&'a Prop> {
    db.props.iter().find(|p| p.name == name).or_else(|| db.props.iter().find(|p| p.name.eq_ignore_ascii_case(name)))
}

fn title_of(p: &Page) -> String {
    if p.title.is_empty() {
        "Untitled".into()
    } else {
        p.title.clone()
    }
}

fn prop_value(cx: Cx, p: &Prop) -> Result<V, String> {
    if cx.depth > 8 {
        return Err("Circular reference".into());
    }
    let val = cx.row.props.get(&p.id);
    Ok(match p.kind {
        PropKind::Text | PropKind::Url => match val {
            Some(Val::Text(s)) => V::Str(s.clone()),
            _ => V::Str(String::new()),
        },
        PropKind::Number => match val {
            Some(Val::Num(n)) => V::Num(*n),
            _ => V::Empty,
        },
        PropKind::Checkbox => V::Bool(matches!(val, Some(Val::Check(true)))),
        PropKind::Select => match val {
            Some(Val::Sel(o)) => cx.db.opt(p.id, *o).map(|o| V::Str(o.name.clone())).unwrap_or(V::Empty),
            _ => V::Empty,
        },
        PropKind::MultiSelect => match val {
            Some(Val::Multi(m)) => V::List(m.iter().filter_map(|o| cx.db.opt(p.id, *o)).map(|o| V::Str(o.name.clone())).collect()),
            _ => V::List(vec![]),
        },
        PropKind::Date => match val {
            Some(Val::Text(s)) => dates::span(s).map(|(a, b)| V::Date(a, b)).unwrap_or(V::Empty),
            _ => V::Empty,
        },
        PropKind::Relation => V::List(related(cx.ws, cx.row, p).iter().map(|r| V::Str(title_of(r))).collect()),
        PropKind::Formula => {
            let e = parse(&p.cfg.formula)?;
            eval(&e, Cx { depth: cx.depth + 1, ..cx })?
        }
        PropKind::Rollup => rollup(cx, p)?,
    })
}

/// Rows linked through relation property `p` of `row` (missing / trashed rows are skipped).
pub fn related<'a>(ws: &'a Workspace, row: &Page, p: &Prop) -> Vec<&'a Page> {
    match row.props.get(&p.id) {
        Some(Val::Multi(ids)) => ids.iter().filter_map(|id| ws.page(*id)).filter(|r| !r.trashed && r.row).collect(),
        _ => vec![],
    }
}

fn rollup(cx: Cx, p: &Prop) -> Result<V, String> {
    let Some(rel) = cx.db.prop(p.cfg.rollup_rel).filter(|r| r.kind == PropKind::Relation) else {
        return Err("Pick a relation to roll up".into());
    };
    let Some(tdb) = cx.ws.db(rel.cfg.relation) else { return Err("The relation has no database".into()) };
    let rows = related(cx.ws, cx.row, rel);
    let tprop = if p.cfg.rollup_prop == 0 { None } else { tdb.prop(p.cfg.rollup_prop) };
    let mut vals: Vec<V> = vec![];
    for r in &rows {
        vals.push(match tprop {
            None => V::Str(title_of(r)),
            Some(tp) => prop_value(Cx { ws: cx.ws, db: tdb, row: r, depth: cx.depth + 1 }, tp)?,
        });
    }
    let flat: Vec<V> = vals.iter().flat_map(|v| if let V::List(l) = v { l.clone() } else { vec![v.clone()] }).collect();
    let nonempty: Vec<&V> = flat.iter().filter(|v| !v.is_empty()).collect();
    let nums: Vec<f64> = nonempty
        .iter()
        .filter_map(|v| match v {
            V::Num(n) => Some(*n),
            V::Date(a, _) => Some(*a as f64),
            _ => None,
        })
        .collect();
    use RollupFn::*;
    Ok(match p.cfg.rollup_fn {
        Count => V::Num(rows.len() as f64),
        CountValues => V::Num(nonempty.len() as f64),
        Unique => {
            let mut seen: Vec<String> = vec![];
            for v in &nonempty {
                let t = v.text();
                if !seen.contains(&t) {
                    seen.push(t);
                }
            }
            V::Num(seen.len() as f64)
        }
        Sum => V::Num(nums.iter().sum()),
        Average => {
            if nums.is_empty() {
                V::Empty
            } else {
                V::Num(nums.iter().sum::<f64>() / nums.len() as f64)
            }
        }
        Min => nums.iter().cloned().fold(None, |a: Option<f64>, x| Some(a.map_or(x, |y| y.min(x)))).map(V::Num).unwrap_or(V::Empty),
        Max => nums.iter().cloned().fold(None, |a: Option<f64>, x| Some(a.map_or(x, |y| y.max(x)))).map(V::Num).unwrap_or(V::Empty),
        Median => {
            if nums.is_empty() {
                V::Empty
            } else {
                let mut s = nums.clone();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let m = s.len() / 2;
                V::Num(if s.len() % 2 == 1 { s[m] } else { (s[m - 1] + s[m]) / 2.0 })
            }
        }
        Range => {
            if nums.is_empty() {
                V::Empty
            } else {
                let mx = nums.iter().cloned().fold(f64::MIN, f64::max);
                let mn = nums.iter().cloned().fold(f64::MAX, f64::min);
                V::Num(mx - mn)
            }
        }
        PercentChecked => {
            if flat.is_empty() {
                V::Empty
            } else {
                V::Num((flat.iter().filter(|v| matches!(v, V::Bool(true))).count() as f64 / flat.len() as f64 * 100.0).round())
            }
        }
        PercentEmpty => {
            if flat.is_empty() {
                V::Empty
            } else {
                V::Num((flat.iter().filter(|v| v.is_empty()).count() as f64 / flat.len() as f64 * 100.0).round())
            }
        }
        ShowOriginal => V::List(nonempty.into_iter().cloned().collect()),
        Earliest | Latest => {
            let ds: Vec<(i64, i64)> = flat.iter().filter_map(|v| if let V::Date(a, b) = v { Some((*a, *b)) } else { None }).collect();
            if ds.is_empty() {
                V::Empty
            } else if p.cfg.rollup_fn == Earliest {
                let m = ds.iter().map(|d| d.0).min().unwrap_or(0);
                V::Date(m, m)
            } else {
                let m = ds.iter().map(|d| d.1).max().unwrap_or(0);
                V::Date(m, m)
            }
        }
    })
}

fn unit_days(n: &str) -> Option<f64> {
    match n.to_lowercase().trim_end_matches('s') {
        "day" => Some(1.0),
        "week" => Some(7.0),
        "hour" => Some(1.0 / 24.0),
        "minute" => Some(1.0 / 1440.0),
        _ => None,
    }
}

fn months_between(a: i64, b: i64) -> f64 {
    let (ya, ma, da) = dates::civil_from_days(a);
    let (yb, mb, db) = dates::civil_from_days(b);
    let mut m = (ya - yb) as i64 * 12 + ma as i64 - mb as i64;
    if m > 0 && da < db {
        m -= 1;
    } else if m < 0 && da > db {
        m += 1;
    }
    m as f64
}

fn add_months(day: i64, n: i64) -> i64 {
    let (y, m, d) = dates::civil_from_days(day);
    let total = y as i64 * 12 + (m as i64 - 1) + n;
    let (ny, nm) = ((total.div_euclid(12)) as i32, (total.rem_euclid(12) + 1) as u32);
    dates::days_from_civil(ny, nm, d.min(dates::days_in_month(ny, nm)))
}

fn as_day(v: &V) -> Result<i64, String> {
    match v {
        V::Date(a, _) => Ok(*a),
        V::Str(s) => dates::span(s).map(|x| x.0).ok_or_else(|| format!("“{s}” is not a date")),
        V::Num(n) => Ok(*n as i64),
        _ => Err("Expected a date".into()),
    }
}

fn format_date(day: i64, fmt: &str) -> String {
    let (y, m, d) = dates::civil_from_days(day);
    let wd = dates::weekday(y, m, d) as usize;
    let full_wd = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"][wd];
    let cs: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        let rest: String = cs[i..].iter().collect();
        let toks: [(&str, String); 10] = [
            ("YYYY", format!("{y:04}")),
            ("MMMM", dates::MONTHS[m as usize - 1].to_owned()),
            ("dddd", full_wd.to_owned()),
            ("MMM", dates::MONTHS[m as usize - 1][..3].to_owned()),
            ("ddd", full_wd[..3].to_owned()),
            ("YY", format!("{:02}", y.rem_euclid(100))),
            ("MM", format!("{m:02}")),
            ("DD", format!("{d:02}")),
            ("M", m.to_string()),
            ("D", d.to_string()),
        ];
        if let Some((t, v)) = toks.iter().find(|(t, _)| rest.starts_with(t)) {
            out.push_str(v);
            i += t.len();
        } else {
            out.push(cs[i]);
            i += 1;
        }
    }
    out
}

fn eval(e: &Expr, cx: Cx) -> Result<V, String> {
    Ok(match e {
        Expr::Num(n) => V::Num(*n),
        Expr::Str(s) => V::Str(s.clone()),
        Expr::Bool(b) => V::Bool(*b),
        Expr::List(l) => V::List(l.iter().map(|x| eval(x, cx)).collect::<Result<_, _>>()?),
        Expr::Neg(x) => V::Num(-eval(x, cx)?.num()),
        Expr::Not(x) => V::Bool(!eval(x, cx)?.truthy()),
        Expr::Cond(c, a, b) => {
            if eval(c, cx)?.truthy() {
                eval(a, cx)?
            } else {
                eval(b, cx)?
            }
        }
        Expr::Bin(op, a, b) => {
            if *op == "&&" {
                return Ok(V::Bool(eval(a, cx)?.truthy() && eval(b, cx)?.truthy()));
            }
            if *op == "||" {
                return Ok(V::Bool(eval(a, cx)?.truthy() || eval(b, cx)?.truthy()));
            }
            let (x, y) = (eval(a, cx)?, eval(b, cx)?);
            match *op {
                "+" => match (&x, &y) {
                    (V::Num(_) | V::Empty | V::Bool(_), V::Num(_) | V::Empty | V::Bool(_)) => V::Num(x.num() + y.num()),
                    (V::Date(a, b), V::Num(n)) => V::Date(a + *n as i64, b + *n as i64),
                    _ => V::Str(format!("{}{}", x.text(), y.text())),
                },
                "-" => match (&x, &y) {
                    (V::Date(a, _), V::Date(b, _)) => V::Num((a - b) as f64),
                    (V::Date(a, b), V::Num(n)) => V::Date(a - *n as i64, b - *n as i64),
                    _ => V::Num(x.num() - y.num()),
                },
                "*" => V::Num(x.num() * y.num()),
                "/" => V::Num(x.num() / y.num()),
                "%" => V::Num(x.num() % y.num()),
                "^" => V::Num(x.num().powf(y.num())),
                "==" => V::Bool(veq(&x, &y)),
                "!=" => V::Bool(!veq(&x, &y)),
                _ => {
                    let ord = match (&x, &y) {
                        (V::Str(p), V::Str(q)) => p.to_lowercase().cmp(&q.to_lowercase()),
                        (V::Date(p, _), V::Date(q, _)) => p.cmp(q),
                        _ => x.num().partial_cmp(&y.num()).unwrap_or(std::cmp::Ordering::Equal),
                    };
                    V::Bool(match *op {
                        "<" => ord.is_lt(),
                        "<=" => ord.is_le(),
                        ">" => ord.is_gt(),
                        _ => ord.is_ge(),
                    })
                }
            }
        }
        Expr::Call(name, args) => call(&name.to_lowercase(), args, cx)?,
    })
}

fn call(name: &str, args: &[Expr], cx: Cx) -> Result<V, String> {
    // lazily evaluated forms
    match name {
        "if" => {
            if args.len() != 3 {
                return Err("if(condition, then, else) needs 3 arguments".into());
            }
            return if eval(&args[0], cx)?.truthy() { eval(&args[1], cx) } else { eval(&args[2], cx) };
        }
        "ifs" => {
            let mut i = 0;
            while i + 1 < args.len() {
                if eval(&args[i], cx)?.truthy() {
                    return eval(&args[i + 1], cx);
                }
                i += 2;
            }
            return if args.len() % 2 == 1 { eval(&args[args.len() - 1], cx) } else { Ok(V::Empty) };
        }
        "prop" => {
            let Some(a) = args.first() else { return Err("prop(\"Name\") needs a property name".into()) };
            let n = eval(a, cx)?.text();
            return match find_prop(cx.db, &n) {
                Some(p) => prop_value(cx, p),
                None if n.eq_ignore_ascii_case("name") || n.eq_ignore_ascii_case("title") => Ok(V::Str(cx.row.title.clone())),
                None => Err(format!("No property called “{n}”")),
            };
        }
        "now" | "today" => return Ok(V::Date(dates::today_day(), dates::today_day())),
        _ => {}
    }
    let a: Vec<V> = args.iter().map(|x| eval(x, cx)).collect::<Result<_, _>>()?;
    let need = |n: usize| -> Result<(), String> {
        if a.len() < n {
            Err(format!("{name}() needs at least {n} argument{}", if n == 1 { "" } else { "s" }))
        } else {
            Ok(())
        }
    };
    let flat_nums = || -> Vec<f64> { a.iter().flat_map(|v| if let V::List(l) = v { l.iter().map(|x| x.num()).collect() } else { vec![v.num()] }).collect() };
    let n1 = |f: fn(f64) -> f64| -> Result<V, String> {
        need(1)?;
        Ok(V::Num(f(a[0].num())))
    };
    Ok(match name {
        "and" => V::Bool(a.iter().all(|v| v.truthy())),
        "or" => V::Bool(a.iter().any(|v| v.truthy())),
        "not" => {
            need(1)?;
            V::Bool(!a[0].truthy())
        }
        "empty" => {
            need(1)?;
            V::Bool(a[0].is_empty())
        }
        "length" => {
            need(1)?;
            match &a[0] {
                V::List(l) => V::Num(l.len() as f64),
                v => V::Num(v.text().chars().count() as f64),
            }
        }
        "concat" => V::Str(a.iter().map(|v| v.text()).collect()),
        "join" => {
            need(1)?;
            let sep = a.get(1).map(|v| v.text()).unwrap_or_else(|| ", ".into());
            match &a[0] {
                V::List(l) => V::Str(l.iter().map(|v| v.text()).collect::<Vec<_>>().join(&sep)),
                v => V::Str(v.text()),
            }
        }
        "slice" => {
            need(2)?;
            let s: Vec<char> = a[0].text().chars().collect();
            let st = (a[1].num().max(0.0) as usize).min(s.len());
            let en = a.get(2).map(|v| (v.num().max(0.0) as usize).min(s.len())).unwrap_or(s.len()).max(st);
            V::Str(s[st..en].iter().collect())
        }
        "contains" | "test" => {
            need(2)?;
            match &a[0] {
                V::List(l) => V::Bool(l.iter().any(|v| veq(v, &a[1]))),
                v => V::Bool(v.text().to_lowercase().contains(&a[1].text().to_lowercase())),
            }
        }
        "replace" => {
            need(3)?;
            V::Str(a[0].text().replacen(&a[1].text(), &a[2].text(), 1))
        }
        "replaceall" => {
            need(3)?;
            V::Str(a[0].text().replace(&a[1].text(), &a[2].text()))
        }
        "lower" => {
            need(1)?;
            V::Str(a[0].text().to_lowercase())
        }
        "upper" => {
            need(1)?;
            V::Str(a[0].text().to_uppercase())
        }
        "trim" => {
            need(1)?;
            V::Str(a[0].text().trim().to_owned())
        }
        "format" => {
            need(1)?;
            V::Str(a[0].text())
        }
        "tonumber" => {
            need(1)?;
            let t: String = a[0].text().chars().filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-')).collect();
            V::Num(t.parse().unwrap_or(0.0))
        }
        "round" => {
            need(1)?;
            let p = 10f64.powi(a.get(1).map(|v| v.num()).unwrap_or(0.0) as i32);
            V::Num((a[0].num() * p).round() / p)
        }
        "floor" => return n1(f64::floor),
        "ceil" => return n1(f64::ceil),
        "abs" => return n1(f64::abs),
        "sqrt" => return n1(f64::sqrt),
        "cbrt" => return n1(f64::cbrt),
        "exp" => return n1(f64::exp),
        "ln" => return n1(f64::ln),
        "log10" | "log" => return n1(f64::log10),
        "log2" => return n1(f64::log2),
        "sign" => return n1(|x| if x == 0.0 { 0.0 } else { x.signum() }),
        "pow" => {
            need(2)?;
            V::Num(a[0].num().powf(a[1].num()))
        }
        "mod" => {
            need(2)?;
            V::Num(a[0].num() % a[1].num())
        }
        "min" => V::Num(flat_nums().into_iter().fold(f64::INFINITY, f64::min)),
        "max" => V::Num(flat_nums().into_iter().fold(f64::NEG_INFINITY, f64::max)),
        "sum" => V::Num(flat_nums().iter().sum()),
        "mean" | "average" => {
            let n = flat_nums();
            V::Num(if n.is_empty() { 0.0 } else { n.iter().sum::<f64>() / n.len() as f64 })
        }
        "median" => {
            let mut n = flat_nums();
            n.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
            V::Num(if n.is_empty() {
                0.0
            } else if n.len() % 2 == 1 {
                n[n.len() / 2]
            } else {
                (n[n.len() / 2 - 1] + n[n.len() / 2]) / 2.0
            })
        }
        "first" | "last" | "at" => {
            need(1)?;
            let l = if let V::List(l) = &a[0] { l.clone() } else { vec![a[0].clone()] };
            let i = match name {
                "first" => 0,
                "last" => l.len().saturating_sub(1),
                _ => a.get(1).map(|v| v.num().max(0.0) as usize).unwrap_or(0),
            };
            l.get(i).cloned().unwrap_or(V::Empty)
        }
        "reverse" => {
            need(1)?;
            match &a[0] {
                V::List(l) => V::List(l.iter().rev().cloned().collect()),
                v => V::Str(v.text().chars().rev().collect()),
            }
        }
        "sort" => {
            need(1)?;
            match &a[0] {
                V::List(l) => {
                    let mut l = l.clone();
                    l.sort_by(|x, y| x.num().partial_cmp(&y.num()).filter(|o| !o.is_eq()).unwrap_or_else(|| x.text().cmp(&y.text())));
                    V::List(l)
                }
                v => v.clone(),
            }
        }
        "unique" => {
            need(1)?;
            match &a[0] {
                V::List(l) => {
                    let mut out: Vec<V> = vec![];
                    for v in l {
                        if !out.iter().any(|o| veq(o, v)) {
                            out.push(v.clone());
                        }
                    }
                    V::List(out)
                }
                v => v.clone(),
            }
        }
        "year" | "month" | "date" | "day" => {
            need(1)?;
            let (y, m, d) = dates::civil_from_days(as_day(&a[0])?);
            V::Num(match name {
                "year" => y as f64,
                "month" => m as f64,
                "date" => d as f64,
                _ => (dates::weekday(y, m, d) + 1) as f64,
            })
        }
        "start" | "datestart" => {
            need(1)?;
            let d = as_day(&a[0])?;
            V::Date(d, d)
        }
        "end" | "dateend" => {
            need(1)?;
            match &a[0] {
                V::Date(_, b) => V::Date(*b, *b),
                v => {
                    let d = as_day(v)?;
                    V::Date(d, d)
                }
            }
        }
        "datebetween" => {
            need(3)?;
            let (x, y) = (as_day(&a[0])?, as_day(&a[1])?);
            let unit = a[2].text();
            if unit.to_lowercase().starts_with("month") {
                V::Num(months_between(x, y))
            } else if unit.to_lowercase().starts_with("year") {
                V::Num((months_between(x, y) / 12.0).trunc())
            } else {
                let u = unit_days(&unit).ok_or("Unit must be days, weeks, months or years")?;
                V::Num(((x - y) as f64 / u).trunc())
            }
        }
        "dateadd" | "datesubtract" => {
            need(3)?;
            let d = as_day(&a[0])?;
            let sign = if name == "dateadd" { 1 } else { -1 };
            let n = a[1].num() as i64 * sign;
            let unit = a[2].text().to_lowercase();
            let nd = if unit.starts_with("month") {
                add_months(d, n)
            } else if unit.starts_with("year") {
                add_months(d, n * 12)
            } else {
                d + (n as f64 * unit_days(&unit).ok_or("Unit must be days, weeks, months or years")?) as i64
            };
            let span = match &a[0] {
                V::Date(x, y) => y - x,
                _ => 0,
            };
            V::Date(nd, nd + span)
        }
        "formatdate" => {
            need(2)?;
            V::Str(format_date(as_day(&a[0])?, &a[1].text()))
        }
        "id" => V::Str(cx.row.id.to_string()),
        "name" => V::Str(cx.row.title.clone()),
        other => return Err(format!("Unknown function “{other}”")),
    })
}

/// Evaluates an expression for one row (used by the formula editor preview).
pub fn eval_str(ws: &Workspace, db_id: u64, row_id: u64, src: &str) -> Result<V, String> {
    let e = parse(src)?;
    let db = ws.db(db_id).ok_or("No database")?;
    let row = ws.page(row_id).ok_or("No row")?;
    eval(&e, Cx { ws, db, row, depth: 0 })
}

fn compute(ws: &Workspace, db: &Database, row: &Page, p: &Prop) -> Option<Val> {
    let cx = Cx { ws, db, row, depth: 0 };
    match prop_value(cx, p) {
        Ok(v) => v.to_val(),
        Err(e) => Some(Val::Text(format!("⚠ {e}"))),
    }
}

/// Re-evaluates all formulas and rollups, caching the results in the rows. Returns true when something changed.
pub fn recompute(ws: &mut Workspace) -> bool {
    let mut any = false;
    for _pass in 0..3 {
        let mut updates: Vec<(u64, u64, Option<Val>)> = vec![];
        for page in ws.pages.iter().filter(|p| p.db.is_some() && !p.trashed) {
            let Some(db) = page.db.as_ref() else { continue };
            let cps: Vec<&Prop> = db.props.iter().filter(|p| p.kind.computed()).collect();
            if cps.is_empty() {
                continue;
            }
            for row in ws.pages.iter().filter(|r| r.row && r.parent == Some(page.id) && !r.trashed) {
                for p in &cps {
                    let nv = compute(ws, db, row, p);
                    if row.props.get(&p.id) != nv.as_ref() {
                        updates.push((row.id, p.id, nv));
                    }
                }
            }
        }
        if updates.is_empty() {
            break;
        }
        any = true;
        for (r, p, v) in updates {
            if let Some(row) = ws.page_mut(r) {
                match v {
                    Some(v) => {
                        row.props.insert(p, v);
                    }
                    None => {
                        row.props.remove(&p);
                    }
                }
            }
        }
    }
    any
}

pub const HELP: &str = "Functions: if · ifs · and · or · not · empty · length · concat · join · slice · contains · replace · replaceAll · lower · upper · trim · format · toNumber · round · floor · ceil · abs · sqrt · pow · mod · min · max · sum · mean · median · first · last · sort · unique · now · today · dateBetween · dateAdd · dateSubtract · formatDate · year · month · date · day · start · end.   Operators: + − * / % ^   == != < > <= >=   and  or  not   a ? b : c";

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_with_rows() -> (Workspace, u64, Vec<u64>) {
        let mut ws = Workspace::seed();
        let db = ws.add_database(None, ViewKind::Table);
        let price = ws.add_prop(db, "Price", PropKind::Number, &[]);
        let qty = ws.add_prop(db, "Qty", PropKind::Number, &[]);
        let total = ws.add_prop(db, "Total", PropKind::Formula, &[]);
        let rows: Vec<u64> = ws.rows(db);
        for (i, r) in rows.iter().enumerate() {
            let p = ws.page_mut(*r).unwrap();
            p.title = format!("Item {}", i + 1);
            p.props.insert(price, Val::Num(2.5 * (i as f64 + 1.0)));
            p.props.insert(qty, Val::Num(i as f64 + 1.0));
        }
        ws.db_mut(db).unwrap().props.iter_mut().find(|p| p.id == total).unwrap().cfg.formula = "round(prop(\"Price\") * prop(\"Qty\") + 1, 1)".into();
        (ws, db, rows)
    }

    #[test]
    fn formulas_compute_and_cache() {
        let (mut ws, db, rows) = ws_with_rows();
        assert!(recompute(&mut ws));
        let total = ws.db(db).unwrap().props.iter().find(|p| p.name == "Total").unwrap().id;
        assert_eq!(ws.page(rows[1]).unwrap().props.get(&total), Some(&Val::Num(11.0)));
        assert!(!recompute(&mut ws), "second pass is stable");
    }

    #[test]
    fn expressions() {
        let (ws, db, rows) = ws_with_rows();
        let ev = |s: &str| eval_str(&ws, db, rows[0], s);
        assert_eq!(ev("1 + 2 * 3 ^ 2").unwrap(), V::Num(19.0));
        assert_eq!(ev("if(2 > 1 and not false, \"yes\", \"no\")").unwrap(), V::Str("yes".into()));
        assert_eq!(ev("prop(\"Name\").length() + 1").unwrap(), V::Num(7.0));
        assert_eq!(ev("\"a\" + 1").unwrap(), V::Str("a1".into()));
        assert_eq!(ev("dateBetween(dateAdd(today(), 10, \"days\"), today(), \"days\")").unwrap(), V::Num(10.0));
        assert_eq!(ev("formatDate(dateAdd(start(\"2026-01-31\"), 1, \"months\"), \"YYYY-MM-DD\")").unwrap(), V::Str("2026-02-28".into()));
        assert_eq!(ev("max(1, 5, 3) > 4 ? 10 : 20").unwrap(), V::Num(10.0));
        assert!(ev("1 +").is_err());
        assert!(ev("nope(1)").is_err());
        assert!(ev("prop(\"Missing\")").is_err());
    }

    #[test]
    fn relation_and_rollup() {
        let (mut ws, items, rows) = ws_with_rows();
        let price = ws.db(items).unwrap().props.iter().find(|p| p.name == "Price").unwrap().id;
        let orders = ws.add_database(None, ViewKind::Table);
        let rel = ws.add_prop(orders, "Items", PropKind::Relation, &[]);
        let roll = ws.add_prop(orders, "Sum", PropKind::Rollup, &[]);
        {
            let d = ws.db_mut(orders).unwrap();
            d.props.iter_mut().find(|p| p.id == rel).unwrap().cfg.relation = items;
            let c = &mut d.props.iter_mut().find(|p| p.id == roll).unwrap().cfg;
            c.rollup_rel = rel;
            c.rollup_prop = price;
            c.rollup_fn = RollupFn::Sum;
        }
        let order = ws.rows(orders)[0];
        ws.page_mut(order).unwrap().props.insert(rel, Val::Multi(vec![rows[0], rows[2]]));
        recompute(&mut ws);
        assert_eq!(ws.page(order).unwrap().props.get(&roll), Some(&Val::Num(10.0)));
    }
}
