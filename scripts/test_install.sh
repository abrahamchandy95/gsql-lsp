#!/bin/sh
# Offline test of scripts/install.sh: builds a fake release directory from a
# locally built gsql-lsp binary and installs from it through file:// (and, when
# python3 can bind a local port, http://) URLs.
#
#   GSQL_LSP_BIN=target/debug/gsql-lsp sh scripts/test_install.sh
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${GSQL_LSP_BIN:-$root/target/debug/gsql-lsp}"
[ -x "$bin" ] || { echo "test_install.sh: build gsql-lsp first (or set GSQL_LSP_BIN)" >&2; exit 2; }
install_sh="$root/scripts/install.sh"
# Shell that runs the installer (e.g. TEST_INSTALLER_WITH=dash).
runner="${TEST_INSTALLER_WITH:-sh}"

work="$(mktemp -d "${TMPDIR:-/tmp}/gsql-install-test.XXXXXX")"
server_pid=""
cleanup() {
    [ -z "$server_pid" ] || { kill "$server_pid" 2>/dev/null; wait "$server_pid" 2>/dev/null; } || true
    rm -rf "$work"
}
trap cleanup EXIT

failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; failures=$((failures + 1)); }
check() { # name, command...
    name="$1"
    shift
    if "$@"; then pass "$name"; else fail "$name"; fi
}
contains() { grep -q -- "$2" "$1"; }

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# A release directory with an archive per unix target, laid out like
# .github/workflows/release.yml does (gsql-lsp-<target>/gsql-lsp).
make_release() { # dir binary
    rel="$1"
    mkdir -p "$rel"
    for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin; do
        stage="$work/stage/gsql-lsp-$target"
        rm -rf "$work/stage"
        mkdir -p "$stage"
        cp "$2" "$stage/gsql-lsp"
        cp "$root/README.md" "$root/LICENSE" "$stage/"
        tar -czf "$rel/gsql-lsp-$target.tar.gz" -C "$work/stage" "gsql-lsp-$target"
    done
    (cd "$rel" && sha gsql-lsp-*.tar.gz > SHA256SUMS)
}

release="$work/release"
make_release "$release" "$bin"
dest="$work/bin"

run() { # extra env assignments are passed via env(1) by the caller
    "$@" >"$work/out" 2>&1
}

# 1. Install succeeds and the binary runs.
check "install from file:// succeeds" run env GSQL_RELEASE_BASE_URL="file://$release" GSQL_LSP_INSTALL_DIR="$dest" $runner "$install_sh"
check "installed binary is executable" test -x "$dest/gsql-lsp"
check "installed binary reports its version" sh -c "'$dest/gsql-lsp' --version | grep -q '^gsql-lsp '"
check "PATH hint is printed when the directory is not in PATH" contains "$work/out" "not in your PATH"

# 2. Re-running is idempotent.
check "re-run succeeds" run env GSQL_RELEASE_BASE_URL="file://$release" GSQL_LSP_INSTALL_DIR="$dest" $runner "$install_sh"
check "re-run reports up to date" contains "$work/out" "already up to date"
check "no stray temp files" sh -c "[ \"\$(/bin/ls -A '$dest')\" = gsql-lsp ]"

# 3. No PATH hint when the directory is on PATH.
run env PATH="$dest:$PATH" GSQL_RELEASE_BASE_URL="file://$release" GSQL_LSP_INSTALL_DIR="$dest" $runner "$install_sh" || true
check "no PATH hint when already on PATH" sh -c "! grep -q 'not in your PATH' '$work/out'"

# 4. A different binary in the release replaces the installed one.
printf '#!/bin/sh\necho "gsql-lsp 9.9.9"\n' >"$work/fake-bin"
chmod 755 "$work/fake-bin"
make_release "$work/release2" "$work/fake-bin"
check "upgrade succeeds" run env GSQL_RELEASE_BASE_URL="file://$work/release2" GSQL_LSP_INSTALL_DIR="$dest" $runner "$install_sh"
check "upgrade replaced the binary" sh -c "[ \"\$('$dest/gsql-lsp' --version)\" = 'gsql-lsp 9.9.9' ]"

