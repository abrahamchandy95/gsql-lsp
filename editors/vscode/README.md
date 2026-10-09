# GSQL for Visual Studio Code

TigerGraph GSQL support powered by [gsql-lsp](https://github.com/abrahamchandy95/gsql-lsp). Automatically indexes all `.gsql` files in your workspace.

## Features

- **Syntax & Semantics**: TextMate syntax highlighting and semantic tokens for types, parameters, and accumulators (`@@`, `@`).
- **Diagnostics**: Flags syntax errors, missing/unused accumulators, duplicate declarations, unknown schema elements, and GSQL language rule violations.
- **Autocorrect & Quick Fixes**: Fixes typos (e.g., `FORM` → `FROM`), adds missing tokens, and auto-declares accumulators. To run them on save, add this to your `settings.json`:

  ```json
  {
    "[gsql]": {
      "editor.codeActionsOnSave": {
        "source.fixAll": "explicit"
      }
    }
  }
  ```

- **Code Intelligence**: Autocomplete, hover docs, signature help, go-to-definition, find references, rename, outline, and inlay hints.

## Requirements

Official releases include the bundled language server. If installing manually:

- Build via Cargo: `cargo install --path crates/gsql-lsp`
- Or place the [release binary](https://github.com/abrahamchandy95/gsql-lsp) on your `PATH`, or set its location via `gsql.server.path`.

## Settings

| Setting                                 | Default    | Description                                                            |
| --------------------------------------- | ---------- | ---------------------------------------------------------------------- |
| `gsql.server.path`                      | `gsql-lsp` | Path to the language server binary (falls back to the bundled server). |
| `gsql.diagnostics.unknownTypes`         | `true`     | Warn about undeclared vertex/edge types.                               |
| `gsql.diagnostics.unknownAttributes`    | `true`     | Warn about attributes not defined on a type.                           |
| `gsql.diagnostics.undefinedNames`       | `true`     | Warn about undefined identifiers in queries.                           |
| `gsql.diagnostics.unused`               | `true`     | Dim unused accumulators and variables.                                 |
| `gsql.diagnostics.languageRules`        | `true`     | Check constraints from the GSQL language reference.                    |
| `gsql.diagnostics.noSchemaNotice`       | `true`     | Alert when no schema definition is found.                              |
| `gsql.diagnostics.duplicateDefinitions` | `true`     | Report repeated `CREATE` statements across files.                      |
| `gsql.diagnostics.style`                | `true`     | Flag style guide issues (e.g., lowercase keywords, `#` comments).      |
| `gsql.diagnostics.floatEquality`        | `true`     | Warn on exact floating-point comparisons.                              |
| `gsql.format.keywordCase`               | `preserve` | Format keywords as `preserve`, `upper`, or `lower`.                    |
| `gsql.inlayHints.enabled`               | `true`     | Show inferred alias types and argument names.                          |
| `gsql.semanticTokens.lexical`           | `false`    | Enable semantic tokens for keywords, literals, and comments.           |

Default indentation is 4 spaces (`editor.tabSize: 4`), per the GSQL Style Guide.

## Building

```bash
npm install
npm run compile
npx vsce package
```
