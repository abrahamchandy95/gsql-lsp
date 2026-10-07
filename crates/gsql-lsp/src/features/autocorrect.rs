//! Autocorrect: keyword typos found by trial reparsing, and suggestions for
//! misspelled names.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::OnceLock;

use tree_sitter::{Node, Tree};

use crate::analysis::Analysis;
use crate::features::Snapshot;
use crate::lsp::types::{Diagnostic, Position, Range};
use crate::syntax;
use crate::text::{PositionEncoding, SourceText, Span};

/// Every keyword of the grammar, in upper case.
pub fn keywords() -> &'static [String] {
    static KEYWORDS: OnceLock<Vec<String>> = OnceLock::new();
    KEYWORDS.get_or_init(|| {
        let language = syntax::language();
        let mut words: Vec<String> = (0..language.node_kind_count() as u16)
            .filter(|&id| !language.node_kind_is_named(id) && language.node_kind_is_visible(id))
            .filter_map(|id| language.node_kind_for_id(id))
            // `POST-ACCUM` may also be written `POST_ACCUM`.
            .map(|kind| kind.replace('-', "_"))
            .filter(|kind| kind.len() > 1 && kind.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
            .collect();
        words.sort();
        words.dedup();
        words
    })
}

/// Edit distance with transpositions (optimal string alignment), ignoring case.
pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_ascii_lowercase().chars().collect();
    let b: Vec<char> = b.to_ascii_lowercase().chars().collect();
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1).min(rows[i][j - 1] + 1).min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// How far a word may be from a suggestion: one edit for short words, two
/// for long ones; nothing for words of one or two letters.
fn max_distance(word: &str) -> usize {
    match word.chars().count() {
        0..=2 => 0,
        3..=5 => 1,
        _ => 2,
    }
}

/// The candidates closest to `word`, best first.
pub fn similar<'c>(word: &str, candidates: impl IntoIterator<Item = &'c str>) -> Vec<&'c str> {
    similar_within(word, candidates, max_distance(word))
}

/// Like [`similar`], with the largest distance given.
fn similar_within<'c>(word: &str, candidates: impl IntoIterator<Item = &'c str>, limit: usize) -> Vec<&'c str> {
    let length = word.chars().count();
    let first = word.chars().next().map(|c| c.to_ascii_lowercase());
    let mut scored: Vec<(usize, bool, usize, std::cmp::Reverse<usize>, &str)> = candidates
        .into_iter()
        // Each edit changes the length by at most one.
        .filter(|c| *c != word && c.chars().count().abs_diff(length) <= limit)
        .map(|c| {
            // On ties, prefer the same first letter, a similar length, and
            // then the longer word (a dropped letter is the likelier typo).
            let other_first = c.chars().next().map(|c| c.to_ascii_lowercase()) != first;
            let count = c.chars().count();
            (distance(word, c), other_first, count.abs_diff(length), std::cmp::Reverse(count), c)
        })
        .filter(|&(d, ..)| d <= limit)
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.4 == b.4);
    scored.into_iter().map(|(.., c)| c).collect()
}

/// A word that, replaced by `keyword`, repairs a syntax error.
#[derive(Debug, Clone, PartialEq)]
pub struct KeywordTypo {
    pub span: Span,
    pub word: String,
    /// The best replacement.
    pub keyword: String,
    /// Other keywords that repair the error too.
    pub alternatives: Vec<String>,
    /// The replacement is certain (a known way of joining keywords), not a
    /// guess from spelling, so "fix all" may apply it.
    pub certain: bool,
    /// The statement the typo is in. Its syntax errors are consequences of
    /// the typo, except for those that remain once it is corrected.
    pub statement: Span,
    /// The statement, when the replacement removes all of its syntax errors.
    pub resolves: Option<Span>,
    /// The part of the document that the typo made the parser misread, so
    /// nothing else derived from its tree there can be trusted.
    pub misparsed: Option<Span>,
    /// For a repair that removes or inserts one token (see
    /// [`Search::token_fix`]): that repair first, then the other repairs that
    /// work. Empty for the repairs above.
    pub token_edits: Vec<TokenEdit>,
}

/// One token removed from a statement or inserted into it.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenEdit {
    /// What the edit replaces: a removed token with the blanks next to it, or
    /// the empty span where a token is inserted (after the token before it).
    pub span: Span,
    /// The text that takes its place: empty for a removed token.
    pub with: String,
    /// The token removed or inserted.
    pub token: String,
    pub inserted: bool,
}

/// Keywords written as one word, and how GSQL spells them.
const JOINED: &[(&str, &str)] = &[
    ("ELSEIF", "ELSE IF"),
    ("ELSIF", "ELSE IF"),
    ("ELIF", "ELSE IF"),
    ("ORDERBY", "ORDER BY"),
    ("GROUPBY", "GROUP BY"),
    ("POSTACCUM", "POST-ACCUM"),
    ("ENDIF", "END"),
    ("ENDWHILE", "END"),
    ("ENDFOR", "END"),
    ("ENDFOREACH", "END"),
    ("ENDCASE", "END"),
];

/// The keyword spelled like `word`: lower case for a lower-case word.
fn styled(keyword: &str, word: &str) -> String {
    if word.chars().all(|c| !c.is_ascii_uppercase()) { keyword.to_ascii_lowercase() } else { keyword.to_string() }
}

/// The byte ranges of string literals and comments of `text`: words in them are
/// not keywords, whatever shape error recovery gives the tree.
fn literal_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                spans.push((start, i));
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
                spans.push((start, i));
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + 2 + n + 2);
                spans.push((start, i));
            }
            _ => i += 1,
        }
    }
    spans
}

