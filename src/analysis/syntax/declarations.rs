use super::{
    ConstructorIndex, IndexedConstructor, IndexedImport, IndexedSymbol, LineIndex, NameTable,
    SymbolId, SymbolKind, SyntaxLine, TextRange, Token, TokenKind,
};

pub(super) fn build_lines(source: &str, index: &LineIndex) -> Vec<SyntaxLine> {
    (0..index.line_starts.len())
        .map(|line| {
            let start = index.line_starts[line];
            let full_end = index
                .line_starts
                .get(line + 1)
                .copied()
                .unwrap_or(source.len());
            let content = index.line_range(source, line).unwrap_or_default();
            let indent = source[content.start..content.end]
                .bytes()
                .take_while(|byte| matches!(byte, b' ' | b'\t'))
                .count();
            SyntaxLine {
                content: TextRange::new(start, content.end),
                full_end,
                code: TextRange::new(start + indent, content.end),
                indent,
            }
        })
        .collect()
}

pub(super) fn parse_imports(source: &str, lines: &[SyntaxLine]) -> Vec<IndexedImport> {
    let mut imports = Vec::new();
    for line in lines {
        let code = &source[line.code.start..line.code.end];
        if code.trim().is_empty() || code.trim_start().starts_with('#') {
            continue;
        }
        if line.indent != 0 {
            break;
        }
        let Some(rest) = code.strip_prefix("import") else {
            break;
        };
        if !rest.starts_with(char::is_whitespace) {
            break;
        }
        let path_start = code.len() - rest.trim_start().len();
        let path_end = code[path_start..]
            .find(char::is_whitespace)
            .map_or(code.len(), |width| path_start + width);
        if path_start == path_end || code[path_start..].starts_with('#') {
            break;
        }
        let suffix = code[path_end..].trim_start();
        let alias = suffix
            .strip_prefix("as")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(str::trim_start)
            .and_then(|rest| {
                let end = rest
                    .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .unwrap_or(rest.len());
                (end > 0
                    && (rest.as_bytes()[0].is_ascii_alphabetic() || rest.as_bytes()[0] == b'_'))
                    .then_some((code.len() - rest.len(), end))
            });
        imports.push(IndexedImport {
            path: TextRange::new(line.code.start + path_start, line.code.start + path_end),
            alias: alias.map(|(start, end)| {
                TextRange::new(line.code.start + start, line.code.start + start + end)
            }),
        });
    }
    imports
}

pub(super) fn parse_symbols(
    source: &str,
    lines: &[SyntaxLine],
    names: &mut NameTable,
) -> Vec<IndexedSymbol> {
    let mut symbols: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.indent == 0)
        .filter_map(|(line_number, line)| parse_top_level_symbol(source, *line, line_number, names))
        .collect();
    let mut symbol_index = symbols.len();
    let mut next_scope_end = source.len();
    for (line_number, line) in lines.iter().enumerate().rev() {
        let code = source[line.code.start..line.code.end].trim();
        if line.indent != 0 || code.is_empty() || code.starts_with('#') {
            continue;
        }
        if symbol_index > 0 && symbols[symbol_index - 1].start_line == line_number {
            symbols[symbol_index - 1].scope = TextRange::new(line.full_end, next_scope_end);
            symbol_index -= 1;
        }
        next_scope_end = line.content.start;
    }
    symbols
}

