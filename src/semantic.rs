//! What each name in a document means, from the port of the compiler's front
//! end. One `Semantics` is one load: its entry is the document, loaded as the
//! compiler loads the file it checks.

use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

use crate::{
    analysis::TextRange,
    frontend::{
        self, Binder, Book, ErrorText, Load, LoadedFile, Meaning, Span, Term, TldTrace, TraceKind,
    },
};

/// A declaration's identity: the file that declares it, its name there, and
/// whether it is a constructor, which can share its name with its type. It
/// does not depend on the entry, as the keys of a book do.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Identity {
    pub path: PathBuf,
    pub name: String,
    pub ctr: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    Def,
    Law,
    Type,
    Constructor,
}

/// A declaration of a loaded file.
#[derive(Clone, Debug)]
pub struct Declaration {
    pub identity: Identity,
    pub key: String,
    pub kind: DeclarationKind,
    /// The range of its name.
    pub range: TextRange,
    /// The declaration that declares it: itself, or the type of a constructor.
    pub tld: usize,
    /// For a def or a law, its parameters: each name and the source text of
    /// its type, when it has one.
    pub parameters: Vec<(String, Option<String>)>,
    /// The source line that declares it, trimmed, without its final colon.
    pub detail: String,
    /// For a def or a law, how many of its parameters are `~` templates.
    pub templates: usize,
    /// A def or a law whose result is a universe, as `def IO(-A: Type) ->
    /// Type` or `law File: Type`: its values are types.
    pub type_former: bool,
}

/// The term is a universe, or a function whose final result is a universe.
fn universe_valued(term: &Term) -> bool {
    match term {
        Term::All { b, .. } => universe_valued(b),
        Term::Typ { .. } | Term::Qnt { .. } => true,
        _ => false,
    }
}

/// The def or law gives types. A def that fills a law has no type of its own,
/// so it takes the type of the law.
fn type_former(load: &Load, tld: &TldTrace) -> bool {
    let law = || {
        load.tlds
            .iter()
            .find(|other| other.kind == TraceKind::Law && other.name == tld.name)
    };
    tld.ret
        .as_ref()
        .or(tld.t.as_ref())
        .or_else(|| law().and_then(|law| law.t.as_ref()))
        .is_some_and(universe_valued)
}

/// One name of a file and what it means.
#[derive(Clone, Debug)]
pub struct Name {
    pub range: TextRange,
    pub meaning: Meaning,
    /// The name has no token of its own at this range, as `T.add` behind the
    /// `+` of `(a + b : T)`.
    pub implicit: bool,
    /// The declaration that holds it.
    pub tld: usize,
}

/// What a name refers to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// A local binder of the entry, declared at this range.
    Local {
        binder: Binder,
        range: Option<TextRange>,
    },
    /// A declaration.
    Global(Identity),
    /// A key that no loaded file declares: the compiler fails to check it.
    Unresolved { key: String },
}

#[derive(Clone, Debug)]
pub struct File {
    pub path: PathBuf,
    pub ns: String,
    pub aliases: Vec<(String, String)>,
    pub source: String,
    /// The names, ordered by their start.
    pub names: Vec<Name>,
    /// The file's declarations, as indexes in the load's declarations.
    pub tlds: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct LoadError {
    pub message: String,
    /// The file and the range where the load stopped.
    pub at: Option<(PathBuf, TextRange)>,
}

/// The result of loading one entry.
#[derive(Debug, Default)]
pub struct Semantics {
    pub files: Vec<File>,
    entry: Option<PathBuf>,
    /// The aliases of the entry's imports, also when the load stopped in them.
    pub entry_aliases: Vec<(String, String)>,
    declarations: Vec<Declaration>,
    by_key: HashMap<(String, bool), usize>,
    by_identity: HashMap<Identity, usize>,
    pub error: Option<LoadError>,
    /// For a partial load, the local binders in scope where it stopped.
    pub scope: Vec<(String, Binder)>,
    book: Book,
    /// The book name of each declaration index.
    tld_names: Vec<Option<String>>,
    /// The range of the type annotation of each annotated binder.
    binder_types: HashMap<Binder, (usize, TextRange)>,
}

/// UTF-16 offsets to byte offsets, for one text.
struct Utf16 {
    /// After each non-ASCII character: its end in UTF-16 units and in bytes.
    steps: Vec<(usize, usize)>,
}

impl Utf16 {
    fn new(text: &str) -> Self {
        let mut steps = Vec::new();
        let mut units = 0;
        for (byte, c) in text.char_indices() {
            units += c.len_utf16();
            if !c.is_ascii() {
                steps.push((units, byte + c.len_utf8()));
            }
        }
        Self { steps }
    }

