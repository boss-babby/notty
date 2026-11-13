//! LaTeX math for Lumen — a small, dependency-free TeX math engine written for egui.
//!
//! * [`typeset`] parses a TeX formula and lays it out in 2-D (fractions, roots, scripts, big operators with limits, stretchy
//!   delimiters, matrices / cases / aligned, accents, `\text`, `\mathbb` …) → a [`MathBox`] that [`paint`] draws with the painter.
//! * [`to_unicode`] is the single-line fallback used inside running text (`$x^2 + \alpha$` → `x² + α`).
//! * [`inline_spans`] finds `$…$` / `$$…$$` in Markdown text (Pandoc rules, so "costs $5 and $10" stays text).
//!
//! The parser is lenient: unknown commands are drawn as literal text, unbalanced braces never panic.

use egui::epaint::text::Fonts;
use egui::text::{LayoutJob, TextFormat};
use egui::{pos2, Color32, FontFamily, FontId, Painter, Pos2, Shape, Stroke};

// ───────────────────────────── math font (XITS Math, OFL) ─────────────────────────────
//
// Formulas are set in a real math font: Latin / Greek letters are mapped to the Unicode "Mathematical Italic / Bold" alphabets (no
// synthetic slant), and every box gets its EXACT ink extents (ascent / descent) and advance from the font's own tables. The old engine
// guessed the extents from the sans-serif UI font, which is what made scripts, fractions and brackets collide.

static MATH_TTF: &[u8] = include_bytes!("../assets/XITSMath-Regular.otf");

fn mface() -> Option<&'static ttf_parser::Face<'static>> {
    static F: std::sync::OnceLock<Option<ttf_parser::Face<'static>>> = std::sync::OnceLock::new();
    F.get_or_init(|| ttf_parser::Face::parse(MATH_TTF, 0).ok()).as_ref()
}

/// Metrics of one glyph in em units: advance, ink top (above the baseline), ink bottom (below), right ink overhang beyond the advance.
#[derive(Clone, Copy, Default, Debug)]
struct Gm {
    adv: f32,
    top: f32,
    bot: f32,
    over: f32,
    ink: bool,
}

fn gm(c: char) -> Option<Gm> {
    let f = mface()?;
    let gid = f.glyph_index(c)?;
    let upem = f.units_per_em() as f32;
    let adv = f.glyph_hor_advance(gid).unwrap_or(0) as f32 / upem;
    Some(match f.glyph_bounding_box(gid) {
        Some(r) => Gm { adv, top: r.y_max as f32 / upem, bot: -(r.y_min as f32) / upem, over: (r.x_max as f32 / upem - adv).max(0.0), ink: true },
        None => Gm { adv, ..Default::default() },
    })
}

/// Maps a character to the math alphabet that matches `face` (Mathematical Italic / Bold / Bold Italic).
fn style_char(c: char, face: Face) -> char {
    if face.mono {
        return c;
    }
    let off = |base: u32, i: u32| char::from_u32(base + i).unwrap_or(c);
    match (c, face.italic, face.bold) {
        ('h', true, false) => '\u{210E}',
        ('a'..='z', true, false) => off(0x1D44E, c as u32 - 'a' as u32),
        ('A'..='Z', true, false) => off(0x1D434, c as u32 - 'A' as u32),
        ('a'..='z', true, true) => off(0x1D482, c as u32 - 'a' as u32),
        ('A'..='Z', true, true) => off(0x1D468, c as u32 - 'A' as u32),
        ('a'..='z', false, true) => off(0x1D41A, c as u32 - 'a' as u32),
        ('A'..='Z', false, true) => off(0x1D400, c as u32 - 'A' as u32),
        ('0'..='9', _, true) => off(0x1D7CE, c as u32 - '0' as u32),
        ('\u{3B1}'..='\u{3C1}', true, false) => off(0x1D6FC, c as u32 - 0x3B1),
        ('\u{3C2}', true, false) => '\u{1D70D}',
        ('\u{3C3}'..='\u{3C9}', true, false) => off(0x1D70E, c as u32 - 0x3C3),
        _ => c,
    }
}

/// Italic correction of the last glyph of a node (how far its ink leans over the advance) in px.
fn italic_corr(n: &Node, size: f32) -> f32 {
    match n {
        Node::Sym { s, face, .. } => s.chars().last().and_then(|c| gm(style_char(c, *face))).map_or(0.0, |g| g.over * size),
        Node::Row(v) => v.last().map_or(0.0, |l| italic_corr(l, size)),
        _ => 0.0,
    }
}

// ───────────────────────────── AST ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Class {
    Ord,
    Op,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
struct Face {
    italic: bool,
    bold: bool,
    mono: bool,
    /// upright letters (`\mathrm`, operator names)
    roman: bool,
}

#[derive(Clone, Debug)]
enum Node {
    Row(Vec<Node>),
    Sym { s: String, class: Class, face: Face },
    /// Big operator (∑ ∏ ∫ …): `limits` = scripts above / below in display style.
    BigOp { s: String, limits: bool },
    Frac(Box<Node>, Box<Node>, bool),
    Sqrt(Option<Box<Node>>, Box<Node>),
    Script(Box<Node>, Option<Box<Node>>, Option<Box<Node>>),
    Delim(String, Box<Node>, String),
    Matrix { rows: Vec<Vec<Node>>, left: String, right: String, align: u8 },
    Accent(String, Box<Node>),
    Space(f32),
    Over(Box<Node>, Box<Node>),
    Under(Box<Node>, Box<Node>),
}

// ───────────────────────────── tokenizer / parser ─────────────────────────────

#[derive(Clone, PartialEq, Debug)]
enum Tok {
    Cmd(String),
    Ch(char),
}

fn tokenize(src: &str) -> Vec<Tok> {
    let cs: Vec<char> = src.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\\' {
            i += 1;
            if i >= cs.len() {
                break;
            }
            if cs[i].is_ascii_alphabetic() {
                let st = i;
                while i < cs.len() && cs[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push(Tok::Cmd(cs[st..i].iter().collect()));
            } else {
                out.push(Tok::Cmd(cs[i].to_string()));
                i += 1;
            }
        } else if c == '%' {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
        } else if c.is_whitespace() {
            i += 1;
        } else {
            out.push(Tok::Ch(c));
            i += 1;
        }
    }
    out
}

struct Parser {
    t: Vec<Tok>,
    i: usize,
}

fn greek(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ", "epsilon" => "ϵ", "varepsilon" => "ε", "zeta" => "ζ",
        "eta" => "η", "theta" => "θ", "vartheta" => "ϑ", "iota" => "ι", "kappa" => "κ", "lambda" => "λ", "mu" => "μ", "nu" => "ν",
        "xi" => "ξ", "pi" => "π", "varpi" => "ϖ", "rho" => "ρ", "varrho" => "ϱ", "sigma" => "σ", "varsigma" => "ς", "tau" => "τ",
        "upsilon" => "υ", "phi" => "ϕ", "varphi" => "φ", "chi" => "χ", "psi" => "ψ", "omega" => "ω",
        "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ", "Xi" => "Ξ", "Pi" => "Π", "Sigma" => "Σ",
        "Upsilon" => "Υ", "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
        _ => return None,
    })
}

