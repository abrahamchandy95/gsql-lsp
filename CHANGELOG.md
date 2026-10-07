# Changelog

## Unreleased (0.1.0)

First release. Nothing is published yet; the repository URL in the install
instructions is a placeholder (see `docs/deployment.md`).

### Grammar (`tree-sitter-gsql`)

- OPENCYPHER bodies: braces inside strings, backtick names and comments no longer
  end the body; `INSTALL QUERY` and `SHOW ...` accept the non-reserved command
  keywords (`clear`, `get`, `grant`, `put`, `use`, `show`, `abort`) as names.
- GSQL scripts end to end: schema DDL (vertex and edge types, graphs, tag-based
  graphs, tuples), schema change jobs (including global jobs), loading jobs,
  queries and shell commands (INSTALL/RUN/INTERPRET/SHOW/DROP with their options,
  users, roles, privileges including default and attribute privileges, data
  sources with triple-quoted JSON, workload queues, GRANT/REVOKE DATA_SOURCE,
  row policies, GET/PUT files, function installation).
- Queries: all accumulator types, including edge accumulators, `TYPEDEF`'d
  HeapAccums and Deviation accumulators; SELECT in classic, pattern-matching
  (multi-hop edge patterns, omitted vertices) and openCypher-style syntax
  (property maps); control flow; DML; virtual edges; map literals; PRINT, LOG,
  `PRINT ... WITH TAGS` and exceptions. The body of an `OPENCYPHER` query is kept
  as raw text and injected as openCypher.
- Keywords are case-insensitive. `ELSE IF` is read both ways (a clause with one
  END, or a nested IF with two), and long ELSE IF ladders parse.
- Highlight, locals, fold, indent, injection, tag and textobject queries; the
  indent queries agree with `gsql-lsp format`.
- Checked against the code examples of the GSQL 4.3 language reference and the
  TigerGraph Server documentation (`scripts/docs_examples.py`).

### Language server (`gsql-lsp`)

- Hover, completion and signature help documentation is sent as plain text (code fences, emphasis, headings, lists and links converted) when the client declares its supported formats and `markdown` is not one of them; clients that declare nothing keep Markdown.
- `textDocument/documentSymbol` returns the flat `SymbolInformation[]` form (with `containerName`) unless the client declares `hierarchicalDocumentSymbolSupport`.
- Diagnostics (every code is listed in `docs/diagnostics.md`, with severity and
  setting; a test keeps the page in step with the sources):
  - Syntax errors with explanations of common mistakes (missing `;` or `,`,
    `ELSEIF`, `POST ACCUM`, IF without END or THEN, WHILE/FOREACH without DO,
    trailing commas, unterminated strings, shell commands that continue on the
    next line).
  - Names: undeclared accumulators, duplicate declarations, undefined names,
    misspelled functions, methods and types, unused declarations (hint),
    reserved words used as names, `CREATE`s that repeat a name.
  - Schema (taken from the workspace): unknown vertex and edge types and
    attributes, including in loading jobs, schema change jobs and `ALTER`
    statements; VALUES lists of LOAD and INSERT statements checked against the
    attributes (`value-count`, `value-skip`, `endpoint-type`); a notice when the
    workspace has no schema.
  - Calls: query calls with the wrong number of arguments, unknown parameter names
    or arguments of the wrong type; built-in functions and methods with the wrong
    number of arguments.
  - Language rules: accumulators assigned, modified or declared where GSQL forbids
    it, invalid accumulator types and wrong case of their names, BREAK/CONTINUE
    outside loops, edge accumulators in POST-ACCUM, OFFSET without ORDER BY,
    virtual edge restrictions, features that interpreted or distributed queries do
    not support, openCypher patterns in `SYNTAX V1`/`V2` queries, `=`/`<>` outside
    `SYNTAX V3`, values of a wrong kind (`type-mismatch`), exact comparison of
    FLOAT/DOUBLE values and deprecated features; an accumulator read in the ACCUM
    clause that updates it (`accumulator-read-after-write`, the read sees the
    snapshot from before the clause) and an edge alias on a `*` repetition
    (`kleene-edge-alias`); see `docs/accumulator-semantics.md`.
  - MapAccum and GroupByAccum input (`accumulator-input`): what `+=` or `=` gives
    a MapAccum must be a `(key -> value)` pair, and a GroupByAccum pair must have
    as many keys and values as it declares. A tuple without `->`, a literal or
    scalar expression (bitwise operators, `IN`, `LIKE` and `IS NULL` included), a
    set, list, vertex or non-map accumulator, a pair with extra values, a nested
    MapAccum value that is not a pair, a tuple or list given to a `SumAccum` value,
    and a pair given to any other accumulator are errors; `(k - v)`, `(k > v)` and
    `(k >> v)` are reported as a mistyped `->` (not as arithmetic) with a quick
    fix. Keys and values of the wrong kind (numbers, strings, booleans, vertices,
    edges, collections, a literal for a tuple type), maps with keys of another
    kind, and wrong keys given to `get`, `containsKey` and `remove` are
    `type-mismatch` warnings, as is a value of the wrong kind added to any
    single-value or element accumulator (`OrAccum += "a"`, `ListAccum<STRING> +=
    1`); AvgAccum, Deviation and bitwise accumulators read as numbers, so
    `@@sum += @@bits` is not reported. Accumulator types with the wrong number or
    kind of type arguments (`MapAccum<STRING>`, `MapAccum @@m`, `SumAccum<INT,
    INT>`, `SumAccum<BOOL>`), GroupByAccum keys after its accumulators or duplicate
    field names (compared case-sensitively), and accumulators as parameters of a
    query without RETURNS (once per parameter) are `accumulator-type` errors;
    `AvgAccum<INT>` (with a quick fix that removes the argument),
    `BitwiseOrAccum<STRING>` and an accumulator parameter of a subquery are only
    warnings, as the documentation does not say a compiler rejects them. A FOREACH that binds the wrong number of
    variables (`FOREACH (k, v, w) IN @@map`, `FOREACH (k, v) IN @@set`) is a
    `foreach-variables` error.
  - `BUILTIN` declarations outside the generated reference file.
