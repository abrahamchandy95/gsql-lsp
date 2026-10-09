# Upstream Registry Drafts

Draft submissions for upstream registries to avoid manual setup.

> **Note:** Before submitting, make sure every draft points to the repository at [https://github.com/abrahamchandy95/gsql-lsp](https://github.com/abrahamchandy95/gsql-lsp).

| Registry                    | Submission Target                                    | Draft Location                                             | Verified Locally                                                                               | Remaining Tasks / Blockers                                                              |
| --------------------------- | ---------------------------------------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `mason-org/mason-registry`  | `packages/gsql-lsp/package.yaml`                     | `mason/gsql-lsp/package.yaml`                              | Structure matches Mason registry packages; assets match release workflow; YAML parses cleanly. | Validate against schema; verify CI requirements and schema URL resolution upon release. |
| `neovim/nvim-lspconfig`     | `lsp/gsql_lsp.lua`                                   | `nvim-lspconfig/lsp/gsql_lsp.lua`                          | Resolves under Neovim 0.12.5; passes `stylua`.                                                 | **Blocked:** requires ~100 GitHub stars. Needs `docgen.lua` and `make lint`.            |
| `nvim-treesitter` (main)    | `lua/.../parsers.lua` & `runtime/queries/gsql/*.scm` | `nvim-treesitter/parsers.lua.entry`, `nvim-treesitter/...` | Parser compiles (ABI 15); queries compile and match standard highlight captures.               | Needs `ts_query_ls` formatting, CI check scripts, and doc generation (`make docs`).     |
| `helix-editor/helix`        | `languages.toml` fragment & queries                  | `helix/languages.toml.fragment`                            | Config diffed against internal Helix settings.                                                 | Test against an actual Helix binary and confirm PR guidelines.                          |
| `zed-industries/extensions` | Submodule & `extensions.toml` entry                  | `zed/README.md` (uses `editors/zed`)                       | Spec checked against standard requirements.                                                    | Needs full verification via Zed/Wasm toolchain; add missing `editors/zed/LICENSE`.      |

## Local Verification Commands

```bash
python3 packaging/upstream/validate_mason.py
ruby -ryaml -rjson -e 'puts JSON.generate(YAML.safe_load(File.read("packaging/upstream/mason/gsql-lsp/package.yaml")))' > /tmp/upstream-mason.json
MASON_YAML_JSON=/tmp/upstream-mason.json nvim --headless --clean -u NONE -l packaging/upstream/test_lspconfig.lua
cc -shared -fPIC -I tree-sitter-gsql/src tree-sitter-gsql/src/parser.c -o /tmp/upstream-ts/gsql.so
nvim --headless --clean -u NONE -l packaging/upstream/test_treesitter.lua
```

Override local paths using `MASON_REGISTRY_JSON`, `LSPCONFIG`, `NVIM_TS`, or `GSQL_PARSER`.

## Maintenance Notes

- **Sync Treesitter Queries**: Copy canonical queries (excluding `textobjects.scm` and `tags.scm`):

  ```bash
  cp tree-sitter-gsql/queries/{highlights,injections,folds,indents,locals}.scm packaging/upstream/nvim-treesitter/runtime/queries/gsql/
  ```

- **Mason Updates**: Update the version field in the package purl with the release tag (e.g., `v0.1.0`). Musl Linux targets map to `linux_x64` / `linux_arm64`.
