# Accumulator Semantics & Diagnostic Rules

`gsql-lsp` enforces static checks derived from the formal semantics of GSQL and TigerGraph documentation.

---

## Core Semantic Principles

1. **Bag Semantics:** `ACCUM` executes once per row in the binding table (counting paths, not distinct vertices).
2. **Snapshot Isolation:** All statements in an `ACCUM` clause read accumulator values from the start of the clause. Updates are aggregated after all rows execute.
3. **Order Invariance:** Reducers must be associative and commutative. `ListAccum`, `ArrayAccum`, and `SumAccum<STRING>` are non-deterministic by design.
4. **POST-ACCUM Execution:** Runs after `ACCUM` finishes. Statements execute sequentially and can read preceding updates within the clause.
5. **Kleene Star Bindings:** Repeating edges (`*`, `*m..n`) generate multiple bindings per path.

---

## Enforced Rules

### `accumulator-read-after-write` (Warning / Hint)

Warns when an accumulator is updated (`+=` or `=`) and then read later in the same `ACCUM` block, as the read still observes the pre-clause snapshot value.

Ignored intentional idioms:

- Read-before-write (`IF NOT t.@visited THEN t.@visited += TRUE ... END`).
- Read within the write expression (`s.@x += s.@x`).
- Writes within alternative `IF`/`ELSE` branches.
- Any read/write inside `POST-ACCUM`.

### `kleene-edge-alias` (Error)

Rejects aliases on variable-length edge patterns (`-(E>*1..3:e)-`). Kleene repetitions yield multiple edge bindings per path and cannot be bound to a single edge variable.

---

## Intentionally Omitted Diagnostics

- Order-dependent accumulators: `ListAccum` / `ArrayAccum` non-determinism is accepted behavior.
- `WHERE` reading updated accumulators: Standard for BFS traversal (`WHERE NOT t.@visited ACCUM t.@visited += TRUE`).
- `=` writes in `ACCUM`: Often an intentional non-deterministic selection (e.g., BFS parent tracking `t.@parent = s`).
- Tractable-class queries: Intractable queries (e.g., unbounded Kleene stars) remain valid GSQL.
- POST-ACCUM sequential reads: Valid by design (e.g., PageRank implementations)
