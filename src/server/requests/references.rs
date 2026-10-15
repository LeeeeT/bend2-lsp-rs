use super::super::{
    adapters,
    lsp::Backend,
    reference_locations::sort_and_deduplicate,
    semantics::{Reading, file_uri, lsp_range, name_at, named_range, readings},
};
use super::import_edits::versioned_edit;
use crate::{
    analysis::{self, TextRange},
    frontend::{Binder, Meaning},
    semantic::{DeclarationKind, Identity, Semantics, Target},
    workspace::Document,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tower_lsp::{
    jsonrpc::{Error, Result},
    lsp_types::{
        CodeLens, CodeLensParams, DocumentHighlight, DocumentHighlightKind,
        DocumentHighlightParams, Location, PrepareRenameResponse, ReferenceParams, RenameParams,
        TextDocumentPositionParams, TextEdit, WorkspaceEdit,
    },
};
use url::Url;

/// What a cursor names, for references and rename.
#[derive(Clone)]
enum Subject {
    Local(Binder),
    Global(Identity),
    /// An import alias of the document, and the namespace it names.
    Alias {
        alias: String,
        sub: String,
    },
}

fn invalid(message: &str) -> Error {
    Error::invalid_params(message.to_owned())
}

fn is_name(name: &str, dotted: bool) -> bool {
    let valid = name.split('.').all(|segment| {
        let mut chars = segment.chars();
        chars
            .next()
            .is_some_and(|head| head.is_ascii_alphabetic() || head == '_')
            && chars.all(|rest| rest.is_ascii_alphanumeric() || rest == '_')
    });
    valid && (dotted || !name.contains('.')) && !analysis::SyntaxIndex::is_keyword_text(name)
}

/// The names of a document's entry that a subject covers, and whether each is
/// implicit. Each range is the part that names the subject: the declared name
/// after an alias, or the alias itself.
fn covered(semantics: &Semantics, subject: &Subject, declarations: bool) -> Vec<(TextRange, bool)> {
    let Some(entry) = semantics.entry() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in &entry.names {
        let text = entry
            .source
            .get(name.range.start..name.range.end)
            .unwrap_or_default();
        match subject {
            Subject::Local(binder) => match name.meaning {
                Meaning::Local(found) if found == *binder => out.push((name.range, name.implicit)),
                Meaning::Binder(found) if found == *binder && declarations => {
                    out.push((name.range, false));
                }
                _ => {}
            },
            Subject::Global(identity) => {
                let declaration = matches!(name.meaning, Meaning::Declaration { .. });
                if (declarations || !declaration)
                    && semantics.target(name) == Target::Global(identity.clone())
                {
                    out.push((named_range(&entry.source, name, identity), name.implicit));
                }
            }
            Subject::Alias { alias, sub } => {
                let through = matches!(
                    &name.meaning,
                    Meaning::Global { key, .. } if key.split_once(':').is_some_and(|(ns, _)| ns == sub)
                );
                if !name.implicit && through && text.starts_with(&format!("{alias}.")) {
                    out.push((
                        TextRange::new(name.range.start, name.range.start + alias.len()),
                        false,
                    ));
                }
            }
        }
    }
    out
}

/// The alias tokens of a document's import lines that declare `alias`.
fn alias_declarations(document: &Document, alias: &str) -> Vec<TextRange> {
    analysis::imports(document)
        .iter()
        .filter(|import| import.alias_text(&document.text) == Some(alias))
        .filter_map(|import| import.alias)
        .collect()
}

impl Backend {
    /// What the cursor names, and the range that a rename replaces there.
    fn subject_at(
        semantics: &Semantics,
        document: &Document,
        offset: usize,
    ) -> Option<(Subject, TextRange)> {
        let entry = semantics.entry()?;
        for import in analysis::imports(document) {
            let Some(range) = import
                .alias
                .filter(|range| range.start <= offset && offset <= range.end)
            else {
                continue;
            };
            let alias = import.alias_text(&document.text)?.to_owned();
            let (_, sub) = entry.aliases.iter().find(|(name, _)| *name == alias)?;
            return Some((
                Subject::Alias {
                    alias,
                    sub: sub.clone(),
                },
                range,
            ));
        }
        let name = name_at(semantics, offset)?;
        let text = entry.source.get(name.range.start..name.range.end)?;
        if let Some((alias, _)) = text.split_once('.')
            && offset < name.range.start + alias.len()
            && let Some((_, sub)) = entry.aliases.iter().find(|(found, _)| found == alias)
        {
            let range = TextRange::new(name.range.start, name.range.start + alias.len());
            let subject = Subject::Alias {
                alias: alias.to_owned(),
                sub: sub.clone(),
            };
            return Some((subject, range));
        }
        match semantics.target(name) {
            Target::Local { binder, .. } => Some((Subject::Local(binder), name.range)),
            Target::Global(identity) => {
                let prefix = text.strip_suffix(identity.name.as_str())?;
                let range = TextRange::new(name.range.start + prefix.len(), name.range.end);
                (range.start <= offset).then_some((Subject::Global(identity), range))
            }
            Target::Unresolved { .. } => None,
        }
    }

    async fn subject(
        &self,
        uri: &Url,
        position: tower_lsp::lsp_types::Position,
    ) -> Option<(Document, Arc<Semantics>, Subject, TextRange)> {
        let document = self.workspace.read().cached_document(uri)?;
        if !Self::supported(&document) {
            return None;
        }
        let offset = adapters::offset_at(&document, position);
        if document.syntax.is_in_comment_or_string(offset) {
            return None;
        }
        let semantics = self.semantics(uri).await?;
        let (subject, range) = Self::subject_at(&semantics, &document, offset)?;
        Some((document, semantics, subject, range))
    }

    /// The locations that a subject covers in the workspace.
    async fn subject_locations(
        &self,
        document: &Document,
        semantics: &Semantics,
        subject: &Subject,
        declarations: bool,
    ) -> Vec<Location> {
        let uri = &document.uri;
        let documents = match subject {
            Subject::Global(_) => self.workspace_semantics().await,
            Subject::Local(_) | Subject::Alias { .. } => self
                .semantics(uri)
                .await
                .map(|found| vec![(uri.clone(), found)])
                .unwrap_or_default(),
        };
        let mut locations = Vec::new();
        for (document_uri, found) in &documents {
            let Some(entry) = found.entry() else {
                continue;
            };
            for (range, _) in covered(found, subject, declarations) {
                locations.push(Location {
                    uri: document_uri.clone(),
                    range: lsp_range(&entry.source, range),
                });
            }
        }
        if declarations {
            match subject {
                Subject::Global(identity) => {
                    locations.extend(self.location(uri, semantics, identity));
                }
                Subject::Alias { alias, .. } => {
                    locations.extend(alias_declarations(document, alias).into_iter().map(
                        |range| Location {
                            uri: uri.clone(),
                            range: adapters::range(document, range),
                        },
                    ));
                }
                Subject::Local(_) => {}
            }
        }
        sort_and_deduplicate(&mut locations);
        locations
    }

    pub(in crate::server) async fn handle_references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
        let td = params.text_document_position;
        let _workspace_read = self
            .workspace_read_with_prelude(Some(&td.text_document.uri))
            .await;
        let Some((document, semantics, subject, _)) =
            self.subject(&td.text_document.uri, td.position).await
        else {
            return Ok(None);
        };
        let locations = self
            .subject_locations(
                &document,
                &semantics,
                &subject,
                params.context.include_declaration,
            )
            .await;
        Ok(Some(locations))
    }

    pub(in crate::server) async fn handle_document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let td = params.text_document_position_params;
        let _workspace_read = self.document_read_with_prelude(&td.text_document.uri).await;
        let Some((document, semantics, subject, _)) =
            self.subject(&td.text_document.uri, td.position).await
        else {
            return Ok(None);
        };
        let mut ranges: Vec<TextRange> = covered(&semantics, &subject, true)
            .into_iter()
            .map(|(range, _)| range)
            .collect();
        if let Subject::Alias { alias, .. } = &subject {
            ranges.extend(alias_declarations(&document, alias));
        }
        ranges.sort_by_key(|range| (range.start, range.end));
        let highlights = ranges
            .into_iter()
            .map(|range| DocumentHighlight {
                range: adapters::range(&document, range),
                kind: Some(DocumentHighlightKind::TEXT),
            })
            .collect();
        Ok(Some(highlights))
    }

    pub(in crate::server) async fn handle_code_lens(
        &self,
        params: CodeLensParams,
    ) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let _workspace_read = self.document_read_with_prelude(&uri).await;
        let Some(document) = self.document(&uri) else {
            return Ok(None);
        };
        if !Self::supported(&document) {
            return Ok(None);
        }
        let Some(semantics) = self.semantics(&uri).await else {
            return Ok(None);
        };
        let Some(entry) = semantics.entry() else {
            return Ok(Some(Vec::new()));
        };
        let mut lenses = Vec::new();
        for declaration in semantics.declarations_in(&entry.path) {
            if declaration.kind != DeclarationKind::Def {
                continue;
            }
            let subject = Subject::Global(declaration.identity.clone());
            let locations: Vec<Location> = covered(&semantics, &subject, false)
                .into_iter()
                .map(|(range, _)| Location {
                    uri: uri.clone(),
                    range: adapters::range(&document, range),
                })
                .collect();
            if locations.is_empty() {
                continue;
            }
            let count = locations.len();
            let selection_range = adapters::range(&document, declaration.range);
            lenses.push(CodeLens {
                range: selection_range,
                command: Some(tower_lsp::lsp_types::Command {
                    title: format!("{count} reference{}", if count == 1 { "" } else { "s" }),
                    command: "editor.action.showReferences".into(),
                    arguments: Some(vec![
                        serde_json::json!(uri.as_str()),
                        serde_json::json!(selection_range.start),
                        serde_json::json!(locations),
                    ]),
                }),
                data: None,
            });
        }
        Ok(Some(lenses))
    }

    fn rename_refusal(document: &Document, offset: usize) -> Result<()> {
        if analysis::imports(document)
            .iter()
            .any(|import| import.path.start <= offset && offset <= import.path.end)
        {
            return Err(invalid(
                "Import paths cannot be renamed with Rename Symbol; rename the file or folder in the project tree instead",
            ));
        }
        Ok(())
    }

    pub(in crate::server) async fn handle_prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let _workspace_read = self.workspace_read_with_prelude(Some(&uri)).await;
        let Some(document) = self.document(&uri) else {
            return Ok(None);
        };
        Self::rename_refusal(&document, adapters::offset_at(&document, params.position))?;
        let Some((document, _, _, range)) = self.subject(&uri, params.position).await else {
            return Ok(None);
        };
        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: adapters::range(&document, range),
            placeholder: document.text[range.start..range.end].to_owned(),
        }))
    }

    pub(in crate::server) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> Result<Option<WorkspaceEdit>> {
        let td = params.text_document_position;
        let uri = td.text_document.uri;
        let _workspace_read = self.workspace_read_with_prelude(Some(&uri)).await;
        let Some(document) = self.document(&uri) else {
            return Ok(None);
        };
        Self::rename_refusal(&document, adapters::offset_at(&document, td.position))?;
        let Some((_, semantics, subject, _)) = self.subject(&uri, td.position).await else {
            return Ok(None);
        };
        let new_name = params.new_name;
        let dotted = matches!(subject, Subject::Global(_));
        if !is_name(&new_name, dotted) {
            return Err(invalid("The new name is not a valid Bend name"));
        }
        let documents = match &subject {
            Subject::Global(_) => self.workspace_semantics().await,
            Subject::Local(_) | Subject::Alias { .. } => vec![(uri.clone(), semantics.clone())],
        };
        let mut edits: HashMap<Url, Vec<TextRange>> = HashMap::new();
        for (document_uri, found) in &documents {
            for (range, implicit) in covered(found, &subject, true) {
                if implicit {
                    return Err(invalid(
                        "This declaration is used through an operator or a do block, which a rename cannot edit",
                    ));
                }
                edits.entry(document_uri.clone()).or_default().push(range);
            }
        }
        if let Subject::Alias { alias, .. } = &subject {
            edits
                .entry(uri.clone())
                .or_default()
                .extend(alias_declarations(&document, alias));
        }
        if let Subject::Global(identity) = &subject {
            let declared = file_uri(&identity.path);
            if !declared
                .is_some_and(|declared| documents.iter().any(|(found, _)| *found == declared))
            {
                return Err(invalid(
                    "This declaration is outside the workspace, so a rename cannot edit it",
                ));
            }
        }
        self.check_rename(&documents, &edits, &subject, &new_name)
            .await?;
        if matches!(subject, Subject::Alias { .. }) {
            let mut ranges = edits.remove(&uri).unwrap_or_default();
            ranges.sort_by_key(|range| (range.start, range.end));
            let alias_edits = ranges
                .into_iter()
                .map(|range| TextEdit {
                    range: adapters::range(&document, range),
                    new_text: new_name.clone(),
                })
                .collect();
            return Ok(Some(versioned_edit(&document, alias_edits)));
        }
        let changes = edits
            .into_iter()
            .filter_map(|(document_uri, ranges)| {
                let found = documents.iter().find(|(found, _)| *found == document_uri)?;
                let source = &found.1.entry()?.source;
                let edits = ranges
                    .into_iter()
                    .map(|range| TextEdit {
                        range: lsp_range(source, range),
                        new_text: new_name.clone(),
                    })
                    .collect();
                Some((document_uri, edits))
            })
            .collect();
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    /// Load each document again with the rename applied, and refuse it unless
    /// every name keeps its meaning: the renamed ones name the renamed
    /// declaration, and the others name what they named before.
    async fn check_rename(
        &self,
        documents: &[(Url, Arc<Semantics>)],
        edits: &HashMap<Url, Vec<TextRange>>,
        subject: &Subject,
        new_name: &str,
    ) -> Result<()> {
        let mut replaced: HashMap<PathBuf, String> = HashMap::new();
        for (document_uri, ranges) in edits {
            let Some((_, found)) = documents.iter().find(|(found, _)| found == document_uri) else {
                continue;
            };
            let (Some(entry), Some(path)) = (found.entry(), self.entry_path(document_uri)) else {
                continue;
            };
            let mut ranges = ranges.clone();
            ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
            ranges.dedup();
            let mut text = entry.source.clone();
            for range in ranges {
                text.replace_range(range.start..range.end, new_name);
            }
            replaced.insert(path, text);
        }
        let uris = documents.iter().map(|(found, _)| found.clone()).collect();
        let Some(after) = self.semantics_edited(uris, replaced).await else {
            return Err(invalid("The rename could not be checked"));
        };
        let renamed = |reading: Reading| match (reading, subject) {
            (Reading::Global(identity), Subject::Global(old)) if identity == *old => {
                Reading::Global(Identity {
                    name: new_name.to_owned(),
                    ..identity
                })
            }
            (other, _) => other,
        };
        for ((_, before), (_, after)) in documents.iter().zip(&after) {
            let old: Vec<Reading> = readings(before).into_iter().map(renamed).collect();
            let new: Vec<Reading> = readings(after);
            let same_error = before.error.as_ref().map(|error| &error.message)
                == after.error.as_ref().map(|error| &error.message);
            if old != new || !same_error {
                return Err(invalid(
                    "The rename would change what other names refer to, or stop a file from loading",
                ));
            }
        }
        Ok(())
    }
}
