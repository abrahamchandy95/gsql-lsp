//! Syntax errors from the parse tree and conservative semantic checks.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use serde_json::json;
use tree_sitter::Node;

use crate::analysis::{
    Context, FILE_SCOPE, Param, Role, ScopeKind, SymbolKind, Ty,
};
use crate::builtin_docs;
use crate::builtins;
use crate::features::autocorrect::{KeywordTypo, Repair};
use crate::features::{Snapshot, plural, resolve};
use crate::lsp::types::{
    Diagnostic, Location, Range, RelatedInformation, TextEdit,
    diagnostic_tag, severity,
};
use crate::syntax;
use crate::text::{
    Span, is_word_char, starts_with_ignore_ascii_case, word_offsets,
};

const SOURCE: &str = "gsql";
const MAX_SYNTAX_ERRORS: usize = 100;

pub fn diagnostics(snapshot: &Snapshot) -> Vec<Diagnostic> {
    let (mut diagnostics, typos, repair) = syntax_diagnostics(snapshot);
    // Statements broken by a misspelled keyword are checked as corrected.
    let semantic = match repair {
        Some(repair) => {
            let analysis = repair.analysis(snapshot.workspace);
            semantic(&snapshot.with_document(
                &repair.source,
                &repair.tree,
                &analysis,
            ))
            .into_iter()
            .map(|d| repair.diagnostic(d))
            .collect()
        }
        None => semantic(snapshot),
    };
    // Other semantic findings on a line with a syntax error are usually
    // artifacts of error recovery, and so is everything on the lines that a
    // keyword typo made the parser misread; they reappear once it is fixed.
    // The style hints are lexical and read the text as written; they are held
    // back from those lines the same way.
    let style = if snapshot.config.diagnostics_style {
        super::style::check(snapshot)
    } else {
        Vec::new()
    };
    let broken_lines: HashSet<u32> = diagnostics
        .iter()
        .map(|d| d.range.start.line)
        .collect();
    let misparsed: Vec<Range> = typos
        .iter()
        .filter(|t| t.resolves.is_none() || !t.token_edits.is_empty())
        .filter_map(|t| t.misparsed)
        .map(|span| snapshot.range(span))
        .collect();
    diagnostics.extend(semantic.into_iter().chain(style).filter(
        |d: &Diagnostic| {
            !broken_lines.contains(&d.range.start.line)
                && !misparsed
                    .iter()
                    .any(|r| r.contains_lines_of(d.range))
        },
    ));
    diagnostics
}

pub(crate) fn diagnostic(
    snapshot: &Snapshot,
    span: Span,
    level: u8,
    code: &str,
    message: String,
) -> Diagnostic {
    Diagnostic {
        range: snapshot.range(span),
        severity: Some(level),
        code: Some(code.to_string()),
        source: Some(SOURCE.to_string()),
        message,
        tags: Vec::new(),
        data: None,
        related_information: Vec::new(),
    }
}

/// Attaches a quick fix to a diagnostic; code actions read it back from the
/// diagnostic's data. `safe` fixes are also applied by "fix all".
pub(crate) fn add_fix(
    diagnostic: &mut Diagnostic,
    title: impl Into<String>,
    edits: Vec<TextEdit>,
    safe: bool,
) {
    let fix = json!({ "title": title.into(), "edits": edits, "safe": safe });
    let data = diagnostic
        .data
        .get_or_insert_with(|| json!({}));
    if let Some(object) = data.as_object_mut()
        && let Some(fixes) = object
            .entry("fixes")
            .or_insert_with(|| json!([]))
            .as_array_mut()
    {
        fixes.push(fix);
    }
}

// ---------------------------------------------------------------------------
// Syntax errors
// ---------------------------------------------------------------------------

pub fn syntax_errors(snapshot: &Snapshot) -> Vec<Diagnostic> {
    syntax_diagnostics(snapshot).0
}

/// The lines where a tree has syntax errors: the first token of each ERROR
/// node (as reported) and each MISSING node.
fn error_lines(tree_root: Node) -> HashSet<u32> {
    let mut lines = HashSet::new();
    syntax::walk(tree_root, |n| {
        if n.is_missing() {
            lines.insert(n.start_position().row as u32);
        } else if n.is_error() {
            let first = syntax::first_leaf(n);
            lines.insert(first.start_position().row as u32);
            if let Some(last) =
                unfinished_at(tree_root, n.start_byte(), n.end_byte())
            {
                lines.insert(last.start_position().row as u32);
            }
        }
    });
    if tree_root.is_error()
        && let Some(start) = last_definition_start(tree_root)
        && let Some(last) =
            unfinished_at(tree_root, start, tree_root.end_byte())
    {
        lines.insert(last.start_position().row as u32);
    }
    lines
}

/// Where the last definition starts in a root the parser gave up on: its last
/// `CREATE` or `INTERPRET` token (the parser may have nested the pieces of an
/// earlier definition's error around it).
fn last_definition_start(root: Node) -> Option<usize> {
    let mut start = None;
    syntax::walk(root, |n| {
        if n.child_count() == 0 && matches!(n.kind(), "CREATE" | "INTERPRET")
        {
            start =
                Some(start.map_or(n.start_byte(), |s: usize| {
                    s.max(n.start_byte())
                }));
        }
    });
    start
}

/// The last token of a definition (`CREATE ...`, `INTERPRET ...`) that the
/// parser gave up on, when the input ends inside it: the error covers the text
/// up to the last code of the document, and brackets are left open. The
/// parser swallows such a definition whole, so what it reports is the first
/// token; the place where the text stops is where the mistake is.
fn unfinished_at<'t>(
    root: Node<'t>,
    start: usize,
    end: usize,
) -> Option<Node<'t>> {
    let mut first: Option<Node> = None;
    let mut last: Option<Node> = None;
    let document_end = last_code_end(root);
    let (mut braces, mut parens) = (0i64, 0i64);
    // (Only the part of the tree that holds the definition, not all of it, for every error.)
    let scope = root
        .descendant_for_byte_range(start, end)
        .unwrap_or(root);
    syntax::walk(scope, |n| {
        if !syntax::is_code_token(n) {
            return;
        }
        if n.start_byte() < start || n.end_byte() > end {
            return;
        }
        first.get_or_insert(n);
        last = Some(n);
        match n.kind() {
            "{" => braces += 1,
            "}" => braces -= 1,
            "(" => parens += 1,
            ")" => parens -= 1,
            _ => {}
        }
    });
    let last = last?;
    let starts_definition =
        first.is_some_and(|f| matches!(f.kind(), "CREATE" | "INTERPRET"));
    (starts_definition
        && last.end_byte() == document_end
        && (braces > 0 || parens > 0))
        .then_some(last)
}

thread_local! {
    /// (node id, end, result) of the last tree asked about.
    static LAST: std::cell::Cell<Option<(usize, usize, usize)>> = const { std::cell::Cell::new(None) };
}

/// Where the last code of the document (not a comment) ends. Remembered for the
/// tree being examined (every error of a tree asks), see [`forget_last_code_end`].
fn last_code_end(root: Node) -> usize {
    if let Some((id, end, found)) = LAST.get()
        && id == root.id()
        && end == root.end_byte()
    {
        return found;
    }
    let mut found = 0;
    syntax::walk(root, |n| {
        if syntax::is_code_token(n) {
            found = found.max(n.end_byte());
        }
    });
    LAST.set(Some((root.id(), root.end_byte(), found)));
    found
}

/// The diagnostic for [`unfinished_at`]: where the text stops and what is expected there.
fn unfinished(
    snapshot: &Snapshot,
    start: usize,
    end: usize,
) -> Option<Diagnostic> {
    let last = unfinished_at(snapshot.root(), start, end)?;
    // (The `ELSE IF` token takes in the blanks after it.)
    let token = syntax::text(last, snapshot.text()).trim_end();
    let message = match last.kind() {
        "-" | "->" | "<-" => format!("unfinished statement: expected a vertex pattern after `{token}`"),
        ")" | "]" => "unfinished statement: expected `;` here, and the closing `)` or `}` of what is open".to_string(),
        "," | "(" | "[" | "{" | ";" | "}" => {
            format!("unfinished statement: expected more input after `{token}`")
        }
        kind if !kind.chars().any(|c| c.is_ascii_alphanumeric()) => {
            format!("unfinished statement: expected an expression after `{token}`")
        }
        // (A stray `@@` or `@` is an error leaf.)
        _ if syntax::is_keyword(last) || last.is_error() => {
            format!("unfinished statement: expected more input after `{token}`")
        }
        _ => "unfinished statement: expected `;` here, and the closing `)` or `}` of what is open".to_string(),
    };
    Some(syntax_error(snapshot, Span::of(last), message))
}

/// Syntax errors of a tree, as diagnostics.
fn tree_errors(snapshot: &Snapshot, diagnostics: &mut Vec<Diagnostic>) {
    let root = snapshot.root();
    if root.has_error() {
        let mut cursor = root.walk();
        let mut stack = vec![root];
        let mut unfinished_ranges: Vec<(usize, usize)> = Vec::new();
        if root.is_error() {
            // The parser gave up on the whole document: its children are the
            // statements, and the pieces of broken ones.
            let children: Vec<Node> = root.children(&mut cursor).collect();
            if let Some(start) = last_definition_start(root)
                && let Some(d) = unfinished(snapshot, start, root.end_byte())
            {
                diagnostics.push(d);
                unfinished_ranges.push((start, root.end_byte()));
            }
            for (start, end, broken) in super::autocorrect::statements(root) {
                let pieces = || {
                    children.iter().filter(|c| {
                        start <= c.start_byte() && c.end_byte() <= end
                    })
                };
                if broken
                    && !unfinished_ranges
                        .iter()
                        .any(|&(s, e)| s <= start && end <= e)
                    && !pieces().any(|c| c.has_error())
                    && let Some(piece) =
                        pieces().find(|c| !c.is_named() && c.kind() != ";")
                {
                    diagnostics.push(unexpected(snapshot, *piece));
                }
            }
            stack = children.into_iter().rev().collect();
        }
        while let Some(node) = stack.pop() {
            if diagnostics.len() >= MAX_SYNTAX_ERRORS {
                break;
            }
            if unfinished_ranges
                .iter()
                .any(|&(s, e)| s <= node.start_byte() && node.end_byte() <= e)
            {
                continue;
            }
            if node.is_missing() {
                diagnostics.push(missing(snapshot, node));
                continue;
            }
            if node.is_error() {
                let d = unexpected(snapshot, node);
                // Only the bare `unexpected CREATE` makes way: what explains the
                // mistake itself (an unterminated string, a SELECT without FROM) stays.
                let bare = (d
                    .message
                    .starts_with("Syntax error: unexpected `")
                    && d.message.ends_with('`'))
                    || d.message.ends_with(MALFORMED_EDGE_STEP);
                let unfinished = if bare {
                    unfinished(snapshot, node.start_byte(), node.end_byte())
                } else {
                    None
                };
                diagnostics.push(unfinished.unwrap_or(d));
                continue;
            }
            if node.has_error() {
                let children: Vec<Node> =
                    node.children(&mut cursor).collect();
                stack.extend(children.into_iter().rev());
            }
        }
    }
}

/// When taking out a trailing comma leaves its statement without syntax
/// errors, the other errors there (noticed at what follows the comma) are
/// consequences of it.
fn drop_errors_of_trailing_commas(
    snapshot: &Snapshot,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let source = snapshot.text();
    let mut resolved: Vec<Range> = Vec::new();
    for d in diagnostics.iter().filter(|d| {
        d.message
            .ends_with("remove the trailing `,`")
    }) {
        let at = snapshot.offset(d.range.start);
        if let Some(span) = super::autocorrect::removing_comma_resolves(
            snapshot.root(),
            source,
            at,
        ) {
            resolved.push(snapshot.range(span));
        }
    }
    if resolved.is_empty() {
        return;
    }
    diagnostics.retain(|d| {
        d.message
            .ends_with("remove the trailing `,`")
            || !resolved
                .iter()
                .any(|r| r.contains_range(d.range))
    });
}

/// Syntax errors, and the misspelled keywords found among them.
fn syntax_diagnostics(
    snapshot: &Snapshot,
) -> (Vec<Diagnostic>, Rc<Vec<KeywordTypo>>, Option<Rc<Repair>>) {
    // (A tree freed earlier may have had the same address.)
    LAST.set(None);
    let root = snapshot.root();
    let (typos, repair) = super::autocorrect::typos_and_repair(
        root,
        snapshot.source,
        snapshot.encoding,
    );
    // Misspellings the parser read as valid code: not filtered by the corrected tree.
    let mut misread: Vec<Diagnostic> = Vec::new();
    absorbed_lines(snapshot, &typos, &mut misread);
    loose_keyword_typos(snapshot, &mut misread);
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    tree_errors(snapshot, &mut diagnostics);
    drop_errors_of_trailing_commas(snapshot, &mut diagnostics);
    // A removed or inserted token (see `KeywordTypo::token_edits`) explains a
    // statement whose errors the parser reported in bare words; a message that
    // says more (a trailing comma, `=>`, an unclosed block) stays.
    let active: Vec<&KeywordTypo> = typos
        .iter()
        .filter(|t| {
            let statement = snapshot.range(t.statement);
            t.token_edits.is_empty()
                || misread
                    .iter()
                    .chain(&diagnostics)
                    .filter(|d| statement.contains_lines_of(d.range))
                    .all(|d| is_bare_message(&d.message))
        })
        .collect();
    // A misspelled keyword explains the errors on its line better than they
    // do, and every error that is gone once the keywords are corrected.
    let typo_lines: HashSet<u32> = active
        .iter()
        .map(|t| snapshot.range(t.span).start.line)
        .collect();
    let resolved: Vec<Range> = active
        .iter()
        .filter_map(|t| t.resolves)
        .map(|span| snapshot.range(span))
        .collect();
    // (Corrections stay on their line, so lines are the same in both texts.)
    let remaining = repair
        .as_ref()
        .map(|r| error_lines(r.tree.root_node()));
    let statements: Vec<Range> = active
        .iter()
        .map(|t| snapshot.range(t.statement))
        .collect();
    let in_statement = |range: &Range| {
        statements
            .iter()
            .any(|r| r.contains_lines_of(*range))
    };
    let keep = |d: &Diagnostic| {
        !typo_lines.contains(&d.range.start.line)
            && !resolved.iter().any(|r| r.contains_range(d.range))
            // The errors of a statement with a typo are those of the corrected
            // statement, below: the original ones are consequences of the typo.
            && !in_statement(&d.range)
    };
    diagnostics.retain(|d| {
        keep(d)
            && remaining
                .as_ref()
                .is_none_or(|lines| lines.contains(&d.range.start.line))
    });
    misread.retain(&keep);
    diagnostics.append(&mut misread);
    // What is still wrong in those statements once the typos are corrected
    // (another mistake, e.g. a missing `;`), reported in the original text.
    if let Some(repair) = repair
        .as_ref()
        .filter(|r| r.tree.root_node().has_error())
    {
        let mut others = Vec::new();
        tree_errors(&repair.snapshot(snapshot), &mut others);
        for d in others {
            let d = repair.diagnostic(d);
            if in_statement(&d.range)
                && !typo_lines.contains(&d.range.start.line)
            {
                diagnostics.push(d);
            }
        }
    }
    for typo in active.iter().copied() {
        if let Some(d) = token_edit_diagnostic(snapshot, typo) {
            diagnostics.push(d);
            continue;
        }
        if typo.word.is_empty()
            && matches!(typo.keyword.trim(), "THEN" | "DO")
        {
            // A missing `THEN` or `DO`: reported on the last token before it.
            let keyword = typo.keyword.trim();
            let at = typo.span.start;
            let (before, _) = token_before(snapshot, at);
            let message = match keyword {
                "THEN" => "missing `THEN` after the IF condition",
                _ => "missing `DO` before the loop body",
            };
            let mut d = syntax_error(snapshot, before, message.to_string());
            add_fix(
                &mut d,
                format!("Insert `{keyword}`"),
                vec![insertion(snapshot, at, keyword)],
                false,
            );
            diagnostics.push(d);
            continue;
        }
        if typo.word.is_empty() {
            // A missing `,` (see `autocorrect`): the correction is an insertion.
            diagnostics.push(missing_comma_error(snapshot, typo.span.start));
            continue;
        }
        // `PST-ACCUM`: the word is only the first half of the keyword `POST-ACCUM`.
        let joined = snapshot.text()[typo.span.end..]
            .get(..6)
            .filter(|rest| rest.eq_ignore_ascii_case("-accum"));
        let (keyword, word) = match joined {
            Some(rest) if typo.keyword.eq_ignore_ascii_case("post") => (
                format!("{}{rest}", typo.keyword),
                format!("{}{rest}", typo.word),
            ),
            _ => (typo.keyword.clone(), typo.word.clone()),
        };
        let mut d = syntax_error(
            snapshot,
            typo.span,
            format!("did you mean `{keyword}` instead of `{word}`?"),
        );
        for keyword in
            std::iter::once(&typo.keyword).chain(&typo.alternatives)
        {
            let edit = snapshot.edit(typo.span, keyword);
            let certain = typo.certain && typo.alternatives.is_empty();
            add_fix(
                &mut d,
                format!("Change to `{keyword}`"),
                vec![edit],
                certain,
            );
        }
        diagnostics.push(d);
    }
    diagnostics.sort_by_key(|d| (d.range.start, d.range.end));
    // One mistake can derail the parser several times on a line (`POST ACCUM`).
    // (A chain of `ELSEIF` on one line is several mistakes with one message.)
    diagnostics.dedup_by(|b, a| {
        a.message == b.message
            && a.range.start.line == b.range.start.line
            && (a.range == b.range || !a.message.contains("did you mean"))
    });
    (diagnostics, typos, repair)
}

/// Whether a syntax message only says which token the parser stumbled on or
/// lacked (`unexpected `x` in query body`, `missing `)``), not what is wrong.
fn is_bare_message(message: &str) -> bool {
    let Some(rest) = message.strip_prefix("Syntax error: ") else {
        return false;
    };
    if rest.starts_with("unfinished statement: ")
        || rest == "missing `;` after this statement"
    {
        return true;
    }
    let Some(rest) = rest
        .strip_prefix("unexpected ")
        .or_else(|| rest.strip_prefix("missing "))
    else {
        return false;
    };
    let rest = match rest.strip_prefix('`') {
        Some(quoted) => quoted
            .split_once('`')
            .map_or("", |(_, after)| after),
        None => {
            return rest == "input" || rest.starts_with("input in ");
        }
    };
    rest.is_empty()
        || rest
            .strip_prefix(" in ")
            .is_some_and(|context| {
                matches!(
                    context,
                    "ACCUM clause"
                        | "POST-ACCUM clause"
                        | "WHERE clause"
                        | "FROM clause"
                        | "SELECT statement"
                        | "parameter list"
                        | "argument list"
                        | "query body"
                        | "loading job"
                        | "schema change job"
                        | "attribute list"
                )
            })
}

