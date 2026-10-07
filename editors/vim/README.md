# GSQL for Vim

Filetype detection, a regex syntax file and comment settings work on their own.
Diagnostics, completion, hover and formatting need an LSP client plugin and the
`gsql-lsp` server.

## 1. The server

```sh
sh scripts/install.sh                     # release binary (from a checkout; see the script header for options)
cargo install --path crates/gsql-lsp      # or build from a checkout
```

or download `gsql-lsp-<target>.tar.gz` from the GitHub releases page and put the
binary on PATH.

## 2. The runtime files

Add this directory to the runtimepath (or copy `ftdetect`, `ftplugin` and `syntax`
into `~/.vim`):

```vim
set runtimepath+=/path/to/gsql-lsp/editors/vim
```

With vim-plug: `Plug '/path/to/gsql-lsp', { 'rtp': 'editors/vim' }`.

## 3. An LSP client

Pick one.

vim-lsp (prabirshrestha/vim-lsp):

```vim
if executable('gsql-lsp')
  autocmd User lsp_setup call lsp#register_server({
        \ 'name': 'gsql-lsp',
        \ 'cmd': {server_info -> ['gsql-lsp']},
        \ 'allowlist': ['gsql'],
        \ 'workspace_config': {'gsql': {'format': {'keywordCase': 'preserve'}}},
        \ })
endif
```

coc.nvim: run `:CocConfig` and add

```json
{
  "languageserver": {
    "gsql": {
      "command": "gsql-lsp",
      "filetypes": ["gsql"],
      "rootPatterns": [".gsqlroot", ".git"],
      "settings": { "gsql": { "format": { "keywordCase": "preserve" } } }
    }
  }
}
```

ALE:

```vim
call ale#linter#Define('gsql', {
      \ 'name': 'gsql-lsp',
      \ 'lsp': 'stdio',
      \ 'executable': 'gsql-lsp',
      \ 'command': '%e',
      \ 'project_root': {buffer -> ale#path#FindNearestDirectory(buffer, '.git')},
      \ })
```

The `settings` names match the VS Code `gsql.*` settings.

## Tests

`make editor-test` loads the syntax file against `test/sample.gsql` in Vim. The LSP
snippets above are not covered by it; they follow each plugin's documented
configuration and were not run.
