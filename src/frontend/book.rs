//! The part of the compiler's book that the parser reads: which names are
//! declared, and the arities and flags that parsing depends on.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Def {
    /// The number of `~` template parameters.
    pub x: usize,
    /// A body was given (`v` is not null).
    pub filled: bool,
    /// Base declared it.
    pub base: bool,
    /// It imports foreign code.
    pub foreign: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Adt {
    pub n: usize,
    pub g: usize,
    pub base: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tld {
    Def(Def),
    Adt(Adt),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CtrDecl {
    pub n: usize,
}

#[derive(Debug, Default)]
pub struct Book {
    pub tlds: HashMap<String, Tld>,
    pub ctrs: HashMap<String, CtrDecl>,
    pub order: Vec<String>,
    pub hols: usize,
}

impl Book {
    #[must_use]
    pub fn declares(&self, k: &str) -> bool {
        self.tlds.contains_key(k) || self.ctrs.contains_key(k)
    }

    #[must_use]
    pub fn def(&self, k: &str) -> Option<&Def> {
        match self.tlds.get(k) {
            Some(Tld::Def(def)) => Some(def),
            _ => None,
        }
    }

    #[must_use]
    pub fn adt(&self, k: &str) -> Option<&Adt> {
        match self.tlds.get(k) {
            Some(Tld::Adt(adt)) => Some(adt),
            _ => None,
        }
    }

    /// Mark the declarations from `from` in the order as Base's.
    pub fn mark_base(&mut self, from: usize) {
        for k in self.order.iter().skip(from) {
            match self.tlds.get_mut(k) {
                Some(Tld::Def(def)) => def.base = true,
                Some(Tld::Adt(adt)) => adt.base = true,
                None => {}
            }
        }
    }
}
