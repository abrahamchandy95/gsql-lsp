# Deployment

How each editor gets the `gsql-lsp` server, its plugin or extension, and the
tree-sitter grammar, and what the maintainer does to publish them.

Status: the GitHub repository does not exist yet. `https://github.com/gsql-lsp/gsql-lsp`
is a placeholder everywhere (see [Setting the repository URL](#setting-the-repository-url)).
Nothing has been published. Statements marked **(verify)** come from memory or
from the documentation of a tool that was not available while writing this and
must be checked when publishing. Statements about the release workflow,
`scripts/install.sh`, the Homebrew formula and the Cargo packages were tested
locally (see [Testing without a release](#testing-without-a-release)).

## What a release contains

Pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`. Asset names carry no
version, so `https://github.com/<owner>/<repo>/releases/latest/download/<asset>`
always points at the newest release.

| Asset | Content |
| --- | --- |
| `gsql-lsp-x86_64-unknown-linux-musl.tar.gz`, `gsql-lsp-aarch64-unknown-linux-musl.tar.gz` | Static Linux binary |
| `gsql-lsp-x86_64-apple-darwin.tar.gz`, `gsql-lsp-aarch64-apple-darwin.tar.gz` | macOS binary (one per architecture, no universal binary) |
| `gsql-lsp-x86_64-pc-windows-msvc.zip` | Windows binary (`gsql-lsp.exe`) |
| `gsql-<platform>.vsix` | VS Code extensions with the server bundled: `linux-x64`, `alpine-x64`, `linux-arm64`, `alpine-arm64`, `darwin-x64`, `darwin-arm64`, `win32-x64` |
| `SHA256SUMS` | `sha256sum` format (`<hash>  <file>`) for the archives and the `.vsix` files |
| `gsql-lsp.rb` | Homebrew formula for this version, generated from `SHA256SUMS` |
| `install.sh` | The installer script |

Every archive unpacks to a `gsql-lsp-<target>/` directory holding `gsql-lsp`
(`gsql-lsp.exe`), `README.md` and `LICENSE`. Installers (`scripts/install.sh`,
cargo-binstall, Homebrew, the Mason package) depend on these names and on this
layout; change them only together.

Tags containing `-` (`v0.2.0-rc.1`) are published as GitHub pre-releases and are
not pushed to crates.io or the extension marketplaces.

## Decision table

| Editor | Binary | Plugin / extension | Grammar | Automatic | By hand |
| --- | --- | --- | --- | --- | --- |
| Neovim 0.11+ | Mason package `gsql-lsp` (once accepted into the registry **(verify)**), `install.sh`, Homebrew, `cargo install`/`cargo binstall` | `editors/neovim` (lazy.nvim spec pointing at the repository) | nvim-treesitter (`:TSInstall gsql`, after its PR is merged) or `:GsqlBuildParser` from the plugin | Filetype, LSP config, queries | Installing the binary; see `editors/neovim/README.md` |
| VS Code / VSCodium | Bundled in the platform `.vsix` | Marketplace / Open VSX, or the `.vsix` from the release | None needed: TextMate grammar | Everything | `gsql.server.path` only for a custom binary |
| Zed | `install.sh`, Homebrew, `cargo install` (Zed does not download it yet) | `editors/zed` (Zed extension registry, or *install dev extension*) | Zed builds it from `[grammars.gsql]` in `extension.toml` (pinned commit) | Grammar, queries | Binary on `PATH` |
| Helix | `install.sh`, Homebrew, `cargo install` | None: merge `editors/helix/languages.toml`, copy `editors/helix/queries/gsql` | `hx --grammar fetch && hx --grammar build` from the repository | Nothing | Config merge, query copy, grammar build |
| Vim 8.1+ | As Helix | `editors/vim` (syntax, ftdetect, ftplugin); LSP through vim-lsp / vim-lsp-settings / Coc, see the README | None: regex syntax | Nothing | Everything |
| Emacs 29+ | As Helix | `editors/emacs/gsql-ts-mode.el` (Eglot) | `M-x treesit-install-language-grammar RET gsql` (repository + `tree-sitter-gsql/src`) | Mode, Eglot entry | Grammar install |

## Getting the binary

| Route | Command | Needs |
| --- | --- | --- |
| Installer script | `curl -fsSL https://github.com/<owner>/<repo>/releases/latest/download/install.sh \| sh` | A release; curl or wget; `sha256sum` or `shasum`; Linux or macOS |
| Homebrew tap | `brew install <owner>/gsql-lsp/gsql-lsp` | A tap repository `homebrew-gsql-lsp` holding `Formula/gsql-lsp.rb` |
| cargo-binstall | `cargo binstall gsql-lsp` | The crate on crates.io and a release |
| crates.io | `cargo install gsql-lsp` | The crates on crates.io; a C compiler (the grammar is C) |
| Windows | Download the `.zip`, put `gsql-lsp.exe` on `PATH` | A release |
| Source | `cargo install --path crates/gsql-lsp` | Rust toolchain, C compiler |

`scripts/install.sh` installs to `$GSQL_LSP_INSTALL_DIR` (default `~/.local/bin`)
and prints a `PATH` hint when needed. Environment: `GSQL_LSP_VERSION` (`0.1.0`,
`v0.1.0` or `latest`), `GSQL_RELEASE_BASE_URL` (a directory URL, `http(s)://` or
`file://`, holding the assets, for mirrors and offline use). Linux binaries are
static (musl), so they run on glibc and musl systems alike. macOS detects Apple
silicon under Rosetta and installs the arm64 binary.

cargo-binstall reads `[package.metadata.binstall]` in `crates/gsql-lsp/Cargo.toml`:
`pkg-url` is `{ repo }/releases/download/v{ version }/gsql-lsp-{ target }.{ archive-format }`
and `{ repo }` is the `repository` field. On glibc Linux it should fall back to
the musl asset because no `-gnu` archive exists **(verify with a real release:
`cargo binstall --dry-run gsql-lsp`)**.

## Setting the repository URL

Replace the placeholder once the repository exists. One obvious place per artifact:

| Artifact | File |
| --- | --- |
| Cargo packages, cargo-binstall, Homebrew formula (its default) | `repository` and `homepage` in `Cargo.toml` (workspace) |
| `tree-sitter-gsql` crate | `repository` in `tree-sitter-gsql/Cargo.toml` |
| Installer | `REPO_URL` at the top of `scripts/install.sh` |
| Neovim plugin | `editors/neovim/lua/gsql/init.lua` |
| VS Code extension | `repository.url` in `editors/vscode/package.json`, link in `editors/vscode/README.md` |
| Zed extension | `repository` (twice) in `editors/zed/extension.toml` |
| Helix | `source.git` in `editors/helix/languages.toml` |
| Emacs | `URL:` header and the grammar source in `editors/emacs/gsql-ts-mode.el` |
| Vim | `URL:` header in `editors/vim/syntax/gsql.vim` |
| Grammar metadata | `tree-sitter-gsql/{tree-sitter.json,package.json,pyproject.toml,CMakeLists.txt,Makefile}` |
| Documentation | `README.md` |

To list every occurrence, and to replace them all (check the diff; the Cargo
and `package.json` entries are the ones with a different form):

```sh
grep -rl 'github.com/gsql-lsp/gsql-lsp' --exclude-dir=.git --exclude-dir=target --exclude=Cargo.lock .
grep -rl 'github.com/gsql-lsp/gsql-lsp' --exclude-dir=.git --exclude-dir=target --exclude=Cargo.lock . \
  | xargs sed -i.bak 's|github.com/gsql-lsp/gsql-lsp|github.com/OWNER/REPO|g'   # then delete *.bak
```

The VS Code `publisher` (`gsql-lsp` in `editors/vscode/package.json`) and the
Homebrew tap name must match the accounts you create.

## Maintainer setup (once)

1. Create the GitHub repository and push `main`. Set the URL as above, commit.
2. Repository secrets (Settings, Secrets and variables, Actions). All optional;
   a publishing step without its secret is skipped:
   - `CARGO_REGISTRY_TOKEN`: crates.io API token with publish rights.
   - `VSCE_PAT`: Azure DevOps personal access token (scope Marketplace, manage)
     of the Visual Studio Marketplace publisher named in `package.json`.
   - `OVSX_PAT`: Open VSX access token (the namespace must exist: `npx ovsx create-namespace <publisher> -p <token>` **(verify)**).
3. Create the publisher accounts: crates.io (log in with GitHub), Marketplace
   publisher, Open VSX namespace.
4. Homebrew tap: create the repository `homebrew-gsql-lsp` (name must start with
   `homebrew-`).

## Release checklist

1. `scripts/set_version.py X.Y.Z` (updates every package, the grammar dependency
   of the server and `Cargo.lock`), update `CHANGELOG.md`, commit.
2. Locally: `make lint`, `make test`, `make editor-test`, `make packaging-test`.
3. In `editors/zed/extension.toml` set `rev` of the grammar to a commit that
   contains `tree-sitter-gsql/src/parser.c` (Zed needs a SHA, not a branch).
   Commit again if it changed.
4. `git tag vX.Y.Z && git push origin main vX.Y.Z`. The workflow checks that the tag
   equals the crate and extension versions, builds five targets, packages
   seven `.vsix` files, publishes the release with `SHA256SUMS`, `gsql-lsp.rb`
   and `install.sh`, then publishes to crates.io, the Marketplace and Open VSX
   if the secrets exist.
5. Smoke test the release: `curl -fsSL .../releases/latest/download/install.sh | sh`
   in a scratch `GSQL_LSP_INSTALL_DIR`, then `gsql-lsp --version`.
6. Homebrew: download `gsql-lsp.rb` from the release into
   `homebrew-gsql-lsp/Formula/gsql-lsp.rb`, commit, push. Check with
   `brew install <owner>/gsql-lsp/gsql-lsp && brew test gsql-lsp`.
7. One-time registry submissions (after the first release; each is reviewed by
   its maintainers, so expect changes):
   - **Mason** (`mason-org/mason-registry`): add `packages/gsql-lsp/package.yaml`
     **(verify the current schema in that repository)**. The compiled registry
     installed with Mason (`registry.json`) shows the shape of a GitHub-release
     package: `source.id = pkg:github/<owner>/<repo>@vX.Y.Z`, one `asset` entry per
     Mason `target` (`linux_x64`, `linux_arm64`, `darwin_x64`, `darwin_arm64`,
     `win_x64`) with `file` (our asset name) and `bin` (`gsql-lsp-<target>/gsql-lsp`,
     `gsql-lsp.exe` for the zip), `bin: { gsql-lsp: "{{source.asset.bin}}" }` and
     `neovim.lspconfig: gsql_lsp`. The archive layout above matches the `ruff`
     entry. The lspconfig name must exist in nvim-lspconfig, or be registered by
     the plugin; Mason itself only installs the binary. New versions are then
     bumped by the registry's renovate bot **(verify)**.
   - **nvim-treesitter** (`main` branch): its `CONTRIBUTING.md` asks for an entry in
     `lua/nvim-treesitter/parsers.lua` with `install_info = { url, revision,
     location = 'tree-sitter-gsql' }` and a `tier`, queries under
     `runtime/queries/gsql/`, and a parser that meets the inclusion criteria
     (actively maintained, CI with the upstream tree-sitter workflows, `ts_query_ls`
     check of the queries, filetype known to Neovim). The grammar lives in a
     subdirectory, which `location` supports. Until the PR is merged, the Neovim
     plugin registers the parser itself and `:GsqlBuildParser` works.
   - **Zed** (`zed-industries/extensions`): add the extension repository as a git
     submodule and an entry in `extensions.toml`, with the version of
     `extension.toml` **(verify in that repository's README)**. Zed compiles the
     extension to WebAssembly on its side; the extension needs the `wasm32-wasip2`
     Rust target to build locally **(verify)**.
   - **Vim / Emacs**: no central registry is required. Emacs users can install
     `gsql-ts-mode.el` with `package-vc-install` from the repository **(verify)**;
     MELPA takes a recipe PR if you want it there.

## Crates

`gsql-lsp` depends on `tree-sitter-gsql` by path and by version
(`{ path = "../../tree-sitter-gsql", version = "0.1.0" }`): cargo uses the path
in the workspace and the version on crates.io. The parser is generated C
(`tree-sitter-gsql/src/parser.c`, committed) and ships inside the grammar crate
through its `include` list; the server crate ships `src/` and
`data/builtin-docs.json`.

Publish the grammar first, then the server (the release workflow does this):

```sh
cargo publish --locked -p tree-sitter-gsql
cargo publish --locked -p gsql-lsp      # may need a minute for the index to pick up the grammar
```

`cargo package --workspace --locked` builds both packages from their `.crate`
files, with the grammar taken from a local overlay, so it also works before the
first publish. Cargo warns that the `tests/` and `examples/` targets of the server
are not in the package; that is expected (the tests read files outside the crate).
`cargo publish --dry-run -p gsql-lsp` alone fails until the grammar is on crates.io.
Whether the crate name `gsql-lsp` and `tree-sitter-gsql` are free on crates.io
has not been checked.

## Testing without a release

Everything below runs offline against a fake release directory:

```sh
cargo build
GSQL_LSP_BIN=target/debug/gsql-lsp sh scripts/test_install.sh   # installer: success, bad checksum, unsupported platform, re-run
sh scripts/test_homebrew.sh                                      # formula generator on a fake SHA256SUMS, ruby -c
cargo package --workspace --allow-dirty                          # (make packaging-test runs all three)
```

To try the installer by hand, lay out a directory like a release
(`gsql-lsp-<target>.tar.gz` containing `gsql-lsp-<target>/gsql-lsp`, and a
`SHA256SUMS` listing the archives) and run
`GSQL_RELEASE_BASE_URL=file:///path/to/dir GSQL_LSP_INSTALL_DIR=/tmp/bin sh scripts/install.sh`.
Other tools accept the same directory through their own overrides where they
have them (`scripts/homebrew_formula.py --checksums`).

Not covered: the `http(s)` download path (`curl`/`wget`) against a server, the
GitHub Actions workflows themselves (parsed as YAML and read only), and the
registries. A first real run of `release.yml` with a pre-release tag (`vX.Y.Z-rc.1`, after
`scripts/set_version.py X.Y.Z-rc.1`) is the test for the workflow; it publishes
to no registry. `vsce` may reject a pre-release version in `package.json`
**(verify)**, in which case the `vscode` job fails on such a tag.
