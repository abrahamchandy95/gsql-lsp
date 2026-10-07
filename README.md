Language Server Protocol (LSP) implementation and Tree-sitter grammar for TigerGraph GSQL.

## Features

| Feature             | Details                                                                          |
| ------------------- | -------------------------------------------------------------------------------- |
| **Diagnostics**     | Syntax errors, schema validation, type mismatches, and style checks.             |
| **Autocorrect**     | Keyword typo fixes and error recovery.                                           |
| **Completion**      | Context-aware suggestions for schema types, accumulators, clauses, and patterns. |
| **Hover**           | Type signatures, doc comments, built-ins, and accumulator methods.               |
| **Navigation**      | Definition, references, document outline, and workspace symbols.                 |
| **Editing**         | 4-space formatting, folding ranges, signature help, and inlay hints.             |
| **Quick Fixes**     | One-click fixes for typos, missing tokens, and format on save (`source.fixAll`). |
| **Semantic Tokens** | Syntax highlighting for types, accumulators, parameters, and built-ins.          |

## Installation

```sh
# Install to ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/abrahamchandy95/gsql-lsp/main/scripts/install.sh | sh

# Or build from source
cargo install --path crates/gsql-lsp
```

## CLI Usage

```bash
gsql-lsp check .         # Lint workspace
gsql-lsp format .        # Format files in place
gsql-lsp config [editor] # Print config (neovim, vscode, helix, zed, emacs, vim)
```

## Editor Setup

### Neovim (0.11+)

```lua
-- lazy.nvim
{
  'abrahamchandy95/gsql-lsp',
  build = function() require('gsql').install_sync() end,
  opts = {},
}
```

### VS Code

Install the `.vsix` package from releases or build from `editors/vscode`.

### Helix

Merge `editors/helix/languages.toml` into your Helix configuration and run `hx --grammar fetch && hx --grammar build`.

### Zed

Install dev extension pointing to `editors/zed`.

### Emacs (29+)

Load `editors/emacs/gsql-ts-mode.el` and connect via Eglot (`M-x eglot`).

### Vim

Add `editors/vim` to `runtimepath` and attach your LSP client.

## Configuration

```json
{
  "gsql": {
    "diagnostics": {
      "unknownTypes": true,
      "unknownAttributes": true,
      "undefinedNames": true,
      "unused": true,
      "languageRules": true,
      "floatEquality": true,
      "duplicateDefinitions": true,
      "style": true
    },
    "format": { "keywordCase": "preserve" },
    "inlayHints": { "enabled": true }
  }
}
```

## Development

```bash
make generate  # Regenerate parser
make test      # Run tests
make lint      # Run clippy and rustfmt
```

## License

MIT
