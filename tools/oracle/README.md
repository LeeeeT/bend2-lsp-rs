# Front end oracle

`src/frontend` is a port of the front end of the Bend compiler: the loader,
the parser and the body flattening of `bend2/bend.ts`. These tools check that
the port builds exactly what the compiler builds.

## The oracle

`instrument.py` copies the pinned `bend.ts` (see
`scripts/bend_compiler_pins.json`) and adds tracing hooks. The hooks only
record values; they change nothing that the parser computes. `oracle.ts`
loads each entry file with the compiler's own `book_load` and prints, for each
entry:

- the files that it loaded, with their namespaces and aliases;
- a SHA-256 digest of each declaration's trace: its telescopes, types and
  constructors, and each body before and after flattening, with every name,
  binder index and span;
- the error that stopped the load, if any.

`bend2-oracle` (this crate) prints the same lines from the port. With
`--dump`, both print the whole traces instead of digests. `compare.py` reports
each entry whose lines differ.

## Occurrences

`bend2-oracle --occurrences` checks what each name in each loaded file means:

- each occurrence must agree with the terms that the parser built: a local at
  a span with the variable of that binder, a binder with a binder node of that
  index and name, a global with a node of that key;
- each identifier token of each file must have an occurrence.

## Running

```sh
BEND=<checkout of bendlang/bend at the pinned commit>
mkdir -p work/bend2
python3 tools/oracle/instrument.py "$BEND/bend2/bend.ts" work/bend2/bend.ts
cp "$BEND/bend2/base.bend" tools/oracle/oracle.ts work/bend2/
find <corpus> -name '*.bend' | sort > entries

# The hub URL is unreachable on purpose: no package is fetched.
export BEND_LIB=<a BendHub mirror in BEND_LIB layout> BEND_HUB=http://127.0.0.1:9
(cd work/bend2 && bun oracle.ts) < entries > oracle.jsonl
BEND_BASE=work/bend2/base.bend cargo run --release -p bend2-oracle < entries > port.jsonl
python3 tools/oracle/compare.py oracle.jsonl port.jsonl
BEND_BASE=work/bend2/base.bend cargo run --release -p bend2-oracle -- --occurrences < entries
```

Each entry loads Base, so a run spends most of its time on Base's trace. Run
one process at a time: a large file can take a few hundred megabytes.