- Autocorrect: a misspelled keyword is found by parsing its statement again with
  similar keywords, and reported once with a quick fix instead of as the errors
  it causes; the statement is checked as corrected meanwhile. Misspelled shell
  commands and commands that swallow the next line are reported too.
- Quick fixes for most diagnostics, and "fix all" (`source.fixAll`) for the
  certain ones; it checks the fixed text again, so one run gets everything.
- Style hints from TigerGraph's GSQL Style Guide (`diagnostics.style`, on by
  default): `keyword-case` (a keyword or reserved word, `TRUE`, `FALSE` and `NULL`
  included, that is not in all caps) and `hash-comment` (a `#` comment; the guide
  writes `//`). Both are hints with a quick fix that "fix all" applies. The guide's
  indentation, 4 spaces per level and no tabs, is the formatter's default (the
  width the client sends still wins); `format.keywordCase = "upper"` lets the
  formatter write keywords in all caps too. A query parameter list or `TYPEDEF
  TUPLE` field list on a line longer than 80 characters, or already spanning
  lines, is laid out one item per line, the closing bracket on its own line
  (lists with comments or multi-line strings are left alone).
- Completion for statements and top-level commands, SELECT clauses, aliases and
  their attributes, accumulators and methods by type, vertex and edge types in
  FROM patterns (schema-aware, openCypher patterns included), loading and schema
  change job bodies, `RUN ... USING` file variables and more, also in unfinished
  code; hover, signature help (also in unfinished code and inside strings), go to
  definition and type definition, find references, document highlights, rename,
  call hierarchy, document links, document and workspace symbols, code actions,
  document and range formatting, folding and selection ranges, inlay hints
  (alias types, argument names, the attribute each VALUES entry fills) and
  semantic tokens (full and range; keywords, literals and comments with
  `semanticTokens.lexical`).
- Built-in reference: functions, methods, accumulators, types and keywords carry
  the documentation pages of the TigerGraph docs (version 4.3 data in
  `crates/gsql-lsp/data`, regenerated by `scripts/sync_builtin_docs.py`) in hover.
  Go to definition on a built-in opens a generated stub file
  (`BUILTIN FUNCTION abs(x) -> number;`, like a `.pyi`) in the platform's cache
  folder; its outline and folding work like any file's.
