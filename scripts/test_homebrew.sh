#!/bin/sh
# Tests scripts/homebrew_formula.py on a fake checksums file.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/gsql-brew-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT
cd "$work"

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
fail() { echo "FAIL $1" >&2; exit 1; }

for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
    echo "fake $t" >"gsql-lsp-$t.tar.gz"
done
echo fake >gsql-lsp-x86_64-pc-windows-msvc.zip
sha gsql-lsp-* >SHA256SUMS

python3 "$root/scripts/homebrew_formula.py" --version v0.3.1 --checksums SHA256SUMS --repo https://example.test/o/r/ -o gsql-lsp.rb
grep -q 'download/v0.3.1/gsql-lsp-aarch64-apple-darwin.tar.gz' gsql-lsp.rb || fail "version not filled"
grep -q 'url "https://example.test/o/r/releases/download/v0.3.1/gsql-lsp-aarch64-apple-darwin.tar.gz"' gsql-lsp.rb || fail "url not filled"
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
    want="$(sha "gsql-lsp-$t.tar.gz" | awk '{ print $1 }')"
    grep -A1 "gsql-lsp-$t.tar.gz" gsql-lsp.rb | grep -q "sha256 \"$want\"" || fail "sha256 of $t"
done
if grep -q '@' gsql-lsp.rb; then fail "unfilled marker"; fi
echo "ok   formula filled from a checksums file"

if command -v ruby >/dev/null 2>&1; then
    ruby -c gsql-lsp.rb >/dev/null || fail "ruby -c on the filled formula"
    ruby -c "$root/packaging/homebrew/gsql-lsp.rb" >/dev/null || fail "ruby -c on the template"
    echo "ok   ruby -c"
else
    echo "skip ruby -c (no ruby)"
fi

# The default repository comes from Cargo.toml.
python3 "$root/scripts/homebrew_formula.py" --version 0.3.1 --checksums SHA256SUMS >default.rb
repo="$(sed -n 's/^repository = "\(.*\)"/\1/p' "$root/Cargo.toml")"
grep -q "homepage \"$repo\"" default.rb || fail "default repo from Cargo.toml"
echo "ok   repository URL read from Cargo.toml"

# A checksums file without one of the archives is an error.
grep -v aarch64-apple-darwin SHA256SUMS >partial
if python3 "$root/scripts/homebrew_formula.py" --version 0.3.1 --checksums partial >/dev/null 2>&1; then
    fail "missing archive accepted"
fi
echo "ok   missing archive rejected"

if python3 "$root/scripts/homebrew_formula.py" --version latest --checksums SHA256SUMS >/dev/null 2>&1; then
    fail "bad version accepted"
fi
echo "ok   bad version rejected"
echo "all homebrew checks passed"
