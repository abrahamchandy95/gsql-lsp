#!/usr/bin/env python3
"""Sets the version of every package in the repository.

    scripts/set_version.py 0.2.0

Updates the Cargo workspace, the tree-sitter grammar (tree-sitter.json,
Cargo.toml, package.json, pyproject.toml), the grammar dependency of
crates/gsql-lsp, the VS Code and Zed extensions, and
the workspace entries of Cargo.lock. Then tag the release: `git tag v0.2.0`.
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def replace_first(path, pattern, version):
    text = path.read_text()
    new, count = re.subn(pattern, lambda m: m.group(1) + version + m.group(3), text, count=1, flags=re.M)
    if count != 1:
        sys.exit(f"{path.relative_to(ROOT)}: version field not found")
    path.write_text(new)


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?", sys.argv[1]):
        sys.exit(__doc__)
    version = sys.argv[1]
    # The first `version` field of each file is the package version; edit it
    # in place so the files keep their formatting.
    toml_version = r'^(version\s*=\s*")([^"]+)(")'
    json_version = r'^(\s*"version":\s*")([^"]+)(")'
    replace_first(ROOT / "Cargo.toml", toml_version, version)
    replace_first(ROOT / "tree-sitter-gsql" / "Cargo.toml", toml_version, version)
    replace_first(ROOT / "tree-sitter-gsql" / "pyproject.toml", toml_version, version)
    replace_first(ROOT / "editors" / "zed" / "extension.toml", toml_version, version)
    replace_first(ROOT / "editors" / "zed" / "Cargo.toml", toml_version, version)
    replace_first(ROOT / "tree-sitter-gsql" / "tree-sitter.json", json_version, version)
    replace_first(ROOT / "tree-sitter-gsql" / "package.json", json_version, version)
    replace_first(ROOT / "editors" / "vscode" / "package.json", json_version, version)
    # The grammar dependency of the server also names its version (crates.io).
    dep = r'(tree-sitter-gsql = \{ path = "[^"]+", version = ")([^"]+)(")'
    replace_first(ROOT / "crates" / "gsql-lsp" / "Cargo.toml", dep, version)
    # Refresh the workspace members' entries in Cargo.lock (no network needed).
    subprocess.run(["cargo", "update", "--workspace", "--offline"], cwd=ROOT, check=True)
    print(f"version set to {version}; commit, then `git tag v{version}` and push the tag")


if __name__ == "__main__":
    main()