fn is_word(node: Node, source: &str) -> bool {
    node.child_count() == 0
        && !node.is_missing()
        && syntax::text(node, source).chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && syntax::text(node, source).chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// The keywords a word could be a misspelling of, given the text after it,
/// and whether the replacement is certain (a known joined keyword such as
/// `ELSEIF`).
fn replacements(word: &str, after: &str) -> (Vec<String>, bool) {
    let upper = word.to_ascii_uppercase();
    if let Some((_, keyword)) = JOINED.iter().find(|(written, _)| *written == upper) {
        return (vec![styled(keyword, word)], true);
    }
    // `POST-ACCUM` is one token, so `POTS-ACCUM` reads as a word and `-ACCUM`.
    if after.get(..6).is_some_and(|rest| rest.eq_ignore_ascii_case("-accum")) {
        let fits = upper != "POST" && distance(&upper, "POST") <= max_distance(word).max(1);
        return (if fits { vec![styled("POST", word)] } else { Vec::new() }, false);
    }
    // A parse that works out confirms the guess, so even two-letter keywords
    // (`TO`, `IF`, `IN`) may be one edit away.
    let limit = max_distance(word).max(usize::from(upper.chars().count() <= 2));
    let keywords = similar_within(&upper, keywords().iter().map(String::as_str), limit);
    let mut options: Vec<String> = keywords.into_iter().take(4).map(|k| styled(k, word)).collect();
    // `SumAcum<INT>`: a word before `<` may be a misspelled accumulator type
    // (their names are case-sensitive, so they keep their spelling).
    if after.trim_start().starts_with('<') {
        let names = crate::builtins::ACCUMULATORS.iter().map(|a| a.name);
        options.splice(0..0, similar(word, names).into_iter().take(2).map(String::from));
    }
    (options, false)
}

/// The top-level statements of a document as byte ranges, and whether each
/// is broken. A broken statement can fall apart into several top-level
/// nodes: a node belongs to the statement before it when it starts on the
/// line where that statement ends, is indented, or starts with `;` or a
/// closing bracket. (When the parser gives up on the whole document, the root
/// is an ERROR and its children are such pieces.)
pub(crate) fn statements(root: Node) -> Vec<(usize, usize, bool)> {
    // (start, end, last row, broken)
    let mut statements: Vec<(usize, usize, usize, bool)> = Vec::new();
    let mut cursor = root.walk();
    // Comments are extras; so are tokens the parser skipped, which matter.
    for child in root.children(&mut cursor).filter(|n| !n.is_extra() || n.has_error()) {
        let mut first = child;
        while let Some(leaf) = first.child(0) {
            first = leaf;
        }
        let position = child.start_position();
        let continues = position.column > 0
            || matches!(first.kind(), ";" | "}" | ")" | "]")
            || statements.last().is_some_and(|s| s.2 == position.row);
        // Only a piece of a statement is an unnamed node at the top level.
        let broken = child.has_error() || (!child.is_named() && child.kind() != ";");
        match statements.last_mut() {
            Some(statement) if continues => {
                statement.1 = child.end_byte();
                statement.2 = child.end_position().row;
                statement.3 |= broken;
            }
            _ => statements.push((child.start_byte(), child.end_byte(), child.end_position().row, broken)),
        }
    }
    statements.into_iter().map(|(start, end, _, broken)| (start, end, broken)).collect()
}

/// Limits that keep large broken files fast: trial parses per document and
/// per statement, bytes parsed per document, and the size of a statement.
const MAX_TRIALS: usize = 320;
const MAX_STATEMENT_TRIALS: usize = 64;
const MAX_TRIAL_BYTES: usize = 4 << 20;
const MAX_STATEMENT_BYTES: usize = 256 << 10;

/// Tokens that [`Search::token_fix`] tries to insert, likeliest first.
const INSERTABLE: &[&str] = &[")", "]", "}", ";", ",", "END", "THEN", "DO", "=", "(", "[", "{"];
/// How many tokens before and after the first error an edit may be at, and
/// how many other repairs are kept besides the best.
const TOKENS_BEFORE: usize = 3;
const TOKENS_AFTER: usize = 1;
const TOKENS_IN_ERROR: usize = 8;
const MAX_TOKEN_REPAIRS: usize = 2;

/// What parsing a statement (with or without replacements) found.
struct Outcome {
    errors: usize,
    /// The rows (within the statement) where ERROR and MISSING nodes start.
    error_rows: Vec<usize>,
    first_error: Option<usize>,
    /// The bytes inside ERROR nodes (a MISSING node counts for one): how much
    /// of the statement the parser gave up on.
    error_bytes: usize,
    /// A MISSING node that is a name or an expression: the parser lacks a value.
    missing_value: bool,
}

impl Outcome {
    fn of(root: Node) -> Outcome {
        let mut errors = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.is_error() || node.is_missing() {
                errors.push(node);
            }
            if node.has_error() {
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
        }
        let mut error_rows: Vec<usize> = errors.iter().map(|n| n.start_position().row).collect();
        error_rows.sort_unstable();
        error_rows.dedup();
        // Nested errors lie inside their ancestors' ranges: count each byte once.
        errors.sort_by_key(|n| (n.start_byte(), std::cmp::Reverse(n.end_byte())));
        let mut error_bytes = 0;
        let mut covered = 0;
        for n in &errors {
            error_bytes += n.end_byte().saturating_sub(covered.max(n.start_byte())).max(usize::from(n.is_missing()));
            covered = covered.max(n.end_byte());
        }
        Outcome {
            errors: errors.len(),
            error_rows,
            first_error: errors.iter().map(|n| n.start_byte()).min(),
            error_bytes,
            missing_value: errors.iter().any(|n| n.is_missing() && n.is_named()),
        }
    }
}

/// A word replaced by a keyword, and what parsing the statement then found.
struct Trial<'t> {
    word: Node<'t>,
    replacement: String,
    certain: bool,
    outcome: Outcome,
}

/// `text` with words replaced (given as (word, replacement), in order).
fn apply(text: &str, edits: &[(Node, &str)]) -> String {
    let mut edited = String::with_capacity(text.len() + 16);
    let mut last = 0;
    for (word, replacement) in edits {
        edited.push_str(&text[last..word.start_byte()]);
        edited.push_str(replacement);
        last = word.end_byte();
    }
    edited.push_str(&text[last..]);
    edited
}

/// The tree of `text` with words replaced, reparsed from the tree of `text`:
/// only what the replacements touch is parsed again, which keeps trials of
/// one statement cheap when the statement is large.
fn parse_edited(
    parser: &mut tree_sitter::Parser,
    base: Option<&Tree>,
    text: &str,
    edits: &[(Node, &str)],
) -> Option<Tree> {
    let edited = apply(text, edits);
    let Some(base) = base else {
        return parser.parse(edited, None);
    };
    let mut old = base.clone();
    let mut order: Vec<&(Node, &str)> = edits.iter().collect();
    order.sort_by_key(|(word, _)| std::cmp::Reverse(word.start_byte()));
    for (word, replacement) in order {
        let start = word.start_position();
        old.edit(&tree_sitter::InputEdit {
            start_byte: word.start_byte(),
            old_end_byte: word.end_byte(),
            new_end_byte: word.start_byte() + replacement.len(),
            start_position: start,
            old_end_position: word.end_position(),
            new_end_position: tree_sitter::Point { row: start.row, column: start.column + replacement.len() },
        });
    }
    parser.parse(edited, Some(&old))
}

/// The tree of `text` with `remove` bytes at `at` replaced by `insert`,
/// reparsed from the tree of `text` like [`parse_edited`].
fn parse_spliced(
    parser: &mut tree_sitter::Parser,
    base: Option<&Tree>,
    text: &str,
    at: usize,
    remove: usize,
    insert: &str,
) -> Option<Tree> {
    let mut edited = String::with_capacity(text.len() + insert.len());
    edited.push_str(&text[..at]);
    edited.push_str(insert);
    edited.push_str(&text[at + remove..]);
    let Some(base) = base else {
        return parser.parse(edited, None);
    };
    let point = |offset: usize| {
        let before = &text[..offset];
        let row = before.matches('\n').count();
        tree_sitter::Point { row, column: offset - before.rfind('\n').map_or(0, |i| i + 1) }
    };
    let start = point(at);
    let mut old = base.clone();
    old.edit(&tree_sitter::InputEdit {
        start_byte: at,
        old_end_byte: at + remove,
        new_end_byte: at + insert.len(),
        start_position: start,
        old_end_position: point(at + remove),
        new_end_position: tree_sitter::Point { row: start.row, column: start.column + insert.len() },
    });
    parser.parse(edited, Some(&old))
}

/// The leaves of a tree with a hash of their path from the root: two
/// leaves with the same hash were read the same way.
fn leaves(root: Node) -> Vec<(usize, usize, u64)> {
    let mut leaves = Vec::new();
    let mut paths = vec![0u64];
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let path = (paths[paths.len() - 1] ^ u64::from(node.kind_id())).wrapping_mul(0x0100_0000_01b3).rotate_left(5);
        if cursor.goto_first_child() {
            paths.push(path);
            continue;
        }
        leaves.push((node.start_byte(), node.end_byte(), path));
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return leaves;
            }
            paths.pop();
        }
    }
}

/// The part of a tree that reads differently in `corrected`, from the first
/// token whose place in the tree changes to the last, given where the edits
/// are (`first..last` of the original text).
fn changed_between(original: Node, corrected: Node, first: usize, last: usize) -> Span {
    let before = leaves(original);
    let after = leaves(corrected);
    let same = |a: &(usize, usize, u64), b: &(usize, usize, u64)| a.2 == b.2;
    let front = before.iter().zip(&after).take_while(|(a, b)| same(a, b)).count();
    let back = before.iter().rev().zip(after.iter().rev()).take_while(|(a, b)| same(a, b)).count();
    let start = before.get(front).map_or(first, |leaf| leaf.0.min(first));
    let end = before.len().checked_sub(back + 1).and_then(|i| before.get(i)).map_or(last, |leaf| leaf.1.max(last));
    Span::new(start, end.max(start))
}

/// Trial parses of statements with words replaced by keywords.
struct Search<'s> {
    source: &'s str,
    parser: tree_sitter::Parser,
    /// The tree of the statement being searched, which trials reuse.
    base: Option<Tree>,
    trials: usize,
    bytes: usize,
    /// The search gives up at this point: diagnostics are recomputed on every
    /// edit, and a file full of errors must not make typing sluggish.
    deadline: std::time::Instant,
}

