# gsql-lsp

Language tooling for [TigerGraph](https://www.tigergraph.com/) **GSQL**:

- **`tree-sitter-gsql/`** – a [tree-sitter](https://tree-sitter.github.io/) grammar
  with highlight, locals, fold, indent, injection, tag and textobject queries.
- **`crates/gsql-lsp/`** – a Language Server Protocol implementation in Rust, built
  on the grammar.
- **`editors/`** – setup for Neovim, VS Code, Helix, Zed, Emacs and Vim.

## Features

The grammar covers GSQL scripts end to end: schema DDL (vertices, edges, graphs,
tuples, tag-based graphs), schema change jobs, loading jobs, queries (all accumulator
types including edge accumulators and TYPEDEF'd heaps, SELECT blocks in classic,
pattern-matching and openCypher-style syntax with multi-hop edge patterns, virtual
edges, control flow, DML, PRINT/LOG/exceptions) and shell commands
(INSTALL/RUN/SHOW/DROP with their options, users, roles, privileges, data sources,
workload queues). Keywords are case-insensitive, as in GSQL. It was checked against
the code examples of the GSQL 4.3 language reference and of the TigerGraph Server
documentation; the examples it rejects are output samples, syntax templates, code in
other languages or contain mistakes.

The language server provides:

| Feature | Details |
| --- | --- |
| Diagnostics | Syntax errors, with explanations of common mistakes; undeclared global (`@@`) or vertex-attached (`@`) accumulators; duplicate declarations; unknown vertex/edge types and attributes (checked against the schema found in the workspace); VALUES lists of LOAD and INSERT statements that do not match the type's attributes; undefined names and misspelled function or type names; unused accumulators and variables; query calls with the wrong number of arguments, unknown parameter names or literals of the wrong type; `=` and `<>` comparisons outside `SYNTAX V3` queries; rules from the language reference: reserved words used as names, accumulators assigned, modified or declared where GSQL forbids it, misplaced virtual edges, invalid accumulator types, OFFSET without ORDER BY, features that interpreted or distributed queries do not support, exact comparison of FLOAT/DOUBLE values, BREAK/CONTINUE outside loops, openCypher patterns in `SYNTAX V1`/`V2` queries and deprecated features; values of a wrong kind (a string added to a numeric accumulator, a number compared with a string); repeated `CREATE` of a name; hints for what the [GSQL Style Guide](https://www.tigergraph.com/docs/gsql-ref/4.3/appendix/gsql-style-guide) advises against (keywords not in all caps, `#` comments). Every code is explained in [docs/diagnostics.md](docs/diagnostics.md) |
| Autocorrect | Misspelled keywords are found by parsing the statement again with similar keywords in their place, so a typo is reported once, as `did you mean FROM instead of FORM?`, with a quick fix, instead of as the errors it causes (even when it derails the whole file or makes a shell command swallow the next lines). The rest of the statement is checked as corrected meanwhile, so a typo in `CREATE VERTEX` does not make every use of the type "unknown". A statement that parses once one token is removed or inserted (a stray word, a missing `)`, `]`, `}` or `;`) is reported as that one mistake. How the search works and how well it does: [docs/error-recovery.md](docs/error-recovery.md) |
| Completion | Context-aware: attributes, accumulators and methods after `alias.`; accumulator methods by accumulator type; vertex types and vertex sets in FROM patterns; edge types inside `-( )-` that touch the source vertex in the written direction, and the far-end vertex types after an edge; the same in openCypher patterns (`(s:|)`, `-[e:|]->`, `(t:A|B|)`) plus attribute keys inside `{..}` property maps; aliases as soon as a WHERE/ACCUM expression starts, even in unfinished clauses; the SELECT clauses that may still follow (WHERE, ACCUM, ...); loop variables typed from the collection; attribute names in `DROP ATTRIBUTE`, `ADD INDEX ... ON` and `INSERT INTO` column lists; graphs, queries, jobs; types; keywords and snippets for statements and top-level commands; the statements of loading and schema change jobs, `RUN LOADING JOB ... USING` file variables, the types and endpoints in schema definitions, `ASC`/`DESC` after `ORDER BY` |
| Hover | Declarations with their doc comments, vertex/edge types with attributes, query signatures, built-in functions, methods of accumulators and SET/LIST/MAP values, accumulator types, keywords, and the attribute a value of a VALUES list loads into |
| Navigation | Go to definition (vertex and edge types, attributes and `ACCUM` variables in the schema files; across files and into `@file.gsql` includes; built-in functions, methods, accumulators, types and keywords open their declaration in a generated stub file (`BUILTIN FUNCTION abs(x) -> number;` with its documentation, like a Python `.pyi`) in the cache folder), type definition, find references, document highlights, rename (accumulators keep their `@`/`@@`), file variables of `RUN LOADING JOB job USING f1=...` (they resolve to the `DEFINE FILENAME` of that job in any file, and rename with it), `$"col"` of `LOAD TEMP_TABLE t` to its column, call hierarchy between queries, document outline, workspace symbols, links to included scripts and local data files |
| Editing | Signature help, formatting (also of a selected range; re-indents by block structure, 4 spaces per level as the GSQL Style Guide says, one parameter or tuple field per line in long lists, keeps hand-aligned continuation lines, optional keyword case), folding ranges, selection ranges, inlay hints (inferred alias types, query argument names, the attribute of each value in VALUES lists) |
| Quick fixes | Correct a misspelled keyword, command, function, type, attribute, parameter or name; insert a missing `;`, `,`, `)`, `]`, `}`, `THEN`, `DO` or `END;` (before the closing `}` of the body); remove a stray token; write `ELSE IF`, `POST-ACCUM` and `DEFAULT` the GSQL way; remove a trailing comma; declare an undeclared accumulator with a type guessed from its use; replace `=`/`<>` with `==`/`!=` or declare the query `SYNTAX V3`; compare floating-point values with a tolerance; rename a reserved word; remove an unused declaration; fix the case of accumulator type names; write keywords in all caps and `//` for `#` comments. "Fix all" (`source.fixAll`) applies the fixes that are certain, e.g. on save; it checks the fixed text again, so one run gets everything that repeated runs would. Mistakes in a statement that also contains a misspelled keyword wait until the keyword is fixed |
| Semantic tokens | Distinguishes vertex types, edge types, parameters, global and vertex-attached accumulators, attributes and built-ins |

The server indexes every `.gsql`/`.gsq` file in the workspace, so a schema defined
in `schema.gsql` is known when editing queries in other files. Documents are
synchronized incrementally, and a panic in one request is answered with an error
instead of ending the server.

Client capabilities are honoured: Markdown is the default for hover, completion and
signature documentation, and plain text is sent when the client declares the format
lists and `markdown` is not among them; the outline is a `DocumentSymbol[]` tree only
for clients that declare `hierarchicalDocumentSymbolSupport` (all real editors do) and
a flat `SymbolInformation[]` list otherwise; snippets are offered only when the client
declares `snippetSupport`.

## Installing the server

Each GitHub release carries prebuilt binaries for Linux (x64 and arm64, statically
linked), macOS (Apple silicon and Intel) and Windows (x64), plus VS Code
extensions with the server bundled. To install a release:

```sh
curl -fsSL https://raw.githubusercontent.com/gsql-lsp/gsql-lsp/main/scripts/install.sh | sh   # to ~/.local/bin
```

or `cargo binstall gsql-lsp` / `brew install` (once published; see
[docs/deployment.md](docs/deployment.md)). Neovim users can skip this: `:GsqlInstall`
downloads the server. To build from a checkout instead:

```sh
cargo install --path crates/gsql-lsp
gsql-lsp --version
```

`gsql-lsp config <editor>` prints a ready-to-paste configuration for neovim, vscode,
helix, zed, emacs or vim (`gsql-lsp config --list`, `--settings` for the server
settings and their defaults).

`gsql-lsp` speaks LSP over stdin/stdout. It also has a command-line checker and
formatter, handy in CI:

```sh
gsql-lsp check path/to/project      # prints file:line:col: severity: message [code]
gsql-lsp check --errors-only a.gsql b.gsql
gsql-lsp check --format github .    # annotations in GitHub Actions (also: --format json)
gsql-lsp format path/to/project     # re-indents files in place
gsql-lsp format --check .           # lists files that are not formatted (exit code 1)
gsql-lsp format --keyword-case upper - < in.gsql   # stdin to stdout (--indent N: spaces per level, default 4)
```

The formatter changes indentation (4 spaces per level, as the GSQL Style Guide
says), trailing whitespace and (optionally) keyword case, and puts the parameters of
a query and the fields of a `TYPEDEF TUPLE` one per line when the line holding the list
is longer than 80 characters or the list already spans lines (the guide splits long
lines); it keeps
hand-aligned continuation lines and leaves files with syntax errors untouched.

Exit codes of both commands: 0 when clean, 1 when there are errors (`check`) or files
that are not formatted (`format`), 2 when a path could not be read (reported on stderr;
the other files are still processed). `gsql-lsp check a.gsql` looks for the schema the
way the server does for a file outside every workspace folder: under the nearest folder
above the file with a `.gsqlroot` file, else in the file's own folder. A directory
argument below a `.gsqlroot` uses that project's schema too, but only the files under
the directory are reported. Files that are not valid UTF-8 are read with the bad bytes
replaced, so they are still indexed. Symbolic links to folders inside the workspace are
followed (each real folder is indexed once, so loops and links to a parent are harmless).

## Editor setup

### Neovim (0.11+)

The plugin (filetype detection, the `gsql_lsp` LSP config, tree-sitter queries, the
server installer) is in `editors/neovim`; a shim at the repository root lets a plain
spec install it. With [lazy.nvim](https://github.com/folke/lazy.nvim) (in LazyVim,
a file in `lua/plugins/`):

```lua
{
  'gsql-lsp/gsql-lsp',
  build = function() require('gsql').install_sync() end,  -- downloads the server
  opts = {},  -- require('gsql').setup(opts); { cmd = { '/path/to/gsql-lsp' } } to use your own binary
}
```

More in [editors/neovim/README.md](editors/neovim/README.md) (`vim.pack`, manual
install, every option).

`setup()` also compiles the parser by itself when it is missing or older than the
grammar. The schema is searched for under the project root: the folder with a
`.gsqlroot` file, else the git root, else the nearest folder with a `README.md`
(so a project that is not in git works too). If hover and go to definition do not
find your types, check `:lua =vim.lsp.get_clients({name='gsql_lsp'})[1].config.root_dir`.

Compile the parser once with `:GsqlBuildParser` (needs a C compiler), or register it
with nvim-treesitter (`setup()` does this when nvim-treesitter is installed) and run
`:TSInstall gsql`. Folding: `vim.wo.foldexpr = 'v:lua.vim.treesitter.foldexpr()'`.
`:checkhealth gsql` shows whether the server and the parser are found.

Without the plugin, Neovim has no `.gsql` filetype, so register it as well (verified: the
client attaches to a `.gsql` buffer). This is enough for the language server
(`gsql-lsp config neovim --variant lsp-config` prints it with the settings):

```lua
vim.filetype.add({ extension = { gsql = 'gsql', gsq = 'gsql' } })
vim.lsp.config('gsql_lsp', {
  cmd = { 'gsql-lsp' },
  filetypes = { 'gsql' },
  root_markers = { { '.gsqlroot' }, { '.git' }, { 'README.md', 'README' } },
})
vim.lsp.enable('gsql_lsp')
```

To apply all the certain fixes at once, map the `source.fixAll` code action:
`vim.keymap.set('n', '<leader>cf', function() vim.lsp.buf.code_action({ context = { only = { 'source.fixAll' } }, apply = true }) end)`.

### VS Code

`editors/vscode` contains the extension (TextMate grammar, language configuration
and LSP client). Install the `.vsix` for your platform from a release (it bundles
the server), or build one with `npm install && npx vsce package` and point
`gsql.server.path` at a `gsql-lsp` binary (`~` and `${workspaceFolder}` are expanded). See its [README](editors/vscode/README.md)
for settings. To apply the certain fixes on save, add
`"editor.codeActionsOnSave": { "source.fixAll": "explicit" }` to the settings for
`[gsql]`.

### Helix

Merge [`editors/helix/languages.toml`](editors/helix/languages.toml) into
`~/.config/helix/languages.toml`, copy `editors/helix/queries/gsql` to
`~/.config/helix/runtime/queries/gsql`, then run `hx --grammar fetch && hx --grammar build`.
Helix roots the workspace at the topmost folder with a `.gsqlroot` file or a `.git`
folder; without either it uses the folder Helix was started in, so put a `.gsqlroot`
file in the project folder (a `README.md` is not a reliable marker there).

### Zed

`editors/zed` is a Zed extension (grammar, queries and the language server). Set
`rev` in `extension.toml` to a commit of this repository, then use
*zed: install dev extension* on that folder. It runs `gsql-lsp` from your `PATH`.
Settings go in Zed's `settings.json` under `lsp.gsql-lsp.settings` (or
`initialization_options`), in the shape described under Configuration; the `gsql`
wrapper key is optional:

```json
{ "lsp": { "gsql-lsp": { "settings": { "diagnostics": { "floatEquality": false } } } } }
```

### Emacs (29+)

```elisp
(load "/path/to/gsql-lsp/editors/emacs/gsql-ts-mode.el")
;; once: M-x treesit-install-language-grammar RET gsql RET
```

`gsql-ts-mode` provides font-lock, indentation and imenu, and registers `gsql-lsp`
with Eglot (`M-x eglot`). Eglot sends `eglot-workspace-configuration` as
`workspace/didChangeConfiguration`, so settings go there (for example in `.dir-locals.el`
or `setq-default`):

```elisp
(setq-default eglot-workspace-configuration
              '(:gsql (:diagnostics (:floatEquality :json-false)
                       :format (:keywordCase "upper"))))
```

### Vim

Add `editors/vim` to `runtimepath` for regex-based highlighting; use any LSP client
(vim-lsp, coc.nvim, ALE) with the command `gsql-lsp`.

### Other editors

Any LSP client works: run `gsql-lsp` for files with the `gsql` language id. The
TextMate grammar in `editors/vscode/syntaxes` also works in Sublime Text and other
TextMate-compatible editors.

## Configuration

Settings are read from `initializationOptions` and from
`workspace/didChangeConfiguration` (under a `gsql` key):

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
      "noSchemaNotice": true,
      "duplicateDefinitions": true,
      "style": true
    },
    "format": { "keywordCase": "preserve" },
    "inlayHints": { "enabled": true },
    "semanticTokens": { "lexical": false }
  }
}
```

- `diagnostics.unknownTypes`/`unknownAttributes` only apply when the workspace
  declares at least one vertex type, so projects whose schema lives only in the
  database do not get spurious warnings.
- `diagnostics.undefinedNames`: undefined names and misspelled function, method and
  type names; `diagnostics.unused`: the hints for unused accumulators and variables.
- `diagnostics.languageRules`: the checks based on rules stated in the GSQL
  language reference (see the Diagnostics row above).
- `diagnostics.noSchemaNotice`: a warning ("No schema found ...") once per file, on the name of the first
  query or loading job that uses vertex or edge types, when the workspace has no schema, because the
  checks of types and attributes are off then and silence would look like "all fine".
  A file outside every workspace folder is checked against the `.gsql` files in its
  own folder (not its subfolders).
- `diagnostics.floatEquality`: the exact-equality warning for FLOAT/DOUBLE values,
  which can be turned off on its own. Comparisons with whole numbers (`x == 0`)
  and the whole-number test `x == float_to_int(x)` are never reported.
- `diagnostics.duplicateDefinitions`: the `duplicate-definition` warning and hint (a repeated
  `CREATE` of a name, in the same file or another one).
- `diagnostics.style`: the hints at what the [GSQL Style Guide](https://www.tigergraph.com/docs/gsql-ref/4.3/appendix/gsql-style-guide)
  advises against: `keyword-case` (keywords and reserved words that are not in all caps) and
  `hash-comment` (`#` comments; write `//`). Each has a quick fix that "fix all" applies. The
  guide's indentation, 4 spaces per level and no tabs, is what the formatter does when the
  editor asks for it, which the editor integrations in this repository do by default.
