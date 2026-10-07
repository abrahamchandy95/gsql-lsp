# GSQL for Visual Studio Code

TigerGraph GSQL language support, powered by [gsql-lsp](https://github.com/gsql-lsp/gsql-lsp).

## Features

- Syntax highlighting (TextMate grammar) refined by semantic tokens: vertex types,
  edge types, parameters, global (`@@`) and vertex-attached (`@`) accumulators.
- Diagnostics: syntax errors, undeclared or unused accumulators, duplicate
  declarations, unknown vertex/edge types and attributes, undefined or misspelled
  names, query arguments (count, names and literal types), and rules from the GSQL
  reference (reserved words, accumulator usage, interpreted and distributed query
  limitations, `=` outside `SYNTAX V3`).
- Autocorrect: a misspelled keyword is reported once, as `did you mean FROM instead
  of FORM?`, with a quick fix, instead of as the syntax errors it causes.
- Completion for keywords, types, accumulators and their methods, vertex and edge
  attributes, schema types in FROM patterns, graphs, queries and built-in functions.
- Hover documentation, signature help, go to definition and type definition, find
  references, rename, document highlights, outline, workspace symbols, folding,
  selection ranges, inlay hints and formatting.
- Quick fixes to correct a misspelled keyword or name, insert a missing token,
  declare an accumulator and more. "Fix all" applies the certain ones; to run it
  on save, add this to your settings:

  ```json
  "[gsql]": { "editor.codeActionsOnSave": { "source.fixAll": "explicit" } }
  ```

The schema can live in other files: the server indexes every `.gsql` file in the
workspace.

## Requirements

The platform-specific packages attached to each release bundle the `gsql-lsp`
server. With a package built without it, install the server from a checkout of the
repository:

```sh
cargo install --path crates/gsql-lsp
```

or download `gsql-lsp-<target>` from the releases page and put it on PATH, or point
`gsql.server.path` at a binary. If no server is found, the extension says so and
offers buttons for the setting and the releases page (`RELEASES_URL` in
`src/extension.ts`). An explicitly configured path always wins
over the bundled server.

## Settings

| Setting | Default | Description |
| --- | --- | --- |
| `gsql.server.path` | `gsql-lsp` | Path to the language server; `~` and `${workspaceFolder}` are expanded (default: the bundled server, else `gsql-lsp` on PATH). |
| `gsql.diagnostics.unknownTypes` | `true` | Warn about undeclared vertex and edge types (when the workspace declares a schema). |
| `gsql.diagnostics.unknownAttributes` | `true` | Warn about attributes a type does not declare. |
| `gsql.diagnostics.undefinedNames` | `true` | Warn about undefined identifiers in queries. |
| `gsql.diagnostics.unused` | `true` | Fade out unused accumulators and variables. |
| `gsql.diagnostics.languageRules` | `true` | Check rules from the GSQL language reference: reserved words, accumulator usage, virtual edges, accumulator types, interpreted/distributed limitations, exact FLOAT/DOUBLE comparison. |
| `gsql.diagnostics.noSchemaNotice` | `true` | Say so when no schema is found (types and attributes are not checked then). |
| `gsql.diagnostics.duplicateDefinitions` | `true` | Report a `CREATE` that repeats a name an earlier `CREATE` defines, in the same file or another one. |
| `gsql.diagnostics.style` | `true` | Hint at deviations from the GSQL Style Guide, with a quick fix: keywords that are not in all caps, `#` comments. |
| `gsql.diagnostics.floatEquality` | `true` | Warn about exact equality between FLOAT/DOUBLE values (not for whole numbers such as `x == 0`). |
| `gsql.format.keywordCase` | `preserve` | `preserve`, `upper` or `lower`. |
| `gsql.inlayHints.enabled` | `true` | Show inferred alias types and query argument names. |
| `gsql.semanticTokens.lexical` | `false` | Also send tokens for keywords, literals and comments. |

GSQL files default to 4 spaces per indentation level (`editor.tabSize` 4,
`editor.insertSpaces` on), as the GSQL Style Guide says, and the formatter follows the
editor's setting. Override it for the language in your settings, for example
`"[gsql]": { "editor.tabSize": 2 }`; `editor.detectIndentation` still lets an existing
file's own indentation win.

## Building

```sh
npm install
npm run compile
npx vsce package
```
