// Load each entry file with the real Bend loader and print what its parser built.
//
// usage: bun oracle.ts [--dump] < entries
//
// Each line of stdin is an entry path. Each line of stdout is one JSON object:
// the files that the entry loaded, a digest of each traced declaration (or the
// whole trace with --dump), and the error that stopped the load, if any.

import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as Bend from "./bend.ts";

type Any = any;

function quant(q: Bend.Quant): string {
  return q.$ === "None" ? "N" : q.$ === "Lone" ? "L" : "M";
}

function span(file: unknown, s: Bend.Span | undefined): unknown {
  if (s === undefined) {
    return null;
  }
  return s.file === file ? [s.beg, s.end] : ["@", s.file.ns, s.beg, s.end];
}

function term(file: unknown, t: Any): unknown {
  const sp = span(file, t.s);
  const go = (x: Any) => term(file, x);
  switch (t.$) {
    case "Var": return ["Var", t.k, t.i, sp, t.v === undefined ? null : go(t.v)];
    case "Ref": return ["Ref", t.k, sp, t.b === true];
    case "Sub": return ["Sub", t.i, patt(file, t.v), go(t.f), sp];
    case "Let": return ["Let", t.k, t.i, t.q.map(quant), t.v.map(go), go(t.f), sp];
    case "Typ": return ["Typ", go(t.g), sp];
    case "Qnt": return ["Qnt", sp];
    case "Qua": return ["Qua", quant(t.q), sp];
    case "Min": return ["Min", go(t.a), go(t.b), sp];
    case "All": return ["All", quant(t.q), t.k, t.i, go(t.A), go(t.B), sp];
    case "Lam": return ["Lam", t.k, t.i, go(t.f), sp, t.q === undefined ? null : quant(t.q)];
    case "App": return ["App", go(t.f), go(t.x), sp];
    case "ADT": return ["ADT", t.k, t.x.map(go), t.r, sp];
    case "Ctr": return ["Ctr", t.k, t.x.map(go), sp];
    case "Lit": return ["Lit", t.k, t.v, sp];
    case "Mat": return ["Mat", t.k, go(t.h), go(t.m), sp];
    case "Efq": return ["Efq", sp];
    case "Eql": return ["Eql", go(t.a), go(t.b), go(t.T), sp];
    case "Rfl": return ["Rfl", sp];
    case "Rwt": return ["Rwt", go(t.e), go(t.p), go(t.f), sp];
    case "Hol": return ["Hol", t.k, sp];
    case "Ann": return ["Ann", go(t.x), go(t.T), sp];
  }
  throw new Error("unknown term " + t.$);
}

function patt(file: unknown, q: Any): unknown {
  return q.$ === "PVar"
    ? ["PVar", q.k, q.i, quant(q.q), span(file, q.s)]
    : ["PCtr", q.k, q.x.map((x: Any) => patt(file, x)), span(file, q.s)];
}

function body(file: unknown, b: Any): unknown {
  switch (b.$) {
    case "Match":
      return ["Match", b.e.map((e: Any) => term(file, e)),
        b.r.map((row: Any) => [row.p.map((q: Any) => patt(file, q)), body(file, row.f)]), span(file, b.s)];
    case "Local":
      return ["Local", b.k.map((q: Any) => patt(file, q)), quant(b.q), b.v.map((v: Any) => term(file, v)), body(file, b.f)];
    default:
      return ["Term", term(file, b)];
  }
}

function tele(file: unknown, cells: Any[] | undefined): unknown {
  return cells === undefined ? null
    : cells.map(([q, k, i, T, s]: Any) => [quant(q), k, i, term(file, T), span(file, s)]);
}

function tld(t: Any): unknown {
  const f = t.p;
  const opt = (x: Any) => x === undefined ? null : term(f, x);
  return {
    kind: t.kind, file: t.file, name: t.name ?? null,
    law: t.law ?? null, unsafe: t.unsafe ?? null, imports: t.imports ?? null,
    tele: tele(f, t.tele), ret: opt(t.ret),
    params: tele(f, t.params), K: opt(t.K),
    ctrs: t.ctrs === undefined ? null : t.ctrs.map(([c, fs]: Any) => [c, tele(f, fs)]),
    T: opt(t.T),
    bodies: t.bodies,
  };
}

function error(e: Any): unknown {
  if (e?.$ !== "Err") {
    return { js: String(e) };
  }
  const text = (x: Any) => typeof x === "string" ? x : x === undefined ? null : "<term>";
  const shown = e.exp === "a pattern (a binder or a constructor)";
  return {
    exp: text(e.exp), obs: shown ? "<term>" : text(e.obs),
    at: e.spn === undefined ? null : [e.spn.file.ns, e.spn.beg, e.spn.end],
  };
}

Bend.TRACE.snap = (file, b, out) => [body(file, b), term(file, out)];

const dump = process.argv.includes("--dump");
const entries = fs.readFileSync(0, "utf8").split("\n").filter((line) => line !== "");
for (const entry of entries) {
  Bend.TRACE.files.length = 0;
  Bend.TRACE.tlds.length = 0;
  let err: unknown = null;
  try {
    await Bend.book_load(Bend.book_nil(), entry, "", new Map());
  } catch (e) {
    err = error(e);
  }
  const tlds = Bend.TRACE.tlds.map((t) => {
    const out = tld(t);
    const digest = createHash("sha256").update(JSON.stringify(out)).digest("hex");
    return dump ? out : { file: t.file, kind: t.kind, name: t.name ?? null, digest };
  });
  console.log(JSON.stringify({ entry, files: Bend.TRACE.files, tlds, error: err }));
}
