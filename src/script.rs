//! LumenScript — the sandboxed scripting language of Lumen's automation engine.
//!
//! * Pure Rust, no dependencies, no I/O, no clock: it runs identically in the Windows build, in WebAssembly and in a future headless server.
//! * Safe by construction: a script can only touch the outside world through the [`Host`] trait (Lumen exposes rows, databases, notifications).
//!   There is no file / network / process access and no `import`.
//! * Hard limits: an instruction budget (infinite loops stop), call-depth limit, parser nesting limit, string / list size limits, output limit.
//! * JavaScript / Python flavoured so people — and AI agents — can write it without learning anything new:
//!
//! ```text
//! // comments
//! let total = 0
//! for t in db("Tasks").rows() {
//!     if t.Status == "Done" && t["Due date"] < today() { total += 1 }
//! }
//! notify("{total} finished tasks")
//! ```
//!
//! This file is self-contained (it only uses `std`), so it is unit-tested on its own and can be reused by other front-ends.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::rc::Rc;

// ───────────────────────────── limits ─────────────────────────────

pub const MAX_STEPS: u64 = 400_000;
const MAX_DEPTH: u32 = 40;
const MAX_NEST: u32 = 80;
const MAX_STR: usize = 1_000_000;
const MAX_LIST: usize = 200_000;
const MAX_OUT_LINES: usize = 400;
const MAX_SRC: usize = 200_000;

// ───────────────────────────── errors ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptError {
    pub msg: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "line {}:{} — {}", self.line, self.col, self.msg)
        } else {
            write!(f, "{}", self.msg)
        }
    }
}

fn serr(msg: impl Into<String>, line: u32, col: u32) -> ScriptError {
    ScriptError { msg: msg.into(), line, col }
}

// ───────────────────────────── values ─────────────────────────────

/// Reference to an object that lives in the host (a database row, a database …).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HostRef {
    pub kind: u8,
    pub id: u64,
}

pub struct Closure {
    def: Rc<FnDef>,
    env: Env,
}

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    List(Rc<RefCell<Vec<Value>>>),
    Map(Rc<RefCell<BTreeMap<String, Value>>>),
    Func(Rc<Closure>),
    Native(Rc<str>),
    Host(HostRef),
}

impl Value {
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }
    pub fn list(v: Vec<Value>) -> Value {
        Value::List(Rc::new(RefCell::new(v)))
    }
    pub fn map(m: BTreeMap<String, Value>) -> Value {
        Value::Map(Rc::new(RefCell::new(m)))
    }
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            _ => true,
        }
    }
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Func(_) | Value::Native(_) => "function",
            Value::Host(h) => match h.kind {
                1 => "row",
                2 => "database",
                _ => "object",
            },
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "inf".into() } else { "-inf".into() }
    } else if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Host(x), Value::Host(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).map_or(false, |w| values_equal(v, w)))
        }
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        (Value::Native(x), Value::Native(y)) => x == y,
        _ => false,
    }
}

fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Str(x), Value::Str(y)) => x.to_lowercase().cmp(&y.to_lowercase()).then_with(|| x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Greater, // empty values sort last
        (_, Value::Null) => Less,
        _ => a.type_name().cmp(b.type_name()),
    }
}

// ───────────────────────────── host interface ─────────────────────────────

/// Everything a script can do to the outside world goes through this trait.
pub trait Host {
    /// Is there a host function with this name (`notify`, `db`, `today` …)?
    fn has_fn(&self, name: &str) -> bool;
    /// Value of a host global (`row`, `trigger` …).
    fn global(&mut self, name: &str) -> Option<Value>;
    /// Call a host function.
    fn call(&mut self, name: &str, args: &[Value]) -> Result<Value, String>;
    fn get(&mut self, o: HostRef, key: &str) -> Result<Value, String>;
    fn set(&mut self, o: HostRef, key: &str, v: Value) -> Result<(), String>;
    fn method(&mut self, o: HostRef, name: &str, args: &[Value]) -> Result<Value, String>;
    /// Text shown by `print` / interpolation for a host object.
    fn display(&mut self, o: HostRef) -> String;
}

/// A host without any capabilities (tests, syntax checks).
pub struct NoHost;

impl Host for NoHost {
    fn has_fn(&self, _: &str) -> bool {
        false
    }
    fn global(&mut self, _: &str) -> Option<Value> {
        None
    }
    fn call(&mut self, n: &str, _: &[Value]) -> Result<Value, String> {
        Err(format!("unknown function {n}"))
    }
    fn get(&mut self, _: HostRef, k: &str) -> Result<Value, String> {
        Err(format!("no property {k}"))
    }
    fn set(&mut self, _: HostRef, k: &str, _: Value) -> Result<(), String> {
        Err(format!("cannot set {k}"))
    }
    fn method(&mut self, _: HostRef, n: &str, _: &[Value]) -> Result<Value, String> {
        Err(format!("no method {n}"))
    }
    fn display(&mut self, _: HostRef) -> String {
        "<object>".into()
    }
}

// ───────────────────────────── lexer ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum SPart {
    Lit(String),
    Code(String, u32, u32),
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(Vec<SPart>),
    Ident(String),
    P(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
struct Token {
    t: Tok,
    line: u32,
    col: u32,
    /// A line break precedes this token (statements end there).
    nl: bool,
}

const PUNCT: [&str; 31] = [
    "**", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "..", "+=", "-=", "*=", "/=", "%=", "+", "-", "*", "/", "%", "=", "<", ">", "!", "?", ":", ",", ".", ";", "|", "&",
];
const PUNCT1: [&str; 6] = ["(", ")", "{", "}", "[", "]"];

fn lex(src: &str, line0: u32, col0: u32) -> Result<Vec<Token>, ScriptError> {
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let (mut line, mut col) = (line0, col0);
    let mut nl = false;
    let mut out: Vec<Token> = vec![];
    macro_rules! adv {
        () => {{
            if cs[i] == '\n' {
                line += 1;
                col = 1;
                nl = true;
            } else {
                col += 1;
            }
            i += 1;
        }};
    }
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            adv!();
            continue;
        }
        if (c == '/' && cs.get(i + 1) == Some(&'/')) || c == '#' {
            while i < cs.len() && cs[i] != '\n' {
                adv!();
            }
            continue;
        }
        if c == '/' && cs.get(i + 1) == Some(&'*') {
            let (l0, c0) = (line, col);
            adv!();
            adv!();
            loop {
                if i >= cs.len() {
                    return Err(serr("unterminated /* comment", l0, c0));
                }
                if cs[i] == '*' && cs.get(i + 1) == Some(&'/') {
                    adv!();
                    adv!();
                    break;
                }
                adv!();
            }
            continue;
        }
        let (tl, tc, tnl) = (line, col, nl);
        nl = false;
        if c.is_ascii_digit() {
            let mut s = String::new();
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '_') {
                if cs[i] != '_' {
                    s.push(cs[i]);
                }
                adv!();
            }
            if i + 1 < cs.len() && cs[i] == '.' && cs[i + 1].is_ascii_digit() {
                s.push('.');
                adv!();
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '_') {
                    if cs[i] != '_' {
                        s.push(cs[i]);
                    }
                    adv!();
                }
            }
            if i < cs.len() && (cs[i] == 'e' || cs[i] == 'E') && cs.get(i + 1).map_or(false, |d| d.is_ascii_digit() || *d == '-' || *d == '+') {
                s.push('e');
                adv!();
                if cs[i] == '-' || cs[i] == '+' {
                    s.push(cs[i]);
                    adv!();
                }
                while i < cs.len() && cs[i].is_ascii_digit() {
                    s.push(cs[i]);
                    adv!();
                }
            }
            let n: f64 = s.parse().map_err(|_| serr(format!("bad number {s}"), tl, tc))?;
            out.push(Token { t: Tok::Num(n), line: tl, col: tc, nl: tnl });
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let mut s = String::new();
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_' || cs[i] == '$') {
                s.push(cs[i]);
                adv!();
            }
            out.push(Token { t: Tok::Ident(s), line: tl, col: tc, nl: tnl });
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            let interp = c != '\'';
            let q = c;
            adv!();
            let mut parts: Vec<SPart> = vec![];
            let mut lit = String::new();
            loop {
                if i >= cs.len() {
                    return Err(serr("unterminated string", tl, tc));
                }
                let ch = cs[i];
                if ch == q {
                    adv!();
                    break;
                }
                if ch == '\\' {
                    adv!();
                    if i >= cs.len() {
                        return Err(serr("unterminated string", tl, tc));
                    }
                    let e = cs[i];
                    lit.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        o => o,
                    });
                    adv!();
                    continue;
                }
                if ch == '{' && interp {
                    adv!();
                    let (cl, cc) = (line, col);
                    let mut depth = 1;
                    let mut code = String::new();
                    let mut inq: Option<char> = None;
                    loop {
                        if i >= cs.len() {
                            return Err(serr("unterminated {…} in string", tl, tc));
                        }
                        let d = cs[i];
                        if let Some(qq) = inq {
                            if d == qq {
                                inq = None;
                            }
                        } else if d == '\'' || d == '"' {
                            inq = Some(d);
                        } else if d == '{' {
                            depth += 1;
                        } else if d == '}' {
                            depth -= 1;
                            if depth == 0 {
                                adv!();
                                break;
                            }
                        }
                        code.push(d);
                        adv!();
                    }
                    if !lit.is_empty() {
                        parts.push(SPart::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(SPart::Code(code, cl, cc));
                    continue;
                }
                lit.push(ch);
                adv!();
            }
            if !lit.is_empty() || parts.is_empty() {
                parts.push(SPart::Lit(lit));
            }
            out.push(Token { t: Tok::Str(parts), line: tl, col: tc, nl: tnl });
            continue;
        }
        let rest: String = cs[i..(i + 2).min(cs.len())].iter().collect();
        if let Some(p) = PUNCT.iter().find(|p| rest.starts_with(**p)) {
            for _ in 0..p.len() {
                adv!();
            }
            out.push(Token { t: Tok::P(p), line: tl, col: tc, nl: tnl });
            continue;
        }
        if let Some(p) = PUNCT1.iter().find(|p| rest.starts_with(**p)) {
            adv!();
            out.push(Token { t: Tok::P(p), line: tl, col: tc, nl: tnl });
            continue;
        }
        return Err(serr(format!("unexpected character “{c}”"), tl, tc));
    }
    out.push(Token { t: Tok::Eof, line, col, nl: true });
    Ok(out)
}

