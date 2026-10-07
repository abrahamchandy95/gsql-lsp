# GSQL for Neovim

Neovim 0.11+ (tested with 0.12). The plugin gives you file type detection, the
`gsql_lsp` language server configuration, tree-sitter queries (highlights, folds,
indents, locals, text objects) and two helpers: `:GsqlInstall` downloads the server,
`:GsqlBuildParser` compiles the tree-sitter parser.

What you need:

| Part | How you get it | Needs |
| --- | --- | --- |
| Plugin | your plugin manager (below) | git |
| Language server `gsql-lsp` | `:GsqlInstall` (release download), or `cargo install --path crates/gsql-lsp`, or any `gsql-lsp` on `$PATH` | curl (or wget) and tar; no Rust toolchain |
| Tree-sitter parser | built automatically by `setup()` from `tree-sitter-gsql/src/parser.c` | a C compiler (`cc`, `gcc`, `clang` or `$CC`) |

`:checkhealth gsql` shows which of these are found, where the server came from
(your `setup({ cmd })`, `$PATH`, `:GsqlInstall`, or a standard folder), its version, and
what to do when something is missing.

The plugin lives in `editors/neovim` of the repository. The repository root has a small
shim (`lua/gsql/init.lua`, `plugin/gsql.lua`) that puts `editors/neovim` on
`runtimepath`, so a plain `owner/repo` spec works with lazy.nvim and `vim.pack` and no
`runtimepath` handling is needed on your side.

## lazy.nvim

```lua
{
  'gsql-lsp/gsql-lsp',            -- PLACEHOLDER repository, see "Where the repository is set"
  build = function() require('gsql').install_sync() end,  -- download the server on install and on every update
  opts = {},                      -- calls require('gsql').setup(opts)
}
```