    fn byte(&self, units: usize) -> usize {
        let step = self.steps.partition_point(|(end, _)| *end <= units);
        match step.checked_sub(1).and_then(|index| self.steps.get(index)) {
            Some((end, byte)) => byte + (units - end),
            None => units,
        }
    }
}

fn byte_range(file: &LoadedFile, utf16: &Utf16, span: Span) -> TextRange {
    TextRange::new(
        utf16.byte(file.source_offset(span.beg)),
        utf16.byte(file.source_offset(span.end)),
    )
}

fn source_text(file: &LoadedFile, utf16: &Utf16, span: Option<Span>) -> Option<String> {
    let range = byte_range(file, utf16, span?);
    file.source.get(range.start..range.end).map(str::to_owned)
}

impl Semantics {
    /// Build the semantics of a load.
    #[must_use]
    pub fn new(load: Load) -> Self {
        let mut semantics = Self::default();
        let mut utf16s = Vec::with_capacity(load.files.len());
        for file in &load.files {
            let utf16 = Utf16::new(&file.source);
            let mut names: Vec<Name> = file
                .occurrences
                .iter()
                .map(|occurrence| Name {
                    range: byte_range(file, &utf16, occurrence.span),
                    meaning: occurrence.meaning.clone(),
                    implicit: occurrence.implicit,
                    tld: occurrence.tld,
                })
                .collect();
            names.sort_by_key(|name| (name.range.start, name.range.end));
            semantics.files.push(File {
                path: PathBuf::from(&file.real),
                ns: file.ns.clone(),
                aliases: file.al.clone(),
                source: file.source.clone(),
                names,
                tlds: file.tlds.clone(),
            });
            utf16s.push(utf16);
        }
        for (file, utf16) in load.files.iter().zip(&utf16s) {
            semantics.declare(&load, utf16, file);
        }
        semantics.error = load.error.as_ref().map(|error| LoadError {
            message: error.exp.clone(),
            at: error.at.as_ref().and_then(|(_, beg, end)| {
                let span = Span {
                    beg: *beg,
                    end: *end,
                };
                match load.error_text? {
                    ErrorText::Parsed(index) => {
                        let file = load.files.get(index)?;
                        let range = byte_range(file, &utf16s[index], span);
                        Some((PathBuf::from(&file.real), range))
                    }
                    ErrorText::Source(index) => {
                        let file = load.reading.get(index)?;
                        let utf16 = Utf16::new(&file.source);
                        let range = TextRange::new(utf16.byte(span.beg), utf16.byte(span.end));
                        Some((PathBuf::from(&file.real), range))
                    }
                }
            }),
        });
        semantics.entry = load.entry.as_ref().map(PathBuf::from);
        semantics.entry_aliases = match semantics.entry() {
            Some(entry) => entry.aliases.clone(),
            None => load
                .reading
                .first()
                .filter(|file| load.entry.as_ref() == Some(&file.real))
                .map(|file| file.al.clone())
                .unwrap_or_default(),
        };
        semantics.type_binders(&load, &utf16s);
        if let Some(scope) = &load.scope {
            semantics.scope.clone_from(&scope.binders);
        }
        semantics.tld_names = load.tlds.iter().map(|tld| tld.name.clone()).collect();
        semantics.book = load.book;
        semantics
    }