- Workspace: every `.gsql`/`.gsq` file below the workspace folders is indexed
  (files that are not UTF-8 lossily) and watched; a project root is the folder
  with a `.gsqlroot` file; a file outside every folder is checked against the
  schema next to it.
- Settings (`initializationOptions` and `workspace/didChangeConfiguration`, under
  `gsql`): `diagnostics.{unknownTypes, unknownAttributes, undefinedNames, unused,
  languageRules, floatEquality, noSchemaNotice, duplicateDefinitions, style}`,
  `format.keywordCase`, `inlayHints.enabled`, `semanticTokens.lexical`.
- Syntax errors that one token explains: a statement that parses without errors
  once a token near its first error is removed, or a `) ] } ; , = ( [` or
  `END`/`THEN`/`DO` is inserted after the token before, is reported as that one
  mistake (`unexpected x ...: the statement parses without it`, `missing ) after
  1`) with the quick fixes "Remove" or "Insert" (never part of "fix all"), instead
  of as the errors it causes. Candidates are checked against the parser's
  lookahead tables. Measured by `scripts/dev/check_repairs.py`; see
  `docs/error-recovery.md`.

### Command line

- `gsql-lsp check <path>...` reports diagnostics for files and directories as
  text, GitHub annotations (`--format github`) or JSON (`--format json`);
  `--errors-only`; exit code 1 for errors and 2 for unreadable paths (the other
  files are still checked). The schema is looked up like the server does.
- `gsql-lsp format [--check] [--keyword-case upper|lower|preserve] [--indent N]
  <path>...` re-indents files in place (4 spaces per level unless `--indent` says
  otherwise) and splits long parameter and tuple field lists, or stdin to stdout
  with `-`; files with syntax errors are left untouched.
- `gsql-lsp config <editor> [--variant NAME | --all] [--settings] [--absolute]
  [--repo URL]` prints ready-to-paste configuration for neovim, vscode, helix, zed,
  emacs and vim; `config --list` shows the variants.

### Editors and deployment

- Neovim (0.11+) plugin: filetype detection, `gsql_lsp` LSP config, queries, the
  parser build (`:GsqlBuildParser`, automatic in `setup()`), `:GsqlInstall` and
  `:GsqlUpdate` to download the server, `:checkhealth gsql`; installable from the
  repository root with lazy.nvim or `vim.pack`.
- VS Code extension (TextMate grammar, language configuration, LSP client, settings
  for the server options, a restart command) with platform-specific `.vsix` builds
  that bundle the server; Helix, Zed (downloads the release binary), Emacs
  (`gsql-ts-mode`, Eglot) and Vim configurations, each with a README.
- GSQL is indented by 4 spaces, as the GSQL Style Guide says, in every integration:
  the Neovim and Vim ftplugins, the VS Code `[gsql]` defaults, Helix, Zed and Emacs.
  In Neovim a project's `.editorconfig` for `*.gsql` wins.
- Release tooling, not yet exercised on a published release: GitHub workflow
  building static Linux (x64, arm64), macOS (Apple silicon, Intel) and Windows
  binaries and the VS Code extensions; `scripts/install.sh`; Homebrew formula
  generator; crates.io and `cargo binstall` metadata; drafts for Mason,
  nvim-lspconfig, nvim-treesitter, Helix and Zed registries
  (`packaging/upstream`); `scripts/set_version.py`.
- CI: grammar corpus and highlight tests, Rust tests on three platforms, editor
  integration tests, packaging checks.

### Robustness

- Panics in a request or notification are caught and answered with an error; `check`
  survives a file that fails. Invalid JSON-RPC messages are answered.
- Documents are synchronized incrementally. A tree with syntax errors of up to
  256 KiB is parsed again from scratch, so edits give the same diagnostics as
  opening the file fresh.
- The server and indexer run on threads with a 256 MiB stack: 100000 nested
  parentheses, 20000 nested type arguments and 5000 nested IFs are diagnosed in
  well under a second and requests keep answering
  (`scripts/dev/check_deep_nesting.py`).
- Large and machine-made files stay usable: a 1 MB file full of syntax errors is
  checked in a second or two in a release build.
- Developer regression scripts live in `scripts/dev`; `tests/stress.rs` applies
  random edits and runs every feature on the result, `tests/formatting.rs` checks
  that formatting is idempotent and only changes layout.
