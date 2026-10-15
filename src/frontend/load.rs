//! A port of the loader of the Bend compiler (`book_load` in `bend.ts`): the
//! imports, their namespaces and aliases, and the load order.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use super::{
    Error,
    book::Book,
    occurrence::{Occurrence, settle},
    parse::{Parse, is_js_space, is_word_unit, text},
    trace::TldTrace,
};

/// The files that the loader reads.
pub trait Files {
    /// The text of a file, decoded as UTF-8 with replacement.
    fn read(&self, path: &Path) -> Option<String>;
    /// The canonical path of an existing file.
    fn real(&self, path: &Path) -> Option<PathBuf>;
}

/// The files on disk.
pub struct Disk;

impl Files for Disk {
    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read(path)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    fn real(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }
}

/// The settings of the compiler that loading depends on.
#[derive(Clone, Debug)]
pub struct Env {
    /// `BEND_LIB`, absolute and normalized.
    pub lib: String,
    /// The canonical path of `base.bend`.
    pub base: String,
    /// `BEND_HUB`, as error messages show it.
    pub hub: String,
}

/// A file as the loader parsed it.
#[derive(Clone, Debug)]
pub struct LoadedFile {
    pub real: String,
    pub ns: String,
    pub al: Vec<(String, String)>,
    /// The text that the parser read: the file with its import lines
    /// blanked, in UTF-16 code units.
    pub parsed: Vec<u16>,
    /// What each name means, in the declarations that parsed completely.
    pub occurrences: Vec<Occurrence>,
}

/// Everything that one load produced, up to its first error.
#[derive(Debug, Default)]
pub struct Load {
    pub book: Book,
    pub files: Vec<LoadedFile>,
    pub tlds: Vec<TldTrace>,
    pub error: Option<Error>,
}

struct Loader<'a> {
    files: &'a dyn Files,
    env: &'a Env,
    load: Load,
    seen: HashMap<String, Option<String>>,
}

/// Load `entry` as the compiler loads the file it checks.
#[must_use]
pub fn load(files: &dyn Files, env: &Env, entry: &str) -> Load {
    let mut loader = Loader {
        files,
        env,
        load: Load::default(),
        seen: HashMap::new(),
    };
    if let Err(error) = loader.book_load(entry, "", None, None) {
        loader.load.error = Some(error);
    }
    loader.load
}

type ErrorSpan = Option<(String, usize, usize)>;

fn error(exp: impl Into<String>, obs: Option<String>, at: &ErrorSpan) -> Error {
    Error {
        exp: exp.into(),
        obs,
        at: at.clone(),
    }
}

fn is_hub(s: &str) -> bool {
    hub_prefix(s).is_some()
}

/// The length of a `0x<hex>/` prefix.
fn hub_prefix(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("0x")?;
    let hex = rest
        .bytes()
        .take_while(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        .count();
    (hex > 0 && rest.as_bytes().get(hex) == Some(&b'/')).then_some(hex + 3)
}

fn plain_segment(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The `ok` test of `book_load`.
fn ok(s: &str, lib: bool) -> bool {
    if is_hub(s) != lib {
        return false;
    }
    let mut rest = &s[hub_prefix(s).unwrap_or(0)..];
    if let Some(stripped) = rest.strip_prefix('/') {
        rest = stripped;
    } else {
        while let Some(stripped) = rest.strip_prefix("../") {
            rest = stripped;
        }
    }
    rest.split('/').all(plain_segment)
}

/// `path.posix.normalize`.
fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            part => parts.push(part),
        }
    }
    let mut out = parts.join("/");
    if out.is_empty() && !absolute {
        out.push('.');
    }
    if trailing && !out.is_empty() {
        out.push('/');
    }
    if absolute { format!("/{out}") } else { out }
}

/// `path.posix.resolve(dir, rel)` for an absolute `dir`.
fn resolve(dir: &str, rel: &str) -> String {
    let joined = if rel.starts_with('/') {
        rel.to_owned()
    } else {
        format!("{dir}/{rel}")
    };
    let out = normalize(&joined);
    if out.len() > 1 {
        out.trim_end_matches('/').to_owned()
    } else {
        out
    }
}