// ───────────────────────────── AST ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Bop {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug)]
struct Expr {
    e: E,
    line: u32,
    col: u32,
}

#[derive(Debug)]
enum E {
    Null,
    Bool(bool),
    Num(f64),
    Str(Vec<StrPart>),
    List(Vec<Expr>),
    Map(Vec<(String, Expr)>),
    Ident(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Bop, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Assign(Box<Expr>, Option<Bop>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Member(Box<Expr>, String, bool),
    Index(Box<Expr>, Box<Expr>),
    Lambda(Rc<FnDef>),
    Range(Box<Expr>, Box<Expr>),
}

#[derive(Debug)]
enum StrPart {
    Lit(String),
    Code(Expr),
}

#[derive(Debug)]
pub struct FnDef {
    name: String,
    params: Vec<String>,
    body: Vec<Stmt>,
}

#[derive(Debug)]
struct Stmt {
    s: S,
    line: u32,
    col: u32,
}

#[derive(Debug)]
enum S {
    Expr(Expr),
    Let(String, Option<Expr>, bool),
    If(Vec<(Expr, Vec<Stmt>)>, Option<Vec<Stmt>>),
    While(Expr, Vec<Stmt>),
    For(String, Option<String>, Expr, Vec<Stmt>),
    Return(Option<Expr>),
    Break,
    Continue,
    Fn(String, Rc<FnDef>),
    Try(Vec<Stmt>, Option<String>, Vec<Stmt>),
    Throw(Expr),
    Block(Vec<Stmt>),
}

// ───────────────────────────── parser ─────────────────────────────

const KEYWORDS: [&str; 22] = [
    "let", "const", "var", "fn", "function", "if", "else", "for", "in", "while", "return", "break", "continue", "try", "catch", "throw", "true", "false", "null", "and", "or", "not",
];

struct Parser {
    toks: Vec<Token>,
    p: usize,
    nest: u32,
}

type PResult<T> = Result<T, ScriptError>;

impl Parser {
    fn peek(&self) -> &Token {
        &self.toks[self.p.min(self.toks.len() - 1)]
    }
    fn peek_at(&self, n: usize) -> &Token {
        &self.toks[(self.p + n).min(self.toks.len() - 1)]
    }
    fn next(&mut self) -> Token {
        let t = self.peek().clone();
        if self.p < self.toks.len() - 1 {
            self.p += 1;
        }
        t
    }
    fn is_p(&self, s: &str) -> bool {
        matches!(&self.peek().t, Tok::P(p) if *p == s)
    }
    fn is_kw(&self, s: &str) -> bool {
        matches!(&self.peek().t, Tok::Ident(i) if i == s)
    }
    fn eat_p(&mut self, s: &str) -> bool {
        if self.is_p(s) {
            self.next();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, s: &str) -> bool {
        if self.is_kw(s) {
            self.next();
            true
        } else {
            false
        }
    }
    fn describe(t: &Token) -> String {
        match &t.t {
            Tok::Eof => "the end of the script".into(),
            Tok::Num(n) => format!("number {}", fmt_num(*n)),
            Tok::Str(_) => "a string".into(),
            Tok::Ident(i) => format!("“{i}”"),
            Tok::P(p) => format!("“{p}”"),
        }
    }
    fn err<T>(&self, msg: &str) -> PResult<T> {
        let t = self.peek();
        Err(serr(format!("{msg}, found {}", Self::describe(t)), t.line, t.col))
    }
    fn expect_p(&mut self, s: &str) -> PResult<()> {
        if self.eat_p(s) {
            Ok(())
        } else {
            self.err(&format!("expected “{s}”"))
        }
    }
    fn ident(&mut self, what: &str) -> PResult<String> {
        if let Tok::Ident(i) = &self.peek().t {
            if !KEYWORDS.contains(&i.as_str()) {
                let i = i.clone();
                self.next();
                return Ok(i);
            }
        }
        self.err(&format!("expected {what}"))
    }
    fn enter(&mut self) -> PResult<()> {
        self.nest += 1;
        if self.nest > MAX_NEST {
            let t = self.peek();
            return Err(serr("code is nested too deeply", t.line, t.col));
        }
        Ok(())
    }

    fn program(&mut self) -> PResult<Vec<Stmt>> {
        let mut v = vec![];
        while self.peek().t != Tok::Eof {
            v.push(self.stmt()?);
        }
        Ok(v)
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_p("{")?;
        self.enter()?;
        let mut v = vec![];
        while !self.is_p("}") {
            if self.peek().t == Tok::Eof {
                return self.err("expected “}” to close the block");
            }
            v.push(self.stmt()?);
        }
        self.next();
        self.nest -= 1;
        Ok(v)
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let r = self.stmt_inner();
        self.nest -= 1;
        r
    }

    fn stmt_inner(&mut self) -> PResult<Stmt> {
        let t = self.peek().clone();
        let (line, col) = (t.line, t.col);
        let mk = |s: S| Stmt { s, line, col };
        if self.eat_p(";") {
            return Ok(mk(S::Block(vec![])));
        }
        if self.is_p("{") {
            return Ok(mk(S::Block(self.block()?)));
        }
        if let Tok::Ident(kw) = &t.t {
            match kw.as_str() {
                "let" | "const" | "var" => {
                    self.next();
                    let name = self.ident("a variable name")?;
                    let init = if self.eat_p("=") { Some(self.expr()?) } else { None };
                    self.eat_p(";");
                    return Ok(mk(S::Let(name, init, kw == "const")));
                }
                "fn" | "function" if matches!(self.peek_at(1).t, Tok::Ident(_)) => {
                    self.next();
                    let name = self.ident("a function name")?;
                    let def = self.fn_rest(name.clone())?;
                    return Ok(mk(S::Fn(name, def)));
                }
                "if" => {
                    self.next();
                    let mut arms = vec![];
                    let c = self.expr()?;
                    let b = self.block()?;
                    arms.push((c, b));
                    let mut els = None;
                    while self.eat_kw("else") {
                        if self.eat_kw("if") {
                            let c = self.expr()?;
                            let b = self.block()?;
                            arms.push((c, b));
                        } else {
                            els = Some(self.block()?);
                            break;
                        }
                    }
                    return Ok(mk(S::If(arms, els)));
                }
                "while" => {
                    self.next();
                    let c = self.expr()?;
                    let b = self.block()?;
                    return Ok(mk(S::While(c, b)));
                }
                "for" => {
                    self.next();
                    let paren = self.eat_p("(");
                    let a = self.ident("a loop variable")?;
                    let b = if self.eat_p(",") { Some(self.ident("a loop variable")?) } else { None };
                    if !self.eat_kw("in") {
                        return self.err("expected “in”");
                    }
                    let it = self.expr()?;
                    if paren {
                        self.expect_p(")")?;
                    }
                    let body = self.block()?;
                    return Ok(mk(S::For(a, b, it, body)));
                }
                "return" => {
                    self.next();
                    let nt = self.peek();
                    let v = if nt.nl || nt.t == Tok::Eof || matches!(nt.t, Tok::P("}") | Tok::P(";")) { None } else { Some(self.expr()?) };
                    self.eat_p(";");
                    return Ok(mk(S::Return(v)));
                }
                "break" => {
                    self.next();
                    self.eat_p(";");
                    return Ok(mk(S::Break));
                }
                "continue" => {
                    self.next();
                    self.eat_p(";");
                    return Ok(mk(S::Continue));
                }
                "throw" => {
                    self.next();
                    let v = self.expr()?;
                    self.eat_p(";");
                    return Ok(mk(S::Throw(v)));
                }
                "try" => {
                    self.next();
                    let body = self.block()?;
                    let mut var = None;
                    if !self.eat_kw("catch") {
                        return self.err("expected “catch”");
                    }
                    let paren = self.eat_p("(");
                    if matches!(&self.peek().t, Tok::Ident(_)) {
                        var = Some(self.ident("a name for the error")?);
                    }
                    if paren {
                        self.expect_p(")")?;
                    }
                    let handler = self.block()?;
                    return Ok(mk(S::Try(body, var, handler)));
                }
                "else" => return self.err("“else” without a matching “if”"),
                _ => {}
            }
        }
        let e = self.expr()?;
        self.eat_p(";");
        Ok(mk(S::Expr(e)))
    }

    fn fn_rest(&mut self, name: String) -> PResult<Rc<FnDef>> {
        self.expect_p("(")?;
        let mut params = vec![];
        while !self.is_p(")") {
            params.push(self.ident("a parameter name")?);
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(")")?;
        let body = self.block()?;
        Ok(Rc::new(FnDef { name, params, body }))
    }

    fn expr(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.assign();
        self.nest -= 1;
        r
    }

    fn assign(&mut self) -> PResult<Expr> {
        let lhs = self.ternary()?;
        let t = self.peek().clone();
        if let Tok::P(p) = t.t {
            let op = match p {
                "=" => Some(None),
                "+=" => Some(Some(Bop::Add)),
                "-=" => Some(Some(Bop::Sub)),
                "*=" => Some(Some(Bop::Mul)),
                "/=" => Some(Some(Bop::Div)),
                "%=" => Some(Some(Bop::Mod)),
                _ => None,
            };
            if let Some(op) = op {
                if !matches!(lhs.e, E::Ident(_) | E::Member(..) | E::Index(..)) {
                    return Err(serr("cannot assign to this expression", t.line, t.col));
                }
                self.next();
                let rhs = self.assign()?;
                return Ok(Expr { line: lhs.line, col: lhs.col, e: E::Assign(Box::new(lhs), op, Box::new(rhs)) });
            }
        }
        Ok(lhs)
    }

    fn ternary(&mut self) -> PResult<Expr> {
        let c = self.coalesce()?;
        if self.is_p("?") {
            self.next();
            let a = self.assign()?;
            self.expect_p(":")?;
            let b = self.assign()?;
            return Ok(Expr { line: c.line, col: c.col, e: E::Cond(Box::new(c), Box::new(a), Box::new(b)) });
        }
        Ok(c)
    }

    fn coalesce(&mut self) -> PResult<Expr> {
        let mut l = self.or()?;
        while self.is_p("??") {
            self.next();
            let r = self.or()?;
            l = Expr { line: l.line, col: l.col, e: E::Coalesce(Box::new(l), Box::new(r)) };
        }
        Ok(l)
    }

    fn or(&mut self) -> PResult<Expr> {
        let mut l = self.and()?;
        while self.is_p("||") || self.is_kw("or") {
            self.next();
            let r = self.and()?;
            l = Expr { line: l.line, col: l.col, e: E::Or(Box::new(l), Box::new(r)) };
        }
        Ok(l)
    }

    fn and(&mut self) -> PResult<Expr> {
        let mut l = self.equality()?;
        while self.is_p("&&") || self.is_kw("and") {
            self.next();
            let r = self.equality()?;
            l = Expr { line: l.line, col: l.col, e: E::And(Box::new(l), Box::new(r)) };
        }
        Ok(l)
    }

    fn bin_level(&mut self, ops: &[(&str, Bop)], next: fn(&mut Parser) -> PResult<Expr>) -> PResult<Expr> {
        let mut l = next(self)?;
        loop {
            let t = self.peek().clone();
            let Tok::P(p) = t.t else { break };
            // a "-" / "+" at the start of a line begins a new statement
            if t.nl && (p == "-" || p == "+") {
                break;
            }
            let Some((_, op)) = ops.iter().find(|(s, _)| *s == p) else { break };
            self.next();
            let r = next(self)?;
            l = Expr { line: t.line, col: t.col, e: E::Bin(*op, Box::new(l), Box::new(r)) };
        }
        Ok(l)
    }

    fn equality(&mut self) -> PResult<Expr> {
        self.bin_level(&[("==", Bop::Eq), ("!=", Bop::Ne)], Parser::comparison)
    }
    fn comparison(&mut self) -> PResult<Expr> {
        self.bin_level(&[("<", Bop::Lt), ("<=", Bop::Le), (">", Bop::Gt), (">=", Bop::Ge)], Parser::range)
    }
    fn range(&mut self) -> PResult<Expr> {
        let l = self.additive()?;
        if self.is_p("..") {
            let t = self.next();
            let r = self.additive()?;
            return Ok(Expr { line: t.line, col: t.col, e: E::Range(Box::new(l), Box::new(r)) });
        }
        Ok(l)
    }
    fn additive(&mut self) -> PResult<Expr> {
        self.bin_level(&[("+", Bop::Add), ("-", Bop::Sub)], Parser::multiplicative)
    }
    fn multiplicative(&mut self) -> PResult<Expr> {
        self.bin_level(&[("*", Bop::Mul), ("/", Bop::Div), ("%", Bop::Mod)], Parser::power)
    }
    fn power(&mut self) -> PResult<Expr> {
        let l = self.unary()?;
        if self.is_p("**") {
            let t = self.next();
            let r = self.power()?;
            return Ok(Expr { line: t.line, col: t.col, e: E::Bin(Bop::Pow, Box::new(l), Box::new(r)) });
        }
        Ok(l)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let t = self.peek().clone();
        if self.is_p("-") {
            self.next();
            self.enter()?;
            let e = self.unary()?;
            self.nest -= 1;
            return Ok(Expr { line: t.line, col: t.col, e: E::Neg(Box::new(e)) });
        }
        if self.is_p("!") || self.is_kw("not") {
            self.next();
            self.enter()?;
            let e = self.unary()?;
            self.nest -= 1;
            return Ok(Expr { line: t.line, col: t.col, e: E::Not(Box::new(e)) });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            let t = self.peek().clone();
            match &t.t {
                Tok::P("(") if !t.nl => {
                    self.next();
                    let args = self.args(")")?;
                    e = Expr { line: t.line, col: t.col, e: E::Call(Box::new(e), args) };
                }
                Tok::P("[") if !t.nl => {
                    self.next();
                    let i = self.expr()?;
                    self.expect_p("]")?;
                    e = Expr { line: t.line, col: t.col, e: E::Index(Box::new(e), Box::new(i)) };
                }
                Tok::P(".") | Tok::P("?.") => {
                    let opt = t.t == Tok::P("?.");
                    self.next();
                    let name = match &self.peek().t {
                        Tok::Ident(n) => n.clone(),
                        _ => return self.err("expected a property name after “.”"),
                    };
                    self.next();
                    e = Expr { line: t.line, col: t.col, e: E::Member(Box::new(e), name, opt) };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn args(&mut self, close: &str) -> PResult<Vec<Expr>> {
        let mut v = vec![];
        while !self.is_p(close) {
            v.push(self.expr()?);
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(close)?;
        Ok(v)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let t = self.next();
        let (line, col) = (t.line, t.col);
        let mk = |e: E| Expr { e, line, col };
        match t.t {
            Tok::Num(n) => Ok(mk(E::Num(n))),
            Tok::Str(parts) => {
                let mut out = vec![];
                for p in parts {
                    match p {
                        SPart::Lit(s) => out.push(StrPart::Lit(s)),
                        SPart::Code(code, l, c) => {
                            let toks = lex(&code, l, c)?;
                            let mut sub = Parser { toks, p: 0, nest: self.nest };
                            let e = sub.expr()?;
                            if sub.peek().t != Tok::Eof {
                                return sub.err("unexpected input inside {…}");
                            }
                            out.push(StrPart::Code(e));
                        }
                    }
                }
                Ok(mk(E::Str(out)))
            }
            Tok::P("(") => {
                let e = self.expr()?;
                self.expect_p(")")?;
                Ok(e)
            }
            Tok::P("[") => {
                let items = self.args("]")?;
                Ok(mk(E::List(items)))
            }
            Tok::P("{") => {
                let mut items = vec![];
                while !self.is_p("}") {
                    let kt = self.next();
                    let key = match kt.t {
                        Tok::Ident(i) => i,
                        Tok::Str(p) if p.len() == 1 && matches!(p[0], SPart::Lit(_)) => match &p[0] {
                            SPart::Lit(s) => s.clone(),
                            _ => String::new(),
                        },
                        Tok::Num(n) => fmt_num(n),
                        _ => return Err(serr("expected a key name in the map", kt.line, kt.col)),
                    };
                    let val = if self.eat_p(":") {
                        self.expr()?
                    } else {
                        // shorthand {name} == {name: name}
                        Expr { e: E::Ident(key.clone()), line: kt.line, col: kt.col }
                    };
                    items.push((key, val));
                    if !self.eat_p(",") {
                        break;
                    }
                }
                self.expect_p("}")?;
                Ok(mk(E::Map(items)))
            }
            Tok::P("|") | Tok::P("||") => {
                let mut params = vec![];
                if t.t == Tok::P("|") {
                    while !self.is_p("|") {
                        params.push(self.ident("a parameter name")?);
                        if !self.eat_p(",") {
                            break;
                        }
                    }
                    self.expect_p("|")?;
                }
                let body = if self.is_p("{") {
                    self.block()?
                } else {
                    let e = self.expr()?;
                    vec![Stmt { line: e.line, col: e.col, s: S::Return(Some(e)) }]
                };
                Ok(mk(E::Lambda(Rc::new(FnDef { name: "lambda".into(), params, body }))))
            }
            Tok::Ident(i) => match i.as_str() {
                "true" => Ok(mk(E::Bool(true))),
                "false" => Ok(mk(E::Bool(false))),
                "null" => Ok(mk(E::Null)),
                "fn" | "function" => {
                    let def = self.fn_rest("anonymous".into())?;
                    Ok(mk(E::Lambda(def)))
                }
                k if KEYWORDS.contains(&k) => Err(serr(format!("unexpected keyword “{k}”"), line, col)),
                _ => Ok(mk(E::Ident(i))),
            },
            _ => {
                self.p = self.p.saturating_sub(1);
                self.err("expected a value")
            }
        }
    }
}

fn parse(src: &str) -> Result<Vec<Stmt>, ScriptError> {
    if src.len() > MAX_SRC {
        return Err(serr("script is too long (200 000 characters max)", 1, 1));
    }
    let toks = lex(src, 1, 1)?;
    Parser { toks, p: 0, nest: 0 }.program()
}

/// Syntax check only (never runs anything).
pub fn check(src: &str) -> Result<(), ScriptError> {
    parse(src).map(|_| ())
}

// ───────────────────────────── interpreter ─────────────────────────────

pub type Env = Rc<RefCell<Scope>>;

pub struct Scope {
    vars: HashMap<String, (Value, bool)>,
    parent: Option<Env>,
}

fn new_scope(parent: Option<Env>) -> Env {
    Rc::new(RefCell::new(Scope { vars: HashMap::new(), parent }))
}

enum Ex {
    Throw(Value, u32, u32),
    Halt(ScriptError),
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

type R<T> = Result<T, Ex>;

pub struct Outcome {
    pub output: Vec<String>,
    pub error: Option<ScriptError>,
    pub steps: u64,
}

/// Parses and runs `src`. Never panics, never runs longer than `MAX_STEPS` instructions.
pub fn run(src: &str, host: &mut dyn Host) -> Outcome {
    let prog = match parse(src) {
        Ok(p) => p,
        Err(e) => return Outcome { output: vec![], error: Some(e), steps: 0 },
    };
    let mut it = Interp { host, steps: 0, depth: 0, out: vec![], truncated: false };
    let env = new_scope(None);
    let res = it.exec_stmts(&prog, &env);
    let error = match res {
        Ok(_) => None,
        Err(Ex::Halt(e)) => Some(e),
        Err(Ex::Throw(v, l, c)) => {
            let m = it.show(&v);
            Some(serr(m, l, c))
        }
    };
    Outcome { output: it.out, error, steps: it.steps }
}

struct Interp<'h> {
    host: &'h mut dyn Host,
    steps: u64,
    depth: u32,
    out: Vec<String>,
    truncated: bool,
}

const BUILTINS: [&str; 30] = [
    "print", "log", "len", "str", "num", "int", "bool", "type", "range", "keys", "values", "abs", "min", "max", "floor", "ceil", "round", "sqrt", "pow", "json", "assert", "error", "sum", "sorted", "reversed",
    "contains", "entries", "clamp", "sign", "is_null",
];

impl<'h> Interp<'h> {
    fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(Ex::Halt(serr(format!("the script used more than {MAX_STEPS} steps — is there an endless loop?"), 0, 0)));
        }
        Ok(())
    }

    fn throw<T>(&self, msg: impl Into<String>, line: u32, col: u32) -> R<T> {
        Err(Ex::Throw(Value::str(msg.into()), line, col))
    }

    fn show(&mut self, v: &Value) -> String {
        self.show_depth(v, 0)
    }

    fn show_depth(&mut self, v: &Value, d: u32) -> String {
        match v {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => fmt_num(*n),
            Value::Str(s) => s.to_string(),
            Value::List(l) => {
                if d > 4 {
                    return "[…]".into();
                }
                let items: Vec<Value> = l.borrow().iter().take(200).cloned().collect();
                let n = l.borrow().len();
                let mut parts: Vec<String> = items.iter().map(|x| self.quoted(x, d + 1)).collect();
                if n > 200 {
                    parts.push(format!("… {} more", n - 200));
                }
                format!("[{}]", parts.join(", "))
            }
            Value::Map(m) => {
                if d > 4 {
                    return "{…}".into();
                }
                let items: Vec<(String, Value)> = m.borrow().iter().take(100).map(|(k, v)| (k.clone(), v.clone())).collect();
                let parts: Vec<String> = items.iter().map(|(k, x)| format!("{k}: {}", self.quoted(x, d + 1))).collect();
                format!("{{{}}}", parts.join(", "))
            }
            Value::Func(c) => format!("<fn {}>", c.def.name),
            Value::Native(n) => format!("<builtin {n}>"),
            Value::Host(h) => self.host.display(*h),
        }
    }

    fn quoted(&mut self, v: &Value, d: u32) -> String {
        match v {
            Value::Str(s) => format!("\"{s}\""),
            _ => self.show_depth(v, d),
        }
    }

    fn emit(&mut self, line: String) {
        if self.out.len() < MAX_OUT_LINES {
            let line = if line.chars().count() > 2000 { line.chars().take(2000).collect::<String>() + "…" } else { line };
            self.out.push(line);
        } else if !self.truncated {
            self.truncated = true;
            self.out.push("… output truncated".into());
        }
    }

    // ── statements ──

    fn exec_stmts(&mut self, stmts: &[Stmt], env: &Env) -> R<Flow> {
        for s in stmts {
            if let S::Fn(name, def) = &s.s {
                let c = Value::Func(Rc::new(Closure { def: def.clone(), env: env.clone() }));
                env.borrow_mut().vars.insert(name.clone(), (c, false));
            }
        }
        for s in stmts {
            match self.exec(s, env)? {
                Flow::Next => {}
                f => return Ok(f),
            }
        }
        Ok(Flow::Next)
    }

    fn exec_block(&mut self, stmts: &[Stmt], env: &Env) -> R<Flow> {
        let scope = new_scope(Some(env.clone()));
        self.exec_stmts(stmts, &scope)
    }

    fn exec(&mut self, s: &Stmt, env: &Env) -> R<Flow> {
        self.tick()?;
        match &s.s {
            S::Expr(e) => {
                self.eval(e, env)?;
            }
            S::Let(name, init, konst) => {
                let v = match init {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Null,
                };
                env.borrow_mut().vars.insert(name.clone(), (v, *konst));
            }
            S::If(arms, els) => {
                for (c, b) in arms {
                    if self.eval(c, env)?.truthy() {
                        return self.exec_block(b, env);
                    }
                }
                if let Some(b) = els {
                    return self.exec_block(b, env);
                }
            }
            S::While(c, b) => {
                while self.eval(c, env)?.truthy() {
                    match self.exec_block(b, env)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    self.tick()?;
                }
            }
            S::For(a, b, it, body) => {
                let iter = self.eval(it, env)?;
                let items: Vec<(Value, Value)> = match &iter {
                    Value::List(l) => l.borrow().iter().enumerate().map(|(i, v)| (Value::Num(i as f64), v.clone())).collect(),
                    Value::Map(m) => m.borrow().iter().map(|(k, v)| (Value::str(k), v.clone())).collect(),
                    Value::Str(s) => s.chars().enumerate().map(|(i, c)| (Value::Num(i as f64), Value::str(c.to_string()))).collect(),
                    Value::Null => vec![],
                    other => return self.throw(format!("cannot loop over a {}", other.type_name()), s.line, s.col),
                };
                let is_map = matches!(iter, Value::Map(_));
                for (k, v) in items {
                    let scope = new_scope(Some(env.clone()));
                    {
                        let mut sc = scope.borrow_mut();
                        match b {
                            // for index, value in …   /   for key, value in map
                            Some(second) => {
                                sc.vars.insert(a.clone(), (k, false));
                                sc.vars.insert(second.clone(), (v, false));
                            }
                            None => {
                                sc.vars.insert(a.clone(), (if is_map { k } else { v }, false));
                            }
                        }
                    }
                    match self.exec_stmts(body, &scope)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    self.tick()?;
                }
            }
            S::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Null,
                };
                return Ok(Flow::Return(v));
            }
            S::Break => return Ok(Flow::Break),
            S::Continue => return Ok(Flow::Continue),
            S::Fn(..) => {}
            S::Throw(e) => {
                let v = self.eval(e, env)?;
                return Err(Ex::Throw(v, s.line, s.col));
            }
            S::Try(body, var, handler) => match self.exec_block(body, env) {
                Err(Ex::Throw(v, ..)) => {
                    let scope = new_scope(Some(env.clone()));
                    if let Some(n) = var {
                        scope.borrow_mut().vars.insert(n.clone(), (v, false));
                    }
                    return self.exec_stmts(handler, &scope);
                }
                other => return other,
            },
            S::Block(b) => return self.exec_block(b, env),
        }
        Ok(Flow::Next)
    }

    // ── variables ──

    fn lookup(&self, env: &Env, name: &str) -> Option<Value> {
        let mut cur = Some(env.clone());
        while let Some(e) = cur {
            if let Some((v, _)) = e.borrow().vars.get(name) {
                return Some(v.clone());
            }
            cur = e.borrow().parent.clone();
        }
        None
    }

    fn assign_var(&mut self, env: &Env, name: &str, v: Value, line: u32, col: u32) -> R<()> {
        let mut cur = Some(env.clone());
        while let Some(e) = cur {
            if let Some(slot) = e.borrow_mut().vars.get_mut(name) {
                if slot.1 {
                    return self.throw(format!("“{name}” is a constant and cannot be changed"), line, col);
                }
                slot.0 = v;
                return Ok(());
            }
            cur = e.borrow().parent.clone();
        }
        self.throw(format!("variable “{name}” is not defined — declare it with let"), line, col)
    }

    // ── expressions ──

    fn eval(&mut self, e: &Expr, env: &Env) -> R<Value> {
        self.tick()?;
        let (line, col) = (e.line, e.col);
        Ok(match &e.e {
            E::Null => Value::Null,
            E::Bool(b) => Value::Bool(*b),
            E::Num(n) => Value::Num(*n),
            E::Str(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        StrPart::Lit(l) => s.push_str(l),
                        StrPart::Code(c) => {
                            let v = self.eval(c, env)?;
                            let t = self.show(&v);
                            s.push_str(&t);
                        }
                    }
                    if s.len() > MAX_STR {
                        return self.throw("string too long", line, col);
                    }
                }
                Value::str(s)
            }
            E::List(items) => {
                let mut v = Vec::with_capacity(items.len());
                for i in items {
                    v.push(self.eval(i, env)?);
                }
                Value::list(v)
            }
            E::Map(items) => {
                let mut m = BTreeMap::new();
                for (k, i) in items {
                    let v = self.eval(i, env)?;
                    m.insert(k.clone(), v);
                }
                Value::map(m)
            }
            E::Ident(name) => match self.lookup(env, name) {
                Some(v) => v,
                None => {
                    if let Some(v) = self.host.global(name) {
                        v
                    } else if BUILTINS.contains(&name.as_str()) || self.host.has_fn(name) {
                        Value::Native(Rc::from(name.as_str()))
                    } else {
                        return self.throw(format!("“{name}” is not defined"), line, col);
                    }
                }
            },
            E::Neg(x) => match self.eval(x, env)? {
                Value::Num(n) => Value::Num(-n),
                o => return self.throw(format!("cannot negate a {}", o.type_name()), line, col),
            },
            E::Not(x) => Value::Bool(!self.eval(x, env)?.truthy()),
            E::And(a, b) => {
                let l = self.eval(a, env)?;
                if l.truthy() { self.eval(b, env)? } else { l }
            }
            E::Or(a, b) => {
                let l = self.eval(a, env)?;
                if l.truthy() { l } else { self.eval(b, env)? }
            }
            E::Coalesce(a, b) => {
                let l = self.eval(a, env)?;
                if matches!(l, Value::Null) { self.eval(b, env)? } else { l }
            }
            E::Cond(c, a, b) => {
                if self.eval(c, env)?.truthy() { self.eval(a, env)? } else { self.eval(b, env)? }
            }
            E::Bin(op, a, b) => {
                let l = self.eval(a, env)?;
                let r = self.eval(b, env)?;
                self.binop(*op, l, r, line, col)?
            }
            E::Range(a, b) => {
                let (l, r) = (self.eval(a, env)?, self.eval(b, env)?);
                match (l, r) {
                    (Value::Num(x), Value::Num(y)) => {
                        let n = (y - x).max(0.0);
                        if n as usize > MAX_LIST {
                            return self.throw("range is too large", line, col);
                        }
                        Value::list((0..n.ceil() as i64).map(|i| Value::Num(x + i as f64)).collect())
                    }
                    _ => return self.throw("a range needs two numbers (a..b)", line, col),
                }
            }
            E::Lambda(def) => Value::Func(Rc::new(Closure { def: def.clone(), env: env.clone() })),
            E::Member(obj, name, opt) => {
                let o = self.eval(obj, env)?;
                if *opt && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                self.get_member(&o, name, line, col)?
            }
            E::Index(obj, idx) => {
                let o = self.eval(obj, env)?;
                let i = self.eval(idx, env)?;
                self.get_index(&o, &i, line, col)?
            }
            E::Assign(target, op, rhs) => return self.assign(target, *op, rhs, env, line, col),
            E::Call(callee, args) => return self.call_expr(callee, args, env, line, col),
        })
    }

    fn binop(&mut self, op: Bop, l: Value, r: Value, line: u32, col: u32) -> R<Value> {
        use Value::*;
        Ok(match op {
            Bop::Eq => Bool(values_equal(&l, &r)),
            Bop::Ne => Bool(!values_equal(&l, &r)),
            Bop::Lt | Bop::Le | Bop::Gt | Bop::Ge => {
                let ord = match (&l, &r) {
                    (Num(a), Num(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Less),
                    (Str(a), Str(b)) => a.cmp(b),
                    (Null, _) | (_, Null) => return Ok(Bool(false)),
                    _ => return self.throw(format!("cannot compare a {} with a {}", l.type_name(), r.type_name()), line, col),
                };
                if let (Num(a), Num(b)) = (&l, &r) {
                    if a.is_nan() || b.is_nan() {
                        return Ok(Bool(false));
                    }
                }
                Bool(match op {
                    Bop::Lt => ord.is_lt(),
                    Bop::Le => ord.is_le(),
                    Bop::Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                })
            }
            Bop::Add => match (&l, &r) {
                (Num(a), Num(b)) => Num(a + b),
                (Str(_), _) | (_, Str(_)) => {
                    let mut s = self.show(&l);
                    s.push_str(&self.show(&r));
                    if s.len() > MAX_STR {
                        return self.throw("string too long", line, col);
                    }
                    Value::str(s)
                }
                (List(a), List(b)) => {
                    let mut v = a.borrow().clone();
                    v.extend(b.borrow().iter().cloned());
                    if v.len() > MAX_LIST {
                        return self.throw("list too long", line, col);
                    }
                    Value::list(v)
                }
                (Map(a), Map(b)) => {
                    let mut m = a.borrow().clone();
                    for (k, v) in b.borrow().iter() {
                        m.insert(k.clone(), v.clone());
                    }
                    Value::map(m)
                }
                _ => return self.throw(format!("cannot add a {} and a {}", l.type_name(), r.type_name()), line, col),
            },
            Bop::Mul => match (&l, &r) {
                (Num(a), Num(b)) => Num(a * b),
                (Str(s), Num(n)) | (Num(n), Str(s)) => {
                    let n = n.max(0.0) as usize;
                    if s.len().saturating_mul(n) > MAX_STR {
                        return self.throw("string too long", line, col);
                    }
                    Value::str(s.repeat(n))
                }
                _ => return self.throw(format!("cannot multiply a {} and a {}", l.type_name(), r.type_name()), line, col),
            },
            Bop::Sub | Bop::Div | Bop::Mod | Bop::Pow => match (&l, &r) {
                (Num(a), Num(b)) => match op {
                    Bop::Sub => Num(a - b),
                    Bop::Div => {
                        if *b == 0.0 {
                            return self.throw("division by zero", line, col);
                        }
                        Num(a / b)
                    }
                    Bop::Mod => {
                        if *b == 0.0 {
                            return self.throw("modulo by zero", line, col);
                        }
                        Num(a.rem_euclid(*b))
                    }
                    _ => Num(a.powf(*b)),
                },
                _ => return self.throw(format!("arithmetic needs numbers, not a {} and a {}", l.type_name(), r.type_name()), line, col),
            },
        })
    }

    fn get_member(&mut self, o: &Value, name: &str, line: u32, col: u32) -> R<Value> {
        match o {
            Value::Map(m) => Ok(m.borrow().get(name).cloned().unwrap_or(Value::Null)),
            Value::Host(h) => match self.host.get(*h, name) {
                Ok(v) => Ok(v),
                Err(e) => self.throw(e, line, col),
            },
            Value::List(l) => match name {
                "length" | "len" | "count" => Ok(Value::Num(l.borrow().len() as f64)),
                "first" => Ok(l.borrow().first().cloned().unwrap_or(Value::Null)),
                "last" => Ok(l.borrow().last().cloned().unwrap_or(Value::Null)),
                _ => self.throw(format!("a list has no property “{name}”"), line, col),
            },
            Value::Str(s) => match name {
                "length" | "len" => Ok(Value::Num(s.chars().count() as f64)),
                _ => self.throw(format!("a string has no property “{name}”"), line, col),
            },
            Value::Null => self.throw(format!("cannot read “{name}” of null (use ?. if it may be empty)"), line, col),
            other => self.throw(format!("a {} has no property “{name}”", other.type_name()), line, col),
        }
    }

    fn get_index(&mut self, o: &Value, i: &Value, line: u32, col: u32) -> R<Value> {
        match (o, i) {
            (Value::List(l), Value::Num(n)) => {
                let l = l.borrow();
                let idx = if *n < 0.0 { l.len() as f64 + n } else { *n };
                Ok(if idx >= 0.0 { l.get(idx as usize).cloned().unwrap_or(Value::Null) } else { Value::Null })
            }
            (Value::Str(s), Value::Num(n)) => {
                let cs: Vec<char> = s.chars().collect();
                let idx = if *n < 0.0 { cs.len() as f64 + n } else { *n };
                Ok(if idx >= 0.0 { cs.get(idx as usize).map(|c| Value::str(c.to_string())).unwrap_or(Value::Null) } else { Value::Null })
            }
            (Value::Map(m), Value::Str(k)) => Ok(m.borrow().get(&**k).cloned().unwrap_or(Value::Null)),
            (Value::Host(h), Value::Str(k)) => match self.host.get(*h, k) {
                Ok(v) => Ok(v),
                Err(e) => self.throw(e, line, col),
            },
            (Value::Null, _) => self.throw("cannot index null", line, col),
            _ => self.throw(format!("cannot index a {} with a {}", o.type_name(), i.type_name()), line, col),
        }
    }

    fn set_index(&mut self, o: &Value, i: &Value, v: Value, line: u32, col: u32) -> R<()> {
        match (o, i) {
            (Value::List(l), Value::Num(n)) => {
                let mut l = l.borrow_mut();
                let idx = if *n < 0.0 { l.len() as f64 + n } else { *n };
                if idx < 0.0 || idx as usize > l.len() || idx as usize >= MAX_LIST {
                    drop(l);
                    return self.throw("list index out of range", line, col);
                }
                if idx as usize == l.len() {
                    l.push(v);
                } else {
                    l[idx as usize] = v;
                }
                Ok(())
            }
            (Value::Map(m), Value::Str(k)) => {
                m.borrow_mut().insert(k.to_string(), v);
                Ok(())
            }
            (Value::Host(h), Value::Str(k)) => match self.host.set(*h, k, v) {
                Ok(()) => Ok(()),
                Err(e) => self.throw(e, line, col),
            },
            _ => self.throw(format!("cannot assign into a {} with a {}", o.type_name(), i.type_name()), line, col),
        }
    }

    fn assign(&mut self, target: &Expr, op: Option<Bop>, rhs: &Expr, env: &Env, line: u32, col: u32) -> R<Value> {
        match &target.e {
            E::Ident(name) => {
                let mut v = self.eval(rhs, env)?;
                if let Some(op) = op {
                    let cur = match self.lookup(env, name) {
                        Some(c) => c,
                        None => return self.throw(format!("variable “{name}” is not defined"), line, col),
                    };
                    v = self.binop(op, cur, v, line, col)?;
                }
                self.assign_var(env, name, v.clone(), line, col)?;
                Ok(v)
            }
            E::Member(obj, name, _) => {
                let o = self.eval(obj, env)?;
                let mut v = self.eval(rhs, env)?;
                let key = Value::str(name);
                if let Some(op) = op {
                    let cur = self.get_member(&o, name, line, col)?;
                    v = self.binop(op, cur, v, line, col)?;
                }
                self.set_index(&o, &key, v.clone(), line, col)?;
                Ok(v)
            }
            E::Index(obj, idx) => {
                let o = self.eval(obj, env)?;
                let i = self.eval(idx, env)?;
                let mut v = self.eval(rhs, env)?;
                if let Some(op) = op {
                    let cur = self.get_index(&o, &i, line, col)?;
                    v = self.binop(op, cur, v, line, col)?;
                }
                self.set_index(&o, &i, v.clone(), line, col)?;
                Ok(v)
            }
            _ => self.throw("cannot assign to this expression", line, col),
        }
    }

    fn call_expr(&mut self, callee: &Expr, args: &[Expr], env: &Env, line: u32, col: u32) -> R<Value> {
        // method call: obj.name(args)
        if let E::Member(obj, name, opt) = &callee.e {
            let recv = self.eval(obj, env)?;
            if *opt && matches!(recv, Value::Null) {
                return Ok(Value::Null);
            }
            let mut av = Vec::with_capacity(args.len());
            for a in args {
                av.push(self.eval(a, env)?);
            }
            return self.call_method(recv, name, av, line, col);
        }
        let f = self.eval(callee, env)?;
        let mut av = Vec::with_capacity(args.len());
        for a in args {
            av.push(self.eval(a, env)?);
        }
        self.call_value(&f, av, line, col)
    }

    fn call_value(&mut self, f: &Value, args: Vec<Value>, line: u32, col: u32) -> R<Value> {
        match f {
            Value::Func(c) => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    self.depth -= 1;
                    return Err(Ex::Halt(serr("too many nested calls (infinite recursion?)", line, col)));
                }
                let scope = new_scope(Some(c.env.clone()));
                {
                    let mut sc = scope.borrow_mut();
                    for (i, p) in c.def.params.iter().enumerate() {
                        sc.vars.insert(p.clone(), (args.get(i).cloned().unwrap_or(Value::Null), false));
                    }
                }
                let r = self.exec_stmts(&c.def.body, &scope);
                self.depth -= 1;
                match r? {
                    Flow::Return(v) => Ok(v),
                    _ => Ok(Value::Null),
                }
            }
            Value::Native(name) => self.call_named(name, args, line, col),
            other => self.throw(format!("a {} is not callable", other.type_name()), line, col),
        }
    }

    fn call_named(&mut self, name: &str, args: Vec<Value>, line: u32, col: u32) -> R<Value> {
        if BUILTINS.contains(&name) {
            return self.builtin(name, args, line, col);
        }
        if self.host.has_fn(name) {
            return match self.host.call(name, &args) {
                Ok(v) => Ok(v),
                Err(e) => self.throw(e, line, col),
            };
        }
        self.throw(format!("“{name}” is not a function"), line, col)
    }

    fn arg_num(&self, args: &[Value], i: usize, fname: &str, line: u32, col: u32) -> R<f64> {
        match args.get(i) {
            Some(Value::Num(n)) => Ok(*n),
            Some(o) => self.throw(format!("{fname}() expects a number as argument {}, got a {}", i + 1, o.type_name()), line, col),
            None => self.throw(format!("{fname}() is missing argument {}", i + 1), line, col),
        }
    }

    fn builtin(&mut self, name: &str, args: Vec<Value>, line: u32, col: u32) -> R<Value> {
        let a0 = args.first().cloned().unwrap_or(Value::Null);
        Ok(match name {
            "print" | "log" => {
                let parts: Vec<String> = args.iter().map(|a| self.show(a)).collect();
                self.emit(parts.join(" "));
                Value::Null
            }
            "len" => match &a0 {
                Value::Str(s) => Value::Num(s.chars().count() as f64),
                Value::List(l) => Value::Num(l.borrow().len() as f64),
                Value::Map(m) => Value::Num(m.borrow().len() as f64),
                Value::Null => Value::Num(0.0),
                o => return self.throw(format!("len() does not work on a {}", o.type_name()), line, col),
            },
            "str" => {
                let s = self.show(&a0);
                Value::str(s)
            }
            "num" => match &a0 {
                Value::Num(n) => Value::Num(*n),
                Value::Bool(b) => Value::Num(*b as u8 as f64),
                Value::Str(s) => s.trim().replace(',', ".").parse::<f64>().map(Value::Num).unwrap_or(Value::Null),
                _ => Value::Null,
            },
            "int" => match &a0 {
                Value::Num(n) => Value::Num(n.trunc()),
                Value::Str(s) => s.trim().parse::<f64>().map(|n| Value::Num(n.trunc())).unwrap_or(Value::Null),
                _ => Value::Null,
            },
            "bool" => Value::Bool(a0.truthy()),
            "is_null" => Value::Bool(matches!(a0, Value::Null)),
            "type" => Value::str(a0.type_name()),
            "range" => {
                let (a, b, step) = match args.len() {
                    1 => (0.0, self.arg_num(&args, 0, "range", line, col)?, 1.0),
                    2 => (self.arg_num(&args, 0, "range", line, col)?, self.arg_num(&args, 1, "range", line, col)?, 1.0),
                    _ => (self.arg_num(&args, 0, "range", line, col)?, self.arg_num(&args, 1, "range", line, col)?, self.arg_num(&args, 2, "range", line, col)?),
                };
                if step == 0.0 {
                    return self.throw("range() step cannot be 0", line, col);
                }
                let n = ((b - a) / step).ceil().max(0.0);
                if n as usize > MAX_LIST {
                    return self.throw("range is too large", line, col);
                }
                Value::list((0..n as i64).map(|i| Value::Num(a + step * i as f64)).collect())
            }
            "keys" => match &a0 {
                Value::Map(m) => Value::list(m.borrow().keys().map(Value::str).collect()),
                _ => return self.throw("keys() needs a map", line, col),
            },
            "values" => match &a0 {
                Value::Map(m) => Value::list(m.borrow().values().cloned().collect()),
                _ => return self.throw("values() needs a map", line, col),
            },
            "entries" => match &a0 {
                Value::Map(m) => Value::list(m.borrow().iter().map(|(k, v)| Value::list(vec![Value::str(k), v.clone()])).collect()),
                _ => return self.throw("entries() needs a map", line, col),
            },
            "abs" => Value::Num(self.arg_num(&args, 0, name, line, col)?.abs()),
            "floor" => Value::Num(self.arg_num(&args, 0, name, line, col)?.floor()),
            "ceil" => Value::Num(self.arg_num(&args, 0, name, line, col)?.ceil()),
            "sqrt" => Value::Num(self.arg_num(&args, 0, name, line, col)?.sqrt()),
            "sign" => Value::Num(self.arg_num(&args, 0, name, line, col)?.signum()),
            "round" => {
                let n = self.arg_num(&args, 0, name, line, col)?;
                let d = args.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0).clamp(0.0, 10.0);
                let m = 10f64.powf(d);
                Value::Num((n * m).round() / m)
            }
            "pow" => Value::Num(self.arg_num(&args, 0, name, line, col)?.powf(self.arg_num(&args, 1, name, line, col)?)),
            "clamp" => {
                let (v, lo, hi) = (self.arg_num(&args, 0, name, line, col)?, self.arg_num(&args, 1, name, line, col)?, self.arg_num(&args, 2, name, line, col)?);
                Value::Num(v.max(lo).min(hi))
            }
            "min" | "max" => {
                let items: Vec<Value> = match (&a0, args.len()) {
                    (Value::List(l), 1) => l.borrow().clone(),
                    _ => args.clone(),
                };
                let mut best: Option<Value> = None;
                for v in items {
                    if matches!(v, Value::Null) {
                        continue;
                    }
                    best = Some(match best {
                        None => v,
                        Some(b) => {
                            let o = cmp_values(&v, &b);
                            if (name == "min" && o.is_lt()) || (name == "max" && o.is_gt()) { v } else { b }
                        }
                    });
                }
                best.unwrap_or(Value::Null)
            }
            "sum" => match &a0 {
                Value::List(l) => {
                    let mut t = 0.0;
                    for v in l.borrow().iter() {
                        if let Value::Num(n) = v {
                            t += n;
                        }
                    }
                    Value::Num(t)
                }
                _ => return self.throw("sum() needs a list", line, col),
            },
            "sorted" => match &a0 {
                Value::List(l) => {
                    let mut v = l.borrow().clone();
                    v.sort_by(cmp_values);
                    Value::list(v)
                }
                _ => return self.throw("sorted() needs a list", line, col),
            },
            "reversed" => match &a0 {
                Value::List(l) => Value::list(l.borrow().iter().rev().cloned().collect()),
                Value::Str(s) => Value::str(s.chars().rev().collect::<String>()),
                _ => return self.throw("reversed() needs a list or string", line, col),
            },
            "contains" => return self.call_method(a0, "contains", args.into_iter().skip(1).collect(), line, col),
            "json" => {
                let s = self.json(&a0, 0);
                Value::str(s)
            }
            "assert" => {
                if !a0.truthy() {
                    let m = args.get(1).map(|m| self.show(m)).unwrap_or_else(|| "assertion failed".into());
                    return self.throw(m, line, col);
                }
                Value::Null
            }
            "error" => {
                let m = self.show(&a0);
                return self.throw(m, line, col);
            }
            _ => return self.throw(format!("unknown builtin {name}"), line, col),
        })
    }

    fn json(&mut self, v: &Value, d: u32) -> String {
        fn esc(s: &str) -> String {
            let mut o = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    '\t' => o.push_str("\\t"),
                    '\r' => o.push_str("\\r"),
                    c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                    c => o.push(c),
                }
            }
            o.push('"');
            o
        }
        if d > 8 {
            return "null".into();
        }
        match v {
            Value::Null | Value::Func(_) | Value::Native(_) => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => {
                if n.is_finite() { fmt_num(*n) } else { "null".into() }
            }
            Value::Str(s) => esc(s),
            Value::List(l) => {
                let items: Vec<Value> = l.borrow().clone();
                format!("[{}]", items.iter().map(|x| self.json(x, d + 1)).collect::<Vec<_>>().join(","))
            }
            Value::Map(m) => {
                let items: Vec<(String, Value)> = m.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                format!("{{{}}}", items.iter().map(|(k, x)| format!("{}:{}", esc(k), self.json(x, d + 1))).collect::<Vec<_>>().join(","))
            }
            Value::Host(h) => {
                let s = self.host.display(*h);
                esc(&s)
            }
        }
    }

    fn call_fn_arg(&mut self, f: &Value, args: Vec<Value>, line: u32, col: u32) -> R<Value> {
        self.tick()?;
        self.call_value(f, args, line, col)
    }

    fn need_fn(&self, args: &[Value], i: usize, m: &str, line: u32, col: u32) -> R<Value> {
        match args.get(i) {
            Some(f @ (Value::Func(_) | Value::Native(_))) => Ok(f.clone()),
            _ => self.throw(format!(".{m}() needs a function, e.g. .{m}(|x| …)"), line, col),
        }
    }

    fn call_method(&mut self, recv: Value, name: &str, args: Vec<Value>, line: u32, col: u32) -> R<Value> {
        let a0 = args.first().cloned().unwrap_or(Value::Null);
        match &recv {
            Value::Host(h) => match self.host.method(*h, name, &args) {
                Ok(v) => Ok(v),
                Err(e) => self.throw(e, line, col),
            },
            Value::Map(m) => {
                let f = m.borrow().get(name).cloned();
                if let Some(f @ (Value::Func(_) | Value::Native(_))) = f {
                    return self.call_value(&f, args, line, col);
                }
                let m2 = m.clone();
                Ok(match name {
                    "keys" => Value::list(m2.borrow().keys().map(Value::str).collect()),
                    "values" => Value::list(m2.borrow().values().cloned().collect()),
                    "entries" => Value::list(m2.borrow().iter().map(|(k, v)| Value::list(vec![Value::str(k), v.clone()])).collect()),
                    "len" | "count" => Value::Num(m2.borrow().len() as f64),
                    "has" => Value::Bool(m2.borrow().contains_key(a0.as_str().unwrap_or(""))),
                    "get" => m2.borrow().get(a0.as_str().unwrap_or("")).cloned().unwrap_or_else(|| args.get(1).cloned().unwrap_or(Value::Null)),
                    "set" => {
                        if let Some(k) = a0.as_str() {
                            m2.borrow_mut().insert(k.to_string(), args.get(1).cloned().unwrap_or(Value::Null));
                        }
                        recv.clone()
                    }
                    "remove" => m2.borrow_mut().remove(a0.as_str().unwrap_or("")).unwrap_or(Value::Null),
                    "merge" => {
                        if let Value::Map(o) = &a0 {
                            let mut n = m2.borrow().clone();
                            for (k, v) in o.borrow().iter() {
                                n.insert(k.clone(), v.clone());
                            }
                            Value::map(n)
                        } else {
                            recv.clone()
                        }
                    }
                    _ => return self.throw(format!("a map has no method “{name}”"), line, col),
                })
            }
            Value::Num(n) => {
                let n = *n;
                Ok(match name {
                    "round" => {
                        let d = a0.as_f64().unwrap_or(0.0).clamp(0.0, 10.0);
                        let m = 10f64.powf(d);
                        Value::Num((n * m).round() / m)
                    }
                    "floor" => Value::Num(n.floor()),
                    "ceil" => Value::Num(n.ceil()),
                    "abs" => Value::Num(n.abs()),
                    "to_fixed" => Value::str(format!("{:.*}", a0.as_f64().unwrap_or(0.0).clamp(0.0, 12.0) as usize, n)),
                    "to_str" => Value::str(fmt_num(n)),
                    _ => return self.throw(format!("a number has no method “{name}”"), line, col),
                })
            }
            Value::Str(s) => {
                let s = s.clone();
                let sa = a0.as_str().unwrap_or("").to_owned();
                Ok(match name {
                    "len" => Value::Num(s.chars().count() as f64),
                    "upper" | "to_upper" => Value::str(s.to_uppercase()),
                    "lower" | "to_lower" => Value::str(s.to_lowercase()),
                    "trim" => Value::str(s.trim()),
                    "contains" | "includes" => Value::Bool(s.contains(&sa)),
                    "starts_with" => Value::Bool(s.starts_with(&sa)),
                    "ends_with" => Value::Bool(s.ends_with(&sa)),
                    "split" => {
                        if sa.is_empty() {
                            Value::list(s.chars().map(|c| Value::str(c.to_string())).collect())
                        } else {
                            Value::list(s.split(&sa).map(Value::str).collect())
                        }
                    }
                    "lines" => Value::list(s.lines().map(Value::str).collect()),
                    "chars" => Value::list(s.chars().map(|c| Value::str(c.to_string())).collect()),
                    "replace" => {
                        let to = args.get(1).map(|v| self.show(v)).unwrap_or_default();
                        if sa.is_empty() { Value::Str(s) } else { Value::str(s.replace(&sa, &to)) }
                    }
                    "index_of" => Value::Num(s.find(&sa).map(|b| s[..b].chars().count() as f64).unwrap_or(-1.0)),
                    "slice" => {
                        let cs: Vec<char> = s.chars().collect();
                        let n = cs.len() as f64;
                        let fix = |v: f64| (if v < 0.0 { (n + v).max(0.0) } else { v.min(n) }) as usize;
                        let a = fix(a0.as_f64().unwrap_or(0.0));
                        let b = fix(args.get(1).and_then(|v| v.as_f64()).unwrap_or(n));
                        Value::str(if a < b { cs[a..b].iter().collect::<String>() } else { String::new() })
                    }
                    "repeat" => {
                        let n = a0.as_f64().unwrap_or(0.0).max(0.0) as usize;
                        if s.len().saturating_mul(n) > MAX_STR {
                            return self.throw("string too long", line, col);
                        }
                        Value::str(s.repeat(n))
                    }
                    "pad_left" | "pad_right" => {
                        let w = a0.as_f64().unwrap_or(0.0).clamp(0.0, 10_000.0) as usize;
                        let ch = args.get(1).and_then(|v| v.as_str()).and_then(|c| c.chars().next()).unwrap_or(' ');
                        let k = w.saturating_sub(s.chars().count());
                        let pad: String = std::iter::repeat(ch).take(k).collect();
                        Value::str(if name == "pad_left" { format!("{pad}{s}") } else { format!("{s}{pad}") })
                    }
                    "to_num" => s.trim().parse::<f64>().map(Value::Num).unwrap_or(Value::Null),
                    "capitalize" => {
                        let mut c = s.chars();
                        Value::str(match c.next() {
                            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                            None => String::new(),
                        })
                    }
                    _ => return self.throw(format!("a string has no method “{name}”"), line, col),
                })
            }
            Value::List(l) => self.list_method(l.clone(), name, args, line, col),
            Value::Null => self.throw(format!("cannot call .{name}() on null (use ?. if it may be empty)"), line, col),
            other => self.throw(format!("a {} has no method “{name}”", other.type_name()), line, col),
        }
    }

    fn list_method(&mut self, l: Rc<RefCell<Vec<Value>>>, name: &str, args: Vec<Value>, line: u32, col: u32) -> R<Value> {
        let a0 = args.first().cloned().unwrap_or(Value::Null);
        let snapshot = |l: &Rc<RefCell<Vec<Value>>>| l.borrow().clone();
        Ok(match name {
            "len" | "count" if args.is_empty() => Value::Num(l.borrow().len() as f64),
            "push" | "add" => {
                if l.borrow().len() + args.len() > MAX_LIST {
                    return self.throw("list too long", line, col);
                }
                l.borrow_mut().extend(args);
                Value::Num(l.borrow().len() as f64)
            }
            "pop" => l.borrow_mut().pop().unwrap_or(Value::Null),
            "shift" => {
                let mut b = l.borrow_mut();
                if b.is_empty() { Value::Null } else { b.remove(0) }
            }
            "clear" => {
                l.borrow_mut().clear();
                Value::Null
            }
            "remove" => {
                let i = a0.as_f64().unwrap_or(-1.0);
                let mut b = l.borrow_mut();
                if i >= 0.0 && (i as usize) < b.len() { b.remove(i as usize) } else { Value::Null }
            }
            "first" => l.borrow().first().cloned().unwrap_or(Value::Null),
            "last" => l.borrow().last().cloned().unwrap_or(Value::Null),
            "contains" | "includes" => Value::Bool(l.borrow().iter().any(|v| values_equal(v, &a0))),
            "index_of" => Value::Num(l.borrow().iter().position(|v| values_equal(v, &a0)).map(|i| i as f64).unwrap_or(-1.0)),
            "join" => {
                let sep = a0.as_str().unwrap_or(", ").to_owned();
                let items = snapshot(&l);
                let parts: Vec<String> = items.iter().map(|x| self.show(x)).collect();
                let s = parts.join(&sep);
                if s.len() > MAX_STR {
                    return self.throw("string too long", line, col);
                }
                Value::str(s)
            }
            "reverse" => Value::list(l.borrow().iter().rev().cloned().collect()),
            "slice" => {
                let b = l.borrow();
                let n = b.len() as f64;
                let fix = |v: f64| (if v < 0.0 { (n + v).max(0.0) } else { v.min(n) }) as usize;
                let (x, y) = (fix(a0.as_f64().unwrap_or(0.0)), fix(args.get(1).and_then(|v| v.as_f64()).unwrap_or(n)));
                Value::list(if x < y { b[x..y].to_vec() } else { vec![] })
            }
            "take" => Value::list(l.borrow().iter().take(a0.as_f64().unwrap_or(0.0).max(0.0) as usize).cloned().collect()),
            "skip" => Value::list(l.borrow().iter().skip(a0.as_f64().unwrap_or(0.0).max(0.0) as usize).cloned().collect()),
            "concat" => {
                let mut v = snapshot(&l);
                for a in &args {
                    match a {
                        Value::List(o) => v.extend(o.borrow().iter().cloned()),
                        o => v.push(o.clone()),
                    }
                }
                if v.len() > MAX_LIST {
                    return self.throw("list too long", line, col);
                }
                Value::list(v)
            }
            "flat" => {
                let mut v = vec![];
                for x in snapshot(&l) {
                    match x {
                        Value::List(o) => v.extend(o.borrow().iter().cloned()),
                        o => v.push(o),
                    }
                }
                Value::list(v)
            }
            "unique" => {
                let mut v: Vec<Value> = vec![];
                for x in snapshot(&l) {
                    if !v.iter().any(|y| values_equal(y, &x)) {
                        v.push(x);
                    }
                }
                Value::list(v)
            }
            "sum" => Value::Num(l.borrow().iter().filter_map(|v| v.as_f64()).sum()),
            "min" | "max" => return self.builtin(name, vec![Value::List(l)], line, col),
            "map" | "for_each" | "filter" | "find" | "find_index" | "some" | "every" | "count" | "sort_by" | "group_by" | "sum_by" => {
                let f = self.need_fn(&args, 0, name, line, col)?;
                let items = snapshot(&l);
                match name {
                    "map" => {
                        let mut out = Vec::with_capacity(items.len());
                        for (i, x) in items.into_iter().enumerate() {
                            out.push(self.call_fn_arg(&f, vec![x, Value::Num(i as f64)], line, col)?);
                        }
                        Value::list(out)
                    }
                    "for_each" => {
                        for (i, x) in items.into_iter().enumerate() {
                            self.call_fn_arg(&f, vec![x, Value::Num(i as f64)], line, col)?;
                        }
                        Value::Null
                    }
                    "filter" => {
                        let mut out = vec![];
                        for (i, x) in items.into_iter().enumerate() {
                            if self.call_fn_arg(&f, vec![x.clone(), Value::Num(i as f64)], line, col)?.truthy() {
                                out.push(x);
                            }
                        }
                        Value::list(out)
                    }
                    "find" | "find_index" => {
                        for (i, x) in items.into_iter().enumerate() {
                            if self.call_fn_arg(&f, vec![x.clone(), Value::Num(i as f64)], line, col)?.truthy() {
                                return Ok(if name == "find" { x } else { Value::Num(i as f64) });
                            }
                        }
                        if name == "find" { Value::Null } else { Value::Num(-1.0) }
                    }
                    "some" => {
                        for x in items {
                            if self.call_fn_arg(&f, vec![x], line, col)?.truthy() {
                                return Ok(Value::Bool(true));
                            }
                        }
                        Value::Bool(false)
                    }
                    "every" => {
                        for x in items {
                            if !self.call_fn_arg(&f, vec![x], line, col)?.truthy() {
                                return Ok(Value::Bool(false));
                            }
                        }
                        Value::Bool(true)
                    }
                    "count" => {
                        let mut n = 0.0;
                        for x in items {
                            if self.call_fn_arg(&f, vec![x], line, col)?.truthy() {
                                n += 1.0;
                            }
                        }
                        Value::Num(n)
                    }
                    "sum_by" => {
                        let mut t = 0.0;
                        for x in items {
                            if let Value::Num(n) = self.call_fn_arg(&f, vec![x], line, col)? {
                                t += n;
                            }
                        }
                        Value::Num(t)
                    }
                    "group_by" => {
                        let mut m: BTreeMap<String, Value> = BTreeMap::new();
                        for x in items {
                            let k = self.call_fn_arg(&f, vec![x.clone()], line, col)?;
                            let k = self.show(&k);
                            match m.get(&k) {
                                Some(Value::List(g)) => g.borrow_mut().push(x),
                                _ => {
                                    m.insert(k, Value::list(vec![x]));
                                }
                            }
                        }
                        Value::map(m)
                    }
                    _ => {
                        // sort_by(key fn)
                        let mut keyed = Vec::with_capacity(items.len());
                        for x in items {
                            let k = self.call_fn_arg(&f, vec![x.clone()], line, col)?;
                            keyed.push((k, x));
                        }
                        keyed.sort_by(|a, b| cmp_values(&a.0, &b.0));
                        Value::list(keyed.into_iter().map(|(_, x)| x).collect())
                    }
                }
            }
            "reduce" => {
                let f = self.need_fn(&args, 0, name, line, col)?;
                let items = snapshot(&l);
                let mut iter = items.into_iter();
                let mut acc = match args.get(1) {
                    Some(v) => v.clone(),
                    None => iter.next().unwrap_or(Value::Null),
                };
                for x in iter {
                    acc = self.call_fn_arg(&f, vec![acc, x], line, col)?;
                }
                acc
            }
            "sort" => {
                let mut v = snapshot(&l);
                if let Some(f @ (Value::Func(_) | Value::Native(_))) = args.first() {
                    // comparator: negative / zero / positive
                    let mut err: Option<Ex> = None;
                    let f = f.clone();
                    v.sort_by(|a, b| {
                        if err.is_some() {
                            return std::cmp::Ordering::Equal;
                        }
                        match self.call_fn_arg(&f, vec![a.clone(), b.clone()], line, col) {
                            Ok(Value::Num(n)) => n.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal),
                            Ok(_) => std::cmp::Ordering::Equal,
                            Err(e) => {
                                err = Some(e);
                                std::cmp::Ordering::Equal
                            }
                        }
                    });
                    if let Some(e) = err {
                        return Err(e);
                    }
                } else {
                    v.sort_by(cmp_values);
                }
                Value::list(v)
            }
            _ => return self.throw(format!("a list has no method “{name}”"), line, col),
        })
    }
}

