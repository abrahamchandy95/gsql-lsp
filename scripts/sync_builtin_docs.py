#!/usr/bin/env python3
"""Extracts the reference of the built-in functions and methods from the
TigerGraph language reference into crates/gsql-lsp/data/builtin-docs.json.

    scripts/sync_builtin_docs.py [--version 4.3]

Reads the pages that scripts/docs_examples.py cached under
target/docs-examples/<version> (run it first to download them). Each function
or method has the same sections on its page: Syntax, Description, Return type,
Parameters (a table) and Example. The server adds them to the reference file of
the built-ins and to hover.
"""

import argparse
import html
import json
import re
import sys
from html.parser import HTMLParser
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/gsql-lsp/data/builtin-docs.json"

# page name (without the version prefix) -> the group of its entries
PAGES = {
    "querying_func_aggregation_functions": "functions",
    "querying_func_context_functions": "functions",
    "querying_func_datetime_functions": "functions",
    "querying_func_list_functions": "functions",
    "querying_func_mathematical_functions": "functions",
    "querying_func_miscellaneous_functions": "functions",
    "querying_func_string_functions": "functions",
    "querying_func_type_conversion_functions": "functions",
    "querying_func_vector_functions": "functions",
    "querying_func_vertex_methods": "vertex",
    "querying_func_edge_methods": "edge",
    "querying_func_json_object_methods": "jsonobject",
    "querying_func_jsonarray_methods": "jsonarray",
}
LABELS = {"syntax": "syntax", "description": "description", "return type": "returns", "parameters": "parameters",
          "example": "examples", "examples": "examples"}


