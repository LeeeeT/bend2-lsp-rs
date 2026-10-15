//! A port of the match and body flattening of the Bend compiler (`bend.ts`,
//! "Flatten"), and of the literal helpers that patterns use.

use std::rc::Rc;

use super::{
    Error,
    parse::{char_is_head, char_is_name, text},
    term::{Body, Case, LitValue, PVar, Patt, Quant, Span, Term},
};

pub(super) const NAT_LITERAL_MAX: u64 = 256;

type Result<T> = std::result::Result<T, Error>;

/// The first constructor pattern of a column.
struct Column {
    name: String,
    fields: Vec<Rc<Patt>>,
    span: Option<Span>,
}

struct Flatten<'a, 'f> {
    fr: &'f mut dyn FnMut() -> i64,
    str: &'a [u16],
    ns: &'a str,
}

pub(super) fn body_flatten(
    b: &Body,
    vars: Vec<PVar>,
    fr: &mut dyn FnMut() -> i64,
    str: &[u16],
    ns: &str,
) -> Result<Term> {
    Flatten { fr, str, ns }.body(b, vars)
}

fn quant_join(a: Quant, b: Quant) -> Quant {
    if a == Quant::Many || b == Quant::Many {
        return Quant::Many;
    }
    if a == Quant::None {
        return b;
    }
    a
}

fn quant_dem(q: Quant, qt: Quant) -> Quant {
    if q == Quant::None {
        return Quant::None;
    }
    qt
}

fn head(row: &Case) -> Option<&Patt> {
    row.p.first().map(AsRef::as_ref)
}

fn patt_mark(x: &PVar, rows: &[Case]) -> Quant {
    rows.iter().fold(x.q, |q, row| match head(row) {
        Some(Patt::Var(p)) => quant_join(q, p.q),
        _ => q,
    })
}

pub(super) fn patt_term(q: &Patt, s: Option<Span>) -> Term {
    match q {
        Patt::Var(v) => Term::var(v.k.clone(), v.i, s.or(v.s)),
        Patt::Ctr { k, x, s: own } => {
            let xs = x.iter().map(|x| patt_term(x, s)).collect();
            Term::ctr(k.clone(), xs, s.or(*own))
        }
    }
}

fn body_sub(b: &Body, i: i64, v: &Rc<Patt>) -> Body {
    let scrut = |e: &Rc<Term>| -> Rc<Term> {
        match &**e {
            Term::Var { i: j, s, .. } if *j == i => Rc::new(patt_term(v, *s)),
            Term::Var { .. } => e.clone(),
            _ => Rc::new(Term::Sub {
                i,
                v: v.clone(),
                f: e.clone(),
                s: None,
            }),
        }
    };
    match b {
        Body::Match { e, r, s } => Body::Match {
            e: e.iter().map(scrut).collect(),
            r: r.iter()
                .map(|row| Case {
                    p: row.p.clone(),
                    f: Rc::new(body_sub(&row.f, i, v)),
                })
                .collect(),
            s: *s,
        },
        Body::Local { k, q, v: values, f } => Body::Local {
            k: k.clone(),
            q: *q,
            v: values.iter().map(scrut).collect(),
            f: Rc::new(body_sub(f, i, v)),
        },
        Body::Term(t) => Body::Term(Rc::new(Term::Sub {
            i,
            v: v.clone(),
            f: t.clone(),
            s: None,
        })),
    }
}

impl Flatten<'_, '_> {
    fn error(&self, exp: String, s: Option<Span>) -> Error {
        Error {
            exp,
            obs: None,
            at: s.map(|s| (self.ns.to_owned(), s.beg, s.end)),
        }
    }

    fn source(&self, s: Option<Span>, k: &str) -> String {
        match s {
            Some(s) => text(
                self.str
                    .get(s.beg..s.end.min(self.str.len()))
                    .unwrap_or(&[]),
            ),
            None => k.to_owned(),
        }
    }

