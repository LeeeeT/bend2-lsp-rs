mod support;

use support::Must;

use std::sync::Arc;

use bend2_lsp::analysis::{
    DocumentSnapshot, FoldingRange, LineIndex, Revision, TextRange, folding_ranges,
};

#[test]
fn line_index_clamps_empty_eof_and_out_of_range_coordinates() {
    let empty = LineIndex::new("");
    assert_eq!(empty.offset("", 0, 0), 0);
    assert_eq!(empty.offset("", u32::MAX, u32::MAX), 0);
    assert_eq!(empty.position("", usize::MAX), (0, 0));
    assert_eq!(empty.line_range("", 0), Some(TextRange::new(0, 0)));
    assert_eq!(empty.line_range("", 1), None);

    let trailing_newline = "x\n";
    let index = LineIndex::new(trailing_newline);
    assert_eq!(index.position(trailing_newline, 2), (1, 0));
    assert_eq!(index.offset(trailing_newline, 1, 0), 2);
    assert_eq!(index.offset(trailing_newline, 2, 0), trailing_newline.len());
}

#[test]
fn line_index_handles_lf_and_crlf_line_ends() {
    let lf = "ab\ncd";
    let lf_index = LineIndex::new(lf);
    assert_eq!(lf_index.position(lf, 2), (0, 2));
    assert_eq!(lf_index.position(lf, 3), (1, 0));
    assert_eq!(lf_index.offset(lf, 0, u32::MAX), 2);
    assert_eq!(lf_index.line_range(lf, 0), Some(TextRange::new(0, 2)));
    assert_eq!(lf_index.line_range(lf, 1), Some(TextRange::new(3, 5)));

    let crlf = "ab\r\ncd\r\n";
    let crlf_index = LineIndex::new(crlf);
    assert_eq!(crlf_index.position(crlf, 2), (0, 2));
    assert_eq!(crlf_index.position(crlf, 3), (0, 2));
    assert_eq!(crlf_index.position(crlf, 4), (1, 0));
    assert_eq!(crlf_index.line_range(crlf, 0), Some(TextRange::new(0, 2)));
    assert_eq!(crlf_index.line_range(crlf, 1), Some(TextRange::new(4, 6)));
    assert_eq!(crlf_index.line_range(crlf, 2), Some(TextRange::new(8, 8)));
}

#[test]
fn line_index_converts_utf16_and_floors_non_boundary_byte_offsets() {
    let source = "Aé🦀Z";
    let index = LineIndex::new(source);
    assert_eq!(index.offset(source, 0, 0), 0);
    assert_eq!(index.offset(source, 0, 1), 1);
    assert_eq!(index.offset(source, 0, 2), 3);
    assert_eq!(index.offset(source, 0, 3), 3);
    assert_eq!(index.offset(source, 0, 4), 7);
    assert_eq!(index.offset(source, 0, 5), 8);
    assert_eq!(index.offset(source, 0, u32::MAX), 8);
    assert_eq!(index.position(source, 2), (0, 1));
    assert_eq!(index.position(source, 6), (0, 2));
    assert_eq!(index.position(source, 7), (0, 4));
    assert_eq!(index.position(source, 8), (0, 5));
    assert_eq!(index.position(source, usize::MAX), (0, 5));
}

#[test]
fn long_unicode_lines_use_sparse_utf16_position_checkpoints() {
    let source = format!("{}🦀{}é{}", "a".repeat(31), "b".repeat(32), "c".repeat(40));
    let line_index = LineIndex::new(&source);
    let snapshot = DocumentSnapshot::new(Revision(1), source.clone());
    for (offset, expected) in [
        (31, 31),
        (32, 31),
        (34, 31),
        (35, 33),
        (66, 64),
        (67, 65),
        (68, 65),
        (69, 66),
        (109, 106),
    ] {
        assert_eq!(line_index.position(&source, offset), (0, expected));
        assert_eq!(
            snapshot.line_index.position(&snapshot.text, offset),
            (0, expected)
        );
    }

    let string_and_comment = DocumentSnapshot::new(Revision(1), "\"🦀\" #é\n".to_owned());
    assert_eq!(
        string_and_comment
            .line_index
            .position(&string_and_comment.text, 10),
        (0, 7)
    );

    let lines: Vec<_> = (0..70)
        .map(|line| if line == 64 { "é" } else { "x" })
        .collect();
    let source = lines.join("\n");
    let offset = source.find('é').must_be("find Unicode line");
    let snapshot = DocumentSnapshot::new(Revision(1), source);
    assert_eq!(
        snapshot.line_index.position(&snapshot.text, offset + 2),
        (64, 1)
    );
}