/// `path.posix.relative(from, to)` for absolute paths.
fn relative(from: &str, to: &str) -> String {
    let from = resolve("/", from);
    let to = resolve("/", to);
    if from == to {
        return String::new();
    }
    let a: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let b: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<&str> = vec![".."; a.len() - common];
    parts.extend(&b[common..]);
    parts.join("/")
}

fn trim_js(s: &[u16]) -> &[u16] {
    let start = s.iter().position(|c| !is_js_space(*c)).unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|c| !is_js_space(*c))
        .map_or(start, |i| i + 1);
    &s[start..end.max(start)]
}

fn is_line_terminator(c: u16) -> bool {
    matches!(c, 10 | 13 | 0x2028 | 0x2029)
}

/// `/^import\s+(\S+)(?:\s+as\s+([A-Za-z_]\w*))?\s*(?:#.*)?$/` on a trimmed line.
fn import_line(line: &[u16]) -> Option<(Vec<u16>, Option<Vec<u16>>)> {
    let rest = line.strip_prefix(&"import".encode_utf16().collect::<Vec<_>>()[..])?;
    let spaces = rest.iter().take_while(|c| is_js_space(**c)).count();
    if spaces == 0 {
        return None;
    }
    let rest = &rest[spaces..];
    let token = rest.iter().take_while(|c| !is_js_space(**c)).count();
    for len in (1..=token).rev() {
        let (path, tail) = rest.split_at(len);
        if let Some(alias) = alias_tail(tail) {
            return Some((path.to_vec(), Some(alias)));
        }
        if comment_tail(tail) {
            return Some((path.to_vec(), None));
        }
    }
    None
}

fn alias_tail(tail: &[u16]) -> Option<Vec<u16>> {
    let spaces = tail.iter().take_while(|c| is_js_space(**c)).count();
    if spaces == 0 {
        return None;
    }
    let rest = tail[spaces..].strip_prefix(&[u16::from(b'a'), u16::from(b's')][..])?;
    let spaces = rest.iter().take_while(|c| is_js_space(**c)).count();
    if spaces == 0 {
        return None;
    }
    let rest = &rest[spaces..];
    let head = rest.first().copied()?;
    if !(u8::try_from(head).is_ok_and(|b| b.is_ascii_alphabetic()) || head == u16::from(b'_')) {
        return None;
    }
    let name = 1 + rest[1..].iter().take_while(|c| is_word_unit(**c)).count();
    comment_tail(&rest[name..]).then(|| rest[..name].to_vec())
}

fn comment_tail(tail: &[u16]) -> bool {
    let spaces = tail.iter().take_while(|c| is_js_space(**c)).count();
    let rest = &tail[spaces..];
    rest.is_empty()
        || rest.first() == Some(&u16::from(b'#')) && !rest.iter().any(|c| is_line_terminator(*c))
}

fn index_of(haystack: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn named(nv: &str) -> bool {
    let Some((name, version)) = nv.split_once('@') else {
        return false;
    };
    let mut name_bytes = name.bytes();
    let name_ok = (1..=64).contains(&name.len())
        && name_bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && name_bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let parts: Vec<&str> = version.split('.').collect();
    name_ok
        && parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (*part == "0" || !part.starts_with('0'))
        })
}