Without `build`, `:GsqlInstall` can be run by hand, or set `opts = { install = 'auto' }` to
download the server when the first `.gsql` file is opened and the server is missing.
Leave the plugin loaded at startup (lazy.nvim's default for a plain spec): file type
detection and `setup()` must run before a `.gsql` buffer is created, so do not add
`ft = 'gsql'`.

`opts` (all optional):

```lua
opts = {
  cmd = { '/path/to/gsql-lsp' },  -- fixed server command; skips the search below
  version = 'latest',             -- release for :GsqlInstall: 'latest' or a tag such as 'v0.1.0'
  install = nil,                  -- server missing: nil = print a hint once, 'prompt', 'auto', or false = silent
  parser = true,                  -- false: do not build the tree-sitter parser automatically
  lsp = true,                     -- false: no language server; a table extends the vim.lsp.config
}
```

### LazyVim

Put the spec in `~/.config/nvim/lua/plugins/gsql.lua`:

```lua
return {
  {
    'gsql-lsp/gsql-lsp',
    build = function() require('gsql').install_sync() end,
    opts = {},
  },
}
```

The plugin calls `vim.lsp.enable('gsql_lsp')` itself, so nothing is needed in LazyVim's
`nvim-lspconfig` options; `gsql_lsp` is not an nvim-lspconfig or Mason server. To change
server settings, pass them through `opts.lsp`:

```lua
opts = { lsp = { settings = { gsql = { diagnostics = { unused = false } } } } },
```

## Indentation and style

The GSQL Style Guide indents the body of a block by 4 spaces, with spaces instead of tabs,
so the ftplugin sets `shiftwidth=4`, `softtabstop=4` and `expandtab` in GSQL buffers.
Formatting (`vim.lsp.buf.format()`, LazyVim's format on save) indents by `shiftwidth`, so
what you type and what the formatter writes agree; it also puts the parameters of a long
query header (and the fields of a long `TYPEDEF TUPLE`) one per line, indented by
`shiftwidth`. A `.editorconfig` that sets `indent_size` or `indent_style` for `*.gsql`
wins over these defaults, and so does your own `after/ftplugin/gsql.lua`
(`vim.bo.shiftwidth = 2`).

For the two things the guide asks for that the formatter leaves alone, the server shows
hints with a quick fix: keywords and reserved words that are not in all caps
(`keyword-case`), and `#` comments (`hash-comment`, the guide writes `//`). The code action
`source.fixAll` applies both. Settings, through `opts.lsp` as above:

```lua
-- no style hints
opts = { lsp = { settings = { gsql = { diagnostics = { style = false } } } } },
-- or: the formatter writes keywords in all caps as well
opts = { lsp = { settings = { gsql = { format = { keywordCase = 'upper' } } } } },
```

## vim.pack (Neovim 0.12)

`vim.pack.add` clones the repository to `stdpath('data')/site/pack/core/opt/gsql-lsp`
and runs `packadd` on it, which sources the shim.

```lua
-- Download the server after the plugin is installed or updated. Register this
-- BEFORE vim.pack.add: it installs synchronously and fires PackChanged during the call.
vim.api.nvim_create_autocmd('PackChanged', {
  callback = function(ev)
    local data = ev.data
    if data.spec.name == 'gsql-lsp' and (data.kind == 'install' or data.kind == 'update') then
      if not data.active then
        vim.cmd.packadd('gsql-lsp')
      end
      require('gsql').install_sync()
    end
  end,
})

vim.pack.add({ { src = 'https://github.com/gsql-lsp/gsql-lsp', name = 'gsql-lsp' } })
require('gsql').setup()
```

## Manual

Clone the repository anywhere and add it to `runtimepath`:

```lua
vim.opt.rtp:append(vim.fn.expand('~/src/gsql-lsp'))   -- the repository root
require('gsql').setup()
```

`require('gsql')` (also) adds `editors/neovim` to `runtimepath`. Appending
`~/src/gsql-lsp/editors/neovim` itself works too, and is what older instructions did.
After startup, `:runtime plugin/gsql.lua` registers the commands. Run `:GsqlInstall` once.

## Without the plugin

Neovim has no built-in `.gsql` filetype, so the filetype line is needed as well as the
LSP config (`gsql-lsp config neovim --variant lsp-config` prints this with the settings):

```lua
vim.filetype.add({ extension = { gsql = 'gsql', gsq = 'gsql' } })
vim.lsp.config('gsql_lsp', {
  cmd = { 'gsql-lsp' },
  filetypes = { 'gsql' },
  root_markers = { { '.gsqlroot' }, { '.git' }, { 'README.md', 'README' } },
})
vim.lsp.enable('gsql_lsp')
```

## Finding the server

`setup()` and `vim.lsp.enable('gsql_lsp')` start the first of these that exists:

1. `setup({ cmd = { ... } })` (or `lsp.cmd`), used as given
2. `gsql-lsp` on `$PATH`
3. the copy made by `:GsqlInstall`: `stdpath('data')/gsql/bin/gsql-lsp` (`.exe` on Windows)
4. `~/.local/bin`, `$CARGO_HOME/bin` (default `~/.cargo/bin`), `/opt/homebrew/bin`, `/usr/local/bin`

A Neovim started from Finder has a short `$PATH`, hence the folders in step 4. After
`:GsqlInstall` finishes, the language server is restarted in the open GSQL buffers
(unless you set `cmd` yourself).

## :GsqlInstall

`:GsqlInstall [tag]` (alias `:GsqlUpdate`) runs asynchronously and reports with
`vim.notify`:

1. picks the release target for your system: `aarch64`/`x86_64` on macOS (`*-apple-darwin`) and
   Linux (`*-unknown-linux-musl`, static), `x86_64-pc-windows-msvc` on Windows; other platforms get a
   message to use `cargo install` instead
2. downloads `SHA256SUMS` and `gsql-lsp-<target>.tar.gz` (`.zip` on Windows) with `curl` (or `wget`)
3. refuses to go on when the archive's sha256 is not the one in `SHA256SUMS`
4. unpacks with `tar` (`Expand-Archive` for a zip on Windows), runs `gsql-lsp --version` on the
   result, and only then moves it to `stdpath('data')/gsql/bin/gsql-lsp` (mode 0755)

The archive and checksum names are those made by `.github/workflows/release.yml`.

### Where the repository is set

One constant per artifact:

- Lua plugin: `M.repo_url` at the top of `editors/neovim/lua/gsql/init.lua`. The release URL
  (`<repo_url>/releases`) and the nvim-treesitter registration both derive from it. It is a
  placeholder (`https://github.com/gsql-lsp/gsql-lsp`) until the real repository exists.
- The plugin specs above name the repository in your own config.

Override the release location without editing the plugin, with a URL that has GitHub's layout
(`<base>/latest/download/<file>` and `<base>/download/<tag>/<file>`; `file://` works):

```lua
vim.g.gsql_release_base_url = 'https://mirror.example.com/gsql-lsp/releases'
```

or `GSQL_RELEASE_BASE_URL=file:///path/to/releases nvim`. The tests use this with a fake
release built on disk.

## Tree-sitter parser

`setup()` compiles `tree-sitter-gsql/src/parser.c` to `editors/neovim/parser/gsql.so` when
that file is missing or older than the grammar (so again after a plugin update). Paths are
relative to the plugin directory, so this works wherever the plugin manager puts the
repository. It needs a C compiler; without one, `setup()` stays silent, the first GSQL
buffer prints a warning that says what to install, and `:GsqlBuildParser` reports the same
as an error. Alternatively, with nvim-treesitter installed, `setup()` registers the parser
and `:TSInstall gsql` builds it (nvim-treesitter's `main` branch also needs the
`tree-sitter` CLI for that).

Use `parser = false` if another plugin manages the parser.

## Mason

The server is not in the Mason registry, so `:GsqlInstall` is the no-Rust route. A registry
entry could use the release archives directly (same shape as the `aiken` package in
mason-registry, whose archives also contain a top-level folder). An untested draft for
`packages/gsql-lsp/package.yaml` in `mason-org/mason-registry`:

```yaml
name: gsql-lsp
description: Language server for TigerGraph GSQL.
homepage: https://github.com/gsql-lsp/gsql-lsp
licenses: [MIT]
languages: [GSQL]
categories: [LSP]
source:
  id: pkg:github/gsql-lsp/gsql-lsp@v0.1.0
  asset:
    - target: darwin_arm64
      file: gsql-lsp-aarch64-apple-darwin.tar.gz
      bin: gsql-lsp-aarch64-apple-darwin/gsql-lsp
    - target: darwin_x64
      file: gsql-lsp-x86_64-apple-darwin.tar.gz
      bin: gsql-lsp-x86_64-apple-darwin/gsql-lsp
    - target: linux_x64_musl
      file: gsql-lsp-x86_64-unknown-linux-musl.tar.gz
      bin: gsql-lsp-x86_64-unknown-linux-musl/gsql-lsp
    - target: linux_arm64_musl
      file: gsql-lsp-aarch64-unknown-linux-musl.tar.gz
      bin: gsql-lsp-aarch64-unknown-linux-musl/gsql-lsp
    - target: win_x64
      file: gsql-lsp-x86_64-pc-windows-msvc.zip
      bin: gsql-lsp-x86_64-pc-windows-msvc/gsql-lsp.exe
bin:
  gsql-lsp: "{{source.asset.bin}}"
```

The target names and `bin` layout were read from the local registry snapshot; the entry
itself was not validated against the registry schema.

## Tests

```sh
make neovim-test                          # end-to-end, :GsqlInstall against a fake release, repository layout
LAZY_NVIM=~/.local/share/nvim/lazy/lazy.nvim make neovim-test   # also load the layout through real lazy.nvim
```

- `test/e2e.lua`: highlighting, folding and the language server features through Neovim's client.
- `test/install.lua`: builds a fake release (tar.gz with the locally built server, `SHA256SUMS`), points
  `GSQL_RELEASE_BASE_URL` at it, and checks the install, checksum rejection, update, server search
  order and `:checkhealth`. Unix only.
- `test/layout.lua`: copies the repository like a clone and checks that `require('gsql')`, file type
  detection, `ftplugin/`, queries, the LSP config and the parser build resolve from the repository root.