fn symbol(name: &str) -> Option<(&'static str, Class)> {
    use Class::*;
    Some(match name {
        "pm" => ("±", Bin), "mp" => ("∓", Bin), "times" => ("×", Bin), "div" => ("÷", Bin), "cdot" => ("⋅", Bin), "ast" => ("∗", Bin),
        "star" => ("⋆", Bin), "circ" => ("∘", Bin), "bullet" => ("•", Bin), "oplus" => ("⊕", Bin), "otimes" => ("⊗", Bin), "ominus" => ("⊖", Bin),
        "cup" => ("∪", Bin), "cap" => ("∩", Bin), "setminus" => ("∖", Bin), "wedge" | "land" => ("∧", Bin), "vee" | "lor" => ("∨", Bin),
        "leq" | "le" => ("≤", Rel), "geq" | "ge" => ("≥", Rel), "neq" | "ne" => ("≠", Rel), "approx" => ("≈", Rel), "equiv" => ("≡", Rel),
        "sim" => ("∼", Rel), "simeq" => ("≃", Rel), "cong" => ("≅", Rel), "propto" => ("∝", Rel), "ll" => ("≪", Rel), "gg" => ("≫", Rel),
        "in" => ("∈", Rel), "notin" => ("∉", Rel), "ni" => ("∋", Rel), "subset" => ("⊂", Rel), "supset" => ("⊃", Rel), "subseteq" => ("⊆", Rel),
        "supseteq" => ("⊇", Rel), "to" | "rightarrow" => ("→", Rel), "leftarrow" | "gets" => ("←", Rel), "leftrightarrow" => ("↔", Rel),
        "Rightarrow" | "implies" => ("⇒", Rel), "Leftarrow" => ("⇐", Rel), "Leftrightarrow" | "iff" => ("⇔", Rel), "mapsto" => ("↦", Rel),
        "uparrow" => ("↑", Rel), "downarrow" => ("↓", Rel), "perp" => ("⊥", Rel), "parallel" => ("∥", Rel), "mid" => ("∣", Rel),
        "infty" => ("∞", Ord), "partial" => ("∂", Ord), "nabla" => ("∇", Ord), "forall" => ("∀", Ord), "exists" => ("∃", Ord),
        "emptyset" | "varnothing" => ("∅", Ord), "neg" | "lnot" => ("¬", Ord), "angle" => ("∠", Ord), "hbar" => ("ℏ", Ord), "ell" => ("ℓ", Ord),
        "prime" => ("′", Ord), "degree" => ("°", Ord), "therefore" => ("∴", Rel), "because" => ("∵", Rel), "dagger" => ("†", Ord),
        "aleph" => ("ℵ", Ord), "Re" => ("ℜ", Ord), "Im" => ("ℑ", Ord), "wp" => ("℘", Ord), "ldots" | "dots" | "dotsc" => ("…", Ord),
        "cdots" | "dotsb" => ("⋯", Ord), "vdots" => ("⋮", Ord), "ddots" => ("⋱", Ord), "langle" => ("⟨", Open), "rangle" => ("⟩", Close),
        "lfloor" => ("⌊", Open), "rfloor" => ("⌋", Close), "lceil" => ("⌈", Open), "rceil" => ("⌉", Close), "lbrace" => ("{", Open),
        "rbrace" => ("}", Close), "{" => ("{", Open), "}" => ("}", Close), "%" => ("%", Ord), "$" => ("$", Ord), "#" => ("#", Ord),
        "&" => ("&", Ord), "_" => ("_", Ord), "|" | "Vert" | "lVert" | "rVert" => ("‖", Ord), "vert" | "lvert" | "rvert" => ("|", Ord),
        "surd" => ("√", Ord), "checkmark" => ("✓", Ord), "copyright" => ("©", Ord), "S" => ("§", Ord), "P" => ("¶", Ord),
        _ => return None,
    })
}

fn bigop(name: &str) -> Option<(&'static str, bool)> {
    Some(match name {
        "sum" => ("∑", true), "prod" => ("∏", true), "coprod" => ("∐", true), "bigcup" => ("⋃", true), "bigcap" => ("⋂", true),
        "bigoplus" => ("⨁", true), "bigotimes" => ("⨂", true), "bigvee" => ("⋁", true), "bigwedge" => ("⋀", true),
        "int" => ("∫", false), "iint" => ("∬", false), "iiint" => ("∭", false), "oint" => ("∮", false),
        _ => return None,
    })
}

const FUNCS: [&str; 32] = [
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh", "coth", "log", "ln", "lg", "exp",
    "lim", "limsup", "liminf", "max", "min", "sup", "inf", "det", "dim", "ker", "gcd", "deg", "arg", "Pr", "hom",
];

fn blackboard(c: char) -> Option<&'static str> {
    Some(match c {
        'R' => "ℝ", 'N' => "ℕ", 'Z' => "ℤ", 'Q' => "ℚ", 'C' => "ℂ", 'P' => "ℙ", 'H' => "ℍ", 'E' => "𝔼",
        _ => return None,
    })
}

fn sym(s: &str, class: Class) -> Node {
    Node::Sym { s: s.to_owned(), class, face: Face::default() }
}