    fn type_binders(&mut self, load: &Load, utf16s: &[Utf16]) {
        for (tld_index, tld) in load.tlds.iter().enumerate() {
            let Some(file_index) = load.files.iter().rposition(|file| file.ns == tld.file) else {
                continue;
            };
            let file = &load.files[file_index];
            let cells = tld
                .tele
                .iter()
                .chain(&tld.params)
                .flatten()
                .chain(tld.ctrs.iter().flatten().flat_map(|(_, fields)| fields));
            for cell in cells {
                if matches!(*cell.t, Term::Qnt { .. }) {
                    continue;
                }
                let Some(span) = cell.t.span() else {
                    continue;
                };
                let binder = Binder {
                    tld: tld_index,
                    index: cell.i,
                };
                let range = byte_range(file, &utf16s[file_index], span);
                self.binder_types.insert(binder, (file_index, range));
            }
        }
    }

    /// The source text of a binder's type annotation.
    #[must_use]
    pub fn binder_type(&self, binder: Binder) -> Option<&str> {
        let (file, range) = self.binder_types.get(&binder)?;
        self.files.get(*file)?.source.get(range.start..range.end)
    }

    /// The range of a binder's type annotation in the entry.
    #[must_use]
    pub fn binder_type_range(&self, binder: Binder) -> Option<TextRange> {
        let (file, range) = self.binder_types.get(&binder)?;
        (*file + 1 == self.files.len()).then_some(*range)
    }

