//! A port of the front end of the Bend compiler (`bend2/bend.ts`, v2.0.38):
//! its loader, parser and body flattening. It resolves every name as the
//! compiler does. `tools/oracle` checks it against the compiler itself.

mod book;
mod flatten;
mod higher;
mod load;
mod number;
mod occurrence;
mod parse;
mod term;
mod trace;

pub use book::{Adt, Book, CtrDecl, Def, Tld};
pub use load::{
    Disk, Env, ErrorText, Files, Load, LoadedFile, ReadingFile, Scope, load, load_partial,
};
pub use occurrence::{Binder, Meaning, Occurrence};
pub use parse::resolve;
pub use term::{Body, Case, Cell, LitValue, PVar, Patt, Quant, Span, Term};
pub use trace::{TldTrace, TraceKind, error_json, json_string};

/// An error of the compiler's loader or parser: what it expected, what it
/// observed, and where (the namespace of the file and a range of its parsed
/// text, in UTF-16 code units).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub exp: String,
    pub obs: Option<String>,
    pub at: Option<(String, usize, usize)>,
}
