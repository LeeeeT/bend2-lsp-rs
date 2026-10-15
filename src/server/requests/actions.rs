use super::super::{
    adapters,
    features::code_end_offset,
    lsp::Backend,
    semantics::{Reading, apply_edits, imports_keep_meaning, readings},
};
use super::import_edits::{
    auto_import_edits, organize_imports, unresolved_reference, versioned_edit,
};
use crate::{
    analysis,
    semantic::{Semantics, Target},
    workspace::Document,
};
use std::collections::{HashMap, HashSet};
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
        NumberOrString, Position, Range, TextEdit,
    },
};
use url::Url;

impl Backend {
    pub(in crate::server) async fn handle_code_action(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        if kind_requested(params.context.only.as_deref(), &CodeActionKind::QUICKFIX) {
            self.workspace.wait_for_discovery().await;
        }
        let _workspace_read = self.document_read(&params.text_document.uri).await;
        let uri = params.text_document.uri;
        let Some(doc) = self.document(&uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let requested_range = params.range;
        let mut actions = Vec::new();
        if kind_requested(
            params.context.only.as_deref(),
            &CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
        ) {
            let edits = organize_imports(&doc, &self.workspace.read());
            if !edits.is_empty() {
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Organize imports".into(),
                    kind: Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS),
                    edit: Some(versioned_edit(&doc, edits)),
                    ..Default::default()
                }));
            }
        }
        if !kind_requested(params.context.only.as_deref(), &CodeActionKind::QUICKFIX) {
            return Ok(Some(actions));
        }
        let semantics = self.semantics(&uri).await;
        if let Some(semantics) = &semantics {
            self.append_auto_import_actions(&doc, semantics, requested_range.start, &mut actions)
                .await;
        }
        for diagnostic in params.context.diagnostics {
            if (diagnostic.range.end.line, diagnostic.range.end.character)
                < (requested_range.start.line, requested_range.start.character)
                || (requested_range.end.line, requested_range.end.character)
                    < (
                        diagnostic.range.start.line,
                        diagnostic.range.start.character,
                    )
            {
                continue;
            }
            if diagnostic.code != Some(NumberOrString::String("parsing".into())) {
                continue;
            }
            let Some(open) = diagnostic
                .message
                .strip_prefix("Unclosed '")
                .and_then(|message| message.chars().next())
            else {
                continue;
            };
            let Some(close) = (match open {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                _ => None,
            }) else {
                continue;
            };
            let insertion = adapters::position_at(
                &doc,
                code_end_offset(&doc, adapters::offset_at(&doc, diagnostic.range.start)),
            );
            let edit = versioned_edit(
                &doc,
                vec![TextEdit {
                    range: Range::new(insertion, insertion),
                    new_text: close.to_string(),
                }],
            );
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Insert '{close}'"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic]),
                edit: Some(edit),
                is_preferred: Some(true),
                ..Default::default()
            }));
        }
        Ok(Some(actions))
    }

    async fn append_auto_import_actions(
        &self,
        doc: &Document,
        semantics: &Semantics,
        position: Position,
        actions: &mut Vec<CodeActionOrCommand>,
    ) {
        let offset = adapters::offset_at(doc, position);
        let Some(entry) = semantics.entry() else {
            return;
        };
        let Some(index) = entry.names.iter().position(|name| {
            !name.implicit && name.range.start <= offset && offset <= name.range.end
        }) else {
            return;
        };
        if !matches!(
            semantics.target(&entry.names[index]),
            Target::Unresolved { .. }
        ) {
            return;
        }
        let Some(reference) = unresolved_reference(doc, offset) else {
            return;
        };
        let name = doc.syntax.name_text(&doc.text, reference.name).to_owned();
        let before = self.semantics_before(&doc.uri, reference.range.start).await;
        let locals: Option<HashSet<String>> = before
            .as_ref()
            .map(|before| before.scope.iter().map(|(name, _)| name.clone()).collect());
        let candidates: Vec<(Url, String, Vec<TextEdit>)> = {
            let workspace = self.workspace.read();
            let base = self
                .compiler
                .base_module
                .read()
                .as_ref()
                .filter(|module| analysis::declaration_range(&module.snapshot, &name).is_some())
                .map(|module| (module.uri.clone(), "Base".to_owned()));
            workspace
                .import_candidates(&doc.uri, &name)
                .into_iter()
                .map(|(target, path)| (target.uri, path))
                .chain(base)
                .filter_map(|(target, path)| {
                    let edits = auto_import_edits(
                        doc,
                        &workspace,
                        &target,
                        &path,
                        reference,
                        locals.as_ref(),
                    )?;
                    Some((target, path, edits))
                })
                .collect()
        };
        for (target, path, edits) in candidates {
            if self
                .import_fixes(doc, semantics, index, &target, &edits)
                .await
            {
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Import {name} from {path}"),
                    kind: Some(CodeActionKind::QUICKFIX),
                    edit: Some(versioned_edit(doc, edits)),
                    ..Default::default()
                }));
            }
        }
    }

    /// Whether these edits make the unresolved name at `index` a declaration
    /// of `target`, and keep the meaning of every other name.
    async fn import_fixes(
        &self,
        doc: &Document,
        before: &Semantics,
        index: usize,
        target: &Url,
        edits: &[TextEdit],
    ) -> bool {
        let Some(path) = self.entry_path(&doc.uri) else {
            return false;
        };
        let ranges: Vec<_> = edits
            .iter()
            .map(|edit| (adapters::text_range(doc, edit.range), edit.new_text.clone()))
            .collect();
        let Some(entry) = before.entry() else {
            return false;
        };
        let text = apply_edits(&entry.source, &ranges);
        let replaced = HashMap::from([(path, text)]);
        let Some(after) = self.semantics_edited(vec![doc.uri.clone()], replaced).await else {
            return false;
        };
        let Some((_, after)) = after.first() else {
            return false;
        };
        let target = target
            .to_file_path()
            .ok()
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path));
        let fixed = matches!(
            readings(after).get(index),
            Some(Reading::Global(identity)) if Some(&identity.path) == target.as_ref()
        );
        fixed && imports_keep_meaning(before, after)
    }
}

fn kind_requested(only: Option<&[CodeActionKind]>, kind: &CodeActionKind) -> bool {
    only.is_none_or(|only| {
        only.iter().any(|requested| {
            let requested = requested.as_str();
            let kind = kind.as_str();
            requested.is_empty()
                || kind == requested
                || kind
                    .strip_prefix(requested)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    })
}
