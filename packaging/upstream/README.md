# Upstream registry drafts

Ready-to-adapt submissions for the registries where a user would otherwise do
manual setup. None of them can be submitted until the project has a real
repository URL and a tagged release; every draft uses the placeholder
`https://github.com/gsql-lsp/gsql-lsp` (search for it when the repository
moves; each artifact has its own place, listed in the table).

| Registry | What to submit | Draft | Repository URL / revision set in | Verified locally | To verify |
| --- | --- | --- | --- | --- | --- |
| mason-org/mason-registry | `packages/gsql-lsp/package.yaml` | `mason/gsql-lsp/package.yaml` | `source.id`, `homepage`, `schemas.lsp` in that file | Key set and value shapes against all 600 packages of the compiled registry mason.nvim downloaded (`validate_mason.py`); asset and bin names against `.github/workflows/release.yml`; `neovim.lspconfig = gsql_lsp` is how `mason-lspconfig.nvim` maps `ensure_installed` (read from its `mappings.lua` and `ensure_installed.lua`); the draft parsed by ruby's YAML | The YAML source schema and its CI (only the compiled `registry.json` is local: the `schema: registry+v1` line and `make`/schema tooling are not, so the draft omits `schema`); whether the registry needs `ci_skip`/other fields; the `vscode:` schema URL resolves only once a release tag exists; acceptance criteria |
| neovim/nvim-lspconfig | `lsp/gsql_lsp.lua` | `nvim-lspconfig/lsp/gsql_lsp.lua` | URLs in the `---@brief` comment | Resolves as `vim.lsp.config.gsql_lsp` under Neovim 0.12.5 (`--clean -u NONE`, draft dir on rtp); keys limited to `vim.lsp.Config` fields; same header/`---@type` layout as the local files; `stylua` with the repo's `.stylua.toml` passes (`test_lspconfig.lua`) | Docgen (`scripts/docgen.lua`) and `make lint` were not run; CONTRIBUTING requires about 100 GitHub stars or similar evidence of use before a new config is accepted, so this cannot be submitted yet |
| nvim-treesitter (main) | Entry in `lua/nvim-treesitter/parsers.lua` and `runtime/queries/gsql/*.scm` | `nvim-treesitter/parsers.lua.entry`, `nvim-treesitter/runtime/queries/gsql/` | `url` and `revision` in `parsers.lua.entry` | Entry merges into the real `parsers.lua`, uses only documented fields, keeps the table sorted, and `location` has `src/parser.c`; the 5 query files compile against a parser built from `src/parser.c` (ABI 15); highlight captures are all in CONTRIBUTING's capture list; examples parse without errors (`test_treesitter.lua`) | `make query` (ts_query_ls format/lint/check), `./scripts/check-parsers.lua gsql`, `make docs` (SUPPORTED_LANGUAGES.md) and the required `ts_query_ls` CI workflow were not run; the query files are copied verbatim and may not match nvim-treesitter's one-node-per-line format; CONTRIBUTING asks for upstream tree-sitter CI workflows in the grammar repository and `tier = 2` (tier 1 needs semver releases and WASM artifacts) |
| helix-editor/helix | Blocks in `languages.toml`, `runtime/queries/gsql/` | `helix/languages.toml.fragment`, `helix/README.md` | `source.git` and `rev` in the fragment | Textual diff against `editors/helix/languages.toml`: only `injection-regex`, dropped default config and `rev` differ | Everything about Helix itself: no Helix binary; PR requirements in `helix/README.md` are from memory |
| zed-industries/extensions | Submodule plus `extensions.toml` entry | `zed/README.md` (no file: the extension is `editors/zed`) | `repository` and `rev` in `editors/zed/extension.toml` | Nothing run; field list of `editors/zed/extension.toml` compared with the remembered requirements | All of it (no Zed, no wasm toolchain); the missing `editors/zed/LICENSE` |

## Checks

```sh
python3 packaging/upstream/validate_mason.py
ruby -ryaml -rjson -e 'puts JSON.generate(YAML.safe_load(File.read("packaging/upstream/mason/gsql-lsp/package.yaml")))' > /tmp/upstream-mason.json
MASON_YAML_JSON=/tmp/upstream-mason.json nvim --headless --clean -u NONE -l packaging/upstream/test_lspconfig.lua
cc -shared -fPIC -I tree-sitter-gsql/src tree-sitter-gsql/src/parser.c -o /tmp/upstream-ts/gsql.so
nvim --headless --clean -u NONE -l packaging/upstream/test_treesitter.lua
```

The scripts read the plugin checkouts and the Mason registry under
`~/.local/share/nvim` (overridable: `MASON_REGISTRY_JSON`, `LSPCONFIG`,
`NVIM_TS`, `GSQL_PARSER`).

## Keeping the drafts current

- Query copies: `nvim-treesitter/runtime/queries/gsql/` is a verbatim copy of
  the canonical queries; refresh with
  `cp tree-sitter-gsql/queries/{highlights,injections,folds,indents,locals}.scm packaging/upstream/nvim-treesitter/runtime/queries/gsql/`
  (`textobjects.scm` and `tags.scm` are not part of nvim-treesitter's layout).
  Upstream nvim-treesitter keeps queries under `runtime/queries/<lang>/`, not
  `queries/<lang>/`, hence the path.
- Version: `version` in the Mason purl follows the release tag (`v0.1.0`).
- Mason target names: the release ships musl-only Linux builds; the draft maps
  them to `linux_x64`/`linux_arm64` (any libc), as the `aiken` package does.
  There are no Windows ARM or 32-bit assets.