#[test]
fn document_revision_keeps_previous_snapshot_immutable() {
    let previous = Arc::new(DocumentSnapshot::new(Revision(1), "old\n".to_owned()));
    let mut new_text = previous.text.clone();
    new_text.replace_range(0..3, "newer");
    let new_index = LineIndex::new(&new_text);
    let current = Arc::new(DocumentSnapshot::with_line_index(
        Revision(2),
        new_text,
        new_index,
    ));

    assert_eq!(previous.revision, Revision(1));
    assert_eq!(previous.text, "old\n");
    assert_eq!(previous.line_index.position(&previous.text, 4), (1, 0));
    assert_eq!(current.revision, Revision(2));
    assert_eq!(current.text, "newer\n");
    assert_eq!(current.line_index.position(&current.text, 6), (1, 0));
}

#[test]
fn folding_ranges_track_nested_indented_blocks_in_source_order() {
    let source = "outer\n  child\n    grandchild\n  sibling\n\nnext\n  leaf\n";
    let snapshot = DocumentSnapshot::new(Revision(1), source.to_owned());

    assert_eq!(
        folding_ranges(&snapshot),
        vec![
            FoldingRange {
                start_line: 0,
                end_line: 3,
            },
            FoldingRange {
                start_line: 1,
                end_line: 2,
            },
            FoldingRange {
                start_line: 5,
                end_line: 6,
            },
        ]
    );
}

#[test]
fn folding_preserves_blank_and_comment_line_boundaries() {
    let source = "outer\n  middle\n    inner\n      body\n\n    # comment\nnext\n  leaf\n";
    let snapshot = DocumentSnapshot::new(Revision(1), source.to_owned());

    assert_eq!(
        folding_ranges(&snapshot),
        vec![
            FoldingRange {
                start_line: 0,
                end_line: 5,
            },
            FoldingRange {
                start_line: 1,
                end_line: 5,
            },
            FoldingRange {
                start_line: 2,
                end_line: 3,
            },
            FoldingRange {
                start_line: 6,
                end_line: 7,
            },
        ]
    );
}