    fn declare(&mut self, load: &Load, utf16: &Utf16, file: &LoadedFile) {
        for occurrence in &file.occurrences {
            let Meaning::Declaration { key, ctr } = &occurrence.meaning else {
                continue;
            };
            let range = byte_range(file, utf16, occurrence.span);
            let name = file.source.get(range.start..range.end).unwrap_or_default();
            let identity = Identity {
                path: PathBuf::from(&file.real),
                name: name.to_owned(),
                ctr: *ctr,
            };
            let tld = load.tlds.get(occurrence.tld);
            let kind = match (ctr, tld.map(|tld| tld.kind)) {
                (true, _) => DeclarationKind::Constructor,
                (false, Some(TraceKind::Type)) => DeclarationKind::Type,
                (false, Some(TraceKind::Law)) => DeclarationKind::Law,
                (false, _) => DeclarationKind::Def,
            };
            let parameters = match (ctr, tld) {
                (false, Some(tld)) => tld
                    .tele
                    .iter()
                    .flatten()
                    .map(|cell| {
                        let typed = !matches!(*cell.t, Term::Qnt { .. });
                        let ty = typed
                            .then(|| source_text(file, utf16, cell.t.span()))
                            .flatten();
                        (cell.k.clone(), ty)
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let line_start = file.source[..range.start]
                .rfind('\n')
                .map_or(0, |at| at + 1);
            let line_end = file.source[range.start..]
                .find('\n')
                .map_or(file.source.len(), |at| range.start + at);
            let line = file.source[line_start..line_end].trim();
            let detail = line.strip_suffix(':').unwrap_or(line).to_owned();
            let position = self.declarations.len();
            self.by_key.insert((key.clone(), *ctr), position);
            self.by_identity.insert(identity.clone(), position);
            self.declarations.push(Declaration {
                identity,
                key: key.clone(),
                kind,
                range,
                tld: occurrence.tld,
                parameters,
                detail,
                templates: load.book.def(key).map_or(0, |def| def.x),
                type_former: !ctr
                    && tld.is_some_and(|tld| tld.kind != TraceKind::Type && type_former(load, tld)),
            });
        }
    }

    /// The entry: the last file loaded, when the load reached it.
    #[must_use]
    pub fn entry(&self) -> Option<&File> {
        let path = self.entry.as_ref()?;
        self.files.iter().rfind(|file| file.path == *path)
    }

    /// The file loaded from `path`.
    #[must_use]
    pub fn file(&self, path: &Path) -> Option<&File> {
        self.files.iter().find(|file| file.path == path)
    }

    /// The explicit names of the entry whose range holds `offset`, innermost
    /// first. Two names share a range when one token both uses and binds.
    #[must_use]
    pub fn names_at(&self, offset: usize) -> Vec<&Name> {
        let Some(entry) = self.entry() else {
            return Vec::new();
        };
        let end = entry
            .names
            .partition_point(|name| name.range.start <= offset);
        let mut found: Vec<&Name> = entry.names[..end]
            .iter()
            .filter(|name| !name.implicit && name.range.start <= offset && offset <= name.range.end)
            .collect();
        found.sort_by_key(|name| name.range.end - name.range.start);
        found
    }

    /// What a name of the entry refers to.
    #[must_use]
    pub fn target(&self, name: &Name) -> Target {
        match &name.meaning {
            Meaning::Binder(binder) | Meaning::Local(binder) => Target::Local {
                binder: *binder,
                range: self.binder_range(*binder),
            },
            Meaning::Global { key, ctr } | Meaning::Declaration { key, ctr } => {
                match self.by_key.get(&(key.clone(), *ctr)) {
                    Some(index) => Target::Global(self.declarations[*index].identity.clone()),
                    None => Target::Unresolved { key: key.clone() },
                }
            }
        }
    }

    /// The range of the token that declares a binder of the entry.
    #[must_use]
    pub fn binder_range(&self, binder: Binder) -> Option<TextRange> {
        self.entry()?
            .names
            .iter()
            .find(|name| name.meaning == Meaning::Binder(binder))
            .map(|name| name.range)
    }

    #[must_use]
    pub fn declaration(&self, identity: &Identity) -> Option<&Declaration> {
        self.by_identity
            .get(identity)
            .map(|index| &self.declarations[*index])
    }

    /// The declaration of a book key: a constructor's (`ctr`) or another's.
    #[must_use]
    pub fn declaration_of_key(&self, key: &str, ctr: bool) -> Option<&Declaration> {
        self.by_key
            .get(&(key.to_owned(), ctr))
            .map(|index| &self.declarations[*index])
    }

    /// The declarations of a loaded file.
    pub fn declarations_in<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = &'a Declaration> {
        self.declarations
            .iter()
            .filter(move |declaration| declaration.identity.path == path)
    }

    /// The declaration that holds the names of a declaration index: a def,
    /// a law or a type, by its name in the book.
    #[must_use]
    pub fn holder(&self, tld: usize) -> Option<&Declaration> {
        let key = self.tld_names.get(tld)?.as_ref()?;
        self.declaration_of_key(key, false)
    }

    /// Each spelling that names a declaration in the entry, as the entry's
    /// parser would resolve it, with the declaration it names.
    #[must_use]
    pub fn visible(&self) -> Vec<(String, &Declaration)> {
        let Some(entry) = self.entry() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for declaration in &self.declarations {
            let key = &declaration.key;
            let (ns, name) = key.split_once(':').unwrap_or(("", key));
            let mut spellings = Vec::new();
            if ns == entry.ns || ns.is_empty() {
                spellings.push(name.to_owned());
            }
            for (alias, sub) in &entry.aliases {
                if sub == ns {
                    spellings.push(format!("{alias}.{name}"));
                }
            }
            for spelling in spellings {
                let resolved = frontend::resolve(&self.book, &entry.ns, &entry.aliases, &spelling);
                if resolved.as_deref() == Some(key.as_str()) {
                    out.push((spelling, declaration));
                }
            }
        }
        out
    }
}
