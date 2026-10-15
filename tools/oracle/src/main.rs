//! Load each entry file with the front end port and print what its parser
//! built, in the format of `oracle.ts`.
//!
//! usage: `BEND_LIB=.. BEND_BASE=.. BEND_HUB=.. bend2-oracle [--dump] < entries`

mod occurrences;

use std::io::{self, BufRead, Write};

use bend2_lsp::frontend::{Disk, Env, Load, TldTrace, TraceKind, error_json, json_string, load};
use sha2::{Digest, Sha256};

fn digest(text: &str) -> String {
    let mut out = String::new();
    for byte in Sha256::digest(text.as_bytes()) {
        out.extend(char::from_digit(u32::from(byte >> 4), 16));
        out.extend(char::from_digit(u32::from(byte & 15), 16));
    }
    out
}

fn kind(tld: &TldTrace) -> &'static str {
    match tld.kind {
        TraceKind::Def => "def",
        TraceKind::Type => "type",
        TraceKind::Law => "law",
    }
}

fn line(entry: &str, loaded: &Load, dump: bool) -> String {
    let mut out = String::from("{\"entry\":");
    out.push_str(&json_string(entry));
    out.push_str(",\"files\":[");
    for (index, file) in loaded.files.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"real\":");
        out.push_str(&json_string(&file.real));
        out.push_str(",\"ns\":");
        out.push_str(&json_string(&file.ns));
        out.push_str(",\"al\":{");
        for (position, (alias, sub)) in file.al.iter().enumerate() {
            if position > 0 {
                out.push(',');
            }
            out.push_str(&json_string(alias));
            out.push(':');
            out.push_str(&json_string(sub));
        }
        out.push_str("}}");
    }
    out.push_str("],\"tlds\":[");
    for (index, tld) in loaded.tlds.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let json = tld.json();
        if dump {
            out.push_str(&json);
            continue;
        }
        out.push_str("{\"file\":");
        out.push_str(&json_string(&tld.file));
        out.push_str(",\"kind\":");
        out.push_str(&json_string(kind(tld)));
        out.push_str(",\"name\":");
        out.push_str(
            &tld.name
                .as_deref()
                .map_or_else(|| "null".to_owned(), json_string),
        );
        out.push_str(",\"digest\":\"");
        out.push_str(&digest(&json));
        out.push_str("\"}");
    }
    out.push_str("],\"error\":");
    match &loaded.error {
        Some(error) => out.push_str(&error_json(error)),
        None => out.push_str("null"),
    }
    out.push('}');
    out
}

fn main() -> io::Result<()> {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let env = Env {
        lib: var("BEND_LIB"),
        base: var("BEND_BASE"),
        hub: var("BEND_HUB"),
    };
    let dump = std::env::args().any(|arg| arg == "--dump");
    let check = std::env::args().any(|arg| arg == "--occurrences");
    let timed = std::env::args().any(|arg| arg == "--load");
    let mut output = io::stdout().lock();
    for entry in io::stdin().lock().lines() {
        let entry = entry?;
        if entry.is_empty() {
            continue;
        }
        let start = std::time::Instant::now();
        let loaded = load(&Disk, &env, &entry);
        if timed {
            let files = loaded.files.len();
            let elapsed = start.elapsed().as_micros();
            let (found, tokens) = occurrences::count(&loaded);
            let parsed = loaded
                .files
                .iter()
                .filter(|file| file.ns.is_empty())
                .map(|file| file.parsed.len())
                .collect::<Vec<_>>();
            writeln!(
                output,
                "{entry}: {files} files, {elapsed} us, parsed {parsed:?}, {found} occurrences, {tokens} tokens"
            )?;
            continue;
        }
        if check {
            let (found, tokens) = occurrences::count(&loaded);
            eprintln!("{entry}: {found} occurrences, {tokens} tokens");
            for problem in occurrences::check(&loaded) {
                writeln!(output, "{entry}: {problem}")?;
            }
            continue;
        }
        writeln!(output, "{}", line(&entry, &loaded, dump))?;
    }
    Ok(())
}
