//! What the parser built for each declaration, and its JSON form. The JSON is
//! byte-identical to the oracle's (`tools/oracle/oracle.ts`), so the two
//! parsers can be compared.

use super::{
    Error,
    term::{Body, Cell, LitValue, Patt, Quant, Span, Term},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceKind {
    Def,
    Type,
    Law,
}

impl TraceKind {
    fn name(self) -> &'static str {
        match self {
            Self::Def => "def",
            Self::Type => "type",
            Self::Law => "law",
        }
    }
}

/// One declaration as the parser built it. A field is `None` when the
/// parser did not reach the point that records it.
#[derive(Clone, Debug)]
pub struct TldTrace {
    pub file: String,
    pub kind: TraceKind,
    pub name: Option<String>,
    pub law: Option<bool>,
    pub unsafe_: Option<bool>,
    pub imports: Option<usize>,
    pub tele: Option<Vec<Cell>>,
    pub ret: Option<Term>,
    pub params: Option<Vec<Cell>>,
    pub kind_term: Option<Term>,
    pub ctrs: Option<Vec<(String, Vec<Cell>)>>,
    pub t: Option<Term>,
    pub bodies: Vec<(Body, Term)>,
}

impl TldTrace {
    pub(super) fn new(ns: &str, kind: TraceKind) -> Self {
        Self {
            file: ns.to_owned(),
            kind,
            name: None,
            law: None,
            unsafe_: None,
            imports: None,
            tele: None,
            ret: None,
            params: None,
            kind_term: None,
            ctrs: None,
            t: None,
            bodies: Vec::new(),
        }
    }

    /// The JSON of the oracle's `tld`.
    #[must_use]
    pub fn json(&self) -> String {
        let mut w = Json::default();
        w.open('{');
        w.key("kind");
        w.string(self.kind.name());
        w.key("file");
        w.string(&self.file);
        w.key("name");
        w.option(self.name.as_deref(), Json::string);
        w.key("law");
        w.option(self.law, Json::bool);
        w.key("unsafe");
        w.option(self.unsafe_, Json::bool);
        w.key("imports");
        w.option(self.imports, Json::number);
        w.key("tele");
        w.option(self.tele.as_deref(), Json::tele);
        w.key("ret");
        w.option(self.ret.as_ref(), Json::term);
        w.key("params");
        w.option(self.params.as_deref(), Json::tele);
        w.key("K");
        w.option(self.kind_term.as_ref(), Json::term);
        w.key("ctrs");
        w.option(self.ctrs.as_deref(), |w, ctrs| {
            w.open('[');
            for (c, fs) in ctrs {
                w.item();
                w.open('[');
                w.item();
                w.string(c);
                w.item();
                w.tele(fs);
                w.close(']');
            }
            w.close(']');
        });
        w.key("T");
        w.option(self.t.as_ref(), Json::term);
        w.key("bodies");
        w.open('[');
        for (b, out) in &self.bodies {
            w.item();
            w.open('[');
            w.item();
            w.body(b);
            w.item();
            w.term(out);
            w.close(']');
        }
        w.close(']');
        w.close('}');
        w.out
    }
}

/// The JSON of the oracle's `error`.
#[must_use]
pub fn error_json(error: &Error) -> String {
    let mut w = Json::default();
    w.open('{');
    w.key("exp");
    w.string(&error.exp);
    w.key("obs");
    w.option(error.obs.as_deref(), Json::string);
    w.key("at");
    w.option(error.at.as_ref(), |w, (ns, beg, end)| {
        w.open('[');
        w.item();
        w.string(ns);
        w.item();
        w.number(*beg);
        w.item();
        w.number(*end);
        w.close(']');
    });
    w.close('}');
    w.out
}

/// A JSON string as `JSON.stringify` writes it.
#[must_use]
pub fn json_string(s: &str) -> String {
    let mut w = Json::default();
    w.string(s);
    w.out
}

#[derive(Default)]
struct Json {
    out: String,
    first: Vec<bool>,
}

impl Json {
    fn open(&mut self, c: char) {
        self.out.push(c);
        self.first.push(true);
    }

    fn close(&mut self, c: char) {
        self.out.push(c);
        self.first.pop();
    }

    fn item(&mut self) {
        if let Some(first) = self.first.last_mut() {
            if !*first {
                self.out.push(',');
            }
            *first = false;
        }
    }

    fn key(&mut self, k: &str) {
        self.item();
        self.string(k);
        self.out.push(':');
    }

    fn null(&mut self) {
        self.out.push_str("null");
    }

    fn bool(&mut self, b: bool) {
        self.out.push_str(if b { "true" } else { "false" });
    }

