use super::super::{adapters, features::cursor_in_comment_or_string, lsp::Backend};
use super::import_edits::{auto_import_plan, import_path_edit};
use crate::{
    analysis,
    semantic::{Declaration, DeclarationKind, Semantics, Target},
    workspace::Document,
};
use std::collections::{HashMap, HashSet};
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionList,
        CompletionParams, CompletionResponse, CompletionTextEdit, InlayHint, InlayHintKind,
        InlayHintLabel, InlayHintParams, SignatureHelp, SignatureHelpParams, TextEdit,
    },
};

const KEYWORDS: [&str; 12] = [
    "def", "type", "law", "match", "case", "do", "return", "for", "exs", "where", "is", "import",
];

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

/// The range of the name that the cursor is in or at the end of.
fn name_run(text: &str, offset: usize) -> analysis::TextRange {
    let bytes = text.as_bytes();
    let mut start = offset.min(bytes.len());
    while start > 0 && bytes.get(start - 1).copied().is_some_and(is_name_byte) {
        start -= 1;
    }
    let mut end = offset.min(bytes.len());
    while bytes.get(end).copied().is_some_and(is_name_byte) {
        end += 1;
    }
    analysis::TextRange::new(start, end)
}

/// The name typed before the cursor. After a dot, completion offers the rest
/// of each spelling that starts with the typed qualifier, and replaces only the
/// text after the dot.
struct Typed<'a> {
    whole: &'a str,
    qualifier: &'a str,
    member: &'a str,
    range: analysis::TextRange,
}

impl<'a> Typed<'a> {
    fn new(text: &'a str, offset: usize) -> Self {
        let run = name_run(text, offset);
        let whole = &text[run.start..offset];
        let split = whole.rfind('.').map_or(0, |dot| dot + 1);
        Self {
            whole,
            qualifier: &whole[..split],
            member: &whole[split..],
            range: analysis::TextRange::new(run.start + split, run.end),
        }
    }

    /// The label of a spelling: its rest after the typed qualifier, when the
    /// rest matches the typed member.
    fn label<'s>(&self, spelling: &'s str) -> Option<&'s str> {
        let rest = spelling.strip_prefix(self.qualifier)?;
        analysis::completion_match(rest, self.member).map(|_| rest)
    }
}

fn declaration_item(label: String, declaration: &Declaration) -> CompletionItem {
    let mut item = CompletionItem::new_simple(label, declaration.detail.clone());
    item.kind = Some(match declaration.kind {
        DeclarationKind::Def | DeclarationKind::Law => CompletionItemKind::FUNCTION,
        DeclarationKind::Type => CompletionItemKind::STRUCT,
        DeclarationKind::Constructor => CompletionItemKind::CONSTRUCTOR,
    });
    item
}

/// Whether a partial load stopped at the end of its entry's text, so that
/// its binders in scope are the scope there.
fn stopped_at_end(before: &Semantics) -> bool {
    let Some(entry) = before.entry() else {
        return false;
    };
    before.error.as_ref().is_some_and(|error| {
        error.at.as_ref().is_some_and(|(path, range)| {
            *path == entry.path && range.start >= entry.source.trim_end().len()
        })
    })
}

/// The locals in scope where a partial load stopped, innermost first.
fn local_items(
    before: &Semantics,
    typed: &Typed,
    labels: &mut HashSet<String>,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    if !stopped_at_end(before) {
        return items;
    }
    for (name, binder) in before.scope.iter().rev() {
        if name.is_empty()
            || name == "_"
            || typed.label(name).is_none()
            || !labels.insert(name.clone())
        {
            continue;
        }
        let detail = before
            .binder_type(*binder)
            .map_or_else(|| "Local binding".to_owned(), |ty| format!("{name}: {ty}"));
        let mut item = CompletionItem::new_simple(name.clone(), detail);
        item.kind = Some(CompletionItemKind::VARIABLE);
        items.push(item);
    }
    items
}

/// The declarations that a name typed where a partial load stopped can name:
/// those it sees, and the entry's own later declarations, which an unknown
/// name of the entry names.
fn declaration_items(
    before: &Semantics,
    whole: Option<&Semantics>,
    typed: &Typed,
    pattern: bool,
    labels: &mut HashSet<String>,
) -> Vec<CompletionItem> {
    let mut offered: Vec<(String, &Declaration)> = before.visible();
    if let Some(whole) = whole
        && let Some(entry) = whole.entry()
    {
        for declaration in whole.declarations_in(&entry.path) {
            let (_, name) = declaration
                .key
                .split_once(':')
                .unwrap_or(("", &declaration.key));
            offered.push((name.to_owned(), declaration));
        }
    }
    let mut items = Vec::new();
    for (spelling, declaration) in offered {
        let wanted = !pattern || declaration.kind == DeclarationKind::Constructor;
        if wanted
            && let Some(label) = typed.label(&spelling)
            && labels.insert(label.to_owned())
        {
            items.push(declaration_item(label.to_owned(), declaration));
        }
    }
    items
}

