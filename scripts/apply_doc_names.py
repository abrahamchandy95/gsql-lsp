#!/usr/bin/env python3
"""Renames the parameters of the built-in functions and methods in
crates/gsql-lsp/src/builtins.rs to the names of the TigerGraph documentation
(crates/gsql-lsp/data/builtin-docs.json, see sync_builtin_docs.py).

An entry is changed only when the documented syntax has the same number of
parameters, with the same ones optional, so checks and signature help behave as
before; the others are listed for a manual look.

    scripts/apply_doc_names.py [--write]
"""

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILTINS = ROOT / "crates/gsql-lsp/src/builtins.rs"
DOCS = ROOT / "crates/gsql-lsp/data/builtin-docs.json"

TABLES = {  # table name in builtins.rs -> group in the docs
    "FUNCTIONS": "functions",
    "VERTEX_METHODS": "vertex",
    "EDGE_METHODS": "edge",
    "JSON_OBJECT_METHODS": "jsonobject",
    "JSON_ARRAY_METHODS": "jsonarray",
}
STRINGS = r'\[((?:\s*"(?:[^"\\]|\\.)*"\s*,?)*)\]'
STRING = r'"(?:[^"\\]|\\.)*"'
ENTRY = re.compile(
    r'((?:\bf!\(\s*\w+,\s*|\bm!\(\s*|\bmutator!\(\s*)"(\w+)",\s*)' + STRINGS + r'(\s*,\s*' + STRING + r'\s*,\s*)(' + STRING + r')'
)


def documented_params(syntax):
    """['date', '[str]'] for `datetime_format(date[, str])`."""
    start, end = syntax.find("("), syntax.rfind(")")
    if start < 0 or end < start:
        return None
    tokens, token, optional, brackets, parens = [], "", False, 0, 0
    for ch in syntax[start + 1:end]:
        if ch == "(":
            parens += 1
        elif ch == ")":
            parens -= 1
        if ch == "," and parens == 0:
            tokens.append((token, optional))
            token, optional = "", False
            continue
        if not token.strip() and not ch.isspace():
            optional = brackets > 0
        if ch == "[":
            brackets += 1
        elif ch == "]":
            brackets -= 1
        token += ch
    tokens.append((token, optional))
    params = []
    for text, optional in tokens:
        name = " ".join(text.split()).replace('"', "")  # `f("roleName")` names a string
        if not name.strip("[] "):
            continue
        if re.search(r"\]\s*\S", name) or " " in name.strip("[] "):
            params.append(name)  # `[DISTINCT] setExp`, `INTERVAL int_value time_unit`
        else:
            name = name.strip("[] ")
            params.append(f"[{name}]" if optional or text.strip().startswith("[") else name)
    return params


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    docs = {(e["group"], e["name"].lower()): e for e in json.loads(DOCS.read_text())["entries"]}
    source = BUILTINS.read_text()
    tables = [(m.start(), m.group(1)) for m in re.finditer(r"pub static (\w+): &\[\w+\] = &\[", source)]
    bounds = tables + [(len(source), None)]
    stats = {"changed": 0, "same": 0, "skipped": 0, "undocumented": 0}
    result = source[: bounds[0][0]]
    for (start, name), (end, _) in zip(bounds, bounds[1:]):
        chunk = source[start:end]
        group = TABLES.get(name)

        def repl(m):
            doc = docs.get((group, m.group(2).lower())) or (docs.get(("loading", m.group(2).lower())) if group == "functions" else None)
            ours = [json.loads(s) for s in re.findall(r'"(?:[^"\\]|\\.)*"', m.group(3))]
            if not doc or not doc["syntax"]:
                stats["undocumented"] += 1
                return m.group(0)
            theirs = documented_params(doc["syntax"][0])
            if theirs is None or len(theirs) != len(ours) or [o.startswith("[") for o in ours] != [t.startswith("[") for t in theirs]:
                stats["skipped"] += 1
                print(f"  kept {group}:{m.group(2)}: table {ours}, docs {theirs} from `{doc['syntax'][0]}`")
                return m.group(0)
            if theirs == ours:
                stats["same"] += 1
                return m.group(0)
            stats["changed"] += 1
            # The description names the parameters in backticks: keep it in step.
            description = m.group(5)
            for old, new in zip(ours, theirs):
                old, new = old.strip("[]"), new.strip("[]")
                if old != new and " " not in new:
                    description = description.replace(f"`{old}`", f"`{new}`")
            return f'{m.group(1)}[{", ".join(json.dumps(t) for t in theirs)}]{m.group(4)}{description}'

        result += ENTRY.sub(repl, chunk) if group else chunk
    print(stats)
    if args.write:
        BUILTINS.write_text(result)


if __name__ == "__main__":
    main()