- `format.keywordCase`: `preserve`, `upper` or `lower`. The style guide writes keywords in
  all caps, so `upper` makes the formatter do what the `keyword-case` hint asks for.
- `inlayHints.enabled`: alias types, query argument names and the attributes of VALUES.
- `semanticTokens.lexical`: also send tokens for keywords, literals, comments and
  operators, for clients without other highlighting.

## Repository layout

```
tree-sitter-gsql/        grammar.js, generated parser (src/), queries/, corpus and highlight tests
crates/gsql-lsp/         the language server (src/), stdio integration tests (tests/)
editors/neovim/          Neovim plugin and its end-to-end test
editors/vscode/          VS Code extension, TextMate grammar and its tokenizer test
editors/helix/           languages.toml and Helix queries
editors/zed/             Zed extension
editors/emacs/           gsql-ts-mode.el
editors/vim/             Vim syntax, ftdetect and ftplugin
scripts/                 installer, query sync, built-in docs, highlight-test and release helpers; dev/ has the LSP regression checks
docs/                    diagnostics.md, deployment.md
packaging/               Homebrew formula and upstream registry drafts
examples/                sample GSQL projects
```

## Development

```sh
make generate      # regenerate tree-sitter-gsql/src from grammar.js (tree-sitter 0.27, no Node.js needed)
make test          # grammar corpus/highlight tests, Rust unit and stdio tests, query sync check
make editor-test   # Neovim end-to-end, TextMate tokenizer and Vim syntax tests
make packaging-test  # installer, Homebrew formula and crate packaging, offline
make queries       # after editing tree-sitter-gsql/queries: regenerate Neovim/Helix/Zed copies
make lint          # rustfmt and clippy
```