fn text_node(s: &str, face: Face) -> Node {
    let mut v = vec![];
    for c in s.chars() {
        v.push(Node::Sym { s: c.to_string(), class: Class::Ord, face });
    }
    Node::Row(v)
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn is_ch(&self, c: char) -> bool {
        matches!(self.peek(), Some(Tok::Ch(x)) if *x == c)
    }

    fn is_cmd(&self, n: &str) -> bool {
        matches!(self.peek(), Some(Tok::Cmd(x)) if x == n)
    }

    /// Parses until a closing `}`, `\right`, `\end`, `&`, `\\` or the end (none of which is consumed).
    fn row(&mut self, face: Face) -> Node {
        let mut items: Vec<Node> = vec![];
        while let Some(t) = self.peek().cloned() {
            match &t {
                Tok::Ch('}') | Tok::Ch('&') => break,
                Tok::Cmd(c) if c == "right" || c == "end" || c == "\\" || c == "cr" => break,
                _ => {}
            }
            let atom = self.atom(face);
            let Some(atom) = atom else { continue };
            // scripts
            let mut cur = atom;
            let (mut sub, mut sup) = (None, None);
            loop {
                if self.is_ch('^') {
                    self.i += 1;
                    let a = self.arg(face);
                    // `x^\prime`, `x'` → merge
                    sup = Some(match sup.take() {
                        Some(prev) => Node::Row(vec![prev, a]),
                        None => a,
                    });
                } else if self.is_ch('_') {
                    self.i += 1;
                    sub = Some(self.arg(face));
                } else if self.is_ch('\'') {
                    self.i += 1;
                    let p = sym("′", Class::Ord);
                    sup = Some(match sup.take() {
                        Some(prev) => Node::Row(vec![prev, p]),
                        None => p,
                    });
                } else if self.is_cmd("limits") || self.is_cmd("nolimits") {
                    let lim = self.is_cmd("limits");
                    self.i += 1;
                    if let Node::BigOp { limits, .. } = &mut cur {
                        *limits = lim;
                    }
                } else {
                    break;
                }
            }
            if sub.is_some() || sup.is_some() {
                cur = Node::Script(Box::new(cur), sub.map(Box::new), sup.map(Box::new));
            }
            items.push(cur);
        }
        Node::Row(items)
    }

    /// One argument: a `{group}` or a single token.
    fn arg(&mut self, face: Face) -> Node {
        if self.is_ch('{') {
            self.i += 1;
            let r = self.row(face);
            if self.is_ch('}') {
                self.i += 1;
            }
            return r;
        }
        match self.atom(face) {
            Some(n) => n,
            None => Node::Row(vec![]),
        }
    }

    /// Raw text of a `{…}` argument (for `\text`, `\begin{env}` …).
    fn raw_arg(&mut self) -> String {
        let mut s = String::new();
        if self.is_ch('{') {
            self.i += 1;
            let mut depth = 1;
            while let Some(t) = self.peek().cloned() {
                self.i += 1;
                match t {
                    Tok::Ch('{') => {
                        depth += 1;
                        s.push('{');
                    }
                    Tok::Ch('}') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        s.push('}');
                    }
                    Tok::Ch(c) => s.push(c),
                    Tok::Cmd(c) => {
                        s.push('\\');
                        s.push_str(&c);
                        s.push(' ');
                    }
                }
            }
        }
        s
    }

    fn delim_tok(&mut self) -> String {
        match self.peek().cloned() {
            Some(Tok::Ch(c)) => {
                self.i += 1;
                c.to_string()
            }
            Some(Tok::Cmd(c)) => {
                self.i += 1;
                match c.as_str() {
                    "{" | "lbrace" => "{".into(),
                    "}" | "rbrace" => "}".into(),
                    "langle" => "⟨".into(),
                    "rangle" => "⟩".into(),
                    "lfloor" => "⌊".into(),
                    "rfloor" => "⌋".into(),
                    "lceil" => "⌈".into(),
                    "rceil" => "⌉".into(),
                    "|" | "Vert" => "‖".into(),
                    "vert" => "|".into(),
                    _ => ".".into(),
                }
            }
            None => ".".into(),
        }
    }

    fn atom(&mut self, face: Face) -> Option<Node> {
        let t = self.peek().cloned()?;
        self.i += 1;
        Some(match t {
            Tok::Ch('{') => {
                let r = self.row(face);
                if self.is_ch('}') {
                    self.i += 1;
                }
                r
            }
            Tok::Ch('}') | Tok::Ch('^') | Tok::Ch('_') | Tok::Ch('&') => return None,
            Tok::Ch(c) => self.char_node(c, face),
            Tok::Cmd(c) => return self.command(&c, face),
        })
    }

    fn char_node(&self, c: char, face: Face) -> Node {
        use Class::*;
        let mk = |s: &str, class| Node::Sym { s: s.to_owned(), class, face: Face { italic: false, ..face } };
        match c {
            '+' => mk("+", Bin),
            '-' => mk("−", Bin),
            '*' => mk("∗", Bin),
            '/' => mk("/", Ord),
            '=' => mk("=", Rel),
            '<' => mk("<", Rel),
            '>' => mk(">", Rel),
            ':' => mk(":", Rel),
            '(' | '[' => mk(&c.to_string(), Open),
            ')' | ']' => mk(&c.to_string(), Close),
            ',' | ';' => mk(&c.to_string(), Punct),
            '!' | '?' | '.' | '|' => mk(&c.to_string(), Ord),
            '~' => Node::Space(0.3),
            c if c.is_alphabetic() => {
                // Latin letters are italic in math; digits / non-Latin scripts stay upright
                let it = face.italic || (c.is_ascii_alphabetic() && !face.bold && !face.mono && !face.roman);
                Node::Sym { s: c.to_string(), class: Ord, face: Face { italic: it, ..face } }
            }
            c => Node::Sym { s: c.to_string(), class: Ord, face: Face { italic: false, ..face } },
        }
    }

    fn command(&mut self, name: &str, face: Face) -> Option<Node> {
        if let Some(g) = greek(name) {
            let upper = name.chars().next().map_or(false, |c| c.is_uppercase());
            return Some(Node::Sym { s: g.to_owned(), class: Class::Ord, face: Face { italic: !upper && !face.bold, ..face } });
        }
        if let Some((s, class)) = symbol(name) {
            return Some(Node::Sym { s: s.to_owned(), class, face: Face { italic: false, ..face } });
        }
        if let Some((s, limits)) = bigop(name) {
            return Some(Node::BigOp { s: s.to_owned(), limits });
        }
        if FUNCS.contains(&name) {
            let limits = matches!(name, "lim" | "limsup" | "liminf" | "max" | "min" | "sup" | "inf" | "det" | "gcd" | "Pr");
            let row = text_node(name, Face { italic: false, ..face });
            return Some(if limits { Node::BigOp { s: format!("\u{1}{name}"), limits: true } } else { row });
        }
        Some(match name {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let a = self.arg(face);
                let b = self.arg(face);
                Node::Frac(Box::new(a), Box::new(b), false)
            }
            "binom" | "dbinom" | "tbinom" => {
                let a = self.arg(face);
                let b = self.arg(face);
                Node::Delim("(".into(), Box::new(Node::Frac(Box::new(a), Box::new(b), true)), ")".into())
            }
            "sqrt" => {
                let mut idx = None;
                if self.is_ch('[') {
                    self.i += 1;
                    let mut items = vec![];
                    while !self.is_ch(']') && self.peek().is_some() {
                        if let Some(a) = self.atom(face) {
                            items.push(a);
                        }
                    }
                    self.i += 1;
                    idx = Some(Box::new(Node::Row(items)));
                }
                let b = self.arg(face);
                Node::Sqrt(idx, Box::new(b))
            }
            "text" | "textrm" | "mathrm" | "textnormal" | "operatorname" | "mbox" | "mathop" | "textsf" | "mathsf" => {
                if self.is_ch('{') && matches!(name, "text" | "textrm" | "textnormal" | "mbox" | "textsf") {
                    let raw = self.raw_arg();
                    let mut v = vec![];
                    for c in raw.chars() {
                        if c == ' ' {
                            v.push(Node::Space(0.28));
                        } else {
                            v.push(Node::Sym { s: c.to_string(), class: Class::Ord, face: Face { italic: false, ..face } });
                        }
                    }
                    Node::Row(v)
                } else {
                    self.arg(Face { italic: false, bold: face.bold, mono: false, roman: true })
                }
            }
            "mathbf" | "boldsymbol" | "bm" | "textbf" | "pmb" => self.arg(Face { bold: true, italic: false, mono: face.mono, roman: false }),
            "mathit" | "textit" | "emph" => self.arg(Face { italic: true, ..face }),
            "mathtt" | "texttt" => self.arg(Face { mono: true, italic: false, bold: false, roman: true }),
            "mathcal" | "mathscr" | "mathfrak" | "mathbb" | "Bbb" => {
                let blackb = name == "mathbb" || name == "Bbb";
                let a = self.arg(Face { bold: !blackb || face.bold, italic: false, mono: false, roman: true });
                if blackb {
                    map_syms(a, &|s| {
                        let mut it = s.chars();
                        match (it.next(), it.next()) {
                            (Some(c), None) => blackboard(c).map(str::to_owned),
                            _ => None,
                        }
                    })
                } else {
                    a
                }
            }
            "hat" | "widehat" | "bar" | "overline" | "vec" | "overrightarrow" | "tilde" | "widetilde" | "dot" | "ddot" | "underline" | "check"
            | "acute" | "grave" | "breve" | "mathring" | "underbrace" | "overbrace" => {
                let a = self.arg(face);
                // `\underbrace{x}_{n}` / `\overbrace{x}^{n}`: the script becomes the label
                if name == "underbrace" && self.is_ch('_') {
                    self.i += 1;
                    let l = self.arg(face);
                    return Some(Node::Under(Box::new(Node::Accent("underbrace".into(), Box::new(a))), Box::new(l)));
                }
                if name == "overbrace" && self.is_ch('^') {
                    self.i += 1;
                    let l = self.arg(face);
                    return Some(Node::Over(Box::new(Node::Accent("overbrace".into(), Box::new(a))), Box::new(l)));
                }
                Node::Accent(name.to_owned(), Box::new(a))
            }
            "overset" | "stackrel" => {
                let top = self.arg(face);
                let base = self.arg(face);
                Node::Over(Box::new(base), Box::new(top))
            }
            "underset" => {
                let bot = self.arg(face);
                let base = self.arg(face);
                Node::Under(Box::new(base), Box::new(bot))
            }
            "left" => {
                let l = self.delim_tok();
                let inner = self.row(face);
                let mut r = ".".to_owned();
                if self.is_cmd("right") {
                    self.i += 1;
                    r = self.delim_tok();
                }
                Node::Delim(l, Box::new(inner), r)
            }
            "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr" => {
                let d = self.delim_tok();
                let class = if matches!(d.as_str(), ")" | "]" | "}" | "⟩" | "⌋" | "⌉") { Class::Close } else { Class::Open };
                Node::Sym { s: d, class, face: Face::default() }
            }
            "begin" => {
                let env = self.raw_arg();
                return Some(self.environment(&env, face));
            }
            "color" | "textcolor" | "colorbox" => {
                let _ = self.raw_arg();
                if name == "color" {
                    return None;
                }
                self.arg(face)
            }
            "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits" | "relax" | "protect" | "allowbreak" => return None,
            "," | "thinspace" => Node::Space(0.17),
            ":" | ">" | "medspace" => Node::Space(0.22),
            ";" | "thickspace" => Node::Space(0.28),
            "!" | "negthinspace" => Node::Space(-0.17),
            " " | "space" => Node::Space(0.3),
            "quad" => Node::Space(1.0),
            "qquad" => Node::Space(2.0),
            "hspace" | "hskip" | "kern" => {
                let _ = self.raw_arg();
                Node::Space(0.5)
            }
            "phantom" | "hphantom" => {
                let _ = self.arg(face);
                Node::Space(0.0)
            }
            "not" => {
                let a = self.atom(face)?;
                Node::Row(vec![sym("̸", Class::Ord), a])
            }
            "mod" | "bmod" | "pmod" => {
                let a = if name == "pmod" { Some(self.arg(face)) } else { None };
                let mut v = vec![Node::Space(0.3), text_node("mod", Face::default()), Node::Space(0.2)];
                if let Some(a) = a {
                    v.insert(0, sym("(", Class::Open));
                    v.push(a);
                    v.push(sym(")", Class::Close));
                }
                Node::Row(v)
            }
            "\\" | "cr" => return None,
            other => text_node(&format!("\\{other}"), Face { italic: false, ..face }),
        })
    }

    fn environment(&mut self, env: &str, face: Face) -> Node {
        let mut rows: Vec<Vec<Node>> = vec![];
        let mut cur: Vec<Node> = vec![];
        if env == "array" || env == "alignedat" {
            let _ = self.raw_arg();
        }
        loop {
            let cell = self.row(face);
            cur.push(cell);
            match self.peek().cloned() {
                Some(Tok::Ch('&')) => self.i += 1,
                Some(Tok::Cmd(c)) if c == "\\" || c == "cr" => {
                    self.i += 1;
                    // optional `[2pt]` after a row break
                    if self.is_ch('[') {
                        while !self.is_ch(']') && self.peek().is_some() {
                            self.i += 1;
                        }
                        self.i += 1;
                    }
                    rows.push(std::mem::take(&mut cur));
                }
                Some(Tok::Cmd(c)) if c == "end" => {
                    self.i += 1;
                    let _ = self.raw_arg();
                    break;
                }
                Some(Tok::Cmd(c)) if c == "right" => {
                    self.i += 1; // stray
                }
                Some(Tok::Ch('}')) => self.i += 1,
                None => break,
                _ => {}
            }
        }
        if !cur.is_empty() && !(cur.len() == 1 && matches!(&cur[0], Node::Row(r) if r.is_empty())) {
            rows.push(cur);
        }
        let (l, r, align) = match env {
            "pmatrix" => ("(", ")", 0),
            "bmatrix" => ("[", "]", 0),
            "Bmatrix" => ("{", "}", 0),
            "vmatrix" => ("|", "|", 0),
            "Vmatrix" => ("‖", "‖", 0),
            "cases" => ("{", ".", 1),
            "aligned" | "align" | "align*" | "split" | "alignedat" | "eqnarray" => (".", ".", 2),
            "gathered" | "gather" | "gather*" | "multline" => (".", ".", 0),
            _ => (".", ".", 0),
        };
        Node::Matrix { rows, left: l.into(), right: r.into(), align }
    }
}