/// How long the search for misspelled keywords may take (unoptimized builds,
/// which the tests use, are an order of magnitude slower).
const SEARCH_TIME: std::time::Duration =
    std::time::Duration::from_millis(if cfg!(debug_assertions) { 2000 } else { 60 });

impl<'s> Search<'s> {
    fn new(source: &'s str) -> Search<'s> {
        Search {
            source,
            parser: syntax::new_parser(),
            base: None,
            trials: 0,
            bytes: 0,
            deadline: std::time::Instant::now() + SEARCH_TIME,
        }
    }

    fn exhausted(&self) -> bool {
        self.trials >= MAX_TRIALS || self.bytes >= MAX_TRIAL_BYTES || std::time::Instant::now() >= self.deadline
    }

    /// Parses `text` with words replaced. `None` when the budget is spent or
    /// the first replacement ends up inside an ERROR node (it does not fit
    /// there either).
    fn trial(&mut self, text: &str, edits: &[(Node, &str)]) -> Option<Outcome> {
        if self.exhausted() {
            return None;
        }
        self.trials += 1;
        self.bytes += text.len();
        let tree = parse_edited(&mut self.parser, self.base.as_ref(), text, edits)?;
        let root = tree.root_node();
        let (word, replacement) = edits[0];
        let start = word.start_byte();
        let misplaced = root
            .descendant_for_byte_range(start, start + replacement.len())
            .is_some_and(|n| syntax::self_and_ancestors(n).any(|a| a.is_error()));
        (!misplaced).then(|| Outcome::of(root))
    }

    /// The part of `text` that the parser reads differently once `edits` are
    /// applied, from the first token whose place in the tree changes to the
    /// last. What the original tree says about it cannot be trusted.
    fn changed(&mut self, text: &str, original: Node, edits: &[(Node, &str)]) -> Option<Span> {
        let corrected = parse_edited(&mut self.parser, self.base.as_ref(), text, edits)?;
        let first = edits.iter().map(|(word, _)| word.start_byte()).min()?;
        let last = edits.iter().map(|(word, _)| word.end_byte()).max()?;
        Some(changed_between(original, corrected.root_node(), first, last))
    }

    /// A missing `,` between items that explains every error of the statement:
    /// reported as a "typo" with no word, whose correction is `, `.
    fn comma_fix(&mut self, root: Node, text: &str, anchor: usize, start: usize, end: usize) -> Option<KeywordTypo> {
        // Only inside lists of items (ACCUM statements have their own messages).
        let in_list = |at: usize| {
            root.descendant_for_byte_range(at, at).is_some_and(|node| {
                syntax::self_and_ancestors(node).any(|a| {
                    matches!(
                        a.kind(),
                        "parameter_list"
                            | "argument_list"
                            | "vertex_attribute_list"
                            | "edge_attribute_list"
                            | "value_list"
                            | "insert_columns"
                            | "column_list"
                            | "list_literal"
                    )
                })
            })
        };
        for at in comma_offsets(root, anchor).into_iter().filter(|at| in_list(*at)) {
            if self.exhausted() {
                return None;
            }
            self.trials += 1;
            let candidate = format!("{}, {}", &text[..at], &text[at..]);
            let fixes_all =
                self.parser.parse(&candidate, None).is_some_and(|tree| Outcome::of(tree.root_node()).errors == 0);
            if fixes_all {
                return Some(KeywordTypo {
                    span: Span::new(start + at, start + at),
                    word: String::new(),
                    keyword: ", ".to_string(),
                    alternatives: Vec::new(),
                    certain: false,
                    statement: Span::new(start, end),
                    resolves: Some(Span::new(start, end)),
                    misparsed: None,
                    token_edits: Vec::new(),
                });
            }
        }
        None
    }

    /// Missing `THEN` after IF conditions and `DO` after WHILE or FOREACH
    /// headers that explain every error of the statement: reported as
    /// "typos" with no word, like a missing `,`. All candidates together are
    /// tried first (one mistake among the others hides which insertion
    /// helps), then the insertion that leaves the least behind, repeatedly.
    fn block_keyword_fix(&mut self, root: Node, text: &str, start: usize, end: usize) -> Vec<KeywordTypo> {
        const ROUNDS: usize = 4;
        let mut candidates = block_keyword_candidates(root);
        if candidates.is_empty() {
            return Vec::new();
        }
        let typos = |inserted: Vec<(usize, &'static str)>| -> Vec<KeywordTypo> {
            inserted
                .into_iter()
                .map(|(at, keyword)| KeywordTypo {
                    span: Span::new(start + at, start + at),
                    word: String::new(),
                    keyword: keyword.to_string(),
                    alternatives: Vec::new(),
                    certain: false,
                    statement: Span::new(start, end),
                    resolves: Some(Span::new(start, end)),
                    misparsed: None,
                    token_edits: Vec::new(),
                })
                .collect()
        };
        if candidates.len() > 1 && !self.exhausted() {
            self.trials += 1;
            let mut all = text.to_string();
            for &(at, keyword) in candidates.iter().rev() {
                all.insert_str(at, keyword);
            }
            self.bytes += all.len();
            if self.parser.parse(&all, None).is_some_and(|tree| Outcome::of(tree.root_node()).errors == 0) {
                return typos(candidates);
            }
        }
        // The insertions so far, as (offset in `text`, keyword), in order.
        let mut inserted: Vec<(usize, &'static str)> = Vec::new();
        let mut current = text.to_string();
        let original = Outcome::of(root);
        let mut score = (original.errors, original.error_bytes);
        for _ in 0..ROUNDS {
            let mut best: Option<((usize, usize), usize, &'static str, String)> = None;
            for &(at, keyword) in &candidates {
                if self.exhausted() {
                    return Vec::new();
                }
                self.trials += 1;
                let mut candidate = current.clone();
                candidate.insert_str(at, keyword);
                self.bytes += candidate.len();
                let Some(tree) = self.parser.parse(&candidate, None) else { continue };
                let outcome = Outcome::of(tree.root_node());
                let found = (outcome.errors, outcome.error_bytes);
                if found < best.as_ref().map_or(score, |b| b.0) {
                    best = Some((found, at, keyword, candidate));
                }
            }
            let Some((found, at, keyword, candidate)) = best else { return Vec::new() };
            // The place in the original text: earlier insertions shifted it.
            let mut shift = 0;
            for &(o, k) in &inserted {
                if at >= o + shift + k.len() {
                    shift += k.len();
                }
            }
            inserted.push((at - shift, keyword));
            inserted.sort_by_key(|i| i.0);
            current = candidate;
            if found.0 == 0 {
                return typos(inserted);
            }
            score = found;
            let Some(tree) = self.parser.parse(&current, None) else { return Vec::new() };
            candidates = block_keyword_candidates(tree.root_node());
            if candidates.is_empty() {
                break;
            }
        }
        Vec::new()
    }

    /// One token that is stray or missing, found by trying: removing a token
    /// near the first error, or inserting one of the tokens that are often
    /// forgotten (closing brackets, `;`, `,`, `END`, `THEN`, `DO`, `=`, opening
    /// brackets) in front of one, so that the statement parses without errors.
    /// Edits nearest to the first error come first, removals before
    /// insertions; the first that works is reported, with up to
    /// [`MAX_TOKEN_REPAIRS`] others.
    fn token_fix(&mut self, text: &str, start: usize, end: usize, original: &Outcome) -> Vec<KeywordTypo> {
        let Some(anchor) = original.first_error else { return Vec::new() };
        let Some(base) = self.base.clone() else { return Vec::new() };
        // (start, end) of each token, and the parser state after it unless it lies in an error
        let mut states: Vec<Option<u16>> = Vec::new();
        let mut leaves: Vec<(usize, usize)> = Vec::new();
        syntax::walk_with_errors(base.root_node(), |n, in_error| {
            if n.child_count() == 0 && !n.is_extra() && !n.is_missing() && n.end_byte() > n.start_byte() {
                leaves.push((n.start_byte(), n.end_byte()));
                states.push((!in_error).then(|| n.next_parse_state()));
            }
        });
        // Whether `token` may follow the token before `k`, per the parser's tables
        // (a superset of what parses: the parser merges states). True when unknown.
        let language = syntax::language();
        let follows = |k: usize, token: &str| -> bool {
            let Some(state) = k.checked_sub(1).and_then(|i| states[i]) else { return true };
            let symbol = language.id_for_node_kind(token, false);
            let Some(mut lookahead) = language.lookahead_iterator(state) else { return true };
            lookahead.any(|candidate| candidate == symbol)
        };
        let first = leaves.iter().position(|l| l.1 > anchor).unwrap_or(leaves.len());
        let from = first.saturating_sub(TOKENS_BEFORE);
        // The parser may have taken back several tokens into one ERROR node
        // (it noticed the problem at the end of it).
        let mut error_end = anchor;
        syntax::walk(base.root_node(), |n| {
            if n.is_error() && n.start_byte() == anchor && n.end_byte() > error_end {
                error_end = n.end_byte();
            }
        });
        let reaches = leaves.iter().rposition(|l| l.0 < error_end).unwrap_or(first);
        let to = (reaches.clamp(first, first + TOKENS_IN_ERROR) + TOKENS_AFTER).min(leaves.len());
        // (distance from the first error, removal before insertion, rank of
        // the token, offset, bytes removed, text put in place, the token)
        let mut edits: Vec<(usize, usize, usize, usize, usize, String, String)> = Vec::new();
        let blank = |c: u8| c == b' ' || c == b'\t';
        for k in from..=to {
            let distance = k.abs_diff(first);
            // (When the parser lacks a name or an expression, a token before it is not stray.)
            if let Some(&(a, b)) = leaves.get(k).filter(|_| !original.missing_value) {
                // A removed token takes the blanks after it along, or else those before it.
                let bytes = text.as_bytes();
                let after = bytes[b..].iter().take_while(|&&c| blank(c)).count();
                let before = bytes[..a].iter().rev().take_while(|&&c| blank(c)).count();
                let line_start = text[..a].rfind('\n').map_or(0, |i| i + 1);
                let (from_byte, to_byte) = if after > 0 {
                    (a, b + after)
                } else if before > 0 && a - before > line_start {
                    (a - before, b)
                } else {
                    (a, b)
                };
                edits.push((distance, 0, 0, from_byte, to_byte - from_byte, String::new(), text[a..b].to_string()));
            }
            // An inserted token goes right after the token before it.
            let Some(&(_, at)) = k.checked_sub(1).and_then(|i| leaves.get(i)) else { continue };
            for (rank, token) in INSERTABLE.iter().enumerate().filter(|(_, t)| follows(k, t)) {
                let with = if matches!(*token, ")" | "]" | "}" | ";" | "," | "(" | "[") {
                    token.to_string()
                } else {
                    format!(" {token}")
                };
                edits.push((distance, 1, rank, at, 0, with, token.to_string()));
            }
        }
        edits.sort();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut found: Vec<TokenEdit> = Vec::new();
        for (_, kind, _, at, removed, with, token) in edits {
            if found.len() > MAX_TOKEN_REPAIRS || self.exhausted() {
                break;
            }
            // A removal must not join the words on both sides into one name (`abs(1;` without `(`).
            let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
            if removed > 0 && text[..at].ends_with(word) && text[at + removed..].starts_with(word) {
                continue;
            }
            // The same text can come from several edits (a token removed from a run of equal ones,
            // or an insertion and a removal that end up in the same place).
            let mut hasher = std::hash::DefaultHasher::new();
            format!("{}{}{}", &text[..at], with, &text[at + removed..]).hash(&mut hasher);
            if !seen.insert(hasher.finish()) {
                continue;
            }
            self.trials += 1;
            self.bytes += text.len();
            let works = parse_spliced(&mut self.parser, Some(&base), text, at, removed, &with).is_some_and(|tree| {
                let root = tree.root_node();
                // (An inserted keyword must be read as one: `END` and others may also be names.)
                let at_keyword = at + with.find(&token).unwrap_or(0);
                let keyword = kind == 0
                    || !token.starts_with(|c: char| c.is_ascii_uppercase())
                    || root
                        .descendant_for_byte_range(at_keyword, at_keyword + token.len())
                        .is_some_and(|n| n.kind() == token);
                keyword && Outcome::of(root).errors == 0
            });
            if works {
                let span = Span::new(start + at, start + at + removed);
                found.push(TokenEdit { span, with, token, inserted: kind == 1 });
            }
        }
        let Some(best) = found.first().cloned() else { return Vec::new() };
        // What the parser misread because of the token (nothing derived from the tree there can be trusted).
        let (at, removed) = (best.span.start - start, best.span.end - best.span.start);
        let misparsed = parse_spliced(&mut self.parser, Some(&base), text, at, removed, &best.with)
            .map(|tree| changed_between(base.root_node(), tree.root_node(), at, at + removed))
            .map(|span| Span::new(start + span.start, start + span.end));
        vec![KeywordTypo {
            span: best.span,
            word: if best.inserted { String::new() } else { best.token.clone() },
            keyword: if best.inserted { best.token.clone() } else { String::new() },
            alternatives: Vec::new(),
            certain: false,
            statement: Span::new(start, end),
            resolves: Some(Span::new(start, end)),
            misparsed,
            token_edits: found,
        }]
    }

    /// Misspelled keywords in the statement at `start..end`, which should
    /// parse by itself. The simplest explanation wins: one replacement that
    /// removes every error, then two that do together, and (if `partial`) only
    /// then one that removes the errors of its own line and leaves the rest.
    fn statement(&mut self, start: usize, end: usize, partial: bool) -> Vec<KeywordTypo> {
        let source = self.source;
        let text = &source[start..end];
        if text.len() > MAX_STATEMENT_BYTES || self.exhausted() {
            return Vec::new();
        }
        self.bytes += text.len();
        let Some(tree) = self.parser.parse(text, None) else {
            return Vec::new();
        };
        let root = tree.root_node();
        self.base = Some(tree.clone());
        let original = Outcome::of(root);
        let Some(anchor) = original.first_error else {
            return Vec::new();
        };
        let absolute = |span: Span| Span::new(start + span.start, start + span.end);
        let typo = |word: Node,
                    keyword: &str,
                    alternatives: Vec<String>,
                    certain: bool,
                    resolves: bool,
                    changed: Option<Span>| {
            KeywordTypo {
                span: absolute(Span::of(word)),
                word: syntax::text(word, text).to_string(),
                keyword: keyword.to_string(),
                certain: certain && alternatives.is_empty(),
                alternatives,
                statement: Span::new(start, end),
                resolves: resolves.then(|| Span::new(start, end)),
                misparsed: changed.map(absolute),
                token_edits: Vec::new(),
            }
        };
        let suspects = suspects(root, text, anchor);
        let mut budget = MAX_STATEMENT_TRIALS;
        let mut tried: Vec<Trial> = Vec::new();
        // 0. Several known joined keywords (a chain of `ELSEIF`, even on one line): one trial for all.
        let mut joined: Vec<(Node, String)> = suspects
            .iter()
            .filter_map(|&w| match replacements(syntax::text(w, text), &text[w.end_byte()..]) {
                (options, true) if options.len() == 1 => options.into_iter().next().map(|r| (w, r)),
                _ => None,
            })
            .collect();
        if joined.len() >= 2 {
            joined.sort_by_key(|(w, _)| w.start_byte());
            let edits: Vec<(Node, &str)> = joined.iter().map(|(w, r)| (*w, r.as_str())).collect();
            budget = budget.saturating_sub(1);
            if self.trial(text, &edits).is_some_and(|o| o.errors == 0) {
                let changed = self.changed(text, root, &edits);
                return joined.iter().map(|(w, r)| typo(*w, r, Vec::new(), true, true, changed)).collect();
            }
        }
        // 1. One replacement that removes every error. Suspects come nearest
        //    to the first error first, so the first word that works is the best.
        for &word in &suspects {
            let (options, certain) = replacements(syntax::text(word, text), &text[word.end_byte()..]);
            let mut complete: Vec<String> = Vec::new();
            for replacement in options {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let Some(outcome) = self.trial(text, &[(word, &replacement)]) else { continue };
                if outcome.errors == 0 {
                    complete.push(replacement);
                } else {
                    tried.push(Trial { word, replacement, certain, outcome });
                }
            }
            let is_keyword = keywords().iter().any(|k| k.eq_ignore_ascii_case(syntax::text(word, text)));
            if !complete.is_empty() && is_keyword {
                // A keyword replaced by another keyword (`INT` for `IN`) is a weak
                // explanation: a missing `,` is better, when it explains everything.
                if let Some(fix) = self.comma_fix(root, text, anchor, start, end) {
                    return vec![fix];
                }
            }
            if let Some((keyword, others)) = complete.split_first() {
                let changed = self.changed(text, root, &[(word, keyword)]);
                return vec![typo(word, keyword, others.to_vec(), certain, true, changed)];
            }
        }
        let inserted = self.block_keyword_fix(root, text, start, end);
        if !inserted.is_empty() {
            return inserted;
        }
        if let Some(fix) = self.comma_fix(root, text, anchor, start, end) {
            return vec![fix];
        }
        // 2. Two misspelled words: a replacement that moves the first error
        //    past its own line, and a later one that removes every error.
        let pushed = tried.iter().filter(|t| {
            let line_end = text[t.word.end_byte()..].find('\n').map_or(text.len(), |i| t.word.end_byte() + i);
            t.outcome.errors <= original.errors && t.outcome.first_error.is_some_and(|e| e > line_end)
        });
        for first in pushed.take(3) {
            for &second in suspects.iter().filter(|s| s.start_byte() > first.word.end_byte()) {
                let (options, second_certain) = replacements(syntax::text(second, text), &text[second.end_byte()..]);
                for replacement in options {
                    if budget == 0 {
                        break;
                    }
                    budget -= 1;
                    let edits = [(first.word, first.replacement.as_str()), (second, replacement.as_str())];
                    if self.trial(text, &edits).is_some_and(|o| o.errors == 0) {
                        let changed = self.changed(text, root, &edits);
                        // Two known joined keywords (`ELSEIF` twice) are as certain as one.
                        let certain = first.certain && second_certain;
                        return vec![
                            typo(first.word, &first.replacement, Vec::new(), certain, true, changed),
                            typo(second, &replacement, Vec::new(), certain, true, changed),
                        ];
                    }
                }
            }
        }
        if !partial {
            return Vec::new();
        }
        // 3. One token removed or inserted that removes every error.
        let fix = self.token_fix(text, start, end, &original);
        if !fix.is_empty() {
            return fix;
        }
        // 4. A replacement that removes the errors of its own line without
        //    causing others (the statement has another mistake too).
        let mut found: Vec<(usize, &Trial, Vec<String>)> = Vec::new();
        for t in &tried {
            let row = t.word.start_position().row;
            // The replacement makes the parse get past the word's line; what
            // errors remain (e.g. a missing `;` further on) are other mistakes.
            let local =
                original.error_rows.contains(&row) && t.outcome.error_rows.first().is_none_or(|first| *first > row);
            if !local || t.outcome.errors >= original.errors {
                continue;
            }
            match found.iter_mut().find(|(r, ..)| *r == row) {
                // One word per line: the one nearest to the first error.
                Some((_, best, alternatives)) if best.word == t.word => alternatives.push(t.replacement.clone()),
                Some(_) => {}
                None => found.push((row, t, Vec::new())),
            }
        }
        found
            .into_iter()
            .map(|(_, t, alternatives)| {
                let changed = self.changed(text, root, &[(t.word, &t.replacement)]);
                typo(t.word, &t.replacement, alternatives, t.certain, false, changed)
            })
            .collect()
    }
}

/// Where `THEN` or `DO` could be missing: after the condition of an IF, ELSE
/// IF or WHILE, or the collection of a FOREACH, that the parser read inside an
/// error and followed by something that is not the keyword. As (offset, text
/// to insert), in document order.
fn block_keyword_candidates(root: Node) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    syntax::walk(root, |node| {
        if !node.has_error() {
            return;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).filter(|c| !c.is_extra()).collect();
        for (i, child) in children.iter().enumerate() {
            if child.child_count() > 0 {
                continue;
            }
            let (keyword, from, until) = match child.kind() {
                "IF" | "ELSE IF" => (" THEN ", i + 1, ["THEN", ""]),
                "WHILE" => (" DO ", i + 1, ["DO", "LIMIT"]),
                "FOREACH" => {
                    let Some(position) = children[i + 1..].iter().take(8).position(|c| c.kind() == "IN") else {
                        continue;
                    };
                    (" DO ", i + position + 2, ["DO", ""])
                }
                _ => continue,
            };
            let [Some(condition), after] = [children.get(from), children.get(from + 1)] else { continue };
            if !condition.is_named() || condition.has_error() {
                continue;
            }
            if after.is_some_and(|a| a.kind() == until[0] || a.kind() == until[1]) {
                continue;
            }
            found.push((condition.end_byte(), keyword));
        }
    });
    // A condition on one line (most are) may sit in an error the parser made
    // a mess of: then the keyword goes at the end of the line.
    let mut leaves: Vec<Node> = Vec::new();
    syntax::walk(root, |n| {
        if n.child_count() == 0 && !n.is_extra() && n.end_byte() > n.start_byte() {
            leaves.push(n);
        }
    });
    for (i, leaf) in leaves.iter().enumerate() {
        let (keyword, closers) = match leaf.kind() {
            "IF" | "ELSE IF" => (" THEN ", ["THEN", "DO"]),
            "WHILE" | "FOREACH" => (" DO ", ["DO", "THEN"]),
            _ => continue,
        };
        let row = leaf.start_position().row;
        let line: Vec<&Node> = leaves[i + 1..].iter().take_while(|n| n.start_position().row == row).collect();
        if let Some(last) = line.last()
            && !line.iter().any(|n| closers.contains(&n.kind()) || n.is_error() || n.is_missing())
            && last.kind() != ";"
        {
            found.push((last.end_byte(), keyword));
        }
    }
    found.sort_by_key(|c| c.0);
    found.dedup_by_key(|c| c.0);
    found
}

/// Words of a statement that may be misspelled keywords: identifiers and
/// words the parser could not place before keywords it did, each nearest to
/// the first syntax error (at `anchor`) first.
fn suspects<'t>(root: Node<'t>, text: &str, anchor: usize) -> Vec<Node<'t>> {
    let literals = literal_spans(text);
    let mut words = Vec::new();
    let mut occurrences: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    syntax::walk_with_errors(root, |n, in_error| {
        if is_word(n, text) && !literals.iter().any(|&(from, to)| from <= n.start_byte() && n.start_byte() < to) {
            let placed = !n.is_named() && !in_error;
            *occurrences.entry(syntax::text(n, text).to_ascii_lowercase()).or_default() += 1;
            words.push((placed, n.start_byte().abs_diff(anchor), n));
        }
    });
    // A misspelled keyword is usually written once; names come back. In a big
    // statement whose error is reported at its start, that tells them apart
    // (but a word right at the error counts, whatever else it is used for:
    // `d` may be a vertex alias elsewhere and a misspelled `DO` here).
    const NEAR: usize = 120;
    words.sort_by_key(|&(placed, distance, n)| {
        let repeated = distance > NEAR && occurrences[&syntax::text(n, text).to_ascii_lowercase()] > 1;
        (placed, repeated, distance)
    });
    words
        .into_iter()
        .map(|(.., n)| n)
        .take(600)
        .filter(|n| !replacements(syntax::text(*n, text), &text[n.end_byte()..]).0.is_empty())
        .take(40)
        .collect()
}

