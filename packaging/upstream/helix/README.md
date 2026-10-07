# Helix: helix-editor/helix

`languages.toml.fragment` holds the `[[language]]`, `[[grammar]]` and
`[language-server.gsql-lsp]` blocks to append to Helix's `languages.toml`.
It is the same as `editors/helix/languages.toml` (the user-facing copy) except:

- `injection-regex` is added (same value as `tree-sitter-gsql/tree-sitter.json`);
- the `[language-server.gsql-lsp.config.gsql]` defaults are left out (they
  repeat the server's own defaults);
- `rev` is a placeholder commit SHA instead of `main`.

Checked locally: the blocks differ from `editors/helix/languages.toml` only in
those three points (diff of the non-comment lines). Not checked: that Helix
accepts the file (no Helix binary here).

## What a PR needs (from memory, **to verify** against Helix's `book/src/guides/adding_languages.md` and CONTRIBUTING.md)

1. The blocks above in `languages.toml`, keeping the file's ordering
   conventions (languages are sorted by name).
2. Queries in `runtime/queries/gsql/`: `highlights.scm` (required for a useful
   result), plus `indents.scm`, `injections.scm`, `locals.scm` and
   `textobjects.scm` if available. They use Helix capture names; generate them
   with `python3 scripts/sync_queries.py`, which writes
   `editors/helix/queries/gsql/`, and copy that directory.
3. `rev` pinned to a full commit SHA that contains `tree-sitter-gsql/src/parser.c`.
4. Regenerate the generated language-support table with `cargo xtask docgen`
   (updates `book/src/generated/lang-support.md`).
5. Check with `hx --grammar fetch && hx --grammar build`, then `hx --health gsql`.
6. Maintainers may ask for the grammar to live in its own repository rather
   than a `subpath` of a monorepo; `subpath` support itself is in Helix's
   grammar fetcher (to verify for the Helix version targeted).
7. Helix generally wants the language to have an established user base
   before merging a new built-in language (to verify; no stated threshold
   recalled).