fn keyword_items(typed: &Typed, labels: &mut HashSet<String>) -> Vec<CompletionItem> {
    KEYWORDS
        .iter()
        .filter(|keyword| typed.label(keyword).is_some())
        .filter(|keyword| labels.insert((**keyword).to_owned()))
        .map(|keyword| {
            let mut item = CompletionItem::new_simple((*keyword).to_owned(), "Keyword".into());
            item.kind = Some(CompletionItemKind::KEYWORD);
            item
        })
        .collect()
}

fn alias_items(doc: &Document, typed: &Typed, labels: &mut HashSet<String>) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for import in analysis::imports(doc) {
        if let Some(alias) = import
            .alias_text(&doc.text)
            .filter(|alias| typed.label(alias).is_some())
            && labels.insert(alias.to_owned())
        {
            let mut item = CompletionItem::new_simple(
                alias.to_owned(),
                format!("Imported module {}", import.path_text(&doc.text)),
            );
            item.kind = Some(CompletionItemKind::MODULE);
            items.push(item);
        }
    }
    items
}

impl Backend {
    pub(in crate::server) async fn handle_completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let discovery_incomplete = {
            // Preserve preceding watcher updates without waiting for the
            // background initial scan.
            let _serial = self.workspace.update_serial.lock().await;
            self.workspace.discovery_pending()
        };
        let td = params.text_document_position;
        let uri = td.text_document.uri;
        let _workspace_read = self.document_read_with_prelude(&uri).await;
        let Some(doc) = self.document(&uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let offset = adapters::offset_at(&doc, td.position);
        if cursor_in_comment_or_string(&doc, offset) {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        }
        if let Some(path) = doc.syntax.completion_import_path(offset) {
            let prefix = &doc.text[path.start..offset];
            let items = self
                .available_import_targets(&doc)
                .into_iter()
                .filter(|target| adapters::import_completion_match(target, prefix).is_some())
                .filter_map(|target| {
                    let edit = import_path_edit(&doc, path, &target)?;
                    let mut item = CompletionItem::new_simple(target, "Available import".into());
                    item.kind = Some(CompletionItemKind::MODULE);
                    item.text_edit = Some(CompletionTextEdit::Edit(edit));
                    Some(item)
                })
                .collect();
            // Import filtering and file-vs-path metadata depend on the current
            // query. A complete array lets Zed reuse stale items after typing.
            return Ok(Some(CompletionResponse::List(CompletionList {
                is_incomplete: true,
                items: adapters::finish_import_completion_items(&doc, path, prefix, items),
            })));
        }
        let typed = Typed::new(&doc.text, offset);
        let pattern = doc.syntax.in_case_pattern(offset);
        let before = self
            .semantics_before(&uri, offset - typed.whole.len())
            .await;
        let whole = self.semantics(&uri).await;
        let mut labels = HashSet::new();
        let mut items = Vec::new();
        if let Some(before) = &before {
            if !pattern {
                items.extend(local_items(before, &typed, &mut labels));
            }
            items.extend(declaration_items(
                before,
                whole.as_deref(),
                &typed,
                pattern,
                &mut labels,
            ));
        }
        if !pattern {
            items.extend(alias_items(&doc, &typed, &mut labels));
            items.extend(keyword_items(&typed, &mut labels));
        }
        let locals: Option<HashSet<String>> = before
            .as_ref()
            .filter(|before| stopped_at_end(before))
            .map(|before| before.scope.iter().map(|(name, _)| name.clone()).collect());
        items.extend(
            self.import_completion_items(&doc, &typed, pattern, locals.as_ref())
                .await,
        );
        Ok(Some(CompletionResponse::List(CompletionList {
            is_incomplete: discovery_incomplete,
            items: adapters::finish_completion_items(&doc, typed.range, typed.member, items),
        })))
    }

