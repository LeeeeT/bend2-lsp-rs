#!/usr/bin/env python3
"""Compare the traces of the compiler's parser and of the port, entry by entry.

usage: compare.py oracle.jsonl port.jsonl
"""

import json
import sys
from collections import Counter


def first_difference(oracle: dict, port: dict) -> str | None:
    if oracle["files"] != port["files"]:
        for a, b in zip(oracle["files"], port["files"], strict=False):
            if a != b:
                return f"file {a} != {b}"
        return f"files: {len(oracle['files'])} != {len(port['files'])}"
    for a, b in zip(oracle["tlds"], port["tlds"], strict=False):
        if a != b:
            return f"tld {a['file']}:{a['name']} ({a['kind']})"
    if len(oracle["tlds"]) != len(port["tlds"]):
        return f"tlds: {len(oracle['tlds'])} != {len(port['tlds'])}"
    if oracle["error"] != port["error"]:
        return f"error {oracle['error']} != {port['error']}"
    return None


def main() -> None:
    counts: Counter[str] = Counter()
    with open(sys.argv[1]) as left, open(sys.argv[2]) as right:
        for x, y in zip(left, right, strict=True):
            compare(json.loads(x), json.loads(y), counts)
    print(dict(counts))


def compare(a: dict, b: dict, counts: Counter[str]) -> None:
    if a["entry"] != b["entry"]:
        raise SystemExit(f"entry order differs: {a['entry']} != {b['entry']}")
    difference = first_difference(a, b)
    counts["same" if difference is None else "different"] += 1
    if difference is not None:
        print(f"{a['entry']}: {difference}")


if __name__ == "__main__":
    main()
