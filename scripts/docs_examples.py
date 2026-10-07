#!/usr/bin/env python3
"""Parses the GSQL code examples of the TigerGraph language reference.

    scripts/docs_examples.py [--version 4.3] [--site gsql-ref] [--report failing.json] [--refresh]

Crawls https://www.tigergraph.com/docs/gsql-ref/<version>/ (pages are cached
under target/docs-examples/<version>; with `--site tigergraph-server`, the
server documentation, which has more shell, admin and loading commands, is
cached under target/docs-examples/tigergraph-server-<version>), extracts the
code blocks of the GSQL
language, skips blocks that are not GSQL source (shell transcripts, JSON
output, EBNF, syntax templates, CSV data) and parses every example with the
tree-sitter grammar. Many examples are fragments, so each one is also tried
inside a query, a loading job, a schema change job and a SELECT statement.

Prints a summary and writes the examples that fail in every form to a JSON
report. Requires the tree-sitter CLI and access to www.tigergraph.com. Run it
when TigerGraph publishes a new version of the language reference.
"""

import argparse
import collections
import html
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE_NAME = "gsql-ref"
GRAMMAR = ROOT / "tree-sitter-gsql"
SITE = "https://www.tigergraph.com/docs/{site}/{version}/"

# How each example is tried: (name, prefix, suffix).
WRAPPERS = [
    ("as written", "", ""),
    ("in a query", "CREATE QUERY wrapper() {\n", "\n}\n"),
    ("in a query", "CREATE QUERY wrapper() {\n", ";\n}\n"),
    ("in a loading job", "CREATE LOADING JOB wrapper FOR GRAPH g {\n", "\n}\n"),
    ("in a loading job", "CREATE LOADING JOB wrapper FOR GRAPH g {\n", ";\n}\n"),
    ("in a schema change job", "CREATE SCHEMA_CHANGE JOB wrapper FOR GRAPH g {\n", "\n}\n"),
    ("in a schema change job", "CREATE SCHEMA_CHANGE JOB wrapper FOR GRAPH g {\n", ";\n}\n"),
    ("as SELECT clauses", "CREATE QUERY wrapper() {\nR = SELECT x\n", ";\n}\n"),
    ("as SELECT clauses", "CREATE QUERY wrapper() {\nR = SELECT x FROM X:x\n", ";\n}\n"),
    ("as a pattern", "CREATE QUERY wrapper() {\nR = SELECT x FROM ", ";\n}\n"),
]


def fetch(url, cache, refresh):
    """The URL the page was served from (after redirects) and its HTML."""
    name = re.sub(r"[^A-Za-z0-9]+", "_", url.split(f"/{SITE_NAME}/", 1)[-1]).strip("_") or "index"
    path = cache / f"{name}.html"
    if path.exists() and not refresh:
        served, _, text = path.read_text(encoding="utf-8", errors="replace").partition("\n")
        return served, text
    request = urllib.request.Request(url, headers={"User-Agent": "gsql-lsp docs check"})
    with urllib.request.urlopen(request, timeout=30) as response:
        served = response.geturl()
        text = response.read().decode("utf-8", errors="replace")
    path.write_text(served + "\n" + text, encoding="utf-8")
    time.sleep(0.2)  # be polite
    return served, text


def canonical(url, base):
    """The page URL a link points to, or None if it is outside the reference."""
    url = urllib.parse.urljoin(base, html.unescape(url)).split("#", 1)[0].split("?", 1)[0]
    url = url.replace("https://docs.tigergraph.com/", "https://www.tigergraph.com/docs/")
    if not url.startswith(SITE_PREFIX) or re.search(r"\.(png|jpe?g|gif|svg|zip|pdf|css|js)$", url, re.I):
        return None
    if "/_attachments/" in url or "/_images/" in url:
        return None
    return url.rstrip("/")


def crawl(version, cache, refresh, limit=1000):
    start = SITE_PREFIX + "intro"
    # Some pages are linked with different capitalization (openCypher-in-gsql).
    pages, queue, seen = {}, [start], {start.lower()}
    while queue and len(pages) < limit:
        url = queue.pop(0)
        try:
            served, page = fetch(url, cache, refresh)
        except OSError as error:
            print(f"skipping {url}: {error}", file=sys.stderr)
            continue
        pages[url] = page
        # Antora writes links relative to the URL the page is served from.
        for link in re.findall(r'href="([^"]+)"', page[page.find("<body") :]):
            target = canonical(link, served)
            if target and target.lower() not in seen:
                seen.add(target.lower())
                queue.append(target)
    return pages