# 5. A wrong checksum is rejected and nothing is installed.
cp -R "$release" "$work/bad"
zeros=0000000000000000000000000000000000000000000000000000000000000000
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin; do
    sed "s/^[0-9a-f]\{64\}  gsql-lsp-$target/$zeros  gsql-lsp-$target/" "$work/bad/SHA256SUMS" >"$work/bad/SHA256SUMS.new"
    mv "$work/bad/SHA256SUMS.new" "$work/bad/SHA256SUMS"
done
check "checksum mismatch is rejected" sh -c "! env GSQL_RELEASE_BASE_URL='file://$work/bad' GSQL_LSP_INSTALL_DIR='$work/bad-dest' $runner '$install_sh' >'$work/out' 2>&1"
check "mismatch message" contains "$work/out" "checksum mismatch"
check "mismatch installs nothing" test ! -e "$work/bad-dest/gsql-lsp"

# 6. A corrupt archive with the original checksums is rejected too.
cp -R "$release" "$work/corrupt"
for f in "$work"/corrupt/*.tar.gz; do printf 'x' >>"$f"; done
check "corrupt archive is rejected" sh -c "! env GSQL_RELEASE_BASE_URL='file://$work/corrupt' GSQL_LSP_INSTALL_DIR='$work/corrupt-dest' $runner '$install_sh' >'$work/out' 2>&1"
check "corrupt archive installs nothing" test ! -e "$work/corrupt-dest/gsql-lsp"

# 7. A checksums file without an entry for the archive is an error.
cp -R "$release" "$work/noentry"
: >"$work/noentry/SHA256SUMS"
check "missing checksum entry is rejected" sh -c "! env GSQL_RELEASE_BASE_URL='file://$work/noentry' GSQL_LSP_INSTALL_DIR='$work/noentry-dest' $runner '$install_sh' >'$work/out' 2>&1"
check "missing entry message" contains "$work/out" "no entry"

# 8. A missing asset is an error.
check "missing release is an error" sh -c "! env GSQL_RELEASE_BASE_URL='file://$work/nonexistent' GSQL_LSP_INSTALL_DIR='$work/none-dest' $runner '$install_sh' >'$work/out' 2>&1"
check "missing release message" contains "$work/out" "could not download"

# 9. Unsupported platforms get a clear message (uname is stubbed).
mkdir -p "$work/stub"
printf '#!/bin/sh\ncase "$1" in -s) echo FreeBSD ;; *) echo amd64 ;; esac\n' >"$work/stub/uname"
chmod 755 "$work/stub/uname"
check "unsupported platform exits non-zero" sh -c "! env PATH='$work/stub:$PATH' GSQL_RELEASE_BASE_URL='file://$release' GSQL_LSP_INSTALL_DIR='$work/unsup-dest' $runner '$install_sh' >'$work/out' 2>&1"
check "unsupported platform message" contains "$work/out" "unsupported platform: FreeBSD amd64"
check "unsupported platform installs nothing" test ! -e "$work/unsup-dest"

# 10. Unknown arguments are rejected; --help works.
check "unknown argument is rejected" sh -c "! $runner '$install_sh' --bogus >'$work/out' 2>&1"
check "--help prints usage" sh -c "$runner '$install_sh' --help | grep -q GSQL_LSP_INSTALL_DIR"

# 11. The download path through curl (http://), when a local port can be bound.
if command -v curl >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
    port=$((20000 + $$ % 20000))
    (cd "$release" && exec python3 -m http.server "$port" --bind 127.0.0.1 >/dev/null 2>&1) &
    server_pid=$!
    i=0
    until curl -fs "http://127.0.0.1:$port/SHA256SUMS" >/dev/null 2>&1; do
        i=$((i + 1))
        [ "$i" -lt 30 ] || break
        sleep 0.2
    done
    if [ "$i" -lt 30 ]; then
        check "install over http:// succeeds" run env GSQL_RELEASE_BASE_URL="http://127.0.0.1:$port/" GSQL_LSP_INSTALL_DIR="$work/http-dest" $runner "$install_sh"
        check "http install produced the binary" test -x "$work/http-dest/gsql-lsp"
    else
        echo "skip http:// test (no local server)"
    fi
else
    echo "skip http:// test (curl or python3 missing)"
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed" >&2
    exit 1
fi
echo "all install.sh checks passed"
