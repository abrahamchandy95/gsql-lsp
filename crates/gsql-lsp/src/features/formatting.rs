//! A conservative formatter: re-indents lines by block structure, keeps the
//! relative indentation of continuation lines (so hand-aligned SELECT
//! clauses survive), trims trailing whitespace and optionally normalizes
//! keyword case. Long or multi-line query parameter lists and TUPLE field
//! lists are laid out one item per line, as the GSQL Style Guide asks of
//! lines over 80 characters. Files with syntax errors are left alone.

use tree_sitter::Node;

use crate::features::{KeywordCase, Snapshot};
use crate::lsp::types::{FormattingOptions, Range, TextEdit};
use crate::syntax;
use crate::text::Span;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Rule {
    /// Indent to an absolute level.
    Level(usize),
    /// Keep the indentation relative to an anchor line; a line that is not
    /// indented past the anchor gets one level, unless `aligned` allows it
    /// to line up with the anchor (SELECT clauses under SELECT).
    Continuation { anchor: usize, aligned: bool },
    /// Keep exactly the indentation relative to an anchor line (the inside
    /// of a block comment).
    Relative { anchor: usize },
    /// Indent `depth` levels past the anchor line, whatever the line had (the
    /// END of an IF inside ACCUM lines up with the IF).
    Under { anchor: usize, depth: usize },
    /// Indent like another, later line (a comment above a SELECT clause
    /// lines up with the clause).
    SameAs { row: usize },
    /// Leave the line untouched (inside a multi-line string).
    Keep,
}

/// Clauses of a SELECT block, which may line up with the SELECT keyword.
const SELECT_CLAUSES: &[&str] = &[
    "into_clause",
    "from_clause",
    "sample_clause",
    "where_clause",
    "accum_clause",
    "per_clause",
    "post_accum_clause",
    "group_by_clause",
    "having_clause",
    "order_by_clause",
    "limit_clause",
];

struct Planner<'a> {
    source: &'a str,
    lines: &'a crate::text::LineIndex,
    rules: Vec<Option<Rule>>,
}

