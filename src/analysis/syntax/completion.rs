use super::{SyntaxIndex, TextRange, TokenId};

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct CompletionIndex {
    imports: Box<[TextRange]>,
    cases: Box<[TextRange]>,
}

impl CompletionIndex {
    pub(super) fn build(source: &str, syntax: &SyntaxIndex) -> Self {
        if ["import", "case"]
            .iter()
            .all(|keyword| syntax.name_id(source, keyword).is_none())
        {
            return Self::default();
        }
        let mut imports = Vec::new();
        let mut cases = Vec::new();
        for (line_number, line) in syntax.lines.iter().enumerate() {
            let code = &source[line.code.start..line.code.end];
            if code.is_empty() || code.starts_with('#') {
                continue;
            }
            if !["import", "case"].iter().any(|keyword| {
                code.strip_prefix(*keyword)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
            }) {
                continue;
            }
            let first = syntax
                .tokens
                .partition_point(|token| token.range.start < line.code.start);
            let limit = syntax
                .tokens
                .partition_point(|token| token.range.start < line.content.end);
            if first == limit {
                continue;
            }
            let text = syntax.token_text(source, TokenId(first));
            if text == Some("import")
                && line.indent == 0
                && let Some(path) =
                    import_context(source, syntax.tokens[first].range.end, line.code.end)
            {
                imports.push(path);
            }
            if text != Some("case") {
                continue;
            }
            let (limit, fallback_end) = pattern_limit(source, syntax, line_number, first, limit);
            let end = (first + 1..limit)
                .find(|index| {
                    syntax.delimiter_context[*index] == syntax.delimiter_context[first]
                        && syntax.token_text(source, TokenId(*index)) == Some(":")
                })
                .map_or(fallback_end, |index| syntax.tokens[index].range.start);
            cases.push(TextRange::new(syntax.tokens[first].range.end, end));
        }
        Self {
            imports: imports.into_boxed_slice(),
            cases: cases.into_boxed_slice(),
        }
    }

    pub(super) fn in_case_pattern(&self, offset: usize) -> bool {
        let case = self.cases.partition_point(|range| range.start <= offset);
        case.checked_sub(1)
            .is_some_and(|index| offset <= self.cases[index].end)
    }

    pub(super) fn import_path(&self, offset: usize) -> Option<TextRange> {
        let index = self
            .imports
            .partition_point(|range| range.start <= offset)
            .checked_sub(1)?;
        let range = self.imports[index];
        (offset <= range.end).then_some(range)
    }
}

fn import_context(source: &str, start: usize, end: usize) -> Option<TextRange> {
    let rest = &source[start..end];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let start = start + rest.len() - rest.trim_start().len();
    let end = source[start..end]
        .find(char::is_whitespace)
        .map_or(end, |width| start + width);
    Some(TextRange::new(start, end))
}

fn pattern_limit(
    source: &str,
    syntax: &SyntaxIndex,
    line_number: usize,
    first: usize,
    limit: usize,
) -> (usize, usize) {
    let line = syntax.lines[line_number];
    let extends = (first + 1..limit).any(|index| {
        matches!(syntax.token_text(source, TokenId(index)), Some("(" | "{"))
            && syntax
                .delimiter_close(TokenId(index))
                .is_none_or(|close| syntax.tokens[close.0].range.end > line.content.end)
    });
    if !extends {
        return (limit, line.content.end);
    }
    let end = syntax.lines[line_number + 1..]
        .iter()
        .find(|next| {
            let code = source[next.code.start..next.code.end].trim_start();
            next.indent <= line.indent && !code.is_empty() && !code.starts_with(['#', ')', '}'])
        })
        .map_or(source.len(), |next| next.content.start);
    (
        syntax
            .tokens
            .partition_point(|token| token.range.start < end),
        end,
    )
}