/// The diagnostic for a statement that parses once a token is removed or
/// inserted (see [`KeywordTypo::token_edits`]): the first repair names the
/// problem, every repair is a quick fix (none is safe without review).
fn token_edit_diagnostic(
    snapshot: &Snapshot,
    typo: &KeywordTypo,
) -> Option<Diagnostic> {
    let first = typo.token_edits.first()?;
    let source = snapshot.text();
    let (span, message) = if first.inserted {
        // Reported on the token before the place: that is where the text stops making sense.
        let (span, token) = token_before(snapshot, first.span.start);
        let previous: String = source[span.start..span.end]
            .chars()
            .take(40)
            .collect();
        let context = token
            .and_then(context_of)
            .map(|c| format!(" {c}"))
            .unwrap_or_default();
        let message = match first.token.as_str() {
            ";" => "missing `;` after this statement".to_string(),
            token => format!("missing `{token}` after `{previous}`{context}"),
        };
        (span, message)
    } else {
        let at = first.span.start
            + source[first.span.start..first.span.end]
                .find(&first.token)
                .unwrap_or(0);
        let span = Span::new(at, at + first.token.len());
        let context = snapshot
            .node_at(span)
            .and_then(context_of)
            .map(|c| format!(" {c}"))
            .unwrap_or_default();
        let shown: String = first.token.chars().take(40).collect();
        (
            span,
            format!(
                "unexpected `{shown}`{context}: the statement parses without it"
            ),
        )
    };
    let mut d = syntax_error(snapshot, span, message);
    for edit in &typo.token_edits {
        let title = if edit.inserted {
            format!("Insert `{}`", edit.token)
        } else {
            format!("Remove `{}`", edit.token)
        };
        add_fix(
            &mut d,
            title,
            vec![snapshot.edit(edit.span, &edit.with)],
            false,
        );
    }
    Some(d)
}

/// Keywords that start a top-level command.
const COMMANDS: &[&str] = &[
    "ABORT",
    "ALTER",
    "BEGIN",
    "CLEAR",
    "CREATE",
    "DROP",
    "END",
    "EXIT",
    "EXPORT",
    "GRANT",
    "HELP",
    "IMPORT",
    "INSTALL",
    "INTERPRET",
    "LS",
    "QUIT",
    "RESUME",
    "REVOKE",
    "RUN",
    "SET",
    "SHOW",
    "TYPEDEF",
    "USE",
    "VERSION",
];

/// Flags an unindented line that a shell command absorbed (see
/// [`super::autocorrect::absorbing_commands`]) when no misspelled keyword
/// explains it: usually the line is a misspelled command.
fn absorbed_lines(
    snapshot: &Snapshot,
    typos: &[KeywordTypo],
    out: &mut Vec<Diagnostic>,
) {
    let source = snapshot.text();
    for (command, absorbed) in
        super::autocorrect::absorbing_commands(snapshot.root(), source)
    {
        if typos
            .iter()
            .any(|t| Span::of(command).contains_span(t.span))
        {
            continue;
        }
        let word = syntax::text(absorbed, source);
        let suggestions = super::autocorrect::similar(
            &word.to_ascii_uppercase(),
            COMMANDS.iter().copied(),
        );
        let span = Span::of(absorbed);
        let message = match suggestions.first() {
            Some(command) => {
                format!("unknown command `{word}`; did you mean `{command}`?")
            }
            None => format!(
                "`{word}` starts a new line but continues the previous command; shell commands end at the end of the line"
            ),
        };
        let mut d = syntax_error(snapshot, span, message);
        for command in suggestions.iter().take(3) {
            let styled = super::autocorrect::styled(command, word);
            add_fix(
                &mut d,
                format!("Change to `{styled}`"),
                vec![snapshot.edit(span, &styled)],
                false,
            );
        }
        out.push(d);
    }
}

/// Keywords of the loosely read parts of GRANT, REVOKE, EXPORT and IMPORT.
const SCOPE_KEYWORDS: &[&str] =
    &["GRAPH", "GLOBAL", "ALL", "QUERY", "VERTEX", "EDGE"];

/// Misspelled keywords that the grammar reads as names because these parts
/// take free-form names: in the scope of GRANT and REVOKE (`ON GRAPRH g`, a
/// name followed by another name, must be a keyword), and after EXPORT or
/// IMPORT (`EXPORT GRAH ALL`).
fn loose_keyword_typos(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let source = snapshot.text();
    let mut report = |word: Node| {
        let text = syntax::text(word, source);
        let Some(keyword) = super::autocorrect::similar(
            &text.to_ascii_uppercase(),
            SCOPE_KEYWORDS.iter().copied(),
        )
        .first()
        .copied() else {
            return;
        };
        let keyword = super::autocorrect::styled(keyword, text);
        let span = Span::of(word);
        let mut d = syntax_error(
            snapshot,
            span,
            format!("did you mean `{keyword}` instead of `{text}`?"),
        );
        add_fix(
            &mut d,
            format!("Change to `{keyword}`"),
            vec![snapshot.edit(span, &keyword)],
            false,
        );
        out.push(d);
    };
    for statement in syntax::named_children(snapshot.root()) {
        let children = syntax::children(statement);
        match statement.kind() {
            "grant_statement" | "revoke_statement" => {
                let Some(on) = children
                    .iter()
                    .position(|c| c.kind() == "ON")
                else {
                    continue;
                };
                for pair in children[on + 1..].windows(2) {
                    let names = |n: &Node| {
                        matches!(
                            n.kind(),
                            "identifier" | "qualified_identifier"
                        )
                    };
                    if pair[0].kind() == "identifier" && names(&pair[1]) {
                        report(pair[0]);
                    }
                }
            }
            "shell_command" => {
                let starts = children
                    .first()
                    .is_some_and(|c| matches!(c.kind(), "EXPORT" | "IMPORT"));
                if let (true, Some(first)) = (
                    starts,
                    children
                        .get(1)
                        .filter(|c| c.kind() == "identifier"),
                ) {
                    report(*first);
                }
            }
            _ => {}
        }
    }
}

fn describe_token(kind: &str) -> String {
    if kind.chars().any(|c| c.is_ascii_lowercase()) {
        kind.replace('_', " ")
    } else {
        format!("`{kind}`")
    }
}

fn syntax_error(
    snapshot: &Snapshot,
    span: Span,
    message: String,
) -> Diagnostic {
    diagnostic(
        snapshot,
        span,
        severity::ERROR,
        "syntax-error",
        format!("Syntax error: {message}"),
    )
}

/// Mistakes recognizable from the text of the line the parser stumbled on.
fn line_hint(snapshot: &Snapshot, offset: usize) -> Option<String> {
    let lines = &snapshot.source.lines;
    let line = lines.line_of(offset);
    let text = &snapshot.text()
        [lines.line_start(line)..lines.line_end(snapshot.text(), line)];
    let lower = text.to_ascii_lowercase();
    // `post accum` with only blanks between: `post-accum` is the right spelling.
    let words: Vec<&str> = lower.split_whitespace().collect();
    let is_word = |w: &str, expected: &str| {
        w.trim_matches(|c: char| !is_word_char(c)) == expected
    };
    words
        .windows(2)
        .any(|pair| is_word(pair[0], "post") && is_word(pair[1], "accum"))
        .then(|| {
            "write `POST-ACCUM` (or `POST_ACCUM`) as one word".to_string()
        })
}

fn missing(snapshot: &Snapshot, node: Node) -> Diagnostic {
    let kind = node.kind();
    let parent = node.parent();
    // `END;` after such an IF reads as a declaration of an accumulator of type END.
    if kind == "global_accumulator"
        && let Some(declaration) = parent
            .and_then(|p| p.parent())
            .filter(|d| d.kind() == "accumulator_declaration")
        && let Some(ty) = declaration.child_by_field_name("type")
        && syntax::text(ty, snapshot.text()).eq_ignore_ascii_case("END")
    {
        let message =
            "`END` closes nothing here: there is no open IF, CASE, WHILE, FOREACH or TRY"
                .to_string();
        return syntax_error(snapshot, Span::of(ty), message);
    }
    let message = line_hint(snapshot, node.start_byte()).unwrap_or_else(|| match kind {
        ";" if follows_accum_clause(node) => {
            "missing `,` between ACCUM statements, or `;` to end the SELECT statement"
                .to_string()
        }
        ";" => "missing `;` after this statement".to_string(),
        "-" | "->" if parent.is_some_and(|p| p.kind() == "edge_step") => {
            MALFORMED_EDGE_STEP.to_string()
        }
        "}" => match parent.map(|p| p.kind()) {
            Some("loading_job_body") => {
                "missing `}` to close the loading job".to_string()
            }
            Some("schema_change_body") => {
                "missing `}` to close the schema change job".to_string()
            }
            Some("query_body" | "opencypher_body") => {
                "missing `}` to close the query body".to_string()
            }
            _ => "missing `}`".to_string(),
        },
        "END" => match parent.map(|p| p.kind()) {
            Some(construct) => {
                format!("missing `END` to close the {}", construct_name(construct))
            }
            None => "missing `END`".to_string(),
        },
        _ => {
            let context = context_of(node)
                .map(|c| format!(" {c}"))
                .unwrap_or_default();
            format!("missing {}{context}", describe_token(kind))
        }
    });
    let mut d = syntax_error(snapshot, Span::of(node), message);
    if line_hint(snapshot, node.start_byte()).is_some() {
        if let Some((title, edits)) =
            post_accum_fix(snapshot, node.start_byte())
        {
            add_fix(&mut d, title, edits, true);
        }
    } else if !node.is_named() {
        // Offer to insert the missing token (anonymous tokens are named by their text).
        let tokens: &[&str] = if kind == ";" && follows_accum_clause(node) {
            &[",", ";"]
        } else {
            &[kind]
        };
        // The parser may place it after a comment: it goes before, not into it.
        let start = node.start_byte();
        let code_end = snapshot.parsed_code_text()[..start]
            .trim_end()
            .len();
        let commented = !snapshot.text()[code_end..start]
            .trim()
            .is_empty();
        let at = if commented { code_end } else { start };
        for token in tokens {
            let edit = insertion(snapshot, at, token);
            add_fix(&mut d, format!("Insert `{token}`"), vec![edit], false);
        }
    }
    d
}

/// The leaf token at the last non-blank character before `at`, and its span
/// (that character's when there is no leaf).
fn token_before<'t>(
    snapshot: &'t Snapshot,
    at: usize,
) -> (Span, Option<Node<'t>>) {
    let before = snapshot.text()[..at].trim_end();
    let last = before
        .char_indices()
        .next_back()
        .map_or(0, |(i, _)| i);
    let token = snapshot
        .root()
        .descendant_for_byte_range(last, last + 1)
        .filter(|n| n.child_count() == 0);
    (token.map_or(Span::new(last, before.len()), Span::of), token)
}

/// The syntax error for a `,` missing before the token at `at`, with its fix.
fn missing_comma_error(snapshot: &Snapshot, at: usize) -> Diagnostic {
    let word: String = snapshot.text()[at..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !",;()".contains(*c))
        .take(40)
        .collect();
    let span = Span::new(at, at + word.len().max(1));
    let mut d =
        syntax_error(snapshot, span, format!("missing `,` before `{word}`"));
    add_fix(
        &mut d,
        "Insert `,`",
        vec![comma_insertion(snapshot, at)],
        false,
    );
    d
}

/// Inserts a `,` in front of the token at `offset`: right after the token
/// before it, ahead of blanks and comments (`a, b`, not `a , b`).
fn comma_insertion(snapshot: &Snapshot, offset: usize) -> TextEdit {
    let before = snapshot.parsed_code_text()[..offset].trim_end();
    if !before.is_empty() && !before.ends_with(',') {
        return snapshot.insert(before.len(), ",");
    }
    insertion(snapshot, offset, ", ")
}

/// Inserts `token` at `offset`, with spaces around words.
fn insertion(snapshot: &Snapshot, offset: usize, token: &str) -> TextEdit {
    let source = snapshot.text();
    let word = token.chars().all(is_word_char);
    let before = source[..offset].chars().next_back();
    let after = source[offset..].chars().next();
    let mut text = String::new();
    if word && before.is_some_and(|c| !c.is_whitespace()) {
        text.push(' ');
    }
    text.push_str(token);
    if word && after.is_some_and(is_word_char) {
        text.push(' ');
    }
    snapshot.insert(offset, text)
}

const MALFORMED_EDGE_STEP: &str = "malformed edge step: write `-(Edge:e)-` or `-(Edge:e)->` between vertex patterns";

fn construct_name(kind: &str) -> &'static str {
    match kind {
        "if_statement" => "IF statement",
        "while_statement" => "WHILE loop",
        "foreach_statement" => "FOREACH loop",
        "case_statement" | "case_expression" => "CASE",
        "try_statement" => "TRY block",
        _ => "block",
    }
}

/// Whether a missing `;` directly follows a SELECT whose last clause is ACCUM
/// or POST-ACCUM (typically a forgotten comma between ACCUM statements).
fn follows_accum_clause(node: Node) -> bool {
    let Some(previous) = syntax::prev_non_comment_sibling(node) else {
        return false;
    };
    let select = match previous.kind() {
        "select_statement" => Some(previous),
        "assignment_statement" => previous
            .child_by_field_name("right")
            .filter(|r| r.kind() == "select_statement"),
        _ => None,
    };
    select
        .and_then(syntax::last_code_child)
        .is_some_and(|last| {
            matches!(last.kind(), "accum_clause" | "post_accum_clause")
        })
}

fn unexpected(snapshot: &Snapshot, node: Node) -> Diagnostic {
    let source = snapshot.text();
    // The first leaf of the error is what the parser could not fit.
    let first = syntax::first_leaf(node);
    let span = if first.end_byte() > first.start_byte() {
        Span::of(first)
    } else {
        Span::of(node)
    };
    // Keep the squiggle on one line.
    let line_end = source[span.start..]
        .find('\n')
        .map(|i| span.start + i)
        .unwrap_or(source.len());
    let mut span =
        Span::new(span.start, span.end.min(line_end).max(span.start));
    let token = syntax::text(first, source).trim();

    if token == "/"
        && source[span.start..].starts_with("/*")
        && !source[span.start + 2..].contains("*/")
    {
        return syntax_error(
            snapshot,
            Span::new(span.start, span.start + 2),
            "unterminated comment: add the closing `*/`".into(),
        );
    }
    if token == ">"
        && span.start > 0
        && source.as_bytes()[span.start - 1] == b'='
    {
        let arrow = Span::new(span.start - 1, span.end);
        let mut d = syntax_error(
            snapshot,
            arrow,
            "`=>` is not a GSQL operator: write key-value pairs as `key -> value`".into(),
        );
        add_fix(
            &mut d,
            String::from("Change to `->`"),
            vec![snapshot.edit(arrow, "->")],
            false,
        );
        return d;
    }
    if let Some(quote) = unterminated_string(node, source) {
        return syntax_error(
            snapshot,
            quote,
            "unterminated string: add the closing `\"`".into(),
        );
    }
    if let Some(select) = select_without_from(node) {
        return syntax_error(
            snapshot,
            Span::of(select),
            "SELECT needs a FROM clause".into(),
        );
    }
    let context = context_of(node);
    // (message, optional (title, edits)) for mistakes with a known repair.
    let lowercase = token.chars().any(|c| c.is_ascii_lowercase());
    let (hint, fix): (Option<String>, Option<(String, Vec<TextEdit>)>) =
        if let Some(hint) = line_hint(snapshot, span.start) {
            (Some(hint), post_accum_fix(snapshot, span.start))
        } else if token.eq_ignore_ascii_case("ELSEIF") {
            let replacement = if lowercase { "else if" } else { "ELSE IF" };
            let edit = snapshot.edit(Span::of(first), replacement);
            (
                Some("write `ELSE IF` as two words".into()),
                Some((format!("Change to `{replacement}`"), vec![edit])),
            )
        } else if token == "=" && context == Some("in attribute list") {
            let edit = spaced(snapshot, Span::of(first), "DEFAULT");
            (
                Some(
                    "use `DEFAULT value` to give an attribute a default"
                        .into(),
                ),
                Some(("Use `DEFAULT`".into(), vec![edit])),
            )
        } else if contains_kind(node, "edge_pattern") {
            // The error may start at the `CREATE` of a definition it swallowed:
            // the squiggle belongs on the pattern.
            if matches!(first.kind(), "CREATE" | "INTERPRET") {
                let mut pattern = None;
                syntax::walk(node, |n| {
                    if n.kind() == "edge_pattern" && pattern.is_none() {
                        pattern = Some(n);
                    }
                });
                if let Some(pattern) = pattern {
                    let end = source[pattern.start_byte()..]
                        .find('\n')
                        .map_or(source.len(), |i| pattern.start_byte() + i);
                    span = Span::new(
                        pattern.start_byte(),
                        pattern.end_byte().min(end),
                    );
                }
            }
            (Some(MALFORMED_EDGE_STEP.to_string()), None)
        } else if let Some(hint) = unclosed_block(node, first) {
            let mut d = syntax_error(snapshot, span, hint);
            // Not part of `source.fixAll`: where the block ends is a guess.
            if let Some(edit) = insert_end(snapshot, node, first) {
                add_fix(&mut d, "Insert `END;`", vec![edit], false);
            }
            return d;
        } else if let Some(comma) =
            trailing_comma(snapshot, token, span.start)
        {
            // The parser notices a trailing comma at the token after it.
            let comma = Span::new(comma, comma + 1);
            let mut d = syntax_error(
                snapshot,
                comma,
                "remove the trailing `,`".into(),
            );
            add_fix(
                &mut d,
                "Remove the trailing `,`",
                vec![snapshot.edit(comma, "")],
                true,
            );
            return d;
        } else if matches!(
            context,
            Some(
                "in parameter list"
                    | "in argument list"
                    | "in attribute list"
            )
        ) && token != ","
        {
            match super::autocorrect::missing_comma_at(
                snapshot.root(),
                source,
                span.start,
            ) {
                // `(STRING a INT b)`: a `,` between the items repairs the list. The
                // parser may notice the problem a token before the place of the `,`.
                Some(at) => return missing_comma_error(snapshot, at),
                None => (None, None),
            }
        } else {
            (None, None)
        };
    let message = hint.unwrap_or_else(|| {
        let shown: String = token.chars().take(40).collect();
        let context = context
            .map(|c| format!(" {c}"))
            .unwrap_or_default();
        if shown.is_empty() {
            format!("unexpected input{context}")
        } else {
            format!("unexpected `{shown}`{context}")
        }
    });
    let mut d = syntax_error(snapshot, span, message);
    if let Some((title, edits)) = fix {
        add_fix(&mut d, title, edits, true);
    }
    d
}

/// Words that end a comma-separated list.
const LIST_ENDERS: &[&str] = &[
    "POST-ACCUM",
    "POST_ACCUM",
    "ACCUM",
    "LIMIT",
    "ORDER",
    "HAVING",
    "GROUP",
    "WHERE",
    "END",
    "ELSE",
    "FROM",
];

/// The offset of a comma that the error at `at` (the token `token`) is a
/// consequence of: the comma itself or the token before `at`, followed by
/// the end of a list, where taking the comma out leaves fewer errors.
fn trailing_comma(
    snapshot: &Snapshot,
    token: &str,
    at: usize,
) -> Option<usize> {
    // Comments between the comma and the next clause do not matter (offsets stay the same).
    let source = snapshot.parsed_code_text();
    let ends_list = |rest: &str| {
        let rest = rest.trim_start();
        rest.starts_with([')', ']', '}', ';'])
            || LIST_ENDERS.iter().any(|word| {
                starts_with_ignore_ascii_case(rest, word)
                    && !rest[word.len()..].starts_with(is_word_char)
            })
    };
    let comma = if token == "," {
        ends_list(&source[at + 1..]).then_some(at)?
    } else {
        // The error may also be at what follows the first word after the comma
        // (`ORDER BY a, LIMIT 3` is noticed at `3`).
        let candidates = [
            at,
            source[..at]
                .trim_end()
                .rfind(|c: char| c.is_whitespace())
                .map_or(0, |i| i + 1),
        ];
        candidates.into_iter().find_map(|from| {
            let before = source[..from].trim_end();
            (before.ends_with(',') && ends_list(&source[from..]))
                .then(|| before.len() - 1)
        })?
    };
    let root = snapshot.root();
    super::autocorrect::removing_comma_helps(root, snapshot.text(), comma)
        .then_some(comma)
}

