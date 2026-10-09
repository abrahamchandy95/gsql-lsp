# gsql-lsp

Language server for TigerGraph GSQL. Minimal dependencies: `tree-sitter`, `tree-sitter-gsql`, and `serde`/`serde_json` (JSON-RPC transport and LSP types are implemented in `src/lsp`).

## How It Works

1. **Documents** (`document.rs`, `text.rs`): Tracks text buffers and syntax trees with incremental Tree-sitter parsing; converts positions between byte offsets and LSP coordinates (UTF-8/16/32).
2. **Analysis** (`analysis/`): Two-pass semantic model: collects declarations and scopes (queries, loops, jobs), resolves identifier roles, and infers types (aliases, sets, loop variables).
3. **Workspace Index** (`workspace.rs`): Indexes global schema declarations and cross-file references. Open files are kept in memory; disk files are scanned in the background and refreshed on file-watch events.
4. **Features** (`features/`): Serves LSP requests from an immutable `Snapshot`. `resolve.rs` maps identifiers to local, workspace, or built-in symbols for hover, navigation, rename, and semantic tokens.
5. **Server** (`server.rs`): Event loop coordinating stdin decoding, background indexing, and diagnostic publishing.

`builtins.rs` provides signatures and docs for keywords, types, accumulators, and built-in functions.

## Testing

- **Unit & Feature Tests**: `cargo test` (uses `|` cursor fixtures via `features::test_support`).
- **Stdio Integration**: `tests/stdio.rs` tests the binary directly over stdio.
- **Validation**: `tests/examples.rs` ensures `examples/` analyze cleanly and `tests/fixtures/` parse without syntax errors.
- **Benchmark**: `cargo run --release --example bench -- file.gsql` times each pipeline phase.