def code_blocks(page):
    """(language, code) of each listing block in the article."""
    article = page[page.find("<article") : page.find("</article>")]
    blocks = []
    for attributes, body in re.findall(r"<pre([^>]*)>(.*?)</pre>", article, re.S):
        language = re.search(r'data-lang="([^"]+)"', body)
        language = language.group(1) if language else ""
        body = re.sub(r'<i class="conum"[^>]*></i>\s*<b>[^<]*</b>', "", body)  # callouts
        code = html.unescape(re.sub(r"<[^>]+>", "", body))
        blocks.append((language.lower(), code.replace("\\->", "->")))
    return blocks


def not_source(code):
    """Why a GSQL-labelled block is not GSQL source, or None."""
    lines = [line for line in code.splitlines() if line.strip()]
    if not lines:
        return "empty"
    first = lines[0].strip()
    if any(re.match(r"\s*GSQL\s*>", line, re.I) for line in lines):
        return "shell transcript"
    if re.match(r"(\$|gsql\b|curl\b|#!/)", first):
        return "shell command"
    if ":=" in code:
        return "EBNF"
    if first[0] in "{[" and ('"' in first or first in ("{", "[")):
        return "JSON output"
    if re.search(r"<[a-z]+[A-Z]\w*>|<\w+ \w+>", code) or re.search(r"\[\s*-\w", code):
        return "syntax template"
    if all(re.fullmatch(r"[^(){};=]*,[^(){};=]*", line) or re.fullmatch(r"[-\w.:|]+", line.strip()) for line in lines):
        return "CSV data"
    return None


def parse_errors(paths):
    """Paths whose parse tree contains ERROR or MISSING nodes."""
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as listing:
        listing.write("\n".join(str(p) for p in paths) + "\n")
    try:
        output = subprocess.run(
            ["tree-sitter", "parse", "--quiet", "--paths", listing.name],
            cwd=GRAMMAR,
            capture_output=True,
            text=True,
        ).stdout
    finally:
        os.unlink(listing.name)
    failing = {}
    for line in output.splitlines():
        if "ERROR" in line or "MISSING" in line:
            path, _, detail = line.partition("\t")
            failing[path.strip()] = detail
    return failing


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", default="4.3", help="version of the language reference (default 4.3)")
    parser.add_argument("--site", default="gsql-ref", help="documentation to crawl: gsql-ref (default) or tigergraph-server")
    parser.add_argument("--report", default="docs-examples-failing.json", help="where to write the failing examples")
    parser.add_argument("--refresh", action="store_true", help="download the pages again")
    args = parser.parse_args()

    global SITE_PREFIX, SITE_NAME
    SITE_NAME = args.site
    SITE_PREFIX = SITE.format(site=args.site, version=args.version)
    cache = ROOT / "target" / "docs-examples" / (args.version if args.site == "gsql-ref" else f"{args.site}-{args.version}")
    cache.mkdir(parents=True, exist_ok=True)
    pages = crawl(args.version, cache, args.refresh)
    print(f"{len(pages)} pages")

    examples, skipped = [], collections.Counter()
    for url, page in sorted(pages.items()):
        if url.endswith("/appendix/notations"):
            continue  # documents the notation of syntax templates
        for language, code in code_blocks(page):
            if language != "gsql":
                continue
            reason = not_source(code)
            if reason:
                skipped[reason] += 1
            else:
                examples.append((url, code))

    work = Path(tempfile.mkdtemp(prefix="gsql-docs-"))
    paths = {}
    for index, (url, code) in enumerate(examples):
        for variant, (_, prefix, suffix) in enumerate(WRAPPERS):
            path = work / f"{index:04d}_{variant}.gsql"
            path.write_text(prefix + code.rstrip() + suffix)
            paths[(index, variant)] = path
    failing = parse_errors(paths.values())

    parsed, report = collections.Counter(), []
    for index, (url, code) in enumerate(examples):
        passing = [v for v in range(len(WRAPPERS)) if str(paths[(index, v)]) not in failing]
        if passing:
            parsed[WRAPPERS[passing[0]][0]] += 1
            continue
        detail = failing[str(paths[(index, 0)])]
        position = re.search(r"\((ERROR|MISSING[^\[]*) \[(\d+), (\d+)\]", detail)
        row = int(position.group(2)) if position else 0
        lines = code.splitlines()
        report.append(
            {
                "page": url,
                "line": row + 1,
                "column": int(position.group(3)) + 1 if position else 0,
                "kind": position.group(1).strip() if position else "error",
                "text": lines[row] if row < len(lines) else "",
                "code": code,
            }
        )

    print(f"{len(examples)} GSQL examples; skipped {sum(skipped.values())} that are not source: {dict(skipped)}")
    print(f"parsed {sum(parsed.values())}: {dict(parsed)}")
    print(f"failing {len(report)} (written to {args.report})")
    Path(args.report).write_text(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
