use super::super::{adapters, lsp::Backend};
use crate::{
    analysis::{self, TokenId, TokenKind},
    frontend::Meaning,
    semantic::{DeclarationKind, Semantics, Target},
    workspace::Document,
};
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        FoldingRange, FoldingRangeParams, SelectionRange, SelectionRangeParams, SemanticTokens,
        SemanticTokensParams, SemanticTokensResult,
    },
};

impl Backend {
    pub(in crate::server) async fn handle_folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let _workspace_read = self.document_read(&params.text_document.uri).await;
        let Some(doc) = self.document(&params.text_document.uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        Ok(Some(adapters::folding_ranges(analysis::folding_ranges(
            &doc,
        ))))
    }

    pub(in crate::server) async fn handle_selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let _workspace_read = self.document_read(&params.text_document.uri).await;
        let Some(doc) = self.document(&params.text_document.uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        Ok(Some(
            params
                .positions
                .into_iter()
                .map(|position| {
                    let offset = adapters::offset_at(&doc, position);
                    let selection = analysis::selection_range(&doc, offset);
                    adapters::selection_range(&doc, selection)
                })
                .collect(),
        ))
    }

    pub(in crate::server) async fn handle_semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let _workspace_read = self.document_read_with_prelude(&uri).await;
        let Some(doc) = self.document(&uri) else {
            return Ok(None);
        };
        if !Self::supported(&doc) {
            return Ok(None);
        }
        let semantics = self.semantics(&uri).await;
        let tokens = semantic_tokens(&doc, semantics.as_deref());
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: adapters::semantic_tokens(&doc, tokens),
        })))
    }
}

const NAMESPACE: u32 = 0;
const TYPE: u32 = 1;
const VARIABLE: u32 = 8;
const ENUM_MEMBER: u32 = 10;
const FUNCTION: u32 = 12;
const KEYWORD: u32 = 15;
const COMMENT: u32 = 17;
const STRING: u32 = 18;
const NUMBER: u32 = 19;
const OPERATOR: u32 = 20;

/// The class of an identifier token from what its name means. A token with
/// no meaning, as in a declaration that does not parse, gets no class.
fn identifier_class(semantics: &Semantics, token: analysis::TextRange) -> Option<u32> {
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
        return Some(NAMESPACE);
    }
    match &name.meaning {
        Meaning::Binder(_) | Meaning::Local(_) => return Some(VARIABLE),
        Meaning::Global { .. } | Meaning::Declaration { .. } => {}
    }
    let Target::Global(identity) = semantics.target(name) else {
        return None;
    };
    Some(match semantics.declaration(&identity)?.kind {
        DeclarationKind::Def | DeclarationKind::Law => FUNCTION,
        DeclarationKind::Type => TYPE,
        DeclarationKind::Constructor => ENUM_MEMBER,
    })
}

fn semantic_tokens(
    document: &Document,
    semantics: Option<&Semantics>,
) -> Vec<analysis::SemanticToken> {
    let syntax = &document.syntax;
    let mut result = Vec::new();
    for (index, token) in syntax.tokens().iter().enumerate() {
        let token_type = match token.kind {
            TokenKind::Comment => Some(COMMENT),
            TokenKind::StringLiteral => Some(STRING),
            TokenKind::Number => Some(NUMBER),
            TokenKind::Operator => Some(OPERATOR),
            TokenKind::Identifier => {
                let keyword = syntax
                    .name_for_token(TokenId(index))
                    .is_some_and(|name| syntax.is_keyword(name));
                if keyword {
                    Some(KEYWORD)
                } else {
                    semantics.and_then(|semantics| identifier_class(semantics, token.range))
                }
            }
            TokenKind::Punctuation | TokenKind::Delimiter => None,
        };
        if let Some(token_type) = token_type {
            result.push(analysis::SemanticToken {
                range: token.range,
                token_type,
            });
        }
    }
    result
}