/// Replaces `span` with a word, adding spaces where it would touch other words.
fn spaced(snapshot: &Snapshot, span: Span, word: &str) -> TextEdit {
    let source = snapshot.text();
    let touches = |c: Option<char>| c.is_some_and(|c| !c.is_whitespace());
    let mut text = String::new();
    if touches(source[..span.start].chars().next_back()) {
        text.push(' ');
    }
    text.push_str(word);
    if touches(source[span.end..].chars().next()) {
        text.push(' ');
    }
    snapshot.edit(span, text)
}

/// `POST ACCUM` written as two words on the line of `offset`.
fn post_accum_fix(
    snapshot: &Snapshot,
    offset: usize,
) -> Option<(String, Vec<TextEdit>)> {
    let lines = &snapshot.source.lines;
    let line = lines.line_of(offset);
    let start = lines.line_start(line);
    let text = &snapshot.text()[start..lines.line_end(snapshot.text(), line)];
    let lower = text.to_ascii_lowercase();
    let at = word_offsets(&lower, "post").find_map(|i| {
        let after = &lower[i + 4..];
        let gap = after.len() - after.trim_start().len();
        (gap > 0 && after.trim_start().starts_with("accum"))
            .then_some((i, i + 4 + gap + 5))
    })?;
    let written = &text[at.0..at.1];
    let replacement = if written
        .chars()
        .any(|c| c.is_ascii_lowercase())
    {
        "post-accum"
    } else {
        "POST-ACCUM"
    };
    let edit =
        snapshot.edit(Span::new(start + at.0, start + at.1), replacement);
    Some((format!("Write `{replacement}`"), vec![edit]))
}

/// The opening quote of a string that is never closed, inside an error.
fn unterminated_string(error: Node, source: &str) -> Option<Span> {
    let mut cursor = error.walk();
    let children: Vec<Node> = error.children(&mut cursor).collect();
    children.iter().find_map(|child| {
        // String content runs up to the closing quote; without one it runs to the end.
        let unterminated = child.kind() == "string_content"
            && !source[child.end_byte()..].starts_with('"');
        unterminated.then(|| {
            Span::new(
                child.start_byte().saturating_sub(1),
                child.start_byte(),
            )
        })
    })
}

fn contains_kind(node: Node, kind: &str) -> bool {
    let mut found = false;
    syntax::walk(node, |n| found |= n.kind() == kind);
    found
}

/// The SELECT keyword of a SELECT statement without FROM inside an error.
fn select_without_from(error: Node) -> Option<Node> {
    let mut select = None;
    let mut from = false;
    syntax::walk(error, |n| {
        if n.kind() == "SELECT" && select.is_none() {
            select = Some(n);
        }
        from |= n.kind() == "FROM";
    });
    select.filter(|_| !from)
}

/// A block keyword at the start of an error without a matching END.
fn unclosed_block(error: Node, first: Node) -> Option<String> {
    let keyword = first.kind();
    if !matches!(keyword, "IF" | "WHILE" | "FOREACH" | "CASE" | "TRY") {
        return None;
    }
    let mut openers = 0usize;
    let mut ends = 0usize;
    let mut previous = "";
    syntax::walk(error, |n| {
        if n.child_count() > 0 {
            return;
        }
        match n.kind() {
            // `ELSE IF` continues an IF rather than opening a new one.
            "IF" if previous != "ELSE" => openers += 1,
            "WHILE" | "FOREACH" | "CASE" | "TRY" => openers += 1,
            "END" => ends += 1,
            _ => {}
        }
        previous = n.kind();
    });
    (openers > ends).then(|| format!("`{keyword}` has no matching `END`"))
}

/// `END;` on its own line before the `}` that closes the body around the
/// unclosed block, indented like the block's keyword. Only when the keyword
/// starts its line, the error sits directly in a body and that body's `}` is
/// alone on its line.
fn insert_end(
    snapshot: &Snapshot,
    error: Node,
    keyword: Node,
) -> Option<TextEdit> {
    let source = snapshot.text();
    let body = error
        .parent()
        .filter(|p| matches!(p.kind(), "query_body" | "block"))?;
    let close = body
        .end_byte()
        .checked_sub(1)
        .filter(|c| source.as_bytes().get(*c) == Some(&b'}'))?;
    let line_start =
        |at: usize| source[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent =
        &source[line_start(keyword.start_byte())..keyword.start_byte()];
    if !indent.chars().all(|c| c == ' ' || c == '\t')
        || close < keyword.start_byte()
    {
        return None;
    }
    let at = line_start(close);
    if !source[at..close]
        .chars()
        .all(|c| c == ' ' || c == '\t')
    {
        return None;
    }
    Some(
        snapshot
            .insert(at, format!("{indent}END;{}", snapshot.source.newline())),
    )
}

fn context_of(node: Node) -> Option<&'static str> {
    for ancestor in syntax::ancestors(node) {
        let context = match ancestor.kind() {
            "accum_clause" => "in ACCUM clause",
            "post_accum_clause" => "in POST-ACCUM clause",
            "where_clause" => "in WHERE clause",
            "from_clause" => "in FROM clause",
            "select_statement" => "in SELECT statement",
            "parameter_list" => "in parameter list",
            "argument_list" => "in argument list",
            "query_body" => "in query body",
            "loading_job_body" => "in loading job",
            "schema_change_body" => "in schema change job",
            "vertex_attribute_list" | "edge_attribute_list" => {
                "in attribute list"
            }
            "source_file" => return None,
            _ => continue,
        };
        return Some(context);
    }
    None
}

// ---------------------------------------------------------------------------
// Semantic checks
// ---------------------------------------------------------------------------

fn semantic(snapshot: &Snapshot) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    undeclared_accumulators(snapshot, &mut diagnostics);
    duplicate_declarations(snapshot, &mut diagnostics);
    if snapshot
        .config
        .diagnostics_duplicate_definitions
    {
        duplicate_definitions(snapshot, &mut diagnostics);
    }
    if snapshot.config.diagnostics_no_schema_notice {
        no_schema_notice(snapshot, &mut diagnostics);
    }
    if snapshot.config.diagnostics_unknown_types {
        unknown_types(snapshot, &mut diagnostics);
        unknown_graphs(snapshot, &mut diagnostics);
    }
    if snapshot
        .config
        .diagnostics_unknown_attributes
    {
        unknown_attributes(snapshot, &mut diagnostics);
    }
    if snapshot.config.diagnostics_undefined_names {
        undefined_names(snapshot, &mut diagnostics);
        misspelled_names(snapshot, &mut diagnostics);
    }
    if snapshot.config.diagnostics_unused {
        unused_declarations(snapshot, &mut diagnostics);
    }
    query_arity(snapshot, &mut diagnostics);
    builtin_arity(snapshot, &mut diagnostics);
    super::values::check(snapshot, &mut diagnostics);
    v3_comparisons(snapshot, &mut diagnostics);
    stray_stubs(snapshot, &mut diagnostics);
    if snapshot.config.diagnostics_language_rules {
        super::rules::check(snapshot, &mut diagnostics);
        graph_without_edges(snapshot, &mut diagnostics);
    }
    diagnostics
}

fn in_query(snapshot: &Snapshot, scope: usize) -> bool {
    snapshot
        .analysis
        .query_scope(scope)
        .is_some()
}

fn undeclared_accumulators(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    for reference in &snapshot.analysis.references {
        if reference.target.is_some()
            || reference.in_error
            || !in_query(snapshot, reference.scope)
        {
            continue;
        }
        let kind = match &reference.role {
            Role::GlobalAccumulator => "global accumulator",
            Role::LocalAccumulator => "accumulator",
            _ => continue,
        };
        let mut d = diagnostic(
            snapshot,
            reference.span,
            severity::ERROR,
            "undeclared-accumulator",
            format!(
                "The {kind} `{}` is not declared in this query",
                reference.name
            ),
        );
        // A declared accumulator of this query with a similar name (`@scor`).
        let wanted = SymbolKind::accumulator(
            reference.role == Role::GlobalAccumulator,
        );
        let query = snapshot
            .analysis
            .query_scope(reference.scope);
        let declared: Vec<&str> = snapshot
            .analysis
            .symbols
            .iter()
            .filter(|s| {
                s.kind == wanted
                    && !s.in_error
                    && query.is_some_and(|q| {
                        snapshot.analysis.query_scope(s.scope) == Some(q)
                    })
            })
            .map(|s| s.name.as_str())
            .collect();
        let suggestions =
            super::autocorrect::similar(&reference.name, declared);
        if let Some(best) = suggestions.first() {
            d.message = format!("{} (did you mean `{best}`?)", d.message);
        }
        for suggestion in suggestions.into_iter().take(3) {
            add_fix(
                &mut d,
                format!("Change to `{suggestion}`"),
                vec![snapshot.edit(reference.span, suggestion)],
                false,
            );
        }
        out.push(d);
    }
}

fn duplicate_declarations(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    use SymbolKind as K;
    let analysis = snapshot.analysis;
    for (scope_id, scope) in analysis.scopes.iter().enumerate() {
        let mut seen: HashSet<(String, u8)> = HashSet::new();
        for symbol in analysis.scope_symbols(scope_id) {
            if symbol.in_error {
                continue;
            }
            let namespace = match symbol.kind {
                K::Parameter | K::Variable | K::File | K::Exception => 0,
                K::VertexSet if !symbol.implicit => 0,
                k if k.is_accumulator() => 1,
                K::TupleType | K::AccumulatorType
                    if scope.kind != ScopeKind::File =>
                {
                    2
                }
                K::FilenameVariable | K::Header | K::LineFilter => 3,
                _ => continue,
            };
            if !seen.insert((symbol.name.clone(), namespace)) {
                out.push(diagnostic(
                    snapshot,
                    symbol.name_span,
                    severity::ERROR,
                    "duplicate-declaration",
                    format!(
                        "`{}` is already declared in this scope",
                        symbol.name
                    ),
                ));
            }
        }
    }
}

/// A `CREATE` of a vertex type, edge type, graph or query whose name another
/// `CREATE` (without `OR REPLACE`) defines before it fails in the database.
/// The first definition is the earliest in the file, or in the file whose URI
/// sorts first. Names that any DROP removes anywhere (and queries without a
/// `FOR GRAPH`, which belong to the current graph) are not judged. A
/// repeat in the same file is a warning; one in another file is a hint, since
/// a workspace may hold alternative versions of a schema.
fn duplicate_definitions(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    use SymbolKind as K;
    let own_key = crate::uri::key(snapshot.uri);
    let workspace = snapshot.workspace;
    for symbol in &snapshot.analysis.symbols {
        if !symbol.strict_create
            || symbol.in_error
            || symbol.scope != FILE_SCOPE
            || !matches!(
                symbol.kind,
                K::VertexType | K::EdgeType | K::Graph | K::Query
            )
            || (symbol.kind == K::Query && symbol.graph.is_none())
            || workspace.is_dropped(symbol.kind, &symbol.name)
        {
            continue;
        }
        let same = |other_graph: &Option<String>| {
            symbol.kind != K::Query || *other_graph == symbol.graph
        };
        // (file key, offset) of every earlier definition.
        let mut first: Option<(String, usize, Location)> = None;
        let mut consider =
            |key: String, offset: usize, location: Location| {
                if first
                    .as_ref()
                    .is_none_or(|(k, o, _)| (&key, offset) < (k, *o))
                {
                    first = Some((key, offset, location));
                }
            };
        for other in &snapshot.analysis.symbols {
            if other.kind == symbol.kind
                && other.name == symbol.name
                && other.strict_create
                && !other.in_error
                && other.scope == FILE_SCOPE
                && other.name_span.start < symbol.name_span.start
                && same(&other.graph)
            {
                let location = snapshot.location(other.name_span);
                consider(own_key.clone(), other.name_span.start, location);
            }
        }
        for other in workspace.find(symbol.kind, &symbol.name) {
            let key = crate::uri::key(&other.uri);
            if key < own_key
                && other.strict_create
                && !other.in_error
                && same(&other.graph)
            {
                consider(key, 0, other.location());
            }
        }
        let Some((key, _, location)) = first else {
            continue;
        };
        let elsewhere = key != own_key;
        let place = if elsewhere {
            let file = crate::uri::file_name(&location.uri);
            format!("{file}:{}", location.range.start.line + 1)
        } else {
            format!("line {}", location.range.start.line + 1)
        };
        let what = if symbol.kind == K::Query {
            "query"
        } else {
            symbol.kind.label()
        };
        let level = if elsewhere {
            severity::HINT
        } else {
            severity::WARNING
        };
        let mut d = diagnostic(
            snapshot,
            symbol.name_span,
            level,
            "duplicate-definition",
            format!(
                "The {what} `{}` is already defined at {place}; a second CREATE fails unless it is CREATE OR REPLACE \
                 or follows a DROP",
                symbol.name
            ),
        );
        d.related_information
            .push(RelatedInformation {
                location,
                message: "First definition".into(),
            });
        out.push(d);
    }
}

/// `message`, with the closest of `candidates` to `name` suggested.
fn with_suggestion<'c>(
    message: String,
    name: &str,
    candidates: impl IntoIterator<Item = &'c str>,
) -> String {
    match super::autocorrect::similar(name, candidates).first() {
        Some(best) => {
            format!("{message} (did you mean `{best}`?)")
        }
        None => message,
    }
}

/// Suggestions for an unknown type: names of `kinds`, and vertex sets where one fits.
pub(crate) fn unknown_type_candidates(
    snapshot: &Snapshot,
    reference: &crate::analysis::Reference,
    kinds: &[SymbolKind],
) -> Vec<String> {
    let mut names: Vec<String> = kinds
        .iter()
        .flat_map(|k| snapshot.workspace.of_kind(*k))
        .map(|s| s.name.clone())
        .collect();
    if reference.role == Role::VertexSource {
        names.extend(
            snapshot
                .analysis
                .visible_symbols(reference.span.start)
                .into_iter()
                .filter(|s| s.kind == SymbolKind::VertexSet)
                .map(|s| s.name.clone()),
        );
    }
    names
}

/// The names of the attributes of any of `owners`.
pub(crate) fn attribute_candidates(
    workspace: &crate::workspace::Workspace,
    owners: &[String],
) -> Vec<String> {
    owners
        .iter()
        .flat_map(|o| workspace.attributes(Some(o)))
        .map(|a| a.name.clone())
        .collect()
}

/// Without a schema in the workspace the checks of types and attributes are
/// off, which looks the same as "all fine": say so, at the first use of a
/// vertex or edge type in a query or loading job, on the name of that query
/// or job so that it is seen when the file is opened.
fn no_schema_notice(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    // (While the workspace is still being indexed the schema is not known yet.)
    if snapshot.workspace.has_schema() || snapshot.workspace.indexing() {
        return;
    }
    let first = snapshot
        .analysis
        .unresolved_uses()
        .find(|r| {
            matches!(
                r.role,
                Role::VertexType
                    | Role::VertexSource
                    | Role::EdgeType
                    | Role::EdgeSource
                    | Role::SchemaType
            ) && matches!(r.context, Context::Query | Context::LoadingJob)
        });
    let Some(reference) = first else {
        return;
    };
    let name = snapshot
        .root()
        .descendant_for_byte_range(reference.span.start, reference.span.start)
        .into_iter()
        .flat_map(syntax::self_and_ancestors)
        .find_map(|node| {
            node.child_by_field_name("name").filter(|_| {
                node.kind().ends_with("_definition")
                    || node.kind().contains("job")
            })
        });
    let line = snapshot.range(reference.span).start.line + 1;
    out.push(diagnostic(
        snapshot,
        name.map_or(reference.span, Span::of),
        severity::WARNING,
        "no-schema",
        format!(
            "No schema found, so vertex types, edge types and attributes are not checked (first used: `{}` on \
             line {line}). Open the project folder, or put a `.gsqlroot` file in the folder that holds the \
             schema files.",
            reference.name
        ),
    ));
}

fn unknown_types(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let workspace = snapshot.workspace;
    if !workspace.has_schema() {
        return;
    }
    for reference in snapshot.analysis.unresolved_uses() {
        let what = match &reference.role {
            Role::VertexType | Role::VertexSource => "vertex type",
            Role::EdgeType | Role::EdgeSource => "edge type",
            Role::SchemaType => "vertex or edge type",
            _ => continue,
        };
        let kinds = reference
            .role
            .global_kinds()
            .unwrap_or_default();
        // Only check uses inside queries, loading jobs, edge endpoints and
        // graph member lists, and what ALTER statements and schema change jobs
        // name: DROP commonly names types that exist only in the database
        // (or no longer exist in the schema files).
        let checked = match reference.context {
            Context::Query | Context::LoadingJob => true,
            Context::Definition => {
                matches!(reference.role, Role::VertexType | Role::SchemaType)
            }
            Context::SchemaEdit => {
                schema_edit_checked(snapshot, reference.span)
            }
            Context::Command => false,
        };
        if !checked {
            continue;
        }
        if workspace.declares(kinds, &reference.name) {
            continue;
        }
        let message = if reference.role == Role::VertexSource {
            format!(
                "`{}` is not a vertex set variable or a known vertex type",
                reference.name
            )
        } else {
            format!("Unknown {what} `{}`", reference.name)
        };
        let names = unknown_type_candidates(snapshot, reference, kinds);
        let message = with_suggestion(
            message,
            &reference.name,
            names.iter().map(String::as_str),
        );
        out.push(diagnostic(
            snapshot,
            reference.span,
            severity::WARNING,
            "unknown-type",
            message,
        ));
    }
}

/// The graph named by `CREATE QUERY`, `CREATE LOADING JOB` and `CREATE
/// SCHEMA_CHANGE JOB ... FOR GRAPH X` and by `USE GRAPH X` must be one of the
/// workspace's `CREATE GRAPH` names. Silent when the workspace creates no graph
/// at all (a partial workspace): graph membership of types is not modelled.
fn unknown_graphs(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let workspace = snapshot.workspace;
    if !workspace.has_schema() {
        return;
    }
    let graphs = workspace.of_kind(SymbolKind::Graph);
    if graphs.is_empty() {
        return;
    }
    for reference in snapshot.analysis.unresolved_uses() {
        if reference.role != Role::Graph {
            continue;
        }
        let Some(node) = snapshot.node_at(reference.span) else {
            continue;
        };
        let Some(parent) = node.parent() else {
            continue;
        };
        let checked = match parent.kind() {
            "use_statement" => true,
            "for_graph_clause" => parent.parent().is_some_and(|definition| {
                matches!(
                    definition.kind(),
                    "query_definition"
                        | "opencypher_query_definition"
                        | "loading_job_definition"
                        | "schema_change_job_definition"
                )
            }),
            _ => false,
        };
        if !checked
            || workspace.declares(&[SymbolKind::Graph], &reference.name)
        {
            continue;
        }
        let message = with_suggestion(
            format!("Unknown graph `{}`", reference.name),
            &reference.name,
            graphs.iter().map(|g| g.name.as_str()),
        );
        out.push(diagnostic(
            snapshot,
            reference.span,
            severity::WARNING,
            "unknown-graph",
            message,
        ));
    }
}