fn map_syms(n: Node, f: &dyn Fn(&str) -> Option<String>) -> Node {
    match n {
        Node::Sym { s, class, face } => match f(&s) {
            Some(m) => Node::Sym { s: m, class, face: Face { italic: false, bold: false, mono: face.mono, roman: true } },
            None => Node::Sym { s, class, face },
        },
        Node::Row(v) => Node::Row(v.into_iter().map(|x| map_syms(x, f)).collect()),
        other => other,
    }
}

fn parse(src: &str) -> Node {
    let mut p = Parser { t: tokenize(src), i: 0 };
    let mut items = vec![];
    // a stray `}` / `&` at top level must not stop the parse
    while p.peek().is_some() {
        let r = p.row(Face::default());
        items.push(r);
        if p.peek().is_some() {
            p.i += 1;
        }
    }
    Node::Row(items)
}

// ───────────────────────────── layout ─────────────────────────────

#[derive(Clone, Debug)]
enum Item {
    Text { x: f32, y: f32, s: String, size: f32, face: Face },
    Line { a: Pos2, b: Pos2, w: f32 },
    Poly { pts: Vec<Pos2>, w: f32 },
    Dot { c: Pos2, r: f32 },
}

#[derive(Clone, Debug, Default)]
pub struct MathBox {
    pub w: f32,
    pub asc: f32,
    pub desc: f32,
    items: Vec<Item>,
}

impl MathBox {
    pub fn height(&self) -> f32 {
        self.asc + self.desc
    }

    fn push_box(&mut self, o: MathBox, dx: f32, dy: f32) {
        for it in o.items {
            self.items.push(match it {
                Item::Text { x, y, s, size, face } => Item::Text { x: x + dx, y: y + dy, s, size, face },
                Item::Line { a, b, w } => Item::Line { a: a + egui::vec2(dx, dy), b: b + egui::vec2(dx, dy), w },
                Item::Poly { pts, w } => Item::Poly { pts: pts.into_iter().map(|p| p + egui::vec2(dx, dy)).collect(), w },
                Item::Dot { c, r } => Item::Dot { c: c + egui::vec2(dx, dy), r },
            });
        }
    }
}

fn font_of(size: f32, face: Face) -> FontId {
    if face.mono {
        FontId::monospace(size * 0.92)
    } else {
        FontId::new(size, FontFamily::Name("math".into()))
    }
}

struct Lay<'a> {
    fonts: &'a Fonts,
}

