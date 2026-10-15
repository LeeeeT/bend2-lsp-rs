#!/usr/bin/env python3
"""Copy the pinned bend.ts and add tracing hooks that record what its parser builds.

The hooks only record values. They never change what the parser computes.
"""

import hashlib
import json
import sys
from pathlib import Path

PINS = json.loads((Path(__file__).parents[2] / "scripts" / "bend_compiler_pins.json").read_text())

HEADER = """
export type Trace = {
  files: Array<{ real: string; ns: string; al: Record<Name, Name> }>;
  tlds: any[];
  snap: (file: unknown, b: Body, out: LTerm) => unknown;
};
export const TRACE: Trace = { files: [], tlds: [], snap: (_file, b, out) => [b, out] };
let TRACE_TLD: any = null;
function trace_tld(p: Parse, kind: string): any {
  TRACE_TLD = { file: p.ns, kind, bodies: [], p };
  TRACE.tlds.push(TRACE_TLD);
  return TRACE_TLD;
}
function trace_body(b: Body, out: LTerm): void {
  TRACE_TLD?.bodies.push(TRACE.snap(TRACE_TLD.p, b, out));
}
"""

EDITS = [
    (
        'import * as url from "node:url";\n',
        'import * as url from "node:url";\n' + HEADER,
    ),
    (
        """export function parse_term_tup(p: Parse, beg: number): LTerm {
  parse_skip(p);
  const b = parse_body(p, parse_col(p.str, p.pos) - 1);
  parse_skip(p);
  if (b.$ !== "Match" && b.$ !== "Local" && parse_take(p, ",")) {
    const rest = parse_term_tup(p, beg);
    return Ctr("Tuple", [b, rest], parse_span(p, beg));
  }
  const out = body_flatten(b, [], () => p.frs++);
""",
        """export function parse_term_tup(p: Parse, beg: number): LTerm {
  parse_skip(p);
  const b = parse_body(p, parse_col(p.str, p.pos) - 1);
  parse_skip(p);
  if (b.$ !== "Match" && b.$ !== "Local" && parse_take(p, ",")) {
    const rest = parse_term_tup(p, beg);
    return Ctr("Tuple", [b, rest], parse_span(p, beg));
  }
  const out = body_flatten(b, [], () => p.frs++);
  trace_body(b, out);
""",
    ),
    (
        """  const b = parse_body(p, parse_col(p.str, p.pos) - 1);
  return body_flatten(b, [], () => p.frs++);
}""",
        """  const b = parse_body(p, parse_col(p.str, p.pos) - 1);
  const out = body_flatten(b, [], () => p.frs++);
  trace_body(b, out);
  return out;
}""",
    ),
    (
        """  const nm  = parse_name(p);
  const q   = parse_reso(p, nm);""",
        """  const tr  = trace_tld(p, "def");
  const nm  = parse_name(p);
  const q   = parse_reso(p, nm);""",
    ),
    (
        """  const tele = parse_tele(p, ")", tk);
  parse_skip(p);
  let def: Def;""",
        """  const tele = parse_tele(p, ")", tk);
  tr.name = k;
  tr.law = law !== undefined;
  tr.unsafe = un;
  tr.tele = tele;
  parse_skip(p);
  let def: Def;""",
    ),
    (
        """    def = book.tlds[k] = { $: "Def", n: tele.length, x: tk.length, T: term_higher(tele_bind(tele, parse_term(p))), v: null, m: p.ns };""",
        """    tr.ret = parse_term(p);
    def = book.tlds[k] = { $: "Def", n: tele.length, x: tk.length, T: term_higher(tele_bind(tele, tr.ret)), v: null, m: p.ns };""",
    ),
    (
        """      def.i.push(p.dir + eff);
    }""",
        """      def.i.push(p.dir + eff);
    }
    tr.imports = def.i.length;""",
    ),
    (
        """    def.v = term_higher(term_lower(term_higher(body_flatten(parse_body(p), vars, () => p.frs++))));""",
        """    const body = parse_body(p);
    const flat = body_flatten(body, vars, () => p.frs++);
    trace_body(body, flat);
    def.v = term_higher(term_lower(term_higher(flat)));""",
    ),
    (
        """    if (parse_word(p, "type")) {
      const k = parse_fresh(p, parse_name(p));
      parse_skip(p);
      const params = parse_take(p, "<") ? parse_tele(p, ">") : [];
      if (!parse_word(p, "is")) {
        parse_fail(p, "'is'");
      }
      const K = parse_term(p);""",
        """    if (parse_word(p, "type")) {
      const tr = trace_tld(p, "type");
      const k = parse_fresh(p, parse_name(p));
      tr.name = k;
      parse_skip(p);
      const params = parse_take(p, "<") ? parse_tele(p, ">") : [];
      tr.params = params;
      tr.ctrs = [];
      if (!parse_word(p, "is")) {
        parse_fail(p, "'is'");
      }
      const K = parse_term(p);
      tr.K = K;""",
    ),
    (
        """        const fs = parse_tele(p, "}");""",
        """        const fs = parse_tele(p, "}");
        tr.ctrs.push([c, fs]);""",
    ),
    (
        """    if (parse_word(p, "law")) {
      const k = parse_fresh(p, parse_name(p));""",
        """    if (parse_word(p, "law")) {
      const tr = trace_tld(p, "law");
      const k = parse_fresh(p, parse_name(p));
      tr.name = k;""",
    ),
    (
        """      const n = cls.findIndex((c) => !c[0]);""",
        """      tr.T = T;
      const n = cls.findIndex((c) => !c[0]);""",
    ),
    (
        """  const n0 = book.order.length;
  parse_book(book, dir, body.join("\\n"), ns, al);""",
        """  const n0 = book.order.length;
  TRACE.files.push({ real, ns, al: { ...al } });
  parse_book(book, dir, body.join("\\n"), ns, al);""",
    ),
]


def main() -> None:
    source, output = Path(sys.argv[1]), Path(sys.argv[2])
    data = source.read_bytes()
    size, digest = PINS["source"]["files"]["bend2/bend.ts"]
    if len(data) != size or hashlib.sha256(data).hexdigest() != digest:
        raise SystemExit(f"{source} is not the pinned bend.ts")
    text = data.decode()
    for old, new in EDITS:
        if text.count(old) != 1:
            raise SystemExit(f"anchor does not occur exactly once:\n{old}")
        text = text.replace(old, new)
    output.write_text(text)


if __name__ == "__main__":
    main()