The editor query files under `editors/` are generated: edit
`tree-sitter-gsql/queries/*.scm` and run `make queries`.

Robustness checks: `tree-sitter fuzz` (in `tree-sitter-gsql/`) fuzzes incremental
parsing, `crates/gsql-lsp/tests/stress.rs` applies random edits and runs every
feature on the result (`GSQL_STRESS_ITERATIONS=20000 cargo test --release --test stress`),
and `crates/gsql-lsp/tests/formatting.rs` checks that formatting is idempotent and
only changes layout.

`make docs-examples` (`scripts/docs_examples.py`) downloads the GSQL language
reference from tigergraph.com, parses every GSQL example in it and writes the ones
that fail to `docs-examples-failing.json`; run it when a new version of the
reference is published (`scripts/docs_examples.py --version 4.4`). With `--site
tigergraph-server` it checks the server documentation instead, which has more shell,
admin and loading commands.

### Releasing

```sh
scripts/set_version.py 0.2.0      # updates every manifest and Cargo.lock
git commit -am "Release 0.2.0" && git tag v0.2.0 && git push --follow-tags
```

Pushing the tag runs `.github/workflows/release.yml`, which builds the binaries and
the platform-specific VS Code extensions and publishes them as a GitHub release. Highlight tests live in
`tree-sitter-gsql/test/highlight`; `scripts/highlight_test.py` writes them from the
specs in `test/highlight-specs` with the assertion columns computed.