/// Misspelled keywords that cause syntax errors. Each broken top-level
/// statement is parsed again with words near its first error replaced by
/// similar keywords (statements are independent, so this stays fast in large
/// files).
fn keyword_typos(root: Node, source: &str) -> Vec<KeywordTypo> {
    if !root.has_error() {
        return Vec::new();
    }
    let mut search = Search::new(source);
    let mut typos = Vec::new();
    for (start, end, broken) in statements(root) {
        if broken {
            typos.extend(search.statement(start, end, true));
        }
    }
    typos
}

/// The first leaf that starts after `node` ends.
fn next_leaf(node: Node) -> Option<Node> {
    let mut current = node;
    loop {
        if let Some(next) = current.next_sibling() {
            let mut leaf = next;
            while let Some(first) = leaf.child(0) {
                leaf = first;
            }
            return Some(leaf);
        }
        current = current.parent()?;
    }
}

/// Where a missing `,` could be: the start of the first few tokens from the
/// point where the parser reported its first error (it may report the error
/// at an item that is already past the place where the `,` belongs).
pub fn comma_offsets(root: Node, anchor: usize) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut leaf = root.descendant_for_byte_range(anchor, anchor);
    while let Some(mut current) = leaf {
        while let Some(first) = current.child(0) {
            current = first;
        }
        if current.start_byte() >= anchor && current.end_byte() > current.start_byte() {
            offsets.push(current.start_byte());
        }
        if offsets.len() >= 6 {
            break;
        }
        leaf = next_leaf(current);
    }
    offsets
}