// Keep type-body parsing out of the already large snapshot builder.
#[cold]
#[inline(never)]
pub(super) fn parse_constructors(
    source: &str,
    lines: &[SyntaxLine],
    symbols: &[IndexedSymbol],
    names: &mut NameTable,
) -> Option<ConstructorIndex> {
    let mut constructors = Vec::new();
    for (index, parent) in symbols.iter().enumerate() {
        if parent.kind != SymbolKind::Struct {
            continue;
        }
        let end = symbols
            .get(index + 1)
            .map_or(lines.len(), |next| next.start_line);
        let body = &lines[parent.start_line + 1..end];
        let mut body_indent = usize::MAX;
        for line in body {
            if line.indent == 0 || line.indent >= body_indent {
                continue;
            }
            let code = source[line.code.start..line.code.end].trim_start();
            if !code.is_empty() && !code.starts_with('#') {
                body_indent = line.indent;
            }
        }
        if body_indent == usize::MAX {
            continue;
        }
        for line in body {
            if line.indent != body_indent {
                continue;
            }
            let code = &source[line.code.start..line.code.end];
            let Some(first) = code.as_bytes().first() else {
                continue;
            };
            if !first.is_ascii_alphabetic() && *first != b'_' {
                continue;
            }
            let name_len = code
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
                .count();
            let remainder = code[name_len..].trim_start();
            if !remainder.is_empty() && !remainder.starts_with('{') {
                continue;
            }
            let name_range = TextRange::new(line.code.start, line.code.start + name_len);
            constructors.push(IndexedConstructor {
                parent: parent.id,
                name: names.intern_name(source, name_range),
                name_range,
                range: TextRange::new(line.content.start, line.full_end),
            });
        }
    }
    if constructors.is_empty() {
        return None;
    }
    let name_indices = if constructors
        .windows(2)
        .all(|pair| pair[0].name.0 <= pair[1].name.0)
    {
        Vec::new()
    } else {
        let mut indices: Vec<_> = (0..constructors.len()).collect();
        indices.sort_unstable_by_key(|index| (constructors[*index].name.0, *index));
        indices
    };
    Some(ConstructorIndex {
        entries: constructors.into_boxed_slice(),
        name_indices: name_indices.into_boxed_slice(),
    })
}

pub(super) fn declaration_tokens(tokens: &[Token], symbols: &[IndexedSymbol]) -> Vec<bool> {
    let mut declarations = vec![false; tokens.len()];
    for symbol in symbols {
        let start = tokens.partition_point(|token| token.range.start < symbol.name_range.start);
        let end = tokens.partition_point(|token| token.range.start < symbol.name_range.end);
        for (offset, token) in tokens[start..end].iter().enumerate() {
            if token.kind == TokenKind::Identifier {
                declarations[start + offset] = true;
            }
        }
    }
    declarations
}

fn parse_top_level_symbol(
    source: &str,
    line: SyntaxLine,
    line_number: usize,
    names: &mut NameTable,
) -> Option<IndexedSymbol> {
    let code = &source[line.code.start..line.code.end];
    let mut parsed = code;
    if let Some(rest) = parsed.strip_prefix("@unsafe") {
        parsed = rest.trim_start();
    }
    let keyword_end = parsed.find(char::is_whitespace)?;
    let keyword = &parsed[..keyword_end];
    let kind = match keyword {
        "def" | "law" => SymbolKind::Function,
        "type" => SymbolKind::Struct,
        _ => return None,
    };
    let remainder = parsed[keyword_end..].trim_start();
    let name_len = remainder
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
        .count();
    if name_len == 0
        || !remainder.as_bytes()[0].is_ascii_alphabetic() && remainder.as_bytes()[0] != b'_'
    {
        return None;
    }
    let parse_offset = code.len() - parsed.len();
    let after_keyword = &parsed[keyword_end..];
    let name_offset =
        parse_offset + keyword_end + after_keyword.len() - after_keyword.trim_start().len();
    let name_range = TextRange::new(
        line.code.start + name_offset,
        line.code.start + name_offset + name_len,
    );
    let detail = code.trim();
    let detail_start = line.code.start + code.len() - code.trim_start().len();
    let detail = detail.strip_suffix(':').unwrap_or(detail);
    let detail_range = TextRange::new(detail_start, detail_start + detail.len());
    let name = names.intern_name(source, TextRange::new(name_range.start, name_range.end));
    Some(IndexedSymbol {
        id: SymbolId(usize::MAX),
        name,
        name_range,
        detail_range,
        scope: TextRange::new(line.full_end, line.full_end),
        kind,
        start_line: line_number,
    })
}
