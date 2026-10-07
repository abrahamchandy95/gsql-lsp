#!/usr/bin/env python3
"""Validates packaging/upstream/mason/gsql-lsp/package.yaml.

Checks against the compiled registry that mason.nvim downloaded locally
(default: ~/.local/share/nvim/mason/registries/github/mason-org/mason-registry/registry.json,
override with MASON_REGISTRY_JSON) and against .github/workflows/release.yml.
The YAML is parsed with ruby (bundled with macOS); python3 has no yaml module.

Usage: python3 packaging/upstream/validate_mason.py
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PKG = ROOT / "packaging/upstream/mason/gsql-lsp/package.yaml"
RELEASE = ROOT / ".github/workflows/release.yml"
REGISTRY = Path(
    os.environ.get(
        "MASON_REGISTRY_JSON",
        Path.home() / ".local/share/nvim/mason/registries/github/mason-org/mason-registry/registry.json",
    )
)

if not REGISTRY.exists():
    raise SystemExit(f"Mason registry not found at {REGISTRY}: install mason.nvim or set MASON_REGISTRY_JSON")

errors = []


def check(cond, msg):
    if not cond:
        errors.append(msg)


def load_yaml(path):
    out = subprocess.run(
        ["ruby", "-ryaml", "-rjson", "-e", "puts JSON.generate(YAML.safe_load(File.read(ARGV[0])))", str(path)],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        sys.exit("yaml parse failed: " + out.stderr)
    return json.loads(out.stdout)


pkg = load_yaml(PKG)
registry = json.loads(REGISTRY.read_text())
print(f"registry: {len(registry)} packages; draft keys: {sorted(pkg)}")

# Top-level keys: every key must be used by real packages; required ones must
# be present in every real package.
all_keys, common = set(), None
for p in registry:
    all_keys |= set(p)
    common = set(p) if common is None else common & set(p)
check(set(pkg) <= all_keys, f"unknown top-level keys: {set(pkg) - all_keys}")
check(common <= set(pkg), f"missing keys present in every registry package: {common - set(pkg)}")

# A comparable package: GitHub release, per-platform archives with a top-level dir.
ref = next(p for p in registry if p["name"] == "aiken")
check(set(pkg) <= set(ref) | {"schemas", "neovim"}, "key set differs from aiken beyond schemas/neovim")
check(set(pkg["source"]) == set(ref["source"]), "source key set differs from aiken")
for a in pkg["source"]["asset"]:
    check(set(a) == {"target", "file", "bin"}, f"asset keys {set(a)}")

# Values.
check(re.fullmatch(r"[a-z0-9-]+", pkg["name"]) is not None, "name format")
check(pkg["categories"] == ["LSP"], "categories")
check(pkg["languages"] == ["GSQL"], "languages")
check(re.fullmatch(r"pkg:github/[^/@]+/[^/@]+@v?\d+\.\d+\.\d+", pkg["source"]["id"]) is not None, "purl")
check(pkg["bin"] == {"gsql-lsp": "{{source.asset.bin}}"}, "bin mapping")
check(pkg["neovim"] == {"lspconfig": "gsql_lsp"}, "neovim.lspconfig")
check(pkg["schemas"]["lsp"].startswith("vscode:https://"), "schemas.lsp")
check(all(p["licenses"] for p in [pkg]), "licenses")

known_targets = set()
for p in registry:
    a = p["source"].get("asset")
    if isinstance(a, list):
        for x in a:
            t = x["target"]
            known_targets |= {t} if isinstance(t, str) else set(t)
mason_targets = []
for a in pkg["source"]["asset"]:
    t = a["target"]
    mason_targets.append(t)
    check(t in known_targets, f"target {t} not used by any registry package")

# Every (rust target -> archive) must match release.yml's matrix and packaging.
release = RELEASE.read_text()
matrix = re.findall(r"\{ target: (\S+), os: \S+, archive: (\S+) \}", release)
check(len(matrix) == 5, f"release.yml matrix parsed {len(matrix)} entries")
by_file = {a["file"]: a for a in pkg["source"]["asset"]}
expected = {}
for target, archive in matrix:
    exe = ".exe" if archive == "zip" else ""
    expected[f"gsql-lsp-{target}.{archive}"] = f"gsql-lsp-{target}/gsql-lsp{exe}"
for f, b in expected.items():
    check(f in by_file, f"release asset {f} not referenced")
    if f in by_file:
        check(by_file[f]["bin"] == b, f"bin for {f}: {by_file[f]['bin']} != {b}")
for f in by_file:
    check(f in expected, f"{f} is not produced by release.yml")
check('name="gsql-lsp-${{ matrix.target }}"' in release, "release.yml archive naming changed")
check(len(set(mason_targets)) == len(mason_targets), "duplicate mason targets")

if errors:
    print("FAIL")
    for e in errors:
        print(" -", e)
    sys.exit(1)
print("OK: key set matches registry packages; assets match release.yml")
