#!/usr/bin/env python3
"""Measures how well syntax errors are reported and repaired, on seeded mutations of valid code.

Developer measurement, run by hand against a built server:
    GSQL_LSP_BIN=target/release/gsql-lsp python3 scripts/dev/check_repairs.py [--seed 7] [--out rows.json]

Corpus: the .gsql files of G2N_DIR (default ~/Downloads/G2N-main) and the GSQL
examples of the cached language reference (--docs-dir or DOCS_EXAMPLES, default target/docs-examples, see
scripts/docs_examples.py) that parse without syntax errors, as written or inside
a query. Each file is mutated at seeded random token positions:

    delete      one token removed
    insert      a stray token (taken from the file) put in front of a token
    replace     one token replaced by another token of the file
    dropcloser  a closing `)`, `}`, `]`, `END` or `;` removed
    swap        two adjacent tokens exchanged

Per class it prints: cases (mutations that change the text), how many show a
syntax error, how many of those report it within one line of the mutation, the
mean number of syntax diagnostics, the share with a quick fix, the share where
some fix leaves no syntax error, where the first fix does, where some fix
restores the original text exactly or up to white space, and the p95 time (ms)
from the edit to the diagnostics.
"""
import argparse, collections, glob, hashlib, html, json, os, random, re, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
os.environ.setdefault("GSQL_LSP_BIN", "gsql-lsp")
import lspc

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOKEN = re.compile(
    r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\\n])*"|@@?[A-Za-z_]\w*|[A-Za-z_]\w*|\d+\.?\d*'
    r"|==|!=|<=|>=|\+=|-=|\*=|/=|->|<-|::|&&|\|\||<<|>>|[^\s]",
    re.S,
)
CLOSERS = (")", "}", "]", "END", ";")
CLASSES = ("delete", "insert", "replace", "dropcloser", "swap")


def tokens(text):
    return [(m.start(), m.end()) for m in TOKEN.finditer(text) if text[m.start() : m.start() + 2] not in ("//", "/*")]


def mutations(text, rng, per):
    """(class, mutated text, offset, token) for `per` seeded positions."""
    toks = tokens(text)
    out = []
    if len(toks) < 10:
        return out
    vocab = sorted({text[a:b] for a, b in toks if len(text[a:b]) < 12})
    closers = [i for i, (a, b) in enumerate(toks) if text[a:b].upper() in CLOSERS]
    for _ in range(per):
        i = rng.randrange(len(toks))
        a, b = toks[i]
        out.append(("delete", text[:a] + text[b:], a, text[a:b]))
        v = rng.choice(vocab)
        out.append(("insert", text[:a] + v + " " + text[a:], a, v))
        v = rng.choice(vocab)
        if v != text[a:b]:
            out.append(("replace", text[:a] + v + text[b:], a, text[a:b] + "->" + v))
        if i + 1 < len(toks):
            a2, b2 = toks[i + 1]
            out.append(("swap", text[:a] + text[a2:b2] + text[b:a2] + text[a:b] + text[b2:], a, text[a:b] + "<>" + text[a2:b2]))
        if closers:
            a, b = toks[rng.choice(closers)]
            out.append(("dropcloser", text[:a] + text[b:], a, text[a:b]))
    return [m for m in out if m[1] != text]


def apply_edits(text, edits):
    starts = [0] + [m.end() for m in re.finditer("\n", text)]

    def offset(p):
        line_start = starts[p["line"]] if p["line"] < len(starts) else len(text)
        n = i = 0
        rest = text[line_start:]
        while i < len(rest) and n < p["character"] and rest[i] != "\n":
            n += 2 if ord(rest[i]) > 0xFFFF else 1
            i += 1
        return line_start + i

    spans = sorted(((offset(e["range"]["start"]), offset(e["range"]["end"]), e["newText"]) for e in edits), reverse=True)
    for a, b, new in spans:
        text = text[:a] + new + text[b:]
    return text


class Session:
    def __init__(self):
        self.client = lspc.Client(root=None)
        self.version = 0
        self.uri = "file:///tmp/check-repairs/doc.gsql"
        self.client.notify(
            "textDocument/didOpen",
            {"textDocument": {"uri": self.uri, "languageId": "gsql", "version": 1, "text": ""}},
        )
        self.version = 1
        self.client.diags(self.uri)

    def diagnostics(self, text):
        self.version += 1
        v = self.version
        start = time.time()
        self.client.notify(
            "textDocument/didChange",
            {"textDocument": {"uri": self.uri, "version": v}, "contentChanges": [{"text": text}]},
        )
        n = self.client.wait_notification(
            "textDocument/publishDiagnostics",
            timeout=30,
            pred=lambda n: n["params"]["uri"] == self.uri and n["params"].get("version") == v,
        )
        return (n["params"]["diagnostics"] if n else None), time.time() - start

    def fixes(self, diags):
        """Edits of the quick fixes of the syntax errors, in diagnostic order."""
        found = []
        for d in diags:
            actions = self.client.result(
                "textDocument/codeAction",
                {"textDocument": {"uri": self.uri}, "range": d["range"], "context": {"diagnostics": [d]}},
            )
            for a in actions or []:
                if not a.get("kind", "").startswith("quickfix") or not a.get("edit"):
                    continue
                edit = a["edit"]
                edits = (edit.get("changes") or {}).get(self.uri) or []
                if not edits and edit.get("documentChanges"):
                    edits = edit["documentChanges"][0]["edits"]
                if edits:
                    found.append(edits)
        return found

    def close(self):
        self.client.kill()


def syntax(diags):
    return [d for d in diags if d.get("code") == "syntax-error"]