impl Lay<'_> {
    fn text_w(&self, s: &str, size: f32, face: Face) -> f32 {
        let f = font_of(size, face);
        s.chars()
            .map(|c| if face.mono { self.fonts.glyph_width(&f, c) } else { gm(c).map_or_else(|| self.fonts.glyph_width(&f, c), |g| g.adv * size) })
            .sum()
    }

    fn text_box(&self, s: &str, size: f32, face: Face) -> MathBox {
        let mapped: String = s.chars().map(|c| style_char(c, face)).collect();
        let w = self.text_w(&mapped, size, face);
        // exact ink extents from the font (fallback for characters the math font lacks: typical text proportions)
        let (mut asc, mut desc) = (0.0f32, 0.0f32);
        let mut any = false;
        for c in mapped.chars() {
            match gm(c) {
                Some(g) if g.ink => {
                    asc = asc.max(g.top * size);
                    desc = desc.max(g.bot * size);
                    any = true;
                }
                Some(_) => {}
                None if !c.is_whitespace() => {
                    asc = asc.max(0.72 * size);
                    desc = desc.max(0.2 * size);
                    any = true;
                }
                None => {}
            }
        }
        if !any {
            asc = 0.0;
            desc = 0.0;
        }
        MathBox { w, asc, desc, items: vec![Item::Text { x: 0.0, y: 0.0, s: mapped, size, face }] }
    }

    fn axis(&self, size: f32) -> f32 {
        0.26 * size
    }

    fn lay(&self, n: &Node, size: f32, display: bool) -> MathBox {
        match n {
            Node::Row(v) => self.row(v, size, display),
            Node::Sym { s, face, .. } => self.text_box(s, size, *face),
            Node::Space(em) => MathBox { w: em * size, asc: 0.0, desc: 0.0, items: vec![] },
            Node::BigOp { s, limits } => self.bigop(s, *limits, size, display, None, None),
            Node::Script(base, sub, sup) => self.script(base, sub.as_deref(), sup.as_deref(), size, display),
            Node::Frac(a, b, tight) => self.frac(a, b, *tight, size, display),
            Node::Sqrt(idx, b) => self.sqrt(idx.as_deref(), b, size, display),
            Node::Delim(l, inner, r) => {
                let b = self.lay(inner, size, display);
                self.delimited(l, r, b, size)
            }
            Node::Matrix { rows, left, right, align } => self.matrix(rows, left, right, *align, size),
            Node::Accent(kind, b) => self.accent(kind, b, size, display),
            Node::Over(base, top) => {
                let bb = self.lay(base, size, display);
                let tb = self.lay(top, size * 0.7, false);
                self.stack(bb, Some(tb), None, size)
            }
            Node::Under(base, bot) => {
                let bb = self.lay(base, size, display);
                let lb = self.lay(bot, size * 0.7, false);
                self.stack(bb, None, Some(lb), size)
            }
        }
    }

    fn class_of(n: &Node) -> Class {
        match n {
            Node::Sym { class, .. } => *class,
            Node::BigOp { .. } => Class::Op,
            Node::Script(b, _, _) => Self::class_of(b),
            _ => Class::Ord,
        }
    }

    fn row(&self, v: &[Node], size: f32, display: bool) -> MathBox {
        let mut out = MathBox::default();
        let mut x = 0.0f32;
        let mut prev: Option<Class> = None;
        for (i, n) in v.iter().enumerate() {
            let mut class = Self::class_of(n);
            // a binary operator at the start or after another operator / relation / opening bracket is unary
            if class == Class::Bin && matches!(prev, None | Some(Class::Bin) | Some(Class::Rel) | Some(Class::Open) | Some(Class::Punct) | Some(Class::Op)) {
                class = Class::Ord;
            }
            // … and a binary operator before a closing bracket / relation / end is an ordinary symbol
            if class == Class::Bin {
                let next = v.get(i + 1).map(Self::class_of);
                if matches!(next, None | Some(Class::Rel) | Some(Class::Close) | Some(Class::Punct)) {
                    class = Class::Ord;
                }
            }
            let before = match (prev, class) {
                (Some(Class::Ord | Class::Close | Class::Op), Class::Bin) => 0.22,
                (Some(Class::Ord | Class::Close), Class::Rel) | (Some(Class::Op), Class::Rel) => 0.28,
                (Some(Class::Bin), _) => 0.22,
                (Some(Class::Rel), _) => 0.28,
                (Some(Class::Punct), _) => 0.17,
                (Some(Class::Ord | Class::Close), Class::Op) => 0.17,
                (Some(Class::Op), Class::Ord | Class::Open) => 0.17,
                _ => 0.0,
            };
            x += before * size;
            let b = self.lay(n, size, display);
            out.asc = out.asc.max(b.asc);
            out.desc = out.desc.max(b.desc);
            let w = b.w;
            out.push_box(b, x, 0.0);
            x += w;
            prev = Some(class);
        }
        out.w = x;
        out
    }

    fn script(&self, base: &Node, sub: Option<&Node>, sup: Option<&Node>, size: f32, display: bool) -> MathBox {
        if let Node::BigOp { s, limits } = base {
            return self.bigop(s, *limits && display, size, display, sub, sup);
        }
        let b = self.lay(base, size, display);
        let ic = italic_corr(base, size);
        let ss = (size * 0.72).max(8.0);
        let sb = sub.map(|n| self.lay(n, ss, false));
        let sp = sup.map(|n| self.lay(n, ss, false));
        let mut out = MathBox::default();
        let bw = b.w;
        let (basc, bdesc) = (b.asc, b.desc);
        out.asc = basc;
        out.desc = bdesc;
        out.push_box(b, 0.0, 0.0);
        let mut w = bw;
        let up = sp.as_ref().map(|s| (0.38 * size).max(basc - 0.32 * size).max(s.desc + 0.2 * size));
        let mut down = sb.as_ref().map(|s| (0.2 * size).max(bdesc + 0.1 * size).max(s.asc - 0.55 * size * 0.5));
        if let (Some(u), Some(d), Some(sp), Some(sb)) = (up, down, sp.as_ref(), sb.as_ref()) {
            // keep a gap between the two scripts
            let need = sp.desc + sb.asc + 0.18 * size;
            if u + d < need {
                down = Some(need - u);
            }
        }
        let sw = sp.as_ref().map_or(0.0, |s| s.w).max(sb.as_ref().map_or(0.0, |s| s.w));
        if let (Some(s), Some(u)) = (sp, up) {
            out.asc = out.asc.max(u + s.asc);
            out.push_box(s, bw + 0.03 * size + ic, -u);
        }
        if let (Some(s), Some(d)) = (sb, down) {
            out.desc = out.desc.max(d + s.desc);
            out.push_box(s, bw + 0.02 * size, d);
        }
        w += sw + 0.06 * size + ic * 0.5;
        out.w = w;
        out
    }

    fn bigop(&self, s: &str, limits: bool, size: f32, display: bool, sub: Option<&Node>, sup: Option<&Node>) -> MathBox {
        // named operators (lim, max …) arrive as "\u{1}name"
        let (txt, named) = match s.strip_prefix('\u{1}') {
            Some(n) => (n.to_owned(), true),
            None => (s.to_owned(), false),
        };
        let is_int = txt.starts_with('∫') || txt.starts_with('∬') || txt.starts_with('∭') || txt.starts_with('∮');
        let scale = if named || !display { 1.0 } else if is_int { 1.3 } else { 1.25 };
        let op_face = Face::default();
        let mut base = self.text_box(&txt, size * scale, op_face);
        if !named {
            // centre the ink of the big symbol on the math axis (exact extents, whatever the glyph looks like)
            let ink_c = (base.asc - base.desc) / 2.0;
            let dy = ink_c - self.axis(size);
            for it in base.items.iter_mut() {
                if let Item::Text { y, .. } = it {
                    *y += dy;
                }
            }
            base.asc = base.asc - ink_c + self.axis(size);
            base.desc = base.desc + ink_c - self.axis(size);
        }
        let ss = (size * 0.72).max(8.0);
        let sb = sub.map(|n| self.lay(n, ss, false));
        let sp = sup.map(|n| self.lay(n, ss, false));
        let mut out = MathBox::default();
        if limits && display {
            let w = base.w.max(sb.as_ref().map_or(0.0, |b| b.w)).max(sp.as_ref().map_or(0.0, |b| b.w));
            out.asc = base.asc;
            out.desc = base.desc;
            let bw = base.w;
            let (ba, bd) = (base.asc, base.desc);
            out.push_box(base, (w - bw) / 2.0, 0.0);
            if let Some(t) = sp {
                let dy = -(ba + 0.14 * size + t.desc);
                out.asc = out.asc.max(-dy + t.asc);
                let tw = t.w;
                out.push_box(t, (w - tw) / 2.0, dy);
            }
            if let Some(t) = sb {
                let dy = bd + 0.14 * size + t.asc;
                out.desc = out.desc.max(dy + t.desc);
                let tw = t.w;
                out.push_box(t, (w - tw) / 2.0, dy);
            }
            out.w = w + 0.04 * size;
        } else {
            let bw = base.w;
            out.asc = base.asc;
            out.desc = base.desc;
            let (ba, bd) = (base.asc, base.desc);
            out.push_box(base, 0.0, 0.0);
            let mut sw = 0.0f32;
            if let Some(t) = sp {
                let dy = -(ba * 0.78).max(0.4 * size);
                out.asc = out.asc.max(-dy + t.asc);
                sw = sw.max(t.w);
                out.push_box(t, bw + if is_int { 0.2 * size } else { 0.03 * size }, dy);
            }
            if let Some(t) = sb {
                let dy = (bd + 0.12 * size).max(0.25 * size);
                out.desc = out.desc.max(dy + t.desc);
                sw = sw.max(t.w);
                out.push_box(t, bw - if is_int { 0.15 * size } else { 0.0 }, dy);
            }
            out.w = bw + sw + 0.08 * size;
        }
        out
    }

    fn stack(&self, base: MathBox, over: Option<MathBox>, under: Option<MathBox>, size: f32) -> MathBox {
        let w = base.w.max(over.as_ref().map_or(0.0, |b| b.w)).max(under.as_ref().map_or(0.0, |b| b.w));
        let mut out = MathBox { w, asc: base.asc, desc: base.desc, items: vec![] };
        let (bw, ba, bd) = (base.w, base.asc, base.desc);
        out.push_box(base, (w - bw) / 2.0, 0.0);
        if let Some(t) = over {
            let dy = -(ba + 0.1 * size + t.desc);
            out.asc = out.asc.max(-dy + t.asc);
            let tw = t.w;
            out.push_box(t, (w - tw) / 2.0, dy);
        }
        if let Some(t) = under {
            let dy = bd + 0.1 * size + t.asc;
            out.desc = out.desc.max(dy + t.desc);
            let tw = t.w;
            out.push_box(t, (w - tw) / 2.0, dy);
        }
        out
    }

    fn frac(&self, a: &Node, b: &Node, tight: bool, size: f32, display: bool) -> MathBox {
        let fs = if display { size * 0.92 } else { size * 0.8 };
        let na = self.lay(a, fs, false);
        let nb = self.lay(b, fs, false);
        let axis = self.axis(size);
        let w = na.w.max(nb.w) + 0.24 * size;
        let gap = 0.16 * size;
        let t = (size * 0.055).max(0.8);
        let mut out = MathBox { w, asc: 0.0, desc: 0.0, items: vec![] };
        let up = axis + t / 2.0 + gap + na.desc;
        let down = -axis + t / 2.0 + gap + nb.asc;
        out.asc = up + na.asc;
        out.desc = down + nb.desc;
        let (aw, bw) = (na.w, nb.w);
        out.push_box(na, (w - aw) / 2.0, -up);
        out.push_box(nb, (w - bw) / 2.0, down);
        if !tight {
            out.items.push(Item::Line { a: pos2(0.06 * size, -axis), b: pos2(w - 0.06 * size, -axis), w: t });
        }
        out
    }

    fn sqrt(&self, idx: Option<&Node>, b: &Node, size: f32, display: bool) -> MathBox {
        let body = self.lay(b, size, display);
        let t = (size * 0.055).max(0.8);
        let rad_w = 0.52 * size;
        let top = body.asc + 0.2 * size;
        let bottom = body.desc + 0.05 * size;
        let h = top + bottom;
        let ib = idx.map(|n| self.lay(n, size * 0.55, false));
        let idx_w = ib.as_ref().map_or(0.0, |i| (i.w - 0.25 * size).max(0.0));
        let mut out = MathBox { w: idx_w + rad_w + body.w + 0.1 * size, asc: top + t, desc: bottom, items: vec![] };
        let x0 = idx_w;
        let pts = vec![
            pos2(x0 + 0.02 * size, bottom - 0.42 * h),
            pos2(x0 + 0.12 * size, bottom - 0.48 * h),
            pos2(x0 + 0.27 * size, bottom),
            pos2(x0 + 0.47 * size, -top),
            pos2(x0 + rad_w + body.w + 0.08 * size, -top),
        ];
        out.items.push(Item::Poly { pts, w: t });
        if let Some(i) = ib {
            let iw = i.w;
            out.push_box(i, (x0 - iw + 0.3 * size).max(0.0), -(0.45 * h) + bottom);
        }
        let bw = body.w;
        out.push_box(body, x0 + rad_w, 0.0);
        out.w = x0 + rad_w + bw + 0.1 * size;
        out
    }

    /// Stretchy delimiters: drawn with strokes so they can be any height.
    fn delim_shape(&self, d: &str, x: f32, top: f32, bottom: f32, size: f32) -> (Vec<Item>, f32) {
        let t = (size * 0.06).max(0.9);
        let h = bottom - top;
        let mid = (top + bottom) / 2.0;
        let curve = |x0: f32, dir: f32, steps: usize, depth: f32| -> Vec<Pos2> {
            // `(`-like arc from the top to the bottom, bulging by `depth` to the side `dir`
            (0..=steps)
                .map(|i| {
                    let u = i as f32 / steps as f32;
                    let q = 2.0 * u - 1.0;
                    pos2(x0 + dir * depth * (1.0 - q * q), top + u * h)
                })
                .collect()
        };
        // ordinary-size brackets are real glyphs of the math font (crisp hinting-free outlines); only tall ones are drawn as strokes
        if h <= 1.9 * size && matches!(d, "(" | ")" | "[" | "]" | "{" | "}" | "|" | "‖" | "⟨" | "⟩" | "⌊" | "⌋" | "⌈" | "⌉") {
            let ch = match d {
                "⟨" => '\u{27E8}',
                "⟩" => '\u{27E9}',
                "‖" => '\u{2016}',
                other => other.chars().next().unwrap_or('('),
            };
            if let Some(g) = gm(ch) {
                if g.ink {
                    let ink = (g.top + g.bot).max(0.1);
                    let fs = (h / ink).clamp(size, 1.9 * size);
                    let y = mid + (g.top - g.bot) * 0.5 * fs;
                    let pad = 0.03 * fs;
                    let it = Item::Text { x: x + pad, y, s: ch.to_string(), size: fs, face: Face::default() };
                    return (vec![it], g.adv * fs + 2.0 * pad);
                }
            }
        }
        match d {
            "(" => {
                let depth = (0.1 * size + h * 0.025).min(0.22 * size);
                let pts: Vec<Pos2> = curve(x + 0.2 * size, -1.0, 12, depth).into_iter().collect();
                (vec![Item::Poly { pts, w: t }], 0.32 * size)
            }
            ")" => {
                let depth = (0.1 * size + h * 0.025).min(0.22 * size);
                let pts: Vec<Pos2> = curve(x + 0.1 * size, 1.0, 12, depth);
                (vec![Item::Poly { pts, w: t }], 0.32 * size)
            }
            "[" | "⌊" | "⌈" => {
                let xs = x + 0.2 * size;
                let mut pts = vec![pos2(xs + 0.12 * size, top), pos2(xs, top), pos2(xs, bottom), pos2(xs + 0.12 * size, bottom)];
                if d == "⌊" {
                    pts.remove(0);
                    pts.insert(0, pos2(xs, top));
                    pts.remove(1);
                }
                if d == "⌈" {
                    pts.truncate(3);
                }
                (vec![Item::Poly { pts, w: t }], 0.34 * size)
            }
            "]" | "⌋" | "⌉" => {
                let xs = x + 0.14 * size;
                let mut pts = vec![pos2(xs - 0.12 * size, top), pos2(xs, top), pos2(xs, bottom), pos2(xs - 0.12 * size, bottom)];
                if d == "⌋" {
                    pts.remove(0);
                }
                if d == "⌉" {
                    pts.pop();
                }
                (vec![Item::Poly { pts, w: t }], 0.34 * size)
            }
            "{" | "}" => {
                let s = if d == "{" { -1.0 } else { 1.0 };
                let cx = x + 0.2 * size;
                let a = 0.09 * size;
                let pts = vec![
                    pos2(cx - s * a, top),
                    pos2(cx, top + a),
                    pos2(cx, mid - a),
                    pos2(cx + s * a, mid),
                    pos2(cx, mid + a),
                    pos2(cx, bottom - a),
                    pos2(cx - s * a, bottom),
                ];
                // for `{` the tip points left
                let pts: Vec<Pos2> = pts.into_iter().map(|p| pos2(if d == "{" { 2.0 * cx - p.x } else { p.x }, p.y)).collect();
                (vec![Item::Poly { pts, w: t }], 0.38 * size)
            }
            "|" => (vec![Item::Line { a: pos2(x + 0.12 * size, top), b: pos2(x + 0.12 * size, bottom), w: t }], 0.26 * size),
            "‖" => (
                vec![
                    Item::Line { a: pos2(x + 0.09 * size, top), b: pos2(x + 0.09 * size, bottom), w: t },
                    Item::Line { a: pos2(x + 0.2 * size, top), b: pos2(x + 0.2 * size, bottom), w: t },
                ],
                0.32 * size,
            ),
            "⟨" => (vec![Item::Poly { pts: vec![pos2(x + 0.26 * size, top), pos2(x + 0.1 * size, mid), pos2(x + 0.26 * size, bottom)], w: t }], 0.34 * size),
            "⟩" => (vec![Item::Poly { pts: vec![pos2(x + 0.08 * size, top), pos2(x + 0.24 * size, mid), pos2(x + 0.08 * size, bottom)], w: t }], 0.34 * size),
            "." | "" => (vec![], 0.0),
            other => {
                let b = self.text_box(other, size, Face::default());
                let w = b.w;
                let mut items = vec![];
                for it in b.items {
                    if let Item::Text { y, s, size, face, .. } = it {
                        items.push(Item::Text { x, y: mid + 0.3 * size + y * 0.0, s, size, face });
                    }
                }
                (items, w)
            }
        }
    }

    fn delimited(&self, l: &str, r: &str, inner: MathBox, size: f32) -> MathBox {
        let axis = self.axis(size);
        let half = (inner.asc - axis).max(inner.desc + axis).max(0.72 * size * 0.5 + 0.0);
        let half = half.max(0.5 * size);
        let (top, bottom) = (-axis - half - 0.04 * size, -axis + half + 0.04 * size);
        let (li, lw) = self.delim_shape(l, 0.0, top, bottom, size);
        let (ri, rw) = self.delim_shape(r, 0.0, top, bottom, size);
        let mut out = MathBox { w: lw + inner.w + rw, asc: (-top).max(inner.asc), desc: bottom.max(inner.desc), items: vec![] };
        out.items.extend(li);
        let iw = inner.w;
        out.push_box(inner, lw, 0.0);
        out.push_box(MathBox { items: ri, ..Default::default() }, lw + iw, 0.0);
        out
    }

    fn matrix(&self, rows: &[Vec<Node>], left: &str, right: &str, align: u8, size: f32) -> MathBox {
        let cells: Vec<Vec<MathBox>> = rows.iter().map(|r| r.iter().map(|c| self.lay(c, size, false)).collect()).collect();
        let ncol = cells.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut colw = vec![0.0f32; ncol];
        for r in &cells {
            for (j, c) in r.iter().enumerate() {
                colw[j] = colw[j].max(c.w);
            }
        }
        let row_gap = 0.35 * size;
        let col_gap = if align == 2 { 0.0 } else { 1.0 * size };
        let mut rh: Vec<(f32, f32)> = vec![];
        for r in &cells {
            let a = r.iter().map(|c| c.asc).fold(0.7 * size, f32::max);
            let d = r.iter().map(|c| c.desc).fold(0.25 * size, f32::max);
            rh.push((a, d));
        }
        let total_h: f32 = rh.iter().map(|(a, d)| a + d).sum::<f32>() + row_gap * rh.len().saturating_sub(1) as f32;
        let axis = self.axis(size);
        let mut y = -axis - total_h / 2.0;
        let mut body = MathBox::default();
        // alignment columns for `aligned`: (right, left) pairs separated by a wider gap
        let gap_after = |j: usize| -> f32 {
            if align == 2 {
                if j % 2 == 1 { 2.0 * size } else { 0.0 }
            } else {
                col_gap
            }
        };
        let mut width = 0.0f32;
        for (i, r) in cells.into_iter().enumerate() {
            let (a, d) = rh[i];
            let base_y = y + a;
            let mut x = 0.0f32;
            for (j, c) in r.into_iter().enumerate() {
                let off = match align {
                    1 => 0.0,
                    2 => if j % 2 == 0 { colw[j] - c.w } else { 0.0 },
                    _ => (colw[j] - c.w) / 2.0,
                };
                body.push_box(c, x + off, base_y);
                x += colw[j] + gap_after(j);
            }
            width = width.max(x - gap_after(ncol.saturating_sub(1)));
            y += a + d + row_gap;
        }
        body.w = width;
        body.asc = (axis + total_h / 2.0).max(0.7 * size);
        body.desc = (total_h / 2.0 - axis).max(0.25 * size);
        if left == "." && right == "." {
            return body;
        }
        self.delimited(left, right, body, size)
    }

    fn accent(&self, kind: &str, b: &Node, size: f32, display: bool) -> MathBox {
        let body = self.lay(b, size, display);
        let t = (size * 0.055).max(0.8);
        let mut out = MathBox { w: body.w, asc: body.asc, desc: body.desc, items: vec![] };
        let w = body.w;
        let top = -(body.asc + 0.1 * size);
        let cx = w / 2.0;
        match kind {
            "overline" | "bar" => {
                let y = if kind == "bar" { -(body.asc + 0.08 * size) } else { top };
                out.items.push(Item::Line { a: pos2(0.02 * size, y), b: pos2(w - 0.02 * size, y), w: t });
                out.asc = body.asc + 0.18 * size;
            }
            "underline" => {
                let y = body.desc + 0.1 * size;
                out.items.push(Item::Line { a: pos2(0.0, y), b: pos2(w, y), w: t });
                out.desc = body.desc + 0.18 * size;
            }
            "hat" | "widehat" | "check" => {
                let hw = if kind == "widehat" { (w * 0.5).max(0.2 * size) } else { 0.18 * size };
                let (tip, base_y) = if kind == "check" { (top, top - 0.1 * size) } else { (top - 0.1 * size, top) };
                out.items.push(Item::Poly { pts: vec![pos2(cx - hw, base_y), pos2(cx, tip), pos2(cx + hw, base_y)], w: t });
                out.asc = body.asc + 0.22 * size;
            }
            "tilde" | "widetilde" => {
                let hw = if kind == "widetilde" { (w * 0.5).max(0.2 * size) } else { 0.17 * size };
                let pts: Vec<Pos2> = (0..=10).map(|i| { let u = i as f32 / 10.0; pos2(cx - hw + 2.0 * hw * u, top - 0.04 * size - 0.05 * size * (u * std::f32::consts::TAU).sin()) }).collect();
                out.items.push(Item::Poly { pts, w: t });
                out.asc = body.asc + 0.22 * size;
            }
            "vec" | "overrightarrow" => {
                let (x0, x1) = if kind == "vec" { (cx - 0.16 * size, cx + 0.16 * size) } else { (0.02 * size, w - 0.02 * size) };
                let y = top - 0.02 * size;
                out.items.push(Item::Line { a: pos2(x0, y), b: pos2(x1, y), w: t });
                out.items.push(Item::Poly { pts: vec![pos2(x1 - 0.07 * size, y - 0.06 * size), pos2(x1, y), pos2(x1 - 0.07 * size, y + 0.06 * size)], w: t });
                out.asc = body.asc + 0.22 * size;
            }
            "dot" | "ddot" => {
                let y = top - 0.03 * size;
                if kind == "dot" {
                    out.items.push(Item::Dot { c: pos2(cx, y), r: 0.04 * size });
                } else {
                    out.items.push(Item::Dot { c: pos2(cx - 0.07 * size, y), r: 0.04 * size });
                    out.items.push(Item::Dot { c: pos2(cx + 0.07 * size, y), r: 0.04 * size });
                }
                out.asc = body.asc + 0.2 * size;
            }
            "underbrace" => {
                let y = body.desc + 0.1 * size;
                let pts = brace_pts(0.0, w, y, 0.08 * size, 1.0);
                out.items.push(Item::Poly { pts, w: t });
                out.desc = body.desc + 0.2 * size;
            }
            "overbrace" => {
                let pts = brace_pts(0.0, w, top, 0.08 * size, -1.0);
                out.items.push(Item::Poly { pts, w: t });
                out.asc = body.asc + 0.2 * size;
            }
            _ => {
                // acute / grave / breve / mathring → a small mark above
                out.items.push(Item::Dot { c: pos2(cx, top - 0.02 * size), r: 0.035 * size });
                out.asc = body.asc + 0.18 * size;
            }
        }
        out.push_box(body, 0.0, 0.0);
        out
    }
}

