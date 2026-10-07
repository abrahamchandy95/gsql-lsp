#!/usr/bin/env python3
"""Writes tree-sitter highlight tests with computed assertion columns.

Each spec line is either source code or an expectation of the form
`  ^token capture` (assert the first occurrence of `token` on the most recent
source line, at or after the previous assertion) which becomes a
`// ^ capture` comment line with the caret under the token.
"""

import sys
from pathlib import Path


def render(spec: str) -> str:
    out = []
    source = None
    cursor = 0
    for line in spec.splitlines():
        if line.lstrip().startswith("^"):
            token, capture = line.strip()[1:].split()
            column = source.index(token, cursor)
            cursor = column + 1
            if column < 2:
                out.append("// <- " + capture)
            else:
                out.append("//" + " " * (column - 2) + "^ " + capture)
        else:
            out.append(line)
            source = line
            cursor = 0
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    for spec_path in sys.argv[1:]:
        spec = Path(spec_path)
        # test/highlight-specs/x.spec -> test/highlight/x.gsql
        target = spec.parent.parent / "highlight" / spec.with_suffix(".gsql").name
        target.write_text(render(spec.read_text()))
        print("wrote", target)