    fn number(&mut self, n: impl std::fmt::Display) {
        self.out.push_str(&n.to_string());
    }

    fn string(&mut self, s: &str) {
        self.out.push('"');
        for c in s.chars() {
            match c {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\\\"),
                '\u{8}' => self.out.push_str("\\b"),
                '\u{c}' => self.out.push_str("\\f"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                c if u32::from(c) < 0x20 => {
                    let code = u32::from(c);
                    self.out.push_str("\\u00");
                    self.out.extend(char::from_digit(code >> 4, 16));
                    self.out.extend(char::from_digit(code & 15, 16));
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    fn option<T>(&mut self, value: Option<T>, write: impl FnOnce(&mut Self, T)) {
        match value {
            Some(value) => write(self, value),
            None => self.null(),
        }
    }

    fn span(&mut self, s: Option<Span>) {
        match s {
            Some(s) => {
                self.open('[');
                self.item();
                self.number(s.beg);
                self.item();
                self.number(s.end);
                self.close(']');
            }
            None => self.null(),
        }
    }

    fn quant(&mut self, q: Quant) {
        self.string(match q {
            Quant::None => "N",
            Quant::Lone => "L",
            Quant::Many => "M",
        });
    }

    fn strings(&mut self, xs: &[String]) {
        self.open('[');
        for x in xs {
            self.item();
            self.string(x);
        }
        self.close(']');
    }

    fn terms(&mut self, xs: &[std::rc::Rc<Term>]) {
        self.open('[');
        for x in xs {
            self.item();
            self.term(x);
        }
        self.close(']');
    }

    fn tele(&mut self, cells: &[Cell]) {
        self.open('[');
        for cell in cells {
            self.item();
            self.open('[');
            self.item();
            self.quant(cell.q);
            self.item();
            self.string(&cell.k);
            self.item();
            self.number(cell.i);
            self.item();
            self.term(&cell.t);
            self.item();
            self.span(cell.s);
            self.close(']');
        }
        self.close(']');
    }

    fn patt(&mut self, q: &Patt) {
        self.open('[');
        match q {
            Patt::Var(v) => {
                self.item();
                self.string("PVar");
                self.item();
                self.string(&v.k);
                self.item();
                self.number(v.i);
                self.item();
                self.quant(v.q);
                self.item();
                self.span(v.s);
            }
            Patt::Ctr { k, x, s } => {
                self.item();
                self.string("PCtr");
                self.item();
                self.string(k);
                self.item();
                self.open('[');
                for y in x {
                    self.item();
                    self.patt(y);
                }
                self.close(']');
                self.item();
                self.span(*s);
            }
        }
        self.close(']');
    }

    fn body(&mut self, b: &Body) {
        self.open('[');
        match b {
            Body::Match { e, r, s } => {
                self.item();
                self.string("Match");
                self.item();
                self.terms(e);
                self.item();
                self.open('[');
                for row in r {
                    self.item();
                    self.open('[');
                    self.item();
                    self.open('[');
                    for q in &row.p {
                        self.item();
                        self.patt(q);
                    }
                    self.close(']');
                    self.item();
                    self.body(&row.f);
                    self.close(']');
                }
                self.close(']');
                self.item();
                self.span(*s);
            }
            Body::Local { k, q, v, f } => {
                self.item();
                self.string("Local");
                self.item();
                self.open('[');
                for p in k {
                    self.item();
                    self.patt(p);
                }
                self.close(']');
                self.item();
                self.quant(*q);
                self.item();
                self.terms(v);
                self.item();
                self.body(f);
            }
            Body::Term(t) => {
                self.item();
                self.string("Term");
                self.item();
                self.term(t);
            }
        }
        self.close(']');
    }

    fn term(&mut self, t: &Term) {
        self.open('[');
        match t {
            Term::Var { k, i, s, v } => {
                self.fields("Var", |w| {
                    w.item();
                    w.string(k);
                    w.item();
                    w.number(i);
                    w.item();
                    w.span(*s);
                    w.item();
                    w.option(v.as_deref(), Json::term);
                });
            }
            Term::Ref { k, s, b } => self.fields("Ref", |w| {
                w.item();
                w.string(k);
                w.item();
                w.span(*s);
                w.item();
                w.bool(*b);
            }),
            Term::Sub { i, v, f, s } => self.fields("Sub", |w| {
                w.item();
                w.number(i);
                w.item();
                w.patt(v);
                w.item();
                w.term(f);
                w.item();
                w.span(*s);
            }),
            Term::Let { k, i, q, v, f, s } => self.fields("Let", |w| {
                w.item();
                w.strings(k);
                w.item();
                w.open('[');
                for x in i {
                    w.item();
                    w.number(x);
                }
                w.close(']');
                w.item();
                w.open('[');
                for x in q {
                    w.item();
                    w.quant(*x);
                }
                w.close(']');
                w.item();
                w.terms(v);
                w.item();
                w.term(f);
                w.item();
                w.span(*s);
            }),
            other => self.term_rest(other),
        }
        self.close(']');
    }

    fn term_rest(&mut self, t: &Term) {
        match t {
            Term::Var { .. } | Term::Ref { .. } | Term::Sub { .. } | Term::Let { .. } => {}
            Term::Typ { g, s } => self.fields("Typ", |w| {
                w.item();
                w.term(g);
                w.item();
                w.span(*s);
            }),
            Term::Qnt { s } => self.fields("Qnt", |w| {
                w.item();
                w.span(*s);
            }),
            Term::Qua { q, s } => self.fields("Qua", |w| {
                w.item();
                w.quant(*q);
                w.item();
                w.span(*s);
            }),
            Term::Min { a, b, s } => self.fields("Min", |w| {
                w.item();
                w.term(a);
                w.item();
                w.term(b);
                w.item();
                w.span(*s);
            }),
            Term::All { q, k, i, a, b, s } => self.fields("All", |w| {
                w.item();
                w.quant(*q);
                w.item();
                w.string(k);
                w.item();
                w.number(i);
                w.item();
                w.term(a);
                w.item();
                w.term(b);
                w.item();
                w.span(*s);
            }),
            Term::Lam { k, i, f, s, q } => self.fields("Lam", |w| {
                w.item();
                w.string(k);
                w.item();
                w.number(i);
                w.item();
                w.term(f);
                w.item();
                w.span(*s);
                w.item();
                w.option(*q, Json::quant);
            }),
            Term::App { f, x, s } => self.fields("App", |w| {
                w.item();
                w.term(f);
                w.item();
                w.term(x);
                w.item();
                w.span(*s);
            }),
            other => self.term_tail(other),
        }
    }

    fn term_tail(&mut self, t: &Term) {
        match t {
            Term::Var { .. }
            | Term::Ref { .. }
            | Term::Sub { .. }
            | Term::Let { .. }
            | Term::Typ { .. }
            | Term::Qnt { .. }
            | Term::Qua { .. }
            | Term::Min { .. }
            | Term::All { .. }
            | Term::Lam { .. }
            | Term::App { .. } => {}
            Term::Adt { k, x, r, s } => self.fields("ADT", |w| {
                w.item();
                w.string(k);
                w.item();
                w.terms(x);
                w.item();
                w.strings(r);
                w.item();
                w.span(*s);
            }),
            Term::Ctr { k, x, s } => self.fields("Ctr", |w| {
                w.item();
                w.string(k);
                w.item();
                w.terms(x);
                w.item();
                w.span(*s);
            }),
            Term::Lit { k, v, s } => self.fields("Lit", |w| {
                w.item();
                w.string(k);
                w.item();
                match v {
                    LitValue::Number(n) => w.number(n),
                    LitValue::String(text) => w.string(text),
                }
                w.item();
                w.span(*s);
            }),
            Term::Mat { k, h, m, s } => self.fields("Mat", |w| {
                w.item();
                w.string(k);
                w.item();
                w.term(h);
                w.item();
                w.term(m);
                w.item();
                w.span(*s);
            }),
            Term::Efq { s } => self.fields("Efq", |w| {
                w.item();
                w.span(*s);
            }),
            Term::Eql { a, b, t, s } => self.fields("Eql", |w| {
                w.item();
                w.term(a);
                w.item();
                w.term(b);
                w.item();
                w.term(t);
                w.item();
                w.span(*s);
            }),
            Term::Rfl { s } => self.fields("Rfl", |w| {
                w.item();
                w.span(*s);
            }),
            Term::Rwt { e, p, f, s } => self.fields("Rwt", |w| {
                w.item();
                w.term(e);
                w.item();
                w.term(p);
                w.item();
                w.term(f);
                w.item();
                w.span(*s);
            }),
            Term::Hol { k, s } => self.fields("Hol", |w| {
                w.item();
                w.string(k);
                w.item();
                w.span(*s);
            }),
            Term::Ann { x, t, s } => self.fields("Ann", |w| {
                w.item();
                w.term(x);
                w.item();
                w.term(t);
                w.item();
                w.span(*s);
            }),
        }
    }

    fn fields(&mut self, tag: &str, rest: impl FnOnce(&mut Self)) {
        self.item();
        self.string(tag);
        rest(self);
    }
}