/// A statement with more syntax errors than this has a bigger problem than a
/// missing comma (an unclosed bracket, say): no comma is suggested for it.
const MAX_ERRORS_FOR_COMMA: usize = 4;

/// The first place among the tokens from `anchor` where inserting a `,`
/// leaves fewer syntax errors in the statement (which has only a few).
pub fn missing_comma_at(root: Node, source: &str, anchor: usize) -> Option<usize> {
    let (start, end, _) = statements(root).into_iter().find(|(s, e, _)| *s <= anchor && anchor <= *e)?;
    if end - start > MAX_STATEMENT_BYTES {
        return None;
    }
    let text = &source[start..end];
    let mut parser = syntax::new_parser();
    let mut count = |text: &str| parser.parse(text, None).map(|tree| Outcome::of(tree.root_node()).errors);
    let before = count(text)?;
    if before > MAX_ERRORS_FOR_COMMA {
        return None;
    }
    comma_offsets(root, anchor).into_iter().find(|&at| {
        // (An offset may lie outside the statement, or inside a character.)
        if at < start || at > end || !source.is_char_boundary(at) {
            return false;
        }
        let at_in_statement = at - start;
        count(&format!("{}, {}", &text[..at_in_statement], &text[at_in_statement..]))
            .is_some_and(|after| after < before)
    })
}

