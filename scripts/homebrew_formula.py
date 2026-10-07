#!/usr/bin/env python3
"""Fills packaging/homebrew/gsql-lsp.rb from a release's checksums file.

    scripts/homebrew_formula.py --version 0.2.0 --checksums SHA256SUMS [-o gsql-lsp.rb]

The checksums file is the SHA256SUMS of the release (`sha256sum` format). The
repository URL defaults to the `repository` field of the workspace Cargo.toml,
the one place it is configured for the Rust packages; --repo overrides it.
Without --output the formula is printed.
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
]


def default_repo():
    match = re.search(r'^repository\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)
    if not match:
        sys.exit("repository not found in Cargo.toml; pass --repo")
    return match.group(1)


def read_checksums(path):
    sums = {}
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        parts = line.split(None, 1)
        if len(parts) != 2 or not re.fullmatch(r"[0-9a-f]{64}", parts[0]):
            sys.exit(f"{path}: not a sha256sum line: {line!r}")
        sums[parts[1].strip().lstrip("*")] = parts[0]
    return sums


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", required=True, help="release version, with or without a leading v")
    parser.add_argument("--checksums", required=True, help="SHA256SUMS file of the release")
    parser.add_argument("--repo", help="repository URL (default: Cargo.toml)")
    parser.add_argument("--template", default=str(ROOT / "packaging" / "homebrew" / "gsql-lsp.rb"))
    parser.add_argument("-o", "--output", help="write the formula here instead of stdout")
    args = parser.parse_args()

    version = args.version.removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?", version):
        sys.exit(f"bad version {args.version!r}")
    sums = read_checksums(args.checksums)
    text = Path(args.template).read_text()
    text = text.replace("@VERSION@", version).replace("@REPO_URL@", (args.repo or default_repo()).rstrip("/"))
    for target in TARGETS:
        archive = f"gsql-lsp-{target}.tar.gz"
        if archive not in sums:
            sys.exit(f"{args.checksums}: no checksum for {archive}")
        text = text.replace(f"@SHA256_{target}@", sums[archive])
    left = re.findall(r"@[A-Za-z0-9_-]+@", text)
    if left:
        sys.exit(f"unfilled markers: {', '.join(sorted(set(left)))}")
    if args.output:
        Path(args.output).write_text(text)
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
