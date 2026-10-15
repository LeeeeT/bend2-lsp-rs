//! The semantics of each document: the document loaded as the compiler loads
//! the file it checks, with the workspace's texts and the compiler's Base.

use super::{lsp::Backend, orchestration::run_staging};
use crate::{
    analysis::{DocumentSnapshot, LineIndex, TextRange},
    frontend::{self, Env, Files, Meaning},
    semantic::{Identity, Name, Semantics, Target},
    workspace::{hub_library_path, normalize_path},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tower_lsp::lsp_types::{Location, Position, Range};
use url::Url;

const DEFAULT_HUB: &str = "https://hub.bend-lang.com";

/// The texts of the workspace by path; other files come from disk.
struct Overlay {
    texts: HashMap<PathBuf, Arc<DocumentSnapshot>>,
    /// Texts that replace the workspace's: a prefix for a partial load, or
    /// edited texts to check an edit.
    replaced: HashMap<PathBuf, String>,
}

impl Overlay {
    fn text(&self, path: &Path) -> Option<String> {
        if let Some(text) = self.replaced.get(path) {
            return Some(text.clone());
        }
        self.texts.get(path).map(|snapshot| snapshot.text.clone())
    }
}

impl Files for Overlay {
    fn read(&self, path: &Path) -> Option<String> {
        self.text(path).or_else(|| {
            std::fs::read(path)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        })
    }

    fn real(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok().or_else(|| {
            let normalized = normalize_path(path);
            (self.texts.contains_key(&normalized) || self.replaced.contains_key(&normalized))
                .then_some(normalized)
        })
    }
}

#[derive(Default)]
struct CacheState {
    generation: u64,
    base: Option<Url>,
    entries: HashMap<PathBuf, Arc<Semantics>>,
}

/// The semantics of documents, for one workspace generation and one Base.
#[derive(Default)]
pub(super) struct SemanticCache {
    state: Mutex<CacheState>,
}

impl SemanticCache {
    fn lookup(&self, generation: u64, base: Option<&Url>, path: &Path) -> Option<Arc<Semantics>> {
        let mut state = self.state.lock().ok()?;
        if state.generation != generation || state.base.as_ref() != base {
            state.generation = generation;
            state.base = base.cloned();
            state.entries.clear();
        }
        state.entries.get(path).cloned()
    }

    fn store(&self, generation: u64, path: PathBuf, semantics: Arc<Semantics>) {
        if let Ok(mut state) = self.state.lock()
            && state.generation == generation
        {
            state.entries.insert(path, semantics);
        }
    }
}

impl Backend {
    fn semantic_inputs(&self) -> (Overlay, Env, Option<Url>) {
        let base = self.compiler.base_module.read().clone();
        let mut texts = HashMap::new();
        {
            let database = self.workspace.read();
            for document in database.indexed_documents() {
                if let Some(path) = database.document_path(&document.uri) {
                    texts.insert(canonical(&path), document.snapshot.clone());
                }
            }
        }
        let base_path = base.as_ref().and_then(|module| {
            let path = module.uri.to_file_path().ok()?;
            let real = std::fs::canonicalize(&path).unwrap_or(path);
            texts.insert(real.clone(), module.snapshot.clone());
            Some(real)
        });
        let env = Env {
            lib: hub_library_path()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            base: base_path.map_or_else(
                || "\0no Base".to_owned(),
                |path| path.to_string_lossy().into_owned(),
            ),
            hub: std::env::var("BEND_HUB").unwrap_or_else(|_| DEFAULT_HUB.to_owned()),
        };
        (
            Overlay {
                texts,
                replaced: HashMap::new(),
            },
            env,
            base.map(|module| module.uri),
        )
    }

    pub(super) fn entry_path(&self, uri: &Url) -> Option<PathBuf> {
        let path = self
            .document_path(uri)
            .or_else(|| uri.to_file_path().ok())?;
        Some(canonical(&path))
    }

    /// The semantics of a document: the document loaded as the entry.
    pub(super) async fn semantics(&self, uri: &Url) -> Option<Arc<Semantics>> {
        let path = self.entry_path(uri)?;
        let generation = self.workspace.generation();
        let (overlay, env, base) = self.semantic_inputs();
        if let Some(found) = self
            .semantics_cache
            .lookup(generation, base.as_ref(), &path)
        {
            return Some(found);
        }
        let entry = path.to_string_lossy().into_owned();
        let semantics = run_staging(self.workspace.staging.clone(), move || {
            Arc::new(Semantics::new(frontend::load(&overlay, &env, &entry)))
        })
        .await?;
        self.semantics_cache
            .store(generation, path, semantics.clone());
        Some(semantics)
    }

    /// The semantics of a document's text before `offset`, loaded partially:
    /// what each name before it means, and the binders in scope there.
    pub(super) async fn semantics_before(&self, uri: &Url, offset: usize) -> Option<Semantics> {
        let path = self.entry_path(uri)?;
        let (mut overlay, env, _) = self.semantic_inputs();
        let text = overlay.read(&path)?;
        let prefix = text.get(..offset.min(text.len()))?.to_owned();
        overlay.replaced.insert(path.clone(), prefix);
        let entry = path.to_string_lossy().into_owned();
        run_staging(self.workspace.staging.clone(), move || {
            Semantics::new(frontend::load_partial(&overlay, &env, &entry))
        })
        .await
    }

    /// The semantics of documents, with some texts replaced: to check what an
    /// edit would mean before the editor applies it.
    pub(super) async fn semantics_edited(
        &self,
        uris: Vec<Url>,
        replaced: HashMap<PathBuf, String>,
    ) -> Option<Vec<(Url, Semantics)>> {
        let (mut overlay, env, _) = self.semantic_inputs();
        overlay.replaced = replaced;
        let mut entries = Vec::with_capacity(uris.len());
        for uri in uris {
            let path = self.entry_path(&uri)?;
            entries.push((uri, path.to_string_lossy().into_owned()));
        }
        run_staging(self.workspace.staging.clone(), move || {
            entries
                .into_iter()
                .map(|(uri, entry)| (uri, Semantics::new(frontend::load(&overlay, &env, &entry))))
                .collect()
        })
        .await
    }

    /// The semantics of every indexed document.
    pub(super) async fn workspace_semantics(&self) -> Vec<(Url, Arc<Semantics>)> {
        let uris: Vec<Url> = self
            .workspace
            .read()
            .indexed_documents()
            .into_iter()
            .map(|document| document.uri)
            .collect();
        let mut out = Vec::with_capacity(uris.len());
        for uri in uris {
            if let Some(semantics) = self.semantics(&uri).await {
                out.push((uri, semantics));
            }
        }
        out
    }
}

/// The LSP range of a byte range of `source`.
pub(super) fn lsp_range(source: &str, range: TextRange) -> Range {
    let index = LineIndex::new(source);
    let (start_line, start_character) = index.position(source, range.start);
    let (end_line, end_character) = index.position(source, range.end);
    Range::new(
        Position::new(start_line, start_character),
        Position::new(end_line, end_character),
    )
}

pub(super) fn file_uri(path: &Path) -> Option<Url> {
    Url::from_file_path(path).ok()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

impl Backend {
    /// The URI of a loaded file as a document names it: the target of one of
    /// the document's imports when that target is the file, or else the
    /// file's canonical path.
    pub(super) fn path_uri(&self, from: &Url, path: &Path) -> Option<Url> {
        let database = self.workspace.read();
        if let Some(document) = database.cached_document(from) {
            for import in crate::analysis::imports(&document) {
                if let Some(target) = database.import_target(from, import.path)
                    && database
                        .document_path(&target.uri)
                        .is_some_and(|target_path| canonical(&target_path) == path)
                {
                    return Some(target.uri);
                }
            }
        }
        file_uri(path)
    }

    /// Where a declaration is declared, as a document names its file.
    pub(super) fn location(
        &self,
        from: &Url,
        semantics: &Semantics,
        identity: &Identity,
    ) -> Option<Location> {
        let declaration = semantics.declaration(identity)?;
        let file = semantics.file(&identity.path)?;
        Some(Location {
            uri: self.path_uri(from, &identity.path)?,
            range: lsp_range(&file.source, declaration.range),
        })
    }
}

/// The part of a name of the entry that names a declaration: the declared
/// name after an alias. An implicit name keeps its whole range.
pub(super) fn named_range(source: &str, name: &Name, identity: &Identity) -> TextRange {
    let text = source
        .get(name.range.start..name.range.end)
        .unwrap_or_default();
    match text.strip_suffix(identity.name.as_str()) {
        Some(prefix) if !name.implicit => {
            TextRange::new(name.range.start + prefix.len(), name.range.end)
        }
        _ => name.range,
    }
}

/// The name under a cursor: a use before a binder, when one token is both.
pub(super) fn name_at(semantics: &Semantics, offset: usize) -> Option<&Name> {
    let names = semantics.names_at(offset);
    names
        .iter()
        .find(|name| !matches!(name.meaning, Meaning::Binder(_)))
        .or_else(|| names.first())
        .copied()
}

/// The module that the alias of a qualified name names, when the cursor is
/// on the alias: `Lint` in `Lint.Here.nodes`.
pub(super) fn module_at(semantics: &Semantics, name: &Name, offset: usize) -> Option<PathBuf> {
    let entry = semantics.entry()?;
    let text = entry.source.get(name.range.start..name.range.end)?;
    let (alias, _) = text.split_once('.')?;
    if offset > name.range.start + alias.len() {
        return None;
    }
    let (_, sub) = entry.aliases.iter().find(|(name, _)| name == alias)?;
    semantics
        .files
        .iter()
        .find(|file| file.ns == *sub)
        .map(|file| file.path.clone())
}

/// What the compiler reads for one name: a local binder, a declaration, or
/// a key that names nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Reading {
    /// A local binder: the position of its declaration in the entry, and its
    /// index there. Other files' declarations do not move it.
    Local {
        declaration: usize,
        index: i64,
    },
    Global(Identity),
    Unresolved(String),
}

/// The readings of the names of a load's entry, in source order.
pub(super) fn readings(semantics: &Semantics) -> Vec<Reading> {
    let Some(entry) = semantics.entry() else {
        return Vec::new();
    };
    entry
        .names
        .iter()
        .map(|name| match semantics.target(name) {
            Target::Local { binder, .. } => Reading::Local {
                declaration: binder.tld.saturating_sub(entry.tlds.start),
                index: binder.index,
            },
            Target::Global(identity) => Reading::Global(identity),
            Target::Unresolved { key } => Reading::Unresolved(key),
        })
        .collect()
}

/// Whether an edit that only adds an import keeps the meaning of every name:
/// each one reads as before, or it was unresolved before. The load must not
/// fail where it did not.
pub(super) fn imports_keep_meaning(before: &Semantics, after: &Semantics) -> bool {
    let old = readings(before);
    let new = readings(after);
    let failed = after.error.as_ref().is_some_and(|error| {
        before
            .error
            .as_ref()
            .is_none_or(|old| old.message != error.message)
    });
    !failed
        && old.len() == new.len()
        && old
            .iter()
            .zip(&new)
            .all(|(old, new)| old == new || matches!(old, Reading::Unresolved(_)))
}

/// A text with byte-range edits applied.
pub(super) fn apply_edits(text: &str, edits: &[(TextRange, String)]) -> String {
    let mut sorted: Vec<&(TextRange, String)> = edits.iter().collect();
    sorted.sort_by_key(|(range, _)| std::cmp::Reverse((range.start, range.end)));
    let mut out = text.to_owned();
    for (range, new_text) in sorted {
        if range.start <= range.end && range.end <= out.len() {
            out.replace_range(range.start..range.end, new_text);
        }
    }
    out
}

impl Backend {
    /// Whether a document, with these edits that add an import, keeps the
    /// meaning of its names.
    pub(super) async fn import_keeps_meaning(
        &self,
        uri: &Url,
        edits: &[(TextRange, String)],
    ) -> bool {
        let (Some(before), Some(path)) = (self.semantics(uri).await, self.entry_path(uri)) else {
            return false;
        };
        let Some(entry) = before.entry() else {
            return false;
        };
        let text = apply_edits(&entry.source, edits);
        let replaced = HashMap::from([(path, text)]);
        let Some(after) = self.semantics_edited(vec![uri.clone()], replaced).await else {
            return false;
        };
        after
            .first()
            .is_some_and(|(_, after)| imports_keep_meaning(&before, after))
    }
}

#[cfg(test)]
mod tests {
    use super::lsp_range;
    use crate::analysis::TextRange;

    #[test]
    fn ranges_count_utf16_columns() {
        let range = lsp_range("é\nab", TextRange::new(3, 5));
        assert_eq!((range.start.line, range.start.character), (1, 0));
        assert_eq!((range.end.line, range.end.character), (1, 2));
    }
}