/// Whether the statement around the `,` at `at` has fewer syntax errors
/// without it (a trailing comma).
pub fn removing_comma_helps(root: Node, source: &str, at: usize) -> bool {
    comma_removal(root, source, at).is_some()
}

/// The statement around the `,` at `at` when it has no syntax error left
/// without the comma: the errors in it are consequences of the comma.
pub fn removing_comma_resolves(root: Node, source: &str, at: usize) -> Option<Span> {
    comma_removal(root, source, at).filter(|(_, after)| *after == 0).map(|(span, _)| span)
}

/// The statement around the `,` at `at` and its number of syntax errors
/// without the comma, when that is fewer than with it.
fn comma_removal(root: Node, source: &str, at: usize) -> Option<(Span, usize)> {
    let (start, end, _) = statements(root).into_iter().find(|(s, e, _)| *s <= at && at < *e)?;
    if end - start > MAX_STATEMENT_BYTES || source.as_bytes().get(at) != Some(&b',') {
        return None;
    }
    let text = &source[start..end];
    let mut parser = syntax::new_parser();
    let mut count = |text: &str| parser.parse(text, None).map(|tree| Outcome::of(tree.root_node()).errors);
    let without = format!("{}{}", &text[..at - start], &text[at - start + 1..]);
    match (count(text), count(&without)) {
        (Some(before), Some(after)) if after < before => Some((Span::new(start, end), after)),
        _ => None,
    }
}

/// Shell commands end at the end of their line (only BEGIN ... END makes a
/// command span lines), but commands with free-form arguments such as SHOW or
/// GRANT read the next line as more arguments. The commands that absorbed a
/// later line that is not indented past them, with the first token of it.
pub fn absorbing_commands<'t>(root: Node<'t>, source: &str) -> Vec<(Node<'t>, Node<'t>)> {
    let mut cursor = root.walk();
    let statements: Vec<Node> = root.children(&mut cursor).collect();
    let mut inside_begin = false;
    let mut commands = Vec::new();
    for statement in statements {
        if statement.kind() == "shell_command" {
            // `BEGIN` ... `END` blocks may span lines.
            match syntax::text(statement, source).trim().to_ascii_uppercase().as_str() {
                "BEGIN" => inside_begin = true,
                "END" | "ABORT" => inside_begin = false,
                _ => {}
            }
        }
        let free_form = matches!(
            statement.kind(),
            "show_statement" | "security_statement" | "shell_command" | "grant_statement" | "revoke_statement"
        );
        let first_row = statement.start_position().row;
        if !free_form || statement.end_position().row == first_row || inside_begin {
            continue;
        }
        let indent = statement.start_position().column;
        let mut absorbed = None;
        syntax::walk(statement, |n| {
            let starts_line = || source[..n.start_byte()].rsplit('\n').next().is_some_and(|s| s.trim().is_empty());
            if absorbed.is_none()
                && n.child_count() == 0
                && n.kind() != "comment"
                && n.start_position().row > first_row
                && n.start_position().column <= indent
                && starts_line()
            {
                absorbed = Some(n);
            }
        });
        if let Some(absorbed) = absorbed {
            commands.push((statement, absorbed));
        }
    }
    commands
}

/// Misspelled keywords that explain syntax errors: in shell commands that
/// absorbed the next lines because of one, and in broken statements.
pub fn typos_in(root: Node, source: &str) -> Vec<KeywordTypo> {
    let mut typos = Vec::new();
    let groups = statements(root);
    for (command, _) in absorbing_commands(root, source) {
        let line_end = source[command.start_byte()..].find('\n').map_or(source.len(), |i| command.start_byte() + i);
        let found = Search::new(source).statement(command.start_byte(), line_end, false);
        if found.is_empty() {
            continue;
        }
        // The pieces after the command are usually consequences too.
        let (start, end) = groups
            .iter()
            .find(|g| g.0 <= command.start_byte() && command.end_byte() <= g.1)
            .map_or((command.start_byte(), command.end_byte()), |g| (g.0, g.1));
        let explained =
            if parses_corrected(source, start, end, &found) { Span::new(start, end) } else { Span::of(command) };
        typos.extend(found.into_iter().map(|typo| KeywordTypo {
            statement: explained,
            resolves: Some(explained),
            misparsed: Some(explained),
            ..typo
        }));
    }
    let explained: Vec<Span> = typos.iter().filter_map(|t| t.resolves).collect();
    let found = keyword_typos(root, source);
    typos.extend(found.into_iter().filter(|t| !explained.iter().any(|s| s.contains_span(t.span))));
    typos
}

/// Whether `start..end` of `source` parses without errors once `typos` are
/// corrected.
fn parses_corrected(source: &str, start: usize, end: usize, typos: &[KeywordTypo]) -> bool {
    let mut text = String::with_capacity(end - start + 16);
    let mut last = start;
    for typo in typos {
        if typo.span.start < last || typo.span.end > end {
            return false;
        }
        text.push_str(&source[last..typo.span.start]);
        text.push_str(&typo.keyword);
        last = typo.span.end;
    }
    text.push_str(&source[last..end]);
    syntax::new_parser().parse(&text, None).is_some_and(|tree| !tree.root_node().has_error())
}

/// The misspelled keywords of a document and its repair, remembered for the
/// last document asked about: the workspace index and the diagnostics of a
/// change both need them, one after the other.
pub fn typos_and_repair(
    root: Node,
    source: &SourceText,
    encoding: PositionEncoding,
) -> (Rc<Vec<KeywordTypo>>, Option<Rc<Repair>>) {
    type Entry = (u64, Rc<Vec<KeywordTypo>>, Option<Rc<Repair>>);
    thread_local! {
        static LAST: RefCell<Option<Entry>> = const { RefCell::new(None) };
    }
    let mut hasher = std::hash::DefaultHasher::new();
    (&source.text, encoding as u8).hash(&mut hasher);
    let key = hasher.finish();
    if let Some((_, typos, repair)) = LAST.with_borrow(|last| last.as_ref().filter(|e| e.0 == key).cloned()) {
        return (typos, repair);
    }
    let typos = Rc::new(typos_in(root, &source.text));
    let repair = Repair::new(source, &typos, encoding).map(Rc::new);
    LAST.set(Some((key, typos.clone(), repair.clone())));
    (typos, repair)
}