/// `CREATE GRAPH g (v)` listing vertex types and no edge type. The TigerGraph
/// documentation disagrees with itself: the `exitOnError` example (Running
/// GSQL) fails this with "There is no edge type specified! Please specify at
/// least one edge type!", while the loading-job UDT example creates
/// `CREATE GRAPH Test_Graph (Vertex_UDT)` and loads into it. So only a hint,
/// and only when every member is known: a vertex type, none an edge type.
fn graph_without_edges(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let workspace = snapshot.workspace;
    for definition in syntax::named_children(snapshot.root())
        .into_iter()
        .filter(|n| n.kind() == "graph_definition")
    {
        if definition.has_error()
            || definition
                .child_by_field_name("base")
                .is_some()
        {
            continue;
        }
        let members: Vec<Node> = definition
            .children_by_field_name("member", &mut definition.walk())
            .collect();
        if members.is_empty() || syntax::has_child(definition, "wildcard") {
            continue;
        }
        let kinds: Vec<Option<bool>> = members
            .iter()
            .map(|m| {
                let name = syntax::text(*m, snapshot.text());
                if workspace.declares(&[SymbolKind::EdgeType], name) {
                    Some(true)
                } else if workspace.declares(&[SymbolKind::VertexType], name)
                {
                    Some(false)
                } else {
                    None
                }
            })
            .collect();
        if kinds.iter().all(|k| *k == Some(false)) {
            let name = definition
                .child_by_field_name("name")
                .unwrap_or(definition);
            out.push(diagnostic(
                snapshot,
                Span::of(name),
                severity::INFORMATION,
                "graph-without-edges",
                "This graph lists vertex types but no edge type. The TigerGraph documentation says `CREATE GRAPH` \
                 needs at least one edge type (\"There is no edge type specified!\") in one place and creates \
                 vertex-only graphs in another; add an edge type, or `(*)`, if the server refuses it"
                    .to_string(),
            ));
        }
    }
}

/// Whether the type name at `span` inside a schema edit refers to a type that
/// has to exist: the target of an ALTER, an endpoint of an added edge type or a
/// member added to a graph (types that the jobs add are in the workspace
/// index, wherever they are). `DROP`s are not checked.
fn schema_edit_checked(snapshot: &Snapshot, span: Span) -> bool {
    let Some(node) = snapshot.node_at(span) else {
        return false;
    };
    let Some(statement) = syntax::find_ancestor(
        node,
        &[
            "alter_type_statement",
            "alter_graph_statement",
            "add_to_graph_statement",
            "edge_definition",
            "vertex_definition",
            "drop_statement",
        ],
    ) else {
        return false;
    };
    match statement.kind() {
        "alter_graph_statement" => syntax::has_child(statement, "ADD"),
        // `ALTER EDGE e DROP PAIR (FROM A, TO Gone)`: only the altered type has to exist.
        "alter_type_statement" if syntax::has_child(statement, "DROP") => {
            statement
                .child_by_field_name("name")
                .is_some_and(|name| Span::of(name) == span)
        }
        "drop_statement" => false,
        _ => true,
    }
}

/// Attributes every vertex or edge has without declaring them.
const IMPLICIT_ATTRIBUTES: &[&str] = &["type"];

fn unknown_attributes(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let workspace = snapshot.workspace;
    let unclosed = UnclosedLists::new(snapshot.root());
    for reference in &snapshot.analysis.references {
        let Role::Attribute(ty) = &reference.role else {
            continue;
        };
        if reference.declaration || reference.in_error {
            continue;
        }
        if let Some(message) = primary_id_read(snapshot, reference, ty) {
            out.push(diagnostic(
                snapshot,
                reference.span,
                severity::WARNING,
                "unknown-attribute",
                message,
            ));
            continue;
        }
        if reference.target.is_some() {
            continue;
        }
        if IMPLICIT_ATTRIBUTES
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&reference.name))
        {
            continue;
        }
        // `v.outdegree(` is a method call being typed, not an attribute.
        if snapshot.text()[reference.span.end..]
            .trim_start()
            .starts_with('(')
            || unclosed.contains(snapshot.root(), reference.span)
        {
            continue;
        }
        let Some((owner_kind, owners)) = ty.schema_owners() else {
            continue;
        };
        if owners.is_empty() {
            continue;
        }
        // Only judge when every possible owner is declared in the workspace.
        if owners
            .iter()
            .any(|o| !workspace.declares(&[owner_kind], o))
        {
            continue;
        }
        let known = workspace
            .find(SymbolKind::Attribute, &reference.name)
            .iter()
            .any(|a| {
                owners
                    .iter()
                    .any(|o| a.owner.as_deref() == Some(o.as_str()))
            });
        if known {
            continue;
        }
        let message = match owners {
            [single] => {
                format!("`{single}` has no attribute `{}`", reference.name)
            }
            many => format!(
                "None of {} has an attribute `{}`",
                many.join(", "),
                reference.name
            ),
        };
        let names = attribute_candidates(workspace, owners);
        let message = with_suggestion(
            message,
            &reference.name,
            names.iter().map(String::as_str),
        );
        out.push(diagnostic(
            snapshot,
            reference.span,
            severity::WARNING,
            "unknown-attribute",
            message,
        ));
    }
}

/// The message for `v.id` in an expression when `id` is only the `PRIMARY_ID`
/// of every vertex type `v` can have. Docs (defining a graph schema, PRIMARY_ID):
/// "By default, a vertex type which is defined with PRIMARY_ID cannot treat the
/// ID as a regular attribute. For example, if the ID's field name is serial_num,
/// neither of the following syntaxes are valid: v.serial_num // error: not
/// supported"; with `WITH primary_id_as_attribute="true"` "the ID can be treated
/// as a readable attribute". (`PRIMARY KEY` forms declare ordinary attributes.)
fn primary_id_read(
    snapshot: &Snapshot,
    reference: &crate::analysis::Reference,
    ty: &Ty,
) -> Option<String> {
    let owners = ty.vertex_types()?;
    if owners.is_empty() {
        return None;
    }
    // (The cheap test first: looking a node up costs the width of a tree the parser gave up on.)
    let found = snapshot
        .workspace
        .find(SymbolKind::Attribute, &reference.name);
    for owner in owners {
        let mut declared = found
            .iter()
            .filter(|a| a.owner.as_deref() == Some(owner.as_str()))
            .peekable();
        // Judge only when the id is the sole declaration of the name for every owner.
        if declared.peek().is_none() || !declared.all(|a| a.id_only) {
            return None;
        }
    }
    // Only reads in expressions: INSERT and LOAD column lists name the id legitimately.
    let node = snapshot.node_at(reference.span)?;
    let parent = node.parent()?;
    if parent.kind() != "member_expression"
        || parent.child_by_field_name("property") != Some(node)
    {
        return None;
    }
    if snapshot.text()[reference.span.end..]
        .trim_start()
        .starts_with('(')
    {
        return None;
    }
    let types = owners.join(", ");
    Some(format!(
        "`{}` is the PRIMARY_ID of `{types}`, which is not an attribute unless the vertex type is declared WITH \
         primary_id_as_attribute=\"true\" (use `to_vertex` to look a vertex up by its id)",
        reference.name
    ))
}

/// Answers "is this text inside an unclosed list" for many spans of one tree.
/// A lookup through a node with thousands of children (what the parser makes
/// of a statement list it gave up on) costs that width per call in
/// tree-sitter, for the node and again for each parent step; so the lookup
/// descends on its own, searching the children of a wide node from a list
/// kept per node.
struct UnclosedLists<'tree> {
    /// The nodes that have a missing token among their children.
    parents_of_missing: HashSet<usize>,
    wide: std::cell::RefCell<HashMap<usize, Rc<Vec<Node<'tree>>>>>,
}

/// Nodes with more children than this are searched by bisection.
const WIDE_NODE: u32 = 32;

impl<'tree> UnclosedLists<'tree> {
    /// Finds the nodes that have a missing token among their children, in
    /// one walk.
    fn new(root: Node<'tree>) -> Self {
        let mut parents_of_missing = HashSet::new();
        if root.has_error() {
            let mut cursor = root.walk();
            'walk: loop {
                let node = cursor.node();
                if node.is_missing() {
                    let mut up = cursor.clone();
                    if up.goto_parent() {
                        parents_of_missing.insert(up.node().id());
                    }
                }
                // Only subtrees with an error can hold a missing token.
                if node.has_error() && cursor.goto_first_child() {
                    continue;
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break 'walk;
                    }
                }
            }
        }
        UnclosedLists {
            parents_of_missing,
            wide: Default::default(),
        }
    }

    /// The first child of `node` that ends at or after `end` (the one the
    /// text up to `end` is in), if it also starts at or before `start`.
    fn child_around(
        &self,
        node: Node<'tree>,
        start: usize,
        end: usize,
    ) -> Option<Node<'tree>> {
        let child = if node.child_count() > WIDE_NODE {
            let children = Rc::clone(
                self.wide
                    .borrow_mut()
                    .entry(node.id())
                    .or_insert_with(|| {
                        let mut cursor = node.walk();
                        Rc::new(node.children(&mut cursor).collect())
                    }),
            );
            let at = children.partition_point(|c| c.end_byte() < end);
            *children.get(at)?
        } else {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .find(|c| c.end_byte() >= end)?
        };
        (child.start_byte() <= start).then_some(child)
    }

    /// Whether the text at `span` sits inside a construct (a call, a list of
    /// values) whose closing token is missing: it is being typed, and what
    /// the parser made of the unfinished text is not to be judged.
    fn contains(&self, root: Node<'tree>, span: Span) -> bool {
        // (Looking a node up costs its depth: only texts with errors need it.)
        if !root.has_error() {
            return false;
        }
        let mut path = vec![root];
        while let Some(child) =
            self.child_around(path[path.len() - 1], span.start, span.end)
        {
            path.push(child);
        }
        // (A few levels reach the call or list; nested expressions can be very deep.)
        for ancestor in path[..path.len() - 1].iter().rev().take(8) {
            let kind = ancestor.kind();
            if kind.ends_with("_statement")
                || kind.ends_with("_body")
                || kind == "source_file"
            {
                break;
            }
            if self
                .parents_of_missing
                .contains(&ancestor.id())
            {
                return true;
            }
        }
        false
    }
}

fn undefined_names(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let workspace = snapshot.workspace;
    let has_schema = workspace.has_schema();
    for reference in snapshot.analysis.unresolved_uses() {
        if reference.role != Role::Value {
            continue;
        }
        if !in_query(snapshot, reference.scope)
            || builtins::constant(&reference.name).is_some()
        {
            continue;
        }
        // `S = ANY;` and `S = _;` name all vertices, as `{ANY}` and `{_}` do.
        if reference.name == "_" || reference.name.eq_ignore_ascii_case("any")
        {
            continue;
        }
        let global_kinds = [
            SymbolKind::VertexType,
            SymbolKind::EdgeType,
            SymbolKind::Query,
            SymbolKind::TupleType,
            SymbolKind::Graph,
        ];
        if workspace.declares(&global_kinds, &reference.name) {
            continue;
        }
        // `lib.query(...)`: package-qualified calls are not resolvable here.
        if reference.qualifier {
            continue;
        }
        // Without a known schema, a capitalized name may be a vertex type.
        let capitalized = reference
            .name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase());
        if !has_schema && capitalized {
            continue;
        }
        out.push(diagnostic(
            snapshot,
            reference.span,
            severity::WARNING,
            "undefined-name",
            format!("`{}` is not defined", reference.name),
        ));
    }
}

/// Calls of unknown functions and uses of unknown types that are spelled like
/// a known one. Other unknown names stay quiet: they may be user-defined
/// functions (written in C++) or types that exist only in the database.
fn misspelled_names(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let analysis = snapshot.analysis;
    let workspace = snapshot.workspace;
    let unclosed = UnclosedLists::new(snapshot.root());
    for reference in analysis.unresolved_uses() {
        if reference.qualifier {
            continue;
        }
        if unclosed.contains(snapshot.root(), reference.span) {
            continue;
        }
        let name = reference.name.as_str();
        // Set for a method an accumulator of a known type does not have.
        let mut unknown_accumulator_method: Option<&str> = None;
        let visible = |kinds: &[SymbolKind]| -> Vec<String> {
            let mut names: Vec<String> = analysis
                .visible_symbols(reference.span.start)
                .into_iter()
                .filter(|s| kinds.contains(&s.kind))
                .map(|s| s.name.clone())
                .collect();
            for kind in kinds {
                names.extend(
                    workspace
                        .of_kind(*kind)
                        .into_iter()
                        .map(|s| s.name.clone()),
                );
            }
            names
        };
        let kinds = reference
            .role
            .global_kinds()
            .unwrap_or_default();
        let (what, candidates): (&str, Vec<String>) = match &reference.role {
            Role::Function => {
                let known = builtins::function(name).is_some()
                    || workspace.declares(kinds, name);
                if known {
                    continue;
                }
                let loading = reference.context == Context::LoadingJob;
                let mut names: Vec<String> = builtins::FUNCTIONS
                    .iter()
                    .filter(|f| {
                        if loading {
                            f.in_loading_jobs()
                        } else {
                            f.in_queries()
                        }
                    })
                    .map(|f| f.name.to_string())
                    .collect();
                if !loading {
                    names.extend(visible(kinds));
                }
                ("function", names)
            }
            Role::TupleType => {
                if workspace.declares(kinds, name) {
                    continue;
                }
                let mut names = visible(kinds);
                names.extend(
                    builtins::ACCUMULATORS
                        .iter()
                        .map(|a| a.name.to_string()),
                );
                names.extend(
                    builtins::PRIMITIVE_TYPES
                        .iter()
                        .map(|(t, _)| t.to_string()),
                );
                // `PIRNT x;` parses as a declaration of `x` with type `PIRNT`.
                names.extend(
                    super::autocorrect::keywords()
                        .iter()
                        .cloned(),
                );
                ("type", names)
            }
            // `@@l.pussh_back(1)`: a method the type does not have, but one spelled like it.
            Role::Method(ty) => {
                let methods = super::resolve::methods_for(ty);
                let accumulator =
                    declared_accumulator_receiver(snapshot, reference, ty);
                if (methods.is_empty() && accumulator.is_none())
                    || builtins::find_method(methods, name).is_some()
                {
                    continue;
                }
                if let Some(kind) = accumulator {
                    unknown_accumulator_method = Some(kind);
                }
                (
                    "method",
                    methods
                        .iter()
                        .map(|m| m.name.to_string())
                        .collect(),
                )
            }
            _ => continue,
        };
        // Only the closest spellings (`cout`: `count`, not also `cot`).
        let suggestions = super::autocorrect::closest(
            name,
            candidates.iter().map(String::as_str),
        );
        let best = suggestions.first();
        if best.is_none() && unknown_accumulator_method.is_none() {
            continue;
        }
        let subject = unknown_accumulator_method
            .map(|kind| format!(" of {kind}"))
            .unwrap_or_default();
        let message = match best {
            Some(best) => {
                format!(
                    "Unknown {what} `{name}`{subject}; did you mean `{best}`?"
                )
            }
            None => format!("Unknown {what} `{name}`{subject}"),
        };
        let code = match what {
            "function" => "unknown-function",
            "method" => "unknown-method",
            _ => "unknown-type-name",
        };
        let mut d = diagnostic(
            snapshot,
            reference.span,
            severity::WARNING,
            code,
            message,
        );
        for suggestion in suggestions.iter().take(3) {
            add_fix(
                &mut d,
                format!("Change to `{suggestion}`"),
                vec![snapshot.edit(reference.span, *suggestion)],
                false,
            );
        }
        out.push(d);
    }
}

/// The canonical accumulator type of the receiver of the method call `reference`
/// when the receiver is certainly a declared accumulator of a built-in type: a
/// global `@@x`, an `@x` or `v.@x`, whose declaration is not in a syntax error.
/// The type of anything else (a method result, an index) is not trusted here.
fn declared_accumulator_receiver(
    snapshot: &Snapshot,
    reference: &crate::analysis::Reference,
    ty: &Ty,
) -> Option<&'static str> {
    let Ty::Accumulator(kind, _) = ty else {
        return None;
    };
    let accumulator = builtins::accumulator(kind)?;
    let property = snapshot.root().descendant_for_byte_range(
        reference.span.start,
        reference.span.start,
    )?;
    let member = property
        .parent()
        .filter(|m| m.kind() == "member_expression")?;
    let object = member.child_by_field_name("object")?;
    let name = match object.kind() {
        "global_accumulator" | "local_accumulator" => object,
        "member_expression" => object
            .child_by_field_name("property")
            .filter(|p| p.kind() == "local_accumulator")?,
        _ => return None,
    };
    let declared = snapshot
        .analysis
        .reference_at(name.start_byte())
        .and_then(|r| snapshot.analysis.target_symbol(r))?;
    (!declared.in_error).then_some(accumulator.name)
}

fn unused_declarations(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let analysis = snapshot.analysis;
    let used: HashSet<usize> = analysis
        .references
        .iter()
        .filter(|r| !r.declaration)
        .filter_map(|r| r.target)
        .collect();
    for (id, symbol) in analysis.symbols.iter().enumerate() {
        let checked = match symbol.kind {
            kind if kind.is_accumulator() => true,
            SymbolKind::Variable => !symbol.implicit,
            _ => false,
        };
        if !checked
            || symbol.in_error
            || used.contains(&id)
            || !in_query(snapshot, symbol.scope)
        {
            continue;
        }
        let mut d = diagnostic(
            snapshot,
            symbol.name_span,
            severity::HINT,
            "unused",
            format!("`{}` is declared but never used", symbol.name),
        );
        d.tags = vec![diagnostic_tag::UNNECESSARY];
        out.push(d);
    }
}

fn query_arity(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let source = snapshot.text();
    syntax::walk_with_errors(snapshot.root(), |node, in_error| {
        if in_error {
            return;
        }
        let Some((query, name_node, arguments)) =
            resolve::query_called_by(snapshot, node)
        else {
            return;
        };
        if arguments.has_error() {
            return;
        }
        let name = syntax::text(name_node, source);
        let values = syntax::code_children(arguments);
        // `RUN QUERY q({"name": "Emma", "age": 21})` passes parameters by
        // name; the others keep their default values.
        if node.kind() != "call_expression"
            && let [object] = values.as_slice()
            && let Some(names) = parameter_names(*object, source)
            && !(query.params.len() == 1
                && names
                    .iter()
                    .all(|(n, ..)| *n != query.params[0].name))
        {
            for (key, span, value) in names {
                if let Some(param) =
                    query.params.iter().find(|p| p.name == key)
                {
                    if let Some(value) = value {
                        argument_type(snapshot, name, param, value, out);
                    }
                    continue;
                }
                let message =
                    format!("Query `{name}` has no parameter `{key}`");
                let mut d = diagnostic(
                    snapshot,
                    span,
                    severity::WARNING,
                    "argument-name",
                    message,
                );
                let candidates = query.params.iter().map(|p| p.name.as_str());
                for suggestion in
                    super::autocorrect::similar(&key, candidates)
                        .into_iter()
                        .take(3)
                {
                    let edit =
                        snapshot.edit(span, format!("\"{suggestion}\""));
                    add_fix(
                        &mut d,
                        format!("Change to `{suggestion}`"),
                        vec![edit],
                        false,
                    );
                }
                out.push(d);
            }
            return;
        }
        let given = values.len();
        let total = query.params.len();
        let required = query
            .params
            .iter()
            .filter(|p| p.default.is_none())
            .count();
        for (value, param) in values.iter().zip(&query.params) {
            argument_type(snapshot, name, param, *value, out);
        }
        if given > total || given < required {
            let callee = format!("Query `{name}`");
            out.push(argument_count(
                snapshot,
                arguments,
                &callee,
                required..=total,
                given,
            ));
        }
    });
}

