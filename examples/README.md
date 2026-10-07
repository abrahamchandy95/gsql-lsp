# Examples

Small GSQL projects used for documentation and as a regression suite (the test
`crates/gsql-lsp/tests/examples.rs` requires them to analyze without errors or
warnings).

| Directory | Contents |
| --- | --- |
| `social/` | Social network schema, loading job and queries (classic and pattern-matching SELECT, HeapAccum, SQL-like SELECT INTO) |
| `finance/` | Transaction schema (tuple attribute, composite key, DISCRIMINATOR, multi-pair edge) and fraud queries (Kleene star, GroupByAccum, exceptions, file output, INSERT/UPDATE/DELETE) |
| `algorithms/` | PageRank, shortest paths, connected components, label propagation, Jaccard similarity, clustering coefficient, k-hop and triangle counting |
| `admin/` | Schema change jobs, users and roles, installing and running queries, packages, openCypher queries |

Open this folder in an editor with gsql-lsp to try completion, hover and navigation:
the queries in `social/` and `finance/` resolve their vertex and edge types from the
schema files next to them.