/// The repair of a document that has syntax errors, for features that find
/// nothing in the text as written.
pub fn repair_of(snapshot: &Snapshot) -> Option<Rc<Repair>> {
    if !snapshot.root().has_error() {
        return None;
    }
    typos_and_repair(snapshot.root(), snapshot.source, snapshot.encoding).1
}

/// Documents larger than this are not repaired (analysis is the costly part).
const MAX_REPAIR_BYTES: usize = 512 << 10;

/// A document with the misspelled keywords that explain whole statements
/// corrected, so that analysis is not thrown off by them: a typo in a vertex
/// type's declaration should not make every use of the type "unknown".
pub struct Repair {
    pub source: SourceText,
    pub tree: Tree,
    pub analysis: Analysis,
    /// (line, character, written length, keyword length) of each correction,
    /// in document order, in the original's coordinates.
    corrections: Vec<(u32, u32, u32, u32)>,
}

impl Repair {
    /// `None` when no typo explains a whole statement.
    pub fn new(original: &SourceText, typos: &[KeywordTypo], encoding: PositionEncoding) -> Option<Repair> {
        // (Removing or inserting a token is a guess too uncertain to analyze the document as if it was made.)
        let mut fixes: Vec<&KeywordTypo> = typos.iter().filter(|t| t.token_edits.is_empty()).collect();
        if fixes.is_empty() || original.text.len() > MAX_REPAIR_BYTES {
            return None;
        }
        fixes.sort_by_key(|t| t.span.start);
        fixes.dedup_by_key(|t| t.span.start);
        let mut text = String::with_capacity(original.text.len() + 16);
        let mut last = 0;
        let mut corrections = Vec::new();
        for typo in fixes {
            if typo.span.start < last || typo.keyword.contains('\n') {
                continue;
            }
            text.push_str(&original.text[last..typo.span.start]);
            text.push_str(&typo.keyword);
            last = typo.span.end;
            let at = original.range(typo.span, encoding).start;
            // Keywords are ASCII: their length is the same in every encoding.
            corrections.push((at.line, at.character, typo.word.len() as u32, typo.keyword.len() as u32));
        }
        text.push_str(&original.text[last..]);
        let tree = syntax::parse(&mut syntax::new_parser(), &text, None);
        let analysis = crate::analysis::analyze(&tree, &text);
        Some(Repair { source: SourceText::new(text), tree, analysis, corrections })
    }

    /// Where a position of the repaired document is in the original.
    pub fn position(&self, position: Position) -> Position {
        let mut shift: i64 = 0;
        let character = i64::from(position.character);
        for &(_, column, written, keyword) in self.corrections.iter().filter(|c| c.0 == position.line) {
            let start = i64::from(column) + shift;
            if character < start {
                break;
            }
            if character < start + i64::from(keyword) {
                let inside = (character - start).min(i64::from(written));
                return Position { line: position.line, character: (i64::from(column) + inside) as u32 };
            }
            shift += i64::from(keyword) - i64::from(written);
        }
        Position { line: position.line, character: (character - shift).max(0) as u32 }
    }

    pub fn range(&self, range: Range) -> Range {
        Range { start: self.position(range.start), end: self.position(range.end) }
    }

    /// Where a position of the original document is in the repaired one.
    pub fn to_repaired(&self, position: Position) -> Position {
        let mut shift: i64 = 0;
        let character = i64::from(position.character);
        for &(_, column, written, keyword) in self.corrections.iter().filter(|c| c.0 == position.line) {
            let column = i64::from(column);
            if character < column {
                break;
            }
            if character < column + i64::from(written) {
                let inside = (character - column).min(i64::from(keyword));
                return Position { line: position.line, character: (column + shift + inside) as u32 };
            }
            shift += i64::from(keyword) - i64::from(written);
        }
        Position { line: position.line, character: (character + shift).max(0) as u32 }
    }

    /// The same snapshot, of the corrected document.
    pub fn snapshot<'a>(&'a self, snapshot: &Snapshot<'a>) -> Snapshot<'a> {
        Snapshot { source: &self.source, tree: &self.tree, analysis: &self.analysis, ..*snapshot }
    }

