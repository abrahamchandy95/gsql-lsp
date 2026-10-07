---@brief
---
--- https://github.com/gsql-lsp/gsql-lsp
---
--- Language server for TigerGraph GSQL: diagnostics, completion, hover, navigation, formatting and inlay hints.
---
--- Pre-built binaries for Linux, macOS and Windows are attached to the releases at
--- https://github.com/gsql-lsp/gsql-lsp/releases. The server can also be installed with
--- `mason.nvim` (`:MasonInstall gsql-lsp`) or built from source:
--- ```sh
--- cargo install --locked --git https://github.com/gsql-lsp/gsql-lsp gsql-lsp
--- ```
---
--- The default `cmd` assumes that the `gsql-lsp` binary can be found in `$PATH`.
---
--- Nvim does not detect `*.gsql` and `*.gsq` files. Register the filetype with
--- ```lua
--- vim.filetype.add({ extension = { gsql = 'gsql', gsq = 'gsql' } })
--- ```
---
--- The project root is the nearest `.gsqlroot` file, else the git root. The `.gsql` files below it
--- are searched for the graph schema. Settings are documented in the project README, for example:
--- ```lua
--- vim.lsp.config('gsql_lsp', {
---   settings = {
---     gsql = { format = { keywordCase = 'upper' } },
---   },
--- })
--- ```

---@type vim.lsp.Config
return {
  cmd = { 'gsql-lsp' },
  filetypes = { 'gsql' },
  root_markers = { '.gsqlroot', '.git' },
}