#[test]
fn folding_handles_empty_documents_and_unclosed_syntax_at_eof() {
    let empty = DocumentSnapshot::new(Revision(1), String::new());
    assert_eq!(folding_ranges(&empty), Vec::<FoldingRange>::new());

    let source = "broken(\n  nested\n    child";
    let malformed = DocumentSnapshot::new(Revision(2), source.to_owned());
    assert_eq!(
        folding_ranges(&malformed),
        vec![
            FoldingRange {
                start_line: 0,
                end_line: 2,
            },
            FoldingRange {
                start_line: 1,
                end_line: 2,
            },
        ]
    );
}
#[test]
fn syntax_index_lexes_once_and_reuses_names_imports_calls_and_context() {
    use bend2_lsp::analysis::TokenKind;

    let source = r#"import lib.bend as Lib
def target(a: Type, b: Type) -> U32:
  "target(y) \" ( [ #"
  # target(x, [)
  target(1, nested(2, 3),
"#;
    let snapshot = DocumentSnapshot::new(Revision(1), source.to_owned());
    let imports = bend2_lsp::analysis::imports(&snapshot);
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].path_text(source), "lib.bend");
    assert_eq!(imports[0].alias_text(source), Some("Lib"));

    let identifiers = snapshot
        .syntax
        .tokens()
        .iter()
        .filter(|token| {
            token.kind == TokenKind::Identifier
                && &source[token.range.start..token.range.end] == "target"
        })
        .count();
    assert_eq!(identifiers, 2, "comments and strings are not identifiers");
    let recursive_call = snapshot
        .syntax
        .calls()
        .iter()
        .find(|call| snapshot.syntax.name_text(source, call.name) == "target")
        .must_be("indexed recursive call");
    assert_eq!(
        snapshot
            .syntax
            .call_arguments(recursive_call)
            .iter()
            .map(|range| &source[range.start..range.end])
            .collect::<Vec<_>>(),
        ["1", "nested(2, 3)"]
    );
    assert_eq!(
        snapshot.syntax.call_at(source.len()).map(|(_, n)| n),
        Some(2)
    );
    let string = snapshot
        .syntax
        .tokens()
        .iter()
        .find(|token| token.kind == TokenKind::StringLiteral)
        .must_be("quoted source must have one string token");
    assert!(string.flags.escaped());
    assert!(
        snapshot
            .syntax
            .is_in_comment_or_string(source.find("target(y)").must_be("string text") + 2)
    );
    assert!(
        snapshot
            .syntax
            .is_in_comment_or_string(source.find("# target").must_be("comment text") + 3)
    );
}

#[test]
fn syntax_index_keeps_malformed_literal_hole_and_delimiter_diagnostics() {
    use bend2_lsp::analysis::DiagnosticKind;

    let snapshot = DocumentSnapshot::new(
        Revision(1),
        "def main() -> Type:\n  ?TODO\n  value[)\n  \"unfinished\n".to_owned(),
    );
    let diagnostics = snapshot.syntax.diagnostics();
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::UnresolvedHole)
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::UnterminatedString('"'))
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.kind == DiagnosticKind::UnmatchedDelimiter(')') })
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::UnclosedDelimiter('['))
    );
}

#[test]
fn constructor_definitions_preserve_source_order_when_names_were_seen_earlier() {
    for prefix in ["", "def prior(value: Second) -> Nat:\n  0n\n\n"] {
        let source = format!(
            "{prefix}type FirstType is Data:\n  First{{}}\n  Second{{}}\n\
             type OtherType is Data:\n  First{{}}\n"
        );
        let snapshot = DocumentSnapshot::new(Revision(1), source.clone());
        for name in ["First", "Second"] {
            let start = source
                .find(&format!("  {name}"))
                .must_be("constructor declaration")
                + 2;
            assert_eq!(
                bend2_lsp::analysis::declaration_range(&snapshot, name),
                Some(TextRange::new(start, start + name.len()))
            );
        }
    }
}

