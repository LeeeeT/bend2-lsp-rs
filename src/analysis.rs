#[path = "analysis/syntax.rs"]
mod syntax;

pub use syntax::{
    CallSite, DiagnosticKind, IndexedConstructor, IndexedImport, IndexedSymbol, NameId, Reference,
    ReferenceKind, SymbolId, SyntaxDiagnostic, SyntaxIndex, Token, TokenFlags, TokenId, TokenKind,
};

pub type Import = IndexedImport;
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn contains(self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Revision(pub i32);

impl Revision {
    pub const UNVERSIONED: Self = Self(-1);
}

const UTF16_CHECKPOINT_STRIDE: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Utf16Checkpoint {
    byte_offset: usize,
    units: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineIndex {
    line_starts: Vec<usize>,
    ascii_lines: Vec<u64>,
    checkpoints: Vec<Utf16Checkpoint>,
}

impl LineIndex {
    #[must_use]
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        let mut ascii_lines = vec![u64::MAX];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
                if (line_starts.len() - 1) % u64::BITS as usize == 0 {
                    ascii_lines.push(u64::MAX);
                }
            } else if !byte.is_ascii() {
                let line = line_starts.len() - 1;
                ascii_lines[line / u64::BITS as usize] &= !(1_u64 << (line % u64::BITS as usize));
            }
        }
        Self::from_parts(source, line_starts, ascii_lines)
    }

    fn from_parts(source: &str, line_starts: Vec<usize>, ascii_lines: Vec<u64>) -> Self {
        let mut index = Self {
            line_starts,
            ascii_lines,
            checkpoints: Vec::new(),
        };
        for line in 0..index.line_starts.len() {
            if index.is_ascii_line(line) {
                continue;
            }
            let start = index.line_starts[line];
            let end = index.line_content_end(source, line);
            let mut units = 0;
            for (count, (offset, scalar)) in source[start..end].char_indices().enumerate() {
                units += scalar.len_utf16();
                if (count + 1) % UTF16_CHECKPOINT_STRIDE == 0 {
                    index.checkpoints.push(Utf16Checkpoint {
                        byte_offset: start + offset + scalar.len_utf8(),
                        units,
                    });
                }
            }
        }
        index
    }

    #[must_use]
    pub fn offset(&self, source: &str, line: u32, character: u32) -> usize {
        let Ok(line) = usize::try_from(line) else {
            return source.len();
        };
        let Some(start) = self.line_starts.get(line).copied() else {
            return source.len();
        };
        let end = self.line_content_end(source, line);
        let requested = usize::try_from(character).unwrap_or(usize::MAX);
        let content = &source[start..end];
        let mut units = 0usize;
        for (offset, scalar) in content.char_indices() {
            let scalar_units = scalar.len_utf16();
            if units.saturating_add(scalar_units) > requested {
                return start + offset;
            }
            units += scalar_units;
        }
        end
    }

    #[must_use]
    pub fn position(&self, source: &str, offset: usize) -> (u32, u32) {
        let mut offset = offset.min(source.len());
        while !source.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = self.line_starts[line];
        let end = self.line_content_end(source, line);
        let offset = offset.min(end);
        let character = if self.is_ascii_line(line) {
            offset - start
        } else {
            let before = self
                .checkpoints
                .partition_point(|checkpoint| checkpoint.byte_offset <= offset);
            let checkpoint = before
                .checked_sub(1)
                .and_then(|index| self.checkpoints.get(index))
                .filter(|checkpoint| checkpoint.byte_offset >= start);
            let (checkpoint_offset, units) = checkpoint.map_or((start, 0), |checkpoint| {
                (checkpoint.byte_offset, checkpoint.units)
            });
            units + source[checkpoint_offset..offset].encode_utf16().count()
        };
        (
            u32::try_from(line).unwrap_or(u32::MAX),
            u32::try_from(character).unwrap_or(u32::MAX),
        )
    }

    #[must_use]
    pub fn line_range(&self, source: &str, line: usize) -> Option<TextRange> {
        let start = *self.line_starts.get(line)?;
        Some(TextRange::new(start, self.line_content_end(source, line)))
    }

    fn is_ascii_line(&self, line: usize) -> bool {
        self.ascii_lines
            .get(line / u64::BITS as usize)
            .is_some_and(|word| word & (1_u64 << (line % u64::BITS as usize)) != 0)
    }

    fn line_content_end(&self, source: &str, line: usize) -> usize {
        let start = self.line_starts[line];
        let mut end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(source.len());
        if end > start && source.as_bytes()[end - 1] == b'\n' {
            end -= 1;
            if end > start && source.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
        }
        end
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct DocumentSnapshot {
    pub revision: Revision,
    pub text: String,
    pub line_index: LineIndex,
    pub syntax: SyntaxIndex,
}

impl DocumentSnapshot {
    #[must_use]
    pub fn new(revision: Revision, text: String) -> Self {
        let (line_index, syntax) = SyntaxIndex::build(&text);
        Self {
            revision,
            text,
            line_index,
            syntax,
        }
    }

    #[must_use]
    pub fn with_line_index(revision: Revision, text: String, line_index: LineIndex) -> Self {
        let syntax = SyntaxIndex::build_with_line_index(&text, &line_index);
        Self {
            revision,
            text,
            line_index,
            syntax,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    Function,
    Struct,
    Constructor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub detail: String,
    pub kind: SymbolKind,
    pub range: TextRange,
    pub selection_range: TextRange,
    pub children: Vec<Self>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    Function,
    Struct,
    Keyword,
    Variable,
    Constructor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    pub label: String,
    pub detail: String,
    pub kind: CompletionKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FoldingRange {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionRange {
    pub range: TextRange,
    pub parent: Option<Box<Self>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureHelp {
    pub label: String,
    pub parameters: Vec<String>,
    pub active_parameter: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSymbol<'a> {
    pub name: &'a str,
    pub kind: SymbolKind,
    pub range: TextRange,
}

#[must_use]
pub fn imports(snapshot: &DocumentSnapshot) -> &[Import] {
    snapshot.syntax.imports()
}

#[must_use]
pub fn folding_ranges(snapshot: &DocumentSnapshot) -> Vec<FoldingRange> {
    let source = &snapshot.text;
    let syntax = &snapshot.syntax;
    let mut ranges = vec![None; syntax.line_count()];
    let mut active = Vec::<(usize, usize)>::new();
    let mut last_code_line = None;
    let mut range_count = 0;

    for index in 0..syntax.line_count() {
        let code_range = syntax.line_code_range(index).unwrap_or_default();
        if source[code_range.start..code_range.end].trim().is_empty() {
            continue;
        }
        let indent = syntax.line_indent(index).unwrap_or_default();
        while active
            .last()
            .is_some_and(|(_, parent_indent)| indent <= *parent_indent)
        {
            let Some((start_line, _)) = active.pop() else {
                break;
            };
            if let Some(end_line) = last_code_line.filter(|end_line| *end_line > start_line) {
                ranges[start_line] = Some(FoldingRange {
                    start_line,
                    end_line,
                });
                range_count += 1;
            }
        }
        active.push((index, indent));
        last_code_line = Some(index);
    }

    while let Some((start_line, _)) = active.pop() {
        if let Some(end_line) = last_code_line.filter(|end_line| *end_line > start_line) {
            ranges[start_line] = Some(FoldingRange {
                start_line,
                end_line,
            });
            range_count += 1;
        }
    }
    let mut output = Vec::with_capacity(range_count);
    output.extend(ranges.into_iter().flatten());
    output
}

#[must_use]
pub fn selection_range(snapshot: &DocumentSnapshot, mut offset: usize) -> SelectionRange {
    let source = &snapshot.text;
    offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let line_number =
        usize::try_from(snapshot.line_index.position(source, offset).0).unwrap_or_default();
    let line = SelectionRange {
        range: snapshot
            .syntax
            .line_content_range(line_number)
            .unwrap_or_default(),
        parent: Some(Box::new(SelectionRange {
            range: TextRange::new(0, source.len()),
            parent: None,
        })),
    };
    let syntax = &snapshot.syntax;
    let Some(token_id) = syntax.token_at_or_before(offset) else {
        return line;
    };
    let Some(mut token) = syntax.token(token_id).copied() else {
        return line;
    };
    if token.range.start == offset
        && !matches!(token.kind, TokenKind::Identifier | TokenKind::Number)
        && token_id.0 > 0
    {
        let previous_id = TokenId(token_id.0 - 1);
        if let Some(previous) = syntax.token(previous_id)
            && previous.range.end == offset
            && matches!(previous.kind, TokenKind::Identifier | TokenKind::Number)
        {
            token = *previous;
        }
    }
    if !matches!(token.kind, TokenKind::Identifier | TokenKind::Number)
        || offset < token.range.start
        || offset > token.range.end
    {
        return line;
    }
    SelectionRange {
        range: token.range,
        parent: Some(Box::new(line)),
    }
}

/// Exact, prefix, and subsequence matching without allocating candidate strings.
/// Lower tuples rank first; ties are resolved by scope and then label by the caller.
#[must_use]
pub fn completion_match(label: &str, query: &str) -> Option<(u8, usize, usize)> {
    if label == query {
        return Some((0, 0, label.len()));
    }
    if label.starts_with(query) {
        return Some((1, 0, label.len()));
    }
    subsequence_completion_match(label, query)
}

fn completion_matches(label: &str, query: &str, ascii: bool) -> bool {
    if label.len() < query.len() {
        return false;
    }
    label.starts_with(query)
        || if ascii {
            ascii_subsequence_matches(label, query)
        } else {
            subsequence_completion_match(label, query).is_some()
        }
}

fn ascii_subsequence_matches(label: &str, query: &str) -> bool {
    let mut characters = label.bytes();
    for wanted in query.bytes() {
        let lower = wanted.to_ascii_lowercase();
        let upper = wanted.to_ascii_uppercase();
        if !characters.any(|character| character == lower || character == upper) {
            return false;
        }
    }
    true
}

fn subsequence_completion_match(label: &str, query: &str) -> Option<(u8, usize, usize)> {
    let mut wanted = query.chars();
    let mut next = wanted.next();
    let mut first = 0;
    let mut matched = 0;
    for (index, character) in label.chars().enumerate() {
        if next.is_some_and(|target| character.eq_ignore_ascii_case(&target)) {
            if matched == 0 {
                first = index;
            }
            matched += 1;
            next = wanted.next();
            if next.is_none() {
                return Some((
                    if first == 0 && index + 1 == matched {
                        2
                    } else {
                        3
                    },
                    first + index + 1 - matched,
                    label.len(),
                ));
            }
        }
    }
    None
}

/// Complete the constructors that a file declares.
#[must_use]
pub fn constructor_completion_items(snapshot: &DocumentSnapshot, prefix: &str) -> Vec<Completion> {
    let syntax = &snapshot.syntax;
    let source = &snapshot.text;
    let ascii_prefix = prefix.is_ascii();
    let mut items = Vec::new();
    for parent in syntax.symbols() {
        if parent.kind != SymbolKind::Struct {
            continue;
        }
        for constructor in syntax.constructors(parent.id) {
            let label = syntax.name_text(source, constructor.name);
            if completion_matches(label, prefix, ascii_prefix) {
                let parent_name = syntax.name_text(source, parent.name);
                let mut detail = String::with_capacity("constructor of ".len() + parent_name.len());
                detail.push_str("constructor of ");
                detail.push_str(parent_name);
                items.push(Completion {
                    label: label.to_owned(),
                    detail,
                    kind: CompletionKind::Constructor,
                });
            }
        }
    }
    items
}

#[must_use]
pub fn module_completion_items(snapshot: &DocumentSnapshot, prefix: &str) -> Vec<Completion> {
    collect_module_completions(snapshot, prefix, prefix.is_ascii())
}

fn collect_module_completions(
    snapshot: &DocumentSnapshot,
    prefix: &str,
    ascii_prefix: bool,
) -> Vec<Completion> {
    let mut items = Vec::new();
    let source = &snapshot.text;
    let syntax = &snapshot.syntax;
    for symbol in syntax.symbols() {
        let name = syntax.name_text(source, symbol.name);
        if completion_matches(name, prefix, ascii_prefix) {
            items.push(completion(
                name.to_owned(),
                source[symbol.detail_range.start..symbol.detail_range.end].to_owned(),
                symbol.kind,
            ));
        }
    }
    items
}

fn completion(label: String, detail: String, symbol_kind: SymbolKind) -> Completion {
    let kind = match symbol_kind {
        SymbolKind::Function | SymbolKind::Constructor => CompletionKind::Function,
        SymbolKind::Struct => CompletionKind::Struct,
    };
    Completion {
        label,
        detail,
        kind,
    }
}

/// Normalize a case-insensitive symbol query once for all indexed documents.
pub struct WorkspaceSymbolQuery {
    normalized: String,
}

impl WorkspaceSymbolQuery {
    #[must_use]
    pub fn new(query: &str) -> Self {
        Self {
            normalized: query.to_lowercase(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }

    pub fn symbols<'a>(
        &'a self,
        snapshot: &'a DocumentSnapshot,
    ) -> impl Iterator<Item = WorkspaceSymbol<'a>> + 'a {
        let source = &snapshot.text;
        let syntax = &snapshot.syntax;
        let is_empty = self.is_empty();
        syntax.symbols().iter().filter_map(move |symbol| {
            let name = syntax.name_text(source, symbol.name);
            if !is_empty && !name.to_lowercase().contains(&self.normalized) {
                return None;
            }
            Some(WorkspaceSymbol {
                name,
                kind: symbol.kind,
                range: symbol.name_range,
            })
        })
    }
}

#[must_use]
pub fn declaration_range(snapshot: &DocumentSnapshot, name: &str) -> Option<TextRange> {
    let syntax = &snapshot.syntax;
    let name = syntax.name_id(&snapshot.text, name)?;
    syntax
        .symbol_by_name(name)
        .map(|symbol| symbol.name_range)
        .or_else(|| {
            syntax
                .constructor_by_name(name)
                .map(|constructor| constructor.name_range)
        })
}

#[must_use]
pub fn document_symbols(snapshot: &DocumentSnapshot) -> Vec<Symbol> {
    let source = &snapshot.text;
    let syntax = &snapshot.syntax;
    syntax
        .symbols()
        .iter()
        .enumerate()
        .map(|(index, declaration)| {
            let end = syntax
                .symbols()
                .get(index + 1)
                .and_then(|next| syntax.line_content_range(next.start_line))
                .map_or(source.len(), |line| line.start);
            let start = syntax
                .line_content_range(declaration.start_line)
                .map_or(0, |line| line.start);
            let children = if declaration.kind == SymbolKind::Struct {
                constructor_symbols(snapshot, declaration)
            } else {
                Vec::new()
            };
            Symbol {
                name: syntax.name_text(source, declaration.name).to_owned(),
                detail: source[declaration.detail_range.start..declaration.detail_range.end]
                    .to_owned(),
                kind: declaration.kind,
                range: TextRange::new(start, end),
                selection_range: declaration.name_range,
                children,
            }
        })
        .collect()
}

fn constructor_symbols(snapshot: &DocumentSnapshot, parent: &IndexedSymbol) -> Vec<Symbol> {
    let source = &snapshot.text;
    let syntax = &snapshot.syntax;
    let parent_name = syntax.name_text(source, parent.name);
    syntax
        .constructors(parent.id)
        .iter()
        .map(|constructor| Symbol {
            name: syntax.name_text(source, constructor.name).to_owned(),
            detail: format!("constructor of {parent_name}"),
            kind: SymbolKind::Constructor,
            range: constructor.range,
            selection_range: constructor.name_range,
            children: Vec::new(),
        })
        .collect()
}
