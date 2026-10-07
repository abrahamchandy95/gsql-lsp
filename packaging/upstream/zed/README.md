# Zed: zed-industries/extensions

Nothing is drafted as a file here: the submission is a submodule plus one
`extensions.toml` entry, both made in a fork of the registry. Everything below
comes from memory of the Zed extension docs and is **to verify** against
https://zed.dev/docs/extensions/developing-extensions and the registry's
README before submitting. No Zed, no wasm toolchain and no network were
available when this was written.

## What the registry needs (to verify)

- A public git repository that contains the extension, added as a git
  submodule under `extensions/<id>` using an HTTPS URL.
- An entry in `extensions.toml`:
  ```toml
  [gsql]
  submodule = "extensions/gsql"
  path = "editors/zed"   # the extension lives in a subdirectory of this repository
  version = "0.1.0"      # must equal `version` in extension.toml
  ```
- `extension.toml` with `id`, `name`, `version`, `schema_version`, `authors`,
  `description`, `repository`. The `id` must not contain `zed` or `extension`
  and should be unique.
- A license file in the extension directory (an OSI-approved license from the
  accepted list; MIT qualifies) -- to verify whether it must sit next to
  `extension.toml`.
- Grammars referenced from `[grammars.<name>]` must give an HTTPS `repository`
  and a full commit SHA in `rev` (a branch name is rejected); `path` selects a
  subdirectory of a monorepo.
- Sort order and formatting of `extensions.toml` are checked in CI
  (`pnpm sort-extensions`, to verify).
- Language servers that download binaries must do so from the extension's wasm
  code (`zed_extension_api`), which is what `src/lib.rs` does.

## Our `editors/zed/extension.toml` against that list

- Present: `id = "gsql"`, `name`, `version`, `schema_version = 1`, `authors`,
  `repository`, `[grammars.gsql]` with `repository`, `rev`, `path`, and
  `[language_servers.gsql-lsp]`.
- `rev` is the all-zero placeholder and the repository URL is the placeholder
  URL: both must be set to real values before submission (the comment in the
  file says so).
- There is no `editors/zed/LICENSE`. The repository root has an MIT `LICENSE`;
  if the registry wants the file beside `extension.toml`, copy or symlink it
  (not done here: `editors/zed` belongs to another task).
- `version` in `extension.toml` (0.1.0) must be bumped together with the
  `extensions.toml` entry on every update; Zed does not read the release tags
  of this repository.
- `editors/zed/Cargo.toml` declares `license = "MIT"`; the crate is built by
  Zed itself (wasm32-wasip2 / wasip1 depending on the Zed version, to verify).

## Steps

1. Replace the placeholder URL everywhere it appears in `editors/zed`.
2. Pin `rev` to a commit that contains `tree-sitter-gsql/src/parser.c` and the
   queries copied into `editors/zed/languages/gsql` (`python3 scripts/sync_queries.py`).
3. Test locally: in Zed run `zed: install dev extension` and select `editors/zed`
   (to verify; this also builds the wasm).
4. Fork `zed-industries/extensions`, `git submodule add <repo-url> extensions/gsql`,
   add the entry above, run the registry's sort script, open a PR.
