use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

use super::{LineIndex, SymbolKind, TextRange};

#[path = "syntax/completion.rs"]
mod completion;
#[path = "syntax/declarations.rs"]
mod declarations;
#[path = "syntax/scanner.rs"]
mod scanner;

use declarations::{
    build_lines, declaration_tokens, parse_constructors, parse_imports, parse_symbols,
};
use scanner::Scanner;

const KEYWORDS: &[&str] = &[
    "def", "law", "type", "is", "match", "case", "do", "return", "for", "exs", "where", "import",
    "as",
];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TokenId(pub usize);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NameId(pub usize);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SymbolId(pub usize);

// usize::MAX cannot index any of the non-ZST token/symbol/reference/call buffers.
// Keep empty dense slots in one word without narrowing the public ID domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IndexSlot(usize);

impl IndexSlot {
    const EMPTY: Self = Self(usize::MAX);

    const fn some(index: usize) -> Self {
        Self(index)
    }

    const fn from_option(index: Option<usize>) -> Self {
        match index {
            Some(index) => Self(index),
            None => Self::EMPTY,
        }
    }

    const fn value(self) -> Option<usize> {
        if self.0 == usize::MAX {
            None
        } else {
            Some(self.0)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind {
    Identifier,
    Number,
    StringLiteral,
    Comment,
    Operator,
    Delimiter,
    Punctuation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenFlags(u8);

impl TokenFlags {
    const ESCAPED: u8 = 1;
    const UNTERMINATED: u8 = 2;

    #[must_use]
    pub const fn escaped(self) -> bool {
        self.0 & Self::ESCAPED != 0
    }

    #[must_use]
    pub const fn unterminated(self) -> bool {
        self.0 & Self::UNTERMINATED != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Token {
    pub range: TextRange,
    pub kind: TokenKind,
    pub flags: TokenFlags,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {
    UnresolvedHole,
    UnterminatedString(char),
    UnmatchedDelimiter(char),
    UnclosedDelimiter(char),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxDiagnostic {
    pub range: TextRange,
    pub kind: DiagnosticKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedImport {
    pub path: TextRange,
    pub alias: Option<TextRange>,
}

impl IndexedImport {
    #[must_use]
    pub fn path_text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.path.start..self.path.end]
    }

    #[must_use]
    pub fn alias_text<'a>(&self, source: &'a str) -> Option<&'a str> {
        let range = self.alias?;
        Some(&source[range.start..range.end])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedSymbol {
    pub id: SymbolId,
    pub name: NameId,
    pub name_range: TextRange,
    pub detail_range: TextRange,
    pub scope: TextRange,
    pub kind: SymbolKind,
    pub start_line: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedConstructor {
    pub parent: SymbolId,
    pub name: NameId,
    pub name_range: TextRange,
    pub range: TextRange,
}

#[derive(Debug, Eq, PartialEq)]
struct ConstructorIndex {
    entries: Box<[IndexedConstructor]>,
    // Empty when entries are already ordered by NameId; otherwise a sorted permutation.
    name_indices: Box<[usize]>,
}

// Keep rare metadata out of the inline snapshot while retaining cheap empty-diagnostic checks.
#[derive(Debug, Eq, PartialEq)]
struct AuxiliaryIndex {
    data: Option<Box<AuxiliaryData>>,
    diagnostic_count: usize,
}

impl AuxiliaryIndex {
    fn new(
        diagnostics: Vec<SyntaxDiagnostic>,
        constructor_index: Option<ConstructorIndex>,
    ) -> Self {
        let diagnostic_count = diagnostics.len();
        Self {
            diagnostic_count,
            data: (diagnostic_count > 0 || constructor_index.is_some()).then(|| {
                Box::new(AuxiliaryData {
                    diagnostics: diagnostics.into_boxed_slice(),
                    constructors: constructor_index,
                })
            }),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct AuxiliaryData {
    diagnostics: Box<[SyntaxDiagnostic]>,
    constructors: Option<ConstructorIndex>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceKind {
    Declaration,
    Read,
    Call,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reference {
    pub name: NameId,
    pub qualifier: Option<NameId>,
    pub qualifier_token: Option<TokenId>,
    pub token: TokenId,
    pub range: TextRange,
    pub kind: ReferenceKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallSite {
    pub name: NameId,
    pub qualifier: Option<NameId>,
    pub qualifier_token: Option<TokenId>,
    pub callee_token: TokenId,
    pub callee_range: TextRange,
    pub call_range: TextRange,
    pub argument_range: TextRange,
    argument_indices: TextRange,
    separator_indices: TextRange,
    pub open: TokenId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DelimiterPair {
    open: TokenId,
    close: TokenId,
}

struct CallInputs<'a> {
    source: &'a str,
    tokens: &'a [Token],
    delimiter_pairs: &'a [DelimiterPair],
    delimiter_context: &'a [IndexSlot],
    symbols: &'a [IndexedSymbol],
    names: &'a mut NameTable,
}

struct CallBuild<'a> {
    source: &'a str,
    tokens: &'a [Token],
    delimiter_pairs: &'a [DelimiterPair],
    delimiter_context: &'a [IndexSlot],
    declarations: &'a [bool],
    names: &'a mut NameTable,
}

struct CallIndex {
    calls: Vec<CallSite>,
    arguments: Vec<TextRange>,
    separators: Vec<usize>,
    by_open: Vec<IndexSlot>,
    by_name_token: Vec<IndexSlot>,
}

struct ReferenceInputs<'a> {
    source: &'a str,
    tokens: &'a [Token],
    identifier_count: usize,
    names: &'a mut NameTable,
    symbols: &'a [IndexedSymbol],
    calls: &'a [CallSite],
}

struct ReferenceIndex {
    references: Vec<Reference>,
    token_references: Vec<IndexSlot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SyntaxLine {
    content: TextRange,
    full_end: usize,
    code: TextRange,
    indent: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct NameTable {
    ranges: Vec<TextRange>,
    lookup: HashMap<u64, Vec<NameId>>,
    token_names: Vec<IndexSlot>,
}
impl NameTable {
    fn new() -> Self {
        Self {
            ranges: Vec::new(),
            lookup: HashMap::new(),
            token_names: Vec::new(),
        }
    }

    fn intern(&mut self, source: &str, range: TextRange, _token: TokenId) -> NameId {
        let text = &source[range.start..range.end];
        let hash = hash_name(text);
        if let Some(candidates) = self.lookup.get(&hash) {
            for &candidate in candidates {
                let existing = self.ranges[candidate.0];
                if source[existing.start..existing.end] == *text {
                    self.token_names.push(IndexSlot::some(candidate.0));
                    return candidate;
                }
            }
        }
        let id = NameId(self.ranges.len());
        self.ranges.push(range);
        self.lookup.entry(hash).or_default().push(id);
        self.token_names.push(IndexSlot::some(id.0));
        id
    }

    fn intern_name(&mut self, source: &str, range: TextRange) -> NameId {
        if let Some(id) = self.find(source, &source[range.start..range.end]) {
            return id;
        }
        let id = NameId(self.ranges.len());
        let name = &source[range.start..range.end];
        self.ranges.push(range);
        self.lookup.entry(hash_name(name)).or_default().push(id);
        id
    }

    fn append_non_name(&mut self) {
        self.token_names.push(IndexSlot::EMPTY);
    }

    fn find(&self, source: &str, text: &str) -> Option<NameId> {
        let candidates = self.lookup.get(&hash_name(text))?;
        candidates.iter().copied().find(|candidate| {
            let range = self.ranges[candidate.0];
            source[range.start..range.end] == *text
        })
    }

    fn range(&self, id: NameId) -> TextRange {
        self.ranges[id.0]
    }

    fn for_token(&self, id: TokenId) -> Option<NameId> {
        self.token_names
            .get(id.0)
            .copied()
            .and_then(IndexSlot::value)
            .map(NameId)
    }
}

fn keyword_name_flags(source: &str, names: &NameTable) -> Vec<bool> {
    let mut flags = vec![false; names.ranges.len()];
    for keyword in KEYWORDS {
        if let Some(name) = names.find(source, keyword) {
            flags[name.0] = true;
        }
    }
    flags
}

#[derive(Debug, Eq, PartialEq)]
pub struct SyntaxIndex {
    tokens: Box<[Token]>,
    names: NameTable,
    keyword_names: Box<[bool]>,
    imports: Box<[IndexedImport]>,
    symbols: Box<[IndexedSymbol]>,
    references: Box<[Reference]>,
    token_references: Box<[IndexSlot]>,
    calls: Box<[CallSite]>,
    call_arguments: Box<[TextRange]>,
    call_separators: Box<[usize]>,
    call_by_open: Box<[IndexSlot]>,
    call_by_name_token: Box<[IndexSlot]>,
    lines: Box<[SyntaxLine]>,
    auxiliary: AuxiliaryIndex,
    delimiter_pairs: Box<[DelimiterPair]>,
    delimiter_context: Box<[IndexSlot]>,
    symbol_by_name: Box<[IndexSlot]>,
    completion: completion::CompletionIndex,
}

impl SyntaxIndex {
    pub(crate) fn build(source: &str) -> (LineIndex, Self) {
        let mut scan = Scanner::new(source, true).scan();
        let line_index = LineIndex::from_parts(
            source,
            std::mem::take(&mut scan.line_starts),
            std::mem::take(&mut scan.line_ascii),
        );
        let index = Self::from_scan(source, &line_index, scan);
        (line_index, index)
    }

    pub(crate) fn build_with_line_index(source: &str, line_index: &LineIndex) -> Self {
        let scan = Scanner::new(source, false).scan();
        Self::from_scan(source, line_index, scan)
    }

    fn from_scan(source: &str, line_index: &LineIndex, scan: ScanOutput) -> Self {
        let lines = build_lines(source, line_index);
        let ScanOutput {
            tokens,
            identifier_count,
            mut names,
            diagnostics,
            delimiter_pairs,
            delimiter_context,
            ..
        } = scan;
        let keyword_names = keyword_name_flags(source, &names);
        let mut symbols = parse_symbols(source, &lines, &mut names);
        for (index, symbol) in symbols.iter_mut().enumerate() {
            symbol.id = SymbolId(index);
        }
        let symbol_by_name = Self::index_symbol_names(&symbols);
        let constructor_index = if symbols
            .iter()
            .any(|symbol| symbol.kind == SymbolKind::Struct)
        {
            parse_constructors(source, &lines, &symbols, &mut names)
        } else {
            None
        };
        let auxiliary = AuxiliaryIndex::new(diagnostics, constructor_index);
        let calls = build_calls(CallInputs {
            source,
            tokens: &tokens,
            delimiter_pairs: &delimiter_pairs,
            delimiter_context: &delimiter_context,
            symbols: &symbols,
            names: &mut names,
        });
        let references = build_references(ReferenceInputs {
            source,
            tokens: &tokens,
            identifier_count,
            names: &mut names,
            symbols: &symbols,
            calls: &calls.calls,
        });
        let mut index = Self {
            tokens: tokens.into_boxed_slice(),
            names,
            imports: parse_imports(source, &lines).into_boxed_slice(),
            keyword_names: keyword_names.into_boxed_slice(),
            symbols: symbols.into_boxed_slice(),
            references: references.references.into_boxed_slice(),
            token_references: references.token_references.into_boxed_slice(),
            calls: calls.calls.into_boxed_slice(),
            call_arguments: calls.arguments.into_boxed_slice(),
            call_separators: calls.separators.into_boxed_slice(),
            call_by_open: calls.by_open.into_boxed_slice(),
            call_by_name_token: calls.by_name_token.into_boxed_slice(),
            lines: lines.into_boxed_slice(),
            auxiliary,
            delimiter_pairs: delimiter_pairs.into_boxed_slice(),
            delimiter_context: delimiter_context.into_boxed_slice(),
            symbol_by_name,
            completion: completion::CompletionIndex::default(),
        };
        index.completion = completion::CompletionIndex::build(source, &index);
        index
    }

    fn index_symbol_names(symbols: &[IndexedSymbol]) -> Box<[IndexSlot]> {
        let name_count = symbols
            .iter()
            .map(|symbol| symbol.name.0)
            .max()
            .map_or(0, |name| name + 1);
        let mut names = vec![IndexSlot::EMPTY; name_count];
        for symbol in symbols {
            let slot = &mut names[symbol.name.0];
            if slot.value().is_none() {
                *slot = IndexSlot::some(symbol.id.0);
            }
        }
        names.into_boxed_slice()
    }

    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    #[must_use]
    pub fn imports(&self) -> &[IndexedImport] {
        &self.imports
    }

    #[must_use]
    pub fn symbols(&self) -> &[IndexedSymbol] {
        &self.symbols
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[SyntaxDiagnostic] {
        if self.auxiliary.diagnostic_count == 0 {
            return &[];
        }
        self.auxiliary
            .data
            .as_deref()
            .map_or(&[], |data| data.diagnostics.as_ref())
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    #[must_use]
    pub fn line_indent(&self, line: usize) -> Option<usize> {
        self.lines.get(line).map(|line| line.indent)
    }

    #[must_use]
    pub fn line_code_range(&self, line: usize) -> Option<TextRange> {
        self.lines.get(line).map(|line| line.code)
    }

    #[must_use]
    pub fn line_content_range(&self, line: usize) -> Option<TextRange> {
        self.lines.get(line).map(|line| line.content)
    }

    #[must_use]
    pub fn line_full_end(&self, line: usize) -> Option<usize> {
        self.lines.get(line).map(|line| line.full_end)
    }

    #[must_use]
    pub fn name_id(&self, source: &str, name: &str) -> Option<NameId> {
        self.names.find(source, name)
    }

    #[must_use]
    pub fn name_for_token(&self, token: TokenId) -> Option<NameId> {
        self.names.for_token(token)
    }

    #[must_use]
    pub fn is_keyword(&self, name: NameId) -> bool {
        self.keyword_names.get(name.0).copied().unwrap_or(false)
    }

    #[must_use]
    pub fn is_keyword_text(name: &str) -> bool {
        KEYWORDS.contains(&name)
    }

    #[must_use]
    pub fn name_range(&self, name: NameId) -> TextRange {
        self.names.range(name)
    }

    #[must_use]
    pub fn name_text<'a>(&self, source: &'a str, name: NameId) -> &'a str {
        let range = self.name_range(name);
        &source[range.start..range.end]
    }

    #[must_use]
    pub(crate) fn symbol_id_by_name(&self, name: NameId) -> Option<SymbolId> {
        self.symbol_by_name
            .get(name.0)
            .copied()
            .and_then(IndexSlot::value)
            .map(SymbolId)
    }

    #[must_use]
    pub fn symbol_by_name(&self, name: NameId) -> Option<&IndexedSymbol> {
        self.symbol_id_by_name(name).map(|id| &self.symbols[id.0])
    }

    #[must_use]
    pub fn constructor_by_name(&self, name: NameId) -> Option<&IndexedConstructor> {
        let index = self.auxiliary.data.as_deref()?.constructors.as_ref()?;
        let constructor = if index.name_indices.is_empty() {
            let position = index.entries.partition_point(|entry| entry.name.0 < name.0);
            index.entries.get(position)?
        } else {
            let position = index
                .name_indices
                .partition_point(|entry| index.entries[*entry].name.0 < name.0);
            &index.entries[*index.name_indices.get(position)?]
        };
        (constructor.name == name).then_some(constructor)
    }

    #[must_use]
    pub fn constructors(&self, parent: SymbolId) -> &[IndexedConstructor] {
        let Some(index) = self
            .auxiliary
            .data
            .as_deref()
            .and_then(|data| data.constructors.as_ref())
        else {
            return &[];
        };
        let start = index
            .entries
            .partition_point(|constructor| constructor.parent.0 < parent.0);
        let end = index
            .entries
            .partition_point(|constructor| constructor.parent.0 <= parent.0);
        &index.entries[start..end]
    }

    #[must_use]
    pub fn symbol_by_id(&self, id: SymbolId) -> Option<&IndexedSymbol> {
        self.symbols.get(id.0)
    }

    #[must_use]
    pub fn reference_for_token(&self, token: TokenId) -> Option<&Reference> {
        let index = self.token_references.get(token.0)?.value()?;
        self.references.get(index)
    }

    #[must_use]
    pub fn calls(&self) -> &[CallSite] {
        &self.calls
    }

    #[must_use]
    pub fn call_arguments(&self, call: &CallSite) -> &[TextRange] {
        &self.call_arguments[call.argument_indices.start..call.argument_indices.end]
    }

    #[must_use]
    pub fn call_by_open(&self, open: TokenId) -> Option<&CallSite> {
        let index = self.call_by_open.get(open.0)?.value()?;
        self.calls.get(index)
    }

    #[must_use]
    pub fn call_for_token(&self, token: TokenId) -> Option<&CallSite> {
        let index = self.call_by_name_token.get(token.0)?.value()?;
        self.calls.get(index)
    }

    #[must_use]
    pub fn call_at(&self, offset: usize) -> Option<(&CallSite, u32)> {
        let previous = self
            .tokens
            .partition_point(|token| token.range.start < offset)
            .checked_sub(1)?;
        let open = self.delimiter_context[previous].value()?;
        let index = self.call_by_open.get(open)?.value()?;
        let call = &self.calls[index];
        let separators =
            &self.call_separators[call.separator_indices.start..call.separator_indices.end];
        let active = separators.partition_point(|separator| *separator < offset);
        Some((call, u32::try_from(active).unwrap_or(u32::MAX)))
    }

    #[must_use]
    pub fn token(&self, id: TokenId) -> Option<&Token> {
        self.tokens.get(id.0)
    }

    #[must_use]
    pub fn token_text<'a>(&self, source: &'a str, id: TokenId) -> Option<&'a str> {
        let range = self.token(id)?.range;
        source.get(range.start..range.end)
    }

    #[must_use]
    pub fn token_at_or_before(&self, offset: usize) -> Option<TokenId> {
        let index = self
            .tokens
            .partition_point(|token| token.range.start <= offset)
            .checked_sub(1)?;
        Some(TokenId(index))
    }

    #[must_use]
    pub fn is_in_comment_or_string(&self, offset: usize) -> bool {
        let Some(id) = self.token_at_or_before(offset) else {
            return false;
        };
        let token = self.tokens[id.0];
        match token.kind {
            TokenKind::Comment => token.range.start <= offset && offset <= token.range.end,
            TokenKind::StringLiteral => token.range.start <= offset && offset < token.range.end,
            _ => false,
        }
    }

    #[must_use]
    pub fn active_call<'a>(
        &self,
        source: &'a str,
        offset: usize,
    ) -> Option<(&'a str, u32, TokenId, TokenId)> {
        let (call, parameter) = self.call_at(offset)?;
        Some((
            self.name_text(source, call.name),
            parameter,
            call.open,
            call.callee_token,
        ))
    }

    #[must_use]
    pub fn call_argument_ranges(&self, open: TokenId) -> &[TextRange] {
        self.call_by_open(open)
            .map_or(&[], |call| self.call_arguments(call))
    }

    #[must_use]
    pub fn delimiter_close(&self, open: TokenId) -> Option<TokenId> {
        self.delimiter_pairs
            .binary_search_by_key(&open, |pair| pair.open)
            .ok()
            .map(|index| self.delimiter_pairs[index].close)
    }

    /// Whether `offset` is in the patterns of a `case`.
    #[must_use]
    pub fn in_case_pattern(&self, offset: usize) -> bool {
        self.completion.in_case_pattern(offset)
    }

    #[must_use]
    pub fn completion_import_path(&self, offset: usize) -> Option<TextRange> {
        self.completion.import_path(offset)
    }
}

fn hash_name(name: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    hasher.finish()
}

struct ScanOutput {
    tokens: Vec<Token>,
    identifier_count: usize,
    names: NameTable,
    diagnostics: Vec<SyntaxDiagnostic>,
    delimiter_pairs: Vec<DelimiterPair>,
    delimiter_context: Vec<IndexSlot>,
    line_starts: Vec<usize>,
    line_ascii: Vec<u64>,
}

fn build_calls(input: CallInputs<'_>) -> CallIndex {
    let CallInputs {
        source,
        tokens,
        delimiter_pairs,
        delimiter_context,
        symbols,
        names,
    } = input;
    let declarations = declaration_tokens(tokens, symbols);
    let mut context = CallBuild {
        source,
        tokens,
        delimiter_pairs,
        delimiter_context,
        declarations: &declarations,
        names,
    };
    let mut calls = Vec::new();
    let mut arguments = Vec::new();
    let mut separators = Vec::new();
    let mut by_open = vec![IndexSlot::EMPTY; tokens.len()];
    let mut by_name_token = vec![IndexSlot::EMPTY; tokens.len()];
    for (open_index, (open_token, by_open_slot)) in
        context.tokens.iter().copied().zip(&mut by_open).enumerate()
    {
        let Some(call) = parse_call(
            &mut context,
            open_index,
            open_token,
            &mut arguments,
            &mut separators,
        ) else {
            continue;
        };
        let call_index = calls.len();
        *by_open_slot = IndexSlot::some(call_index);
        by_name_token[call.callee_token.0] = IndexSlot::some(call_index);
        if let Some(qualifier_token) = call.qualifier_token {
            by_name_token[qualifier_token.0] = IndexSlot::some(call_index);
        }
        calls.push(call);
    }
    CallIndex {
        calls,
        arguments,
        separators,
        by_open,
        by_name_token,
    }
}

fn parse_call(
    context: &mut CallBuild<'_>,
    open_index: usize,
    open_token: Token,
    arguments: &mut Vec<TextRange>,
    separators: &mut Vec<usize>,
) -> Option<CallSite> {
    if open_token.kind != TokenKind::Delimiter
        || &context.source[open_token.range.start..open_token.range.end] != "("
    {
        return None;
    }
    let callee_index = open_index.checked_sub(1)?;
    let callee_token = context.tokens[callee_index];
    if callee_token.kind != TokenKind::Identifier
        || context.declarations[callee_index]
        || !context.source[callee_token.range.end..open_token.range.start]
            .bytes()
            .all(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    let name = context.names.for_token(TokenId(callee_index))?;
    let start_index = qualified_chain_start(context.source, context.tokens, callee_index);
    let qualifier = (start_index < callee_index).then(|| {
        let end = context.tokens[callee_index - 2].range.end;
        context.names.intern_name(
            context.source,
            TextRange::new(context.tokens[start_index].range.start, end),
        )
    });
    let qualifier_token = (start_index < callee_index).then_some(TokenId(start_index));
    let close = context
        .delimiter_pairs
        .binary_search_by_key(&TokenId(open_index), |pair| pair.open)
        .ok()
        .map(|index| context.delimiter_pairs[index].close);
    let argument_end = close.map_or(context.source.len(), |close| {
        context.tokens[close.0].range.start
    });
    let call_end = close.map_or(context.source.len(), |close| {
        context.tokens[close.0].range.end
    });
    let limit = close.map_or(context.tokens.len(), |close| close.0);
    let argument_start_index = arguments.len();
    let separator_start_index = separators.len();
    let mut segment_start = open_token.range.end;
    for index in open_index + 1..limit {
        let token = context.tokens[index];
        if token.kind == TokenKind::Punctuation
            && context.delimiter_context[index].value() == Some(open_index)
            && &context.source[token.range.start..token.range.end] == ","
        {
            if let Some(range) = trimmed_range(context.source, segment_start, token.range.start) {
                arguments.push(range);
            }
            separators.push(token.range.start);
            segment_start = token.range.end;
        }
    }
    if let Some(range) = trimmed_range(context.source, segment_start, argument_end) {
        arguments.push(range);
    }
    Some(CallSite {
        name,
        qualifier,
        qualifier_token,
        callee_token: TokenId(callee_index),
        callee_range: callee_token.range,
        call_range: TextRange::new(context.tokens[start_index].range.start, call_end),
        argument_range: TextRange::new(open_token.range.end, argument_end),
        argument_indices: TextRange::new(argument_start_index, arguments.len()),
        separator_indices: TextRange::new(separator_start_index, separators.len()),
        open: TokenId(open_index),
    })
}

fn build_references(input: ReferenceInputs<'_>) -> ReferenceIndex {
    let ReferenceInputs {
        source,
        tokens,
        identifier_count,
        names,
        symbols,
        calls,
    } = input;
    let mut references = Vec::with_capacity(identifier_count);
    let mut token_references = vec![IndexSlot::EMPTY; tokens.len()];
    let mut call_tokens = vec![false; tokens.len()];
    for call in calls {
        call_tokens[call.callee_token.0] = true;
    }
    append_declaration_references(tokens, symbols, &mut references, &mut token_references);
    for (token_index, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Identifier || token_references[token_index].value().is_some() {
            continue;
        }
        let token_id = TokenId(token_index);
        let Some(name) = names.for_token(token_id) else {
            continue;
        };
        let start_index = qualified_chain_start(source, tokens, token_index);
        let (qualifier, qualifier_token) = if start_index < token_index {
            let end = tokens[token_index - 2].range.end;
            (
                Some(
                    names.intern_name(source, TextRange::new(tokens[start_index].range.start, end)),
                ),
                Some(TokenId(start_index)),
            )
        } else {
            (None, None)
        };
        let index = references.len();
        references.push(Reference {
            name,
            qualifier,
            qualifier_token,
            token: token_id,
            range: token.range,
            kind: if call_tokens[token_index] {
                ReferenceKind::Call
            } else {
                ReferenceKind::Read
            },
        });
        token_references[token_index] = IndexSlot::some(index);
    }
    ReferenceIndex {
        references,
        token_references,
    }
}

fn append_declaration_references(
    tokens: &[Token],
    symbols: &[IndexedSymbol],
    references: &mut Vec<Reference>,
    token_references: &mut [IndexSlot],
) {
    for symbol in symbols {
        let start = tokens.partition_point(|token| token.range.start < symbol.name_range.start);
        let end = tokens.partition_point(|token| token.range.start < symbol.name_range.end);
        let Some(token_index) = tokens[start..end]
            .iter()
            .rposition(|token| token.kind == TokenKind::Identifier)
            .map(|offset| start + offset)
        else {
            continue;
        };
        let index = references.len();
        references.push(Reference {
            name: symbol.name,
            qualifier: None,
            qualifier_token: None,
            token: TokenId(token_index),
            range: symbol.name_range,
            kind: ReferenceKind::Declaration,
        });
        for (offset, token) in tokens[start..end].iter().enumerate() {
            if token.kind == TokenKind::Identifier {
                token_references[start + offset] = IndexSlot::some(index);
            }
        }
    }
}

fn qualified_chain_start(source: &str, tokens: &[Token], mut index: usize) -> usize {
    while index >= 2
        && tokens[index - 1].kind == TokenKind::Punctuation
        && &source[tokens[index - 1].range.start..tokens[index - 1].range.end] == "."
        && tokens[index - 1].range.end == tokens[index].range.start
        && tokens[index - 2].kind == TokenKind::Identifier
        && tokens[index - 2].range.end == tokens[index - 1].range.start
    {
        index -= 2;
    }
    index
}

fn trimmed_range(source: &str, start: usize, end: usize) -> Option<TextRange> {
    let text = source.get(start..end)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let prefix = text.len() - text.trim_start().len();
    Some(TextRange::new(
        start + prefix,
        start + prefix + trimmed.len(),
    ))
}
