//! What each name in a file means, as the compiler's parser decides it.
//!
//! The parser records an occurrence where it decides the meaning of a name,
//! and corrects it where it reinterprets a parsed term: a name that becomes a
//! binder, a type head or a constructor arm, and an operator that its
//! annotation names. A use with no name of its own, such as `T.add` behind
//! `(a + b : T)` or `Con` behind `[a]`, is an implicit occurrence at the span
//! of its syntax.

use super::term::Span;

/// A local binder: the declaration it belongs to (its index in the load's
/// declarations) and its index in the parser's binder counter.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Binder {
    pub tld: usize,
    pub index: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Meaning {
    /// Declares a top-level name (`ctr` is false) or a constructor.
    Declaration { key: String, ctr: bool },
    /// Declares a local binder.
    Binder(Binder),
    /// Uses a local binder.
    Local(Binder),
    /// Uses the top-level name or constructor with this book key.
    Global { key: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Occurrence {
    /// The span in the parsed text, in UTF-16 code units.
    pub span: Span,
    pub meaning: Meaning,
    /// The occurrence has no name of its own at this span.
    pub implicit: bool,
    /// The declaration that holds it: its index in the load's declarations.
    pub tld: usize,
}

/// A meaning that the parser has not settled yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Event {
    Settled(Meaning),
    /// An unbound name: the global `key`, unless a binder takes it.
    Fallback {
        key: String,
    },
    /// An operator whose namespace a later `: T` names.
    Operator,
    /// A `+x` name, which only a binder gives a meaning; `outer` spans the `+`.
    Plus {
        outer: Span,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Recorded {
    pub span: Span,
    pub event: Event,
    pub implicit: bool,
    pub tld: usize,
}

/// The occurrences of the declarations that parsed completely. An unsettled
/// event keeps no meaning: a fallback is its global, and an operator or a
/// `+x` that no binder took means nothing to the compiler.
pub(super) fn settle(recorded: Vec<Recorded>) -> Vec<Occurrence> {
    recorded
        .into_iter()
        .filter_map(|record| {
            let meaning = match record.event {
                Event::Settled(meaning) => meaning,
                Event::Fallback { key } => Meaning::Global { key },
                Event::Operator | Event::Plus { .. } => return None,
            };
            Some(Occurrence {
                span: record.span,
                meaning,
                implicit: record.implicit,
                tld: record.tld,
            })
        })
        .collect()
}
