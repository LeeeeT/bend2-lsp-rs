mod support;

use bend2_lsp::{
    frontend::{Env, Files, load},
    semantic::{Identity, Semantics, Target},
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

fn semantics(files: &[(&str, &str)]) -> Semantics {
    let memory = Memory(
        files
            .iter()
            .chain([&(BASE, "def builtin() -> U32:\n  1\n")])
            .map(|(path, text)| (PathBuf::from(path), (*text).to_owned()))
            .collect(),
    );
    let env = Env {
        lib: "/memory/lib".to_owned(),
        base: BASE.to_owned(),
        hub: "http://127.0.0.1:9".to_owned(),
    };
    Semantics::new(load(&memory, &env, MAIN))
}

fn target_at(semantics: &Semantics, source: &str, needle: &str) -> Target {
    let offset = source.find(needle).must_be("needle in source");
    let name = *semantics
        .names_at(offset)
        .first()
        .must_be("a name at the needle");
    semantics.target(name)
}

#[test]
fn an_entry_that_stops_in_an_import_after_base_is_never_read_as_base() {
    let source = "import Base\nimport ./dep.bend as Dep\nimport ./missing.bend as Missing\ndef main() -> U32:\n  builtin()\n";
    let semantics = semantics(&[
        (MAIN, source),
        ("/memory/project/dep.bend", "def value() -> U32:\n  2\n"),
    ]);
    assert!(
        semantics.entry().is_none(),
        "the compiler reads no name of the entry"
    );
    assert!(
        semantics
            .names_at(source.find("builtin").must_be("call"))
            .is_empty()
    );
    assert_eq!(
        semantics.entry_aliases,
        [("Dep".to_owned(), "dep".to_owned())],
        "the import that fails names no module"
    );
    let error = semantics.error.as_ref().must_be("a load error");
    let (path, range) = error.at.as_ref().must_be("an error location");
    assert_eq!(path, Path::new(MAIN));
    assert_eq!(
        &source[range.start..],
        &source[source.find("./missing.bend").must_be("missing import")..]
    );
}

#[test]
fn a_local_never_hides_a_qualified_name() {
    let source = "import ./dep.bend as Dep\ndef main(Dep: U32) -> U32:\n  Dep.value\n";
    let semantics = semantics(&[
        (MAIN, source),
        ("/memory/project/dep.bend", "def value() -> U32:\n  2\n"),
    ]);
    assert_eq!(
        target_at(&semantics, source, "Dep.value"),
        Target::Global(Identity {
            path: PathBuf::from("/memory/project/dep.bend"),
            name: "value".to_owned(),
            ctr: false,
        })
    );
}

#[test]
fn a_parameter_hides_a_declaration_of_its_name() {
    let source = "def value() -> U32:\n  1\ndef main(value: U32) -> U32:\n  value\n";
    let semantics = semantics(&[(MAIN, source)]);
    assert!(matches!(
        target_at(&semantics, source, "value()"),
        Target::Global(_)
    ));
    let use_site = source.rfind("value").must_be("use of the parameter");
    assert!(matches!(
        target_at(&semantics, source, &source[use_site..]),
        Target::Local { .. }
    ));
}

#[test]
fn a_type_and_its_constructor_of_one_name_stay_apart() {
    let source = "type Word is Data:\n  Word{}\ndef make() -> Word:\n  Word{}\n";
    let semantics = semantics(&[(MAIN, source)]);
    let identity = |ctr| {
        Target::Global(Identity {
            path: PathBuf::from(MAIN),
            name: "Word".to_owned(),
            ctr,
        })
    };
    assert_eq!(target_at(&semantics, source, "Word is"), identity(false));
    assert_eq!(target_at(&semantics, source, "Word{}\ndef"), identity(true));
    assert_eq!(
        target_at(&semantics, source, "Word:\n  Word{}\n"),
        identity(false)
    );
    let built = source.rfind("Word{}").must_be("constructor use");
    assert_eq!(
        target_at(&semantics, source, &source[built..]),
        identity(true)
    );
}
