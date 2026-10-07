-- Configuration for `vim.lsp.enable('gsql_lsp')` (Neovim 0.11+).
---@type vim.lsp.Config
return {
  -- $PATH, else the copy from :GsqlInstall, else the usual install folders.
  cmd = require('gsql').server_command(),
  filetypes = { 'gsql' },
  -- The project root decides which files are searched for the schema. In order
  -- of priority: an explicit `.gsqlroot` file, the git root, then the nearest
  -- folder with a README (for projects that are not in git, such as a
  -- downloaded archive). Without any of them the server only knows the open file.
  root_markers = { { '.gsqlroot' }, { '.git' }, { 'README.md', 'README' } },
  settings = {
    gsql = {
      diagnostics = {
        unknownTypes = true,
        unknownAttributes = true,
        undefinedNames = true,
        unused = true,
        languageRules = true,
        floatEquality = true,
        noSchemaNotice = true,
        duplicateDefinitions = true,
        style = true,
      },
      format = { keywordCase = 'preserve' },
      inlayHints = { enabled = true },
      semanticTokens = { lexical = false },
    },
  },
}
