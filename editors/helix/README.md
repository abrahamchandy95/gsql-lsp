# GSQL for Helix

Highlighting (tree-sitter), indentation, text objects and the gsql-lsp language
server. Nothing to install from a marketplace: copy three things.

## 1. The server

Any one of:

```sh
sh scripts/install.sh
cargo install --path crates/gsql-lsp
```

Or download `gsql-lsp-<target>.tar.gz` (`.zip` on Windows) from the GitHub releases
page and put the `gsql-lsp` binary on PATH. To use a binary that is not on PATH, set
`command = "/full/path/gsql-lsp"` in `languages.toml`.

# 2. The language and grammar

Append `editors/helix/languages.toml` to `~/.config/helix/languages.toml`, then:

```sh
hx --grammar fetch
hx --grammar build
```

The `[[grammar]]` block points at the `tree-sitter-gsql` subdirectory of the
repository (`subpath`); `src/parser.c` is committed, so no tree-sitter CLI is needed,
only a C compiler. Pin `rev` to a tag or commit for reproducible builds. To build
from a local checkout instead, use
`source = { path = "/path/to/gsql-lsp/tree-sitter-gsql" }`.

## 3. The queries

```sh
mkdir -p ~/.config/helix/runtime/queries
cp -r editors/helix/queries/gsql ~/.config/helix/runtime/queries/
```

Verify with `hx --health gsql`: the language server, grammar and query files
should show as found.

## Settings

The `[language-server.gsql-lsp.config.gsql]` table in `languages.toml` holds the
server settings (same names as the VS Code `gsql.*` settings).

Status: written against the Helix configuration format; not run (Helix is not
installed on the development machine).