fn brace_pts(x0: f32, x1: f32, y: f32, a: f32, dir: f32) -> Vec<Pos2> {
    let mid = (x0 + x1) / 2.0;
    vec![
        pos2(x0, y - dir * a),
        pos2(x0 + a, y),
        pos2(mid - a, y),
        pos2(mid, y + dir * a),
        pos2(mid + a, y),
        pos2(x1 - a, y),
        pos2(x1, y - dir * a),
    ]
}

// ───────────────────────────── public API ─────────────────────────────

/// Parses and lays out `tex`. `display` = block equation (bigger fractions, limits above / below big operators).
pub fn typeset(fonts: &Fonts, tex: &str, size: f32, display: bool) -> MathBox {
    let ast = parse(tex);
    let lay = Lay { fonts };
    let mut b = lay.lay(&ast, size, display);
    b.asc = b.asc.max(0.7 * size);
    b.desc = b.desc.max(0.2 * size);
    b
}

/// Draws a laid-out formula; `origin` is the top-left corner of its bounding box.
pub fn paint(p: &Painter, origin: Pos2, mb: &MathBox, color: Color32) {
    let ppp = p.ctx().pixels_per_point().max(1.0);
    let snap = |v: f32| (v * ppp).round() / ppp;
    // a stroke of `w` points becomes a whole number of device pixels, centred so that both edges sit on pixel boundaries (crisp bars)
    let thick = |w: f32| (w * ppp).round().max(1.0);
    let centre = |c: f32, wpx: f32| ((c * ppp - wpx / 2.0).round() + wpx / 2.0) / ppp;
    let base = origin + egui::vec2(0.0, mb.asc);
    let base = pos2(snap(base.x), snap(base.y));
    for it in &mb.items {
        match it {
            Item::Text { x, y, s, size, face } => {
                let font = font_of(*size, *face);
                let mut job = LayoutJob::default();
                job.append(s, 0.0, TextFormat { font_id: font, color, ..Default::default() });
                job.wrap.max_width = f32::INFINITY;
                let g = p.layout_job(job);
                let off = g.rows.first().and_then(|r| r.glyphs.first()).map_or(*size * 0.97, |gl| gl.pos.y);
                p.galley(pos2(snap(base.x + x), snap(base.y + y - off)), g, color);
            }
            Item::Line { a, b, w } => {
                let (a, b) = (base + a.to_vec2(), base + b.to_vec2());
                let wpx = thick(*w);
                let (a, b) = if (a.y - b.y).abs() < 0.01 {
                    let y = centre(a.y, wpx);
                    (pos2(snap(a.x), y), pos2(snap(b.x), y))
                } else if (a.x - b.x).abs() < 0.01 {
                    let x = centre(a.x, wpx);
                    (pos2(x, snap(a.y)), pos2(x, snap(b.y)))
                } else {
                    (a, b)
                };
                p.line_segment([a, b], Stroke::new(wpx / ppp, color));
            }
            Item::Poly { pts, w } => {
                let wpx = thick(*w * 1.1);
                p.add(Shape::line(pts.iter().map(|q| base + q.to_vec2()).collect(), Stroke::new(wpx / ppp, color)));
            }
            Item::Dot { c, r } => {
                p.circle_filled(base + c.to_vec2(), r.max(0.7 / ppp), color);
            }
        }
    }
}

