#!/bin/sh
# Installs the gsql-lsp language server from a GitHub release.
#
#   curl -fsSL https://github.com/abrahamchandy95/gsql-lsp/releases/latest/download/install.sh | sh
#
# Environment:
#   GSQL_LSP_VERSION      release to install, e.g. 0.1.0 or v0.1.0 (default: latest)
#   GSQL_LSP_INSTALL_DIR  target directory (default: $HOME/.local/bin)
#   GSQL_RELEASE_BASE_URL directory URL that holds the assets; overrides the
#                         GitHub URLs below. http(s):// or file:// (for mirrors
#                         and offline tests).

set -eu

REPO_URL="https://github.com/abrahamchandy95/gsql-lsp"

BIN=gsql-lsp
VERSION="${GSQL_LSP_VERSION:-latest}"
INSTALL_DIR="${GSQL_LSP_INSTALL_DIR:-${HOME:-.}/.local/bin}"

say() { printf '%s\n' "$*"; }
err() { printf 'install.sh: %s\n' "$*" >&2; }
die() {
  err "$*"
  exit 1
}

case "${1:-}" in
-h | --help)
  sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
  ;;
"") ;;
*) die "unknown argument $1 (see --help)" ;;
esac

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
  Linux)
    case "$arch" in
    x86_64 | amd64) echo x86_64-unknown-linux-musl ;;
    aarch64 | arm64) echo aarch64-unknown-linux-musl ;;
    *) return 1 ;;
    esac
    ;;
  Darwin)
    # A shell running under Rosetta reports x86_64 on Apple silicon.
    if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
      arch=arm64
    fi
    case "$arch" in
    x86_64) echo x86_64-apple-darwin ;;
    arm64 | aarch64) echo aarch64-apple-darwin ;;
    *) return 1 ;;
    esac
    ;;
  *) return 1 ;;
  esac
}

if ! target="$(detect_target)"; then
  err "unsupported platform: $(uname -s) $(uname -m)"
  err "Release binaries exist for Linux and macOS on x86_64 and aarch64, and Windows x86_64"
  err "(download the .zip from $REPO_URL/releases). Or build from source:"
  err "  cargo install gsql-lsp"
  exit 1
fi

if [ -n "${GSQL_RELEASE_BASE_URL:-}" ]; then
  base="${GSQL_RELEASE_BASE_URL%/}"
elif [ "$VERSION" = latest ]; then
  base="$REPO_URL/releases/latest/download"
else
  base="$REPO_URL/releases/download/v${VERSION#v}"
fi

archive="$BIN-$target.tar.gz"

fetch() { # url destination
  case "$1" in
  file://*) cp "${1#file://}" "$2" ;;
  *)
    if command -v curl >/dev/null 2>&1; then
      curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
      wget -q "$1" -O "$2"
    else
      die "neither curl nor wget is available"
    fi
    ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die "neither sha256sum nor shasum is available"
  fi
}

command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || die "neither sha256sum nor shasum is available"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/gsql-lsp-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

say "Downloading $archive from $base"
fetch "$base/$archive" "$tmp/$archive" || die "could not download $base/$archive"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download $base/SHA256SUMS"

# `sha256sum` lines read "<hash>  <file>" ("<hash> *<file>" in binary mode).
expected="$(awk -v f="$archive" '{ n = $2; sub(/^\*/, "", n); if (n == f) { print $1; exit } }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || die "SHA256SUMS has no entry for $archive"
actual="$(sha256_of "$tmp/$archive")"
if [ "$expected" != "$actual" ]; then
  die "checksum mismatch for $archive (expected $expected, got $actual); nothing was installed"
fi

tar -xzf "$tmp/$archive" -C "$tmp" || die "could not extract $archive"
src="$tmp/$BIN-$target/$BIN"
[ -f "$src" ] || die "$archive does not contain $BIN-$target/$BIN"

mkdir -p "$INSTALL_DIR"
dest="$INSTALL_DIR/$BIN"
if [ -f "$dest" ] && cmp -s "$src" "$dest"; then
  say "$dest is already up to date"
else
  # Copy next to the destination, then rename: replaces a running server safely.
  cp "$src" "$dest.new.$$"
  chmod 755 "$dest.new.$$"
  mv -f "$dest.new.$$" "$dest"
  say "Installed $dest"
fi
"$dest" --version || true

case ":${PATH:-}:" in
*":$INSTALL_DIR:"*) ;;
*)
  say ""
  say "$INSTALL_DIR is not in your PATH. Add it, e.g. in ~/.profile or ~/.zshrc:"
  say "  export PATH=\"$INSTALL_DIR:\$PATH\""
  ;;
esac