impl Loader<'_> {
    fn exists(&self, path: &str) -> bool {
        self.files.real(Path::new(path)).is_some()
    }

    fn book_file(&self, file: &str, at: &ErrorSpan) -> Result<String, Error> {
        let lib = format!("{}/", self.env.lib);
        if let Some(rest) = file.strip_prefix(&lib)
            && !self.exists(file)
        {
            let pkg = rest.split('/').next().unwrap_or("");
            let hash = pkg.get(2..).unwrap_or("");
            return Err(error(
                format!(
                    "a file at {}/{pkg}/manifest hashing to {hash}",
                    self.env.hub
                ),
                None,
                at,
            ));
        }
        match self.files.real(Path::new(file)) {
            Some(real) => Ok(real.to_string_lossy().into_owned()),
            None => Err(error(format!("no such file: {file}"), None, at)),
        }
    }

    fn name_hash(&self, nv: &str, at: &ErrorSpan) -> Result<String, Error> {
        if !named(nv) {
            return Err(error(
                "a package as <name>@<version>: a-z, 0-9 and -, 1 to 64 characters, at four numbers like 1.0.0.0",
                Some(format!("'{nv}'")),
                at,
            ));
        }
        let path = format!("{}/names/{nv}", self.env.lib);
        let old = self
            .files
            .read(Path::new(&path))
            .map(|text| {
                let units: Vec<u16> = text.encode_utf16().collect();
                String::from_utf16_lossy(trim_js(&units))
            })
            .unwrap_or_default();
        let valid = old.len() == 34
            && old.starts_with("0x")
            && old[2..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if valid {
            return Ok(old);
        }
        Err(error(
            format!("a package named {nv} on {}", self.env.hub),
            None,
            at,
        ))
    }

    /// `book_load`
    fn book_load(
        &mut self,
        file: &str,
        ns: &str,
        at: Option<ErrorSpan>,
        root: Option<&str>,
    ) -> Result<(), Error> {
        let at = at.unwrap_or(None);
        let real = self.book_file(file, &at)?;
        if let Some(state) = self.seen.get(&real) {
            if state.is_none() {
                return Err(error(format!("an import cycle through {file}"), None, &at));
            }
            return Ok(());
        }
        self.seen.insert(real.clone(), None);
        let dir = real[..real.rfind('/').map_or(0, |slash| slash + 1)].to_owned();
        let top = root.map_or_else(|| dir.clone(), str::to_owned);
        let source = self.files.read(Path::new(&real)).unwrap_or_default();
        let units: Vec<u16> = source.encode_utf16().collect();
        let lines: Vec<&[u16]> = units.split(|unit| *unit == 10).collect();
        let mut body: Vec<&[u16]> = lines.clone();
        let mut aliases: Vec<(String, String)> = Vec::new();
        let mut offset = 0;
        for (index, raw) in lines.iter().enumerate() {
            let line_at = offset;
            offset += raw.len() + 1;
            let line = trim_js(raw);
            if line.is_empty() || line.first() == Some(&u16::from(b'#')) {
                continue;
            }
            let keyword: Vec<u16> = "import".encode_utf16().collect();
            let starts =
                line.starts_with(&keyword) && line.get(6).is_none_or(|unit| is_js_space(*unit));
            if !starts {
                break;
            }
            body[index] = &[];
            let import = Import {
                raw,
                line,
                line_at,
                ns,
                dir: &dir,
                top: &top,
            };
            self.import(&import, &mut aliases)?;
        }
        self.parse_file(real, ns, aliases, body.join(&10))
    }

    fn import(
        &mut self,
        import: &Import,
        aliases: &mut Vec<(String, String)>,
    ) -> Result<(), Error> {
        let parsed = import_line(import.line);
        let needle = parsed.as_ref().map_or(import.line, |(path, _)| &path[..]);
        let found = index_of(import.raw, needle).unwrap_or(usize::MAX - import.line_at);
        let beg = import.line_at + found;
        let sp: ErrorSpan = Some((import.ns.to_owned(), beg, beg));
        let line_text = text(import.line);
        let Some((path, alias)) =
            parsed.filter(|(path, alias)| alias.is_some() || text(path) == "Base")
        else {
            return Err(error(
                "an import ('import Base', or 'import <path> as <Name>')",
                Some(format!("'{line_text}'")),
                &sp,
            ));
        };
        let path = text(&path);
        let Some(alias) = alias else {
            let base = self.env.base.clone();
            return self.book_load(&base, "", Some(sp), None);
        };
        let alias = text(&alias);
        if !path.ends_with(".bend") {
            let obs = Some(format!("'{path}'"));
            return Err(error("an import of a .bend file", obs, &sp));
        }
        if aliases.iter().any(|(name, _)| *name == alias) {
            return Err(error(
                format!("a fresh alias ({alias} names an earlier import)"),
                Some(format!("'{line_text}'")),
                &sp,
            ));
        }
        let bad = || {
            error(
                "an import path of plain names (letters, digits, _ and -; the hub's files import the hub's)",
                Some(format!("'{path}'")),
                &sp,
            )
        };
        let named_package = path
            .find('/')
            .map(|slash| &path[..slash])
            .filter(|head| head.contains('@'));
        let spelled = match named_package {
            Some(package) => format!(
                "{}{}",
                self.name_hash(package, &sp)?,
                &path[package.len()..]
            ),
            None => path.clone(),
        };
        let normalized = normalize(&spelled);
        let checked = spelled.strip_prefix("./").unwrap_or(&spelled);
        let checked = checked.get(..checked.len().saturating_sub(5)).unwrap_or("");
        if !ok(checked, is_hub(&spelled)) {
            return Err(bad());
        }
        let target = if is_hub(&spelled) {
            format!("{}/{normalized}", self.env.lib)
        } else {
            resolve(import.dir.trim_end_matches('/'), &normalized)
        };
        let got = self.book_file(&target, &sp)?;
        let lib = self.files.real(Path::new(&self.env.lib)).map_or_else(
            || "\0".to_owned(),
            |real| format!("{}/", real.to_string_lossy()),
        );
        let in_lib = got.starts_with(&lib);
        let sub_path = if in_lib {
            got[lib.len()..].to_owned()
        } else {
            relative(import.top, &got)
        };
        let sub = sub_path
            .strip_suffix(".bend")
            .unwrap_or(&sub_path)
            .to_owned();
        if !ok(&sub, in_lib) || (is_hub(import.ns) && !in_lib) {
            return Err(bad());
        }
        aliases.push((alias, sub.clone()));
        self.book_load(&got, &sub, Some(sp), Some(import.top))
    }

    fn parse_file(
        &mut self,
        real: String,
        ns: &str,
        aliases: Vec<(String, String)>,
        parsed: Vec<u16>,
    ) -> Result<(), Error> {
        let first = self.load.book.order.len();
        let mut events = Vec::new();
        let mut p = Parse {
            book: &mut self.load.book,
            str: &parsed,
            ns,
            al: &aliases,
            pos: 0,
            stk: Vec::new(),
            frs: 0,
            tlds: &mut self.load.tlds,
            events: &mut events,
        };
        let result = p.book();
        self.load.files.push(LoadedFile {
            real: real.clone(),
            ns: ns.to_owned(),
            al: aliases,
            parsed,
            occurrences: settle(events),
        });
        result?;
        if real == self.env.base {
            self.load.book.mark_base(first);
        }
        self.seen.insert(real, Some(ns.to_owned()));
        Ok(())
    }
}

