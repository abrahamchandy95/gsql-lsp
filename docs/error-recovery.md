# Error recovery

How gsql-lsp finds and repairs syntax errors, what it took from the paper
*Don't Panic! Better, Fewer, Syntax Errors for LR Parsers* (the CPCT+ algorithm; the
PDF is in `docs/papers/`), and how well it works, measured on seeded mutations of
valid code.

## What the paper proposes

Page numbers are those of the paper.

- Repairs are searched on the LR parse stack: a Dijkstra search over
  configurations (stack, remaining tokens, repair sequence) with the moves *insert
  any terminal that is valid in the state*, *delete the next token* and *shift one
  token* (Fig. 5, p. 7; "CR Shift 3", p. 9). Insert and delete cost 1, shifting
  costs 0 (p. 8). A search ends after `Nshifts = 3` shifts or at acceptance.
- It keeps the complete set of minimum-cost repair sequences (p. 11) and reports
  them all (Fig. 1c, p. 2: for `int x y;` the repairs are `Delete y`, `Insert ,`
  and `Insert =`).
- Configurations with the same stack, the same remaining input and compatible
  repair tails are merged (pp. 11-12); without merging the failure rate doubles
  (3.63% against 1.63%, p. 17).
- Equal-cost repairs are ranked by how far parsing continues after them (up to
  `Ntry = 250` tokens), and only the furthest are kept (pp. 12-13). Ranking the
  other way round gives 31.9% more errors (p. 17).
- The limits of earlier algorithms (`Ntotal` and the caps on inserts and deletes)
  are dropped, because "it is impossible to find good values for these constants"
  (p. 14); a timeout, 0.5 s per file, bounds the search instead. `Nshifts = 3`
  (p. 12) and `Ntry = 250` ("somewhat arbitrarily", p. 13) are kept. The recovery
  time per file is reported in the summary table of Fig. 11 (p. 17); the contrast
  between its median and its mean, which a few outliers dominate, is discussed on
  p. 16.
- Tokens whose value matters can be declared `%avoid_insert`; repairs that insert
  them are listed last, so `Delete +` is shown before `Insert Int` for `2 + + 3`
  (p. 22).
- The paper has no ground truth for what the user meant (p. 15). It counts the
  number of error locations as the proxy, over 200,000 real invalid Java files.
  Mutation is named as a route for other languages, with the doubt that it does not
  reproduce human errors (p. 23).

## What gsql-lsp does

gsql-lsp sits on top of tree-sitter, which has its own cost-based recovery
(inserting one MISSING token, skipping tokens in an ERROR node) and exposes no
parse stack. Repairs are therefore searched by *trial parses*: a broken top-level
statement (`autocorrect::statements`) is parsed again with an edit, and an edit
counts as a repair when the whole statement then parses with no ERROR and no
MISSING node. `features/autocorrect.rs` tries, per statement, in this order, and
stops at the first stage that succeeds:

1. several known joined keywords (`ELSEIF` chains) at once;
2. one word replaced by a similar keyword (distance 1 for words of 3-5 letters, 2
   for longer ones, up to 4 options);
3. `THEN` / `DO` inserted after IF, WHILE and FOREACH headers (greedy, up to 4
   rounds), and a `,` inserted inside list nodes;
4. two words replaced by keywords;
5. **(new) one token removed or inserted** (below);
6. a keyword replacement that fixes only its own line.

The search runs under a per-document deadline (60 ms in release builds), a trial
count per document and a trial count per statement. Success is "zero errors in the
statement", which is stricter than the paper's `Ntry` lookahead, so a complete repair
needs no separate "furthest" ranking. The keyword stages add a spelling prior
that CPCT+ lacks; it is kept as the tie-break there.

Repairs of "typos" (`KeywordTypo`) replace the parser's own messages for the
statement, and the statement is not reported again (`resolves`).

## What was added

### Stage 5: one token removed or inserted

`Search::token_fix` is CPCT+ restricted to one edit at the error point
(delete or insert), with trial parses in place of the stack:

- **Window.** The tokens from three before the first error to one after it. When the
  parser took tokens back into one ERROR node (it noticed the problem at its end)
  the window runs to the end of that node, at most 8 tokens past the error.
- **Removals.** Each token of the window is removed together with the blanks after
  it (or before it), so that removing a stray word from `PRINT x x;` gives
  `PRINT x;`.
- **Insertions.** One of `) ] } ; , END THEN DO = ( [ {` is inserted right after
  the token before each position (so `abs(1;` becomes `abs(1);`, not `abs(1 );`).
  Only these structural tokens are tried (an offline experiment that tried all
  single-token insertions found no original text restored by tokens outside this
  set; the script of that experiment is not part of the repository).
  Identifiers, numbers and strings are never invented (the paper's
  `%avoid_insert`, p. 22).
