# GSQL for Neovim

TigerGraph GSQL plugin for Neovim 0.11+ (0.12 tested). Provides filetype detection, Tree-sitter queries (highlights, folds, indents, text objects), and language server integration via [gsql-lsp](https://github.com/abrahamchandy95/gsql-lsp).

## Requirements

| Component          | Source                                      | Prerequisite                      |
| ------------------ | ------------------------------------------- | --------------------------------- |
| Plugin             | Plugin manager                              | `git`                             |
| Language Server    | `:GsqlInstall`, `cargo install`, or `$PATH` | `curl` (or `wget`), `tar`         |
| Tree-sitter Parser | Built automatically on `setup()`            | C compiler (`cc`, `gcc`, `clang`) |

Run `:checkhealth gsql` to verify requirements and server paths.

## Installation

### lazy.nvim / LazyVim

Do not lazy-load with `ft = 'gsql'`; filetype detection must run at startup.

```lua
{
  'abrahamchandy95/gsql-lsp',
  build = function() require('gsql').install_sync() end, -- auto-download on install/update
  opts = {},
}
```

## Configuration Options

```lua
opts = {
  cmd = { '/path/to/gsql-lsp' },  -- Custom binary path (skips PATH search)
  version = 'latest',             -- Target release tag for :GsqlInstall
  install = nil,                  -- Missing server: nil (hint), 'auto', 'prompt', or false
  parser = true,                  -- Automatically compile tree-sitter parser
  lsp = {                         -- Extend vim.lsp.config / pass server settings
    settings = {
      gsql = {
        diagnostics = { style = true },
        format = { keywordCase = 'upper' }, -- 'preserve', 'upper', or 'lower'
      },
    },
  },
}
```

## Formatting & Style

- **Indentation**: Defaults to 4 spaces (`shiftwidth=4`, `softtabstop=4`, `expandtab`) per the GSQL Style Guide. Respects `.editorconfig`.
- **Diagnostics**: Flags lowercase keywords and `#` comments (the style guide prefers `//`). Apply quick fixes with `source.fixAll`.

## Commands

- `:GsqlInstall [tag]` (alias `:GsqlUpdate`): Asynchronously fetches the platform binary, verifies SHA-256 checksums, and installs to `stdpath('data')/gsql/bin/`.
- `:GsqlBuildParser`: Manually compiles `tree-sitter-gsql/src/parser.c` (or use `:TSInstall gsql` if using nvim-treesitter).
- `:checkhealth gsql`: Diagnoses missing tools, server binaries, and parser status.

## Alternative Setups

### vim.pack (Neovim 0.12)

```lua
vim.api.nvim_create_autocmd('PackChanged', {
  callback = function(ev)
    if ev.data.spec.name == 'gsql-lsp' and (ev.data.kind == 'install' or ev.data.kind == 'update') then
      if not ev.data.active then vim.cmd.packadd('gsql-lsp') end
      require('gsql').install_sync()
    end
  end,
})

vim.pack.add({ { src = 'https://github.com/abrahamchandy95/gsql-lsp', name = 'gsql-lsp' } })
require('gsql').setup()
```

### Standalone (Without Plugin)

```lua
vim.filetype.add({ extension = { gsql = 'gsql', gsq = 'gsql' } })
vim.lsp.config('gsql_lsp', {
  cmd = { 'gsql-lsp' },
  filetypes = { 'gsql' },
  root_markers = { { '.gsqlroot' }, { '.git' }, { 'README.md' } },
})
vim.lsp.enable('gsql_lsp')
```

## Server Resolution Order

The plugin starts the first available binary found in:

1. `opts.cmd`
2. `$PATH`
3. `:GsqlInstall` binary (`stdpath('data')/gsql/bin/gsql-lsp`)
4. Standard paths (`~/.local/bin`, `$CARGO_HOME/bin`, `/opt/homebrew/bin`, `/usr/local/bin`)

## Testing

```bash
make neovim-test
LAZY_NVIM=~/.local/share/nvim/lazy/lazy.nvim make neovim-test
```
