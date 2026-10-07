# GSQL for Zed

Dev extension: Zed menu, `zed: install dev extension`, pick `editors/zed`
(needs a Rust toolchain with the `wasm32-wasip1` target for Zed to build it).

The server is found in this order:

1. `lsp.gsql-lsp.binary.path` in Zed settings.
2. `gsql-lsp` on the worktree's PATH.
3. Download of the latest GitHub release asset `gsql-lsp-<target>.tar.gz` (`.zip` on
   Windows), cached per version in the extension's working directory; older versions
   are removed.

The repository used for the download is the `REPOSITORY` constant in
`src/lib.rs`; the grammar repository and `rev` are in `extension.toml`. Both are
placeholders until the project is published.

Status: `src/lib.rs` is written against the `zed_extension_api` 0.6 documentation
(no crate source or wasm32 toolchain available offline); it has not been compiled.