- **Lookahead pruning.** An insertion is tried only when the token is in the
  lookahead set of the parser state after the token before it
  (`Node::next_parse_state` with `Language::lookahead_iterator`). The set is a
  superset of what parses (tree-sitter merges states and does not perform pending
  reductions), so it only saves trials; states of tokens inside error nodes are
  not trusted and prune nothing. Pruning cut p95 diagnostics time from 57 ms to
  39 ms in the harness without losing fixes beyond noise (delete: 60.6% to 59.7% of
  mutations with a zero-error fix, insert 93.3% to 94.0%).
- **Ordering and alternatives.** Candidates are sorted by distance from the first
  error, removal before insertion, then by the order of the list above; equal texts
  are tried once (the paper's merging, in its simplest form). The first repair that
  parses without errors is reported with up to two more as further quick fixes
  ("Remove `x`", "Insert `)`"), like the paper's Fig. 1c. Reordering (punctuation
  insertions before removals, or by class before distance) and keeping more alternatives (up to 5)
  did not change the exact-restoration numbers beyond noise, so the plain order
  stays.
- **A value is missing.** When the parser reports a missing name or expression
  (a named MISSING node), no token is removed: `PRINT ;` lacks an expression, and
  removing `PRINT` explains nothing.
- **Keywords must read as keywords.** An inserted `END`, `THEN` or `DO` must be
  parsed as that keyword, not as a name (`PRINT END;` parses, with a variable named
  `END`).
- **Certain or a guess.** These repairs are guesses: their quick fixes are never in
  `source.fixAll`, and the corrected text is not used to analyse the document (the
  keyword typos are; see `Repair`). The part the parser misread because of the
  token is computed as for keyword typos, so semantic findings there are
  suppressed.

### Diagnostics

`features/diagnostics.rs` reports a token repair as one error and drops the
parser's errors for the statement:

- `unexpected `x` in query body: the statement parses without it`, on the token;
- `missing `)` after `1` in argument list`, on the token before the insertion
  (`missing `;` after this statement` keeps its old wording).

It does so only when the parser's own messages for the statement are bare
(`unexpected x`, `missing x`, `unfinished statement`). A message that says more
(trailing comma, `=>`, an unclosed IF, a malformed edge step, an unterminated
string, a SELECT without FROM, `missing , between ACCUM statements`) stays, and
the token repair is dropped: running the harness with those preempted gave a
higher "fix" rate and a lower exactness, and broke specific tests.

The existing missing-comma fix now inserts the comma after the token before it
(`a, b`, not `a , b`), which raised the share of exactly restored texts.

## Measurement

`scripts/dev/check_repairs.py` mutates valid code at seeded random token
positions and asks the server (LSP, `didChange`, `codeAction`) what it reports.
Corpus: the 45 G2N queries and 150 GSQL examples of the cached language
reference that parse without a syntax error (as written or inside a query). Per
class (seed 7, 5195 mutations):

| class | mutation |
| --- | --- |
| delete | one token removed |
| insert | a stray token of the file put in front of a token |
| replace | one token replaced by another token of the file |
| dropcloser | a closing `)`, `}`, `]`, `END` or `;` removed |
| swap | two adjacent tokens exchanged |

Columns: `cases` (mutations that change the text), `seen` (mutations with a syntax
error reported), `near%` (the error is within one line of the mutation), `diags`
(mean syntax diagnostics per seen mutation; fewer means fewer cascade errors,
the paper's proxy), `fix%` (a quick fix is offered), `zero%` (some offered fix
leaves no syntax error), `1st%` (the first fix does), `exact%` (some fix restores
the original text byte for byte), `ws%` (up to white space), `p95ms` (edit to
diagnostics).

Before (the build at the start of this work, `gsql-lsp` 0.1.0 at 5ecf285):

```
class       cases  seen  near%  diags  fix%  zero%  1st% exact%   ws%  p95ms
delete       1050   909   96.0   1.45  25.5   17.5  17.1    6.1  15.1     24
insert       1050   849   98.5   1.08  15.1   12.1  12.1    0.0   0.6     23
replace      1025   673   94.7   1.64  20.5    9.7   9.7    0.0   0.0     22
dropcloser   1038  1030   87.4   1.41  75.3   54.9  51.7   41.7  49.8     26
swap         1032   957   97.6   1.76  19.0    2.6   2.5    0.0   0.0     29
```

After:

```
class       cases  seen  near%  diags  fix%  zero%  1st% exact%   ws%  p95ms
delete       1050   909   96.8   1.37  56.3   51.6  51.2   19.1  27.1     44
insert       1050   849   98.6   1.04  93.9   92.5  92.5   75.3  75.9     30
replace      1025   673   94.7   1.59  37.7   28.2  28.2    0.0   0.0     38
dropcloser   1038  1030   92.0   1.20  87.8   82.4  79.3   64.0  72.9     61
swap         1032   957   97.6   1.75  39.6   23.8  23.7    0.0   0.0     50
```

A second seed (11, 5180 mutations) agrees: before / after `fix%` delete
24.7 / 61.0, insert 15.7 / 92.3, replace 22.0 / 39.7, dropcloser 72.0 / 85.6, swap
17.7 / 39.7; `exact%` delete 6.7 / 18.5, insert 0.0 / 74.3, dropcloser 42.6 /
64.0; `diags` delete 1.44 / 1.35, insert 1.11 / 1.03, dropcloser 1.36 / 1.15.

