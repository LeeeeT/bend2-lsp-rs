//! The one parse-time error of `term_higher` and `term_lower`: an operator
//! with no type. The checks follow their evaluation order, so the first
//! error is the compiler's first error.

use super::{Error, parse::text, term::Term};

type Result<T> = std::result::Result<T, Error>;

struct Higher<'a> {
    str: &'a [u16],
    ns: &'a str,
}

/// The parts of `term_higher(t)` that run eagerly, as for a type.
pub(super) fn check_type(t: &Term, str: &[u16], ns: &str) -> Result<()> {
    Higher { str, ns }.eager(t)
}

/// `term_lower(term_higher(t))`, which runs every part, as for a def body.
pub(super) fn check_value(t: &Term, str: &[u16], ns: &str) -> Result<()> {
    Higher { str, ns }.force(t)
}

impl Higher<'_> {
    fn force(&self, t: &Term) -> Result<()> {
        self.eager(t)?;
        self.lower(t)
    }

    fn eager(&self, t: &Term) -> Result<()> {
        match t {
            Term::Ref { k, s, .. } => {
                if k.rfind('.') == Some(0) {
                    let op = match s {
                        Some(s) => text(
                            self.str
                                .get(s.beg..s.end.min(self.str.len()))
                                .unwrap_or(&[]),
                        ),
                        None => k.clone(),
                    };
                    return Err(Error {
                        exp: format!("a type for this operator (write (a {op} b : Nat))"),
                        obs: None,
                        at: s.map(|s| (self.ns.to_owned(), s.beg, s.end)),
                    });
                }
                Ok(())
            }
            Term::Sub { f, .. } => self.eager(f),
            Term::Let { v, .. } => v.iter().try_for_each(|v| self.eager(v)),
            Term::Typ { g, .. } => self.eager(g),
            Term::All { a, .. } => self.eager(a),
            Term::Min { a, b, .. } => {
                self.eager(a)?;
                self.eager(b)
            }
            Term::App { f, x, .. } => {
                self.eager(f)?;
                self.eager(x)
            }
            Term::Adt { x, .. } | Term::Ctr { x, .. } => x.iter().try_for_each(|x| self.eager(x)),
            Term::Mat { h, m, .. } => {
                self.eager(h)?;
                self.eager(m)
            }
            Term::Eql { a, b, t, .. } => {
                self.eager(a)?;
                self.eager(b)?;
                self.eager(t)
            }
            Term::Rwt { e, p, f, .. } => {
                self.eager(e)?;
                self.eager(p)?;
                self.eager(f)
            }
            Term::Ann { x, t, .. } => {
                self.eager(x)?;
                self.eager(t)
            }
            Term::Var { .. }
            | Term::Lam { .. }
            | Term::Qnt { .. }
            | Term::Qua { .. }
            | Term::Lit { .. }
            | Term::Efq { .. }
            | Term::Rfl { .. }
            | Term::Hol { .. } => Ok(()),
        }
    }

    fn lower(&self, t: &Term) -> Result<()> {
        match t {
            Term::Let { v, f, .. } => {
                v.iter().try_for_each(|v| self.lower(v))?;
                self.force(f)
            }
            Term::All { a, b, .. } => {
                self.lower(a)?;
                self.force(b)
            }
            Term::Lam { f, .. } => self.force(f),
            Term::Sub { f, .. } | Term::Typ { g: f, .. } => self.lower(f),
            Term::Min { a, b, .. }
            | Term::App { f: a, x: b, .. }
            | Term::Mat { h: a, m: b, .. } => {
                self.lower(a)?;
                self.lower(b)
            }
            Term::Ann { x, t, .. } => {
                self.lower(x)?;
                self.lower(t)
            }
            Term::Adt { x, .. } | Term::Ctr { x, .. } => x.iter().try_for_each(|x| self.lower(x)),
            Term::Eql { a, b, t, .. } => {
                self.lower(a)?;
                self.lower(b)?;
                self.lower(t)
            }
            Term::Rwt { e, p, f, .. } => {
                self.lower(e)?;
                self.lower(p)?;
                self.lower(f)
            }
            Term::Var { .. }
            | Term::Ref { .. }
            | Term::Qnt { .. }
            | Term::Qua { .. }
            | Term::Lit { .. }
            | Term::Efq { .. }
            | Term::Rfl { .. }
            | Term::Hol { .. } => Ok(()),
        }
    }
}
