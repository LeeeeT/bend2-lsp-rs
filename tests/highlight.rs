mod support;

use bend2_lsp::{
    analysis::{DocumentSnapshot, Revision},
    frontend::{Env, Files, load},
    highlight::{Class, highlights},
    semantic::Semantics,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use support::Must;

struct Memory(HashMap<PathBuf, String>);

impl Files for Memory {
    fn read(&self, path: &Path) -> Option<String> {
        self.0.get(path).cloned()
    }

    fn real(&self, path: &Path) -> Option<PathBuf> {
        self.0.contains_key(path).then(|| path.to_path_buf())
    }
}

const BASE: &str = "/memory/Base.bend";
const MAIN: &str = "/memory/project/main.bend";
const SHAPES: &str = "/memory/project/shapes.bend";

fn semantics(main: &str) -> Semantics {
    let memory = Memory(
        [
            (BASE, "type Nat is Data:\n  Zero{}\n  Succ{pred: Nat}\n"),
            (
                SHAPES,
                "import Base\ntype Shape is Data:\n  Circle{radius: Nat}\n",
            ),
            (MAIN, main),
        ]
        .into_iter()
        .map(|(path, text)| (PathBuf::from(path), text.to_owned()))
        .collect(),
    );
    let env = Env {
        lib: "/memory/lib".to_owned(),
        base: BASE.to_owned(),
        hub: "http://127.0.0.1:9".to_owned(),
    };
    Semantics::new(load(&memory, &env, MAIN))
}

fn classes(source: &str, semantics: Option<&Semantics>) -> Vec<(String, Class, bool)> {
    let snapshot = DocumentSnapshot::new(Revision(0), source.to_owned());
    highlights(&snapshot.syntax, semantics)
        .into_iter()
        .map(|found| {
            let text = source
                .get(found.range.start..found.range.end)
                .must_be("a highlight inside the source");
            (text.to_owned(), found.class, found.declaration)
        })
        .collect()
}

const SOURCE: &str = "# Shapes\nimport Base\nimport ./shapes.bend as S\n\ndef unit(n: Nat) -> S.Shape:\n  S.Circle{n}\n\ndef two() -> Nat:\n  Succ{Succ{Zero{}}}\n";

#[test]
fn a_loaded_document_classes_each_name_by_its_meaning() {
    let semantics = semantics(SOURCE);
    let found = classes(SOURCE, Some(&semantics));
    for expected in [
        ("# Shapes", Class::Comment, false),
        ("import", Class::Keyword, false),
        ("def", Class::Keyword, false),
        ("unit", Class::Function, true),
        ("n", Class::Variable, true),
        ("Nat", Class::Type, false),
        ("S", Class::Namespace, false),
        ("Circle", Class::Constructor, false),
        ("Succ", Class::Constructor, false),
    ] {
        let expected = (expected.0.to_owned(), expected.1, expected.2);
        assert!(
            found.contains(&expected),
            "missing highlight {expected:?}: {found:?}"
        );
    }
    let uses = found
        .iter()
        .filter(|(text, class, declaration)| {
            text == "n" && *class == Class::Variable && !declaration
        })
        .count();
    assert_eq!(uses, 1, "the use of n is not a declaration: {found:?}");
}

#[test]
fn a_document_without_semantics_has_only_lexical_highlights() {
    let found = classes(SOURCE, None);
    assert!(found.contains(&("def".to_owned(), Class::Keyword, false)));
    assert!(found.contains(&("# Shapes".to_owned(), Class::Comment, false)));
    assert!(found.iter().all(|(_, class, _)| !matches!(
        class,
        Class::Variable | Class::Function | Class::Type | Class::Constructor
    )));
    let namespaces: Vec<_> = found
        .iter()
        .filter(|(_, class, _)| *class == Class::Namespace)
        .collect();
    assert_eq!(
        namespaces,
        [&("S".to_owned(), Class::Namespace, true)],
        "only the alias of the import has a class: {found:?}"
    );
}

#[test]
fn a_universe_is_a_type_with_or_without_semantics() {
    let source = "import Base\n\ntype Box<a, -A: Kind(a)> is Data:\n  Full{value: A}\n\ndef pick(-T: Type, x: T) -> T:\n  x\n";
    let semantics = semantics(source);
    for found in [classes(source, Some(&semantics)), classes(source, None)] {
        for universe in ["Kind", "Data", "Type"] {
            let expected = (universe.to_owned(), Class::Type, false);
            assert!(
                found.contains(&expected),
                "missing highlight {expected:?}: {found:?}"
            );
        }
    }
}

#[test]
fn a_def_or_a_law_that_gives_types_is_a_type() {
    let source = "import Base\n\nlaw Handle:\n  Type\n\nlaw Family:\n  for -A: Type\n  Data\n\ndef Twice(-A: Type) -> Nat -> Type:\n  n => A\n\ndef open(h: Handle, f: Family(Nat)) -> Twice(Nat, Zero{}):\n  h\n";
    let semantics = semantics(source);
    let found = classes(source, Some(&semantics));
    for expected in [
        ("Handle", Class::Type, true),
        ("Family", Class::Type, true),
        ("Twice", Class::Type, true),
        ("Handle", Class::Type, false),
        ("Family", Class::Type, false),
        ("Twice", Class::Type, false),
        ("open", Class::Function, true),
    ] {
        let expected = (expected.0.to_owned(), expected.1, expected.2);
        assert!(
            found.contains(&expected),
            "missing highlight {expected:?}: {found:?}"
        );
    }
}

#[test]
fn an_import_has_keywords_a_string_path_and_a_declared_alias() {
    let source = "import Base\nimport ./shapes.bend as S # shapes\n\ndef unit(n: Nat) -> S.Shape:\n  S.Circle{n}\n";
    let semantics = semantics(source);
    for found in [classes(source, Some(&semantics)), classes(source, None)] {
        let lines: Vec<_> = found.iter().take(6).cloned().collect();
        let expected = [
            ("import", Class::Keyword, false),
            ("Base", Class::String, false),
            ("import", Class::Keyword, false),
            ("./shapes.bend", Class::String, false),
            ("as", Class::Keyword, false),
            ("S", Class::Namespace, true),
        ]
        .map(|(text, class, declaration)| (text.to_owned(), class, declaration));
        assert_eq!(lines, expected, "{found:?}");
        assert_eq!(found[6], ("# shapes".to_owned(), Class::Comment, false));
    }
}