#[test]
fn dense_index_lookups_distinguish_zero_ids_missing_entries_and_nested_calls() {
    use bend2_lsp::analysis::TokenId;

    let snapshot = DocumentSnapshot::new(
        Revision(1),
        "def first(value: U32) -> U32:\n  value\n\
         def main() -> U32:\n  first(first(1))\n  unknown\n"
            .to_owned(),
    );
    let syntax = &snapshot.syntax;
    let first = snapshot.text.find("first").must_be("first declaration");
    let declaration = syntax
        .token_at_or_before(first)
        .must_be("declaration token");
    assert_eq!(
        syntax
            .reference_for_token(declaration)
            .must_be("declaration reference")
            .kind,
        bend2_lsp::analysis::ReferenceKind::Declaration
    );
    assert_eq!(
        syntax.name_text(
            &snapshot.text,
            syntax.name_for_token(TokenId(0)).must_be("first name")
        ),
        "def"
    );
    assert!(syntax.reference_for_token(TokenId(0)).is_some());
    let punctuation = syntax
        .token_at_or_before(first + "first".len())
        .must_be("opening delimiter");
    assert_eq!(syntax.name_for_token(punctuation), None);
    assert_eq!(syntax.reference_for_token(punctuation), None);
    assert_eq!(syntax.call_for_token(TokenId(0)), None);
    assert_eq!(syntax.call_by_open(TokenId(0)), None);
    for token in [TokenId(syntax.tokens().len()), TokenId(usize::MAX)] {
        assert_eq!(syntax.name_for_token(token), None);
        assert!(syntax.reference_for_token(token).is_none());
        assert!(syntax.call_for_token(token).is_none());
        assert!(syntax.call_by_open(token).is_none());
    }

    let outer = snapshot.text.find("first(first").must_be("outer call");
    let inner = outer + "first(".len();
    let outer_token = syntax.token_at_or_before(outer).must_be("outer callee");
    let inner_token = syntax.token_at_or_before(inner).must_be("inner callee");
    let outer_call = syntax
        .call_for_token(outer_token)
        .must_be("first call index");
    let inner_call = syntax.call_for_token(inner_token).must_be("nested call");
    assert_eq!(outer_call.callee_token, outer_token);
    assert_eq!(inner_call.callee_token, inner_token);
    assert_eq!(syntax.call_by_open(outer_call.open), Some(outer_call));
    assert_eq!(syntax.call_by_open(inner_call.open), Some(inner_call));
    assert_eq!(
        syntax
            .call_at(inner + "first(".len())
            .map(|(call, argument)| (call.callee_token, argument)),
        Some((inner_token, 0))
    );

    let unknown = snapshot.text.find("unknown").must_be("unresolved name");
    let unknown_token = syntax
        .token_at_or_before(unknown)
        .must_be("unresolved token");
    assert_eq!(
        syntax
            .reference_for_token(unknown_token)
            .must_be("unknown reference")
            .kind,
        bend2_lsp::analysis::ReferenceKind::Read
    );
    assert_eq!(syntax.call_for_token(unknown_token), None);
}

#[test]
fn module_completion_preserves_subsequence_order_case_and_repeated_characters() {
    let snapshot = DocumentSnapshot::new(
        Revision(1),
        concat!(
            "def aba() -> U32:\n  0\n",
            "def aab() -> U32:\n  0\n",
            "def AAA() -> U32:\n  0\n",
            "def zaa() -> U32:\n  0\n",
        )
        .into(),
    );
    let cases: &[(&str, &[&str])] = &[
        ("aa", &["aba", "aab", "AAA", "zaa"]),
        ("aBa", &["aba"]),
        ("aaaa", &[]),
        ("λA", &[]),
        ("", &["aba", "aab", "AAA", "zaa"]),
    ];
    for &(query, expected) in cases {
        let items = bend2_lsp::analysis::module_completion_items(&snapshot, query);
        assert!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .eq(expected.iter().copied()),
            "query {query}: {items:?}"
        );
    }
}

#[test]
fn module_completion_preserves_ascii_case_underscores_repeated_letters_and_unicode() {
    let source = concat!(
        "def Foo_Bar() -> U32:\n  0\n",
        "def foo__foo() -> U32:\n  0\n",
        "def Ff() -> U32:\n  0\n",
        "def foo() -> U32:\n  0\n",
    );
    let snapshot = DocumentSnapshot::new(Revision(1), source.to_owned());
    let cases: &[(&str, &[&str])] = &[
        ("F_F", &["foo__foo"]),
        ("FF", &["foo__foo", "Ff"]),
        ("πF", &[]),
        ("FOO", &["Foo_Bar", "foo__foo", "foo"]),
        ("_F", &["foo__foo"]),
        ("fff", &[]),
    ];
    for &(query, expected) in cases {
        let mut labels = bend2_lsp::analysis::module_completion_items(&snapshot, query)
            .into_iter()
            .map(|completion| completion.label)
            .collect::<Vec<_>>();
        labels.sort_unstable();
        let mut expected = expected.to_vec();
        expected.sort_unstable();
        assert_eq!(labels, expected, "query {query:?}");
    }
    assert_eq!(
        bend2_lsp::analysis::completion_match("πfoo", "πF"),
        Some((2, 0, 5))
    );
}