impl Planner<'_> {
    fn starts_line(&self, node: Node) -> bool {
        let row = node.start_position().row;
        let line_start = self.lines.line_start(row);
        self.source[line_start..node.start_byte()]
            .trim()
            .is_empty()
    }

    fn set(&mut self, row: usize, rule: Rule) {
        if let Some(slot) = self.rules.get_mut(row) {
            *slot = Some(rule);
        }
    }

    fn set_if_starts_line(&mut self, node: Node, level: usize) {
        if self.starts_line(node) {
            self.set(node.start_position().row, Rule::Level(level));
        }
    }

    /// Lines after the first line of `node` (up to `end_row`) keep their
    /// indentation relative to `anchor`, unless a nested rule assigns them.
    fn continuation(
        &mut self,
        anchor: usize,
        from_row: usize,
        end_row: usize,
    ) {
        for row in from_row..=end_row {
            if let Some(slot) = self.rules.get_mut(row)
                && slot.is_none()
            {
                *slot = Some(Rule::Continuation {
                    anchor,
                    aligned: false,
                });
            }
        }
    }

    /// Lets the clauses of a SELECT block in `node` line up with the
    /// statement's first line.
    fn align_select_clauses(&mut self, node: Node) {
        let select = match node.kind() {
            "select_statement" => Some(node),
            "assignment_statement" | "vertex_set_declaration" => node
                .child_by_field_name("right")
                .or_else(|| node.child_by_field_name("value"))
                .filter(|v| v.kind() == "select_statement"),
            _ => None,
        };
        let Some(select) = select else {
            return;
        };
        for clause in syntax::named_children(select) {
            if SELECT_CLAUSES.contains(&clause.kind())
                && self.starts_line(clause)
            {
                let row = clause.start_position().row;
                if let Some(Some(Rule::Continuation { aligned, .. })) =
                    self.rules.get_mut(row)
                {
                    *aligned = true;
                }
                self.align_comments_above(clause);
            }
        }
    }

    /// Comment-only lines directly above a clause take the clause's indent.
    fn align_comments_above(&mut self, clause: Node) {
        let target = clause.start_position().row;
        let mut previous = clause.prev_sibling();
        while let Some(comment) = previous.filter(|c| c.kind() == "comment") {
            let row = comment.start_position().row;
            if !self.starts_line(comment) || row != comment.end_position().row
            {
                break;
            }
            if let Some(slot) = self.rules.get_mut(row)
                && matches!(slot, Some(Rule::Continuation { .. }))
            {
                *slot = Some(Rule::SameAs { row: target });
            }
            previous = comment.prev_sibling();
        }
    }

    fn container(&mut self, node: Node, level: usize) {
        for child in syntax::children(node) {
            match child.kind() {
                "{" | ";" | "," => {}
                "}" => {
                    self.set_if_starts_line(child, level.saturating_sub(1))
                }
                _ => self.statement(child, level),
            }
        }
    }

    fn statement(&mut self, node: Node, level: usize) {
        let first = node.start_position().row;
        let end = node.end_position().row;
        self.set_if_starts_line(node, level);
        let anchor = first;
        match node.kind() {
            "query_definition"
            | "interpret_query_statement"
            | "loading_job_definition"
            | "schema_change_job_definition" => match node
                .child_by_field_name("body")
            {
                Some(body) if body.kind() != "opencypher_body" => {
                    let brace_row = body.start_position().row;
                    // A `{` on its own line lines up with the statement, like its `}`.
                    if brace_row > first
                        && self.starts_line(body)
                        && self.source[body.start_byte()..].starts_with('{')
                    {
                        self.set(brace_row, Rule::Level(level));
                        self.continuation(anchor, first + 1, brace_row - 1);
                    } else {
                        self.continuation(anchor, first + 1, brace_row);
                    }
                    self.container(body, level + 1);
                }
                _ => self.continuation(anchor, first + 1, end),
            },
            "if_statement" | "while_statement" | "foreach_statement" => {
                for child in syntax::children(node) {
                    match child.kind() {
                        "block" => self.block(child, level + 1),
                        "else_if_clause" | "else_clause" => {
                            self.set_if_starts_line(child, level);
                            for part in syntax::children(child) {
                                if part.kind() == "block" {
                                    self.block(part, level + 1);
                                }
                            }
                            self.continuation(
                                child.start_position().row,
                                child.start_position().row + 1,
                                child.end_position().row,
                            );
                        }
                        "END" => self.set_if_starts_line(child, level),
                        _ => {}
                    }
                }
                self.continuation(anchor, first + 1, end);
            }
            "case_statement" | "try_statement" => {
                for child in syntax::children(node) {
                    match child.kind() {
                        "block" => self.block(child, level + 1),
                        "when_clause" | "exception_handler"
                        | "else_clause" => {
                            self.set_if_starts_line(child, level + 1);
                            for part in syntax::children(child) {
                                if part.kind() == "block" {
                                    self.block(part, level + 2);
                                }
                            }
                            self.continuation(
                                child.start_position().row,
                                child.start_position().row + 1,
                                child.end_position().row,
                            );
                        }
                        "EXCEPTION" | "END" => {
                            self.set_if_starts_line(child, level)
                        }
                        _ => {}
                    }
                }
                self.continuation(anchor, first + 1, end);
            }
            _ => {
                self.continuation(anchor, first + 1, end);
                self.align_select_clauses(node);
                self.nested_control_flow(node);
            }
        }
    }

    /// Control flow among the comma-separated statements of ACCUM, POST-ACCUM
    /// and UPDATE ... SET is laid out like its statement-level kind: a
    /// branch's statements one level in from its IF (or WHEN), END and ELSE
    /// lined up with the IF.
    fn nested_control_flow(&mut self, node: Node) {
        // Multi-line statements of the lists continue from their own first
        // line. (They are found from their list: asking a node for its parent
        // makes tree-sitter search down from the root.)
        let mut listed = std::collections::HashSet::new();
        let mut constructs = Vec::new();
        syntax::walk(node, |n| {
            let multiline = n.start_position().row < n.end_position().row;
            if n != node
                && multiline
                && (listed.contains(&n.id()) || is_control_flow(n))
            {
                constructs.push(n);
            }
            if matches!(
                n.kind(),
                "accum_clause" | "post_accum_clause" | "block"
            ) {
                listed.extend(
                    syntax::code_children(n)
                        .into_iter()
                        .map(|c| c.id()),
                );
            }
        });
        // Outer nodes come first, so inner ones override their lines.
        for construct in constructs {
            let row = construct.start_position().row;
            for line in row + 1..=construct.end_position().row {
                self.set(
                    line,
                    Rule::Continuation {
                        anchor: row,
                        aligned: false,
                    },
                );
            }
            if !is_control_flow(construct) {
                continue;
            }
            for child in syntax::children(construct) {
                let (child_row, child_end) =
                    (child.start_position().row, child.end_position().row);
                match child.kind() {
                    "END" if self.starts_line(child) => self.set(
                        child_row,
                        Rule::Under {
                            anchor: row,
                            depth: 0,
                        },
                    ),
                    "else_if_clause" | "else_clause"
                        if construct.kind() == "if_statement" =>
                    {
                        if self.starts_line(child) {
                            self.set(
                                child_row,
                                Rule::Under {
                                    anchor: row,
                                    depth: 0,
                                },
                            );
                        }
                    }
                    "when_clause" | "else_clause" => {
                        if self.starts_line(child) {
                            self.set(
                                child_row,
                                Rule::Under {
                                    anchor: row,
                                    depth: 1,
                                },
                            );
                        }
                        for line in child_row + 1..=child_end {
                            self.set(
                                line,
                                Rule::Continuation {
                                    anchor: child_row,
                                    aligned: false,
                                },
                            );
                        }
                        if !self.starts_line(child)
                            && self.starts_line(construct)
                        {
                            // `CASE WHEN c THEN` on one line: its body is
                            // as deep as the bodies of the branches below
                            // (not after `ACCUM CASE`, whose layout is the user's).
                            for body in syntax::children(child)
                                .into_iter()
                                .filter(|b| b.kind() == "block")
                            {
                                for stmt in syntax::named_children(body) {
                                    if stmt.start_position().row > child_row
                                        && self.starts_line(stmt)
                                    {
                                        self.set(
                                            stmt.start_position().row,
                                            Rule::Under {
                                                anchor: row,
                                                depth: 2,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn block(&mut self, node: Node, level: usize) {
        for child in syntax::children(node) {
            if child.kind() != ";" {
                self.statement(child, level);
            }
        }
    }
}

/// For the raw text of an openCypher body starting on row `first`: the rows
/// that begin inside a string literal, a backtick-quoted name or a block
/// comment (their text is data and is kept as written), and the rows that
/// end inside a string (their trailing whitespace is data too).
fn cypher_protected_rows(
    text: &str,
    first: usize,
) -> (Vec<usize>, Vec<usize>) {
    #[derive(PartialEq, Clone, Copy)]
    enum State {
        Code,
        Quote(char),
        LineComment,
        BlockComment,
    }
    let (mut kept, mut open) = (Vec::new(), Vec::new());
    let mut state = State::Code;
    let mut row = first;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (state, c) {
            (_, '\n') => {
                match state {
                    State::Quote(q) => {
                        kept.push(row + 1);
                        if q != '`' {
                            open.push(row);
                        }
                    }
                    State::BlockComment => kept.push(row + 1),
                    State::LineComment => state = State::Code,
                    State::Code => {}
                }
                row += 1;
            }
            (State::Code, '"' | '\'' | '`') => state = State::Quote(c),
            (State::Code, '/') if chars.peek() == Some(&'/') => {
                chars.next();
                state = State::LineComment;
            }
            (State::Code, '/') if chars.peek() == Some(&'*') => {
                chars.next();
                state = State::BlockComment;
            }
            (State::Quote(_), '\\') => {
                // An escaped newline still continues the literal.
                if chars.peek() != Some(&'\n') {
                    chars.next();
                }
            }
            (State::Quote(q), _) if c == q => state = State::Code,
            (State::BlockComment, '*') if chars.peek() == Some(&'/') => {
                chars.next();
                state = State::Code;
            }
            _ => {}
        }
    }
    (kept, open)
}

/// Lines longer than this are split (the GSQL Style Guide).
const MAX_LINE: usize = 80;

/// A query's parameter list or a TUPLE's field list, which is laid out one
/// item per line when it spans lines or its line is too long.
struct List {
    /// The statement's first row and its parent, to chain sibling lists.
    statement_row: usize,
    parent: Option<usize>,
    open_row: usize,
    close_row: usize,
    /// The byte after `(` or `<`, and the byte of `)` or `>`.
    open_end: usize,
    close_start: usize,
    /// Byte spans of the items.
    items: Vec<(usize, usize)>,
    /// Byte spans of the string literals inside, whose spaces are data.
    strings: Vec<(usize, usize)>,
}

/// The lists that may be laid out, in order. A list's statement starts its
/// line, or follows a sibling's list that closes on that line (`> T; TYPEDEF
/// TUPLE<`), so the items always sit one level past the line they open on.
/// Lists with comments or multi-line strings are left to the line pass, and so
/// is a chain of lists when such a list opens on its last line, which would
/// otherwise end up half laid out.
fn breakable_lists(root: Node, source: &str) -> Vec<List> {
    let mut lists: Vec<List> = Vec::new();
    syntax::walk(root, |node| {
        let (list, open, close, item_kind) = match node.kind() {
            "typedef_statement" => (node, "<", ">", "tuple_field"),
            _ => match node
                .child_by_field_name("parameters")
                .filter(|p| p.kind() == "parameter_list")
            {
                Some(parameters) => (parameters, "(", ")", "parameter"),
                None => return,
            },
        };
        let tokens = syntax::children(list);
        let (Some(open), Some(close)) = (
            tokens.iter().find(|t| t.kind() == open),
            tokens
                .iter()
                .rev()
                .find(|t| t.kind() == close),
        ) else {
            return;
        };
        let items: Vec<(usize, usize)> = tokens
            .iter()
            .filter(|t| t.kind() == item_kind)
            .map(|t| (t.start_byte(), t.end_byte()))
            .collect();
        let mut strings = Vec::new();
        let mut safe = !items.is_empty();
        syntax::walk(list, |n| match n.kind() {
            "comment" => safe = false,
            "string" if n.start_position().row != n.end_position().row => {
                safe = false
            }
            "string" => strings.push((n.start_byte(), n.end_byte())),
            _ => {}
        });
        let (open_row, close_row) =
            (open.start_position().row, close.start_position().row);
        let statement_row = node.start_position().row;
        let parent = node.parent().map(|p| p.id());
        let line_start = node.start_byte() - node.start_position().column;
        let starts_line = source[line_start..node.start_byte()]
            .trim()
            .is_empty();
        let follows_sibling = lists.last().is_some_and(|l| {
            l.close_row == statement_row
                && l.parent == parent
                && parent.is_some()
        });
        let shares_row = lists
            .last()
            .is_some_and(|l| l.close_row >= open_row);
        if !safe
            || !(starts_line || follows_sibling)
            || (shares_row && !follows_sibling)
        {
            if close_row > open_row
                && lists
                    .last()
                    .is_some_and(|l| l.close_row == open_row)
            {
                // Drop the chain that ends on this row.
                while let Some(dropped) = lists.pop() {
                    if lists
                        .last()
                        .is_none_or(|l| l.close_row != dropped.statement_row)
                    {
                        break;
                    }
                }
            }
            return;
        }
        lists.push(List {
            statement_row,
            parent,
            open_row,
            close_row,
            open_end: open.end_byte(),
            close_start: close.start_byte(),
            items,
            strings,
        });
    });
    lists
}

/// Whitespace runs (newlines too) outside the string literals of `text`,
/// which starts at byte `offset`, become single spaces.
fn collapse_whitespace(
    text: &str,
    offset: usize,
    strings: &[(usize, usize)],
) -> String {
    let mut out = String::new();
    let mut in_space = false;
    for (i, c) in text.char_indices() {
        let in_string = strings
            .iter()
            .any(|&(start, end)| start <= offset + i && offset + i < end);
        if c.is_whitespace() && !in_string {
            in_space = true;
            continue;
        }
        if in_space {
            out.push(' ');
            in_space = false;
        }
        out.push(c);
    }
    out
}

fn is_control_flow(node: Node) -> bool {
    matches!(
        node.kind(),
        "if_statement"
            | "case_statement"
            | "while_statement"
            | "foreach_statement"
    )
}

/// The width of a line's indentation in columns, with tab stops every `tab`
/// columns (the unit tabs are written with, so formatting twice is stable).
fn indentation(line: &str, tab: usize) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .fold(0, |column, c| {
            if c == '\t' {
                (column / tab + 1) * tab
            } else {
                column + 1
            }
        })
}

fn render_indent(columns: usize, options: &FormattingOptions) -> String {
    if options.insert_spaces {
        " ".repeat(columns)
    } else {
        let tab = options.tab_size.max(1) as usize;
        format!(
            "{}{}",
            "\t".repeat(columns / tab),
            " ".repeat(columns % tab)
        )
    }
}

/// Formats the document (or only the lines of `range`), returning line edits.
pub fn format(
    snapshot: &Snapshot,
    options: &FormattingOptions,
    range: Option<Range>,
) -> Vec<TextEdit> {
    let root = snapshot.root();
    // The planner works on tree-sitter rows, which a lone `\r` does not end.
    if root.has_error()
        || snapshot
            .source
            .lines
            .has_lone_carriage_returns()
    {
        return Vec::new();
    }
    let source = snapshot.text();
    let lines = &snapshot.source.lines;
    let line_count = lines.line_count();
    let mut planner = Planner {
        source,
        lines,
        rules: vec![None; line_count],
    };
    planner.container(root, 0);
    // Lines that end inside a string literal keep their trailing whitespace.
    let mut open_string = vec![false; line_count];

    // Never touch lines that start inside a string literal, and keep the
    // layout of block comments.
    syntax::walk(root, |node| {
        let (first, last) =
            (node.start_position().row, node.end_position().row);
        if first == last {
            return;
        }
        match node.kind() {
            "string" => {
                open_string[first] = true;
                (first + 1..=last)
                    .for_each(|row| planner.set(row, Rule::Keep));
            }
            "comment" => (first + 1..=last).for_each(|row| {
                planner.set(row, Rule::Relative { anchor: first })
            }),
            "opencypher_body" => {
                let (kept, open) = cypher_protected_rows(
                    &source[node.start_byte()..node.end_byte()],
                    first,
                );
                kept.into_iter()
                    .for_each(|row| planner.set(row, Rule::Keep));
                open.into_iter()
                    .for_each(|row| open_string[row] = true);
            }
            _ => {}
        }
    });

    let unit = options.tab_size.max(1) as usize;
    let keyword_edits =
        keyword_case_edits(snapshot, snapshot.config.format_keyword_case);
    let original: Vec<&str> = (0..line_count)
        .map(|row| {
            &source[lines.line_start(row)..lines.line_end(source, row)]
        })
        .collect();
    let newline = snapshot.source.newline();
    // The text of `start..end` with the keyword-case edits inside it applied.
    let cased = |start: usize, end: usize| {
        let mut text = source[start..end].to_string();
        let from = keyword_edits.partition_point(|e| e.0 < start);
        let to = keyword_edits.partition_point(|e| e.0 < end);
        for (s, e, replacement) in keyword_edits[from..to].iter().rev() {
            if *e <= end {
                text.replace_range(s - start..e - start, replacement);
            }
        }
        text
    };
    let in_range = |first: usize, last: usize| {
        range.is_none_or(|r| {
            (r.start.line as usize) <= last && first <= (r.end.line as usize)
        })
    };
    let mut lists = breakable_lists(root, source)
        .into_iter()
        .peekable();
    let mut new_indent: Vec<usize> = vec![0; line_count];
    let mut edits = Vec::new();
    let mut skip_to = 0;
    for row in 0..line_count {
        if row < skip_to {
            continue;
        }
        let line = original[row];
        let content = line.trim_start_matches([' ', '\t']);
        let is_blank = content.trim().is_empty();
        // A comment above a clause is indented as that clause is.
        let (rule, source_row) = match planner.rules[row] {
            Some(Rule::SameAs { row: target }) if target > row => {
                (planner.rules[target], target)
            }
            rule => (rule, row),
        };
        let indent = match rule {
            Some(Rule::Keep) => {
                // As written, blank lines too, except keyword case after a string.
                new_indent[row] = indentation(line, unit);
                let span = Span::new(
                    lines.line_start(row),
                    lines.line_end(source, row),
                );
                let text = cased(span.start, span.end);
                if text != line && in_range(row, row) {
                    edits.push(snapshot.edit(span, text));
                }
                continue;
            }
            _ if is_blank => 0,
            Some(Rule::Level(level)) => level * unit,
            Some(Rule::Continuation { anchor, aligned }) if anchor < row => {
                let shown = original[source_row];
                let relative = indentation(shown, unit) as isize
                    - indentation(original[anchor], unit) as isize;
                let closes = shown
                    .trim_start_matches([' ', '\t'])
                    .starts_with([')', ']', '}']);
                let relative = if closes || (aligned && relative == 0) {
                    relative.max(0)
                } else if relative <= 0 {
                    unit as isize
                } else {
                    relative
                };
                (new_indent[anchor] as isize + relative).max(0) as usize
            }
            Some(Rule::Under { anchor, depth }) if anchor < row => {
                new_indent[anchor] + depth * unit
            }
            Some(Rule::Relative { anchor }) if anchor < row => {
                let relative = indentation(line, unit) as isize
                    - indentation(original[anchor], unit) as isize;
                (new_indent[anchor] as isize + relative).max(0) as usize
            }
            _ => indentation(line, unit),
        };
        new_indent[row] = indent;
        while lists.next_if(|l| l.open_row < row).is_some() {}
        let content_start =
            lines.line_start(row) + (line.len() - content.len());
        let long = indent + content.trim_end().chars().count() > MAX_LINE;
        // The first list on this line that spans lines, or any if the line is long.
        let mut first = None;
        while let Some(list) = lists.next_if(|l| l.open_row == row) {
            if list.close_row > row || long {
                first = Some(list);
                break;
            }
        }
        if let Some(mut list) = first {
            // One item per line, one level past this line; the closing bracket
            // and the rest of its line at this line's indent. A sibling's list
            // that opens on that line is laid out the same way, in the same edit.
            let mut text = format!(
                "{}{}",
                render_indent(indent, options),
                cased(content_start, list.open_end)
            );
            loop {
                let last = list.items.len() - 1;
                for (i, &(start, end)) in list.items.iter().enumerate() {
                    let mut item = cased(start, end);
                    if item.contains('\n') {
                        item =
                            collapse_whitespace(&item, start, &list.strings);
                    }
                    let comma = if i < last { "," } else { "" };
                    text.push_str(&format!(
                        "{newline}{}{item}{comma}",
                        render_indent(indent + unit, options)
                    ));
                }
                if list.close_row > list.open_row {
                    new_indent[list.open_row + 1..list.close_row]
                        .fill(indent + unit);
                    new_indent[list.close_row] = indent;
                }
                let close_line_end = lines.line_end(source, list.close_row);
                let rest = cased(list.close_start + 1, close_line_end);
                let rest = if open_string[list.close_row] {
                    rest.trim_start()
                } else {
                    rest.trim()
                };
                let separator = if rest.is_empty() { "" } else { " " };
                let close = format!(
                    "{}{separator}",
                    &source[list.close_start..list.close_start + 1]
                );
                let long =
                    indent + close.len() + rest.chars().count() > MAX_LINE;
                let mut next = None;
                while let Some(candidate) =
                    lists.next_if(|l| l.open_row == list.close_row)
                {
                    if candidate.close_row > candidate.open_row || long {
                        next = Some(candidate);
                        break;
                    }
                }
                text.push_str(&format!(
                    "{newline}{}{close}",
                    render_indent(indent, options)
                ));
                match next {
                    Some(candidate) => {
                        text.push_str(
                            cased(list.close_start + 1, candidate.open_end)
                                .trim_start(),
                        );
                        list = candidate;
                    }
                    None => {
                        text.push_str(rest);
                        break;
                    }
                }
            }
            skip_to = list.close_row + 1;
            let end = lines.line_end(source, list.close_row);
            if text != source[lines.line_start(row)..end]
                && in_range(row, list.close_row)
            {
                edits.push(
                    snapshot
                        .edit(Span::new(lines.line_start(row), end), text),
                );
            }
            continue;
        }
        let body = if open_string[row] {
            content
        } else {
            content.trim_end()
        };
        let formatted = if is_blank {
            String::new()
        } else {
            let body = cased(content_start, content_start + body.len());
            format!("{}{}", render_indent(indent, options), body)
        };
        if formatted != line && in_range(row, row) {
            let span =
                Span::new(lines.line_start(row), lines.line_end(source, row));
            edits.push(snapshot.edit(span, formatted));
        }
    }
    if range.is_none() {
        // Exactly one trailing newline.
        let trimmed = source.trim_end();
        if !trimmed.is_empty() {
            let last_line = lines.line_of(trimmed.len() - 1);
            let start = lines.line_end(source, last_line);
            if &source[start..] != newline {
                edits.retain(|e| e.range.start.line as usize <= last_line);
                edits.push(
                    snapshot.edit(Span::new(start, source.len()), newline),
                );
            }
        }
    }
    edits
}

/// (start, end, replacement) for the keywords whose case differs from `case`.
/// Reserved words that are literals (`TRUE`, `FALSE`, `NULL`) count as keywords.
/// The `keyword-case` style hint uses this too, so it and the formatter agree.
pub(crate) fn keyword_case_edits(
    snapshot: &Snapshot,
    case: KeywordCase,
) -> Vec<(usize, usize, String)> {
    if case == KeywordCase::Preserve {
        return Vec::new();
    }
    let source = snapshot.text();
    let mut edits = Vec::new();
    syntax::walk(snapshot.root(), |node| {
        if !(syntax::is_keyword(node)
            || matches!(node.kind(), "boolean" | "null"))
        {
            return;
        }
        let text = syntax::text(node, source);
        let replacement = match case {
            KeywordCase::Upper => text.to_ascii_uppercase(),
            KeywordCase::Lower => text.to_ascii_lowercase(),
            KeywordCase::Preserve => return,
        };
        if replacement != text && replacement.eq_ignore_ascii_case(text) {
            edits.push((node.start_byte(), node.end_byte(), replacement));
        }
    });
    edits.sort();
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;
    use crate::lsp::types::Position;
    use crate::text::{PositionEncoding, SourceText};

    fn apply(text: &str, edits: &[TextEdit]) -> String {
        SourceText::new(text.to_string())
            .apply_edits(edits, PositionEncoding::Utf16)
    }

    /// The layout rules do not depend on the width, so these tests use 2 and
    /// keep their inputs short; the default width has its own test.
    fn two_spaces() -> FormattingOptions {
        FormattingOptions {
            tab_size: 2,
            ..FormattingOptions::default()
        }
    }

    fn format_text(text: &str, case: KeywordCase) -> String {
        let mut fixture = Fixture::new(text);
        fixture.config.format_keyword_case = case;
        let edits = format(&fixture.snapshot(), &two_spaces(), None);
        apply(text, &edits)
    }

    #[test]
    fn indents_by_four_spaces_by_default() {
        // The GSQL Style Guide: "Indent the body of a block by 4 spaces."
        assert_eq!(FormattingOptions::default().tab_size, 4);
        let input = "CREATE QUERY hello(VERTEX<Person> p) {\nStart = {p};\nIF TRUE THEN\nPRINT Start;\nEND;\n}\n";
        let expected = "CREATE QUERY hello(VERTEX<Person> p) {\n    Start = {p};\n    IF TRUE THEN\n        PRINT Start;\n    END;\n}\n";
        let fixture = Fixture::new(input);
        let edits =
            format(&fixture.snapshot(), &FormattingOptions::default(), None);
        assert_eq!(apply(input, &edits), expected);
    }

    #[test]
    fn the_style_guides_recommended_layouts_are_left_alone() {
        // Examples marked "Recommended" in the GSQL Style Guide (appendix of the language
        // reference): SELECT clauses lined up one level in, or `SELECT` on its own line.
        for text in [
            "CREATE QUERY active_members (INT activity_threshold) FOR GRAPH Social_Net {\n    SumAccum<INT> @activity_amount;\n    start = {Person.*};\n    result = SELECT v FROM start:v -(:e)- Post:tgt\n        ACCUM v.@activity_amount +=1\n        HAVING v.@activity_amount >= activity_threshold;\n    PRINT result;\n}\n",
            "CREATE QUERY activity_align (INT activity_threshold) FOR GRAPH Social_Net {\n    SumAccum<INT> @activity_amount;\n    start = {Person.*};\n    result =\n        SELECT v\n        FROM start:v -(:e)- Post:tgt\n        ACCUM v.@activity_amount +=1;\n    PRINT result;\n}\n",
        ] {
            let fixture = Fixture::new(text);
            let edits = format(
                &fixture.snapshot(),
                &FormattingOptions::default(),
                None,
            );
            assert!(edits.is_empty(), "{edits:?}");
        }
    }

    #[test]
    fn reindents_blocks() {
        let input = "CREATE QUERY q() {\nINT x = 1;\n      IF x > 0 THEN\nPRINT x;\n   ELSE\n PRINT 0;\n        END;\n}\n";
        let expected = "CREATE QUERY q() {\n  INT x = 1;\n  IF x > 0 THEN\n    PRINT x;\n  ELSE\n    PRINT 0;\n  END;\n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    #[test]
    fn keeps_relative_alignment_of_continuations() {
        let input = "CREATE QUERY q() {\n      R = SELECT t FROM S:s -(E)- :t\n               WHERE t.x > 1\n               ACCUM @@a += 1;\n}\n";
        let expected = "CREATE QUERY q() {\n  R = SELECT t FROM S:s -(E)- :t\n           WHERE t.x > 1\n           ACCUM @@a += 1;\n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    #[test]
    fn indents_flush_continuations_and_closing_parens() {
        let input =
            "CREATE VERTEX Person (\nPRIMARY_ID id STRING,\nname STRING\n)\n";
        let expected = "CREATE VERTEX Person (\n  PRIMARY_ID id STRING,\n  name STRING\n)\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    #[test]
    fn leaves_multiline_strings_and_block_comments_in_opencypher_bodies_alone()
     {
        let input = "CREATE OPENCYPHER QUERY q() FOR GRAPH g {\nMATCH (u:P)\nWHERE u.name = \"a\nb  c\"   \n   AND u.k = 'x\n  y'\n/* note\n   more */\nRETURN u // it's\n}\n";
        let expected = "CREATE OPENCYPHER QUERY q() FOR GRAPH g {\n  MATCH (u:P)\n  WHERE u.name = \"a\nb  c\"   \n   AND u.k = 'x\n  y'\n  /* note\n   more */\n  RETURN u // it's\n}\n";
        let once = format_text(input, KeywordCase::Preserve);
        assert_eq!(once, expected);
        assert_eq!(format_text(&once, KeywordCase::Preserve), once);
        // The token stream, whitespace inside string literals included, is unchanged.
        let tokens = |t: &str| {
            let mut out = Vec::new();
            let mut quote = None;
            let mut current = String::new();
            for c in t.chars() {
                match quote {
                    Some(q) => {
                        current.push(c);
                        if c == q {
                            out.push(std::mem::take(&mut current));
                            quote = None;
                        }
                    }
                    None if c == '"' || c == '\'' => {
                        quote = Some(c);
                        current.push(c);
                    }
                    None if c.is_whitespace() => {}
                    None => out.push(c.to_string()),
                }
            }
            out
        };
        assert_eq!(tokens(&once), tokens(input));
    }

    #[test]
    fn leaves_multiline_strings_alone() {
        let input = "CREATE QUERY q() {\n    PRINT \"a\n   b\";\n}\n";
        let expected = "CREATE QUERY q() {\n  PRINT \"a\n   b\";\n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    #[test]
    fn normalizes_keyword_case_and_trailing_whitespace() {
        let input = "create query q() {   \n  print true;\n}";
        let expected = "CREATE QUERY q() {\n  PRINT TRUE;\n}\n";
        assert_eq!(format_text(input, KeywordCase::Upper), expected);
    }

    #[test]
    fn keyword_case_applies_on_the_line_where_a_multi_line_string_ends() {
        let input = "create query q() {\n  print true;\n  print \"a \nb\", true and false;\n}\n";
        let expected = "CREATE QUERY q() {\n  PRINT TRUE;\n  PRINT \"a \nb\", TRUE AND FALSE;\n}\n";
        let once = format_text(input, KeywordCase::Upper);
        assert_eq!(once, expected);
        assert_eq!(
            format_text(&once, KeywordCase::Upper),
            once,
            "second pass"
        );
        // The `keyword-case` style hint agrees.
        let fixture = Fixture::new(&once);
        assert!(
            keyword_case_edits(&fixture.snapshot(), KeywordCase::Upper)
                .is_empty()
        );
        assert_eq!(format_text(expected, KeywordCase::Lower), input);
    }

    #[test]
    fn whitespace_only_lines_inside_a_multi_line_string_are_kept() {
        let input = "CREATE QUERY q() {\n  print \"a\n   \nb\", true;\n}\n";
        let expected =
            "CREATE QUERY q() {\n  PRINT \"a\n   \nb\", TRUE;\n}\n";
        let once = format_text(input, KeywordCase::Upper);
        assert_eq!(once, expected);
        assert_eq!(
            format_text(&once, KeywordCase::Upper),
            once,
            "second pass"
        );
        // In an openCypher body too.
        let input = "CREATE OPENCYPHER QUERY q() FOR GRAPH g {\nMATCH (u:P)\n\
                     WHERE u.name = \"a\n   \nb\"\nRETURN u\n}\n";
        let expected = "CREATE OPENCYPHER QUERY q() FOR GRAPH g {\n  MATCH (u:P)\n  \
                        WHERE u.name = \"a\n   \nb\"\n  RETURN u\n}\n";
        let once = format_text(input, KeywordCase::Preserve);
        assert_eq!(once, expected);
        assert_eq!(
            format_text(&once, KeywordCase::Preserve),
            once,
            "second pass"
        );
    }

    #[test]
    fn formatting_with_tabs_is_stable() {
        for tab_size in [2, 4, 8] {
            let options = FormattingOptions {
                tab_size,
                insert_spaces: false,
                ..FormattingOptions::default()
            };
            let mut text =
                "CREATE QUERY q() {\n\tR = SELECT s FROM P:s\n\t\tWHERE s.x > 1;\n\tPRINT R;\n}\n".to_string();
            let mut passes = Vec::new();
            for _ in 0..3 {
                let fixture = Fixture::new(&text);
                text = apply(
                    &text,
                    &format(&fixture.snapshot(), &options, None),
                );
                passes.push(text.clone());
            }
            assert_eq!(passes[0], passes[2], "tab size {tab_size}");
        }
    }

    #[test]
    fn refuses_to_format_broken_files() {
        let fixture = Fixture::new("CREATE QUERY q() {\n PRINT ;\n}\n");
        assert!(
            format(&fixture.snapshot(), &FormattingOptions::default(), None)
                .is_empty()
        );
    }

    #[test]
    fn keeps_trailing_spaces_inside_strings() {
        let input = "CREATE QUERY q() {\n    PRINT \"a  \n b\";   \n}\n";
        let expected = "CREATE QUERY q() {\n  PRINT \"a  \n b\";   \n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    #[test]
    fn keeps_block_comment_layout() {
        let input = "CREATE QUERY q() {\n      /*\n       Title\n      flush text\n          indented\n      */\n      PRINT 1;\n}\n";
        let expected = "CREATE QUERY q() {\n  /*\n   Title\n  flush text\n      indented\n  */\n  PRINT 1;\n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    #[test]
    fn keeps_select_clauses_aligned_with_select() {
        let input = "CREATE QUERY q() {\n    SELECT p.g AS g, COUNT(p) AS n INTO T\n    FROM Person:p\n    GROUP BY p.g;\n  R = SELECT s\n  FROM P:s;\n  PRINT T, R;\n}\n";
        let expected = "CREATE QUERY q() {\n  SELECT p.g AS g, COUNT(p) AS n INTO T\n  FROM Person:p\n  GROUP BY p.g;\n  R = SELECT s\n  FROM P:s;\n  PRINT T, R;\n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    fn assert_formats(input: &str, expected: &str) {
        let once = format_text(input, KeywordCase::Preserve);
        assert_eq!(once, expected);
        assert_eq!(
            format_text(&once, KeywordCase::Preserve),
            once,
            "second pass"
        );
    }

    #[test]
    fn one_line_case_when_in_accum_lines_up_branches() {
        let input = "CREATE QUERY q(INT a) FOR GRAPH g {\nR = SELECT s FROM S:s ACCUM\nCASE WHEN a > 1 THEN\n@@t += 1\nWHEN a > 0 THEN\n@@t += 2\nELSE\n@@t += 3\nEND;\n}\n";
        let expected = "CREATE QUERY q(INT a) FOR GRAPH g {\n  R = SELECT s FROM S:s ACCUM\n    CASE WHEN a > 1 THEN\n        @@t += 1\n      WHEN a > 0 THEN\n        @@t += 2\n      ELSE\n        @@t += 3\n    END;\n}\n";
        assert_formats(input, expected);
        // The bare form is unchanged.
        let bare = "CREATE QUERY q(INT a) FOR GRAPH g {\n  R = SELECT s FROM S:s ACCUM\n    CASE\n      WHEN a > 1 THEN\n        @@t += 1\n      ELSE\n        @@t += 3\n    END;\n}\n";
        assert_formats(bare, bare);
    }

    #[test]
    fn one_line_case_when_statement_is_stable() {
        let input = "CREATE QUERY q(INT a) FOR GRAPH g {\nCASE WHEN a > 1 THEN\nPRINT 1;\nWHEN a > 0 THEN\nPRINT 2;\nELSE\nPRINT 3;\nEND;\n}\n";
        let expected = "CREATE QUERY q(INT a) FOR GRAPH g {\n  CASE WHEN a > 1 THEN\n      PRINT 1;\n    WHEN a > 0 THEN\n      PRINT 2;\n    ELSE\n      PRINT 3;\n  END;\n}\n";
        assert_formats(input, expected);
    }

    #[test]
    fn brace_on_its_own_line_lines_up_with_the_closing_brace() {
        let input =
            "CREATE OR REPLACE QUERY q(INT a)\nFOR GRAPH g\n{\nPRINT 1;\n}\n";
        let expected = "CREATE OR REPLACE QUERY q(INT a)\n  FOR GRAPH g\n{\n  PRINT 1;\n}\n";
        assert_formats(input, expected);
        // The same-line brace is unchanged.
        let same = "CREATE QUERY q() FOR GRAPH g {\n  PRINT 1;\n}\n";
        assert_formats(same, same);
    }

    #[test]
    fn comments_above_clauses_take_the_clause_indent() {
        let input = "CREATE QUERY q() FOR GRAPH g {\nS = {Person.*};\nR = SELECT s FROM S:s\n// comment between clauses\nWHERE s.x > 1 // trail\n// another\nACCUM s.@c += 1;\n}\n";
        let expected = "CREATE QUERY q() FOR GRAPH g {\n  S = {Person.*};\n  R = SELECT s FROM S:s\n  // comment between clauses\n  WHERE s.x > 1 // trail\n  // another\n  ACCUM s.@c += 1;\n}\n";
        assert_formats(input, expected);
        // Clauses indented past the SELECT keep that, and so do their comments.
        let deeper = "CREATE QUERY q() FOR GRAPH g {\n  R = SELECT s FROM S:s\n        // c\n        WHERE s.x > 1;\n}\n";
        let expected = "CREATE QUERY q() FOR GRAPH g {\n  R = SELECT s FROM S:s\n        // c\n        WHERE s.x > 1;\n}\n";
        assert_formats(deeper, expected);
    }

    #[test]
    fn comments_not_above_a_clause_keep_the_continuation_indent() {
        let input = "CREATE QUERY q() FOR GRAPH g {\n  R = SELECT s FROM S:s\n  WHERE s.x > 1\n      // inside the condition\n      AND s.y > 2;\n}\n";
        assert_formats(input, input);
    }

    #[test]
    fn multi_line_parameter_and_tuple_lists_get_one_item_per_line() {
        let input = "CREATE QUERY q(INT a,\n      STRING b=\"\") FOR GRAPH g {\nTYPEDEF TUPLE<x INT,\n   y STRING> T;\nPRINT a;\n}\n";
        let expected = "CREATE QUERY q(\n  INT a,\n  STRING b=\"\"\n) FOR GRAPH g {\n  TYPEDEF TUPLE<\n    x INT,\n    y STRING\n  > T;\n  PRINT a;\n}\n";
        assert_formats(input, expected);
        // A closing bracket alone on its line, and a long line with the default width.
        let input = "CREATE QUERY q(INT a, INT b\n) {\n  PRINT a;\n}\n";
        assert_formats(
            input,
            "CREATE QUERY q(\n  INT a,\n  INT b\n) {\n  PRINT a;\n}\n",
        );
        let long = format!(
            "CREATE QUERY q(INT a, STRING {}) {{\n    PRINT a;\n}}\n",
            "b".repeat(60)
        );
        let fixture = Fixture::new(&long);
        let once = apply(
            &long,
            &format(&fixture.snapshot(), &FormattingOptions::default(), None),
        );
        let b = "b".repeat(60);
        assert_eq!(
            once,
            format!(
                "CREATE QUERY q(\n    INT a,\n    STRING {b}\n) {{\n    PRINT a;\n}}\n"
            )
        );
    }

    #[test]
    fn short_lists_and_lists_with_comments_are_left_alone() {
        for text in [
            "CREATE QUERY q(INT a, STRING b) FOR GRAPH g {\n  TYPEDEF TUPLE<x INT, y STRING> T;\n  PRINT a;\n}\n",
            "CREATE QUERY q() FOR GRAPH g {\n  PRINT 1;\n}\n",
            "CREATE QUERY q(INT a, // first\n  STRING b) FOR GRAPH g {\n  TYPEDEF TUPLE<x INT, /* y */\n    y STRING> T;\n  PRINT a;\n}\n",
        ] {
            assert_formats(text, text);
        }
    }

    #[test]
    fn items_that_span_lines_are_joined_outside_strings() {
        let input = "CREATE QUERY q(INT a, STRING\n   b = \"x  y\") {\n  PRINT a;\n}\n";
        let expected = "CREATE QUERY q(\n  INT a,\n  STRING b = \"x  y\"\n) {\n  PRINT a;\n}\n";
        assert_formats(input, expected);
    }

    #[test]
    fn list_layout_follows_the_statement_indent_and_keyword_case() {
        // A TYPEDEF one level in puts its fields two levels in.
        let input = "create query q(int a,\n  string b) for graph g {\n        typedef tuple<x int,\ny string> T;\n  print a;\n}\n";
        let expected = "CREATE QUERY q(\n  INT a,\n  STRING b\n) FOR GRAPH g {\n  TYPEDEF TUPLE<\n    x INT,\n    y STRING\n  > T;\n  PRINT a;\n}\n";
        let once = format_text(input, KeywordCase::Upper);
        assert_eq!(once, expected);
        assert_eq!(format_text(&once, KeywordCase::Upper), once);
    }

    #[test]
    fn range_formatting_lays_out_the_whole_list_or_none_of_it() {
        let input = "CREATE QUERY q(INT a,\nINT b,\nINT c) {\nPRINT a;\nPRINT b;\n}\n";
        let fixture = Fixture::new(input);
        let line = |l: u32| {
            Some(Range::new(Position::new(l, 0), Position::new(l, 1)))
        };
        // A range on the middle item lays out the whole list, in one edit.
        let edits = format(&fixture.snapshot(), &two_spaces(), line(1));
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(
            apply(input, &edits),
            "CREATE QUERY q(\n  INT a,\n  INT b,\n  INT c\n) {\nPRINT a;\nPRINT b;\n}\n"
        );
        // A range below the list leaves it alone.
        let edits = format(&fixture.snapshot(), &two_spaces(), line(4));
        assert_eq!(
            apply(input, &edits),
            "CREATE QUERY q(INT a,\nINT b,\nINT c) {\nPRINT a;\n  PRINT b;\n}\n"
        );
        // A range over the whole text gives the same as formatting the document.
        let all = Some(Range::new(Position::new(0, 0), Position::new(6, 0)));
        let edits = format(&fixture.snapshot(), &two_spaces(), all);
        assert_eq!(
            apply(input, &edits),
            format_text(input, KeywordCase::Preserve)
        );
    }

    #[test]
    fn a_sibling_list_on_the_closing_line_is_laid_out_too() {
        let input = "CREATE QUERY q() FOR GRAPH g {\n  TYPEDEF TUPLE<a INT,\n  b INT> T; TYPEDEF TUPLE<c INT,\n d INT> U;\n  PRINT 1;\n}\n";
        let expected = "CREATE QUERY q() FOR GRAPH g {\n  TYPEDEF TUPLE<\n    a INT,\n    b INT\n  > T; TYPEDEF TUPLE<\n    c INT,\n    d INT\n  > U;\n  PRINT 1;\n}\n";
        assert_formats(input, expected);
        let fixture = Fixture::new(input);
        let edits = format(
            &fixture.snapshot(),
            &two_spaces(),
            Some(Range::new(Position::new(3, 0), Position::new(3, 1))),
        );
        assert_eq!(edits.len(), 1, "{edits:?}");
        // A short list before a multi-line one on the same line stays inline.
        let input = "CREATE QUERY q() FOR GRAPH g {\n  TYPEDEF TUPLE<a INT> A; TYPEDEF TUPLE<b INT,\n c INT> B;\n  PRINT 1;\n}\n";
        let expected = "CREATE QUERY q() FOR GRAPH g {\n  TYPEDEF TUPLE<a INT> A; TYPEDEF TUPLE<\n    b INT,\n    c INT\n  > B;\n  PRINT 1;\n}\n";
        assert_formats(input, expected);
        // A long line with a one-line list on it.
        let long = format!(
            "CREATE QUERY q() FOR GRAPH g {{\n  TYPEDEF TUPLE<{} INT> A; TYPEDEF TUPLE<b INT> B;\n}}\n",
            "a".repeat(60)
        );
        let a = "a".repeat(60);
        assert_formats(
            &long,
            &format!(
                "CREATE QUERY q() FOR GRAPH g {{\n  TYPEDEF TUPLE<\n    {a} INT\n  > A; TYPEDEF TUPLE<b INT> B;\n}}\n"
            ),
        );
    }

    #[test]
    fn lists_whose_statement_does_not_start_the_line_are_left_alone() {
        let long = format!(
            "CREATE QUERY q() FOR GRAPH g {{ TYPEDEF TUPLE<{} INT, b INT> T; PRINT 1; }}\n",
            "a".repeat(60)
        );
        assert_formats(&long, &long);
        // Nor is a list ending on the line where such a list starts to span lines.
        let input = "CREATE QUERY q(INT a,\n  INT b) FOR GRAPH g { TYPEDEF TUPLE<x INT,\n    y INT> T;\n  PRINT a;\n}\n";
        assert_formats(input, input);
        // Or where a list with a comment does.
        let input = "CREATE QUERY q() FOR GRAPH g {\n  TYPEDEF TUPLE<a INT,\n    b INT> A; TYPEDEF TUPLE<c INT, // c\n      d INT> B;\n}\n";
        assert_formats(input, input);
    }

    #[test]
    fn lists_go_one_level_past_the_line_they_open_on() {
        let input = "CREATE OR REPLACE QUERY\n    q(INT a,\n  INT b) FOR GRAPH g {\n  PRINT a;\n}\n";
        let expected = "CREATE OR REPLACE QUERY\n    q(\n      INT a,\n      INT b\n    ) FOR GRAPH g {\n  PRINT a;\n}\n";
        assert_formats(input, expected);
        let input = "CREATE QUERY q\n  (INT a,\n   INT b) {\n  PRINT a;\n}\n";
        assert_formats(
            input,
            "CREATE QUERY q\n  (\n    INT a,\n    INT b\n  ) {\n  PRINT a;\n}\n",
        );
    }

    #[test]
    fn list_layout_with_tabs_and_crlf() {
        let input = "CREATE QUERY q(INT a,\r\n INT b) {\r\nTYPEDEF TUPLE<x INT,\r\n y INT> T;\r\nPRINT a;\r\n}\r\n";
        let fixture = Fixture::new(input);
        let tabs = FormattingOptions {
            tab_size: 4,
            insert_spaces: false,
            ..FormattingOptions::default()
        };
        let once = apply(input, &format(&fixture.snapshot(), &tabs, None));
        assert_eq!(
            once,
            "CREATE QUERY q(\r\n\tINT a,\r\n\tINT b\r\n) {\r\n\tTYPEDEF TUPLE<\r\n\t\tx INT,\r\n\t\ty INT\r\n\t> T;\r\n\tPRINT a;\r\n}\r\n"
        );
        assert!(
            format(&Fixture::new(&once).snapshot(), &tabs, None).is_empty()
        );
    }

    #[test]
    fn mixed_line_endings_take_the_newline_code_actions_use() {
        // The first line break is `\n`.
        let input = "CREATE QUERY q(INT a,\n INT b) {\r\nPRINT a;\r\n}\r\n";
        assert_eq!(
            Fixture::new(input)
                .snapshot()
                .source
                .newline(),
            "\n"
        );
        let expected =
            "CREATE QUERY q(\n  INT a,\n  INT b\n) {\r\n  PRINT a;\r\n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), expected);
    }

    #[test]
    fn formatting_is_idempotent() {
        let input = "CREATE QUERY q() {\n  FOREACH x IN @@s DO\n    CASE\n      WHEN x > 1 THEN\n        PRINT x;\n      ELSE\n        PRINT 0;\n    END;\n  END;\n}\n";
        assert_eq!(format_text(input, KeywordCase::Preserve), input);
    }
}