Reading the numbers:

- A stray token (insert) is the paper's `Delete` case: a fix is now offered for 94%
  of them and restores the exact text for 75%, from 15% and 0%.
- A deleted or forgotten closer (dropcloser, delete) gains most from the
  insertions; `exact%` of dropcloser goes from 42% to 64%, and the errors
  after the first (cascades) drop from 1.41 to 1.20 per mutation.
- `zero%` of replace and swap rise, but their `exact%` stays at 0: one-token edits
  cannot undo them, and the repair found is a valid but different program (it says
  `unexpected x ...: the statement parses without it`, which is true, and is only
  offered as a quick fix). An offline experiment (all single-token edits tried on whole
  files; its script is not in the repository) found that a single edit gives a
  parse-clean text for 26% (replace) and 34% (swap) of the cases and never the
  original text, so more was not attempted.
- For delete, 38% of the deleted tokens are names, strings or numbers that cannot be
  invented; the ceiling for exact restoration is about 62% of the mutations, and the
  same offline experiment reaches 43%. The result, 19% of mutations restored byte for
  byte and 27% up to white space, is below that: the window is narrow and the
  deadline is short.
- Latency goes up: p95 from 22-29 ms to 30-61 ms per `didChange` on broken
  documents (valid documents are not searched). It stays under the 60 ms deadline
  of the keyword search per document, which also bounds the worst case. The check
  of `/tmp/gt/perf1/ev3_8.gsql` and `/tmp/gt/perf2/e50_400.gsql` takes 0.19 s and
  0.23 s, with the output unchanged.

Not changed by the work: valid code. `gsql-lsp check` on the G2N queries prints
the one `openMin` hint as before, the Documents query its one no-schema warning,
and the broken fragments of the cached documentation change only in syntax messages
(61 of 1776 lines over all versions; no semantic finding is new);
`check_typo_recall.py` stays at found 776 of 793, noisy 0;
`check_attribute_lookups.py` 3068/3068; `check_definitions_under_typos.py` 0
failures; `check_deep_nesting.py` shows no FAILED.

## Ideas of the paper not taken, and why

| idea | decision |
| --- | --- |
| Search on the LR stack, multi-token insert sequences, `Nshifts`, stack merging (Figs. 5-9) | Not possible: tree-sitter exposes no parse stack, only trees and per-node parse states. Repairs longer than one edit exist only as the combinations of `statement()` (two keyword replacements, repeated `THEN` / `DO`). |
| Panic mode baseline (Fig. 4) | tree-sitter's own skipping is the baseline here. |
| Grammar annotations, `%avoid_insert` declarations, semantic actions (section 7) | No semantic actions. The effect of `%avoid_insert` is hard-coded: names, numbers and strings are never inserted. |
| Rank partial repairs by how far parsing continues (section 5.3, pp. 12-13, example in Fig. 10, p. 14), and repair repeatedly | Not done. Complete repairs make the ranking moot, and repeated one-token repairs would only lower the cascade count of replace and swap, whose single-token repair is never the original text (exact 0%). The cascade count already fell where a one-token repair exists. |
| Replace and swap repairs beyond keyword typos | Single-edit ceiling 26-34% with 0% exact restoration. |
| Bootstrap intervals, 30 runs, 200,000 files | The harness is deterministic for a seed; variance comes from the choice of mutations, so two seeds are reported instead. |

## Limits

- One edit per statement. A statement with two mistakes (a keyword typo and a
  missing `)`, say) has no one-token repair and keeps the parser's messages.
- A repair is judged only by the statement parsing; it can be a valid program the
  author did not mean. That is why it is a quick fix, never part of fix all, and
  why up to three alternatives are shown.
- While a token repair is reported for a statement, the semantic findings inside the
  region the parser misread are held back, as for keyword typos: over the cached
  docs, `undeclared-accumulator` for `@@cnt` disappears in two data-type examples
  (4.1 to 4.3), and the position of such a statement's error moves from the
  offending word to the `;` the repair names.
- Identifiers, numbers and strings are never inserted, so a deleted name is reported
  as `missing identifier` as before, with no fix.
- The lookahead set is a superset and unknown inside error nodes; it prunes trials
  but proves nothing. The window (3 tokens before the first error) misses a closer
  that belongs further back.
- Mutation is a proxy for human errors (the paper's own doubt, p. 23). In the
  corpus a stray token is a random token of the file; real stray tokens are usually
  not.

## Reproducing

```
cargo build --release -p gsql-lsp
GSQL_LSP_BIN=target/release/gsql-lsp DOCS_EXAMPLES=target/docs-examples \
    python3 scripts/dev/check_repairs.py --seed 7
```

(`G2N_DIR` and `DOCS_EXAMPLES` name the corpora; `--per-file`, `--docs` and
`--docs-per-file` set the sample size; `--out` writes the rows as JSON.) Run it
against the build before a change and the build after it; the table above is the
output for 45 G2N files with 10 positions each and 150 examples with 4 each.
