use super::super::{
    adapters,
    features::{cursor_in_comment_or_string, static_hover, token_at},
    lsp::Backend,
    semantics::{module_at, name_at},
};
use crate::analysis;
use crate::semantic::{DeclarationKind, Identity, Semantics, Target};
use crate::workspace::Document;
use std::path::{Path, PathBuf};
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        DocumentLink, DocumentLinkParams, GotoDefinitionParams, GotoDefinitionResponse, Hover,
        HoverContents, HoverParams, Location, MarkupContent, MarkupKind, Range,
    },
};
use url::Url;

fn markdown(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

fn scalar(location: Location) -> GotoDefinitionResponse {
    GotoDefinitionResponse::Scalar(location)
}

impl Backend {
    fn module_location(&self, from: &Url, path: &Path) -> Option<Location> {
        Some(Location {
            uri: self.path_uri(from, path)?,
            range: Range::default(),
        })
    }

    pub(in crate::server) fn available_import_targets(&self, document: &Document) -> Vec<String> {
        self.workspace
            .read()
            .available_import_targets(&document.uri)
    }

    fn base_path(&self) -> Option<PathBuf> {
        let module = self.compiler.base_module.read().clone()?;
        let path = module.uri.to_file_path().ok()?;
        Some(std::fs::canonicalize(&path).unwrap_or(path))
    }

    /// The file that an import of the document loads, as the compiler loads it.
    fn import_path(
        &self,
        semantics: &Semantics,
        document: &Document,
        import: &analysis::IndexedImport,
    ) -> Option<PathBuf> {
        if import.path_text(&document.text) == "Base" && import.alias.is_none() {
            let base = self.base_path()?;
            return semantics.file(&base).map(|file| file.path.clone());
        }
        let alias = import.alias_text(&document.text)?;
        let (_, sub) = semantics
            .entry_aliases
            .iter()
            .find(|(name, _)| name == alias)?;
        semantics
            .files
            .iter()
            .find(|file| file.ns == *sub)
            .map(|file| file.path.clone())
    }

    pub(in crate::server) async fn handle_document_link(
        &self,
        params: DocumentLinkParams,
    ) -> Result<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let _workspace_read = self.document_read_with_prelude(&uri).await;
        let Some(doc) = self.document(&uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let Some(semantics) = self.semantics(&uri).await else {
            return Ok(None);
        };
        let mut links = Vec::new();
        for import in analysis::imports(&doc) {
            let path = import.path_text(&doc.text);
            let target = self
                .import_path(&semantics, &doc, import)
                .and_then(|target| self.path_uri(&doc.uri, &target));
            if let Some(target) = target {
                links.push(DocumentLink {
                    range: adapters::range(&doc, import.path),
                    target: Some(target),
                    tooltip: Some(format!("Open {path}")),
                    data: None,
                });
            }
        }
        Ok(Some(links))
    }

    pub(in crate::server) async fn handle_hover(
        &self,
        params: HoverParams,
    ) -> Result<Option<Hover>> {
        let td = params.text_document_position_params;
        let _workspace_read = self.document_read_with_prelude(&td.text_document.uri).await;
        let Some(doc) = self.document(&td.text_document.uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let offset = adapters::offset_at(&doc, td.position);
        if cursor_in_comment_or_string(&doc, offset) {
            return Ok(None);
        }
        let semantics = self.semantics(&td.text_document.uri).await;
        let value = semantics.as_deref().and_then(|semantics| {
            let name = name_at(semantics, offset)?;
            match semantics.target(name) {
                Target::Local { binder, .. } => {
                    let entry = semantics.entry()?;
                    let text = entry.source.get(name.range.start..name.range.end)?;
                    let ty = semantics.binder_type(binder)?;
                    Some(format!("```bend\n{text}: {ty}\n```"))
                }
                Target::Global(identity) => semantics
                    .declaration(&identity)
                    .map(|declaration| format!("```bend\n{}\n```", declaration.detail)),
                Target::Unresolved { .. } => None,
            }
        });
        let value = value.or_else(|| static_hover(&token_at(&doc, offset)));
        Ok(value.map(markdown))
    }

    pub(in crate::server) async fn handle_goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let td = params.text_document_position_params;
        let _workspace_read = self.document_read_with_prelude(&td.text_document.uri).await;
        let Some(doc) = self.document(&td.text_document.uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let offset = adapters::offset_at(&doc, td.position);
        if cursor_in_comment_or_string(&doc, offset) {
            return Ok(None);
        }
        let Some(semantics) = self.semantics(&td.text_document.uri).await else {
            return Ok(None);
        };
        let import = analysis::imports(&doc).iter().find(|import| {
            let contains =
                |range: analysis::TextRange| range.start <= offset && offset <= range.end;
            contains(import.path) || import.alias.is_some_and(contains)
        });
        if let Some(import) = import {
            let target = self.import_path(&semantics, &doc, import);
            return Ok(target
                .and_then(|path| self.module_location(&doc.uri, &path))
                .map(scalar));
        }
        let Some(name) = name_at(&semantics, offset) else {
            return Ok(None);
        };
        if let Some(module) = module_at(&semantics, name, offset) {
            return Ok(self.module_location(&doc.uri, &module).map(scalar));
        }
        let response = match semantics.target(name) {
            Target::Local { range, .. } => range.map(|range| Location {
                uri: doc.uri.clone(),
                range: adapters::range(&doc, range),
            }),
            Target::Global(identity) => self.location(&doc.uri, &semantics, &identity),
            Target::Unresolved { .. } => None,
        };
        Ok(response.map(scalar))
    }

    pub(in crate::server) async fn handle_goto_type_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let td = params.text_document_position_params;
        let _workspace_read = self.document_read_with_prelude(&td.text_document.uri).await;
        let Some(doc) = self.document(&td.text_document.uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let offset = adapters::offset_at(&doc, td.position);
        if cursor_in_comment_or_string(&doc, offset) {
            return Ok(None);
        }
        let Some(semantics) = self.semantics(&td.text_document.uri).await else {
            return Ok(None);
        };
        let Some(name) = name_at(&semantics, offset) else {
            return Ok(None);
        };
        let identity = match semantics.target(name) {
            Target::Local { binder, .. } => {
                let Some(range) = semantics.binder_type_range(binder) else {
                    return Ok(None);
                };
                type_in(&semantics, range)
            }
            Target::Global(identity) => type_of(&semantics, &identity),
            Target::Unresolved { .. } => None,
        };
        Ok(identity
            .and_then(|identity| self.location(&doc.uri, &semantics, &identity))
            .map(scalar))
    }
}

/// The type that a declaration names: a type itself, or the type of a
/// constructor.
fn type_of(semantics: &Semantics, identity: &Identity) -> Option<Identity> {
    let declaration = semantics.declaration(identity)?;
    match declaration.kind {
        DeclarationKind::Type => Some(identity.clone()),
        DeclarationKind::Constructor => semantics
            .holder(declaration.tld)
            .map(|holder| holder.identity.clone()),
        DeclarationKind::Def | DeclarationKind::Law => None,
    }
}

/// The first type that a type annotation of the entry names.
fn type_in(semantics: &Semantics, range: analysis::TextRange) -> Option<Identity> {
    let entry = semantics.entry()?;
    entry
        .names
        .iter()
        .filter(|name| range.start <= name.range.start && name.range.end <= range.end)
        .filter(|name| !name.implicit)
        .find_map(|name| match semantics.target(name) {
            Target::Global(identity) => type_of(semantics, &identity),
            _ => None,
        })
}
