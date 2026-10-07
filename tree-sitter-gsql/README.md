# tree-sitter-gsql

A [tree-sitter](https://tree-sitter.github.io/) grammar for TigerGraph **GSQL**:
schema DDL, schema change and loading jobs, queries and shell commands.

```gsql
CREATE QUERY friends(VERTEX<Person> p, INT k = 3) FOR GRAPH Social {
    SumAccum<INT> @@count;
    Start = {p};
    Result = SELECT t FROM Start:s -(Friendship:e)- Person:t
        WHERE t.age > 18
        ACCUM @@count += 1;
    PRINT Result[Result.name AS name], @@count;
}
```

Keywords are case-insensitive and appear in the tree as anonymous nodes named after
their upper-case spelling (`select` → `"SELECT"`), so queries match them regardless of
how they are written. Keywords are contextual: `type`, `count` or `limit` remain
valid identifiers where GSQL allows them.

## Queries

| File | Purpose |
| --- | --- |
| `queries/highlights.scm` | Highlighting (nvim-treesitter capture names; later patterns take precedence) |
| `queries/locals.scm` | Scopes, definitions and references |
| `queries/folds.scm` | Folding |
| `queries/indents.scm` | Indentation (nvim-treesitter captures) |
| `queries/injections.scm` | openCypher in `OPENCYPHER` query bodies, `comment` |
| `queries/tags.scm` | Code navigation tags |
| `queries/textobjects.scm` | nvim-treesitter-textobjects |

Helix and Zed variants are generated into `../editors` by `../scripts/sync_queries.py`.

## Development

```sh
tree-sitter generate --js-runtime native   # or plain `tree-sitter generate` with Node.js
tree-sitter test                           # corpus (test/corpus) and highlight tests (test/highlight)
tree-sitter parse file.gsql
```

The node types are documented in `src/node-types.json`.