// ───────────────────────────── inline: `$…$` in Markdown text ─────────────────────────────

/// `(start, end, tex, display)` char ranges of every `$…$` / `$$…$$` in `s` (end exclusive, delimiters included).
/// Pandoc rules: an opening `$` is followed by a non-space, the closing one is preceded by a non-space and not followed by a digit;
/// `\$` is a literal dollar.
pub fn inline_spans(s: &str) -> Vec<(usize, usize, String, bool)> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '\\' {
            i += 2;
            continue;
        }
        if cs[i] != '$' {
            i += 1;
            continue;
        }
        let double = i + 1 < cs.len() && cs[i + 1] == '$';
        let open = if double { 2 } else { 1 };
        let st = i + open;
        if st >= cs.len() || (!double && cs[st].is_whitespace()) {
            i += open;
            continue;
        }
        let mut j = st;
        let mut found = None;
        while j < cs.len() {
            if cs[j] == '\\' {
                j += 2;
                continue;
            }
            if cs[j] == '$' {
                if double {
                    if j + 1 < cs.len() && cs[j + 1] == '$' {
                        found = Some(j);
                        break;
                    }
                } else if !cs[j - 1].is_whitespace() && !cs.get(j + 1).map_or(false, |c| c.is_ascii_digit()) && j > st {
                    found = Some(j);
                    break;
                } else if cs[j - 1].is_whitespace() {
                    // `$5 and $10`: the second `$` can't close; keep scanning is wrong, bail out
                    break;
                }
            }
            if !double && cs[j] == '\n' {
                break;
            }
            j += 1;
        }
        match found {
            Some(e) => {
                let tex: String = cs[st..e].iter().collect();
                let end = e + open;
                out.push((i, end, tex, double));
                i = end;
            }
            None => i += open,
        }
    }
    out
}

/// Single-line Unicode rendering of a TeX formula (running text, search, plain-text export).
pub fn to_unicode(tex: &str) -> String {
    let ast = parse(tex);
    let mut s = String::new();
    uni(&ast, &mut s);
    s
}

fn script_chars(s: &str, sup: bool) -> Option<String> {
    let mut o = String::new();
    for c in s.chars() {
        let m = if sup {
            match c {
                '0' => '⁰', '1' => '¹', '2' => '²', '3' => '³', '4' => '⁴', '5' => '⁵', '6' => '⁶', '7' => '⁷', '8' => '⁸', '9' => '⁹',
                '+' => '⁺', '−' | '-' => '⁻', '=' => '⁼', '(' => '⁽', ')' => '⁾', 'n' => 'ⁿ', 'i' => 'ⁱ', 'x' => 'ˣ', 'T' => 'ᵀ', 'a' => 'ᵃ',
                'b' => 'ᵇ', 'c' => 'ᶜ', 'd' => 'ᵈ', 'e' => 'ᵉ', 'k' => 'ᵏ', 'm' => 'ᵐ', 'p' => 'ᵖ', 't' => 'ᵗ', 'y' => 'ʸ', '∗' | '*' => '*', '′' => '′', 'π' => 'π',
                _ => return None,
            }
        } else {
            match c {
                '0' => '₀', '1' => '₁', '2' => '₂', '3' => '₃', '4' => '₄', '5' => '₅', '6' => '₆', '7' => '₇', '8' => '₈', '9' => '₉',
                '+' => '₊', '−' | '-' => '₋', '=' => '₌', '(' => '₍', ')' => '₎', 'a' => 'ₐ', 'e' => 'ₑ', 'i' => 'ᵢ', 'j' => 'ⱼ', 'k' => 'ₖ',
                'n' => 'ₙ', 'o' => 'ₒ', 'p' => 'ₚ', 'r' => 'ᵣ', 's' => 'ₛ', 't' => 'ₜ', 'u' => 'ᵤ', 'v' => 'ᵥ', 'x' => 'ₓ', 'm' => 'ₘ', 'h' => 'ₕ', 'l' => 'ₗ',
                _ => return None,
            }
        };
        o.push(m);
    }
    Some(o)
}