def corpus(g2n, docs_dir, rng):
    files = []
    for f in sorted(glob.glob(os.path.join(g2n, "**", "*.gsql"), recursive=True)):
        files.append((os.path.relpath(f, g2n), open(f).read()))
    docs = []
    sys.path.insert(0, os.path.join(ROOT, "scripts"))
    import docs_examples

    seen = set()
    base = docs_dir
    for version in ("4.3", "4.2", "4.1", "3.11", "tigergraph-server-4.3"):
        directory = os.path.join(base, version)
        for name in sorted(os.listdir(directory)) if os.path.isdir(directory) else []:
            if not name.endswith(".html"):
                continue
            page = open(os.path.join(directory, name), errors="replace").read()
            for index, (language, code) in enumerate(docs_examples.code_blocks(page)):
                digest = hashlib.md5(code.encode()).hexdigest()
                if language != "gsql" or docs_examples.not_source(code) or digest in seen or not 80 <= len(code) <= 6000:
                    continue
                seen.add(digest)
                docs.append((f"docs/{version}/{name[:-5]}#{index}", code.rstrip() + "\n"))
    rng.shuffle(docs)
    return files, docs


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--g2n", default=os.environ.get("G2N_DIR", os.path.expanduser("~/Downloads/G2N-main")))
    ap.add_argument("--docs-dir", default=os.environ.get("DOCS_EXAMPLES", os.path.join(ROOT, "target", "docs-examples")))
    ap.add_argument("--per-file", type=int, default=10, help="mutation positions per G2N file")
    ap.add_argument("--docs", type=int, default=150, help="docs examples to use")
    ap.add_argument("--docs-per-file", type=int, default=4, help="mutation positions per docs example")
    ap.add_argument("--out", help="write the rows as JSON")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    g2n_files, docs = corpus(args.g2n, args.docs_dir, rng)
    session = Session()
    # Docs examples that are valid (as written, or inside a query).
    valid_docs = []
    for name, code in docs:
        if len(valid_docs) >= args.docs:
            break
        for wrapped in (code, "CREATE QUERY wrapper() {\n" + code + "\n}\n"):
            d, _ = session.diagnostics(wrapped)
            if d is not None and not syntax(d):
                valid_docs.append((name, wrapped))
                break
    work = [(n, t, args.per_file) for n, t in g2n_files] + [(n, t, args.docs_per_file) for n, t in valid_docs]
    print(f"{len(g2n_files)} G2N files, {len(valid_docs)} docs examples", file=sys.stderr)
    rows = []
    for name, text, per in work:
        for kind, mutated, at, token in mutations(text, rng, per):
            d, dt = session.diagnostics(mutated)
            if d is None:
                rows.append(dict(file=name, kind=kind, timeout=True))
                continue
            syn = syntax(d)
            row = dict(file=name, kind=kind, at=at, dt=dt, nsyn=len(syn), line=mutated.count("\n", 0, at) + 1)
            row["token"] = token
            if syn:
                line = mutated.count("\n", 0, at)
                row["dist"] = min(abs(x["range"]["start"]["line"] - line) for x in syn)
                fixes = session.fixes(syn)[:8]
                row["nfix"] = len(fixes)
                row["fixed"] = row["exact"] = row["ws"] = row["first_fixed"] = row["first_ws"] = False
                for i, edits in enumerate(fixes):
                    fixed_text = apply_edits(mutated, edits)
                    d3, _ = session.diagnostics(fixed_text)
                    zero = d3 is not None and not syntax(d3)
                    row["fixed"] |= zero
                    if i == 0:
                        row["first_fixed"] = zero
                        row["first_ws"] = re.sub(r"\s+", "", fixed_text) == re.sub(r"\s+", "", text)
                    row["exact"] |= fixed_text == text
                    row["ws"] |= re.sub(r"\s+", "", fixed_text) == re.sub(r"\s+", "", text)
                row["msgs"] = [x["message"][:80] for x in syn][:3]
            rows.append(row)
    session.close()
    if args.out:
        json.dump(rows, open(args.out, "w"))
    report(rows)


def report(rows):
    print(f"{'class':11s} {'cases':>5s} {'seen':>5s} {'near%':>6s} {'diags':>6s} {'fix%':>5s} {'zero%':>6s} {'1st%':>5s} {'exact%':>6s} {'ws%':>5s} {'p95ms':>6s}")
    by = collections.defaultdict(list)
    for r in rows:
        by[r["kind"]].append(r)
    total = []
    for kind in CLASSES:
        rs = by.get(kind, [])
        seen = [r for r in rs if r.get("nsyn")]
        total += rs
        if not seen:
            print(f"{kind:11s} {len(rs):5d} {0:5d}")
            continue
        pct = lambda n: 100.0 * n / len(seen)
        times = sorted(r["dt"] for r in rs if "dt" in r)
        print(
            f"{kind:11s} {len(rs):5d} {len(seen):5d} {pct(sum(r['dist'] <= 1 for r in seen)):6.1f} "
            f"{sum(r['nsyn'] for r in seen) / len(seen):6.2f} {pct(sum(bool(r['nfix']) for r in seen)):5.1f} "
            f"{pct(sum(r['fixed'] for r in seen)):6.1f} {pct(sum(r['first_fixed'] for r in seen)):5.1f} "
            f"{pct(sum(r['exact'] for r in seen)):6.1f} {pct(sum(r['ws'] for r in seen)):5.1f} "
            f"{times[int(len(times) * 0.95)] * 1000:6.0f}"
        )
    print(f"timeouts: {sum(1 for r in rows if r.get('timeout'))}, cases: {len(total)}")


if __name__ == "__main__":
    main()