    /// `match_flatten`
    fn matches(
        &mut self,
        scrutinees: &[Rc<Term>],
        rows: &[Case],
        span: Option<Span>,
        vars: Vec<PVar>,
    ) -> Result<Term> {
        if scrutinees.is_empty() {
            return match rows.first() {
                Some(row) => self.body(&row.f, vars),
                None => {
                    Err(self.error("a case (this match has no row to return)".to_owned(), span))
                }
            };
        }
        if vars.is_empty() {
            return Err(self.unmatchable(&scrutinees[0], span));
        }
        let first = vars[0].clone();
        let ctr = rows.iter().find_map(|row| match head(row) {
            Some(Patt::Ctr { k, x, s }) => Some(Column {
                name: k.clone(),
                fields: x.clone(),
                span: *s,
            }),
            _ => None,
        });
        let found = match &*scrutinees[0] {
            Term::Var { i, .. } => vars.iter().position(|var| var.i == *i),
            _ => None,
        };
        if let Some(position) = found
            && ctr.is_none()
            && !rows.is_empty()
        {
            return self.var_column(scrutinees, rows, span, vars, position);
        }
        if found == Some(0) {
            let Some(ctr) = ctr else {
                return Ok(Term::Efq { s: span });
            };
            return self.ctr_column(scrutinees, rows, span, vars, ctr);
        }
        let rest = self.matches(scrutinees, rows, span, vars[1..].to_vec())?;
        Ok(Term::lam(first.k, first.i, rest, first.s, Some(first.q)))
    }

    fn unmatchable(&self, scrutinee: &Term, span: Option<Span>) -> Error {
        let mut inner = scrutinee;
        while let Term::Sub { f, .. } = inner {
            inner = f;
        }
        match inner {
            Term::Var { k, s, .. } => self.error(
                format!(
                    "'{}' can't be matched in this position (it is matched after a local statement or after a match on a later binder, it was already matched, or it is a def)",
                    self.source(*s, k)
                ),
                *s,
            ),
            Term::Ctr { k, s, .. } => self.matched_value(&self.source(*s, k), span),
            Term::Lit { k, s, .. } => self.matched_value(&self.source(*s, k), span),
            other => self.error(
                "a parameter or field scrutinee (a match cannot scrutinize a computed value: give it its own def)".to_owned(),
                other.span().or(span),
            ),
        }
    }