    /// A diagnostic of the repaired document, placed in the original.
    pub fn diagnostic(&self, mut diagnostic: Diagnostic) -> Diagnostic {
        diagnostic.range = self.range(diagnostic.range);
        let fixes = diagnostic.data.as_mut().and_then(|d| d.get_mut("fixes")).and_then(|f| f.as_array_mut());
        for fix in fixes.into_iter().flatten() {
            let edits = fix.get_mut("edits").and_then(|e| e.as_array_mut());
            for edit in edits.into_iter().flatten() {
                if let Some(range) = edit.get("range").and_then(|r| serde_json::from_value::<Range>(r.clone()).ok()) {
                    edit["range"] = serde_json::to_value(self.range(range)).unwrap_or_default();
                }
            }
        }
        diagnostic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;

    fn typos(text: &str) -> Vec<(String, String)> {
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        typos_in(snapshot.root(), snapshot.text()).into_iter().map(|t| (t.word, t.keyword)).collect()
    }

    #[test]
    fn a_token_repair_never_joins_two_words() {
        let text = "CREATE QUERY q() {\n  PRINT abs(1;\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let found = typos_in(snapshot.root(), snapshot.text());
        let edits: Vec<String> =
            found.iter().flat_map(|t| t.token_edits.iter().map(|e| format!("{}{}", e.inserted, e.token))).collect();
        assert!(edits.contains(&"true)".to_string()), "{edits:?}");
        assert!(!edits.contains(&"false(".to_string()), "removing `(` would make `abs1`: {edits:?}");
        // The same text is offered once.
        let mut unique = edits.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), edits.len(), "{edits:?}");
    }

    #[test]
    fn words_in_strings_and_comments_are_not_keywords() {
        let text = "CREATE QUERY q(INT a) {\n  PRINT \"ELSEIF\", \"ELSEIF\"; // ELSEIF\n  /* ELSEIF */\n  IF a==0 THEN PRINT 0; ELSEIF a==1 THEN PRINT 1; ELSEIF a==2 THEN PRINT 2; END;\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let found = typos_in(snapshot.root(), snapshot.text());
        assert!(!found.is_empty(), "the real chain is found");
        for typo in &found {
            let line = snapshot.text()[..typo.span.start].matches('\n').count();
            assert_eq!(line, 3, "only the ELSEIFs of the chain: {:?}", typo.word);
        }
    }

    #[test]
    fn measures_edit_distance_with_transpositions() {
        assert_eq!(distance("FORM", "FROM"), 1);
        assert_eq!(distance("edn", "END"), 1);
        assert_eq!(distance("SELCT", "SELECT"), 1);
        assert_eq!(distance("abc", "xyz"), 3);
    }

    #[test]
    fn finds_misspelled_keywords() {
        let cases = [
            ("CREATE QUERY q() {\n  R = SELECT s FORM P:s;\n  PRINT R;\n}\n", ("FORM", "FROM")),
            ("CREATE QUERY q() {\n  R = SELCT s FROM P:s;\n  PRINT R;\n}\n", ("SELCT", "SELECT")),
            ("CREATE QUERY q() {\n  R = SELECT s FROM P:s WHER s.x > 1;\n  PRINT R;\n}\n", ("WHER", "WHERE")),
            ("CREATE QUERY q(INT x) {\n  IF x > 1 THN PRINT x; END;\n}\n", ("THN", "THEN")),
            ("CREATE QUERY q() {\n  FOREACH x IN RANGE[1, 2] DO PRINT x; edn;\n}\n", ("edn", "end")),
            ("CRATE QUERY q() { PRINT 1; }\n", ("CRATE", "CREATE")),
        ];
        for (text, (word, keyword)) in cases {
            assert_eq!(typos(text), [(word.to_string(), keyword.to_string())], "{text}");
        }
    }

    #[test]
    fn a_typo_in_a_query_header_is_the_only_finding() {
        let example = include_str!("../../../../examples/algorithms/k_hop.gsql");
        let broken = example.replacen("CREATE QUERY", "CREATE QUEERY", 1);
        let fixture = Fixture::new(&broken);
        let found: Vec<String> =
            crate::features::diagnostics::diagnostics(&fixture.snapshot()).into_iter().map(|d| d.message).collect();
        assert_eq!(found, ["Syntax error: did you mean `QUERY` instead of `QUEERY`?"]);
    }

    fn syntax_messages(text: &str) -> Vec<String> {
        let fixture = Fixture::new(text);
        crate::features::diagnostics::syntax_errors(&fixture.snapshot()).into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn finds_a_typo_after_the_parser_gave_up_on_the_whole_file() {
        // The misspelled END of a CASE in POST-ACCUM turns the whole file into
        // one ERROR node that starts at the first line.
        let example = include_str!("../../../../examples/finance/fraud.gsql");
        let broken = example.replacen("               END;", "               TND;", 1);
        assert_eq!(syntax_messages(&broken), ["Syntax error: did you mean `END` instead of `TND`?"]);
    }

    #[test]
    fn finds_a_typo_that_makes_a_command_absorb_the_next_lines() {
        let text = "REVOKE ROLE analyst ON GRAPH Social FORM bob\nSHOW ROLE\nINSTALL QUERY -OPTIMIZE q\n";
        assert_eq!(syntax_messages(text), ["Syntax error: did you mean `FROM` instead of `FORM`?"]);
    }

    #[test]
    fn leaves_correct_statements_next_to_a_typo_alone() {
        // `note` could become `NOT`, and the `CREATE VERTEX` line parses either way.
        let text = "BEGIN\nCREATE VERTEX Scratch (PRIMARY_ID id INT, note STRING)\nENE\n";
        assert_eq!(syntax_messages(text), ["Syntax error: did you mean `END` instead of `ENE`?"]);
        let text = "CRETE SCHEMA_CHANGE JOB j FOR GRAPH g {\n  DROP EDGE Tagged;\n  DROP VERTEX Tag;\n}\n";
        assert_eq!(syntax_messages(text), ["Syntax error: did you mean `CREATE` instead of `CRETE`?"]);
    }

    #[test]
    fn maps_positions_of_the_repaired_document_back() {
        // Two corrections on one line: `SELCT` (+1) and `FORM` (+0); the line
        // after them is untouched.
        let text = "CREATE QUERY q() {\n  R = SELCT s FORM P:s WHERE s.x > 1;\n  PRINT R;\n}\n";
        let source = SourceText::new(text.to_string());
        let typo = |word: &str, keyword: &str| {
            let start = text.find(word).unwrap();
            KeywordTypo {
                span: Span::new(start, start + word.len()),
                word: word.to_string(),
                keyword: keyword.to_string(),
                alternatives: Vec::new(),
                certain: false,
                statement: Span::new(0, text.len()),
                resolves: Some(Span::new(0, text.len())),
                misparsed: None,
                token_edits: Vec::new(),
            }
        };
        let corrected = [typo("SELCT", "SELECT"), typo("FORM", "FROM")];
        let repair = Repair::new(&source, &corrected, PositionEncoding::Utf16).expect("repaired");
        assert!(repair.source.text.contains("R = SELECT s FROM P:s"), "{}", repair.source.text);
        let at = |line, character| repair.position(Position { line, character });
        // Before the corrections, inside them (the last letter of `SELECT`
        // maps to the end of `SELCT`), after them.
        assert_eq!(at(1, 2), Position { line: 1, character: 2 });
        assert_eq!(at(1, 7), Position { line: 1, character: 7 });
        assert_eq!(at(1, 11), Position { line: 1, character: 11 });
        assert_eq!(at(1, 13), Position { line: 1, character: 12 });
        let where_in_repaired = repair.source.text.lines().nth(1).unwrap().find("WHERE").unwrap() as u32;
        let where_in_original = text.lines().nth(1).unwrap().find("WHERE").unwrap() as u32;
        assert_eq!(at(1, where_in_repaired), Position { line: 1, character: where_in_original });
        assert_eq!(at(2, 4), Position { line: 2, character: 4 });
    }

    #[test]
    fn finds_misspelled_accumulator_types_and_a_typo_next_to_another_mistake() {
        let text = "CREATE QUERY q() {\n  SumAcum<INT> @@n;\n  PRINT @@n;\n}\n";
        assert_eq!(typos(text), [("SumAcum".to_string(), "SumAccum".to_string())]);
        // A typo and an unrelated missing `;` are both reported.
        let text = "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  R = SELECT s FORM P:s ACCUM @@n += 1;\n  INT y = 1\n  PRINT R, y;\n}\n";
        assert_eq!(
            syntax_messages(text),
            ["Syntax error: did you mean `FROM` instead of `FORM`?", "Syntax error: missing `;` after this statement"]
        );
    }

    #[test]
    fn finds_two_letter_keywords_and_typos_far_from_the_reported_error() {
        let job = "CREATE LOADING JOB j FOR GRAPH g {\n  LOAD f SO VERTEX P VALUES ($0);\n}\n";
        assert_eq!(typos(job), [("SO".to_string(), "TO".to_string())]);
        // The parser reports the error at the start of this long query, far from the typo.
        let body: String = (0..60).map(|i| format!("  INT v{i} = {i};\n  PRINT v{i};\n")).collect();
        let query = format!("CREATE QUERY q(BOOL a) {{\n{body}  IF a THEN PRINT 1; EDN;\n  PRINT 2;\n}}\n");
        assert_eq!(typos(&query), [("EDN".to_string(), "END".to_string())]);
    }

    /// The repairs of one token found for `text`: (removed or inserted, token, text after the edit).
    fn token_repairs(text: &str) -> Vec<(bool, String, String)> {
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let found = typos_in(snapshot.root(), snapshot.text());
        found
            .iter()
            .flat_map(|t| &t.token_edits)
            .map(|e| {
                let mut edited = text.to_string();
                edited.replace_range(e.span.start..e.span.end, &e.with);
                (e.inserted, e.token.clone(), edited)
            })
            .collect()
    }

    #[test]
    fn finds_one_token_that_is_stray_or_missing() {
        let stray = "CREATE QUERY q() {\n  INT x = 1;\n  PRINT x x;\n}\n";
        let found = token_repairs(stray);
        assert_eq!(found[0], (false, "x".to_string(), "CREATE QUERY q() {\n  INT x = 1;\n  PRINT x;\n}\n".to_string()));
        let missing = "CREATE QUERY q() {\n  INT x = abs(1;\n  PRINT x;\n}\n";
        let found = token_repairs(missing);
        assert_eq!(found[0].1, ")");
        assert_eq!(found[0].2, "CREATE QUERY q() {\n  INT x = abs(1);\n  PRINT x;\n}\n");
        // Every repair leaves a statement without syntax errors.
        for (_, _, edited) in token_repairs(missing).iter().chain(&token_repairs(stray)) {
            let tree = syntax::parse(&mut syntax::new_parser(), edited, None);
            assert!(!tree.root_node().has_error(), "{edited}");
        }
    }

    #[test]
    fn does_not_remove_a_token_when_a_value_is_missing() {
        // `PRINT ;` lacks an expression: removing `PRINT` is no explanation, and neither is
        // inserting `END` (a name here, not the keyword).
        assert!(token_repairs("CREATE QUERY q() {\n  PRINT ;\n}\n").is_empty());
    }

    #[test]
    fn two_mistakes_in_one_statement_have_no_one_token_repair() {
        let text = "CREATE QUERY q() {\n  INT x = abs(1;\n  PRINT x x;\n}\n";
        assert!(token_repairs(text).is_empty());
    }

    #[test]
    fn leaves_correct_code_alone() {
        assert!(typos("CREATE QUERY q() {\n  R = SELECT s FROM P:s;\n  PRINT R;\n}\n").is_empty());
        // An error that no keyword fixes.
        let found = typos("CREATE QUERY q() {\n  PRINT ;\n}\n");
        assert!(found.is_empty(), "{found:?}");
    }
}