/// The warning that `callee` takes `expected` arguments, not `given`.
fn argument_count(
    snapshot: &Snapshot,
    arguments: Node,
    callee: &str,
    expected: std::ops::RangeInclusive<usize>,
    given: usize,
) -> Diagnostic {
    let (required, most) = expected.into_inner();
    let expected = if required == most {
        format!("{most}")
    } else {
        format!("{required} to {most}")
    };
    diagnostic(
        snapshot,
        Span::of(arguments),
        severity::WARNING,
        "argument-count",
        format!(
            "{callee} expects {expected} argument{}, but {given} given",
            plural(most)
        ),
    )
}

/// Calls of built-in functions and methods with too few or too many
/// arguments. Only signatures that are a plain list of names are checked
/// (not variadic ones), functions a query or tuple type of that name replaces
/// are not, and loading job functions only when the reference pages document
/// them (the functions shared with queries are not checked there). `log`
/// is also the LOG statement, which takes any arguments.
fn builtin_arity(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let source = snapshot.text();
    syntax::walk_with_errors(snapshot.root(), |node, in_error| {
        if in_error || node.kind() != "call_expression" {
            return;
        }
        let (Some(function), Some(arguments)) = (
            node.child_by_field_name("function"),
            node.child_by_field_name("arguments"),
        ) else {
            return;
        };
        if arguments.has_error() {
            return;
        }
        let (name, params) = match function.kind() {
            "identifier" => {
                let name = syntax::text(function, source);
                // A query or tuple type of that name replaces the built-in.
                let Some(resolve::Callee::Builtin(builtin)) =
                    resolve::callee(snapshot, function)
                else {
                    return;
                };
                let in_job = snapshot
                    .analysis
                    .reference_at(function.start_byte())
                    .is_some_and(|r| r.context == Context::LoadingJob);
                let known = if in_job {
                    // `split` is documented with other signatures there.
                    builtin.category == builtins::Category::Loading
                        && builtin_docs::function(name).is_some()
                } else {
                    builtin.in_queries()
                };
                if !known || builtin.name == "log" {
                    return;
                }
                (builtin.name, builtin.params)
            }
            "member_expression" => {
                let Some(property) = function.child_by_field_name("property")
                else {
                    return;
                };
                let Some(reference) = snapshot
                    .analysis
                    .reference_at(property.start_byte())
                else {
                    return;
                };
                let Role::Method(ty) = &reference.role else {
                    return;
                };
                let Some(method) = builtins::find_method(
                    super::resolve::methods_for(ty),
                    &reference.name,
                ) else {
                    return;
                };
                (method.name, method.params)
            }
            _ => return,
        };
        let Some((required, most)) = builtins::arity(params) else {
            return;
        };
        let given = syntax::code_children(arguments).len();
        if !(required..=most).contains(&given) {
            let callee = format!("`{name}`");
            out.push(argument_count(
                snapshot,
                arguments,
                &callee,
                required..=most,
                given,
            ));
        }
    });
}

/// The parameter names (with their spans) and values of a JSON object of
/// parameters, `{"name": "Emma", "age": 21}`.
fn parameter_names<'t>(
    object: Node<'t>,
    source: &str,
) -> Option<Vec<(String, Span, Option<Node<'t>>)>> {
    if object.kind() != "vertex_set_literal" {
        return None;
    }
    let mut names = Vec::new();
    for pair in syntax::code_children(object) {
        let key = pair
            .child_by_field_name("key")
            .filter(|k| pair.kind() == "pair" && k.kind() == "string")?;
        let text = syntax::text(key, source);
        names.push((
            text.trim_matches('"').to_string(),
            Span::of(key),
            pair.child_by_field_name("value"),
        ));
    }
    Some(names)
}

/// A literal passed to a query parameter that cannot take it: a string or a
/// fraction for an INT, a number for a BOOL, a list for a single value. Only
/// literals are checked, and only where the value is certainly wrong.
fn argument_type(
    snapshot: &Snapshot,
    query: &str,
    param: &Param,
    value: Node,
    out: &mut Vec<Diagnostic>,
) {
    let Some(literal) = resolve::literal_argument(value) else {
        return;
    };
    let ty = resolve::param_type(param);
    let accepted: &[&str] = match ty.as_str() {
        "BOOL" => &["a boolean"],
        "INT" => &["an integer", "a negative integer"],
        "UINT" => &["an integer"],
        "FLOAT" | "DOUBLE" => &[
            "an integer",
            "a negative integer",
            "a number with a fraction",
        ],
        // A number may well be accepted for these; a list or a boolean is not.
        "STRING" | "DATETIME" => &[
            "an integer",
            "a negative integer",
            "a number with a fraction",
            "a string",
        ],
        _ if ty.starts_with("VERTEX") => &["an integer", "a string"],
        _ => return,
    };
    if accepted.contains(&literal) {
        return;
    }
    let message = format!(
        "Query `{query}` expects {} for `{}`, not {literal}",
        param.ty, param.name
    );
    out.push(diagnostic(
        snapshot,
        Span::of(value),
        severity::WARNING,
        "argument-type",
        message,
    ));
}

/// `BUILTIN ...` declarations are for the generated reference file of the
/// built-ins; anywhere else they declare nothing.
fn stray_stubs(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    if super::reference::is_reference_file(snapshot.uri) {
        return;
    }
    for node in syntax::named_children(snapshot.root())
        .into_iter()
        .filter(|n| n.kind() == "stub_declaration")
    {
        out.push(diagnostic(
            snapshot,
            Span::of(node),
            severity::WARNING,
            "stub-declaration",
            "`BUILTIN` declarations belong to the generated reference file of the built-ins; this one declares nothing"
                .to_string(),
        ));
    }
}

