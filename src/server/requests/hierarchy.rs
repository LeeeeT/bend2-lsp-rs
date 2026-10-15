use super::super::{
    adapters,
    features::cursor_in_comment_or_string,
    lsp::Backend,
    semantics::{file_uri, lsp_range, name_at, named_range},
};
use crate::frontend::Meaning;
use crate::semantic::{Declaration, DeclarationKind, Identity, Semantics, Target};
use std::collections::BTreeMap;
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
        CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
        Position, Range, SymbolKind, TypeHierarchyItem, TypeHierarchyPrepareParams,
        TypeHierarchySubtypesParams, TypeHierarchySupertypesParams,
    },
};
use url::Url;

fn symbol_kind(kind: DeclarationKind) -> SymbolKind {
    match kind {
        DeclarationKind::Def | DeclarationKind::Law => SymbolKind::FUNCTION,
        DeclarationKind::Type => SymbolKind::STRUCT,
        DeclarationKind::Constructor => SymbolKind::CONSTRUCTOR,
    }
}

/// The URI and range of a declaration of a loaded file.
fn place(semantics: &Semantics, declaration: &Declaration) -> Option<(Url, Range)> {
    let file = semantics.file(&declaration.identity.path)?;
    let uri = file_uri(&declaration.identity.path)?;
    Some((uri, lsp_range(&file.source, declaration.range)))
}

fn call_item(semantics: &Semantics, declaration: &Declaration) -> Option<CallHierarchyItem> {
    let (uri, range) = place(semantics, declaration)?;
    Some(CallHierarchyItem {
        name: declaration.identity.name.clone(),
        kind: symbol_kind(declaration.kind),
        tags: None,
        detail: Some(declaration.detail.clone()),
        uri,
        range,
        selection_range: range,
        data: None,
    })
}

fn type_item(semantics: &Semantics, declaration: &Declaration) -> Option<TypeHierarchyItem> {
    let (uri, range) = place(semantics, declaration)?;
    Some(TypeHierarchyItem {
        name: declaration.identity.name.clone(),
        kind: symbol_kind(declaration.kind),
        tags: None,
        detail: Some(declaration.detail.clone()),
        uri,
        range,
        selection_range: range,
        data: None,
    })
}

fn is_function(declaration: &Declaration) -> bool {
    matches!(
        declaration.kind,
        DeclarationKind::Def | DeclarationKind::Law
    )
}

impl Backend {
    /// The declaration that the cursor names.
    async fn declaration_at(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<(std::sync::Arc<Semantics>, Identity)> {
        let document = self.document(uri)?;
        if !Self::supported(&document) {
            return None;
        }
        let offset = adapters::offset_at(&document, position);
        if cursor_in_comment_or_string(&document, offset) {
            return None;
        }
        let semantics = self.semantics(uri).await?;
        let Target::Global(identity) = semantics.target(name_at(&semantics, offset)?) else {
            return None;
        };
        Some((semantics, identity))
    }

    /// The semantics of the file that declares an item, and the item.
    async fn item_declaration(
        &self,
        item_uri: &Url,
        name: &str,
        kind: SymbolKind,
    ) -> Option<(std::sync::Arc<Semantics>, Identity)> {
        let semantics = self.semantics(item_uri).await?;
        let path = semantics.entry()?.path.clone();
        let identity = Identity {
            path,
            name: name.to_owned(),
            ctr: kind == SymbolKind::CONSTRUCTOR,
        };
        semantics.declaration(&identity)?;
        Some((semantics, identity))
    }

    pub(in crate::server) async fn handle_prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let td = params.text_document_position_params;
        let _workspace_read = self.document_read_with_prelude(&td.text_document.uri).await;
        let Some((semantics, identity)) = self
            .declaration_at(&td.text_document.uri, td.position)
            .await
        else {
            return Ok(None);
        };
        let item = semantics
            .declaration(&identity)
            .filter(|declaration| is_function(declaration))
            .and_then(|declaration| call_item(&semantics, declaration));
        Ok(item.map(|item| vec![item]))
    }