// ───────────────────────────── syntax highlighting (for the editor) ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hl {
    Plain,
    Keyword,
    Literal,
    Number,
    Str,
    Comment,
    Func,
    Punct,
}

/// Splits `src` into `(byte range, class)` chunks covering the whole text. Never fails, works on half-typed code.
pub fn highlight(src: &str) -> Vec<(std::ops::Range<usize>, Hl)> {
    let b: Vec<(usize, char)> = src.char_indices().collect();
    let mut out: Vec<(std::ops::Range<usize>, Hl)> = vec![];
    let mut i = 0;
    let end_of = |j: usize| if j < b.len() { b[j].0 } else { src.len() };
    while i < b.len() {
        let (start, c) = b[i];
        let mut j = i + 1;
        let kind;
        if (c == '/' && b.get(i + 1).map(|x| x.1) == Some('/')) || c == '#' {
            while j < b.len() && b[j].1 != '\n' {
                j += 1;
            }
            kind = Hl::Comment;
        } else if c == '/' && b.get(i + 1).map(|x| x.1) == Some('*') {
            j = i + 2;
            while j < b.len() && !(b[j].1 == '*' && b.get(j + 1).map(|x| x.1) == Some('/')) {
                j += 1;
            }
            j = (j + 2).min(b.len());
            kind = Hl::Comment;
        } else if c == '"' || c == '\'' || c == '`' {
            while j < b.len() && b[j].1 != c {
                if b[j].1 == '\\' {
                    j += 1;
                }
                j += 1;
            }
            j = (j + 1).min(b.len());
            kind = Hl::Str;
        } else if c.is_ascii_digit() {
            while j < b.len() && (b[j].1.is_ascii_digit() || (b[j].1 == '.' && b.get(j + 1).map_or(false, |x| x.1.is_ascii_digit())) || b[j].1 == '_') {
                j += 1;
            }
            kind = Hl::Number;
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            while j < b.len() && (b[j].1.is_alphanumeric() || b[j].1 == '_' || b[j].1 == '$') {
                j += 1;
            }
            let w = &src[start..end_of(j)];
            let next = b[j.min(b.len())..].iter().find(|x| !x.1.is_whitespace()).map(|x| x.1);
            kind = match w {
                "true" | "false" | "null" => Hl::Literal,
                w if KEYWORDS.contains(&w) => Hl::Keyword,
                _ if next == Some('(') => Hl::Func,
                _ => Hl::Plain,
            };
        } else if c.is_whitespace() {
            while j < b.len() && b[j].1.is_whitespace() {
                j += 1;
            }
            kind = Hl::Plain;
        } else {
            kind = if "+-*/%=<>!&|?:.,;(){}[]".contains(c) { Hl::Punct } else { Hl::Plain };
        }
        let e = end_of(j.min(b.len()));
        out.push((start..e, kind));
        i = j;
    }
    out
}