/// `=` and `<>` compare values only in SYNTAX V3 queries (TigerGraph 4.1.3
/// and later); V2, the default, needs `==` and `!=`.
fn v3_comparisons(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let source = snapshot.text();
    for query in syntax::named_children(snapshot.root()) {
        if !matches!(
            query.kind(),
            "query_definition" | "interpret_query_statement"
        ) {
            continue;
        }
        let v3 = syntax::declared_syntax_versions(query, source)
            .iter()
            .any(|version| version.eq_ignore_ascii_case("v3"));
        let Some(body) = query
            .child_by_field_name("body")
            .filter(|_| !v3)
        else {
            continue;
        };
        syntax::walk(body, |node| {
            let Some(operator) = node
                .child_by_field_name("operator")
                .filter(|_| node.kind() == "binary_expression")
            else {
                return;
            };
            let replacement = match operator.kind() {
                "=" => "==",
                "<>" => "!=",
                _ => return,
            };
            // Inside code the parser gave up on, an assignment reads as one.
            if syntax::self_and_ancestors(node).any(|a| a.is_error()) {
                return;
            }
            let mut d = diagnostic(
                snapshot,
                Span::of(operator),
                severity::WARNING,
                "v3-comparison",
                format!(
                    "`{}` compares values only in SYNTAX V3 queries; use `{replacement}`",
                    operator.kind()
                ),
            );
            add_fix(
                &mut d,
                format!("Replace with `{replacement}`"),
                vec![snapshot.edit(Span::of(operator), replacement)],
                true,
            );
            out.push(d);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{
        Fixture, SCHEMA_URI, apply_fix, findings, messages_with,
        syntax_messages, with_code,
    };

    fn messages(text: &str) -> Vec<String> {
        messages_with(text, &[])
    }

    /// The first syntax error of a text: (line, message).
    fn first_error(text: &str) -> (u32, String) {
        let fixture = Fixture::new(text);
        let d = syntax_errors(&fixture.snapshot())
            .into_iter()
            .next()
            .expect("a syntax error");
        (d.range.start.line, d.message)
    }

    /// The end offsets of every leaf token of a text (a string is one token), and 0.
    fn token_gaps(text: &str) -> Vec<usize> {
        let fixture = Fixture::new(text);
        let mut gaps = vec![0];
        let snapshot = fixture.snapshot();
        let mut stack = vec![snapshot.root()];
        while let Some(node) = stack.pop() {
            if node.child_count() == 0 || node.kind() == "string" {
                if node.end_byte() > node.start_byte() {
                    gaps.push(node.end_byte());
                }
            } else {
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
        }
        gaps.sort_unstable();
        gaps.dedup();
        gaps
    }

    /// A statement list the parser gives up on becomes one error node with
    /// thousands of direct children. The checks of every reference below it
    /// must not scan those children again (this took tens of seconds at this
    /// size, after a stray `;;;` in a long file).
    #[test]
    fn a_swallowed_rest_of_the_file_does_not_make_the_checks_quadratic() {
        let mut text = String::from("CREATE QUERY q(INT a) FOR GRAPH g {\n");
        for i in 0..1200 {
            text.push_str(&format!(
                "  IF a == {i} THEN @@k{i} += 1;\n  ELSE IF a == 3 THEN @@m += foo{i};\n"
            ));
        }
        text.push_str("}\n");
        let fixture = Fixture::new(&text);
        let start = std::time::Instant::now();
        let all = diagnostics(&fixture.snapshot());
        assert!(!all.is_empty());
        assert!(
            start.elapsed().as_secs_f64() < 3.0,
            "took {:?}",
            start.elapsed()
        );
    }

    fn codes_and_messages(text: &str) -> Vec<(String, String)> {
        let fixture = Fixture::new(text);
        let mut all: Vec<_> = diagnostics(&fixture.snapshot())
            .into_iter()
            .map(|d| (format!("{:?}", d.code), d.message))
            .collect();
        all.sort();
        all
    }

    #[test]
    fn comments_between_any_two_tokens_change_no_diagnostic() {
        let texts = [
            "CREATE QUERY q(FLOAT a, FLOAT b) {\n  IF a == b THEN PRINT 1; END;\n}\n",
            "CREATE QUERY q(FLOAT a) {\n  IF a == (0) THEN PRINT 1; END;\n  IF (a) == ((floor((a)))) THEN PRINT 2; END;\n}\n",
            "CREATE QUERY q(FLOAT a) {\n  IF a == -(1.5) THEN PRINT 1; END;\n}\n",
            "CREATE QUERY q() {\n  INT x = (\"abc\");\n  INT y = -\"s\";\n  PRINT x + y;\n}\n",
            "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  @@n += (\"abc\");\n  INT z = 1 - \"a\";\n}\n",
            "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  S = SELECT s FROM Person:s ACCUM @@n = 1;\n  PRINT S;\n}\n",
            "CREATE QUERY q() {\n  S = SELECT s FROM Person:s LIMIT 5 OFFSET 2;\n  PRINT S;\n}\n",
            "CREATE QUERY q() {\n  ListAccum<INT> @@l;\n  PRINT @@l.clear();\n  BREAK;\n}\n",
            "CREATE QUERY q() {\n  BREAK;\n  CONTINUE;\n}\n",
            "CREATE QUERY q() SYNTAX V2 {\n  S = SELECT s FROM (s:Person)-[e:Knows]-(t);\n  PRINT S;\n}\n",
            "CREATE QUERY q() {\n  INT a = 1;\n  IF a = 1 THEN PRINT abs(); END;\n  PRINT undefined_name;\n}\n",
            "CREATE QUERY q(INT n) {\n  PRINT n;\n}\nRUN QUERY q(\"x\")\nRUN QUERY q(1, 2)\nRUN QUERY q({\"m\": 1})\n",
            "CREATE QUERY q(UINT n, BOOL b) {\n  PRINT n;\n}\nRUN QUERY q(-1, 2)\nRUN QUERY q(1.5, [1])\n",
            "INTERPRET QUERY () {\n  S = SELECT s FROM (s:_)-[:Knows]->(t:Person);\n  PRINT S;\n}\n",
            "INTERPRET QUERY () {\n  TRY PRINT 1; EXCEPTION WHEN ANY THEN PRINT 2; END;\n  RETURN 1;\n}\n",
            "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  S = SELECT s FROM Person:s ACCUM @@n += 1\n  PRINT S;\n}\n",
            "CREATE QUERY q() {\n  INT count = 1;\n  sumaccum<INT> @@a;\n  FOREACH i IN RANGE[0, 2] DO\n    SumAccum<INT> @c;\n  END;\n}\n",
        ];
        let mut reported = 0;
        for text in texts {
            let expected = codes_and_messages(text);
            reported += usize::from(!expected.is_empty());
            for comment in ["/* c */", "// c\n"] {
                for gap in token_gaps(text) {
                    let mut commented = text.to_string();
                    commented.insert_str(gap, comment);
                    assert_eq!(
                        codes_and_messages(&commented),
                        expected,
                        "{comment:?} at {gap} in {text}"
                    );
                }
            }
        }
        // Most texts have something to report; the rest must stay clean.
        assert!(reported >= 12, "{reported}");
    }

    #[test]
    fn an_unfinished_statement_is_reported_where_the_text_stops() {
        let cases = [
            (
                "CREATE QUERY q() {\n  PRINT 1;\n  INT x = 1 +\n",
                2,
                "after `+`",
            ),
            ("CREATE QUERY q() {\n  INT x = \n", 1, "after `=`"),
            (
                "CREATE QUERY q() {\n  PRINT 1;\n  PRINT foo(",
                2,
                "after `(`",
            ),
            (
                "CREATE QUERY q() {\n PRINT 1;\n WHILE true DO\n PRINT 2;\n IF x THEN",
                4,
                "after `THEN`",
            ),
            ("INTERPRET QUERY () {\n  PRINT 1;\n  x = ", 2, "after `=`"),
            (
                "CREATE QUERY q() {\n  PRINT 1;\n  S = SELECT s FROM Person:s -(Knows:e)->",
                2,
                "vertex pattern after `->`",
            ),
            (
                "CREATE QUERY q() FOR GRAPH G\n{\n  S = SELECT s FROM Person:s -(Knows:e)-",
                2,
                "vertex pattern after `-`",
            ),
            // Keywords spelled with `-`, `_` or a space.
            (
                "CREATE QUERY q() {\n  R = SELECT s FROM Person:s ACCUM s.@a += 1 POST-ACCUM",
                1,
                "more input after `POST-ACCUM`",
            ),
            (
                "CREATE QUERY q() {\n  R = SELECT s FROM Person:s ACCUM s.@a += 1 POST_ACCUM\n",
                1,
                "more input after `POST_ACCUM`",
            ),
            (
                "CREATE QUERY q() {\n  IF true THEN PRINT 1; ELSE IF ",
                1,
                "more input after `ELSE IF`",
            ),
            // An accumulator name that is not typed yet.
            (
                "CREATE QUERY q() {\n  SumAccum<INT> @@",
                1,
                "more input after `@@`",
            ),
            (
                "CREATE QUERY q() {\n  SumAccum<INT> @",
                1,
                "more input after `@`",
            ),
        ];
        for (text, line, expected) in cases {
            let (at, message) = first_error(text);
            assert_eq!(at, line, "{text:?}: {message}");
            assert!(
                message.starts_with("Syntax error: unfinished statement")
                    && message.contains(expected),
                "{message}"
            );
        }
        // A closed body keeps its own message; an error in the middle stays where it is.
        assert_eq!(
            first_error("CREATE QUERY q() {\n  PRINT 1;\n").1,
            "Syntax error: missing `}` to close the query body"
        );
        assert_eq!(first_error("CREATE QUERY q() {\n  PRINT 1 2;\n}\n").0, 1);
    }

    #[test]
    fn calls_being_typed_are_not_unknown_attributes_or_functions() {
        let query = "CREATE QUERY q() FOR GRAPH G {\n  SumAccum<INT> @@n;\n  S = SELECT v FROM Person:v ACCUM @@n += v.outdegree(";
        let found = movie_messages(query);
        assert!(
            !found
                .iter()
                .any(|m| m.contains("no attribute")),
            "{found:?}"
        );
        let found = movie_messages(&format!("{query}\"X\""));
        assert!(
            !found
                .iter()
                .any(|m| m.contains("no attribute")),
            "{found:?}"
        );
        // The same text with a misspelled attribute is still flagged.
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G {\n  S = SELECT v FROM Person:v WHERE v.nme == \"a\";\n  PRINT S;\n}\n",
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("no attribute `nme`")),
            "{found:?}"
        );
        let job = "CREATE LOADING JOB j FOR GRAPH G {\n  LOAD f TO VERTEX V_List VALUES ($0, LIST($2,$4)";
        assert!(
            !messages(job)
                .iter()
                .any(|m| m.contains("Unknown function")),
            "{:?}",
            messages(job)
        );
    }

    #[test]
    fn documented_admin_commands_are_valid() {
        let text = "GRANT DATA_SOURCE k1 TO GRAPH graph1, graph2\nREVOKE DATA_SOURCE k1 FROM GRAPH graph1\nRUN LOADING JOB -n 10,$ j1\nSHOW VERTEX co?*y\nCREATE VERTEX V (PRIMARY_ID id STRING, e FIXED_BINARY(8))\nSHOW GRANTS TO ROLE moderator\nUPDATE DESCRIPTION OF QUERY q1 \"text\"\nSHOW DESCRIPTION OF QUERY_PARAM q1.p1\nDROP DESCRIPTION OF QUERY q1, q2 ON GRAPH g\nGET TokenBank TO \"/x.cpp\"\nPUT ExprFunctions FROM \"/x.hpp\"\nALTER VERTEX Person IN GLOBAL SET ROW POLICY lib.f ON (gender)\nSHOW ROW POLICY\nINSTALL FUNCTION lib1.func1\nDROP FUNCTION lib1.*\n";
        let found = messages(text);
        assert!(found.is_empty(), "{found:?}");
        // Still an error: GRANT DATA_SOURCE needs GRAPH.
        assert!(!messages("GRANT DATA_SOURCE k1 TO g1\n").is_empty());
    }

    #[test]
    fn complete_blocks_and_lists_are_not_flagged_as_missing_keywords_or_trailing_commas()
     {
        let valid = "CREATE QUERY q(INT a) FOR GRAPH g {\n  SumAccum<INT> @@s, @@t;\n  IF a == 1 THEN\n    PRINT a;\n  ELSE IF a == 2 THEN\n    PRINT 2;\n  END;\n  WHILE a < 3 LIMIT 4 DO\n    a = a + 1;\n  END;\n  FOREACH i IN RANGE[1,3] DO\n    PRINT i;\n  END;\n  R = SELECT p FROM P:p ACCUM @@s += 1, @@t += 2 POST-ACCUM @@s += 3 ORDER BY p.a, p.b LIMIT 3;\n  PRINT a, R;\n}\n";
        assert!(messages(valid).iter().all(
            |m| !m.contains("missing `THEN`") && !m.contains("trailing")
        ));
        // A misspelled THEN is still a misspelling, not a missing keyword.
        let typo = "CREATE QUERY q(INT a) {\n  IF a == 1 THN\n    PRINT a;\n  END;\n}\n";
        let found = messages(typo);
        assert!(
            found
                .iter()
                .any(|m| m.contains("did you mean `THEN`")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .all(|m| !m.contains("missing `THEN`")),
            "{found:?}"
        );
        // A comma in the middle of a list is not trailing.
        let middle = "CREATE QUERY q() {\n  PRINT 1,, 2;\n}\n";
        assert!(
            messages(middle)
                .iter()
                .all(|m| !m.contains("trailing")),
            "{:?}",
            messages(middle)
        );
    }

    #[test]
    fn a_nested_else_if_inside_a_foreach_of_post_accum_is_valid() {
        // From a real query: `ELSE IF ... ELSE ... END END,` in a FOREACH body.
        let text = include_str!("../../tests/fixtures/nested_else_if.gsql");
        let found = messages(text);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn else_if_may_close_with_one_end_or_nest_with_two() {
        // `ELSE IF` continues its IF (one END) or starts a nested IF (two ENDs).
        let nested = "CREATE QUERY q(BOOL a, BOOL b) {\n  SumAccum<INT> @@n;\n  R = SELECT s FROM P:s ACCUM IF a THEN @@n += 1 ELSE IF b THEN @@n += 2 ELSE @@n += 3 END END;\n  IF a THEN PRINT 1; ELSE IF b THEN PRINT 2; ELSE PRINT 3; END; END;\n  IF a THEN PRINT 1; ELSE IF b THEN PRINT 2; END;\n  PRINT R;\n}\n";
        assert!(messages(nested).is_empty(), "{:?}", messages(nested));
        // A long ELSE IF ladder parses.
        let ladder: String = (0..30)
            .map(|i| format!(" ELSE IF x == {i} THEN PRINT {i};"))
            .collect();
        let text = format!(
            "CREATE QUERY q(INT x) {{\n  IF x < 0 THEN PRINT 0;{ladder} ELSE PRINT 1; END;\n}}\n"
        );
        assert!(messages(&text).is_empty(), "{:?}", messages(&text));
    }

    #[test]
    fn finds_typos_in_loosely_read_commands() {
        let text = "GRANT ROLE analyst ON GRAPRH Social TO alice, bob\nREVOKE ROLE r ON GRAPH Social FROM bob\nEXPORT GRAH ALL TO \"/tmp/x\"\nEXPORT GRAPH ALL TO \"/tmp/x\"\n";
        assert_eq!(
            messages(text),
            [
                "Syntax error: did you mean `GRAPH` instead of `GRAPRH`?",
                "Syntax error: did you mean `GRAPH` instead of `GRAH`?"
            ]
        );
    }

    #[test]
    fn a_keyword_typo_in_a_query_keeps_the_typos_of_loosely_read_commands() {
        let typo =
            "CREATE QUERY q(INT x) {\n  IF x > 0 THN PRINT x; END;\n}\n";
        for (command, expected) in [
            ("GRANT ROLE r ON GRAPRH g TO u\n", "instead of `GRAPRH`"),
            ("EXPORT GRAH ALL TO \"/tmp/x\"\n", "instead of `GRAH`"),
            ("SHOW VERTEX *\nDROPP QUERY q\n", "unknown command `DROPP`"),
        ] {
            let found = syntax_messages(&format!("{command}{typo}"));
            assert!(found.iter().any(|m| m.contains(expected)), "{found:?}");
            assert!(
                found
                    .iter()
                    .any(|m| m.contains("instead of `THN`")),
                "{found:?}"
            );
        }
    }

    #[test]
    fn checks_literal_arguments_against_parameter_types() {
        let text = "CREATE QUERY q(INT n, UINT u, BOOL b, DOUBLE d, STRING s, VERTEX<Person> p) { PRINT n; }\nRUN QUERY q(1, 2, true, 1.5, \"x\", \"person1\")\nRUN QUERY q(\"1\", -2, 1, \"a\", [1], 3)\nRUN QUERY q({\"n\": 2.5, \"b\": false})\n";
        assert_eq!(
            messages(text),
            [
                "Query `q` expects INT for `n`, not a string",
                "Query `q` expects UINT for `u`, not a negative integer",
                "Query `q` expects BOOL for `b`, not an integer",
                "Query `q` expects DOUBLE for `d`, not a string",
                "Query `q` expects STRING for `s`, not a list",
                "Query `q` expects INT for `n`, not a number with a fraction",
            ]
        );
    }

    #[test]
    fn suggests_methods_for_misspelled_calls() {
        let text = "CREATE QUERY q() {\n  ListAccum<INT> @@l;\n  @@l += 1;\n  @@l.pussh_back(1);\n  PRINT @@l.sise(), @@l.size();\n}\n";
        let found = messages(text);
        assert!(
            found.iter().any(|m| m.contains(
                "Unknown method `sise` of ListAccum; did you mean `size`?"
            )),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|m| m == "Unknown method `pussh_back` of ListAccum"),
            "{found:?}"
        );
        assert_eq!(
            found
                .iter()
                .filter(|m| m.contains("Unknown method"))
                .count(),
            2,
            "{found:?}"
        );
    }

    #[test]
    fn reports_methods_an_accumulator_type_does_not_have() {
        let text = "CREATE QUERY q(UINT len) {\n  BitwiseOrAccum<len> @@x;\n  ListAccum<INT> @@l;\n  SumAccum<INT> @c;\n  HeapAccum<Rec>(3, v DESC) @@h;\n  @@x.resize(len + 1);\n  @@l.nosuch(1);\n  @@l.push(1);\n  S = SELECT v FROM Person:v POST-ACCUM v.@c.bar();\n  @@h.resize(5);\n  @@x.clear();\n  @@l.removeOne(1);\n  PRINT @@x, @@l, @@l.size(), @@l.get(0).foo(), range(1, 3).step(2);\n}\n";
        let found: Vec<String> = messages(text)
            .into_iter()
            .filter(|m| m.contains("Unknown method"))
            .collect();
        assert_eq!(
            found,
            [
                "Unknown method `resize` of BitwiseOrAccum",
                "Unknown method `nosuch` of ListAccum",
                "Unknown method `push` of ListAccum",
                "Unknown method `bar` of SumAccum",
            ]
        );
    }

    #[test]
    fn half_typed_definitions_do_not_panic() {
        // The search for a missing comma once ran past the end of the statement.
        for text in [
            "CREATE VERTEX c(P)\nCREATE",
            "CREATE VERTEX c(P)\nCREATE QUERY",
            "CREATE VERTEX c(P INT\nCREATE",
        ] {
            assert!(!messages(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn an_earlier_error_stays_when_the_last_definition_is_unfinished() {
        let text = "CREATE QUERY a() {\n  PRINT 1;\n}\nCREATE QUERY b() {\n  SumAccum<INT> @@n;\n  S = SELECT v FROM Person:v ACCUM @@n += v.outdegree(\n}\nCREATE QUERY c() {\n  PRINT 2 +\n";
        let fixture = Fixture::new(text);
        let lines: Vec<(u32, String)> = syntax_errors(&fixture.snapshot())
            .into_iter()
            .map(|d| (d.range.start.line + 1, d.message))
            .collect();
        assert!(
            lines.iter().any(|(line, _)| *line == 7),
            "b's error: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|(line, m)| *line == 9
                    && m.contains("unfinished statement")),
            "c's: {lines:?}"
        );
    }

    #[test]
    fn a_stray_or_missing_token_explains_the_whole_statement() {
        let stray = "CREATE QUERY q() {\n  INT x = 1;\n  PRINT x x;\n}\n";
        assert_eq!(
            syntax_messages(stray),
            [
                "Syntax error: unexpected `x` in query body: the statement parses without it"
            ]
        );
        // The parser's follow-up errors (here at the `PRINT` and the `}`) are not reported.
        let open = "CREATE QUERY q() {\n  R = SELECT s FROM P:s WHERE s.a == abs(1 ACCUM @@n += 1;\n  PRINT R;\n}\n";
        assert_eq!(
            syntax_messages(open).len(),
            1,
            "{:?}",
            syntax_messages(open)
        );
        // A message that says more than "unexpected" stays.
        let comma = "CREATE QUERY q(INT a, ) {\n  PRINT a;\n}\n";
        assert_eq!(
            syntax_messages(comma),
            ["Syntax error: remove the trailing `,`"]
        );
    }

    #[test]
    fn a_trailing_comma_before_post_accum_is_one_error() {
        for comment in ["", " /* c */", " // c"] {
            let text = format!(
                "CREATE QUERY q() {{\n  SumAccum<INT> @b;\n  S = {{Person.*}};\n  R = SELECT p FROM S:p ACCUM p.@b += 1,{comment}\n  POST-ACCUM p.@b += 2;\n  PRINT R;\n}}\n"
            );
            let found: Vec<String> = messages(&text)
                .into_iter()
                .filter(|m| m.contains("rror"))
                .collect();
            assert_eq!(
                found,
                ["Syntax error: remove the trailing `,`"],
                "{comment:?}: {found:?}"
            );
        }
    }

    #[test]
    fn hints_at_a_graph_without_edge_types() {
        let types = "CREATE VERTEX P (PRIMARY_ID id STRING)\nCREATE VERTEX C (PRIMARY_ID id STRING)\nCREATE DIRECTED EDGE E (FROM P, TO C)\n";
        let codes = |graph: &str| -> Vec<(String, Option<u8>)> {
            let text = format!("{types}{graph}\n");
            let fixture = Fixture::new(&text);
            diagnostics(&fixture.snapshot())
                .into_iter()
                .filter(|d| d.code.as_deref() == Some("graph-without-edges"))
                .map(|d| (d.message, d.severity))
                .collect()
        };
        let found = codes("CREATE GRAPH g (P, C)");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].1, Some(severity::INFORMATION));
        assert!(
            found[0]
                .0
                .contains("There is no edge type specified"),
            "{found:?}"
        );
        // With an edge type, with (*), empty, unknown members or tag-based: nothing.
        for graph in [
            "CREATE GRAPH g (P, C, E)",
            "CREATE GRAPH g (*)",
            "CREATE GRAPH g ()",
            "CREATE GRAPH g (P, Nope)",
            "CREATE GRAPH g AS base (P)",
        ] {
            assert!(codes(graph).is_empty(), "{graph}");
        }
    }

    #[test]
    fn dropped_pairs_may_name_vanished_types() {
        let schema = "CREATE VERTEX A (PRIMARY_ID id STRING)\nCREATE DIRECTED EDGE E (FROM A, TO A)\n";
        let job = "CREATE SCHEMA_CHANGE JOB j FOR GRAPH G {\n  ALTER EDGE E DROP PAIR (FROM A, TO Gone);\n}\n";
        let found = messages_with(job, &[(SCHEMA_URI, schema)]);
        assert!(found.iter().all(|m| !m.contains("Gone")), "{found:?}");
    }

    #[test]
    fn flags_stub_declarations_outside_the_reference_file() {
        let found = messages("BUILTIN FUNCTION abs(x) -> number;\n");
        assert!(
            found
                .iter()
                .any(|m| m.contains("generated reference file")),
            "{found:?}"
        );
    }

    #[test]
    fn only_a_versioned_reference_file_may_hold_stub_declarations() {
        let stubs = |uri: &str| {
            let mut fixture =
                Fixture::new("BUILTIN FUNCTION foo(a) -> INT;\n");
            fixture.uri = uri.to_string();
            diagnostics(&fixture.snapshot())
                .into_iter()
                .filter(|d| d.code.as_deref() == Some("stub-declaration"))
                .count()
        };
        assert_eq!(stubs("file:///proj/reference-notes.gsql"), 1);
        let current = format!(
            "file:///cache/gsql-lsp/reference-{}.gsql",
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(stubs(&current), 0);
        assert_eq!(stubs("file:///cache/gsql-lsp/reference-0.0.9.gsql"), 0);
    }

    #[test]
    fn says_so_when_there_is_no_schema() {
        let query = "CREATE QUERY q() {\n  R = SELECT s FROM Person:s;\n  PRINT R;\n}\n";
        let mut fixture = Fixture::new(query);
        assert!(
            diagnostics(&fixture.snapshot()).is_empty(),
            "off in the test fixture"
        );
        fixture.config.update(
            &serde_json::json!({ "diagnostics": { "noSchemaNotice": true } }),
        );
        let found = diagnostics(&fixture.snapshot());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].code.as_deref(), Some("no-schema"));
        assert_eq!(found[0].severity, Some(severity::WARNING));
        assert_eq!(found[0].range.start.line, 0, "on the query name");
        // With a schema in the workspace, nothing to say.
        let mut with_schema = Fixture::with_schema(
            query,
            "CREATE VERTEX Person (PRIMARY_ID id STRING)\n",
        );
        with_schema.config.update(
            &serde_json::json!({ "diagnostics": { "noSchemaNotice": true } }),
        );
        assert!(diagnostics(&with_schema.snapshot()).is_empty());
        // A query that uses no schema types needs no schema.
        let mut plain =
            Fixture::new("CREATE QUERY q() { INT x = 1; PRINT x; }\n");
        plain.config.update(
            &serde_json::json!({ "diagnostics": { "noSchemaNotice": true } }),
        );
        assert!(diagnostics(&plain.snapshot()).is_empty());
    }

    #[test]
    fn a_missing_comma_between_items_is_named_and_leaves_no_cascade() {
        // `INT` is a valid keyword, so no keyword is "misspelled" here, and the
        // parameters are still known once the comma is assumed.
        let text = "CREATE QUERY q(STRING grain=\"\" INT n=-1) {\n  PRINT grain, n;\n}\n";
        assert_eq!(
            messages(text),
            ["Syntax error: missing `,` before `INT`"]
        );
        let text = "CREATE QUERY q(STRING a, STRING b DOUBLE c) {\n  PRINT a, b, c;\n}\n";
        assert_eq!(
            messages(text),
            ["Syntax error: missing `,` before `DOUBLE`"]
        );
    }

    #[test]
    fn a_missing_comma_goes_before_a_comment_that_ends_the_line() {
        for comment in ["# first", "// first", "/* first */"] {
            let text = format!(
                "CREATE QUERY q(INT a {comment}\n  INT b) {{\n  PRINT a, b;\n}}\n"
            );
            let repaired = apply_fix(&text, "Insert `,`");
            assert!(syntax_messages(&repaired).is_empty(), "{repaired}");
            let expected = text
                .replace(&format!("a {comment}"), &format!("a, {comment}"));
            assert_eq!(repaired, expected);
        }
    }

    #[test]
    fn a_missing_token_goes_before_a_comment_that_ends_the_line() {
        for comment in ["# end", "// end"] {
            for gap in [" ", "\n  "] {
                let text = format!(
                    "CREATE QUERY q() {{\n  PRINT 1;{gap}{comment}\n"
                );
                let repaired = apply_fix(&text, "Insert `}`");
                assert!(syntax_messages(&repaired).is_empty(), "{repaired}");
                assert_eq!(repaired, text.replace("1;", "1;}"));
            }
        }
    }

    #[test]
    fn a_missing_token_goes_before_an_unclosed_comment() {
        // Everything after an unclosed `/*` is comment, strings and `#` included.
        for rest in
            ["", " \"s\" # c", "\n}\nCREATE QUERY r() {\n  PRINT \"r\";"]
        {
            let text = format!(
                "CREATE QUERY q() {{\n  INT x = 1 /* todo{rest}\n}}\n"
            );
            assert_eq!(
                apply_fix(&text, "Insert `;`"),
                text.replace("1 /*", "1; /*")
            );
        }
        // In openCypher such a `/*` is plain text.
        let text = "CREATE OPENCYPHER QUERY c() FOR GRAPH g {\n  MATCH (u:P) RETURN u /* end";
        assert_eq!(apply_fix(text, "Insert `}`"), format!("{text}}}"));
    }

    #[test]
    fn a_comment_sign_in_a_string_over_lines_is_not_a_comment() {
        // The `#` or `//` starting the string's second line is part of the string.
        for sign in ["#", "//"] {
            let text = format!(
                "CREATE QUERY q() {{\n  S = SELECT s FROM Person:s WHERE s.name == \"a\n\
                 {sign}\" ORDER BY s.name, LIMIT 3;\n  PRINT S;\n}}\n"
            );
            assert_eq!(
                syntax_messages(&text),
                ["Syntax error: remove the trailing `,`"],
                "{text}"
            );
            let text = format!(
                "CREATE QUERY q(STRING a=\"x\n{sign}\" INT b) {{\n  PRINT a, b;\n}}\n"
            );
            assert_eq!(
                apply_fix(&text, "Insert `,`"),
                text.replace("\" INT", "\", INT")
            );
            let text =
                format!("CREATE QUERY q() {{\n  PRINT \"a\n{sign}b\";\n");
            assert_eq!(
                apply_fix(&text, "Insert `}`"),
                text.replace("b\";", "b\";}")
            );
        }
    }

    #[test]
    fn a_quote_in_an_opencypher_string_does_not_hide_later_comments() {
        let cypher = "CREATE OPENCYPHER QUERY c() FOR GRAPH g {\n  \
                      MATCH (u:P) WHERE u.name = 'O\"Reilly' RETURN u\n}\n";
        let text = format!(
            "{cypher}CREATE QUERY q(INT a # first\n  INT b) {{\n  PRINT a, b;\n}}\n"
        );
        assert_eq!(
            apply_fix(&text, "Insert `,`"),
            text.replace("a #", "a, #")
        );
        let text = format!(
            "{cypher}CREATE QUERY q() {{\n  S = SELECT s FROM Person:s \
             ORDER BY s.name, # c\n  LIMIT 3;\n  PRINT S;\n}}\n"
        );
        assert_eq!(
            syntax_messages(&text),
            ["Syntax error: remove the trailing `,`"]
        );
    }

    #[test]
    fn a_missing_brace_goes_before_an_opencypher_comment() {
        for filter in ["", " WHERE u.name = 'O\"Reilly'"] {
            for comment in ["// end", "/* end */"] {
                let text = format!(
                    "CREATE OPENCYPHER QUERY c() FOR GRAPH g {{\n  \
                     MATCH (u:P){filter} RETURN u {comment}"
                );
                let repaired = apply_fix(&text, "Insert `}`");
                assert!(syntax_messages(&repaired).is_empty(), "{repaired}");
                let expected = text.replace(
                    &format!(" {comment}"),
                    &format!("}} {comment}"),
                );
                assert_eq!(repaired, expected);
            }
        }
    }

    #[test]
    fn explains_an_extra_end() {
        let text = "CREATE QUERY q(INT k) {\n  IF k > 1 THEN PRINT 1; END;\n  END;\n  PRINT 2;\n}\n";
        let found = messages(text);
        assert_eq!(
            found,
            [
                "Syntax error: `END` closes nothing here: there is no open IF, CASE, WHILE, FOREACH or TRY"
            ]
        );
    }

    #[test]
    fn suggests_declared_accumulators_for_misspelled_ones() {
        let text = "CREATE QUERY q() {\n  SumAccum<INT> @@count_all;\n  SumAccum<INT> @score;\n  R = SELECT s FROM P:s ACCUM s.@scor += 1;\n  @@count_al += 1;\n  PRINT R, @@count_all;\n}\n";
        let found = messages(text);
        assert!(
            found.iter().any(|m| m.contains("`@scor`")
                && m.contains("did you mean `@score`?")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`@@count_al`")
                    && m.contains("did you mean `@@count_all`?")),
            "{found:?}"
        );
    }

    #[test]
    fn an_undeclared_accumulator_carries_nothing_but_its_fixes() {
        let data = |text: &str| {
            let fixture = Fixture::new(text);
            diagnostics(&fixture.snapshot())
                .into_iter()
                .find(|d| d.code.as_deref() == Some("undeclared-accumulator"))
                .expect("reported")
                .data
        };
        assert_eq!(data("CREATE QUERY q() {\n  @@total += 1;\n}\n"), None);
        let similar = data(
            "CREATE QUERY q() {\n  SumAccum<INT> @@totals;\n  @@total += 1;\n  PRINT @@totals;\n}\n",
        );
        let keys: Vec<String> = similar
            .and_then(|d| {
                d.as_object()
                    .map(|o| o.keys().cloned().collect())
            })
            .unwrap_or_default();
        assert_eq!(keys, ["fixes"]);
    }

    #[test]
    fn explains_arrows_and_unterminated_comments() {
        let text = "CREATE QUERY q() {\n  MapAccum<STRING, INT> @@m;\n  @@m += (\"a\" => 1);\n}\n";
        assert_eq!(
            messages(text),
            [
                "Syntax error: `=>` is not a GSQL operator: write key-value pairs as `key -> value`"
            ]
        );
        let text = "CREATE QUERY q() {\n  /* note\n  PRINT 1;\n}\n";
        let found = messages(text);
        assert_eq!(
            found[0],
            "Syntax error: unterminated comment: add the closing `*/`",
            "{found:?}"
        );
    }

    #[test]
    fn checks_parameters_passed_by_name() {
        let text = "CREATE QUERY greet(INT age = 3, STRING name = \"John\", DATETIME birthday) { PRINT age; }\nRUN QUERY greet({\"name\": \"Emma\", \"age\": 21})\nRUN QUERY greet({\"nmae\": \"Emma\"})\nRUN QUERY greet(1, 2, 3, 4)\n";
        assert_eq!(
            messages(text),
            [
                "Query `greet` has no parameter `nmae`",
                "Query `greet` expects 1 to 3 arguments, but 4 given"
            ]
        );
    }

    #[test]
    fn seed_sets_and_v3_patterns_may_name_variables() {
        let schema = [(
            SCHEMA_URI,
            "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\nCREATE UNDIRECTED EDGE Friend (FROM Person, TO Person)\n",
        )];
        let text = "CREATE QUERY q() {\n  S7 = ANY;\n  S10 = _;\n  workers = {Person.*};\n  r = SELECT v FROM (t:workers) -[e]- (v) WHERE t.age > 1;\n  PRINT S7, S10, r;\n}\n";
        let found = messages_with(text, &schema);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn post_accum_hint_needs_two_words() {
        let text = "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  R = SELECT s FROM P:s ACCUM @@n += 1 POST-ACCUM @@n +=;\n  PRINT R;\n}\n";
        let found = messages(text);
        assert!(
            !found.is_empty()
                && found
                    .iter()
                    .all(|m| !m.contains("as one word")),
            "{found:?}"
        );
    }

    #[test]
    fn control_flow_bodies_are_blocks() {
        // The same name in two branches or loops is two variables.
        let text = "CREATE QUERY q(BOOL b) {\n  IF b THEN\n    INT x = 1;\n    PRINT x;\n  ELSE\n    INT x = 2;\n    PRINT x;\n  END;\n  WHILE b LIMIT 2 DO\n    SumAccum<INT> @@n;\n    @@n += 1;\n    PRINT @@n;\n  END;\n  WHILE b LIMIT 2 DO\n    SumAccum<INT> @@n;\n    @@n += 2;\n    PRINT @@n;\n  END;\n}\n";
        assert!(messages(text).is_empty(), "{:?}", messages(text));
        // A block's variables end with it; vertex sets do not.
        let text = "CREATE QUERY q(BOOL b) {\n  IF b THEN\n    INT x = 1;\n    S = {Person.*};\n    PRINT x;\n  END;\n  PRINT x, S;\n}\n";
        assert_eq!(messages(text), ["`x` is not defined"]);
    }

    #[test]
    fn explains_common_syntax_mistakes() {
        let cases = [
            (
                "CREATE QUERY q() {\n  INT x = 1\n  PRINT x;\n}\n",
                "missing `;` after this statement",
            ),
            (
                "CREATE QUERY q(INT x) {\n  IF x > 1 THEN\n    PRINT x;\n}\n",
                "`IF` has no matching `END`",
            ),
            (
                "CREATE QUERY q() {\n  SumAccum<INT> @@a, @@b;\n  R = SELECT s FROM P:s\n      ACCUM @@a += 1\n            @@b += 1;\n  PRINT R;\n}\n",
                "missing `,` between ACCUM statements",
            ),
            (
                "CREATE QUERY q() {\n  SumAccum<INT> @x;\n  R = SELECT s FROM P:s ACCUM s.@x += 1 POST ACCUM s.@x = 2;\n  PRINT R;\n}\n",
                "`POST-ACCUM`",
            ),
            (
                "CREATE QUERY q() {\n  PRINT \"unclosed;\n}\n",
                "unterminated string",
            ),
            (
                "CREATE QUERY q() {\n  PRINT 1;\n",
                "missing `}` to close the query body",
            ),
            (
                "CREATE QUERY q(INT a, ) {\n  PRINT a;\n}\n",
                "remove the trailing `,`",
            ),
            (
                "CREATE QUERY q() {\n  SumAccum<INT> @@s;\n  R = SELECT p FROM P:p ACCUM @@s += 1,\n  POST-ACCUM @@s += 2;\n}\n",
                "remove the trailing `,`",
            ),
            (
                "CREATE QUERY q() {\n  SumAccum<INT> @@s;\n  R = SELECT p FROM P:p ACCUM @@s += 1, @@s += 2,;\n}\n",
                "remove the trailing `,`",
            ),
            (
                "CREATE QUERY q() {\n  PRINT 1, 2,;\n}\n",
                "remove the trailing `,`",
            ),
            (
                "CREATE QUERY q() {\n  SumAccum<INT> @@s, @@t,;\n}\n",
                "remove the trailing `,`",
            ),
            (
                "CREATE QUERY q() {\n  R = SELECT p FROM P:p ORDER BY p.a, LIMIT 3;\n  PRINT R;\n}\n",
                "remove the trailing `,`",
            ),
            (
                "CREATE QUERY q(INT a) {\n  IF a == 1 PRINT a; END;\n}\n",
                "missing `THEN` after the IF condition",
            ),
            (
                "CREATE QUERY q(INT a) {\n  WHILE a < 1 PRINT a; END;\n}\n",
                "missing `DO` before the loop body",
            ),
            (
                "CREATE QUERY q(INT a) {\n  FOREACH i IN RANGE[1,3]\n    PRINT i;\n  END;\n}\n",
                "missing `DO` before the loop body",
            ),
            (
                "CREATE QUERY q(INT x) {\n  IF x > 1 THEN PRINT 1; ELSEIF x > 0 THEN PRINT 2; END;\n}\n",
                "`ELSE IF`",
            ),
            (
                "CREATE QUERY q() {\n  R = SELECT s WHERE s.age > 1;\n  PRINT R;\n}\n",
                "SELECT needs a FROM clause",
            ),
            (
                "CREATE QUERY q() {\n  R = SELECT t FROM P:s -(Knows:e) P:t;\n  PRINT R;\n}\n",
                "malformed edge step",
            ),
            (
                "CREATE VERTEX P (PRIMARY_ID id STRING, name STRING = \"x\")\n",
                "`DEFAULT value`",
            ),
        ];
        for (text, expected) in cases {
            let found = messages(text);
            assert!(
                found.iter().any(|m| m.contains(expected)),
                "{expected:?} not in {found:?} for:\n{text}"
            );
            // Error recovery must not produce follow-up semantic errors.
            assert!(
                found
                    .iter()
                    .all(|m| m.starts_with("Syntax error")
                        || m.contains("never used")),
                "{found:?}"
            );
        }
    }

    #[test]
    fn a_comment_after_a_trailing_comma_does_not_hide_it() {
        for comment in ["# note", "// note", "/* note */"] {
            let text = format!(
                "CREATE QUERY q() {{\n  PRINT 1, 2, {comment}\n  ;\n}}\n"
            );
            assert_eq!(
                syntax_messages(&text),
                ["Syntax error: remove the trailing `,`"],
                "{text}"
            );
        }
    }

    #[test]
    fn select_vertex_names_a_vertex_type() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\n";
        let query = "CREATE QUERY q(STRING f) {\n  S = {SelectVertex(f, $0, Person, \",\", true)};\n  PRINT S;\n}\n";
        let found = messages_with(query, &[(SCHEMA_URI, schema)]);
        assert!(found.is_empty(), "{found:?}");
        let found = messages_with(
            &query.replace("Person", "Persn"),
            &[(SCHEMA_URI, schema)],
        );
        assert_eq!(
            found,
            vec!["Unknown vertex type `Persn` (did you mean `Person`?)"]
        );
    }

    #[test]
    fn result_aliases_are_names() {
        let found = messages(
            "CREATE QUERY q() SYNTAX V3 {\n  SELECT n, COUNT(f) AS cnt INTO T FROM (n)-[:Knows]-(f) HAVING cnt > 3 ORDER BY cnt DESC;\n  PRINT T;\n}\n",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn equality_operators_need_syntax_v3() {
        let query = "R = SELECT s FROM P:s WHERE s.age = 5 OR s.age <> 6;\n  PRINT R;\n}\n";
        let v2 = messages(&format!("CREATE QUERY q() {{\n  {query}"));
        assert!(
            v2.contains(
                &"`=` compares values only in SYNTAX V3 queries; use `==`"
                    .to_string()
            ),
            "{v2:?}"
        );
        assert!(
            v2.contains(
                &"`<>` compares values only in SYNTAX V3 queries; use `!=`"
                    .to_string()
            ),
            "{v2:?}"
        );
        let v3 =
            messages(&format!("CREATE QUERY q() SYNTAX V3 {{\n  {query}"));
        assert!(v3.is_empty(), "{v3:?}");
    }

    #[test]
    fn reports_missing_semicolon() {
        let found =
            messages("CREATE QUERY q() {\n  INT x = 1\n  PRINT x;\n}\n");
        assert!(
            found
                .iter()
                .any(|m| m.contains("Syntax error")),
            "{found:?}"
        );
    }

    #[test]
    fn clean_query_has_no_diagnostics() {
        let found = messages(
            "CREATE QUERY q(VERTEX<Person> p) {\n  SumAccum<INT> @@n;\n  S = {p};\n  R = SELECT t FROM S:s -(Knows:e)- :t ACCUM @@n += 1;\n  PRINT @@n, R;\n}\n",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn reports_undeclared_and_unused_accumulators() {
        let found = messages(
            "CREATE QUERY q() {\n  SumAccum<INT> @@unused;\n  @@total += 1;\n}\n",
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`@@total` is not declared")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`@@unused` is declared but never used")),
            "{found:?}"
        );
    }

    #[test]
    fn option_keys_are_not_names() {
        let found = messages(
            "CREATE QUERY q(LIST<FLOAT> v) {\n  MapAccum<VERTEX, FLOAT> @@dist;\n  R = vectorSearch({Doc.emb}, v, 5, {distance_map: @@dist});\n  PRINT R;\n}\n",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn reports_duplicates_and_undefined_names() {
        let found = messages(
            "CREATE QUERY q(INT k) {\n  INT k = 1;\n  PRINT kk;\n}\n",
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`k` is already declared")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`kk` is not defined")),
            "{found:?}"
        );
    }

    #[test]
    fn checks_types_and_attributes_against_the_workspace() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\nCREATE UNDIRECTED EDGE Knows (FROM Person, TO Person)\n";
        let query = "CREATE QUERY q() {\n  R = SELECT t FROM Person:s -(Knowz:e)- Persn:t WHERE s.agee > 1 AND s.age > 2;\n  PRINT R;\n}\n";
        let found = messages_with(query, &[(SCHEMA_URI, schema)]);
        assert!(
            found
                .iter()
                .any(|m| m.starts_with("Unknown edge type `Knowz`")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`Persn` is not a vertex set variable")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|m| m
                == "`Person` has no attribute `agee` (did you mean `age`?)"),
            "{found:?}"
        );
        // The valid attribute itself is not reported (only suggested).
        assert!(
            !found
                .iter()
                .any(|m| m.contains("no attribute `age`")),
            "{found:?}"
        );
    }

    #[test]
    fn resolves_typedefs_virtual_edges_and_pattern_properties() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\nCREATE DIRECTED EDGE Knows (FROM Person, TO Person)\n";
        let query = r#"CREATE QUERY q(FILE f, INT len) SYNTAX V3 {
  TYPEDEF TUPLE<STRING name, INT score> Rec;
  TYPEDEF HeapAccum<Rec>(3, score DESC) Top;
  Top @@top;
  BitwiseOrAccum<len> @@bits;
  SumAccum<INT> EDGE @weight;
  TUPLE<INT a, STRING b> pair = (1, "x");
  CREATE DIRECTED VIRTUAL EDGE Near (FROM Person, TO Person, dist DOUBLE);
  R = SELECT t FROM (s:Person {name: "x"})-[e:Knows]->(t)
      ACCUM e.@weight += 1, @@bits += 1, INSERT INTO Near VALUES (s, t, 1.0)
      POST-ACCUM (t) @@top += Rec(t.name, 1);
  S = SELECT t FROM Person:s -(Near>:n)- Person:t WHERE n.dist > 1.0 AND s.name = "x";
  PRINT @@top, R, S, pair, f;
}
"#;
        let found = messages_with(query, &[(SCHEMA_URI, schema)]);
        assert!(found.is_empty(), "{found:?}");
        let typo = query
            .replace("{name: ", "{nmae: ")
            .replace("n.dist", "n.dst");
        let found = messages_with(&typo, &[(SCHEMA_URI, schema)]);
        assert!(
            found
                .iter()
                .any(|m| m.starts_with("`Person` has no attribute `nmae`")),
            "{found:?}"
        );
    }

    #[test]
    fn optional_documented_arguments_are_accepted() {
        // `evaluate(expression)` returns a BOOL: the type argument is optional.
        let text = "CREATE QUERY n() FOR GRAPH G {\n  S = {Person.*};\n  R = SELECT s FROM S:s WHERE evaluate(\"s.age > 1\") AND evaluate(\"s.age\", \"int\") > 0;\n  PRINT R;\n}\n";
        assert!(
            messages(text)
                .iter()
                .all(|m| !m.contains("expects")),
            "{:?}",
            messages(text)
        );
    }

    #[test]
    fn checks_builtin_arity() {
        let text = "CREATE QUERY n() FOR GRAPH G {\n  ListAccum<INT> @@l;\n  SetAccum<INT> @@s;\n  STRING t = \"abc\";\n  PRINT abs(), abs(1, 2, 3), lower(), substr(t), upper(t, t), now(1), @@l.size(1), @@s.contains(), @@l.get();\n  PRINT abs(1), round(1.5), round(1.5, 1), substr(t, 1), substr(t, 1, 2), now(), @@l.size(), @@s.contains(1), coalesce(1, 2, 3), count(@@s), @@l.get(0);\n}\n";
        assert_eq!(
            messages(text),
            [
                "`abs` expects 1 argument, but 0 given",
                "`abs` expects 1 argument, but 3 given",
                "`lower` expects 1 argument, but 0 given",
                "`substr` expects 2 to 3 arguments, but 1 given",
                "`upper` expects 1 argument, but 2 given",
                "`now` expects 0 arguments, but 1 given",
                "`size` expects 0 arguments, but 1 given",
                "`contains` expects 1 argument, but 0 given",
                "`get` expects 1 argument, but 0 given",
            ]
        );
    }

    #[test]
    fn user_queries_replace_builtin_names_in_arity_checks() {
        let text = "CREATE QUERY abs(INT a, INT b) { PRINT a, b; }\nCREATE QUERY q() { PRINT abs(1, 2); }\n";
        assert!(
            messages(text)
                .iter()
                .all(|m| !m.starts_with("`abs`"))
        );
    }

    #[test]
    fn tuple_types_declared_in_a_query_replace_builtin_names_in_arity_checks()
    {
        let text = "CREATE QUERY q() {\n  TYPEDEF TUPLE <INT a, INT b> abs;\n  ListAccum<abs> @@l;\n  @@l += abs(1, 2);\n  PRINT @@l;\n}\n";
        let found = messages(text);
        assert!(found.iter().all(|m| !m.starts_with("`abs`")), "{found:?}");
    }

    #[test]
    fn loading_functions_are_checked_in_loading_jobs_only() {
        let text = "CREATE LOADING JOB j FOR GRAPH G {\n  LOAD f TO VERTEX V VALUES ($0, split($1, \",\"), split($1, \"=\", \";\"), gsql_substring($2));\n}\nCREATE QUERY q() { PRINT gsql_substring(\"a\"); LOG(true, 1, 2); }\n";
        let found = messages(text);
        assert!(
            found.contains(
                &"`gsql_substring` expects 2 to 3 arguments, but 1 given"
                    .to_string()
            ),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .all(|m| !m.starts_with("`split`")
                    && !m.starts_with("`log`")),
            "{found:?}"
        );
        assert_eq!(
            found
                .iter()
                .filter(|m| m.starts_with("`gsql_substring`"))
                .count(),
            1,
            "{found:?}"
        );
    }

    #[test]
    fn checks_query_arity() {
        let other = "CREATE QUERY helper(INT a, INT b = 2) { PRINT a, b; }\n";
        let found = messages_with(
            "RUN QUERY helper()\nRUN QUERY helper(1)\n",
            &[("file:///test/h.gsql", other)],
        );
        assert_eq!(
            found,
            vec!["Query `helper` expects 1 to 2 arguments, but 0 given"]
        );
    }

    #[test]
    fn a_tuple_type_shadows_a_query_of_the_same_name_in_arity_checks() {
        let tuple = "TYPEDEF TUPLE <INT a, STRING b> Pair;\n";
        let query =
            ("file:///test/p.gsql", "CREATE QUERY Pair() { PRINT 1; }\n");
        let call = |typedef: &str| {
            format!(
                "CREATE QUERY main() {{\n  {typedef}ListAccum<Pair> @@l;\n  @@l += Pair(1, \"x\");\n  PRINT @@l;\n}}\n"
            )
        };
        let counts = |text: &str, others: &[(&str, &str)]| -> Vec<String> {
            let found = messages_with(text, others);
            found
                .into_iter()
                .filter(|m| m.contains("expects"))
                .collect()
        };
        // Without a tuple type, the call is checked against the query.
        assert_eq!(
            counts(&call(""), &[query]),
            ["Query `Pair` expects 0 arguments, but 2 given"]
        );
        // The tuple type in this file, before or after the query.
        assert!(
            counts(&format!("{tuple}{}{}", query.1, call("")), &[])
                .is_empty()
        );
        assert!(
            counts(&format!("{}{tuple}{}", query.1, call("")), &[])
                .is_empty()
        );
        // In another file.
        let others = [("file:///test/t.gsql", tuple), query];
        assert!(counts(&call(""), &others).is_empty());
        // In the calling query.
        assert!(counts(&call(tuple), &[query]).is_empty());
    }

    /// The messages of `query`, with a schema of people and movies.
    fn movie_messages(query: &str) -> Vec<String> {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\nCREATE VERTEX Movie (PRIMARY_ID id STRING, title STRING)\nCREATE DIRECTED EDGE ACTED_IN (FROM Person, TO Movie)\nCREATE UNDIRECTED EDGE Knows (FROM Person, TO Person)\nCREATE DIRECTED EDGE Likes (FROM Person, TO Person | FROM Person, TO Movie)\nCREATE DIRECTED EDGE Any_E (FROM *, TO Movie)\n";
        messages_with(query, &[(SCHEMA_URI, schema)])
    }

    #[test]
    fn an_alias_reached_through_an_edge_has_the_edge_end_type() {
        let no_attr = |found: &[String], t: &str, a: &str| {
            found.iter().any(|m| {
                m.starts_with(&format!("`{t}` has no attribute `{a}`"))
            })
        };
        // V3, directed: out -> TO, in -> FROM.
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G SYNTAX V3 {\n  SELECT m INTO T FROM (p:Person)-[:ACTED_IN]->(m) WHERE m.zzz == 1;\n  PRINT T;\n}\n",
        );
        assert!(no_attr(&found, "Movie", "zzz"), "{found:?}");
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G SYNTAX V3 {\n  SELECT m INTO T FROM (p:Movie)<-[:ACTED_IN]-(m) WHERE m.zzz == 1;\n  PRINT T;\n}\n",
        );
        assert!(no_attr(&found, "Person", "zzz"), "{found:?}");
        // V2.
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G {\n  A = SELECT m FROM Person:p -(ACTED_IN>)- :m WHERE m.zzz == 1;\n  PRINT A;\n}\n",
        );
        assert!(no_attr(&found, "Movie", "zzz"), "{found:?}");
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G {\n  A = SELECT m FROM Movie:p -(<ACTED_IN)- :m WHERE m.zzz == 1;\n  PRINT A;\n}\n",
        );
        assert!(no_attr(&found, "Person", "zzz"), "{found:?}");
        // The valid attribute is accepted.
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G {\n  A = SELECT m FROM Person:p -(ACTED_IN>)- :m WHERE m.title == \"x\";\n  PRINT A;\n}\n",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_keyword_typo_does_not_lose_the_edge_end_type_of_an_alias() {
        let select = "A = SELECT m FROM Person:p -(ACTED_IN>)- :m WHERE m.zzz == 1;\n  PRINT A;";
        let typo = "IF x > 0 THN PRINT x; END;";
        for text in [
            format!(
                "CREATE QUERY q(INT x) FOR GRAPH G {{\n  {select}\n  {typo}\n}}\n"
            ),
            format!(
                "CREATE QUERY q() FOR GRAPH G {{\n  {select}\n}}\nCREATE QUERY r(INT x) FOR GRAPH G {{\n  {typo}\n}}\n"
            ),
        ] {
            let found = movie_messages(&text);
            assert!(
                found
                    .iter()
                    .any(|m| m.contains("instead of `THN`")),
                "{found:?}"
            );
            assert!(
                found
                    .iter()
                    .any(|m| m.starts_with("`Movie` has no attribute `zzz`")),
                "{found:?}"
            );
        }
    }

    #[test]
    fn an_alias_behind_an_alternation_has_the_union_of_the_end_types() {
        let q = |from: &str, cond: &str, syntax: &str| {
            movie_messages(&format!(
                "CREATE QUERY q() FOR GRAPH G {syntax}{{\n  A = SELECT m FROM {from} WHERE {cond};\n  PRINT A;\n}}\n"
            ))
        };
        for (from, syntax) in [
            ("Person:p -(ACTED_IN>|Knows)- :m", ""),
            ("(p:Person)-[:ACTED_IN|Knows]->(m)", "SYNTAX V3 "),
        ] {
            // Movie from ACTED_IN, Person from Knows.
            let found =
                q(from, "m.title == \"x\" AND m.name == \"y\"", syntax);
            assert!(found.is_empty(), "{from}: {found:?}");
            let found = q(from, "m.zzz == 1", syntax);
            assert!(
                found.iter().any(|m| m.starts_with(
                    "None of Movie, Person has an attribute `zzz`"
                )),
                "{from}: {found:?}"
            );
        }
    }

    #[test]
    fn an_alias_stays_untyped_when_the_edge_does_not_decide() {
        for from in [
            // Wildcard end, unknown edge, wildcard edge, repetition, sequence.
            "Person:p -(Any_E>)- :m",
            "Person:p -(Nope>)- :m",
            "Person:p -(_>)- :m",
            "Person:p -(ACTED_IN>*1..2)- :m",
            "Person:p -(ACTED_IN>.Knows)- :m",
        ] {
            let query = format!(
                "CREATE QUERY q() FOR GRAPH G {{\n  A = SELECT m FROM {from} WHERE m.zzz == 1;\n  PRINT A;\n}}\n"
            );
            let found = movie_messages(&query);
            assert!(
                !found
                    .iter()
                    .any(|m| m.contains("has no attribute")),
                "{from}: {found:?}"
            );
        }
        // The union of both ends: an attribute of either type is fine.
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G {\n  A = SELECT m FROM Person:p -(Likes>)- :m WHERE m.title == \"x\" AND m.name == \"y\";\n  PRINT A;\n}\n",
        );
        assert!(found.is_empty(), "{found:?}");
        // An explicit type wins over the edge's.
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G SYNTAX V3 {\n  SELECT m INTO T FROM (p:Person)-[:Knows]-(m:Movie) WHERE m.title == \"x\";\n  PRINT T;\n}\n",
        );
        assert!(found.is_empty(), "{found:?}");
        // V3 repetition and wildcard edges.
        let found = movie_messages(
            "CREATE QUERY q() FOR GRAPH G SYNTAX V3 {\n  SELECT m INTO T FROM (p:Person)-[:ACTED_IN*1..2]->(m) WHERE m.zzz == 1;\n  PRINT T;\n}\n",
        );
        assert!(
            !found
                .iter()
                .any(|m| m.contains("has no attribute")),
            "{found:?}"
        );
        // Without a schema nothing changes.
        let found = messages(
            "CREATE QUERY q() FOR GRAPH G {\n  A = SELECT m FROM Person:p -(ACTED_IN>)- :m WHERE m.zzz == 1;\n  PRINT A;\n}\n",
        );
        assert!(
            !found
                .iter()
                .any(|m| m.contains("has no attribute")),
            "{found:?}"
        );
    }

    #[test]
    fn vector_attributes_declared_by_alter_are_known() {
        let schema = "CREATE VERTEX Account (PRIMARY_ID id STRING, name STRING)\nCREATE GRAPH G (Account)\n\
            CREATE SCHEMA_CHANGE JOB sc FOR GRAPH G {\n  ALTER VERTEX Account ADD VECTOR ATTRIBUTE emb1(DIMENSION=3, METRIC=\"L2\");\n}\n";
        let rest = "CREATE LOADING JOB lv FOR GRAPH G {\n  DEFINE FILENAME f;\n\
            \x20 LOAD f TO VECTOR ATTRIBUTE emb1 ON VERTEX Account VALUES ($0, SPLIT($1, \",\")) USING SEPARATOR=\"|\";\n\
            \x20 LOAD f TO VECTOR ATTRIBUTE emb9 ON VERTEX Account VALUES ($0, SPLIT($1, \",\")) USING SEPARATOR=\"|\";\n\
            \x20 LOAD f TO VERTEX Account VALUES ($0, $1);\n}\n\
            CREATE OR REPLACE QUERY q1 (LIST<FLOAT> qv) FOR GRAPH G SYNTAX v3 {\n\
            \x20 v = vectorSearch({Account.emb1}, qv, 5);\n  w = vectorSearch({Account.emb7}, qv, 5);\n  PRINT v, w;\n}\n";
        let emb9 = "`Account` has no attribute `emb9` (did you mean `emb1`?)";
        let emb7 = "`Account` has no attribute `emb7` (did you mean `emb1`?)";
        // In one file, and with the ALTER in another file of the workspace (where the
        // type of `Account` in `{Account.emb7}` is not known, so that is not judged).
        for (found, expected) in [
            (findings(&format!("{schema}{rest}"), &[]), vec![emb9, emb7]),
            (findings(rest, &[(SCHEMA_URI, schema)]), vec![emb9]),
        ] {
            assert_eq!(
                with_code(&found, "unknown-attribute"),
                expected,
                "{found:?}"
            );
            // A vector attribute is no column of the plain vertex.
            assert!(with_code(&found, "value-count").is_empty(), "{found:?}");
        }
    }

    #[test]
    fn primary_id_is_no_attribute_unless_declared_so() {
        let schema = "CREATE VERTEX A (PRIMARY_ID serial_num STRING, name STRING)\n\
            CREATE VERTEX B (PRIMARY_ID serial_num STRING, name STRING) WITH primary_id_as_attribute=\"true\"\n\
            CREATE VERTEX C (serial_num STRING PRIMARY KEY, name STRING)\n\
            CREATE VERTEX D (serial_num STRING, name STRING, PRIMARY KEY (serial_num, name))\n\
            CREATE VERTEX E (PRIMARY_ID serial_num STRING) WITH STATS=\"none\", PRIMARY_ID_AS_ATTRIBUTE=TRUE\n\
            CREATE DIRECTED EDGE AB (FROM A, TO B)\nCREATE GRAPH G (*)\n";
        let query = |body: &str| {
            format!("CREATE QUERY q() FOR GRAPH G {{\n{body}\n}}\n")
        };
        let message = "`serial_num` is the PRIMARY_ID of `A`, which is not an attribute unless the vertex type is declared \
            WITH primary_id_as_attribute=\"true\" (use `to_vertex` to look a vertex up by its id)";
        // In another file and in the file of the schema.
        let read = query(
            "S = {A.*};\nR = SELECT v FROM S:v WHERE v.serial_num == \"x\";\nPRINT R;",
        );
        let found = findings(&read, &[(SCHEMA_URI, schema)]);
        assert_eq!(
            with_code(&found, "unknown-attribute"),
            [message],
            "{found:?}"
        );
        let found = findings(&format!("{schema}{read}"), &[]);
        assert_eq!(
            with_code(&found, "unknown-attribute"),
            [message],
            "{found:?}"
        );
        // Reads in ACCUM and PRINT, through a typed alias.
        let more = query(
            "S = {A.*};\nR = SELECT v FROM A:v ACCUM STRING s = v.serial_num;\nPRINT R[R.serial_num];",
        );
        let found = findings(&more, &[(SCHEMA_URI, schema)]);
        assert_eq!(
            with_code(&found, "unknown-attribute").len(),
            2,
            "{found:?}"
        );
        // Silent: the option, PRIMARY KEY forms, the other attributes, a name that is no attribute of B.
        for ok in [
            "S = {B.*};\nR = SELECT v FROM S:v WHERE v.serial_num == \"x\";\nPRINT R;",
            "S = {E.*};\nR = SELECT v FROM S:v WHERE v.serial_num == \"x\";\nPRINT R;",
            "S = {C.*};\nR = SELECT v FROM S:v WHERE v.serial_num == \"x\";\nPRINT R;",
            "S = {D.*};\nR = SELECT v FROM S:v WHERE v.serial_num == \"x\";\nPRINT R;",
            "S = {A.*};\nR = SELECT v FROM S:v WHERE v.name == \"x\" AND v.type == \"A\";\nPRINT R;",
            "S = {A.*};\nPRINT to_vertex(\"x\", \"A\");\nPRINT S;",
            // Not judged when one of the possible types has the attribute.
            "S = {A.*, B.*};\nR = SELECT v FROM S:v WHERE v.serial_num == \"x\";\nPRINT R;",
        ] {
            let found = findings(&query(ok), &[(SCHEMA_URI, schema)]);
            assert!(
                with_code(&found, "unknown-attribute").is_empty(),
                "{ok}: {found:?}"
            );
        }
        // Loading and inserting name the id by position, not as an attribute.
        let load = "CREATE LOADING JOB j FOR GRAPH G {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX A VALUES ($0, $1);\n}\n\
            CREATE QUERY ins() FOR GRAPH G {\n  INSERT INTO A (PRIMARY_ID, name) VALUES (\"k\", \"n\");\n}\n";
        let found = findings(load, &[(SCHEMA_URI, schema)]);
        assert!(
            with_code(&found, "unknown-attribute").is_empty(),
            "{found:?}"
        );
    }

    #[test]
    fn checks_graph_names() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\nCREATE GRAPH Social (Person)\n";
        let text = "CREATE QUERY q() FOR GRAPH Socail { PRINT 1; }\n\
            CREATE LOADING JOB j FOR GRAPH Nope {\n  DEFINE FILENAME f;\n}\n\
            CREATE SCHEMA_CHANGE JOB s FOR GRAPH Nope2 {\n  ADD VERTEX V (PRIMARY_ID id STRING);\n}\n\
            USE GRAPH Gone\nCREATE QUERY ok() FOR GRAPH Social { PRINT 1; }\nUSE GRAPH Social\nUSE GLOBAL\n";
        let found = findings(text, &[(SCHEMA_URI, schema)]);
        assert_eq!(
            with_code(&found, "unknown-graph"),
            [
                "Unknown graph `Socail` (did you mean `Social`?)",
                "Unknown graph `Nope`",
                "Unknown graph `Nope2`",
                "Unknown graph `Gone`"
            ],
            "{found:?}"
        );
        // Silent: no graph is created anywhere, or no schema at all.
        let vertices = "CREATE VERTEX Person (PRIMARY_ID id STRING)\n";
        let found = findings(text, &[(SCHEMA_URI, vertices)]);
        assert!(with_code(&found, "unknown-graph").is_empty(), "{found:?}");
        let found = findings(text, &[]);
        assert!(with_code(&found, "unknown-graph").is_empty(), "{found:?}");
    }

    #[test]
    fn checks_types_named_by_schema_edits() {
        let text = "CREATE VERTEX Person (PRIMARY_ID id STRING)\nCREATE GRAPH G (Person, Nope)\nCREATE GRAPH Everything (*)\n\
            CREATE SCHEMA_CHANGE JOB s FOR GRAPH G {\n  ALTER VERTEX Ghost DROP ATTRIBUTE (x);\n\
            \x20 ADD DIRECTED EDGE X (FROM Person, TO Ghost2);\n  ADD VERTEX Fresh (PRIMARY_ID id STRING);\n\
            \x20 ADD DIRECTED EDGE Y (FROM Fresh, TO Person);\n  ALTER EDGE Y ADD PAIR (FROM Person, TO Fresh);\n\
            \x20 ALTER VERTEX Fresh ADD ATTRIBUTE (a INT);\n  DROP VERTEX Gone;\n  ALTER GRAPH G DROP VERTEX Gone2;\n\
            \x20 ALTER GRAPH G ADD VERTEX Fresh;\n  ALTER GRAPH G ADD VERTEX Later;\n}\nDROP EDGE Old;\n";
        let found = findings(text, &[]);
        assert_eq!(
            with_code(&found, "unknown-type"),
            [
                "Unknown vertex or edge type `Nope`",
                "Unknown vertex type `Ghost`",
                "Unknown vertex type `Ghost2`",
                "Unknown vertex or edge type `Later`",
            ],
            "{found:?}"
        );
        // Another job may add the type.
        let other = "CREATE SCHEMA_CHANGE JOB more FOR GRAPH G {\n  ADD VERTEX Later (PRIMARY_ID id STRING);\n}\n";
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\n";
        let job = "CREATE SCHEMA_CHANGE JOB j FOR GRAPH G {\n  ALTER VERTEX Later DROP ATTRIBUTE (a);\n}\n";
        let found = findings(
            job,
            &[("file:///test/other.gsql", other), (SCHEMA_URI, schema)],
        );
        assert!(with_code(&found, "unknown-type").is_empty(), "{found:?}");
        // Without a schema nothing is known to be missing.
        let found = findings(job, &[]);
        assert!(with_code(&found, "unknown-type").is_empty(), "{found:?}");
    }

    #[test]
    fn reports_duplicate_definitions() {
        let text = "CREATE VERTEX Person(PRIMARY_ID id STRING)\nCREATE VERTEX Person(PRIMARY_ID id STRING)\n\
            CREATE GRAPH G(Person)\nCREATE GRAPH H(Person)\nCREATE QUERY a() FOR GRAPH G { PRINT 1; }\n\
            CREATE QUERY a() FOR GRAPH G { PRINT 2; }\nCREATE QUERY a() FOR GRAPH H { PRINT 3; }\n\
            CREATE OR REPLACE QUERY a() FOR GRAPH G { PRINT 4; }\nCREATE QUERY b() { PRINT 1; }\nCREATE QUERY b() { PRINT 2; }\n";
        let fixture = Fixture::new(text);
        let all = diagnostics(&fixture.snapshot());
        let duplicates: Vec<_> = all
            .iter()
            .filter(|d| d.code.as_deref() == Some("duplicate-definition"))
            .collect();
        let lines: Vec<u32> = duplicates
            .iter()
            .map(|d| d.range.start.line)
            .collect();
        // The second Person and the second `a` of graph G; `b` has no graph, so it is not judged.
        assert_eq!(lines, [1, 5], "{duplicates:?}");
        assert_eq!(duplicates[0].severity, Some(severity::WARNING));
        assert_eq!(
            duplicates[1].related_information[0]
                .location
                .range
                .start
                .line,
            4
        );
        assert!(
            duplicates[1]
                .message
                .contains("already defined at line 5"),
            "{duplicates:?}"
        );
    }

    #[test]
    fn redefinitions_after_drop_or_add_are_fine() {
        let text = "CREATE VERTEX Person(PRIMARY_ID id STRING)\nCREATE GRAPH G(Person)\n\
            CREATE QUERY a() FOR GRAPH G { PRINT 1; }\nDROP QUERY a\nCREATE QUERY a() FOR GRAPH G { PRINT 2; }\n\
            DROP VERTEX Person\nCREATE VERTEX Person(PRIMARY_ID id STRING, n INT)\n\
            CREATE SCHEMA_CHANGE JOB s FOR GRAPH G {\n  ADD VERTEX Person(PRIMARY_ID id STRING);\n}\n";
        assert!(
            with_code(&findings(text, &[]), "duplicate-definition")
                .is_empty()
        );
        // A DROP in another file counts too.
        let text = "CREATE VERTEX P(PRIMARY_ID id STRING)\nCREATE VERTEX P(PRIMARY_ID id STRING)\n";
        let found =
            findings(text, &[("file:///test/drop.gsql", "DROP VERTEX P\n")]);
        assert!(
            with_code(&found, "duplicate-definition").is_empty(),
            "{found:?}"
        );
    }

    #[test]
    fn duplicate_definitions_across_files_are_hints_on_the_later_file() {
        let schema = "CREATE VERTEX Person(PRIMARY_ID id STRING)\n";
        // main.gsql sorts after a.gsql: reported here, not there.
        let fixture =
            Fixture::with_files(schema, &[("file:///test/a.gsql", schema)]);
        let all = diagnostics(&fixture.snapshot());
        let duplicate = all
            .iter()
            .find(|d| d.code.as_deref() == Some("duplicate-definition"))
            .expect("reported");
        assert_eq!(duplicate.severity, Some(severity::HINT));
        assert_eq!(
            duplicate.related_information[0].location.uri,
            "file:///test/a.gsql"
        );
        assert!(
            duplicate
                .message
                .contains("already defined at a.gsql:1"),
            "{duplicate:?}"
        );
        let fixture =
            Fixture::with_files(schema, &[("file:///test/z.gsql", schema)]);
        let all = diagnostics(&fixture.snapshot());
        assert!(
            all.iter()
                .all(|d| d.code.as_deref() != Some("duplicate-definition")),
            "{all:?}"
        );
    }

    #[test]
    fn a_duplicate_definition_names_the_other_file_as_it_is_spelled() {
        let schema = "CREATE VERTEX Person(PRIMARY_ID id STRING)\n";
        let found =
            findings(schema, &[("file:///test/a%20schema.gsql", schema)]);
        let duplicate = with_code(&found, "duplicate-definition");
        assert!(
            duplicate
                .iter()
                .any(|m| m.contains("already defined at a schema.gsql:1")),
            "{duplicate:?}"
        );
    }

    #[test]
    fn duplicate_definitions_have_their_own_switch() {
        let schema = "CREATE VERTEX Person(PRIMARY_ID id STRING)\n";
        let same_file = format!("{schema}{schema}");
        let mut fixture = Fixture::with_files(
            &same_file,
            &[("file:///test/a.gsql", schema)],
        );
        let count = |fixture: &Fixture| {
            diagnostics(&fixture.snapshot())
                .iter()
                .filter(|d| d.code.as_deref() == Some("duplicate-definition"))
                .count()
        };
        assert_eq!(count(&fixture), 2);
        fixture.config.update(
            &serde_json::json!({ "diagnostics": { "duplicateDefinitions": false } }),
        );
        assert_eq!(count(&fixture), 0);
    }
}
