//! A port of the parser of the Bend compiler (`bend.ts`, "Parse"). Each
//! method does what the function of the same name does there.

use std::rc::Rc;

use super::{
    Error,
    book::{Adt, Book, CtrDecl, Def, Tld},
    flatten::{NAT_LITERAL_MAX, body_flatten, lit_step},
    number::{f32_round, to_f32},
    occurrence::{Binder, Event, Meaning, Recorded},
    term::{Body, Case, Cell, LitValue, PVar, Patt, Quant, Span, Term},
    trace::{TldTrace, TraceKind},
};

pub(super) const KEYWORDS: [&str; 16] = [
    "def", "type", "law", "match", "case", "do", "return", "for", "exs", "where", "is", "import",
    "Type", "Data", "Kind", "Quant",
];

const ESCAPES: [(u8, u32); 7] = [
    (b'n', 10),
    (b't', 9),
    (b'r', 13),
    (b'0', 0),
    (b'\\', 92),
    (b'\'', 39),
    (b'"', 34),
];

/// An infix operator: its spelling, precedence, right associativity and the
/// name it calls.
struct Infix {
    op: &'static str,
    precedence: i32,
    right: bool,
    name: &'static str,
}

const fn infix(op: &'static str, precedence: i32, right: bool, name: &'static str) -> Infix {
    Infix {
        op,
        precedence,
        right,
        name,
    }
}

const INFIX: [Infix; 23] = [
    infix("<-", -1, false, ""),
    infix("->", 0, true, ""),
    infix("&", 1, true, "Pair"),
    infix("|", 1, true, "Or"),
    infix("||", 2, false, "Bool.or"),
    infix("&&", 3, false, "Bool.and"),
    infix("<", 4, false, ".is_lt"),
    infix("<=", 4, false, ".is_le"),
    infix(">", 4, false, ".is_gt"),
    infix(">=", 4, false, ".is_ge"),
    infix("<>", 5, true, "Con"),
    infix("++", 5, true, "String.append"),
    infix("<&>", 5, true, ""),
    infix(".|.", 6, false, ".or"),
    infix(".^.", 7, false, ".xor"),
    infix(".&.", 8, false, ".and"),
    infix("<<", 9, false, ".shln"),
    infix(">>", 9, false, ".shrn"),
    infix("+", 10, false, ".add"),
    infix("-", 10, false, ".sub"),
    infix("*", 11, false, ".mul"),
    infix("/", 11, false, ".div"),
    infix("%", 11, false, ".mod"),
];

pub(super) type Result<T> = std::result::Result<T, Error>;

const U32_MAX: u64 = 0xffff_ffff;

fn byte(unit: Option<u16>) -> Option<u8> {
    unit.and_then(|unit| u8::try_from(unit).ok())
}

#[must_use]
pub fn char_is_head(unit: Option<u16>) -> bool {
    byte(unit).is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
}