    fn var_column(
        &mut self,
        scrutinees: &[Rc<Term>],
        rows: &[Case],
        span: Option<Span>,
        mut vars: Vec<PVar>,
        position: usize,
    ) -> Result<Term> {
        let marked = PVar {
            q: patt_mark(&vars[position], rows),
            ..vars[position].clone()
        };
        let marked_patt = Rc::new(Patt::Var(marked.clone()));
        let mut rest = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(Patt::Var(head)) = head(row) else {
                return Err(self.error(
                    "a variable pattern (this column has no constructor row)".to_owned(),
                    head(row).and_then(Patt::span),
                ));
            };
            rest.push(Case {
                p: row.p[1..].to_vec(),
                f: Rc::new(body_sub(&row.f, head.i, &marked_patt)),
            });
        }
        vars[position] = marked;
        self.matches(&scrutinees[1..], &rest, span, vars)
    }

    fn ctr_column(
        &mut self,
        scrutinees: &[Rc<Term>],
        rows: &[Case],
        span: Option<Span>,
        vars: Vec<PVar>,
        ctr: Column,
    ) -> Result<Term> {
        let first = vars[0].clone();
        let quant = patt_mark(&first, rows);
        let mut fields = Vec::with_capacity(ctr.fields.len());
        for field in &ctr.fields {
            fields.push(match &**field {
                Patt::Var(var) => PVar {
                    q: quant_join(var.q, quant),
                    ..var.clone()
                },
                Patt::Ctr { s, .. } => {
                    let index = (self.fr)();
                    PVar {
                        k: format!("_{index}"),
                        i: index,
                        q: quant,
                        s: *s,
                    }
                }
            });
        }
        let field_patts: Vec<Rc<Patt>> = fields
            .iter()
            .map(|field| Rc::new(Patt::Var(field.clone())))
            .collect();
        let whole = Rc::new(Patt::Ctr {
            k: ctr.name.clone(),
            x: field_patts.clone(),
            s: first.s,
        });
        let first_patt = Rc::new(Patt::Var(first.clone()));
        let mut hits = Vec::new();
        for row in rows {
            match head(row) {
                Some(Patt::Ctr { k, x, .. }) if *k == ctr.name => {
                    let mut patts = x.clone();
                    patts.extend(row.p[1..].iter().cloned());
                    hits.push(Case {
                        p: patts,
                        f: Rc::new(body_sub(&row.f, first.i, &whole)),
                    });
                }
                Some(Patt::Var(head)) => {
                    let renamed = body_sub(&row.f, head.i, &first_patt);
                    let mut patts = field_patts.clone();
                    patts.extend(row.p[1..].iter().cloned());
                    hits.push(Case {
                        p: patts,
                        f: Rc::new(body_sub(&renamed, first.i, &whole)),
                    });
                }
                _ => {}
            }
        }
        let mut hit_scrutinees: Vec<Rc<Term>> = field_patts
            .iter()
            .map(|patt| Rc::new(patt_term(patt, None)))
            .collect();
        hit_scrutinees.extend(scrutinees[1..].iter().cloned());
        let mut hit_vars = fields;
        hit_vars.extend(vars[1..].iter().cloned());
        let hit = self.matches(&hit_scrutinees, &hits, span, hit_vars)?;
        let misses: Vec<Case> = rows
            .iter()
            .filter(|row| !matches!(head(row), Some(Patt::Ctr { k, .. }) if *k == ctr.name))
            .cloned()
            .collect();
        let miss = self.matches(scrutinees, &misses, span, vars)?;
        Ok(Term::Mat {
            k: ctr.name,
            h: Rc::new(hit),
            m: Rc::new(miss),
            s: ctr.span,
        })
    }

    fn matched_value(&self, x: &str, s: Option<Span>) -> Error {
        let units: Vec<u16> = x.encode_utf16().collect();
        let name =
            char_is_head(units.first().copied()) && units.iter().all(|c| char_is_name(Some(*c)));
        if name {
            self.error(
                format!("'{x}' can't be matched here (match it in the same match as the pattern that introduced it)"),
                s,
            )
        } else {
            self.error(
                format!("'{x}' can't be matched (this value is already a constructor: bind its fields directly)"),
                s,
            )
        }
    }

    fn body(&mut self, b: &Body, vars: Vec<PVar>) -> Result<Term> {
        match b {
            Body::Local { k, q, v, f } => {
                if k.len() == 1
                    && let Patt::Ctr { .. } = &k[0]
                {
                    let row = Case {
                        p: vec![Rc::new(k[0].clone())],
                        f: f.clone(),
                    };
                    let s = v.first().and_then(|t| t.span());
                    return self.matches(&v[..1], &[row], s, vars);
                }
                let ws: Vec<PVar> = k
                    .iter()
                    .filter_map(|p| match p {
                        Patt::Var(v) => Some(v.clone()),
                        Patt::Ctr { .. } => None,
                    })
                    .collect();
                let mut g = self.body(f, ws.clone())?;
                for w in &ws {
                    let Term::Lam { f, .. } = g else {
                        return Err(self.error(
                            "a parameter or field scrutinee (a match cannot scrutinize a local binder: give it its own def)".to_owned(),
                            w.s,
                        ));
                    };
                    g = Rc::unwrap_or_clone(f);
                }
                let x = Term::Let {
                    k: ws.iter().map(|w| w.k.clone()).collect(),
                    i: ws.iter().map(|w| w.i).collect(),
                    q: ws.iter().map(|w| quant_dem(*q, w.q)).collect(),
                    v: v.clone(),
                    f: Rc::new(g),
                    s: ws.first().and_then(|w| w.s),
                };
                self.body(&Body::Term(Rc::new(x)), vars)
            }
            Body::Match { e, r, s } => self.matches(e, r, *s, vars),
            Body::Term(t) => {
                let mut out = (**t).clone();
                for v in vars.into_iter().rev() {
                    out = Term::lam(v.k, v.i, out, v.s, Some(v.q));
                }
                Ok(out)
            }
        }
    }
}

/// `word_to_term`
fn word_to_term(n: u64, s: Option<Span>) -> Term {
    let mut out = Term::ctr("WNil", Vec::new(), s);
    for i in (0..32).rev() {
        let bit = if (n >> i) & 1 == 1 { "True" } else { "False" };
        out = Term::ctr("WCon", vec![Term::ctr(bit, Vec::new(), s), out], s);
    }
    out
}

/// `lit_step`: one layer of a literal's constructor tree.
pub(super) fn lit_step(t: &Term) -> Term {
    let Term::Lit { k, v, s } = t else {
        return t.clone();
    };
    match v {
        LitValue::String(v) => {
            let mut chars = v.chars();
            match chars.next() {
                None => Term::ctr("SNil", Vec::new(), *s),
                Some(c) => Term::ctr(
                    "SCon",
                    vec![
                        Term::ctr(
                            "Chr",
                            vec![Term::number("U32", u64::from(u32::from(c)), *s)],
                            *s,
                        ),
                        Term::Lit {
                            k,
                            v: LitValue::String(chars.as_str().to_owned()),
                            s: *s,
                        },
                    ],
                    *s,
                ),
            }
        }
        LitValue::Number(n) => {
            if *k != "Nat" {
                return Term::ctr(*k, vec![word_to_term(*n, *s)], *s);
            }
            if *n == 0 {
                Term::ctr("Zero", Vec::new(), *s)
            } else {
                Term::ctr("Succ", vec![Term::number(k, n - 1, *s)], *s)
            }
        }
    }
}
