mod support;

use support::Must;

use std::{collections::HashMap, sync::Arc};

use bend2_lsp::{
    analysis::{DocumentSnapshot, Revision, TextRange},
    resolve::{self, Declaration, Module, ModuleSource, Target},
};
use url::Url;

const MAIN: &str = "file:///main.bend";
const LINT: &str = "file:///lint.bend";
const BASE: &str = "file:///Base.bend";

struct Modules {
    documents: HashMap<Url, Arc<DocumentSnapshot>>,
    base: Option<Module>,
}

impl ModuleSource for Modules {
    fn import_target(&self, source: &Url, path: TextRange) -> Option<Module> {
        let snapshot = self.documents.get(source)?;
        let file = snapshot.text[path.start..path.end].strip_prefix("./")?;
        let uri = Url::parse(&format!("file:///{file}")).ok()?;
        let snapshot = self.documents.get(&uri)?.clone();
        Some(Module { uri, snapshot })
    }

    fn base(&self) -> Option<Module> {
        self.base.clone()
    }
}

fn snapshot(text: &str) -> Arc<DocumentSnapshot> {
    Arc::new(DocumentSnapshot::new(Revision(1), text.to_owned()))
}

fn uri(text: &str) -> Url {
    Url::parse(text).must_be("valid test URI")
}

fn modules(main: &str, lint: &str, base: &str) -> Modules {
    let base = Module {
        uri: uri(BASE),
        snapshot: snapshot(base),
    };
    Modules {
        documents: HashMap::from([
            (uri(MAIN), snapshot(main)),
            (uri(LINT), snapshot(lint)),
            (base.uri.clone(), base.snapshot.clone()),
        ]),
        base: Some(base),
    }
}

fn offset(text: &str, needle: &str, occurrence: usize) -> usize {
    text.match_indices(needle)
        .nth(occurrence)
        .must_be("needle occurs in the test source")
        .0
}

fn reference_target(modules: &Modules, needle: &str, occurrence: usize) -> Option<Target> {
    let main = uri(MAIN);
    let source = modules.documents[&main].clone();
    let at = offset(&source.text, needle, occurrence);
    let token = source
        .syntax
        .token_at_or_before(at)
        .must_be("token at the needle");
    let reference = source
        .syntax
        .reference_for_token(token)
        .must_be("reference at the needle");
    resolve::reference_target(modules, &main, &source, reference)
}

fn call_target(modules: &Modules, needle: &str, occurrence: usize) -> Option<Target> {
    let main = uri(MAIN);
    let source = modules.documents[&main].clone();
    let at = offset(&source.text, needle, occurrence);
    let token = source
        .syntax
        .token_at_or_before(at)
        .must_be("token at the needle");
    let call = source
        .syntax
        .call_for_token(token)
        .must_be("call at the needle");
    resolve::call_target(modules, &main, &source, call)
}

fn name(target: &Target) -> String {
    let syntax = &target.module.syntax;
    let symbol = syntax
        .symbol_by_id(target.symbol())
        .must_be("target symbol exists");
    format!(
        "{}:{}",
        target.module.uri.path(),
        syntax.name_text(&target.module.text, symbol.name)
    )
}

const LINT_SOURCE: &str = "\
type Here is Data:
  Here{}

def Here.nodes(here: Here) -> U32:
  0
";

const BASE_SOURCE: &str = "\
type Maybe is Data:
  None{}
  Some{value: U32}

def List.map(xs: U32) -> U32:
  xs
";

#[test]
fn local_call_resolves_to_its_declaration() {
    let main = "\
def leaf(x: U32) -> U32:
  x

def root(x: U32) -> U32:
  leaf(x)
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    let target = call_target(&modules, "leaf(x)", 0).must_be("local call resolves");
    assert_eq!(name(&target), "/main.bend:leaf");
}

#[test]
fn aliased_chain_resolves_the_member_after_the_alias() {
    let main = "\
import ./lint.bend as Lint

def root(here: Lint.Here) -> U32:
  Lint.Here.nodes(here)
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    let target = call_target(&modules, "nodes(here)", 0).must_be("aliased chain call resolves");
    assert_eq!(name(&target), "/lint.bend:Here.nodes");
    let annotation = reference_target(&modules, "Here)", 0).must_be("aliased type resolves");
    assert_eq!(name(&annotation), "/lint.bend:Here");
}

#[test]
fn inner_segment_of_a_chain_does_not_resolve() {
    let main = "\
import ./lint.bend as Lint

def root(here: Lint.Here) -> U32:
  Lint.Here.nodes(here)
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    assert!(reference_target(&modules, "Here.nodes", 0).is_none());
    assert!(reference_target(&modules, "Lint.Here.nodes", 0).is_none());
}

#[test]
fn dotted_base_name_resolves_through_the_prelude() {
    let main = "\
import Base

def root(xs: U32) -> U32:
  List.map(xs)

def qualified(xs: U32) -> U32:
  Base.List.map(xs)
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    let target = call_target(&modules, "map(xs)", 0).must_be("dotted Base call resolves");
    assert_eq!(name(&target), "/Base.bend:List.map");
    let qualified = call_target(&modules, "map(xs)", 1).must_be("Base-qualified call resolves");
    assert_eq!(name(&qualified), "/Base.bend:List.map");
}

#[test]
fn base_name_needs_the_base_import() {
    let main = "\
def root(xs: U32) -> U32:
  List.map(xs)
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    assert!(call_target(&modules, "map(xs)", 0).is_none());
}

#[test]
fn constructor_resolves_to_its_type() {
    let main = "\
import Base

type Pair is Data:
  Both{left: U32, right: U32}

def root(x: U32) -> Pair:
  Both{x, x}

def some(x: U32) -> Maybe:
  Some{x}
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    let local = reference_target(&modules, "Both{x", 0).must_be("local constructor resolves");
    assert!(matches!(local.declaration, Declaration::Constructor(_)));
    assert_eq!(name(&local), "/main.bend:Pair");
    let prelude = reference_target(&modules, "Some{x", 0).must_be("Base constructor resolves");
    assert!(matches!(prelude.declaration, Declaration::Constructor(_)));
    assert_eq!(name(&prelude), "/Base.bend:Maybe");
}

#[test]
fn parameter_shadows_an_import_alias() {
    let main = "\
import ./lint.bend as Lint

def root(Lint: U32) -> U32:
  Lint.Here.nodes(Lint)
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    assert!(call_target(&modules, "nodes(Lint)", 0).is_none());
}

#[test]
fn call_target_rejects_a_type() {
    let main = "\
import ./lint.bend as Lint

def root(x: U32) -> U32:
  Lint.Here(x)
";
    let modules = modules(main, LINT_SOURCE, BASE_SOURCE);
    assert!(call_target(&modules, "Here(x)", 0).is_none());
    let reference = reference_target(&modules, "Here(x)", 0).must_be("type reference resolves");
    assert_eq!(name(&reference), "/lint.bend:Here");
}
