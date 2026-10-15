//! Checks the occurrences of a load against the terms that its parser built
//! and against the identifier tokens of each file.

use std::collections::{HashMap, HashSet};

use bend2_lsp::frontend::{Body, Cell, Load, Meaning, Patt, Span, Term, TldTrace};

/// The names, binders and keys that one declaration's terms hold.
#[derive(Default)]
struct Nodes {
    /// `(span, binder index)` of each local variable.
    locals: HashSet<(usize, usize, i64)>,
    /// `(binder index, name)` of each binder.
    binders: HashSet<(i64, String)>,
    /// The start of each node that names a key, and the key.
    named: HashSet<(usize, String)>,
    /// Every key that the terms use.
    keys: HashSet<String>,
    /// The constructor keys of match arms, which have no span of their own.
    arms: HashSet<String>,
    /// The declaration's own name and constructors.
    declared: HashSet<String>,
}

impl Nodes {
    fn key(&mut self, key: &str, span: Option<Span>) {
        self.keys.insert(key.to_owned());
        if let Some(span) = span {
            self.named.insert((span.beg, key.to_owned()));
        }
    }

    fn cells(&mut self, cells: &[Cell]) {
        for cell in cells {
            self.binders.insert((cell.i, cell.k.clone()));
            self.term(&cell.t);
        }
    }

    fn patt(&mut self, patt: &Patt) {
        match patt {
            Patt::Var(var) => {
                self.binders.insert((var.i, var.k.clone()));
            }
            Patt::Ctr { k, x, s } => {
                self.key(k, *s);
                for field in x {
                    self.patt(field);
                }
            }
        }
    }

    fn body(&mut self, body: &Body) {
        match body {
            Body::Match { e, r, .. } => {
                for term in e {
                    self.term(term);
                }
                for row in r {
                    for patt in &row.p {
                        self.patt(patt);
                    }
                    self.body(&row.f);
                }
            }
            Body::Local { k, v, f, .. } => {
                for patt in k {
                    self.patt(patt);
                }
                for term in v {
                    self.term(term);
                }
                self.body(f);
            }
            Body::Term(term) => self.term(term),
        }
    }

    fn term(&mut self, term: &Term) {
        match term {
            Term::Var { k, i, s, v } => {
                if let Some(fallback) = v {
                    self.term(fallback);
                } else if let Some(span) = s {
                    self.locals.insert((span.beg, span.end, *i));
                }
                let _ = k;
            }
            Term::Ref { k, s, .. } => self.key(k, *s),
            Term::Sub { v, f, .. } => {
                self.patt(v);
                self.term(f);
            }
            Term::Let { k, i, v, f, .. } => {
                for (name, index) in k.iter().zip(i) {
                    self.binders.insert((*index, name.clone()));
                }
                for value in v {
                    self.term(value);
                }
                self.term(f);
            }
            Term::Typ { g, .. } => self.term(g),
            Term::Min { a, b, .. } | Term::App { f: a, x: b, .. } => {
                self.term(a);
                self.term(b);
            }
            Term::All { k, i, a, b, .. } => {
                self.binders.insert((*i, k.clone()));
                self.term(a);
                self.term(b);
            }
            Term::Lam { k, i, f, .. } => {
                self.binders.insert((*i, k.clone()));
                self.term(f);
            }
            Term::Adt { k, x, s, .. } | Term::Ctr { k, x, s } => {
                self.key(k, *s);
                for arg in x {
                    self.term(arg);
                }
            }
            Term::Mat { k, h, m, .. } => {
                self.keys.insert(k.clone());
                self.arms.insert(k.clone());
                self.term(h);
                self.term(m);
            }
            Term::Eql { a, b, t, .. } => {
                self.term(a);
                self.term(b);
                self.term(t);
            }
            Term::Rwt { e, p, f, .. } => {
                self.term(e);
                self.term(p);
                self.term(f);
            }
            Term::Ann { x, t, .. } => {
                self.term(x);
                self.term(t);
            }
            Term::Qnt { .. }
            | Term::Qua { .. }
            | Term::Lit { .. }
            | Term::Efq { .. }
            | Term::Rfl { .. }
            | Term::Hol { .. } => {}
        }
    }

    fn of(tld: &TldTrace) -> Self {
        let mut nodes = Self::default();
        for cells in [&tld.tele, &tld.params].into_iter().flatten() {
            nodes.cells(cells);
        }
        for term in [&tld.ret, &tld.kind_term, &tld.t].into_iter().flatten() {
            nodes.term(term);
        }
        for (_, fields) in tld.ctrs.iter().flatten() {
            nodes.cells(fields);
        }
        for (body, out) in &tld.bodies {
            nodes.body(body);
            nodes.term(out);
        }
        if let Some(name) = &tld.name {
            nodes.declared.insert(name.clone());
        }
        for (ctr, _) in tld.ctrs.iter().flatten() {
            nodes.declared.insert(ctr.clone());
        }
        nodes
    }
}

const KEYWORDS: [&str; 18] = [
    "def", "type", "law", "match", "case", "do", "return", "for", "exs", "where", "is", "import",
    "Type", "Data", "Kind", "Quant", "unsafe", "as",
];