    pub(in crate::server) async fn handle_incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let _workspace_read = self.workspace_read_with_prelude(None).await;
        let item = params.item;
        let Some((_, target)) = self
            .item_declaration(&item.uri, &item.name, item.kind)
            .await
        else {
            return Ok(Some(Vec::new()));
        };
        let mut calls: BTreeMap<(String, String), (CallHierarchyItem, Vec<Range>)> =
            BTreeMap::new();
        for (_, semantics) in self.workspace_semantics().await {
            let Some(entry) = semantics.entry() else {
                continue;
            };
            for name in &entry.names {
                if matches!(name.meaning, Meaning::Declaration { .. })
                    || semantics.target(name) != Target::Global(target.clone())
                {
                    continue;
                }
                let Some(caller) = semantics.holder(name.tld) else {
                    continue;
                };
                let Some(caller_item) = call_item(&semantics, caller) else {
                    continue;
                };
                let key = (caller_item.uri.to_string(), caller_item.name.clone());
                let range = lsp_range(&entry.source, named_range(&entry.source, name, &target));
                calls
                    .entry(key)
                    .or_insert_with(|| (caller_item, Vec::new()))
                    .1
                    .push(range);
            }
        }
        Ok(Some(
            calls
                .into_values()
                .map(|(from, from_ranges)| CallHierarchyIncomingCall { from, from_ranges })
                .collect(),
        ))
    }

    pub(in crate::server) async fn handle_outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let _workspace_read = self
            .workspace_read_with_prelude(Some(&params.item.uri))
            .await;
        let item = params.item;
        let Some((semantics, origin)) = self
            .item_declaration(&item.uri, &item.name, item.kind)
            .await
        else {
            return Ok(Some(Vec::new()));
        };
        let Some(entry) = semantics.entry() else {
            return Ok(Some(Vec::new()));
        };
        let mut calls: BTreeMap<(String, String), (CallHierarchyItem, Vec<Range>)> =
            BTreeMap::new();
        for name in &entry.names {
            if semantics.holder(name.tld).map(|holder| &holder.identity) != Some(&origin) {
                continue;
            }
            let Target::Global(callee) = semantics.target(name) else {
                continue;
            };
            let Some(declaration) = semantics
                .declaration(&callee)
                .filter(|declaration| is_function(declaration) && callee != origin)
            else {
                continue;
            };
            let Some(callee_item) = call_item(&semantics, declaration) else {
                continue;
            };
            let key = (callee_item.uri.to_string(), callee_item.name.clone());
            let range = lsp_range(&entry.source, named_range(&entry.source, name, &callee));
            calls
                .entry(key)
                .or_insert_with(|| (callee_item, Vec::new()))
                .1
                .push(range);
        }
        Ok(Some(
            calls
                .into_values()
                .map(|(to, from_ranges)| CallHierarchyOutgoingCall { to, from_ranges })
                .collect(),
        ))
    }

    pub(in crate::server) async fn handle_prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let td = params.text_document_position_params;
        let _workspace_read = self.document_read_with_prelude(&td.text_document.uri).await;
        let Some((semantics, identity)) = self
            .declaration_at(&td.text_document.uri, td.position)
            .await
        else {
            return Ok(None);
        };
        let item = semantics
            .declaration(&identity)
            .filter(|declaration| {
                matches!(
                    declaration.kind,
                    DeclarationKind::Type | DeclarationKind::Constructor
                )
            })
            .and_then(|declaration| type_item(&semantics, declaration));
        Ok(item.map(|item| vec![item]))
    }

    pub(in crate::server) async fn handle_supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let _workspace_read = self.document_read_with_prelude(&params.item.uri).await;
        let item = params.item;
        let Some((semantics, identity)) = self
            .item_declaration(&item.uri, &item.name, item.kind)
            .await
        else {
            return Ok(Some(Vec::new()));
        };
        let parent = semantics
            .declaration(&identity)
            .filter(|declaration| declaration.kind == DeclarationKind::Constructor)
            .and_then(|declaration| semantics.holder(declaration.tld))
            .and_then(|parent| type_item(&semantics, parent));
        Ok(Some(parent.into_iter().collect()))
    }

    pub(in crate::server) async fn handle_subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let _workspace_read = self.document_read_with_prelude(&params.item.uri).await;
        let item = params.item;
        let Some((semantics, identity)) = self
            .item_declaration(&item.uri, &item.name, item.kind)
            .await
        else {
            return Ok(Some(Vec::new()));
        };
        let Some(declaration) = semantics
            .declaration(&identity)
            .filter(|declaration| declaration.kind == DeclarationKind::Type)
        else {
            return Ok(Some(Vec::new()));
        };
        let children = semantics
            .declarations_in(&identity.path)
            .filter(|child| {
                child.kind == DeclarationKind::Constructor && child.tld == declaration.tld
            })
            .filter_map(|child| type_item(&semantics, child))
            .collect();
        Ok(Some(children))
    }
}
