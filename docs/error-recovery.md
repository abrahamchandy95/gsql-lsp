# Error Recovery

How `gsql-lsp` detects, isolates, and repairs syntax errors using trial parsing over Tree-sitter.

---

## Recovery Pipeline

When a statement fails to parse, `gsql-lsp` executes candidate repairs in sequence and stops at the first clean parse:

1. **Joined Keywords:** Expands combined tokens (e.g., `ELSEIF` → `ELSE IF`).
2. **Single Keyword Typos:** Substitutes words using edit distance (Levenshtein 1–2).
3. **Control Tokens:** Inserts missing `THEN` or `DO` after `IF`, `WHILE`, and `FOREACH`, or missing commas in lists.
4. **Dual Keyword Typos:** Replaces two misspelled keywords in a statement.
5. **Single-Token Insertion/Removal:** Removes a stray token or inserts a missing delimiter (`)`, `]`, `}`, `;`, `,`, `=`, `END`, `THEN`, `DO`).
6. **Line-Scoped Fixes:** Applies keyword replacements isolated to the active line.

---

## Behavior & Diagnostics

- **Precise Reporting:** Replaces generic parser cascades with targeted messages (e.g., `missing ')' after '1'` or `unexpected 'x'`).
- **Quick Fixes:** Offers up to 3 viable alternatives. Single-token guess repairs are never applied automatically via `source.fixAll`.
- **Cascade Suppression:** Suppresses secondary semantic errors (such as undeclared identifiers) within misparsed regions until syntax is valid.
- **Latency Bounds:** Execution is capped by a strict 60 ms deadline per document.

---

## Limitations

- **One Edit Per Statement:** Statements with multiple structural errors fall back to standard Tree-sitter diagnostics.
- **No Synthetic Identifiers:** The engine never fabricates missing names, numbers, or string literals.
- **Syntactic Validation Only:** A successful trial parse proves grammatical validity, not user intent.
