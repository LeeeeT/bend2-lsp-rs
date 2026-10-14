//! Resolve names across modules: local declarations, import aliases, and the
//! `Base` prelude.

use crate::analysis::{
    CallSite, DocumentSnapshot, IndexedConstructor, Reference, SymbolId, SymbolKind, TextRange,
    TokenId, TokenKind,
};
use std::{ops::Deref, sync::Arc};
use url::Url;

/// A module that a name can resolve into.
#[derive(Clone)]
pub struct Module {
    pub uri: Url,
    pub snapshot: Arc<DocumentSnapshot>,
}

impl Deref for Module {
    type Target = DocumentSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

/// The modules that imports refer to.
pub trait ModuleSource {
    /// The module that the import with this path range in `source` refers to.
    fn import_target(&self, source: &Url, path: TextRange) -> Option<Module>;

    /// The `Base` prelude, if it is loaded.
    fn base(&self) -> Option<Module>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Declaration {
    Symbol(SymbolId),
    Constructor(IndexedConstructor),
}

/// The declaration that a name resolves to, and the module that declares it.
#[derive(Clone)]
pub struct Target {
    pub module: Module,
    pub declaration: Declaration,
}

impl Target {
    /// The declared symbol: the symbol itself, or the type of a constructor.
    #[must_use]
    pub fn symbol(&self) -> SymbolId {
        match self.declaration {
            Declaration::Symbol(id) => id,
            Declaration::Constructor(constructor) => constructor.parent,
        }
    }
}

/// The module that `alias` names in `source`.
#[must_use]
pub fn module(
    modules: &impl ModuleSource,
    uri: &Url,
    source: &DocumentSnapshot,
    alias: &str,
) -> Option<Module> {
    let import = source
        .syntax
        .imports()
        .iter()
        .find(|import| import.alias_text(&source.text) == Some(alias))?;
    if import.path_text(&source.text) == "Base" {
        return modules.base();
    }
    modules.import_target(uri, import.path)
}

/// The `Base` prelude, if `source` imports it.
#[must_use]
pub fn prelude(modules: &impl ModuleSource, source: &DocumentSnapshot) -> Option<Module> {
    source
        .syntax
        .imports()
        .iter()
        .any(|import| import.path_text(&source.text) == "Base")
        .then(|| modules.base())
        .flatten()
}

/// The declaration of the qualified name that ends at `reference`.
///
/// Each segment of a qualified name is its own reference. Only the last
/// segment resolves, because only it names the whole chain.
#[must_use]
pub fn reference_target(
    modules: &impl ModuleSource,
    uri: &Url,
    source: &Arc<DocumentSnapshot>,
    reference: &Reference,
) -> Option<Target> {
    if continues_chain(source, reference.token) {
        return None;
    }
    target(
        modules,
        uri,
        source,
        Name {
            resolved: reference.resolved,
            range: reference.range,
            qualifier_token: reference.qualifier_token,
        },
    )
}

/// The function that `call` invokes.
#[must_use]
pub fn call_target(
    modules: &impl ModuleSource,
    uri: &Url,
    source: &Arc<DocumentSnapshot>,
    call: &CallSite,
) -> Option<Target> {
    let target = target(
        modules,
        uri,
        source,
        Name {
            resolved: call.callee,
            range: call.callee_range,
            qualifier_token: call.qualifier_token,
        },
    )?;
    let Declaration::Symbol(id) = target.declaration else {
        return None;
    };
    (target.module.syntax.symbol_by_id(id)?.kind == SymbolKind::Function).then_some(target)
}

#[derive(Clone, Copy)]
struct Name {
    resolved: Option<SymbolId>,
    range: TextRange,
    qualifier_token: Option<TokenId>,
}

fn target(
    modules: &impl ModuleSource,
    uri: &Url,
    source: &Arc<DocumentSnapshot>,
    name: Name,
) -> Option<Target> {
    let local = || Module {
        uri: uri.clone(),
        snapshot: source.clone(),
    };
    if let Some(id) = name.resolved {
        return source.syntax.symbol_by_id(id).is_some().then(|| Target {
            module: local(),
            declaration: Declaration::Symbol(id),
        });
    }
    let start = match name.qualifier_token {
        Some(token) => {
            if source
                .syntax
                .symbol_for_token(token)
                .is_some_and(|id| source.syntax.binding_by_id(id).is_some())
            {
                return None;
            }
            source.syntax.token(token)?.range.start
        }
        None => name.range.start,
    };
    let full = source.text.get(start..name.range.end)?;
    if let Some((alias, rest)) = full.split_once('.')
        && let Some(imported) = module(modules, uri, source, alias).or_else(|| {
            (alias == "Base")
                .then(|| prelude(modules, source))
                .flatten()
        })
    {
        return declaration(imported, rest);
    }
    let constructor = source
        .syntax
        .name_id(&source.text, full)
        .and_then(|id| source.syntax.constructor_by_name(id))
        .copied();
    if let Some(constructor) = constructor {
        return Some(Target {
            module: local(),
            declaration: Declaration::Constructor(constructor),
        });
    }
    declaration(prelude(modules, source)?, full)
}

fn declaration(module: Module, name: &str) -> Option<Target> {
    let id = module.syntax.name_id(&module.text, name)?;
    let declaration = module
        .syntax
        .symbol_by_name(id)
        .map(|symbol| Declaration::Symbol(symbol.id))
        .or_else(|| {
            module
                .syntax
                .constructor_by_name(id)
                .map(|constructor| Declaration::Constructor(*constructor))
        })?;
    Some(Target {
        module,
        declaration,
    })
}

fn continues_chain(source: &DocumentSnapshot, token: TokenId) -> bool {
    let syntax = &source.syntax;
    let (Some(end), Some(dot), Some(next)) = (
        syntax.token(token),
        syntax.token(TokenId(token.0 + 1)),
        syntax.token(TokenId(token.0 + 2)),
    ) else {
        return false;
    };
    syntax.token_text(&source.text, TokenId(token.0 + 1)) == Some(".")
        && end.range.end == dot.range.start
        && dot.range.end == next.range.start
        && next.kind == TokenKind::Identifier
}
