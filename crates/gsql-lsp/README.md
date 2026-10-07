# gsql-lsp (server crate)

The language server for TigerGraph GSQL. It depends only on `tree-sitter`, the
`tree-sitter-gsql` grammar and `serde`/`serde_json`; the JSON-RPC transport and
the protocol types it needs are implemented in `src/lsp`.

## How it works

1. **Documents** (`document.rs`) keep the text and the syntax tree. Edits from
   `textDocument/didChange` are applied incrementally and the tree is reparsed
   with tree-sitter's incremental parsing. `text.rs` converts between byte
   offsets, tree-sitter points and LSP positions (UTF-8, UTF-16 or UTF-32, as
   negotiated in `initialize`).
2. **Analysis** (`analysis/`) turns a tree into a semantic model in two passes:
   declarations and scopes first (queries, SELECT blocks, FOREACH loops, loading
   jobs), then every identifier is classified by its syntactic *role* (vertex
   type, edge type, attribute of `x`, accumulator, value, ...) and resolved
   against the scope chain. Types are inferred where cheap: alias types from FROM
   patterns, vertex-set element types from seeds and SELECT results, loop
   variables from the collection they iterate.
3. **Workspace index** (`workspace.rs`) holds the workspace-level declarations
   (vertex and edge types with their attributes, graphs, queries, jobs, tuples)
   and the references to them for every `.gsql` file. Open documents are indexed
   from memory; other files are scanned from disk in a background thread at
   startup and re-read on `workspace/didChangeWatchedFiles`.
4. **Features** (`features/`) answer requests from a `Snapshot` (one document
   plus the workspace index). `features/resolve.rs` maps an identifier to what it
   denotes – a local symbol, a workspace declaration or a built-in – and is shared
   by hover, navigation, rename and semantic tokens.
5. **Server** (`server.rs`) runs the message loop: a reader thread decodes
   messages from stdin, the indexer thread reports scanned files, and the main
   thread handles both in order. Diagnostics are pushed after every change, and
   for all open documents when workspace-level declarations change.

`builtins.rs` holds the documentation and signatures of keywords, types,
accumulators, built-in functions and methods.

## Testing

- Unit tests live next to the code (`cargo test`); feature tests build fixtures
  from source text with a `|` cursor marker (`features::test_support`).
- `tests/stdio.rs` drives the binary over stdio like an editor would.
- `tests/examples.rs` requires `examples/` to analyze cleanly and
  `tests/fixtures/` to parse without syntax errors.
- `cargo run --release --example bench -- file.gsql` times each phase.