    /// Declarations of other files, with the import that makes each visible.
    /// Each import edit is checked once: the document must keep the meaning
    /// of its names with it.
    async fn import_completion_items(
        &self,
        doc: &Document,
        typed: &Typed<'_>,
        in_case_pattern: bool,
        locals: Option<&HashSet<String>>,
    ) -> Vec<CompletionItem> {
        let prefix = typed.whole;
        let mut planned: Vec<(CompletionItem, Option<TextEdit>)> = Vec::new();
        {
            let workspace = self.workspace.read();
            let candidates =
                workspace.import_completion_candidates(&doc.uri, prefix, in_case_pattern);
            let base = self.compiler.base_module.read().clone().filter(|_| {
                !doc.syntax
                    .imports()
                    .iter()
                    .any(|import| import.path_text(&doc.text) == "Base")
            });
            let base_candidates = base.map(|module| {
                let completions = if in_case_pattern {
                    analysis::constructor_completion_items(&module.snapshot, prefix)
                } else {
                    analysis::module_completion_items(&module.snapshot, prefix)
                };
                (
                    Document::with_snapshot(module.uri, "bend".into(), module.snapshot),
                    "Base".to_owned(),
                    completions,
                )
            });
            for (target, path, completions) in candidates.into_iter().chain(base_candidates) {
                for mut item in adapters::completion_items(completions) {
                    let Some(plan) =
                        auto_import_plan(doc, &workspace, &target.uri, &path, &item.label, locals)
                    else {
                        continue;
                    };
                    let insert = if typed.qualifier.is_empty() {
                        plan.qualified_name
                    } else if let Some(rest) = typed.label(&plan.qualified_name) {
                        rest.clone_into(&mut item.label);
                        rest.to_owned()
                    } else {
                        continue;
                    };
                    let detail = item.detail.get_or_insert_with(String::new);
                    detail.push_str(" — from ");
                    detail.push_str(&path);
                    item.label_details = Some(CompletionItemLabelDetails {
                        detail: None,
                        description: Some(path.clone()),
                    });
                    item.insert_text = Some(insert);
                    item.sort_text = adapters::completion_sort_text(&item, typed.member, true);
                    planned.push((item, plan.import_edit));
                }
            }
        }
        let mut verdicts: HashMap<String, bool> = HashMap::new();
        let mut items = Vec::with_capacity(planned.len());
        for (mut item, edit) in planned {
            if let Some(edit) = &edit {
                let key = format!("{:?}{}", edit.range, edit.new_text);
                let keeps = if let Some(keeps) = verdicts.get(&key) {
                    *keeps
                } else {
                    let range = adapters::text_range(doc, edit.range);
                    let edits = [(range, edit.new_text.clone())];
                    let keeps = self.import_keeps_meaning(&doc.uri, &edits).await;
                    verdicts.insert(key, keeps);
                    keeps
                };
                if !keeps {
                    continue;
                }
            }
            item.additional_text_edits = edit.map(|edit| vec![edit]);
            items.push(item);
        }
        items
    }

    pub(in crate::server) async fn handle_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
        let td = params.text_document_position_params;
        let uri = td.text_document.uri;
        let _workspace_read = self.document_read_with_prelude(&uri).await;
        let Some(doc) = self.document(&uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let offset = adapters::offset_at(&doc, td.position);
        let Some((_, active, _, callee)) = doc.syntax.active_call(&doc.text, offset) else {
            return Ok(None);
        };
        let Some(head) = doc.syntax.token(callee).map(|token| token.range) else {
            return Ok(None);
        };
        let Some(before) = self.semantics_before(&uri, offset).await else {
            return Ok(None);
        };
        let Some(declaration) = callee_declaration(&before, head.start) else {
            return Ok(None);
        };
        let parameters: Vec<String> = declaration
            .parameters
            .iter()
            .map(|(name, ty)| {
                ty.as_ref()
                    .map_or_else(|| name.clone(), |ty| format!("{name}: {ty}"))
            })
            .collect();
        let active = usize::try_from(active).unwrap_or(usize::MAX);
        Ok(Some(adapters::signature_help(analysis::SignatureHelp {
            label: declaration.detail.clone(),
            active_parameter: active.min(parameters.len().saturating_sub(1)),
            parameters,
        })))
    }

    pub(in crate::server) async fn handle_inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
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
        let range = adapters::text_range(&doc, params.range);
        let mut hints = Vec::new();
        for call in doc.syntax.calls() {
            if call.call_range.end < range.start || range.end < call.call_range.start {
                continue;
            }
            let Some(declaration) = callee_declaration(&semantics, call.callee_range.start) else {
                continue;
            };
            let arguments = doc.syntax.call_arguments(call);
            let templated = arguments
                .first()
                .and_then(|argument| doc.text.get(argument.start..argument.end))
                .is_some_and(|text| text.trim_start().starts_with('~'));
            let skip = if templated { 0 } else { declaration.templates };
            let names = declaration.parameters.iter().skip(skip);
            for (argument, (name, _)) in arguments.iter().zip(names) {
                if !range.contains(argument.start) {
                    continue;
                }
                hints.push(InlayHint {
                    position: adapters::position_at(&doc, argument.start),
                    label: InlayHintLabel::String(format!("{name}: ")),
                    kind: Some(InlayHintKind::PARAMETER),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                });
            }
        }
        hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
        Ok(Some(hints))
    }
}

/// The def or law that the name starting at `offset` calls.
fn callee_declaration(semantics: &Semantics, offset: usize) -> Option<&Declaration> {
    let entry = semantics.entry()?;
    let name = entry
        .names
        .iter()
        .find(|name| !name.implicit && name.range.start == offset)?;
    let Target::Global(identity) = semantics.target(name) else {
        return None;
    };
    semantics.declaration(&identity).filter(|declaration| {
        matches!(
            declaration.kind,
            DeclarationKind::Def | DeclarationKind::Law
        )
    })
}
