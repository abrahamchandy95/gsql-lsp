# Accumulator semantics and the rules derived from them

gsql-lsp checks a few accumulator and pattern rules that follow from the
semantics of GSQL as defined in the paper on its formal semantics (PDF copy in
`docs/papers/`). Page numbers below are PDF pages; the SIGMOD page is the PDF
page plus 376. Only statements of the paper are cited; where a rule rests on the
TigerGraph documentation alone, it says so.

## What the paper defines

1. **ACCUM runs once per row of the binding table** (PDF p.5 and p.6, section 4.1).
   Multiplicities are kept (bag semantics, section 6, PDF p.8: a pattern has as
   many matches of a pair of vertices as there are distinct paths between them),
   which is why a `SumAccum` counts paths and not distinct vertices. The ACCUM clause
   is executed once for each row of the binding table (PDF p.7, section 4.3); the
   Appendix (p.14-15) describes how the implementation simulates the repeated
   executions without materialising the paths.
2. **Snapshot semantics** (PDF p.7, section 4.3). All executions of the ACCUM
   clause start from the same snapshot of the accumulator values, and the inputs
   they produce are not visible to the other executions. The inputs are
   aggregated into the accumulators only after all executions have completed
   (a Map phase, then a Reduce phase).
3. **Order invariance** (p.7). The result is well defined because the combiners
   are commutative and associative. The exceptions are `ListAccum`,
   `ArrayAccum` and `SumAccum<STRING>`, whose result is "in general
   non-deterministically ordered".
4. **POST-ACCUM is syntactic sugar** whose statements coincide with those of
   ACCUM (p.7, section 4.4). The paper's own PageRank (Fig. 4, p.6; text on p.8)
   reads `v.@score` after assigning it in POST-ACCUM, so the snapshot rule is
   not applied there.
5. **Edge variables under a Kleene star** have several bindings per path
   (p.11, "A Tractable Class of Queries"), which is one reason such queries are
   outside the tractable class.
6. The SQL clauses (SELECT, WHERE, GROUP BY, HAVING, ORDER BY, LIMIT) inherit SQL
   semantics (p.6-7, section 4.2). The paper says nothing more about DISTINCT,
   HAVING, PER or the aliases allowed in POST-ACCUM.

## Rules gsql-lsp derives

### `accumulator-read-after-write` (warning; hint for a plain `=`)

An accumulator that is accumulated (`+=`) in an ACCUM clause and read in a later
statement of the same block of that clause. By (2) the read sees the value from before
the clause. The paper's ACCUM clauses only have `+=`; a plain `=` write is an
extrapolation of the same snapshot rule, so it is reported as a hint. The TigerGraph documentation (section "Parallelism in
ACCUM clause" of the SELECT statement, all of 3.11 to 4.3) has the same example
and the same advice: move the second statement to POST-ACCUM.

```gsql
// warning: @@count_total gets the value of p.@active_flag from before the clause
S = SELECT p FROM Person:p -(KNOWS:e)- Person:t
    ACCUM p.@active_flag += 1,
          @@count_total += p.@active_flag;

// fine: POST-ACCUM runs after the ACCUM clause has completed
S = SELECT p FROM Person:p -(KNOWS:e)- Person:t
    ACCUM p.@active_flag += 1
    POST-ACCUM @@count_total += p.@active_flag;
```

Silent on purpose:

- A read before the write (`IF NOT t.@visited THEN t.@visited += TRUE, @@n += 1 END`,
  the usual BFS idiom) and a read in the write statement itself (`s.@x += s.@x`).
- A read in the other branch of an IF, or after the IF, from a write inside it.
- A subscript target (`@@m[k] += 1`) and a mutating method call on the accumulator.
- POST-ACCUM, see (4): the paper, the documentation's tutorial
  (`POST-ACCUM s.@cnt2 += s.@cnt1`) and real queries read the written value there.
- A write and a read through two different names of the same vertex.

### `kleene-edge-alias` (error)

An edge alias on an edge pattern that repeats with `*` or `*m..n`
(`-(E>*1..3:e)-`). The rule comes from the TigerGraph documentation ("Repeating a
pattern"), which forbids it. The paper only supports the reason: a variable bound in
the scope of a Kleene star has several bindings per path, which takes a query out of
the tractable class (PDF p.11); it does not call the pattern an error. An exact count
(`*3`, or equal bounds such as `*2..2`) and repetition with a condition are left alone,
as is an alias on a plain edge.

```gsql
S = SELECT y FROM X:x -(F>*1..3:e)- Y:y;   // error
S = SELECT y FROM X:x -(F>*1..3)- Y:y;     // fine
```

### Checked against

The rules give no hit on the code examples of the cached documentation of GSQL
3.11, 4.1, 4.2, 4.3 and of the TigerGraph server documentation 4.3, each example
also wrapped in a query, except for the documentation's own wrong example of the
snapshot rule (which is the intended hit), and none on the real queries used as
test corpus.

## Documented, not diagnosed

- **Order-dependent accumulators** (3). `ListAccum`, `ArrayAccum` and
  `SumAccum<STRING>` fed by ACCUM collect their input in no fixed order. A
  diagnostic would be noisy: they are used on purpose (BFS parent lists,
  sampling, sorting afterwards), and the paper says the user accepts the
  non-determinism when choosing them.

## Dropped, and why

- **Reading a value written in POST-ACCUM**: see (4); flagging it would hit the
  paper's PageRank and real code.
- **`WHERE` reading an accumulator that ACCUM of the same block updates**:
  `WHERE NOT t.@visited ACCUM t.@visited += TRUE` is the standard BFS idiom.
- **`=` to a vertex accumulator in ACCUM** (a race between executions): the
  paper's ACCUM only has `+=`, but the documentation allows `=` and
  `t.@parent = s` is a deliberate "any one" choice. Intent is not decidable. The
  global case is already `accumulator-assignment`.
- **Multiplicity of multi-hop patterns** (`t.@cnt += 1` counts paths): correct
  behavior, intent cannot be judged statically.
- **Commutativity of combiners**: `+=` versus `=` type errors are covered by
  existing rules; the order-dependent types are the case above.
- **Tractable-class conditions** (p.11, "A Tractable Class of Queries"): the paper
  only says that vertex and edge variables bound in the scope of a Kleene star, and
  accumulators of type ListAccum, ArrayAccum and SumAccum<STRING>, take a query
  out of the class that is evaluated in polynomial time; it does not forbid them.
  Not an error. (The error of `kleene-edge-alias` comes from the TigerGraph
  documentation alone, see the section on that rule.)
- **Plain variables assigned in ACCUM or POST-ACCUM** (the counter that stays at
  1, "dt is not updated yet"): stated by the documentation only ("Base type
  variables", and the `count_employment_relationships` example), not by the
  paper. Not implemented; a hint is possible later once scope data of the
  symbol table is reliable for clause-local declarations.
- **POST-ACCUM naming two vertex aliases**, **vertex attribute assignment in an
  edge-induced ACCUM**, **SQL-like SELECT column order**: documentation only
  (the paper does not state them), and the compiler's behavior cannot be
  confirmed offline.
- **"A local variable can be declared in ACCUM once and cannot be reassigned"**
  (one documentation sentence): it contradicts the declaration page and real
  code (`DOUBLE runCum = 0, ... runCum = runCum + td`). Not enforced.
