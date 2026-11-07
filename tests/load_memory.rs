use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use bend2_lsp::frontend::{Env, Files, load, load_partial, load_traced};

struct Memory(HashMap<PathBuf, String>);

impl Files for Memory {
    fn read(&self, path: &Path) -> Option<String> {
        self.0.get(path).cloned()
    }

    fn real(&self, path: &Path) -> Option<PathBuf> {
        self.0.contains_key(path).then(|| path.to_path_buf())
    }
}

fn environment() -> Env {
    Env {
        lib: "/memory/lib".to_owned(),
        base: "/memory/base.bend".to_owned(),
        hub: "http://127.0.0.1:9".to_owned(),
    }
}

fn memory(source: &str) -> Memory {
    Memory(HashMap::from([
        (PathBuf::from("/memory/main.bend"), source.to_owned()),
        (
            PathBuf::from("/memory/dep.bend"),
            "def value() -> U32:\n  7\n".to_owned(),
        ),
    ]))
}

#[test]
fn normal_loads_keep_names_and_types_without_body_traces() {
    let files = memory("import ./dep.bend as Dep\ndef main(x: U32) -> U32:\n  Dep.value()\n");
    let env = environment();
    let compact = load(&files, &env, "/memory/main.bend");
    let mut traced = load_traced(&files, &env, "/memory/main.bend");
    assert!(compact.error.is_none());
    assert_eq!(compact.error, traced.error);
    assert_eq!(compact.files.len(), 2);
    assert_eq!(compact.tlds.len(), traced.tlds.len());
    assert!(compact.tlds.iter().all(|tld| tld.bodies.is_empty()));
    assert!(traced.tlds.iter().all(|tld| !tld.bodies.is_empty()));
    for (normal, full) in compact.tlds.iter().zip(&mut traced.tlds) {
        full.bodies.clear();
        assert_eq!(normal.json(), full.json());
    }
    for (normal, full) in compact.files.iter().zip(&traced.files) {
        assert_eq!(normal.real, full.real);
        assert_eq!(normal.source, full.source);
        assert_eq!(
            format!("{:?}", normal.occurrences),
            format!("{:?}", full.occurrences)
        );
    }
}

#[test]
fn compact_loads_report_the_same_parser_errors() {
    let files = memory("import ./dep.bend as Dep\ndef main(x: U32) -> U32:\n  unknown(\n");
    let env = environment();
    let compact = load(&files, &env, "/memory/main.bend");
    let traced = load_traced(&files, &env, "/memory/main.bend");
    assert!(compact.error.is_some());
    assert_eq!(compact.error, traced.error);
    assert_eq!(compact.tlds.len(), traced.tlds.len());
}

#[test]
fn partial_loads_do_not_keep_body_traces() {
    let files = memory("import ./dep.bend as Dep\ndef main(x: U32) -> U32:\n  unknown(\n");
    let loaded = load_partial(&files, &environment(), "/memory/main.bend");
    assert!(loaded.error.is_some());
    assert!(loaded.scope.is_some());
    assert!(loaded.tlds.iter().all(|tld| tld.bodies.is_empty()));
}