fn uni(n: &Node, out: &mut String) {
    match n {
        Node::Row(v) => {
            let mut prev: Option<Class> = None;
            for (i, x) in v.iter().enumerate() {
                let class = Lay::class_of(x);
                let next = v.get(i + 1).map(Lay::class_of);
                let spaced = matches!(class, Class::Rel) || (class == Class::Bin && !matches!(prev, None | Some(Class::Bin | Class::Rel | Class::Open)) && !matches!(next, None | Some(Class::Rel | Class::Close)));
                if spaced && !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
                uni(x, out);
                if spaced || class == Class::Punct {
                    out.push(' ');
                }
                prev = Some(class);
            }
        }
        Node::Sym { s, .. } => out.push_str(s),
        Node::Space(em) => {
            if *em > 0.1 {
                out.push(' ');
            }
        }
        Node::BigOp { s, .. } => out.push_str(s.trim_start_matches('\u{1}')),
        Node::Frac(a, b, _) => {
            let (mut x, mut y) = (String::new(), String::new());
            uni(a, &mut x);
            uni(b, &mut y);
            let wrap = |s: String| if s.chars().count() > 1 && !s.chars().all(|c| c.is_alphanumeric()) { format!("({s})") } else { s };
            out.push_str(&format!("{}⁄{}", wrap(x), wrap(y)));
        }
        Node::Sqrt(i, b) => {
            let mut x = String::new();
            uni(b, &mut x);
            if let Some(i) = i {
                let mut k = String::new();
                uni(i, &mut k);
                match k.as_str() {
                    "3" => out.push('∛'),
                    "4" => out.push('∜'),
                    _ => {
                        out.push_str(&script_chars(&k, true).unwrap_or(k));
                        out.push('√');
                    }
                }
            } else {
                out.push('√');
            }
            if x.chars().count() > 1 {
                out.push('(');
                out.push_str(&x);
                out.push(')');
            } else {
                out.push_str(&x);
            }
        }
        Node::Script(b, sub, sup) => {
            uni(b, out);
            for (node, is_sup) in [(sub.as_deref(), false), (sup.as_deref(), true)] {
                if let Some(nd) = node {
                    let mut t = String::new();
                    uni(nd, &mut t);
                    let t2 = t.replace(' ', "");
                    match script_chars(&t2, is_sup) {
                        Some(sc) => out.push_str(&sc),
                        None => {
                            out.push(if is_sup { '^' } else { '_' });
                            if t.chars().count() > 1 {
                                out.push('(');
                                out.push_str(&t);
                                out.push(')');
                            } else {
                                out.push_str(&t);
                            }
                        }
                    }
                }
            }
        }
        Node::Delim(l, b, r) => {
            if l != "." {
                out.push_str(l);
            }
            uni(b, out);
            if r != "." {
                out.push_str(r);
            }
        }
        Node::Matrix { rows, left, right, .. } => {
            if left != "." {
                out.push_str(left);
            }
            for (i, r) in rows.iter().enumerate() {
                if i > 0 {
                    out.push_str("; ");
                }
                for (j, c) in r.iter().enumerate() {
                    if j > 0 {
                        out.push_str(", ");
                    }
                    uni(c, out);
                }
            }
            if right != "." {
                out.push_str(right);
            }
        }
        Node::Accent(kind, b) => {
            let mut x = String::new();
            uni(b, &mut x);
            let comb = match kind.as_str() {
                "hat" | "widehat" => "\u{302}",
                "bar" | "overline" => "\u{304}",
                "vec" | "overrightarrow" => "\u{20D7}",
                "tilde" | "widetilde" => "\u{303}",
                "dot" => "\u{307}",
                "ddot" => "\u{308}",
                _ => "",
            };
            if kind == "underline" || kind == "underbrace" || kind == "overbrace" || comb.is_empty() {
                out.push_str(&x);
            } else {
                for c in x.chars() {
                    out.push(c);
                    out.push_str(comb);
                }
            }
        }
        Node::Over(b, _) | Node::Under(b, _) => uni(b, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollar_rules() {
        assert_eq!(inline_spans("a $x^2$ b").len(), 1);
        assert_eq!(inline_spans("costs $5 and $10").len(), 0);
        assert_eq!(inline_spans("$$\\frac{a}{b}$$").len(), 1);
        assert_eq!(inline_spans("\\$5 and $x$").len(), 1);
        assert_eq!(inline_spans("$ x $").len(), 0);
    }

    #[test]
    fn unicode() {
        assert_eq!(to_unicode("x^2 + \\alpha"), "x² + α");
        assert_eq!(to_unicode("\\sqrt{x}"), "√x");
        assert_eq!(to_unicode("a_1"), "a₁");
        assert_eq!(to_unicode("\\frac{1}{2}"), "1⁄2");
        assert!(to_unicode("\\sum_{i=1}^{n} i").starts_with('∑'));
    }

    #[test]
    fn parses_everything_without_panic() {
        for t in [
            "\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}",
            "\\left( \\frac{a}{b} \\right)^2",
            "\\int_0^\\infty e^{-x^2} dx = \\frac{\\sqrt{\\pi}}{2}",
            "}{ \\frac",
            "\\begin{cases} x & y \\\\ z",
            "\\unknown{x}^",
            "\\sum_{i=1}^n \\binom{n}{k}",
        ] {
            let _ = to_unicode(t);
        }
    }
}

#[cfg(test)]
mod typeset_tests {
    use super::*;

    fn boxed(tex: &str, size: f32) -> MathBox {
        let ctx = egui::Context::default();
        crate::theme::setup_fonts(&ctx);
        let mut out = None;
        let _ = ctx.run(Default::default(), |ctx| {
            out = Some(ctx.fonts(|f| typeset(f, tex, size, true)));
        });
        out.unwrap()
    }

    #[test]
    fn math_font_has_exact_metrics_and_math_alphabets() {
        let g = gm('\u{1D465}').expect("math italic x is in the math font");
        assert!(g.adv > 0.3 && g.top > 0.3 && g.top < 0.8, "{g:?}");
        let sigma = gm('∑').unwrap();
        assert!(sigma.top + sigma.bot > 0.8);
        assert_eq!(style_char('x', Face { italic: true, ..Default::default() }), '\u{1D465}');
        assert_eq!(style_char('α', Face { italic: true, ..Default::default() }), '\u{1D6FC}');
        assert_eq!(style_char('7', Face::default()), '7');
    }

    #[test]
    fn formulas_lay_out_finite_and_text_never_overlaps() {
        for tex in ["E = mc^2", "\\frac{a+b}{c-d}", "\\sum_{i=0}^{n} i^2 = \\frac{n(n+1)(2n+1)}{6}", "\\int_0^\\infty e^{-x^2}\\,dx = \\frac{\\sqrt{\\pi}}{2}", "\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}", "\\left( \\frac{1}{2} \\right)^n", "x_{i}^{j} + \\alpha\\beta"] {
            let b = boxed(tex, 24.0);
            assert!(b.w.is_finite() && b.w > 10.0, "{tex}: w = {}", b.w);
            assert!(b.asc.is_finite() && b.desc.is_finite() && b.height() > 10.0, "{tex}");
            // texts that share a baseline are laid out left to right without overlapping (advance widths from the font)
            let mut row: Vec<(f32, f32, f32, f32)> = b
                .items
                .iter()
                .filter_map(|it| match it {
                    Item::Text { x, y, s, size, face } if !face.mono => Some((*y, *x, *x + s.chars().filter_map(gm).map(|g| g.adv * size).sum::<f32>(), *size)),
                    _ => None,
                })
                .collect();
            row.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.partial_cmp(&b.1).unwrap()));
            for p in row.windows(2) {
                if (p[0].0 - p[1].0).abs() < 0.01 && (p[0].3 - p[1].3).abs() < 0.01 {
                    assert!(p[0].2 <= p[1].1 + 0.6, "{tex}: overlap {:?}", p);
                }
            }
        }
    }
}