fn is_head(unit: u16) -> bool {
    u8::try_from(unit).is_ok_and(|b| b.is_ascii_alphabetic() || b == b'_')
}

fn is_name(unit: u16) -> bool {
    u8::try_from(unit).is_ok_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

/// The identifier tokens of a parsed text, outside comments, strings,
/// characters, numbers and hole names.
fn tokens(text: &[u16]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0;
    let at = |i: usize| text.get(i).copied();
    while let Some(unit) = at(pos) {
        match u8::try_from(unit).ok() {
            Some(b'#') => {
                while at(pos).is_some_and(|unit| unit != 10) {
                    pos += 1;
                }
            }
            Some(quote @ (b'"' | b'\'')) => {
                pos += 1;
                while let Some(unit) = at(pos) {
                    pos += if unit == u16::from(b'\\') { 2 } else { 1 };
                    if unit == u16::from(quote) {
                        break;
                    }
                }
            }
            Some(b'?') => {
                pos += 1;
                while at(pos).is_some_and(is_name) {
                    pos += 1;
                }
            }
            Some(digit) if digit.is_ascii_digit() => {
                while at(pos).is_some_and(is_name) {
                    pos += 1;
                }
            }
            _ if is_head(unit) => {
                let beg = pos;
                while at(pos).is_some_and(is_name) {
                    pos += 1;
                }
                let word = String::from_utf16_lossy(&text[beg..pos]);
                if !KEYWORDS.contains(&word.as_str()) {
                    out.push(Span { beg, end: pos });
                }
            }
            _ => pos += 1,
        }
    }
    out
}

/// The problems of one load: each occurrence that its terms do not justify,
/// and each identifier token with no occurrence.
pub fn count(loaded: &Load) -> (usize, usize) {
    let entry = loaded
        .files
        .last()
        .filter(|file| file.ns.is_empty() && loaded.error.is_none());
    let occurrences = entry.map_or(0, |file| file.occurrences.len());
    let tokens = entry.map_or(0, |file| tokens(&file.parsed).len());
    (occurrences, tokens)
}

pub fn check(loaded: &Load) -> Vec<String> {
    let entry_tlds = loaded
        .tlds
        .iter()
        .rev()
        .take_while(|tld| tld.file.is_empty())
        .count();
    let first = loaded.tlds.len() - entry_tlds;
    let nodes: HashMap<usize, Nodes> = (first..loaded.tlds.len())
        .map(|index| (index, Nodes::of(&loaded.tlds[index])))
        .collect();
    let mut all = Nodes::default();
    for n in nodes.values() {
        all.named.extend(n.named.iter().cloned());
        all.keys.extend(n.keys.iter().cloned());
        all.arms.extend(n.arms.iter().cloned());
        all.declared.extend(n.declared.iter().cloned());
    }
    let mut problems = Vec::new();
    let entry = loaded
        .files
        .last()
        .filter(|file| file.ns.is_empty() && loaded.error.is_none());
    if let Some(file) = entry {
        let text = |span: Span| String::from_utf16_lossy(&file.parsed[span.beg..span.end]);
        let mut covered: HashMap<(usize, usize), usize> = HashMap::new();
        for occurrence in &file.occurrences {
            let span = occurrence.span;
            if !occurrence.implicit {
                *covered.entry((span.beg, span.end)).or_default() += 1;
            }
            let ok = match &occurrence.meaning {
                Meaning::Local(binder) => {
                    let found = nodes
                        .get(&binder.tld)
                        .is_some_and(|n| n.locals.contains(&(span.beg, span.end, binder.index)));
                    let named = nodes
                        .get(&binder.tld)
                        .is_some_and(|n| n.binders.contains(&(binder.index, text(span))));
                    if !found && named {
                        problems.push(format!(
                            "unverified: {}: '{}' at {}..{}: {:?} has no variable node",
                            file.real,
                            text(span),
                            span.beg,
                            span.end,
                            occurrence.meaning
                        ));
                    }
                    found || named
                }
                Meaning::Binder(binder) => nodes
                    .get(&binder.tld)
                    .is_some_and(|n| n.binders.contains(&(binder.index, text(span)))),
                Meaning::Global { key } if occurrence.implicit => all.keys.contains(key),
                Meaning::Global { key } => {
                    all.named.contains(&(span.beg, key.clone()))
                        || all.arms.contains(key)
                        || loaded.book.declares(key)
                }
                Meaning::Declaration { key, .. } => all.declared.contains(key),
            };
            if !ok {
                problems.push(format!(
                    "{}: '{}' at {}..{}: {:?}",
                    file.real,
                    text(span),
                    span.beg,
                    span.end,
                    occurrence.meaning
                ));
            }
        }
        if loaded.error.is_none() {
            for token in tokens(&file.parsed) {
                let marked = token
                    .beg
                    .checked_sub(1)
                    .and_then(|index| file.parsed.get(index))
                    == Some(&u16::from(b'+'));
                if !covered.contains_key(&(token.beg, token.end)) {
                    let why = if marked {
                        "is a +name in term position, which the compiler leaves unbound"
                    } else {
                        "has no occurrence"
                    };
                    problems.push(format!(
                        "{}: '{}' at {}..{} {why}",
                        file.real,
                        text(token),
                        token.beg,
                        token.end
                    ));
                }
            }
        }
    }
    problems
}
