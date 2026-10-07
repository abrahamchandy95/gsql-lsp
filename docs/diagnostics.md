# Diagnostics

Reference for diagnostic codes emitted by `gsql-lsp`.

---

## Schema & System

| Code               | Severity | Description                                                  | Setting          |
| ------------------ | -------- | ------------------------------------------------------------ | ---------------- |
| `stub-declaration` | Warning  | Built-in declared outside generated reference files          | —                |
| `no-schema`        | Warning  | Workspace missing schema; type and attribute checks disabled | `noSchemaNotice` |

---

## Syntax

| Code           | Severity | Description                                        | Setting |
| -------------- | -------- | -------------------------------------------------- | ------- |
| `syntax-error` | Error    | Parsing failure, missing delimiter, or syntax typo | —       |

---

## Declarations & Identifiers

| Code                     | Severity        | Description                                                | Setting                |
| ------------------------ | --------------- | ---------------------------------------------------------- | ---------------------- |
| `undeclared-accumulator` | Error           | Accumulator used without prior declaration                 | —                      |
| `duplicate-declaration`  | Error           | Identifier already declared in current scope               | —                      |
| `undefined-name`         | Warning         | Unknown identifier                                         | `undefinedNames`       |
| `unknown-function`       | Warning         | Function not found in built-ins or queries                 | `undefinedNames`       |
| `unknown-method`         | Warning         | Method does not exist on receiver type                     | `undefinedNames`       |
| `unknown-type-name`      | Warning         | Unrecognized type name in declaration                      | `undefinedNames`       |
| `unknown-type`           | Warning         | Vertex or edge type missing from schema                    | `unknownTypes`         |
| `graph-without-edges`    | Info            | Graph defines vertices without edge types                  | `languageRules`        |
| `unknown-graph`          | Warning         | Referenced graph does not exist in workspace               | `unknownTypes`         |
| `unknown-attribute`      | Warning         | Attribute not defined on target type                       | `unknownAttributes`    |
| `duplicate-definition`   | Warning / Hint  | Conflicting duplicate top-level schema or query definition | `duplicateDefinitions` |
| `unused`                 | Hint            | Declared variable or accumulator is never read             | `unused`               |
| `argument-count`         | Warning         | Parameter count mismatch in call                           | —                      |
| `argument-name`          | Warning         | Unknown parameter name in JSON argument object             | —                      |
| `argument-type`          | Warning         | Argument type incompatible with parameter definition       | `languageRules`        |
| `reserved-word`          | Error / Warning | Identifier collides with a reserved keyword                | `languageRules`        |

---

## LOAD & INSERT Statements

| Code            | Severity        | Description                                               | Setting |
| --------------- | --------------- | --------------------------------------------------------- | ------- |
| `value-count`   | Warning / Error | Column count does not match schema or column list         | —       |
| `value-skip`    | Error           | Primary ID cannot be skipped in LOAD                      | —       |
| `endpoint-type` | Warning         | Generic edge LOAD requires explicit endpoint vertex types | —       |

---

## Accumulators & Loops

| Code                           | Severity        | Description                                               | Setting         |
| ------------------------------ | --------------- | --------------------------------------------------------- | --------------- |
| `accumulator-assignment`       | Error           | Direct assignment used instead of accumulation in ACCUM   | `languageRules` |
| `accumulator-mutator`          | Error           | Mutator method invoked in an invalid clause context       | `languageRules` |
| `accumulator-declaration`      | Error           | Attached accumulator declared inside loop body            | `languageRules` |
| `loop-control`                 | Error           | Loop control statement used outside loop scope            | `languageRules` |
| `foreach-variables`            | Error           | Variable count does not match collection shape            | `languageRules` |
| `edge-accumulator`             | Error           | Edge accumulator accessed inside POST-ACCUM               | `languageRules` |
| `accumulator-type`             | Error / Warning | Invalid accumulator nesting, arguments, or type structure | `languageRules` |
| `accumulator-input`            | Error           | Invalid input shape or missing mapping operator           | `languageRules` |
| `accumulator-read-after-write` | Warning / Hint  | Accumulator read after write in same ACCUM clause         | `languageRules` |
| `accumulator-case`             | Warning         | Accumulator type name case mismatch                       | `languageRules` |

---

## Query Logic

| Code                | Severity | Description                                             | Setting         |
| ------------------- | -------- | ------------------------------------------------------- | --------------- |
| `v3-comparison`     | Warning  | Equality operator requires SYNTAX V3                    | —               |
| `cypher-syntax`     | Warning  | OpenCypher pattern used in V1 or V2 syntax              | `languageRules` |
| `limit-offset`      | Error    | OFFSET specified without ORDER BY                       | `languageRules` |
| `kleene-edge-alias` | Error    | Variable-length path pattern cannot bind an edge alias  | `languageRules` |
| `having-alias`      | Error    | HAVING clause references unselected vertex alias        | `languageRules` |
| `per-alias`         | Error    | Alias used outside PER clause list                      | `languageRules` |
| `pattern-join`      | Warning  | Disjoint FROM patterns cannot be joined                 | `languageRules` |
| `type-mismatch`     | Warning  | Incompatible types in expression, assignment, or return | `languageRules` |
| `float-equality`    | Warning  | Exact equality comparison on floating-point values      | `floatEquality` |
| `virtual-edge`      | Error    | Invalid declaration or placement of virtual edge        | `languageRules` |
| `interpreted-mode`  | Warning  | Feature not supported in interpreted query execution    | `languageRules` |
| `distributed-mode`  | Warning  | Feature not supported in distributed query execution    | `languageRules` |
| `deprecated`        | Hint     | Deprecated syntax or feature usage                      | `languageRules` |

---

## Style Guide

| Code           | Severity | Description                                      | Setting |
| -------------- | -------- | ------------------------------------------------ | ------- |
| `keyword-case` | Hint     | Keywords and reserved words must be uppercase    | `style` |
| `hash-comment` | Hint     | Use double-slash comments instead of hash symbol | `style` |