## Limitations

- The grammar follows the documented GSQL syntax and is deliberately a little more
  permissive than TigerGraph, so some invalid programs parse without errors.
- The bodies of `OPENCYPHER` queries are not parsed; they are handed to a Cypher
  grammar through an injection when the editor has one.
- `ELSE IF` is read both ways, as GSQL does: a clause of the same IF (one END) or
  an ELSE branch that starts with a nested IF (two ENDs). The ENDs decide. Written
  on one line, `ELSE IF` is always a clause, so long ladders parse quickly; the
  extra ENDs of the nested reading are accepted after an ELSE IF chain (`ELSE IF` can
  also be read as an ELSE branch holding an IF closed by its own END), so a surplus END
  there is not reported.
- Type inference is local and best effort: attribute checks and completions are
  precise when an alias' vertex type is known (from a pattern, a typed vertex set or
  a seed such as `{Person.*}`), and fall back to all types otherwise.
- A document with syntax errors is parsed afresh after each edit only up to 256 KiB (a
  fresh parse recovers from errors better than an incremental one); a larger document
  keeps the incremental tree, so its error messages can differ slightly until it is
  error-free.
- The schema is read from the files of the workspace only; there is no live
  introspection of a database and no setting for schema folders outside the project.
- The workspace scan is bounded: at most 10,000 GSQL files (files that declare schema
  objects, then those nearest the root, are kept), 50,000 folders, and files up to 8 MB.
  When a limit is hit, one warning is logged (`window/logMessage`) naming it; open a
  smaller folder to index everything.
- The VS Code, Zed and Emacs integrations have not been run in those editors by the
  test suite; their grammars and queries are validated against the parser.

## License

[MIT](LICENSE)
