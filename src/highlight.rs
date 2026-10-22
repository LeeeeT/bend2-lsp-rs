//! The class of each token of a document, for syntax highlighting. The lexer
//! gives the class of each token, and the names of a load give the class of
//! each identifier.

use crate::{
    analysis::{SyntaxIndex, TextRange, TokenId, TokenKind},
    frontend::Meaning,
    semantic::{DeclarationKind, Semantics, Target},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Class {
    Keyword,
    Comment,
    /// A string, or the path of an import, as `Base` in `import Base`.
    String,
    Number,
    Operator,
    /// The alias of an import, as `Bytes` in `import … as Bytes` and in
    /// `Bytes.from_string`.
    Namespace,
    /// A local binder or a use of it.
    Variable,
    /// A def or a law.
    Function,
    /// A type, a universe, or a def or a law that gives types.
    Type,
    Constructor,
}

impl Class {
    pub const ALL: [Self; 10] = [
        Self::Keyword,
        Self::Comment,
        Self::String,
        Self::Number,
        Self::Operator,
        Self::Namespace,
        Self::Variable,
        Self::Function,
        Self::Type,
        Self::Constructor,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Highlight {
    pub range: TextRange,
    pub class: Class,
    /// The token declares its name: a binder, or the name of a declaration.
    pub declaration: bool,
}

/// The class of an identifier token from what its name means, and whether the
/// token declares the name. A token with no meaning, as in a declaration that
/// does not parse, gets no class.
fn identifier(semantics: &Semantics, token: TextRange) -> Option<(Class, bool)> {
    let entry = semantics.entry()?;
    let end = entry
        .names
        .partition_point(|name| name.range.start <= token.start);
    let name = entry.names[..end]
        .iter()
        .rev()
        .find(|name| !name.implicit && name.range.end >= token.end)?;
    let text = entry.source.get(name.range.start..name.range.end)?;
    if let Some((alias, _)) = text.split_once('.')
        && token.start == name.range.start
        && token.end == name.range.start + alias.len()
        && entry.aliases.iter().any(|(found, _)| found == alias)
    {
        return Some((Class::Namespace, false));
    }
    let declaration = match &name.meaning {
        Meaning::Binder(_) => return Some((Class::Variable, true)),
        Meaning::Local(_) => return Some((Class::Variable, false)),
        Meaning::Declaration { .. } => true,
        Meaning::Global { .. } => false,
    };
    let Target::Global(identity) = semantics.target(name) else {
        return None;
    };
    let found = semantics.declaration(&identity)?;
    let class = match found.kind {
        DeclarationKind::Def | DeclarationKind::Law if found.type_former => Class::Type,
        DeclarationKind::Def | DeclarationKind::Law => Class::Function,
        DeclarationKind::Type => Class::Type,
        DeclarationKind::Constructor => Class::Constructor,
    };
    Some((class, declaration))
}

/// The highlights of a document in source order. A universe, as `Type`, is a
/// type. The path of an import is one string, and its alias declares a
/// namespace. Without semantics, as for a document that does not load,
/// identifiers other than keywords, universes and aliases get none.
#[must_use]
pub fn highlights(syntax: &SyntaxIndex, semantics: Option<&Semantics>) -> Vec<Highlight> {
    let imports = syntax.imports();
    let mut result = Vec::new();
    let mut paths = imports.iter().map(|import| import.path).peekable();
    for (index, token) in syntax.tokens().iter().enumerate() {
        while paths
            .next_if(|path| path.end <= token.range.start)
            .is_some()
        {}
        if let Some(&path) = paths.peek()
            && path.start <= token.range.start
        {
            if result
                .last()
                .is_none_or(|last: &Highlight| last.range != path)
            {
                result.push(Highlight {
                    range: path,
                    class: Class::String,
                    declaration: false,
                });
            }
            continue;
        }
        let alias = imports
            .iter()
            .any(|import| import.alias == Some(token.range));
        let found = match token.kind {
            TokenKind::Identifier if alias => Some((Class::Namespace, true)),
            TokenKind::Comment => Some((Class::Comment, false)),
            TokenKind::StringLiteral => Some((Class::String, false)),
            TokenKind::Number => Some((Class::Number, false)),
            TokenKind::Operator => Some((Class::Operator, false)),
            TokenKind::Identifier => match syntax.name_for_token(TokenId(index)) {
                Some(name) if syntax.is_keyword(name) => Some((Class::Keyword, false)),
                Some(name) if syntax.is_universe(name) => Some((Class::Type, false)),
                _ => semantics.and_then(|semantics| identifier(semantics, token.range)),
            },
            TokenKind::Punctuation | TokenKind::Delimiter => None,
        };
        if let Some((class, declaration)) = found {
            result.push(Highlight {
                range: token.range,
                class,
                declaration,
            });
        }
    }
    result
}
