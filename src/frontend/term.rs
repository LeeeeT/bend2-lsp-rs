//! The parsed terms of the Bend compiler (`LTerm`, `Body` and `Patt` in
//! `bend.ts`), with the same fields and the same meaning. Children are
//! shared, as JavaScript shares the objects of a term, so a subterm that
//! flattening puts in two branches costs one pointer.

use std::rc::Rc;

/// A range of UTF-16 code units in the parsed text of one file.
///
/// The parsed text is the file with its import lines blanked, as the
/// compiler parses it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub beg: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Quant {
    None,
    Lone,
    Many,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LitValue {
    Number(u64),
    String(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Var {
        k: String,
        i: i64,
        s: Option<Span>,
        v: Option<Rc<Term>>,
    },
    Ref {
        k: String,
        s: Option<Span>,
        b: bool,
    },
    Sub {
        i: i64,
        v: Rc<Patt>,
        f: Rc<Term>,
        s: Option<Span>,
    },
    Let {
        k: Vec<String>,
        i: Vec<i64>,
        q: Vec<Quant>,
        v: Vec<Rc<Term>>,
        f: Rc<Term>,
        s: Option<Span>,
    },
    Typ {
        g: Rc<Term>,
        s: Option<Span>,
    },
    Qnt {
        s: Option<Span>,
    },
    Qua {
        q: Quant,
        s: Option<Span>,
    },
    Min {
        a: Rc<Term>,
        b: Rc<Term>,
        s: Option<Span>,
    },
    All {
        q: Quant,
        k: String,
        i: i64,
        a: Rc<Term>,
        b: Rc<Term>,
        s: Option<Span>,
    },
    Lam {
        k: String,
        i: i64,
        f: Rc<Term>,
        s: Option<Span>,
        q: Option<Quant>,
    },
    App {
        f: Rc<Term>,
        x: Rc<Term>,
        s: Option<Span>,
    },
    Adt {
        k: String,
        x: Vec<Rc<Term>>,
        r: Vec<String>,
        s: Option<Span>,
    },
    Ctr {
        k: String,
        x: Vec<Rc<Term>>,
        s: Option<Span>,
    },
    Lit {
        k: &'static str,
        v: LitValue,
        s: Option<Span>,
    },
    Mat {
        k: String,
        h: Rc<Term>,
        m: Rc<Term>,
        s: Option<Span>,
    },
    Efq {
        s: Option<Span>,
    },
    Eql {
        a: Rc<Term>,
        b: Rc<Term>,
        t: Rc<Term>,
        s: Option<Span>,
    },
    Rfl {
        s: Option<Span>,
    },
    Rwt {
        e: Rc<Term>,
        p: Rc<Term>,
        f: Rc<Term>,
        s: Option<Span>,
    },
    Hol {
        k: String,
        s: Option<Span>,
    },
    Ann {
        x: Rc<Term>,
        t: Rc<Term>,
        s: Option<Span>,
    },
}

impl Term {
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::Var { s, .. }
            | Self::Ref { s, .. }
            | Self::Sub { s, .. }
            | Self::Let { s, .. }
            | Self::Typ { s, .. }
            | Self::Qnt { s }
            | Self::Qua { s, .. }
            | Self::Min { s, .. }
            | Self::All { s, .. }
            | Self::Lam { s, .. }
            | Self::App { s, .. }
            | Self::Adt { s, .. }
            | Self::Ctr { s, .. }
            | Self::Lit { s, .. }
            | Self::Mat { s, .. }
            | Self::Efq { s }
            | Self::Eql { s, .. }
            | Self::Rfl { s }
            | Self::Rwt { s, .. }
            | Self::Hol { s, .. }
            | Self::Ann { s, .. } => *s,
        }
    }

    pub(super) fn span_mut(&mut self) -> &mut Option<Span> {
        match self {
            Self::Var { s, .. }
            | Self::Ref { s, .. }
            | Self::Sub { s, .. }
            | Self::Let { s, .. }
            | Self::Typ { s, .. }
            | Self::Qnt { s }
            | Self::Qua { s, .. }
            | Self::Min { s, .. }
            | Self::All { s, .. }
            | Self::Lam { s, .. }
            | Self::App { s, .. }
            | Self::Adt { s, .. }
            | Self::Ctr { s, .. }
            | Self::Lit { s, .. }
            | Self::Mat { s, .. }
            | Self::Efq { s }
            | Self::Eql { s, .. }
            | Self::Rfl { s }
            | Self::Rwt { s, .. }
            | Self::Hol { s, .. }
            | Self::Ann { s, .. } => s,
        }
    }

    pub(super) fn var(k: impl Into<String>, i: i64, s: Option<Span>) -> Self {
        Self::Var {
            k: k.into(),
            i,
            s,
            v: None,
        }
    }

    pub(super) fn reference(k: impl Into<String>, s: Option<Span>) -> Self {
        Self::Ref {
            k: k.into(),
            s,
            b: false,
        }
    }

    pub(super) fn app(f: Self, x: Self, s: Option<Span>) -> Self {
        Self::App {
            f: Rc::new(f),
            x: Rc::new(x),
            s,
        }
    }

    pub(super) fn ctr(k: impl Into<String>, x: Vec<Self>, s: Option<Span>) -> Self {
        Self::Ctr {
            k: k.into(),
            x: x.into_iter().map(Rc::new).collect(),
            s,
        }
    }

    pub(super) fn lam(
        name: impl Into<String>,
        index: i64,
        body: Self,
        span: Option<Span>,
        quant: Option<Quant>,
    ) -> Self {
        Self::Lam {
            k: name.into(),
            i: index,
            f: Rc::new(body),
            s: span,
            q: quant,
        }
    }

    pub(super) fn all(
        quant: Quant,
        name: impl Into<String>,
        index: i64,
        domain: Self,
        codomain: Self,
        span: Option<Span>,
    ) -> Self {
        Self::All {
            q: quant,
            k: name.into(),
            i: index,
            a: Rc::new(domain),
            b: Rc::new(codomain),
            s: span,
        }
    }

    pub(super) fn number(k: &'static str, v: u64, s: Option<Span>) -> Self {
        Self::Lit {
            k,
            v: LitValue::Number(v),
            s,
        }
    }

    /// The head and the arguments of an application spine.
    #[must_use]
    pub fn unapply(&self) -> (&Self, Vec<&Self>) {
        let mut xs = Vec::new();
        let mut cur = self;
        while let Self::App { f, x, .. } = cur {
            xs.push(&**x);
            cur = f;
        }
        xs.reverse();
        (cur, xs)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PVar {
    pub k: String,
    pub i: i64,
    pub q: Quant,
    pub s: Option<Span>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Patt {
    Var(PVar),
    Ctr {
        k: String,
        x: Vec<Rc<Patt>>,
        s: Option<Span>,
    },
}

impl Patt {
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::Var(v) => v.s,
            Self::Ctr { s, .. } => *s,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub p: Vec<Rc<Patt>>,
    pub f: Rc<Body>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Match {
        e: Vec<Rc<Term>>,
        r: Vec<Case>,
        s: Option<Span>,
    },
    Local {
        k: Vec<Patt>,
        q: Quant,
        v: Vec<Rc<Term>>,
        f: Rc<Body>,
    },
    Term(Rc<Term>),
}

/// One cell of a telescope: a quantity, a name, a binder index, a type and
/// the span of the name.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub q: Quant,
    pub k: String,
    pub i: i64,
    pub t: Rc<Term>,
    pub s: Option<Span>,
}
