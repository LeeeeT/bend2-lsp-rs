use super::super::{adapters, lsp::Backend};
use crate::{analysis, highlight};
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
        let found = highlight::highlights(&doc.syntax, semantics.as_deref());
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: adapters::semantic_tokens(&doc, found),
        })))
    }
}