/// Names of the built-in functions (used by the editor's reference panel).
pub fn builtin_names() -> &'static [&'static str] {
    &BUILTINS
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod script_tests {
    use super::*;

    fn run_ok(src: &str) -> Vec<String> {
        let o = run(src, &mut NoHost);
        assert!(o.error.is_none(), "unexpected error: {:?}\n{src}", o.error);
        o.output
    }
    fn run_err(src: &str) -> ScriptError {
        run(src, &mut NoHost).error.expect("expected an error")
    }

    #[test]
    fn arithmetic_and_strings() {
        assert_eq!(run_ok("print(1 + 2 * 3, 2 ** 3 ** 2, 10 % 4, 7 / 2, -3 + 5)"), vec!["7 512 2 3.5 2"]);
        assert_eq!(run_ok("let n = 3\nprint(\"n is {n}, double {n * 2}\", 'raw {n}')"), vec!["n is 3, double 6 raw {n}"]);
        assert_eq!(run_ok("print(\"a\" + 1, \"ab\" * 2, \"Hello\".upper(), \" x \".trim().len())"), vec!["a1 abab HELLO 1"]);
    }

    #[test]
    fn control_flow_and_functions() {
        let src = "fn fib(n) { if n < 2 { return n } return fib(n - 1) + fib(n - 2) }\nprint(fib(15))\nlet t = 0\nfor i in 1..5 { if i == 3 { continue } t += i }\nprint(t)\nlet k = 0\nwhile true { k += 1\n if k >= 4 { break } }\nprint(k)";
        assert_eq!(run_ok(src), vec!["610", "7", "4"]);
    }

    #[test]
    fn collections_and_lambdas() {
        let src = "let xs = [5, 3, 8, 1]\nprint(xs.sort(), xs.map(|x| x * 2).filter(|x| x > 6), xs.reduce(|a, b| a + b, 0))\nlet m = {a: 1, \"b c\": 2}\nm.d = 4\nm[\"a\"] += 10\nprint(m, keys(m), m.has(\"d\"))\nfor k, v in xs { if k == 0 { print(k, v) } }";
        assert_eq!(run_ok(src), vec!["[1, 3, 5, 8] [10, 16] 17", "{a: 11, b c: 2, d: 4} [\"a\", \"b c\", \"d\"] true", "0 5"]);
    }

    #[test]
    fn closures_try_catch_and_optional() {
        let src = "fn counter() { let c = 0\n return || { c += 1\n return c } }\nlet f = counter()\nf()\nprint(f())\ntry { error(\"boom\") } catch e { print(\"caught\", e) }\ntry { let z = 1 / 0 } catch e { print(e) }\nlet n = null\nprint(n?.x, n ?? \"dflt\", true ? 1 : 2)";
        assert_eq!(run_ok(src), vec!["2", "caught boom", "division by zero", "null dflt 1"]);
    }

    #[test]
    fn newlines_end_statements() {
        assert_eq!(run_ok("let a = 1\nlet b = 2\n-a\nprint(a + b)\nlet s = [1,\n2,\n3]\nprint(s.len())"), vec!["3", "3"]);
    }

    #[test]
    fn errors_have_positions() {
        let e = run_err("let a = 1\nprint(a +)\n");
        assert_eq!(e.line, 2);
        let e = run_err("let a = 1\nlet b = a.foo()\n");
        assert_eq!((e.line, e.msg.contains("no method")), (2, true));
        assert!(run_err("x = 1").msg.contains("not defined"));
        assert!(run_err("const x = 1\nx = 2").msg.contains("constant"));
        assert!(run_err("print(unknown_fn(1))").msg.contains("not defined"));
    }

    #[test]
    fn sandbox_limits() {
        let e = run_err("while true { }");
        assert!(e.msg.contains("steps"), "{}", e.msg);
        let e = run_err("fn f() { return f() }\nf()");
        assert!(e.msg.contains("nested"), "{}", e.msg);
        let e = run_err("let s = \"x\"\nwhile true { s = s + s }");
        assert!(e.msg.contains("too long") || e.msg.contains("steps"), "{}", e.msg);
        let deep = "(".repeat(500) + "1" + &")".repeat(500);
        assert!(run_err(&format!("print({deep})")).msg.contains("nested"));
    }

    #[test]
    fn highlight_covers_everything() {
        let src = "let x = \"hi {x}\" // c\nfn go() { return 1.5 }";
        let h = highlight(src);
        let mut pos = 0;
        for (r, _) in &h {
            assert_eq!(r.start, pos);
            pos = r.end;
        }
        assert_eq!(pos, src.len());
        assert!(h.iter().any(|(_, k)| *k == Hl::Comment) && h.iter().any(|(_, k)| *k == Hl::Keyword));
        // half-typed code never panics
        for cut in 0..src.len() {
            if src.is_char_boundary(cut) {
                let _ = highlight(&src[..cut]);
            }
        }
    }

    #[test]
    fn json_and_misc() {
        assert_eq!(run_ok("print(json({a: [1, \"x\", null], b: true}))"), vec!["{\"a\":[1,\"x\",null],\"b\":true}"]);
        assert_eq!(run_ok("print([3,1,2].sort_by(|x| -x), range(3), [1,2,3,4].group_by(|x| x % 2 == 0))"), vec!["[3, 2, 1] [0, 1, 2] {false: [1, 3], true: [2, 4]}"]);
    }

    #[test]
    fn fuzz_never_panics() {
        let pieces = ["let ", "x", " = ", "1", "(", ")", "{", "}", "[", "]", "|", "\"a{", "}\"", "fn ", "if ", "else ", "for ", " in ", ".", "?.", "..", "+", "-", "\n", ",", "return ", "try ", "catch "];
        let mut seed = 12345u64;
        for _ in 0..3000 {
            let mut s = String::new();
            for _ in 0..(seed % 14 + 2) {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                s.push_str(pieces[(seed >> 33) as usize % pieces.len()]);
            }
            let _ = run(&s, &mut NoHost);
            let _ = highlight(&s);
        }
    }
}