/// One import line of a file.
struct Import<'a> {
    raw: &'a [u16],
    line: &'a [u16],
    line_at: usize,
    ns: &'a str,
    dir: &'a str,
    top: &'a str,
}

#[cfg(test)]
mod tests {
    use super::{import_line, normalize, ok, relative, resolve};

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn import_lines_match_the_compiler_regex() {
        let found = import_line(&units("import ./x.bend as X # note"));
        assert_eq!(found, Some((units("./x.bend"), Some(units("X")))));
        let greedy = import_line(&units("import foo#bar"));
        assert_eq!(greedy, Some((units("foo#bar"), None)));
        let comment = import_line(&units("import foo#bar baz"));
        assert_eq!(comment, Some((units("foo"), None)));
        assert_eq!(
            import_line(&units("import Base")),
            Some((units("Base"), None))
        );
        assert_eq!(import_line(&units("imports x")), None);
    }

    #[test]
    fn paths_follow_node_posix_rules() {
        assert_eq!(normalize("./a/b.bend"), "a/b.bend");
        assert_eq!(normalize("../../a.bend"), "../../a.bend");
        assert_eq!(resolve("/x/y", "../z.bend"), "/x/z.bend");
        assert_eq!(relative("/x/y/", "/x/z/w.bend"), "../z/w.bend");
        assert!(ok("../lib/text/main", false));
        assert!(ok("0xab/a/b", true));
        assert!(!ok("a/../b", false));
    }
}