class Page(HTMLParser):
    """Collects the sections of a page: (heading, level, discrete, content items)."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.blocks = []  # (kind, level, text)  kind: h / p / li / pre / row
        self.stack = []
        self.text = []
        self.row = None
        self.cell = None
        self.skip = 0
        self.discrete = False
        self.list_depth = 0

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag in ("script", "style", "nav"):
            self.skip += 1
        if self.skip:
            return
        if tag in ("h1", "h2", "h3", "h4"):
            self.flush()
            self.stack.append(("h", int(tag[1]), "discrete" in (a.get("class") or "")))
        elif tag == "dt":
            self.flush()
            self.stack.append(("dt", 0, False))
        elif tag == "p" and self.cell is None:
            self.flush()
            self.stack.append(("p", 0, False))
        elif tag == "li":
            self.flush()
            self.stack.append(("li", 0, False))
        elif tag == "pre":
            self.flush()
            self.stack.append(("pre", 0, False))
        elif tag == "tr":
            self.flush()
            self.row = []
        elif tag in ("td", "th") and self.row is not None:
            self.cell = []
        elif tag == "code" and not self.in_pre():
            (self.cell if self.cell is not None else self.text).append("`")
        elif tag == "br":
            self.text.append("\n")

    def in_pre(self):
        return any(s[0] == "pre" for s in self.stack)

    def handle_endtag(self, tag):
        if tag in ("script", "style", "nav"):
            self.skip = max(0, self.skip - 1)
            return
        if self.skip:
            return
        if tag in ("h1", "h2", "h3", "h4", "p", "li", "pre", "dt") and self.stack and self.stack[-1][0] == {"h1": "h", "h2": "h", "h3": "h", "h4": "h"}.get(tag, tag):
            self.flush()
        elif tag in ("td", "th") and self.cell is not None:
            self.row.append(plain_math(" ".join("".join(self.cell).split())))
            self.cell = None
        elif tag == "tr" and self.row is not None:
            self.blocks.append(("row", 0, False, self.row))
            self.row = None
        elif tag == "code" and not self.in_pre():
            (self.cell if self.cell is not None else self.text).append("`")

    def handle_data(self, data):
        if self.skip:
            return
        if self.cell is not None:
            self.cell.append(data)
        elif self.stack:
            self.text.append(data)

    def flush(self):
        if not self.stack:
            return
        kind, level, discrete = self.stack.pop()
        raw = "".join(self.text)
        self.text = []
        text = raw.strip("\n") if kind == "pre" else plain_math(" ".join(raw.split()))
        text = text.replace("` `", " ").replace("``", "")
        if text:
            self.blocks.append((kind, level, discrete, text))


# Mistakes of the documentation that would mislead: (function, parameter) -> type.
TYPE_FIXES = {("datetime_format", "str"): "STRING"}


# Facts the documentation leaves out, added as notes: function -> note.
EXTRA_NOTES = {
    "datetime_to_epoch": "The result is in seconds, not milliseconds: the example turns one minute (00:01:00) into 60.",
    "epoch_to_datetime": "The argument is in seconds, not milliseconds: 1 gives 00:00:01.",
}


def is_page_junk(text):
    """A paragraph or table cell that documents nothing: the placeholder of an
    empty section or the copyright line of the page footer."""
    return text in ("None", "None.") or text.startswith("Copyright ©")


def plain_math(text):
    r"""MathJax written out: `\(C_{j}\)` becomes `C_j`."""
    def written_out(m):
        return m.group(1).replace("{", "").replace("}", "")

    return re.sub(r"\\\((.*?)\\\)", written_out, text)


def example_text(text):
    """Text shown as code (an example or its caption): the backticks of inline
    code would be shown as they are."""
    return text.replace("`", "")


def heading_name(text):
    m = re.match(r"^\.?([A-Za-z_]\w*)\s*\(", text.replace("`", ""))
    return m.group(1) if m else None


def parse_page(path, group):
    page = Page()
    page.feed(path.read_text())
    entries, current, label, caption = [], None, None, ""
    for block in page.blocks:
        kind, level, discrete, text = block
        if kind in ("p", "li") and is_page_junk(text):
            continue
        if kind == "h":
            section = LABELS.get(text.strip().lower().rstrip(":"))
            if section:
                label = section
                caption = ""
                continue
            name = heading_name(text)
            label = None
            if name:
                current = {"name": name, "group": group, "syntax": [], "description": [], "returns": "",
                           "parameters": [], "notes": [], "examples": []}
                entries.append(current)
            else:
                current = None
            continue
        if kind == "dt" and current is not None and label == "parameters":
            names = [n.strip() for n in text.replace("`", "").split(",")]
            current["parameters"].append({"name": ", ".join(names), "description": "", "type": ""})
            continue
        if current is not None and group == "loading":
            # A loading function has a page of its own: the lead paragraph is its
            # description; a paragraph after a term describes that parameter.
            if label is None and kind == "p":
                current["description"].append(text)
                continue
            if label == "parameters" and kind == "p" and current["parameters"]:
                last = current["parameters"][-1]
                last["description"] = (last["description"] + " " + text).strip()
                continue
        if current is None or label is None:
            continue
        if label == "syntax" and kind in ("p", "pre"):
            current["syntax"].extend(line for line in text.replace("`", "").splitlines() if line.strip())
        elif label == "description" and kind in ("p", "li"):
            current["description"].append(("- " if kind == "li" else "") + text)
        elif label == "returns" and kind in ("p", "li"):
            current["returns"] = (current["returns"] + " " + text).strip()
        elif label == "parameters" and kind == "row" and len(text) >= 2 and text[0].lower() != "parameter":
            # A row of placeholders: "None | None | None".
            if all(is_page_junk(cell) or not cell.strip() for cell in text):
                continue
            if not text[0].strip():  # a note set in a box under the table
                current["notes"].append(text[1])
                continue
            name = text[0].replace("`", "")
            kind_of = text[2].replace("`", "") if len(text) > 2 else ""
            current["parameters"].append({"name": name, "description": text[1],
                                          "type": TYPE_FIXES.get((current["name"], name), kind_of)})
        elif label == "parameters" and kind in ("p", "li"):
            current["notes"].append(("- " if kind == "li" else "") + text)
        elif label == "examples" and kind == "p":
            caption = example_text(text)
        elif label == "examples" and kind == "pre":
            code = example_text(text)
            current["examples"].append((caption + "\n" if caption else "") + code)
            caption = ""
    return entries


def parse_accumulators(path):
    """The accumulator types with their description and method tables."""
    page = Page()
    page.feed(path.read_text())
    types, current, in_types, section = [], None, False, None
    for kind, level, discrete, text in page.blocks:
        if kind == "h":
            if level == 2:
                in_types = text.strip() == "Accumulator Types"
                current = None
            elif in_types and level == 3:
                names = [n.strip() for n in re.split(r"\s*/\s*", text.strip())]
                current = {"names": names, "description": [], "methods": []}
                types.append(current)
                section = None
            elif in_types and level == 4:
                section = text.strip()
            continue
        if not in_types or current is None:
            continue
        if kind == "p" and section is None and not current["methods"] and len(current["description"]) < 4:
            current["description"].append(text)
        elif kind == "row" and len(text) == 4 and text[2].strip().lower() in ("accessor", "mutator"):
            signature = text[0].replace("`", "")
            current["methods"].append({"signature": signature, "returns": text[1].replace("`", ""),
                                       "mutator": text[2].strip().lower() == "mutator", "description": text[3]})
    return types


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", default="4.3")
    args = parser.parse_args()
    cache = ROOT / "target/docs-examples" / args.version
    prefix = args.version.replace(".", "_") + "_"
    entries = []
    # The loading-job functions have a page each.
    loading = {p.stem[len(prefix):]: "loading" for p in sorted(cache.glob(f"{prefix}ddl_and_loading_functions_*.html"))}
    for page, group in {**PAGES, **loading}.items():
        path = cache / f"{prefix}{page}.html"
        if not path.exists():
            sys.exit(f"{path} is missing; run scripts/docs_examples.py --version {args.version} first")
        entries.extend(parse_page(path, group))
    for entry in entries:
        note = EXTRA_NOTES.get(entry["name"])
        if note and entry["group"] == "functions" and note not in entry["notes"]:
            entry["notes"].append(note)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    accumulators = parse_accumulators(cache / f"{prefix}querying_accumulators.html")
    data = {"version": args.version, "entries": entries, "accumulators": accumulators}
    OUT.write_text(json.dumps(data, indent=1, ensure_ascii=False) + "\n")
    with_params = sum(1 for e in entries if e["parameters"])
    print(f"{len(entries)} entries ({with_params} with parameters, "
          f"{sum(1 for e in entries if e['examples'])} with examples), {len(accumulators)} accumulator pages "
          f"with {sum(len(a['methods']) for a in accumulators)} methods -> {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