#[must_use]
pub fn char_is_name(unit: Option<u16>) -> bool {
    byte(unit).is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

fn is_digit(unit: Option<u16>) -> bool {
    byte(unit).is_some_and(|b| b.is_ascii_digit())
}

/// `/\s/` of JavaScript on one code unit.
#[must_use]
pub fn is_js_space(unit: u16) -> bool {
    matches!(
        unit,
        0x09..=0x0d
            | 0x20
            | 0xa0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

/// `\w` of JavaScript on one code unit.
pub(super) fn is_word_unit(unit: u16) -> bool {
    byte(Some(unit)).is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub(super) fn text(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

fn starts_with_ascii(units: &[u16], ascii: &str) -> bool {
    units.len() >= ascii.len()
        && units
            .iter()
            .zip(ascii.bytes())
            .all(|(unit, b)| *unit == u16::from(b))
}

/// `name_key`: a key as output spells it.
pub(super) fn name_key(key: &str) -> String {
    key.replacen(':', ".", 1)
}

/// `parse_col`: the column of `pos`, from 1.
pub(super) fn parse_col(src: &[u16], pos: usize) -> usize {
    let from = pos.saturating_sub(1).min(src.len().saturating_sub(1));
    match (0..=from).rev().find(|i| src.get(*i) == Some(&10)) {
        Some(newline) => pos - newline,
        None => pos + 1,
    }
}

pub(super) struct Parse<'a> {
    pub book: &'a mut Book,
    pub str: &'a [u16],
    pub ns: &'a str,
    pub al: &'a [(String, String)],
    pub pos: usize,
    pub stk: Vec<(String, i64)>,
    pub frs: i64,
    pub tlds: &'a mut Vec<TldTrace>,
    pub events: &'a mut Vec<Recorded>,
}

impl Parse<'_> {
    // Occurrences

    fn record(&mut self, span: Option<Span>, event: Event, implicit: bool) {
        if let Some(span) = span {
            self.events.push(Recorded {
                span,
                event,
                implicit,
            });
        }
    }

    fn implicit(&mut self, key: &str, span: Option<Span>) {
        let meaning = Meaning::Global {
            key: key.to_owned(),
        };
        self.record(span, Event::Settled(meaning), true);
    }

    fn explicit(&mut self, span: Option<Span>, meaning: Meaning) {
        self.record(span, Event::Settled(meaning), false);
    }

    fn binder(&self, index: i64) -> Binder {
        Binder {
            tld: self.tlds.len().saturating_sub(1),
            index,
        }
    }

    /// The span of the name that the lexer just read.
    fn token(&self, name: &str) -> Span {
        Span {
            beg: self.pos - name.len(),
            end: self.pos,
        }
    }

    /// Give a new meaning to the name recorded at `span`.
    fn settle(&mut self, span: Option<Span>, event: Event) {
        let Some(span) = span else {
            return;
        };
        let found = self
            .events
            .iter_mut()
            .rev()
            .find(|record| record.span == span && !record.implicit);
        match found {
            Some(record) => record.event = event,
            None => self.record(Some(span), event, false),
        }
    }

    fn unit(&self, index: usize) -> Option<u16> {
        self.str.get(index).copied()
    }

    fn slice(&self, beg: usize, end: usize) -> &[u16] {
        let end = end.min(self.str.len());
        self.str.get(beg.min(end)..end).unwrap_or(&[])
    }

    fn col(&self, pos: usize) -> usize {
        parse_col(self.str, pos)
    }

    fn span(&self, beg: usize) -> Span {
        Span { beg, end: self.pos }
    }

    fn error(&self, exp: impl Into<String>, span: Option<Span>) -> Error {
        Error {
            exp: exp.into(),
            obs: None,
            at: span.map(|span| (self.ns.to_owned(), span.beg, span.end)),
        }
    }

    fn fail_at(&self, exp: impl Into<String>, beg: usize, end: usize) -> Error {
        let obs = if beg < end {
            format!("'{}'", text(self.slice(beg, end)))
        } else if self.pos < self.str.len() {
            format!("'{}'", text(self.slice(self.pos, self.pos + 1)))
        } else {
            "end of input".to_owned()
        };
        Error {
            exp: exp.into(),
            obs: Some(obs),
            at: Some((self.ns.to_owned(), beg, end)),
        }
    }

    fn fail(&self, exp: impl Into<String>) -> Error {
        self.fail_at(exp, self.pos, self.pos)
    }

    fn peek(&self) -> Option<u16> {
        self.unit(self.pos)
    }

    fn bump(&mut self) -> Option<u16> {
        let unit = self.peek();
        self.pos += 1;
        unit
    }

    fn at(&self, ascii: &str) -> bool {
        self.str
            .get(self.pos..)
            .is_some_and(|rest| starts_with_ascii(rest, ascii))
    }

    fn take(&mut self, ascii: &str) -> bool {
        if !self.at(ascii) {
            return false;
        }
        self.pos += ascii.len();
        true
    }

    fn skip(&mut self) {
        while let Some(unit) = self.unit(self.pos) {
            if matches!(unit, 32 | 10 | 13 | 9) {
                self.pos += 1;
                continue;
            }
            if unit == 35 {
                while self.unit(self.pos).is_some_and(|unit| unit != 10) {
                    self.pos += 1;
                }
                continue;
            }
            return;
        }
    }

    fn eat(&mut self, ascii: &str) -> Result<()> {
        self.skip();
        if !self.take(ascii) {
            return Err(self.fail(format!("'{ascii}'")));
        }
        Ok(())
    }

    fn at_word(&mut self, word: &str) -> bool {
        self.skip();
        self.at(word) && !char_is_name(self.unit(self.pos + word.len()))
    }

    fn word(&mut self, word: &str) -> bool {
        if !self.at_word(word) {
            return false;
        }
        self.take(word);
        true
    }

    fn lexeme(&mut self) -> Result<String> {
        self.skip();
        if !char_is_head(self.peek()) {
            return Err(self.fail("a name"));
        }
        let beg = self.pos;
        while self.pos < self.str.len() && char_is_name(self.unit(self.pos)) {
            self.pos += 1;
        }
        let name = text(self.slice(beg, self.pos));
        let valid = name.split('.').all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|head| head.is_ascii_alphabetic() || head == '_')
                && chars.all(|rest| rest.is_ascii_alphanumeric() || rest == '_')
        });
        if !valid {
            let exp = format!("a name (words joined by dots, got '{name}')");
            return Err(self.fail_at(exp, beg, self.pos));
        }
        Ok(name)
    }

    fn name(&mut self) -> Result<String> {
        let name = self.lexeme()?;
        if KEYWORDS.contains(&name.as_str()) {
            let beg = self.pos - name.len();
            let exp = format!("a name (got the keyword '{name}')");
            return Err(self.fail_at(exp, beg, self.pos));
        }
        Ok(name)
    }

    fn char(&mut self) -> Result<u32> {
        if self.take("\\") {
            if let Some((code, len)) = self.unicode_escape() {
                self.pos += len;
                return Ok(code);
            }
            let escaped = byte(self.bump());
            return match ESCAPES.iter().find(|(key, _)| Some(*key) == escaped) {
                Some((_, code)) => Ok(*code),
                None => Err(self.fail(r#"an escape (\n \t \r \0 \\ \' \" \u{1F600})"#)),
            };
        }
        let Some(code) = self.code_point_at(self.pos) else {
            return Err(self.fail("a character"));
        };
        self.pos += if code > 0xffff { 2 } else { 1 };
        Ok(code)
    }

    /// `/^u\{([0-9a-f]+)\}/i` on the next 11 code units.
    fn unicode_escape(&self) -> Option<(u32, usize)> {
        let window = self.slice(self.pos, self.pos + 11);
        if !matches!(byte(window.first().copied()), Some(b'u' | b'U'))
            || byte(window.get(1).copied()) != Some(b'{')
        {
            return None;
        }
        let digits: Vec<u16> = window
            .iter()
            .skip(2)
            .take_while(|unit| byte(Some(**unit)).is_some_and(|b| b.is_ascii_hexdigit()))
            .copied()
            .collect();
        if digits.is_empty() || byte(window.get(2 + digits.len()).copied()) != Some(b'}') {
            return None;
        }
        let code = u64::from_str_radix(&text(&digits), 16).ok()?;
        Some((u32::try_from(code).ok()?, digits.len() + 3))
    }

    fn code_point_at(&self, index: usize) -> Option<u32> {
        let first = self.unit(index)?;
        if (0xd800..0xdc00).contains(&first)
            && let Some(second) = self.unit(index + 1)
            && (0xdc00..0xe000).contains(&second)
        {
            let high = (u32::from(first) - 0xd800) << 10;
            return Some(0x10000 + high + (u32::from(second) - 0xdc00));
        }
        Some(u32::from(first))
    }

    // Binders

    fn fresh(&mut self) -> i64 {
        let index = self.frs;
        self.frs += 1;
        index
    }

    fn open(&mut self, name: &str) -> i64 {
        let index = self.fresh();
        if name != "_" {
            self.stk.push((name.to_owned(), index));
        }
        index
    }

    fn close(&mut self, len: usize) {
        self.stk.truncate(len);
    }

    fn var(&mut self, name: &str, span: Option<Span>, implicit: bool) -> Result<Term> {
        if let Some((_, index)) = self.stk.iter().rev().find(|(bound, _)| bound == name) {
            let index = *index;
            let local = Meaning::Local(self.binder(index));
            self.record(span, Event::Settled(local), implicit);
            return Ok(Term::var(name, index, span));
        }
        let key = self.reso(name)?;
        if name.contains('.') {
            let global = Meaning::Global { key: key.clone() };
            self.record(span, Event::Settled(global), implicit);
            return Ok(Term::reference(key, span));
        }
        self.record(span, Event::Fallback { key: key.clone() }, implicit);
        let index = self.fresh();
        Ok(Term::Var {
            k: name.to_owned(),
            i: index,
            s: span,
            v: Some(Rc::new(Term::reference(key, span))),
        })
    }

    fn qual(&self, name: &str) -> String {
        if self.ns.is_empty() {
            name.to_owned()
        } else {
            format!("{}:{name}", self.ns)
        }
    }

    fn alias(&self, alias: &str) -> Option<&str> {
        self.al
            .iter()
            .find(|(name, _)| name == alias)
            .map(|(_, sub)| sub.as_str())
    }

    fn reso(&self, name: &str) -> Result<String> {
        let mut key = self.qual(name);
        if let Some(dot) = name.find('.')
            && let Some(sub) = self.alias(&name[..dot])
        {
            key = format!("{sub}:{}", &name[dot + 1..]);
            if key != name && self.book.declares(&key) && self.book.declares(name) {
                let exp = format!(
                    "an unambiguous name (the alias {} shadows {name})",
                    &name[..dot]
                );
                return Err(self.fail(exp));
            }
        }
        let own = self.book.declares(&key);
        let far = self.book.declares(name);
        Ok(if own || !far { key } else { name.to_owned() })
    }

    fn call(&mut self, name: &str, args: Vec<Term>, span: Option<Span>) -> Result<Term> {
        let key = self.reso(name)?;
        self.implicit(&key, span);
        let head = Term::reference(key, span);
        Ok(args
            .into_iter()
            .fold(head, |fun, arg| Term::app(fun, arg, span)))
    }

    fn quant(&mut self) -> Quant {
        self.skip();
        if self.take("-") {
            return Quant::None;
        }
        if self.take("+") {
            return Quant::Many;
        }
        Quant::Lone
    }

    // Patt

    fn bind(&mut self, term: &Term) -> Result<PVar> {
        let Term::Var { k, i, s, .. } = term else {
            return Err(self.fail("a lambda binder (one name: k => body)"));
        };
        let quant = if *i < 0 { Quant::Many } else { Quant::Lone };
        let index = self.open(k);
        let binder = Event::Settled(Meaning::Binder(self.binder(index)));
        if let Some(span) = *s {
            let plus = self
                .events
                .iter_mut()
                .rev()
                .find(|record| matches!(record.event, Event::Plus { outer } if outer == span));
            match plus {
                Some(record) => record.event = binder,
                None => self.settle(Some(span), binder),
            }
        }
        Ok(PVar {
            k: k.clone(),
            i: index,
            q: quant,
            s: *s,
        })
    }

    fn patt(&mut self, term: &Term) -> Result<Patt> {
        match term {
            Term::Var { k, s, .. } => {
                if self.book.ctrs.contains_key(&self.reso(k)?) {
                    let key = name_key(k);
                    let exp = format!(
                        "a braced constructor pattern ({key} is a constructor: write {key}{{}}, or rename the binder)"
                    );
                    return Err(self.error(exp, *s));
                }
                Ok(Patt::Var(self.bind(term)?))
            }
            Term::Ctr { k, x, s } => {
                let Some(ctr) = self.book.ctrs.get(k) else {
                    let exp = format!("a declared constructor (unknown: {})", name_key(k));
                    return Err(self.error(exp, *s));
                };
                if ctr.n != x.len() {
                    let fields = if ctr.n == 1 { " field" } else { " fields" };
                    let exp = format!("a {} pattern with {}{fields}", name_key(k), ctr.n);
                    return Err(self.error(exp, *s));
                }
                let mut fields = Vec::with_capacity(x.len());
                for field in x {
                    fields.push(Rc::new(self.patt(field)?));
                }
                Ok(Patt::Ctr {
                    k: k.clone(),
                    x: fields,
                    s: *s,
                })
            }
            Term::Lit { s, .. } => {
                let stepped = lit_step(term);
                let mut keys = Vec::new();
                literal_ctrs(&stepped, &mut keys);
                for key in keys {
                    self.implicit(&key, *s);
                }
                self.patt(&stepped)
            }
            _ => Err(Error {
                exp: "a pattern (a binder or a constructor)".to_owned(),
                obs: Some("<term>".to_owned()),
                at: term
                    .span()
                    .map(|span| (self.ns.to_owned(), span.beg, span.end)),
            }),
        }
    }

    // Term

    fn term(&mut self, level: i32) -> Result<Term> {
        self.skip();
        let beg = self.pos;
        let mut base = self.term_base(beg)?;
        if base.span().is_none() {
            *base.span_mut() = Some(self.span(beg));
        }
        let start = base.span().map_or(beg, |span| span.beg);
        self.term_ops(base, start, level)
    }

    fn term_base(&mut self, beg: usize) -> Result<Term> {
        let head = self.peek();
        if char_is_head(head) {
            return self.term_word(beg);
        }
        if is_digit(head) {
            return self.term_num();
        }
        match byte(head) {
            Some(b'@') => self.term_all(false),
            Some(b'&') => {
                let quant = match byte(self.unit(self.pos + 1)) {
                    Some(b'0') => Some(Quant::None),
                    Some(b'1') => Some(Quant::Lone),
                    Some(b'2') => Some(Quant::Many),
                    _ => None,
                };
                let Some(quant) = quant else {
                    return self.term_all(true);
                };
                self.bump();
                self.bump();
                Ok(Term::Qua { q: quant, s: None })
            }
            Some(b'+') => self.term_plus(beg),
            Some(b'\\') => self.term_match(beg),
            Some(b'%') => self.term_rewrite(beg),
            Some(b'{') => self.term_brace(beg),
            Some(b'(') => {
                self.bump();
                self.term_tup(beg)
            }
            Some(b'[') => self.term_list(beg),
            Some(b'\'') => self.term_char(beg),
            Some(b'"') => self.term_string(beg),
            Some(b'?') => {
                self.bump();
                let name = self.name()?;
                if name == "TODO" {
                    self.book.hols += 1;
                }
                Ok(Term::Hol { k: name, s: None })
            }
            _ => Err(self.fail("a term")),
        }
    }

    fn term_word(&mut self, beg: usize) -> Result<Term> {
        let word = self.lexeme()?;
        let kind = |level: Quant| Term::Typ {
            g: Rc::new(Term::Qua { q: level, s: None }),
            s: None,
        };
        match word.as_str() {
            "Type" => return Ok(kind(Quant::Lone)),
            "Data" => return Ok(kind(Quant::Many)),
            "Quant" => return Ok(Term::Qnt { s: None }),
            "Kind" => {
                self.eat("(")?;
                let level = self.term(0)?;
                self.eat(")")?;
                return Ok(Term::Typ {
                    g: Rc::new(level),
                    s: None,
                });
            }
            "do" => return self.term_do(beg),
            "match" => {
                let exp = "a term (a match heads a def body, not a term)";
                return Err(self.fail_at(exp, beg, self.pos));
            }
            "case" => {
                let exp = "a match heading this case (this case is orphaned)";
                return Err(self.fail_at(exp, beg, self.pos));
            }
            "return" => {
                return Err(self.fail_at("a do-block heading this return", beg, self.pos));
            }
            _ => {}
        }
        if KEYWORDS.contains(&word.as_str()) {
            let exp = format!("a term (the keyword '{word}' cannot head one)");
            return Err(self.fail_at(exp, beg, self.pos));
        }
        let token = Some(Span {
            beg,
            end: beg + word.len(),
        });
        if self.take("{") {
            let args = self.term_args("}")?;
            let key = self.reso(&word)?;
            self.explicit(token, Meaning::Global { key: key.clone() });
            return Ok(Term::ctr(key, args, None));
        }
        let span = Some(self.span(beg));
        self.var(&word, span, false)
    }

    fn term_do(&mut self, beg: usize) -> Result<Term> {
        let monad = self.name()?;
        let token = Some(self.token(&monad));
        self.eat("<")?;
        let family = self.reso(&monad)?;
        self.explicit(
            token,
            Meaning::Global {
                key: family.clone(),
            },
        );
        let args = self.term_args(">")?;
        let span = Some(self.span(beg));
        let filled = self.fill(&family, args, span);
        self.eat(":")?;
        self.skip();
        let col = self.col(self.pos);
        self.do_stmt(&monad, &filled, col)
    }

    fn term_char(&mut self, beg: usize) -> Result<Term> {
        self.bump();
        let code = self.char()?;
        if !self.take("'") {
            return Err(self.fail("a closing '"));
        }
        let span = Some(self.span(beg));
        let value = Term::number("U32", u64::from(code), span);
        self.implicit("Chr", span);
        Ok(Term::ctr("Chr", vec![value], span))
    }

    fn term_string(&mut self, beg: usize) -> Result<Term> {
        self.bump();
        let mut codes = Vec::new();
        while !self.take("\"") {
            if self.pos >= self.str.len() {
                return Err(self.fail("a closing \""));
            }
            codes.push(self.char()?);
        }
        let span = Some(self.span(beg));
        let literal = lit_of(&codes, span);
        if let Term::Ctr { .. } = literal {
            for key in ["SCon", "Chr", "SNil"] {
                self.implicit(key, span);
            }
        }
        Ok(literal)
    }

    fn term_plus(&mut self, beg: usize) -> Result<Term> {
        self.bump();
        let term = self.term(12)?;
        let span = Some(self.span(beg));
        let key = match &term {
            Term::Adt { k, .. } => k.clone(),
            Term::Var { k, .. } | Term::Ref { k, .. } => self.reso(k)?,
            _ => String::new(),
        };
        let family = self.book.adt(&key).copied();
        if let Term::Var { k, .. } = &term
            && family.is_none()
        {
            let outer = self.span(beg);
            self.settle(term.span(), Event::Plus { outer });
            return Ok(Term::var(k.clone(), -1, span));
        }
        if !matches!(term, Term::Adt { .. }) {
            let meaning = Meaning::Global { key: key.clone() };
            self.settle(term.span(), Event::Settled(meaning));
        }
        let is_adt = matches!(term, Term::Adt { .. });
        let Some(family) =
            family.filter(|family| family.g != 0 && (family.g >= family.n || is_adt))
        else {
            return Err(self
                .fail("a quantified datatype after + (+D<..> sets D's leading quantities to &2)"));
        };
        let args = match term {
            Term::Adt { x, .. } => x,
            _ => (0..family.n)
                .map(|_| {
                    Rc::new(Term::Qua {
                        q: Quant::Lone,
                        s: span,
                    })
                })
                .collect(),
        };
        let args = args
            .into_iter()
            .enumerate()
            .map(|(index, arg)| {
                if index < family.g {
                    Rc::new(Term::Qua {
                        q: Quant::Many,
                        s: span,
                    })
                } else {
                    arg
                }
            })
            .collect();
        Ok(Term::Adt {
            k: key,
            x: args,
            r: Vec::new(),
            s: span,
        })
    }

    fn term_match(&mut self, beg: usize) -> Result<Term> {
        self.bump();
        self.eat("{")?;
        let mut arms: Vec<(String, Term)> = Vec::new();
        let mut tail = Term::Efq { s: None };
        loop {
            self.skip();
            if self.take("}") {
                break;
            }
            let term = self.term(0)?;
            self.skip();
            if let Term::Var { k, .. } | Term::Ref { k, .. } = &term
                && self.take(":")
            {
                let arm = self.term(0)?;
                let key = self.reso(k)?;
                let meaning = Meaning::Global { key: key.clone() };
                self.settle(term.span(), Event::Settled(meaning));
                arms.push((key, arm));
                self.skip();
                self.take(";");
                continue;
            }
            tail = term;
            self.skip();
            self.take(";");
            self.eat("}")?;
            break;
        }
        let span = Some(self.span(beg));
        if tail.span().is_none() {
            *tail.span_mut() = span;
        }
        Ok(arms
            .into_iter()
            .rev()
            .fold(tail, |out, (ctr, arm)| Term::Mat {
                k: ctr,
                h: Rc::new(arm),
                m: Rc::new(out),
                s: span,
            }))
    }

    fn term_rewrite(&mut self, beg: usize) -> Result<Term> {
        self.bump();
        let first = self.term(0)?;
        self.skip();
        let mut binder = String::new();
        let mut evidence = first.clone();
        let named = self.take("@");
        if named {
            let Term::Var { k, .. } = &first else {
                return Err(self.fail("a name before @ (a rewrite binder is one name: %e@E : P)"));
            };
            binder.clone_from(k);
            evidence = self.term(0)?;
        }
        self.eat(":")?;
        let depth = self.stk.len();
        let hole = self.fresh();
        self.stk.push(("_".to_owned(), hole));
        let index = self.open(&binder);
        if named {
            let meaning = Meaning::Binder(self.binder(index));
            self.settle(first.span(), Event::Settled(meaning));
        }
        let motive = self.term(0)?;
        self.close(depth);
        self.skip();
        self.take(";");
        let rest = self.block()?;
        let span = Some(self.span(beg));
        let inner = Term::lam(binder, index, motive, first.span(), None);
        Ok(Term::Rwt {
            e: Rc::new(evidence),
            p: Rc::new(Term::lam("_", hole, inner, span, None)),
            f: Rc::new(rest),
            s: span,
        })
    }

    fn term_brace(&mut self, beg: usize) -> Result<Term> {
        self.bump();
        self.skip();
        if self.take("==") {
            self.eat("}")?;
            return Ok(Term::Rfl { s: None });
        }
        let left = self.term(0)?;
        self.skip();
        let unequal = self.take("!=");
        let operator = Some(Span {
            beg: self.pos.saturating_sub(2),
            end: self.pos,
        });
        let right = if unequal || self.take("==") {
            Some(self.term(0)?)
        } else {
            None
        };
        self.eat(":")?;
        let ty = self.term(0)?;
        self.eat("}")?;
        let Some(right) = right else {
            return Ok(Term::Ann {
                x: Rc::new(left),
                t: Rc::new(ty),
                s: None,
            });
        };
        let equal = |span| Term::Eql {
            a: Rc::new(left),
            b: Rc::new(right),
            t: Rc::new(ty),
            s: span,
        };
        if !unequal {
            return Ok(equal(None));
        }
        let span = Some(self.span(beg));
        let index = self.open("_");
        self.implicit("Empty", operator);
        let empty = Term::reference("Empty", operator);
        Ok(Term::all(Quant::Lone, "_", index, equal(span), empty, span))
    }

    fn term_list(&mut self, beg: usize) -> Result<Term> {
        self.bump();
        self.skip();
        let mut items = if self.at("]") {
            Vec::new()
        } else {
            vec![self.term(0)?]
        };
        self.skip();
        if !items.is_empty() && self.take(":") {
            return self.term_array(beg, items.swap_remove(0));
        }
        self.take(",");
        items.extend(self.term_args("]")?);
        let span = Some(self.span(beg));
        self.implicit("Nil", span);
        if !items.is_empty() {
            self.implicit("Con", span);
        }
        Ok(items
            .into_iter()
            .rev()
            .fold(Term::ctr("Nil", Vec::new(), span), |tail, item| {
                Term::ctr("Con", vec![item, tail], span)
            }))
    }

    fn term_array(&mut self, beg: usize, mut item: Term) -> Result<Term> {
        let ty = self.term(12)?;
        self.skip();
        let count = self.take("*");
        if !count {
            self.eat("^")?;
        }
        let size = self.term(0)?;
        self.eat("]")?;
        let span = Some(self.span(beg));
        self.term_ns(&mut item, &ty)?;
        let mut depth = size;
        if count {
            let total = nat_from_term(&depth).unwrap_or(0);
            if !total.is_power_of_two() {
                let exp = "a power of two count (^d takes a depth)";
                return Err(self.error(exp, depth.span()));
            }
            depth = Term::number("Nat", u64::from(total.trailing_zeros()), depth.span());
        }
        self.implicit("Array.new", span);
        let head = Term::reference("Array.new", span);
        let typed = Term::app(Term::app(head, ty, span), depth, span);
        Ok(Term::app(typed, item, span))
    }

    fn nl(&self) -> bool {
        let mut index = self.pos;
        while index > 0 {
            index -= 1;
            match self.unit(index) {
                Some(10) => return true,
                Some(32 | 13 | 9) => {}
                _ => return false,
            }
        }
        true
    }

    fn term_ops(&mut self, term: Term, beg: usize, level: i32) -> Result<Term> {
        let mut out = term;
        loop {
            self.skip();
            if self.nl() && (self.at("(") || self.at("[")) {
                return Ok(out);
            }
            if self.at("(") || self.at("!(") {
                out = self.term_call(out, beg)?;
                continue;
            }
            if self.at("[") {
                out = self.term_index(out, beg)?;
                continue;
            }
            if level == 0 && self.take("=>") {
                let depth = self.stk.len();
                let binder = self.bind(&out)?;
                let body = self.block()?;
                self.close(depth);
                out = Term::lam(binder.k, binder.i, body, binder.s, Some(binder.q));
                continue;
            }
            match self.term_infix(out, beg, level)? {
                Ok(next) => out = next,
                Err(done) => return Ok(done),
            }
        }
    }

    fn term_call(&mut self, term: Term, beg: usize) -> Result<Term> {
        let mut out = term;
        if let Term::Var {
            v: Some(fallback), ..
        } = out
        {
            out = Rc::unwrap_or_clone(fallback);
        }
        if self.at("!") {
            let Term::Ref { b, .. } = &mut out else {
                return Err(self.fail("a named def before ! (only f!(..) offloads)"));
            };
            *b = true;
            self.bump();
            return Ok(out);
        }
        self.bump();
        let templates = match &out {
            Term::Ref { k, .. } => self.book.def(k).map_or(0, |def| def.x),
            _ => 0,
        };
        let mut args = Vec::new();
        self.skip();
        while templates > 0 && self.at("~") {
            if args.len() == templates {
                let name = match &out {
                    Term::Ref { k, .. } => name_key(k),
                    _ => String::new(),
                };
                return Err(self.fail(format!("a term ({name} takes {templates} ~)")));
            }
            self.bump();
            args.push(self.term(0)?);
            self.skip();
            self.take(",");
            self.skip();
        }
        args.extend(self.term_args(")")?);
        let span = Some(self.span(beg));
        Ok(args
            .into_iter()
            .fold(out, |fun, arg| Term::app(fun, arg, span)))
    }

    fn term_index(&mut self, array: Term, beg: usize) -> Result<Term> {
        self.bump();
        let mut index = self.term(0)?;
        self.eat("]")?;
        let span = Some(self.span(beg));
        self.term_ns(&mut index, &Term::reference("U32", span))?;
        self.skip();
        let word = Term::reference("U32", span);
        self.implicit("U32", span);
        if !self.nl() && self.take("<-") {
            self.implicit("Array.set", span);
            let value = self.term(2)?;
            let head = Term::app(Term::reference("Array.set", span), word, span);
            let written = Term::app(Term::app(head, array, span), index, span);
            return Ok(Term::app(written, value, span));
        }
        self.implicit("Array.get", span);
        let head = Term::app(Term::reference("Array.get", span), word, span);
        Ok(Term::app(Term::app(head, array, span), index, span))
    }

    /// One infix operator after `out`. `Ok(Err(out))` means that no operator
    /// applies here, so the term ends.
    fn term_infix(
        &mut self,
        out: Term,
        beg: usize,
        level: i32,
    ) -> Result<std::result::Result<Term, Term>> {
        let window = self.slice(self.pos, self.pos + 3);
        let found = (1..=window.len()).rev().find_map(|len| {
            let spelled = &window[..len];
            INFIX
                .iter()
                .find(|infix| infix.op.len() == len && starts_with_ascii(spelled, infix.op))
        });
        let Some(infix) = found else {
            return Ok(Err(out));
        };
        let op = infix.op;
        let next = self.unit(self.pos + op.len());
        let glued = self
            .pos
            .checked_sub(1)
            .and_then(|index| self.unit(index))
            .is_some_and(|unit| !is_js_space(unit));
        if (infix.precedence < level && !(op == "<" && glued))
            || ((op == "-" || op == "+") && (byte(next) == Some(b'>') || char_is_head(next)))
            || (op.starts_with('>') && glued)
            || (op == "%" && !next.is_some_and(is_js_space))
        {
            return Ok(Err(out));
        }
        self.pos += op.len();
        let operator = Some(Span {
            beg: self.pos - op.len(),
            end: self.pos,
        });
        let precedence = if infix.right {
            infix.precedence
        } else {
            infix.precedence + 1
        };
        let rhs = self.term(precedence)?;
        let span = Some(self.span(beg));
        self.skip();
        if op == "<" && glued && self.type_operator_ahead() {
            return Err(self.fail("'>' or ',' (a compound type argument takes parens: F<(A & B)>)"));
        }
        if op == "<" && (self.at(">") || self.at(",")) {
            return self.term_family(&out, rhs, span).map(Ok);
        }
        Ok(Ok(match op {
            "->" => {
                let index = self.open("_");
                Term::all(Quant::Lone, "_", index, out, rhs, span)
            }
            "<&>" => Term::Min {
                a: Rc::new(out),
                b: Rc::new(rhs),
                s: span,
            },
            "<>" => {
                self.implicit(infix.name, span);
                Term::ctr(infix.name, vec![out, rhs], span)
            }
            "&" | "|" => {
                self.implicit(infix.name, span);
                let head = Term::reference(infix.name, span);
                Term::app(Term::app(head, out, span), rhs, span)
            }
            _ => {
                if infix.name.starts_with('.') {
                    self.record(operator, Event::Operator, true);
                } else {
                    self.implicit(infix.name, operator);
                }
                let head = Term::reference(infix.name, operator);
                Term::app(Term::app(head, out, span), rhs, span)
            }
        }))
    }

    fn term_family(&mut self, out: &Term, first: Term, span: Option<Span>) -> Result<Term> {
        let (Term::Var { k, .. } | Term::Ref { k, .. }) = out else {
            return Err(self.fail("a family name before <..> (a comparison here needs parens)"));
        };
        let name = k.clone();
        let head = out.span();
        self.take(",");
        let family = self.reso(&name)?;
        let meaning = Meaning::Global {
            key: family.clone(),
        };
        self.settle(head, Event::Settled(meaning));
        let mut args = vec![first];
        args.extend(self.term_args(">")?);
        let filled = self.fill(&family, args, span);
        Ok(Term::Adt {
            k: family,
            x: shared(filled),
            r: Vec::new(),
            s: span,
        })
    }

    fn type_operator_ahead(&self) -> bool {
        let first = byte(self.unit(self.pos));
        let second = byte(self.unit(self.pos + 1));
        (first == Some(b'-') && second == Some(b'>'))
            || (matches!(first, Some(b'&' | b'|')) && !matches!(second, Some(b'&' | b'|')))
    }

    fn term_ns(&mut self, term: &mut Term, ty: &Term) -> Result<()> {
        if let Term::Let { f, .. } = term {
            return self.term_ns(Rc::make_mut(f), ty);
        }
        let (head, _) = term.unapply();
        let Term::Ref { k, .. } = head else {
            return Ok(());
        };
        if k.rfind('.') == Some(0) {
            let (family, _) = ty.unapply();
            let (Term::Var { k: name, .. } | Term::Ref { k: name, .. } | Term::Adt { k: name, .. }) =
                family
            else {
                let exp = "a type name after : (the operators' namespace)";
                return Err(self.error(exp, family.span()));
            };
            let key = self.reso(&format!("{name}{k}"))?;
            let operator = head.span();
            if let Some(record) = self
                .events
                .iter_mut()
                .rev()
                .find(|record| Some(record.span) == operator && record.event == Event::Operator)
            {
                record.event = Event::Settled(Meaning::Global { key: key.clone() });
            }
            if let Term::Ref { k, .. } = head_mut(term) {
                *k = key;
            }
        } else if k != "Bool.and" && k != "Bool.or" && k != "String.append" {
            return Ok(());
        }
        self.term_ns_args(term, ty)
    }

    fn term_ns_args(&mut self, term: &mut Term, ty: &Term) -> Result<()> {
        if let Term::App { f, x, .. } = term {
            self.term_ns_args(Rc::make_mut(f), ty)?;
            self.term_ns(Rc::make_mut(x), ty)?;
        }
        Ok(())
    }

    fn term_args(&mut self, close: &str) -> Result<Vec<Term>> {
        let mut args = Vec::new();
        loop {
            self.skip();
            if self.take(close) {
                return Ok(args);
            }
            args.push(self.term(0)?);
            self.skip();
            self.take(",");
        }
    }

    fn term_all(&mut self, exists: bool) -> Result<Term> {
        let beg = self.pos;
        self.bump();
        let quant = if exists { Quant::Lone } else { self.quant() };
        let name = self.name()?;
        let token = Some(self.token(&name));
        self.eat(":")?;
        let domain = self.term(1)?;
        self.eat("->")?;
        let depth = self.stk.len();
        let index = self.open(&name);
        self.explicit(token, Meaning::Binder(self.binder(index)));
        let codomain = self.term(0)?;
        self.close(depth);
        if exists {
            let span = Some(self.span(beg));
            self.implicit("Exists", span);
            let head = Term::app(Term::reference("Exists", span), domain, span);
            return Ok(Term::app(
                head,
                Term::lam(name, index, codomain, span, None),
                span,
            ));
        }
        Ok(Term::all(quant, name, index, domain, codomain, None))
    }

    fn term_tup(&mut self, beg: usize) -> Result<Term> {
        self.skip();
        let col = self.col(self.pos).saturating_sub(1);
        let body = self.body(col)?;
        self.skip();
        if let Body::Term(first) = &body
            && self.take(",")
        {
            let rest = self.term_tup(beg)?;
            let span = Some(self.span(beg));
            self.implicit("Tuple", span);
            return Ok(Term::ctr("Tuple", vec![(**first).clone(), rest], span));
        }
        let mut out = self.flatten(&body, Vec::new())?;
        self.trace_body(&body, &out);
        if self.take(":") {
            let ty = self.term(0)?;
            self.term_ns(&mut out, &ty)?;
        }
        self.eat(")")?;
        Ok(out)
    }

    fn term_num(&mut self) -> Result<Term> {
        let beg = self.pos;
        let mut end = beg;
        while is_digit(self.unit(end)) {
            end += 1;
        }
        let digits = text(self.slice(beg, end));
        let mut stop = end;
        let mut float = false;
        let mut nat = false;
        if byte(self.unit(end)) == Some(b'n') {
            nat = true;
            stop = end + 1;
        } else if byte(self.unit(end)) == Some(b'.') && is_digit(self.unit(end + 1)) {
            float = true;
            stop = self.float_end(end + 1);
        }
        let number = u32_value(&digits);
        self.pos = stop;
        if float {
            let literal = text(self.slice(beg, stop));
            let value = f32_round(&literal);
            if !value.is_finite() {
                let exp = format!("a float literal with a finite f32 value (got {literal})");
                return Err(self.fail_at(exp, beg, self.pos));
            }
            return Ok(Term::number(
                "F32",
                u64::from(to_f32(value).to_bits()),
                None,
            ));
        }
        if !nat {
            if char_is_name(self.peek()) {
                return Err(self.fail("a numeric literal (NUMBER is U32, NUMBER n is Nat)"));
            }
            let Some(number) = number else {
                let exp = format!("a u32 literal up to 4294967295 (got {digits})");
                return Err(self.fail_at(exp, beg, self.pos));
            };
            return Ok(Term::number("U32", number, None));
        }
        let Some(number) = number else {
            return Err(self.fail(format!("a nat literal up to 4294967295n (got {digits}n)")));
        };
        if self.take("+") {
            return self.term_succ(beg, number);
        }
        if char_is_name(self.peek()) {
            return Err(self.fail("a nat literal (NUMBER n)"));
        }
        Ok(Term::number("Nat", number, None))
    }

    /// The end of `\d+([eE][+-]?\d+)?` from `index`.
    fn float_end(&self, index: usize) -> usize {
        let mut end = index;
        while is_digit(self.unit(end)) {
            end += 1;
        }
        if matches!(byte(self.unit(end)), Some(b'e' | b'E')) {
            let mut exponent = end + 1;
            if matches!(byte(self.unit(exponent)), Some(b'+' | b'-')) {
                exponent += 1;
            }
            if is_digit(self.unit(exponent)) {
                while is_digit(self.unit(exponent)) {
                    exponent += 1;
                }
                end = exponent;
            }
        }
        end
    }

    fn term_succ(&mut self, beg: usize, number: u64) -> Result<Term> {
        let mut out = self.term(0)?;
        let span = Some(self.span(beg));
        if let Term::Lit {
            k: "Nat",
            v: LitValue::Number(value),
            ..
        } = out
            && number + value <= U32_MAX
        {
            return Ok(Term::number("Nat", number + value, span));
        }
        if number > NAT_LITERAL_MAX {
            self.implicit("Nat.add", span);
            let head = Term::app(
                Term::reference("Nat.add", span),
                Term::number("Nat", number, span),
                span,
            );
            return Ok(Term::app(head, out, span));
        }
        if number > 0 {
            self.implicit("Succ", span);
        }
        for _ in 0..number {
            out = Term::ctr("Succ", vec![out], span);
        }
        Ok(out)
    }

    fn fill(&self, family: &str, args: Vec<Term>, span: Option<Span>) -> Vec<Term> {
        match self.book.adt(family) {
            Some(adt) if args.len() + adt.g == adt.n => {
                let mut out: Vec<Term> = (0..adt.g)
                    .map(|_| Term::Qua {
                        q: Quant::Lone,
                        s: span,
                    })
                    .collect();
                out.extend(args);
                out
            }
            _ => args,
        }
    }

    fn do_stmt(&mut self, monad: &str, args: &[Term], col: usize) -> Result<Term> {
        self.skip();
        let beg = self.pos;
        if self.word("return") {
            let value = self.term(0)?;
            let mut all = args.to_vec();
            all.push(value);
            let span = Some(self.span(beg));
            return self.call(&format!("{monad}.pure"), all, span);
        }
        let first = self.term(0)?;
        self.skip();
        let typed = matches!(first, Term::Var { .. }) && self.take(":");
        let step = !typed && self.more(col);
        let ty = if typed {
            self.term(1)?
        } else if step {
            self.var("Unit", first.span(), true)?
        } else {
            first.clone()
        };
        self.skip();
        let assign = typed && !self.at("==") && self.take("=");
        if typed && !assign {
            self.eat("<-")?;
        } else if !typed && !step && !self.take("<-") {
            return self.do_reply(monad, args, first);
        }
        let value = if step { first.clone() } else { self.term(0)? };
        self.skip();
        self.take(";");
        let span = Some(self.span(beg));
        let depth = self.stk.len();
        let binder = if typed {
            self.bind(&first)?
        } else {
            self.bind(&Term::var("_", 0, None))?
        };
        let rest = self.do_stmt(monad, args, col)?;
        self.close(depth);
        if assign {
            return Ok(Term::Let {
                k: vec![binder.k],
                i: vec![binder.i],
                q: vec![binder.q],
                v: vec![Rc::new(Term::Ann {
                    x: Rc::new(value),
                    t: Rc::new(ty),
                    s: span,
                })],
                f: Rc::new(rest),
                s: span,
            });
        }
        let lambda = Term::lam(binder.k, binder.i, rest, span, Some(binder.q));
        let mut all: Vec<Term> = args
            .iter()
            .take(args.len().saturating_sub(1))
            .cloned()
            .collect();
        all.push(ty);
        all.extend(args.last().cloned());
        all.push(value);
        all.push(lambda);
        self.call(&format!("{monad}.bind"), all, span)
    }

    fn do_reply(&mut self, monad: &str, args: &[Term], value: Term) -> Result<Term> {
        let key = self.reso(monad)?;
        let span = value.span();
        let ty = if self.book.adt(&key).is_some() {
            self.implicit(&key, span);
            Term::Adt {
                k: key,
                x: shared(args.to_vec()),
                r: Vec::new(),
                s: span,
            }
        } else {
            self.call(monad, args.to_vec(), span)?
        };
        Ok(Term::Ann {
            x: Rc::new(value),
            t: Rc::new(ty),
            s: span,
        })
    }

    // Body

    fn body(&mut self, col: usize) -> Result<Body> {
        self.skip();
        let beg = self.pos;
        if self.word("match") {
            return self.body_match(col, beg);
        }
        let quant = if self.take("-") {
            Quant::None
        } else {
            Quant::Lone
        };
        let first = if quant == Quant::None {
            let name = self.name()?;
            let outer = self.span(beg);
            self.record(Some(self.token(&name)), Event::Plus { outer }, false);
            Term::var(name, 0, Some(outer))
        } else {
            self.term(0)?
        };
        let mut targets = vec![first];
        self.skip();
        while !self.nl()
            && (char_is_head(self.peek()) || quant == Quant::Lone && self.at("+"))
            && !self.keyword_ahead()
        {
            targets.push(self.term(0)?);
            self.skip();
        }
        let at = self.pos;
        let checkpoint = self.events.len();
        let mut ty = if targets.len() == 1 && self.take(":") {
            Some(self.term(0)?)
        } else {
            None
        };
        self.skip();
        if ty.is_some() && !self.at("=") {
            self.pos = at;
            self.events.truncate(checkpoint);
            ty = None;
        }
        let mut values: Vec<Term> = Vec::new();
        if ty.is_none()
            && quant == Quant::Lone
            && targets.len() == 1
            && (!self.at("=") || self.at("=="))
        {
            let col_at = self.col(beg);
            let written = term_write(&targets[0], beg).filter(|_| self.more(col_at));
            let Some(written) = written else {
                return Ok(Body::Term(Rc::new(targets.swap_remove(0))));
            };
            let use_site = written.span();
            let copy = self
                .events
                .iter()
                .rev()
                .find(|record| Some(record.span) == use_site && !record.implicit)
                .cloned();
            self.events.extend(copy);
            values.push(targets.swap_remove(0));
            targets = vec![written];
        } else {
            self.eat("=")?;
            if let Some(ty) = &ty {
                let value = self.term(0)?;
                let span = Some(self.span(beg));
                values.push(Term::Ann {
                    x: Rc::new(value),
                    t: Rc::new(ty.clone()),
                    s: span,
                });
            }
        }
        while values.len() < targets.len() {
            values.push(self.term(0)?);
        }
        self.body_local(col, quant, &targets, values, ty.is_some())
    }

    fn body_local(
        &mut self,
        col: usize,
        quant: Quant,
        targets: &[Term],
        values: Vec<Term>,
        typed: bool,
    ) -> Result<Body> {
        self.skip();
        self.take(";");
        let depth = self.stk.len();
        let mut patterns = Vec::with_capacity(targets.len());
        for target in targets {
            if (targets.len() > 1 || typed) && !matches!(target, Term::Var { .. }) {
                let exp = "a name (a parallel or typed let binds names; destructure in its body)";
                return Err(self.error(exp, target.span()));
            }
            patterns.push(self.patt(target)?);
        }
        let rest = self.body(col)?;
        self.close(depth);
        Ok(Body::Local {
            k: patterns,
            q: quant,
            v: shared(values),
            f: Rc::new(rest),
        })
    }

    fn body_match(&mut self, col: usize, beg: usize) -> Result<Body> {
        let scrutinees = self.terms()?;
        self.skip();
        let case_col = self.col(self.pos);
        let mut rows = Vec::new();
        while case_col > col && self.at_word("case") && self.col(self.pos) >= case_col {
            let row_col = self.col(self.pos);
            self.word("case");
            self.skip();
            let patterns_beg = self.pos;
            let terms = self.terms()?;
            if terms.len() != scrutinees.len() {
                let exp = format!("{} patterns (one per scrutinee)", scrutinees.len());
                return Err(self.fail_at(exp, patterns_beg, self.pos - 1));
            }
            let depth = self.stk.len();
            let mut patterns = Vec::with_capacity(terms.len());
            for term in &terms {
                patterns.push(Rc::new(self.patt(term)?));
            }
            let body = self.body(row_col)?;
            self.close(depth);
            rows.push(Case {
                p: patterns,
                f: Rc::new(body),
            });
        }
        Ok(Body::Match {
            e: shared(scrutinees),
            r: rows,
            s: Some(self.span(beg)),
        })
    }

    /// Whether the name characters ahead spell a keyword.
    fn keyword_ahead(&self) -> bool {
        let mut end = self.pos;
        while byte(self.unit(end))
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        {
            end += 1;
        }
        let ahead = self.slice(self.pos, end);
        KEYWORDS
            .iter()
            .any(|keyword| keyword.len() == ahead.len() && starts_with_ascii(ahead, keyword))
    }

    fn more(&self, col: usize) -> bool {
        self.at(";") || self.pos < self.str.len() && self.col(self.pos) == col
    }

    fn block(&mut self) -> Result<Term> {
        self.skip();
        let col = self.col(self.pos).saturating_sub(1);
        let body = self.body(col)?;
        let out = self.flatten(&body, Vec::new())?;
        self.trace_body(&body, &out);
        Ok(out)
    }

    fn terms(&mut self) -> Result<Vec<Term>> {
        let mut terms = Vec::new();
        loop {
            terms.push(self.term(0)?);
            self.skip();
            if self.take(":") {
                return Ok(terms);
            }
            self.take(",");
        }
    }

    fn flatten(&mut self, body: &Body, vars: Vec<PVar>) -> Result<Term> {
        let mut frs = self.frs;
        let out = body_flatten(
            body,
            vars,
            &mut || {
                let index = frs;
                frs += 1;
                index
            },
            self.str,
            self.ns,
        );
        self.frs = frs;
        out
    }

    fn trace_body(&mut self, body: &Body, out: &Term) {
        if let Some(tld) = self.tlds.last_mut() {
            tld.bodies.push((body.clone(), out.clone()));
        }
    }

    fn trace(&mut self, record: impl FnOnce(&mut TldTrace)) {
        if let Some(tld) = self.tlds.last_mut() {
            record(tld);
        }
    }

    // Tele

    fn tele(&mut self, close: &str, templates: &mut Vec<String>) -> Result<Vec<Cell>> {
        let mut cells: Vec<Cell> = Vec::new();
        loop {
            self.skip();
            if self.take(close) {
                return Ok(cells);
            }
            if close == ")" && self.at("~") && templates.len() < cells.len() {
                return Err(self.fail("a plain binder (only leading binders take ~)"));
            }
            let template = close == ")" && self.take("~");
            let quant = if template { Quant::None } else { self.quant() };
            let beg = self.pos;
            let name = self.name()?;
            let name_end = self.pos;
            if close == "}" && cells.iter().any(|cell| cell.k == name) {
                let at = self.pos - name.len();
                let exp = format!("a fresh field name (duplicate declaration: {name})");
                return Err(self.fail_at(exp, at, self.pos));
            }
            let span = Some(self.span(beg));
            self.skip();
            let bare = quant == Quant::Lone && !self.at(":");
            if !bare {
                self.eat(":")?;
            }
            let ty = if bare {
                Term::Qnt { s: span }
            } else {
                self.term(0)?
            };
            if template {
                templates.push(name.clone());
            }
            let index = self.open(&name);
            let token = Some(Span {
                beg: name_end - name.len(),
                end: name_end,
            });
            self.explicit(token, Meaning::Binder(self.binder(index)));
            cells.push(Cell {
                q: if bare { Quant::None } else { quant },
                k: name,
                i: index,
                t: Rc::new(ty),
                s: span,
            });
            self.skip();
            self.take(",");
        }
    }

    // Book

    fn fresh_name(&self, name: &str, ctrs: bool, what: &str) -> Result<String> {
        let key = self.qual(name);
        let alias = name.find('.').map_or("", |dot| &name[..dot]);
        let taken = |key: &str| {
            if ctrs {
                self.book.ctrs.contains_key(key)
            } else {
                self.book.tlds.contains_key(key)
            }
        };
        let aliased = self.alias(alias).is_some();
        if taken(&key) || taken(name) || aliased {
            let why = if aliased {
                format!("{alias} is an import's alias")
            } else {
                format!("duplicate declaration: {name}")
            };
            let at = self.pos - name.len();
            return Err(self.fail_at(format!("{what} ({why})"), at, self.pos));
        }
        Ok(key)
    }

    fn def(&mut self, unsafe_: bool) -> Result<()> {
        self.tlds.push(TldTrace::new(self.ns, TraceKind::Def));
        let name = self.name()?;
        let token = Some(self.token(&name));
        let resolved = self.reso(&name)?;
        let law = self
            .book
            .def(&resolved)
            .filter(|def| !def.filled && !def.base && !def.foreign)
            .copied();
        let key = if law.is_some() {
            resolved
        } else {
            self.fresh_name(&name, false, "a fresh name")?
        };
        let meaning = if law.is_some() {
            Meaning::Global { key: key.clone() }
        } else {
            Meaning::Declaration {
                key: key.clone(),
                ctr: false,
            }
        };
        self.explicit(token, meaning);
        let unsafe_ = self.take("?") || unsafe_;
        self.eat("(")?;
        self.skip();
        if law.is_some() && self.at("~") {
            return Err(self.fail("a name"));
        }
        let mut templates = Vec::new();
        let tele = self.tele(")", &mut templates)?;
        self.trace(|tld| {
            tld.name = Some(key.clone());
            tld.law = Some(law.is_some());
            tld.unsafe_ = Some(unsafe_);
            tld.tele = Some(tele.clone());
        });
        self.skip();
        let templates = match law {
            Some(law) => self.def_law(&tele, law)?,
            None => self.def_type(&name, &key, &tele, templates.len())?,
        };
        self.eat(":")?;
        if self.at_word("import") {
            self.def_foreign(&key, templates)?;
        } else {
            self.def_body(&key, &tele)?;
        }
        self.book.order.push(key);
        Ok(())
    }

    fn def_law(&self, tele: &[Cell], law: Def) -> Result<usize> {
        if tele.iter().any(|cell| !matches!(*cell.t, Term::Qnt { .. })) {
            return Err(self.fail("a name"));
        }
        if tele.len() < law.x {
            let exp = format!("a name for each ~ clause of the law ({})", law.x);
            return Err(self.fail(exp));
        }
        Ok(law.x)
    }

    fn def_type(
        &mut self,
        name: &str,
        key: &str,
        tele: &[Cell],
        templates: usize,
    ) -> Result<usize> {
        if !self.take("->") {
            let exp = format!(
                "'->' (a def with no return type fills a law; no law named {name} is in scope)"
            );
            return Err(self.fail(exp));
        }
        let ret = self.term(0)?;
        self.trace(|tld| tld.ret = Some(ret.clone()));
        super::higher::check_type(&tele_bind(tele, ret), self.str, self.ns)?;
        let def = Def {
            x: templates,
            filled: false,
            base: false,
            foreign: false,
        };
        self.book.tlds.insert(key.to_owned(), Tld::Def(def));
        Ok(templates)
    }

    fn def_foreign(&mut self, key: &str, templates: usize) -> Result<()> {
        if templates > 0 {
            return Err(self.fail("a body (a template is not foreign)"));
        }
        let mut count = 0;
        while self.word("import") {
            self.eat("\"")?;
            let mut path = Vec::new();
            while let Some(unit) = self.peek().filter(|unit| *unit != u16::from(b'"')) {
                path.push(unit);
                self.bump();
            }
            self.eat("\"")?;
            let path = text(&path);
            if !(path.ends_with(".c") || path.ends_with(".js")) {
                return Err(self.fail("a .c or .js path"));
            }
            count += 1;
        }
        if let Some(Tld::Def(def)) = self.book.tlds.get_mut(key) {
            def.foreign = true;
        }
        self.trace(|tld| tld.imports = Some(count));
        Ok(())
    }

    fn def_body(&mut self, key: &str, tele: &[Cell]) -> Result<()> {
        let vars = tele
            .iter()
            .map(|cell| PVar {
                k: cell.k.clone(),
                i: cell.i,
                q: Quant::Lone,
                s: cell.s,
            })
            .collect();
        let body = self.body(0)?;
        let flat = self.flatten(&body, vars)?;
        self.trace_body(&body, &flat);
        super::higher::check_value(&flat, self.str, self.ns)?;
        if let Some(Tld::Def(def)) = self.book.tlds.get_mut(key) {
            def.filled = true;
        }
        Ok(())
    }

    fn adt(&mut self) -> Result<()> {
        self.tlds.push(TldTrace::new(self.ns, TraceKind::Type));
        let name = self.name()?;
        let token = Some(self.token(&name));
        let key = self.fresh_name(&name, false, "a fresh name")?;
        let meaning = Meaning::Declaration {
            key: key.clone(),
            ctr: false,
        };
        self.explicit(token, meaning);
        self.trace(|tld| tld.name = Some(key.clone()));
        self.skip();
        let params = if self.take("<") {
            self.tele(">", &mut Vec::new())?
        } else {
            Vec::new()
        };
        self.trace(|tld| {
            tld.params = Some(params.clone());
            tld.ctrs = Some(Vec::new());
        });
        if !self.word("is") {
            return Err(self.fail("'is'"));
        }
        let kind = self.term(0)?;
        self.trace(|tld| tld.kind_term = Some(kind.clone()));
        self.eat(":")?;
        let quants = params
            .iter()
            .position(|cell| !matches!(*cell.t, Term::Qnt { .. }))
            .unwrap_or(params.len());
        super::higher::check_type(&tele_bind(&params, kind), self.str, self.ns)?;
        let adt = Adt {
            n: params.len(),
            g: quants,
            base: false,
        };
        self.book.tlds.insert(key.clone(), Tld::Adt(adt));
        loop {
            self.skip();
            let declaration = ["def", "type", "law"].iter().any(|word| self.at_word(word));
            if !char_is_head(self.peek()) || declaration {
                break;
            }
            self.adt_ctr(&key, &params)?;
        }
        self.book.order.push(key);
        Ok(())
    }

    fn adt_ctr(&mut self, key: &str, params: &[Cell]) -> Result<()> {
        let name = self.name()?;
        let token = Some(self.token(&name));
        let ctr = self.fresh_name(&name, true, "a fresh constructor name")?;
        let meaning = Meaning::Declaration {
            key: ctr.clone(),
            ctr: true,
        };
        self.explicit(token, meaning);
        self.eat("{")?;
        let depth = self.stk.len();
        let fields = self.tele("}", &mut Vec::new())?;
        self.trace(|tld| {
            if let Some(ctrs) = tld.ctrs.as_mut() {
                ctrs.push((ctr.clone(), fields.clone()));
            }
        });
        self.close(depth);
        let mut cells = params.to_vec();
        cells.extend(fields.iter().cloned());
        let tip = Term::Adt {
            k: key.to_owned(),
            x: params
                .iter()
                .map(|cell| Rc::new(Term::var(cell.k.clone(), cell.i, None)))
                .collect(),
            r: Vec::new(),
            s: None,
        };
        super::higher::check_type(&tele_bind(&cells, tip), self.str, self.ns)?;
        self.book.ctrs.insert(ctr, CtrDecl { n: fields.len() });
        Ok(())
    }

    fn law(&mut self) -> Result<()> {
        self.tlds.push(TldTrace::new(self.ns, TraceKind::Law));
        let name = self.name()?;
        let token = Some(self.token(&name));
        let key = self.fresh_name(&name, false, "a fresh name")?;
        let meaning = Meaning::Declaration {
            key: key.clone(),
            ctr: false,
        };
        self.explicit(token, meaning);
        self.trace(|tld| tld.name = Some(key.clone()));
        self.eat(":")?;
        let mut clauses: Vec<Clause> = Vec::new();
        let mut templates = 0;
        while self.at_word("for") || self.at_word("exs") {
            let clause = self.law_clause(clauses.len(), &mut templates)?;
            clauses.push(clause);
        }
        let end = self.block()?;
        let ty = clauses.into_iter().rev().fold(end, |ty, clause| {
            let span = clause.span;
            if clause.all {
                Term::all(clause.quant, clause.name, clause.index, clause.ty, ty, span)
            } else {
                self.implicit("Exists", span);
                let head = Term::app(Term::reference("Exists", span), clause.ty, span);
                let body = Term::lam(clause.name, clause.index, ty, span, None);
                Term::app(head, body, span)
            }
        });
        self.trace(|tld| tld.t = Some(ty.clone()));
        super::higher::check_type(&ty, self.str, self.ns)?;
        let def = Def {
            x: templates,
            filled: false,
            base: false,
            foreign: false,
        };
        self.book.tlds.insert(key.clone(), Tld::Def(def));
        self.book.order.push(key);
        Ok(())
    }

    fn law_clause(&mut self, count: usize, templates: &mut usize) -> Result<Clause> {
        let all = self.word("for");
        if !all {
            self.word("exs");
        }
        self.skip();
        if all && self.at("~") && *templates < count {
            return Err(self.fail("a plain clause (only leading clauses take ~)"));
        }
        let template = all && self.take("~");
        let quant = if template {
            Quant::None
        } else if all {
            self.quant()
        } else {
            Quant::Lone
        };
        let beg = self.pos;
        let name = self.name()?;
        let token = Some(self.token(&name));
        let span = Some(self.span(beg));
        if template {
            *templates += 1;
        }
        self.eat(":")?;
        let mut ty = self.term(0)?;
        if self.at_word("where") {
            let beg = self.pos;
            self.word("where");
            let where_span = Some(self.span(beg));
            let depth = self.stk.len();
            let index = self.open(&name);
            self.explicit(token, Meaning::Binder(self.binder(index)));
            let witness = self.term(0)?;
            self.close(depth);
            self.implicit("Exists", where_span);
            let head = Term::app(Term::reference("Exists", where_span), ty, span);
            ty = Term::app(
                head,
                Term::lam(name.clone(), index, witness, span, None),
                span,
            );
        }
        let index = self.open(&name);
        self.explicit(token, Meaning::Binder(self.binder(index)));
        Ok(Clause {
            all,
            quant,
            name,
            index,
            ty,
            span,
        })
    }

    pub(super) fn book(&mut self) -> Result<()> {
        loop {
            self.skip();
            if self.pos >= self.str.len() {
                return Ok(());
            }
            self.frs = 0;
            self.close(0);
            let start = self.events.len();
            if let Err(error) = self.declaration() {
                self.events.truncate(start);
                return Err(error);
            }
        }
    }

    fn declaration(&mut self) -> Result<()> {
        {
            if self.take("@") {
                if !self.word("unsafe") {
                    return Err(self.fail("'unsafe' (the one decorator)"));
                }
                if !self.word("def") {
                    return Err(self.fail("'def' (@unsafe marks the def below it)"));
                }
                self.def(true)?;
            } else if self.word("def") {
                self.def(false)?;
            } else if self.word("type") {
                self.adt()?;
            } else if self.word("law") {
                self.law()?;
            } else {
                return Err(self.fail("'def', 'type' or 'law'"));
            }
        }
        Ok(())
    }
}

/// The constructor keys of a stepped literal, down to its next literal.
fn literal_ctrs(term: &Term, keys: &mut Vec<String>) {
    if let Term::Ctr { k, x, .. } = term {
        keys.push(k.clone());
        for field in x {
            literal_ctrs(field, keys);
        }
    }
}

struct Clause {
    all: bool,
    quant: Quant,
    name: String,
    index: i64,
    ty: Term,
    span: Option<Span>,
}

fn head_mut(term: &mut Term) -> &mut Term {
    match term {
        Term::App { f, .. } => head_mut(Rc::make_mut(f)),
        other => other,
    }
}

/// The value of a decimal digit string, when it is at most `0xffffffff`.
fn u32_value(digits: &str) -> Option<u64> {
    let significant = digits.trim_start_matches('0');
    if significant.len() > 10 {
        return None;
    }
    let value: u64 = if significant.is_empty() {
        0
    } else {
        significant.parse().ok()?
    };
    (value <= U32_MAX).then_some(value)
}

/// `nat_from_term`.
fn nat_from_term(term: &Term) -> Option<u64> {
    let mut count = 0;
    let mut cur = term;
    while let Term::Ctr { k, x, .. } = cur
        && k == "Succ"
        && x.len() == 1
    {
        count += 1;
        cur = &x[0];
    }
    match cur {
        Term::Ctr { k, x, .. } if k == "Zero" && x.is_empty() => Some(count),
        Term::Lit {
            k: "Nat",
            v: LitValue::Number(value),
            ..
        } if count + value <= U32_MAX => Some(count + value),
        _ => None,
    }
}

/// `lit_of`
pub(super) fn lit_of(codes: &[u32], span: Option<Span>) -> Term {
    let scalars: Option<String> = codes.iter().map(|code| char::from_u32(*code)).collect();
    if let Some(value) = scalars {
        return Term::Lit {
            k: "String",
            v: LitValue::String(value),
            s: span,
        };
    }
    codes
        .iter()
        .rev()
        .fold(Term::ctr("SNil", Vec::new(), span), |tail, code| {
            let unit = Term::number("U32", u64::from(*code), span);
            let chr = Term::ctr("Chr", vec![unit], span);
            Term::ctr("SCon", vec![chr, tail], span)
        })
}

/// `term_write`: the array of `a[i] <- v` when it heads its own statement.
fn term_write(term: &Term, beg: usize) -> Option<Term> {
    let (head, args) = term.unapply();
    if let Term::Ref { k, .. } = head
        && k == "Array.set"
        && args.len() == 4
        && let Term::Var { s: Some(span), .. } = args[1]
        && span.beg == beg
    {
        return Some(args[1].clone());
    }
    None
}

fn shared(terms: Vec<Term>) -> Vec<Rc<Term>> {
    terms.into_iter().map(Rc::new).collect()
}

pub(super) fn tele_bind(tele: &[Cell], end: Term) -> Term {
    tele.iter().rev().fold(end, |out, cell| {
        Term::all(
            cell.q,
            cell.k.clone(),
            cell.i,
            (*cell.t).clone(),
            out,
            cell.s,
        )
    })
}
