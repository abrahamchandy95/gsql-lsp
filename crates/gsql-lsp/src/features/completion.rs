//! Context-aware completion.

use std::collections::HashSet;

use tree_sitter::Node;

use crate::analysis::{ScopeKind, SymbolKind, Ty};
use crate::builtins;
use crate::features::Snapshot;
use crate::features::resolve::methods_for;
use crate::lsp::types::{CompletionItem, CompletionList, Position, Range, TextEdit, completion_kind as kind};
use crate::syntax;
use crate::text::Span;
use crate::workspace::GlobalSymbol;

/// Where the cursor is, as far as completion is concerned.
#[derive(Debug, Clone, PartialEq)]
enum Context {
    /// After `object.`; `local_only` when the user already typed `@`.
    Member {
        ty: MemberOf,
        local_only: bool,
    },
    GlobalAccumulator,
    LocalAccumulator,
    VertexType,
    EdgeType,
    /// The edge slot of `-(..)-` after a source vertex of known types.
    EdgeStep {
        source: Vec<String>,
        direction: Direction,
        /// Edge types written before the cursor in the same alternation.
        used: Vec<String>,
    },
    /// The vertex after `-(Edge)-`: only the types the edge can lead to.
    VertexTarget(Vec<String>),
    /// Clauses that may still follow those written in a SELECT block (`stage`
    /// is the last one present); `only` when nothing else fits there.
    SelectClauses {
        stage: usize,
        only: bool,
    },
    /// A field type of a tuple: no accumulators.
    ScalarType,
    /// The type of an attribute in a schema definition.
    AttributeType,
    /// Attribute names of a vertex or edge type; `columns` in an INSERT list,
    /// `used` are the names already in the list.
    Attributes {
        owner: String,
        columns: bool,
        /// In `DROP ATTRIBUTE`: the primary id cannot be dropped.
        drop: bool,
        used: Vec<String>,
    },
    /// The key slot of a `{..}` property map of a pattern: attributes of the
    /// labelled types, minus the keys already written.
    PropertyKeys {
        owners: Vec<String>,
        used: Vec<String>,
    },
    /// An edge type to insert (a reverse edge is created with its edge) or a variable naming one.
    InsertEdge,
    /// The vertex types an edge endpoint may have (any when empty).
    Endpoint(Vec<String>),
    /// The members of a `CREATE GRAPH` list, minus those already listed.
    GraphMembers(Vec<String>),
    /// After `RUN LOADING JOB name USING`: the job's file variables and run options.
    RunOption {
        job: String,
        used: Vec<String>,
    },
    /// Names of the `DEFINE HEADER`s of a loading job.
    HeaderNames,
    /// Names of the temp tables of a loading job.
    TempTables,
    /// Column names of the headers of a loading job (`$"name"`).
    Columns(Vec<String>),
    Keywords(Vec<&'static str>),
    SchemaType,
    /// A vertex position in a FROM pattern: vertex sets and vertex types.
    VertexSource,
    Graph,
    /// A query name; `all` where the command also takes the keyword ALL.
    Query {
        all: bool,
    },
    /// A job name of the given kinds; `all` where the command also takes ALL.
    Job {
        kinds: Vec<SymbolKind>,
        all: bool,
    },
    LoadSource,
    Type,
    Statement,
    DmlStatement,
    TopLevel,
    LoadingStatement,
    SchemaChangeStatement,
    UsingOption,
    WithOption,
    Expression,
    /// A position where a new name is being typed.
    Nothing,
}

/// Direction of an edge step as far as it is written.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Direction {
    Out,
    In,
    Either,
}

#[derive(Debug, Clone, PartialEq)]
enum MemberOf {
    Value(Ty),
    /// `Person.` in a seed set: only `*` makes sense.
    VertexTypeName,
    /// A value whose type is not tracked and that is not a vertex: nothing to offer.
    Nothing,
}

pub fn completion(snapshot: &Snapshot, position: Position, snippets: bool) -> CompletionList {
    let offset = snapshot.offset(position);
    let mut builder = Builder {
        snapshot,
        offset,
        snippets,
        items: Vec::new(),
        seen: HashSet::new(),
        replace: None,
        clauses_listed: false,
    };
    let blanked = code_text(snapshot);
    let text = blanked;
    let word_start = word_start(text, offset);
    // Names inside the strings of a loading job (the string may not be closed yet).
    if let Some(context) = job_string(text, word_start) {
        builder.fill(&context);
        return CompletionList { is_incomplete: false, items: builder.items };
    }
    if in_comment_or_string(snapshot, offset) || in_open_block_comment(snapshot.text(), offset) {
        return CompletionList { is_incomplete: false, items: Vec::new() };
    }
    let accumulator_start = text[..word_start].trim_end_matches('@').len();
    let ats = word_start - accumulator_start;
    if ats > 0 {
        builder.replace = Some(Span::new(accumulator_start, offset));
    }
    let context = detect(snapshot, offset, accumulator_start, ats);
    builder.fill(&context);
    CompletionList { is_incomplete: false, items: builder.items }
}

fn in_comment_or_string(snapshot: &Snapshot, offset: usize) -> bool {
    if offset == 0 {
        return false;
    }
    let Some(node) = snapshot.root().descendant_for_byte_range(offset - 1, offset - 1) else {
        return false;
    };
    for n in syntax::self_and_ancestors(node).take(3) {
        match n.kind() {
            "comment" => {
                let text = syntax::text(n, snapshot.text());
                let line_comment = text.starts_with("//") || text.starts_with('#');
                // A line comment extends to the end of its line; a block
                // comment ends at `*/`.
                return offset > n.start_byte() && (offset < n.end_byte() || line_comment);
            }
            "string" => {
                let closed = n.end_byte() - n.start_byte() >= 2 && syntax::text(n, snapshot.text()).ends_with('"');
                return offset > n.start_byte() && (offset < n.end_byte() || !closed);
            }
            "string_content" | "escape_sequence" => return true,
            _ => {}
        }
    }
    false
}

/// Where the string or comment that starts at `index` ends (`end` if it is cut off
/// there), and whether it is a block comment that is not closed. `None` if `index` is code.
/// A string ends at its line.
fn noncode_end(bytes: &[u8], index: usize, end: usize) -> Option<(usize, bool)> {
    let line_end = || bytes[index..end].iter().position(|&b| b == b'\n').map_or(end, |n| index + n);
    let next = if index + 1 < end { bytes[index + 1] } else { 0 };
    match bytes[index] {
        b'"' => {
            let mut at = index + 1;
            while at < end {
                match bytes[at] {
                    b'\\' => at += 2,
                    b'"' => return Some((at + 1, false)),
                    b'\n' => return Some((at, false)),
                    _ => at += 1,
                }
            }
            Some((end, false))
        }
        b'#' => Some((line_end(), false)),
        b'/' if next == b'/' => Some((line_end(), false)),
        b'/' if next == b'*' => {
            let mut at = index + 2;
            let mut previous = b' ';
            while at < end {
                let c = bytes[at];
                at += 1;
                if previous == b'*' && c == b'/' {
                    return Some((at, false));
                }
                previous = c;
            }
            Some((end, true))
        }
        _ => None,
    }
}

/// The text of the document with comments blanked out (same byte offsets, line breaks
/// kept; strings stay): comments are trivia between any two tokens, so every scan of the
/// text before the cursor reads this instead of the document. Computed once per text
/// (cached on the `SourceText`).
fn code_text<'a>(snapshot: &Snapshot<'a>) -> &'a str {
    let source: &'a crate::text::SourceText = snapshot.source;
    source.blanked.get_or_init(|| {
        let bytes = source.text.as_bytes();
        let mut out = bytes.to_vec();
        let mut index = 0;
        while index < bytes.len() {
            let Some((stop, _)) = noncode_end(bytes, index, bytes.len()) else {
                index += 1;
                continue;
            };
            if bytes[index] != b'"' {
                out[index..stop].iter_mut().filter(|b| **b != b'\n').for_each(|b| *b = b' ');
            }
            index = stop;
        }
        String::from_utf8(out).expect("only whole characters are blanked")
    })
}

/// `text[..end]` with comments and strings blanked out (same byte offsets, line breaks
/// kept), and whether `end` is inside a `/* .. */` comment, closed or not.
fn code_only(text: &str, end: usize) -> (String, bool) {
    let bytes = text.as_bytes();
    let mut out = bytes[..end].to_vec();
    let (mut index, mut in_block) = (0, false);
    while index < end {
        let Some((stop, open)) = noncode_end(bytes, index, end) else {
            index += 1;
            continue;
        };
        let stop = stop.min(end);
        out[index..stop].iter_mut().filter(|b| **b != b'\n').for_each(|b| *b = b' ');
        in_block = open;
        index = stop;
    }
    (String::from_utf8(out).expect("only whole characters are blanked"), in_block)
}

/// Whether `offset` is inside a `/* .. */` comment, closed or not (found from the
/// text: strings and line comments are skipped).
fn in_open_block_comment(text: &str, offset: usize) -> bool {
    code_only(text, offset).1
}

/// The names offered inside `USER_DEFINED_HEADER="` and `$"` of a loading job.
fn job_string(text: &str, word_start: usize) -> Option<Context> {
    let before = text[..word_start].trim_end();
    if text[..word_start].ends_with("$\"") {
        return Some(Context::Columns(header_columns(text, word_start)));
    }
    let before = before.strip_suffix('"')?.trim_end().strip_suffix('=')?.trim_end();
    let name = "USER_DEFINED_HEADER";
    (before.len() >= name.len() && before[before.len() - name.len()..].eq_ignore_ascii_case(name))
        .then_some(Context::HeaderNames)
}

/// The column names of the `DEFINE HEADER` statements before `offset`: those
/// of the header the LOAD around `offset` selects with `USER_DEFINED_HEADER`,
/// or of every header when it selects none that is defined.
fn header_columns(text: &str, offset: usize) -> Vec<String> {
    let lower = text[..offset].to_ascii_lowercase();
    let mut headers: Vec<(String, Vec<String>)> = Vec::new();
    for (index, _) in lower.match_indices("define header") {
        let statement = text[index..offset].split(';').next().unwrap_or("");
        let Some((head, list)) = statement.split_once('=') else {
            continue;
        };
        let name = head.split_whitespace().nth(2).unwrap_or("").to_string();
        let columns = list.split('"').skip(1).step_by(2).filter(|p| !p.is_empty()).map(str::to_string).collect();
        headers.push((name, columns));
    }
    if let Some(selected) = selected_header(text, offset)
        && let Some((_, columns)) = headers.iter().rev().find(|(name, _)| *name == selected)
    {
        return columns.clone();
    }
    let mut columns: Vec<String> = Vec::new();
    for column in headers.into_iter().flat_map(|(_, columns)| columns) {
        if !columns.contains(&column) {
            columns.push(column);
        }
    }
    columns
}

/// The `USER_DEFINED_HEADER="name"` of the LOAD statement around `offset`
/// (its option clause follows the VALUES list, so it is read on both sides).
fn selected_header(text: &str, offset: usize) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let loads = |from: usize, to: usize| -> Vec<usize> {
        lower[from..to]
            .match_indices("load")
            .map(|(index, _)| from + index)
            .filter(|&at| !text[..at].ends_with(word) && !text[at + 4..].starts_with(word))
            // (A column named `load`, as in `$"load"`, is inside quotes.)
            .filter(|&at| {
                // (Counted from the cursor for a later LOAD: the name being typed there is open.)
                let base = if at > offset { offset } else { text[..at].rfind([';', '}']).map_or(0, |i| i + 1) };
                text[base..at].matches('"').count().is_multiple_of(2)
            })
            .collect()
    };
    let mut start = text[..offset].rfind([';', '}']).map_or(0, |i| i + 1);
    let mut end = text[offset..].find([';', '}']).map_or(text.len(), |i| offset + i);
    // An unfinished LOAD runs on into the next one, and the one around the
    // cursor may follow an earlier LOAD that lacks its `;`.
    if let Some(&at) = loads(start, offset).last() {
        start = at;
    }
    if let Some(&at) = loads(offset, end).first() {
        end = at;
    }
    let key = "user_defined_header";
    // (A mention inside a string or a `$"name"` is not the option.)
    let mut after = offset;
    if text[start..offset].matches('"').count() % 2 == 1 {
        // The cursor is inside a `$".."` name: its closing quote, if typed, ends it.
        let rest = &text[offset..end];
        if let Some(quote) = rest.find('"').filter(|&q| rest[..q].find(')').is_none()) {
            after = offset + quote + 1;
        }
    }
    let found = lower[start..end].match_indices(key).find_map(|(index, _)| {
        let at = start + index;
        let quotes = if at < offset {
            text[start..at].matches('"').count()
        } else {
            text[after.min(at)..at].matches('"').count()
        };
        (quotes % 2 == 0 && (at < offset || at >= after)).then_some(at + key.len())
    })?;
    let value = text[found..end].trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
    Some(value[..value.find('"')?].to_string())
}

/// Where the unclosed `-(` of a FROM pattern around `offset` opens.
fn edge_parenthesis(text: &str, offset: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = offset;
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b'(' => {
                let before = text[..index].trim_end();
                // The grouped form `-((A|B):e)-` opens one parenthesis further out.
                if let Some(outer) = before.strip_suffix('(') {
                    let outer = outer.trim_end();
                    let open = before.len() - 1;
                    return (outer.ends_with('-') && !outer.ends_with("--") && after_from_keyword(text, open))
                        .then_some(open);
                }
                return (before.ends_with('-') && !before.ends_with("--") && after_from_keyword(text, index))
                    .then_some(index);
            }
            b')' | b';' | b'{' | b'}' | b'\n' => return None,
            _ => {}
        }
    }
    None
}

/// Whether the statement around `open` has a FROM before it (a pattern, not
/// `age -(` in an arithmetic expression).
fn after_from_keyword(text: &str, open: usize) -> bool {
    let start = text[..open].rfind([';', '{', '}']).map_or(0, |i| i + 1);
    let statement = &text[start..open];
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    statement
        .to_ascii_lowercase()
        .match_indices("from")
        .any(|(at, word)| !statement[..at].ends_with(is_word) && !statement[at + word.len()..].starts_with(is_word))
}

/// Whether `offset` is inside an unclosed `-(` of a FROM pattern.
fn in_edge_parentheses(text: &str, offset: usize) -> bool {
    edge_parenthesis(text, offset).is_some()
}

/// Where the unclosed `(` around `offset` opens (not across a statement).
fn enclosing_parenthesis(text: &str, offset: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let (mut depth, mut index) = (0, offset);
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b')' => depth += 1,
            b'(' if depth == 0 => return Some(index),
            b'(' => depth -= 1,
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
    }
    None
}

/// The type word (`TUPLE`, `MapAccum`, ..) whose unclosed `<..>` contains `offset`.
fn enclosing_type_angle(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let (mut depth, mut index) = (0, offset);
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b'>' => depth += 1,
            b'<' if depth == 0 => {
                let word = previous_tokens(text, index, 1).into_iter().next()?;
                let is_type =
                    matches!(word.as_str(), "TUPLE" | "SET" | "BAG" | "LIST" | "MAP") || word.ends_with("ACCUM");
                return is_type.then_some(word);
            }
            b'<' => depth -= 1,
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
    }
    None
}

/// Whether `offset` is inside the braces of a query whose closing brace has
/// not come yet (found from the text: strings and comments are skipped).
fn in_unclosed_query_body(text: &str, offset: usize) -> bool {
    let header = query_header_before(text, offset);
    let lower = text[header..offset].to_ascii_lowercase();
    let lower = lower.trim_start();
    (lower.starts_with("create") || lower.starts_with("interpret")) && braces_open(text, header, offset)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum JobKind {
    Loading,
    SchemaChange,
}

/// The kind of the job whose braces are still open at `offset`, from the text (the
/// tree has no job body around an unfinished one).
fn unclosed_job_body(text: &str, offset: usize) -> Option<JobKind> {
    let lower = code_only(text, offset).0.to_ascii_lowercase();
    let mut best = None;
    for (index, _) in lower.match_indices("create") {
        if lower[..index].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let rest = lower[index + 6..].trim_start();
        let rest = rest.strip_prefix("or replace").map_or(rest, str::trim_start);
        let rest = rest.strip_prefix("global").map_or(rest, str::trim_start);
        let kind = if rest.starts_with("loading") {
            JobKind::Loading
        } else if rest.starts_with("schema_change") {
            JobKind::SchemaChange
        } else {
            continue;
        };
        let after = rest.trim_start_matches(|c: char| is_word_char(c)).trim_start();
        if after.starts_with("job") {
            best = Some((index, kind));
        }
    }
    let (index, kind) = best?;
    // A query that starts later is the one the cursor may be in.
    (index >= query_header_before(text, offset) && braces_open(text, index, offset)).then_some(kind)
}

/// Whether a `{` after `header` is still open at `offset` (strings and comments skipped).
fn braces_open(text: &str, header: usize, offset: usize) -> bool {
    let (mut depth, mut seen_open) = (0i64, false);
    let mut chars = text[header..offset].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                while let Some(n) = chars.next() {
                    if n == '\\' {
                        chars.next();
                    } else if n == '"' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '#' => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for n in chars.by_ref() {
                    if previous == '*' && n == '/' {
                        break;
                    }
                    previous = n;
                }
            }
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => depth -= 1,
            _ => {}
        }
    }
    seen_open && depth > 0
}

/// Where the last `CREATE [OR REPLACE] [..] QUERY` (or `INTERPRET [OPENCYPHER] QUERY (`)
/// before `offset` starts.
fn query_header_before(text: &str, offset: usize) -> usize {
    let lower = code_only(text, offset).0.to_ascii_lowercase();
    let mut best = 0;
    for (index, _) in lower.match_indices("create") {
        let rest = lower[index + 6..].trim_start();
        let rest = rest.strip_prefix("or replace").map_or(rest, str::trim_start);
        let rest = rest.strip_prefix("distributed").map_or(rest, str::trim_start);
        let rest = rest.strip_prefix("template").map_or(rest, str::trim_start);
        if rest.starts_with("query") || rest.starts_with("opencypher") {
            best = index;
        }
    }
    for (index, _) in lower.match_indices("interpret") {
        if lower[..index].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let rest = lower[index + 9..].trim_start();
        let rest = rest.strip_prefix("opencypher").map_or(rest, str::trim_start);
        // `INTERPRET QUERY name(..)` runs a stored query and has no body.
        if rest.strip_prefix("query").is_some_and(|r| r.trim_start().starts_with('(')) && index > best {
            best = index;
        }
    }
    best
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn word_start(text: &str, offset: usize) -> usize {
    text[..offset].char_indices().rev().take_while(|(_, c)| is_word_char(*c)).last().map(|(i, _)| i).unwrap_or(offset)
}

/// Tokens before `offset`, last first (words upper-cased, punctuation as-is).
fn previous_tokens(text: &str, offset: usize, count: usize) -> Vec<String> {
    let mut tokens = raw_tokens(text, offset, count);
    tokens.iter_mut().for_each(|t| t.make_ascii_uppercase());
    tokens
}

/// Like `previous_tokens`, with the case of the text.
fn raw_tokens(text: &str, offset: usize, count: usize) -> Vec<String> {
    let is_token_char = |c: char| is_word_char(c) || c == '@' || c == '$';
    let mut tokens = Vec::new();
    let mut chars = text[..offset].char_indices().rev().peekable();
    while tokens.len() < count {
        let Some((index, c)) = chars.next() else {
            break;
        };
        if c.is_whitespace() {
            continue;
        }
        if is_token_char(c) {
            let end = index + c.len_utf8();
            let mut start = index;
            while let Some(&(previous, p)) = chars.peek() {
                if !is_token_char(p) {
                    break;
                }
                start = previous;
                chars.next();
            }
            tokens.push(text[start..end].to_string());
            continue;
        }
        if c == '"' {
            // Skip back over a string literal.
            for (_, p) in chars.by_ref() {
                if p == '"' {
                    break;
                }
            }
            tokens.push("\"\"".into());
            continue;
        }
        // Two-character arrows that matter for patterns.
        match (c, chars.peek().map(|&(_, p)| p)) {
            ('>', Some('-')) => {
                chars.next();
                tokens.push("->".into());
            }
            ('-', Some('<')) => {
                chars.next();
                tokens.push("<-".into());
            }
            _ => tokens.push(c.to_string()),
        }
    }
    tokens
}

/// The node just before `offset` and its ancestors.
fn ancestors_at<'t>(snapshot: &'t Snapshot, offset: usize) -> Vec<Node<'t>> {
    let root = snapshot.root();
    let Some(node) = root.descendant_for_byte_range(offset.saturating_sub(1), offset.saturating_sub(1)) else {
        return Vec::new();
    };
    syntax::lineage(root, node)
}

fn detect(snapshot: &Snapshot, offset: usize, word_start: usize, ats: usize) -> Context {
    let blanked = code_text(snapshot);
    let text = blanked;
    let analysis = snapshot.analysis;
    let scope = analysis.scope_at(offset);
    // (A syntax error can swallow the whole query, and with it its scope.)
    let in_query = analysis.query_scope(scope).is_some() || in_unclosed_query_body(text, offset);
    let ancestors = ancestors_at(snapshot, offset);
    let inside = |kind: &str| ancestors.iter().any(|n| n.kind() == kind && n.start_byte() < offset);
    let open_job = unclosed_job_body(text, offset);
    let in_job = inside("loading_job_body") || open_job == Some(JobKind::Loading);
    let in_schema_change = inside("schema_change_body") || open_job == Some(JobKind::SchemaChange);

    let before = &text[..word_start];
    // The dot may be separated from the word (and its object) by spaces or line breaks.
    let code = code_only(text, word_start).0;
    if code.trim_end().ends_with('.') && !ends_in_number(&code.trim_end()[..code.trim_end().len() - 1]) {
        // (Global accumulators are not members: `e.@@` has nothing to offer.)
        if ats >= 2 {
            return Context::Nothing;
        }
        let object_end = code.trim_end().len() - 1;
        return Context::Member { ty: member_type(snapshot, object_end), local_only: ats == 1 };
    }
    if ats >= 2 {
        return Context::GlobalAccumulator;
    }
    if ats == 1 {
        return Context::LocalAccumulator;
    }

    let tokens = previous_tokens(text, word_start, 6);
    let t = |i: usize| tokens.get(i).map(String::as_str).unwrap_or("");

    if in_query && let Some(context) = cypher_context(snapshot, text, offset, word_start) {
        return context;
    }

    // Parenthesised lists of schema definitions come first: their types are not query types.
    if let Some(context) = definition_lists(snapshot, text, word_start) {
        return context;
    }
    // Type arguments and typed positions (after any other word `<` compares).
    if t(0) == "<" {
        match t(1) {
            "VERTEX" => return Context::VertexType,
            "EDGE" => return Context::EdgeType,
            "(" if t(2) == "-" => {
                return edge_step(snapshot, text, offset, word_start).unwrap_or(Context::EdgeType);
            }
            "TUPLE" => return Context::ScalarType,
            "SET" | "BAG" | "LIST" | "MAP" | "FILE" => return Context::Type,
            word if word.ends_with("ACCUM") => return Context::Type,
            _ => {}
        }
    }
    if let Some(open) = enclosing_type_angle(text, word_start) {
        // After a comma a type follows; after a type the field name, which is new.
        if t(0) == "," {
            return if open == "TUPLE" { Context::ScalarType } else { Context::Type };
        }
        if t(0) == ">" || ends_operand(t(0)) {
            return Context::Nothing;
        }
    }
    if t(0) == "RETURNS" || (t(0) == "(" && t(1) == "RETURNS") {
        return Context::Type;
    }
    if let Some(context) = schema_lists(text, word_start, &tokens) {
        return context;
    }
    match (t(1), t(0)) {
        ("FOR" | "USE" | "DROP" | "ALTER", "GRAPH") => return Context::Graph,
        ("CREATE", "GRAPH") => return Context::Nothing,
        // `INSTALL QUERY ALL` and `DROP QUERY ALL` are the only forms with ALL; RUN, SHOW
        // and INTERPRET name one query.
        ("INSTALL" | "DROP", "QUERY") => return Context::Query { all: true },
        ("RUN" | "SHOW" | "INTERPRET", "QUERY") => return Context::Query { all: false },
        // A job of the written kind; `DROP JOB` and `SHOW JOB` take either (`DROP JOB ALL`).
        ("LOADING", "JOB") => return Context::Job { kinds: vec![SymbolKind::LoadingJob], all: false },
        ("SCHEMA_CHANGE", "JOB") => return Context::Job { kinds: vec![SymbolKind::SchemaChangeJob], all: false },
        ("DROP" | "SHOW", "JOB") => {
            let kinds = vec![SymbolKind::LoadingJob, SymbolKind::SchemaChangeJob];
            return Context::Job { kinds, all: t(1) == "DROP" };
        }
        ("INTO", "EDGE") => return Context::InsertEdge,
        ("TO" | "DROP" | "ALTER" | "DELETE" | "INTO", "VERTEX") => return Context::VertexType,
        ("TO" | "DROP" | "ALTER" | "DELETE", "EDGE") => return Context::EdgeType,
        ("INSERT", "INTO") => return Context::SchemaType,
        ("CREATE" | "ADD" | "DIRECTED" | "UNDIRECTED", "VERTEX" | "EDGE") => return Context::Nothing,
        _ => {}
    }
    if t(0) == "SHOW" && !in_query {
        return Context::Keywords(SHOW_KINDS.to_vec());
    }
    if !in_query
        && !in_job
        && !in_schema_change
        && enclosing_parenthesis(text, word_start).is_none()
        && let Some(words) = header_keywords(text, word_start, &tokens)
    {
        return Context::Keywords(words);
    }
    if t(0) == "DEFINE" && in_job {
        return Context::Keywords(vec!["FILENAME", "HEADER", "INPUT_LINE_FILTER"]);
    }
    if t(0) == "TEMP_TABLE" {
        return if t(1) == "LOAD" { Context::TempTables } else { Context::Nothing };
    }
    if t(0) == "LOAD" {
        return Context::LoadSource;
    }
    if t(0) == ":" || t(0) == "AS" {
        return Context::Nothing;
    }
    if (t(0) == "USING" || t(0) == ",")
        && let Some((job, used)) = run_loading_job(text, word_start)
    {
        return Context::RunOption { job, used };
    }
    // FROM patterns.
    let in_edge_parens = inside("edge_step") || in_edge_parentheses(text, word_start);
    if in_edge_parens && (t(0) == "(" || t(0) == "|" || t(0) == "<") {
        return edge_step(snapshot, text, offset, word_start).unwrap_or(Context::EdgeType);
    }
    if in_query && (t(0) == "-" || t(0) == "->") && t(1) == ")" {
        return edge_target(snapshot, text, word_start).unwrap_or(Context::VertexSource);
    }
    if in_query && t(0) == "FROM" {
        return Context::VertexSource;
    }
    if in_job && t(0) == "FROM" {
        return Context::LoadSource;
    }
    if t(0) == "," && inside("from_clause") {
        return Context::VertexSource;
    }
    // USING / WITH options.
    if t(0) == "USING" || (t(0) == "," && inside("using_clause")) {
        return Context::UsingOption;
    }
    if t(0) == "WITH" && !in_query {
        return Context::WithOption;
    }
    // Parameter lists: types at the start of a parameter, names after them.
    if t(0) == "(" && t(2) == "QUERY" {
        return Context::Type;
    }
    if inside("parameter_list") {
        return if t(0) == "(" || t(0) == "," { Context::Type } else { Context::Nothing };
    }
    // Statement starts.
    let after_statement_end = matches!(t(0), ";" | "{" | "THEN" | "DO" | "ELSE" | "");
    if in_job {
        return if after_statement_end { Context::LoadingStatement } else { Context::Expression };
    }
    if in_schema_change {
        return if after_statement_end { Context::SchemaChangeStatement } else { Context::Nothing };
    }
    if in_query {
        let in_dml = inside("accum_clause") || inside("post_accum_clause");
        if t(0) == "ACCUM" || t(0) == "POST_ACCUM" || (in_dml && t(0) == ",") {
            return Context::DmlStatement;
        }
        if after_statement_end {
            return if in_dml { Context::DmlStatement } else { Context::Statement };
        }
        if ends_operand(t(0))
            && let Some(words) = delete_statement_keywords(snapshot, text, word_start)
        {
            return Context::Keywords(words);
        }
        if ends_operand(t(0))
            && let Some(stage) = select_stage(snapshot, text, word_start)
        {
            // Nothing but a clause fits after FROM, after LIMIT, and after ORDER BY's ASC/DESC.
            let ordered = stage == 7 && matches!(t(0), "ASC" | "DESC");
            return Context::SelectClauses { stage, only: stage == 0 || stage == 8 || ordered };
        }
        return Context::Expression;
    }
    // Top level: a command starts at the beginning of a line.
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    if before[line_start..].trim().is_empty() || t(0) == ";" {
        return Context::TopLevel;
    }
    Context::Nothing
}

/// Whether `word_start` is in the header of a query (`CREATE QUERY q(..) FOR GRAPH g ..`,
/// `INTERPRET QUERY (..)`): after its parameter list, before the `{` of its body.
fn in_query_header(text: &str, word_start: usize) -> bool {
    let header = query_header_before(text, word_start);
    let code = code_only(text, word_start).0.to_ascii_lowercase();
    let code = &code[header..];
    let count = |w: &str| code.match_indices(w).filter(|(i, _)| !code[..*i].ends_with(is_word_char)).count();
    // (`INTERPRET QUERY name(..)` runs a stored query: no header.)
    let interpret = code.strip_prefix("interpret").map(|r| {
        let r = r.trim_start();
        let r = r.strip_prefix("opencypher").map_or(r, str::trim_start);
        r.strip_prefix("query").is_some_and(|r| r.trim_start().starts_with('('))
    });
    (code.starts_with("create") || interpret == Some(true))
        && code.contains('(')
        && !code.contains(['{', ';'])
        && count("create") + count("interpret") == 1
        && count("query") >= 1
        && enclosing_parenthesis(text, word_start).is_none()
}

/// Whether the tokens from `i` on (see `previous_tokens`) are `JOB` of a
/// `CREATE [OR REPLACE] [GLOBAL] LOADING | SCHEMA_CHANGE JOB`.
fn job_header(tokens: &[String], i: usize) -> bool {
    let t = |i: usize| tokens.get(i).map(String::as_str).unwrap_or("");
    let creating = |i: usize| match t(i) {
        "CREATE" => true,
        "GLOBAL" => t(i + 1) == "CREATE",
        "REPLACE" => t(i + 1) == "OR" && t(i + 2) == "CREATE",
        _ => false,
    };
    t(i) == "JOB" && matches!(t(i + 1), "LOADING" | "SCHEMA_CHANGE") && creating(i + 2)
}

/// The keywords that can follow the words of a definition or command header, from the
/// grammar: `CREATE`, `USE`, `DROP`, `INSTALL`, `RUN`, `INTERPRET`, the clauses of a
/// query or job header, `FOR` and `SYNTAX`. `tokens` are those before the word.
fn header_keywords(text: &str, word_start: usize, tokens: &[String]) -> Option<Vec<&'static str>> {
    let t = |i: usize| tokens.get(i).map(String::as_str).unwrap_or("");
    // Whether the tokens from `i` on are `CREATE`, or `CREATE OR REPLACE`.
    let creating = |i: usize| match t(i) {
        "CREATE" => true,
        "REPLACE" => t(i + 1) == "OR" && t(i + 2) == "CREATE",
        _ => false,
    };
    let words: &[&'static str] = match t(0) {
        "CREATE" => &[
            "QUERY",
            "OR",
            "DISTRIBUTED",
            "TEMPLATE",
            "FUNCTION",
            "OPENCYPHER",
            "VERTEX",
            "DIRECTED",
            "UNDIRECTED",
            "EDGE",
            "GRAPH",
            "LOADING",
            "SCHEMA_CHANGE",
            "GLOBAL",
            "DATA_SOURCE",
            "PACKAGE",
            "USER",
            "ROLE",
            "SECRET",
            "GROUP",
            "TOKEN",
        ],
        "OR" if t(1) == "CREATE" => &["REPLACE"],
        "REPLACE" if creating(0) => &["QUERY", "DISTRIBUTED", "TEMPLATE", "FUNCTION", "OPENCYPHER", "LOADING"],
        "DISTRIBUTED" if creating(1) => &["QUERY", "FUNCTION", "OPENCYPHER"],
        "TEMPLATE" if creating(1) => &["QUERY", "FUNCTION"],
        "OPENCYPHER" if creating(1) || matches!(t(1), "DISTRIBUTED" | "INTERPRET") => &["QUERY"],
        "DIRECTED" | "UNDIRECTED" if t(1) == "CREATE" => &["EDGE"],
        "LOADING" if creating(1) || matches!(t(1), "RUN" | "GLOBAL") => &["JOB"],
        "SCHEMA_CHANGE" if matches!(t(1), "CREATE" | "RUN" | "GLOBAL") => &["JOB"],
        "GLOBAL" if t(1) == "CREATE" => &["SCHEMA_CHANGE"],
        "GLOBAL" if t(1) == "RUN" => &["LOADING", "SCHEMA_CHANGE"],
        "USE" => &["GRAPH", "GLOBAL"],
        "DROP" => &[
            "VERTEX",
            "EDGE",
            "TUPLE",
            "GRAPH",
            "FUNCTION",
            "QUERY",
            "JOB",
            "PACKAGE",
            "DATA_SOURCE",
            "USER",
            "ROLE",
            "SECRET",
            "GROUP",
            "TAG",
            "ALL",
        ],
        "INSTALL" => &["QUERY", "FUNCTION"],
        "RUN" => &["QUERY", "LOADING", "SCHEMA_CHANGE", "GLOBAL"],
        "INTERPRET" => &["QUERY", "OPENCYPHER"],
        "FOR" if in_query_header(text, word_start) || job_header(tokens, 2) => &["GRAPH"],
        "SYNTAX" if in_query_header(text, word_start) => &["v1", "v2", "v3"],
        "API" if in_query_header(text, word_start) => &["v1", "v2"],
        ")" if in_query_header(text, word_start) => &["FOR", "RETURNS", "API", "SYNTAX"],
        // After `FOR GRAPH g`: the other clauses of the header.
        _ if t(1) == "GRAPH" && t(2) == "FOR" && in_query_header(text, word_start) => &["RETURNS", "API", "SYNTAX"],
        // `CREATE LOADING JOB j`: the graph clause comes before the body.
        _ if job_header(tokens, 1) => &["FOR"],
        _ => return None,
    };
    Some(words.to_vec())
}

/// Tokens that leave an expression unfinished.
const OPEN_WORDS: &[&str] = &[
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "IN",
    "LIKE",
    "BETWEEN",
    "IS",
    "ACCUM",
    "POST_ACCUM",
    "BY",
    "LIMIT",
    "HAVING",
    "SELECT",
    "FROM",
    "SAMPLE",
    "PER",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "DISTINCT",
    "INTO",
    "ON",
    "AS",
    "UNION",
    "INTERSECT",
    "MINUS",
    "DO",
    "IF",
    "WHILE",
    "FOREACH",
    "RETURN",
    "PRINT",
];

/// Whether a token (see `previous_tokens`) may end a complete operand.
fn ends_operand(token: &str) -> bool {
    match token {
        ")" | "]" | "\"\"" => true,
        "" => false,
        word => word.chars().all(|c| is_word_char(c) || c == '@' || c == '$') && !OPEN_WORDS.contains(&word),
    }
}

/// Ranks of the clauses of a SELECT block, in the order the grammar has them.
fn clause_stage(kind: &str) -> Option<usize> {
    Some(match kind {
        "from_clause" => 0,
        "sample_clause" => 1,
        "where_clause" => 2,
        "accum_clause" | "per_clause" => 3,
        "post_accum_clause" => 4,
        "group_by_clause" => 5,
        "having_clause" => 6,
        "order_by_clause" => 7,
        "limit_clause" => 8,
        _ => return None,
    })
}

/// The last clause written so far in the SELECT block the cursor is in, if a
/// FROM clause precedes the cursor.
fn select_stage(snapshot: &Snapshot, text: &str, word_start: usize) -> Option<usize> {
    let end = text[..word_start].trim_end().len();
    let last = end.checked_sub(1)?;
    let node = snapshot.root().descendant_for_byte_range(last, last)?;
    let select = syntax::self_and_ancestors(node).find(|n| n.kind() == "select_statement")?;
    let clauses = syntax::named_children(select);
    if !clauses.iter().any(|c| c.kind() == "from_clause" && c.start_byte() < word_start) {
        return None;
    }
    clauses.iter().filter(|c| c.start_byte() < word_start).filter_map(|c| clause_stage(c.kind())).max()
}

/// WHERE after the FROM clause of a `DELETE s FROM ..` statement (the only clause the
/// statement has besides FROM), if nothing follows that clause yet. (An `UPDATE s FROM ..`
/// without its SET is a syntax error without a statement node, so it offers nothing.)
fn delete_statement_keywords(snapshot: &Snapshot, text: &str, word_start: usize) -> Option<Vec<&'static str>> {
    let last = text[..word_start].trim_end().len().checked_sub(1)?;
    let node = snapshot.root().descendant_for_byte_range(last, last)?;
    let statement = syntax::self_and_ancestors(node).find(|n| n.kind() == "delete_statement")?;
    let clauses = syntax::named_children(statement);
    let from_last = clauses.last().is_some_and(|c| c.kind() == "from_clause" && c.end_byte() <= word_start);
    from_last.then(|| vec!["WHERE"])
}

/// The clause keywords that may still follow `stage` (GROUP BY only in SYNTAX v2).
fn clauses_after(stage: usize, v1: bool) -> Vec<&'static str> {
    // (keyword, rank, whether it may be repeated)
    [
        ("SAMPLE", 1, false),
        ("WHERE", 2, false),
        ("ACCUM", 3, false),
        ("PER", 3, false),
        ("POST-ACCUM", 4, true),
        ("GROUP BY", 5, false),
        ("HAVING", 6, false),
        ("ORDER BY", 7, false),
        ("LIMIT", 8, false),
    ]
    .into_iter()
    .filter(|&(name, rank, repeat)| (stage < rank || (repeat && stage == rank)) && !(v1 && name == "GROUP BY"))
    .map(|(name, ..)| name)
    .collect()
}

/// Whether the LIMIT clause that ends at `offset` already has an OFFSET (or `LIMIT j, k` form).
fn limit_has_offset(text: &str, offset: usize) -> bool {
    let lower = text[..offset].to_ascii_lowercase();
    let start = lower.rfind("limit").map_or(0, |i| i + "limit".len());
    lower[start..].contains("offset") || lower[start..].contains(',')
}

/// Whether the query the cursor is in says `SYNTAX v1`.
fn declares_syntax_v1(text: &str, offset: usize) -> bool {
    let header = query_header_before(text, offset);
    let end = text[header..].find('{').map_or(offset, |i| header + i).min(offset);
    let flat: String = text[header..end].chars().filter(|c| !c.is_whitespace() && *c != '"').collect();
    flat.to_ascii_lowercase().contains("syntaxv1")
}

/// The word that ends `text` (after trimming whitespace).
/// Whether `text` ends in a number (`1`, `2.5`: its `.` is a decimal point, not a member access).
fn ends_in_number(text: &str) -> bool {
    let digits = text.len() - text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && !text[..text.len() - digits].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '@')
}

fn trailing_word(text: &str) -> &str {
    let text = text.trim_end();
    &text[word_start(text, text.len())..]
}

/// The vertex types of the source of the edge step whose `-(` starts at `open`.
fn step_source(snapshot: &Snapshot, text: &str, open: usize) -> Vec<String> {
    let s = text[..open].trim_end();
    let s = s.strip_suffix('-').unwrap_or(s).trim_end();
    let s = s.strip_suffix('<').unwrap_or(s).trim_end();
    let last = trailing_word(s);
    if last.is_empty() {
        return Vec::new();
    }
    let rest = s[..s.len() - last.len()].trim_end();
    let (type_word, alias) = match rest.strip_suffix(':') {
        Some(rest) => (trailing_word(rest), Some(last)),
        None => (last, None),
    };
    let visible = snapshot.analysis.visible_symbols(open);
    if !type_word.is_empty() && !type_word.eq_ignore_ascii_case("from") {
        if !snapshot.workspace.find(SymbolKind::VertexType, type_word).is_empty() {
            return vec![type_word.to_string()];
        }
        let variable = [SymbolKind::VertexSet, SymbolKind::Parameter, SymbolKind::Variable];
        if let Some(symbol) = visible.iter().find(|v| v.name == type_word && variable.contains(&v.kind))
            && let Ty::Vertex(types) | Ty::VertexSet(types) = &symbol.ty
        {
            return types.clone();
        }
    }
    let alias_symbol = alias.and_then(|a| visible.iter().find(|v| v.name == a && v.kind == SymbolKind::Alias));
    match alias_symbol.map(|s| &s.ty) {
        Some(Ty::Vertex(types)) => types.clone(),
        _ => Vec::new(),
    }
}

/// The edge slot of a step: edges that touch the source vertex in the written direction.
fn edge_step(snapshot: &Snapshot, text: &str, offset: usize, word_start: usize) -> Option<Context> {
    let open = edge_parenthesis(text, word_start)?;
    let source = step_source(snapshot, text, open);
    if source.is_empty() || source.iter().any(|t| t == "*") {
        return None;
    }
    let reverse = text[..open].trim_end().strip_suffix('-').is_some_and(|s| s.ends_with('<'));
    let direction = if reverse || text[open + 1..word_start].contains('<') {
        Direction::In
    } else {
        let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
        let close = matching_close(text, open);
        let after = (close < line_end).then(|| text[close + 1..line_end].trim_start());
        if after.is_some_and(|r| r.starts_with("->")) { Direction::Out } else { Direction::Either }
    };
    // The alternatives written before the cursor need no second mention.
    let used = text[open + 1..word_start]
        .split(['|', '(', ')'])
        .map(|part| part.split(':').next().unwrap_or("").trim().trim_start_matches('<').trim_end_matches('>'))
        .filter(|name| !name.is_empty() && name.chars().all(is_word_char))
        .map(str::to_string)
        .collect();
    Some(Context::EdgeStep { source, direction, used })
}

/// The vertex slot after `-(Edge)-`: the types on the far end of the named edges.
fn edge_target(snapshot: &Snapshot, text: &str, word_start: usize) -> Option<Context> {
    let s = text[..word_start].trim_end();
    let (s, arrow_out) = match s.strip_suffix("->") {
        Some(rest) => (rest, true),
        None => (s.strip_suffix('-')?, false),
    };
    let s = s.trim_end().strip_suffix(')')?;
    let mut depth = 0;
    let open = s.char_indices().rev().find_map(|(i, c)| match c {
        ')' => {
            depth += 1;
            None
        }
        '(' if depth == 0 => Some(i),
        '(' => {
            depth -= 1;
            None
        }
        _ => None,
    })?;
    let inner = s[open + 1..].trim();
    let incoming = inner.starts_with('<') || s[..open].trim_end().strip_suffix('-').is_some_and(|b| b.ends_with('<'));
    target_types(snapshot, inner, incoming, arrow_out).map(Context::VertexTarget)
}

/// The alternatives of an edge step as written between `-(` and `)-`, without a
/// repetition (`E*1..3`, `(E|F>|<G)*`) or the parentheses around the alternation
/// (`(A|B):e`); and whether a `>` follows that group. None where the written
/// repetition may be empty.
fn step_alternatives(inner: &str) -> Option<(&str, bool)> {
    let mut depth = 0;
    let star = inner.char_indices().find_map(|(i, c)| {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '*' if depth == 0 => return Some(i),
            _ => {}
        }
        None
    });
    let mut list = inner.trim();
    if let Some(star) = star {
        // "(E*) means edge type E repeats any number of times (including zero)": then the
        // far end may be the source itself, so only a repetition that starts at 1 or more
        // names the far ends ("(E*1..3) means edge type E occurs one to three times").
        if !inner[star + 1..].trim_start().starts_with(|c: char| ('1'..='9').contains(&c)) {
            return None;
        }
        list = inner[..star].trim();
    }
    let mut out = false;
    if list.starts_with('(') {
        let close = matching_close(list, 0);
        if close >= list.len() {
            return None;
        }
        out = list[close + 1..].trim_start().starts_with('>');
        list = list[1..close].trim();
    }
    Some((list, out))
}

/// The vertex types on the far end of the `|`-separated edges in `inner` (`Lives>|<Friend:e`
/// or, in brackets, `Lives|Friend`; also repeated and grouped as `step_alternatives` reads
/// them); `incoming` for `<-`, `arrow_out` for a closing `->`.
fn target_types(snapshot: &Snapshot, inner: &str, incoming: bool, arrow_out: bool) -> Option<Vec<String>> {
    let (inner, group_out) = step_alternatives(inner)?;
    let mut types = Vec::new();
    for part in inner.split('|') {
        let name = part.split(':').next().unwrap_or("").trim();
        let (name, part_in) = match name.strip_prefix('<') {
            Some(name) => (name, true),
            None => (name, incoming),
        };
        let (name, part_out) = match name.strip_suffix('>') {
            Some(name) => (name, true),
            None => (name, arrow_out || group_out),
        };
        if name.is_empty() || !name.chars().all(is_word_char) {
            return None;
        }
        let edge = snapshot.workspace.find(SymbolKind::EdgeType, name).into_iter().next()?;
        let ends = &edge.ends;
        if ends.from.is_empty() || ends.to.is_empty() || ends.from.iter().chain(&ends.to).any(|t| t == "*") {
            return None;
        }
        let sides = match (ends.directed, part_in, part_out) {
            (true, true, _) => vec![&ends.from],
            (true, false, true) => vec![&ends.to],
            _ => vec![&ends.from, &ends.to],
        };
        types.extend(sides.into_iter().flatten().cloned());
    }
    Some(types)
}

/// The vertex types a name stands for in a pattern: a vertex type, or a vertex set,
/// parameter or variable of known types.
fn named_vertex_types(snapshot: &Snapshot, name: &str, offset: usize) -> Option<Vec<String>> {
    if !snapshot.workspace.find(SymbolKind::VertexType, name).is_empty() {
        return Some(vec![name.to_string()]);
    }
    let variable = [SymbolKind::VertexSet, SymbolKind::Parameter, SymbolKind::Variable];
    let visible = snapshot.analysis.visible_symbols(offset);
    let symbol = visible.iter().find(|v| v.name == name && variable.contains(&v.kind))?;
    match &symbol.ty {
        Ty::Vertex(types) | Ty::VertexSet(types) => Some(types.clone()),
        _ => None,
    }
}

/// The matching closer of the bracket at `open` (the end of `text` if unclosed).
fn matching_close(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut index = open;
    while index < bytes.len() {
        if let Some((stop, _)) = noncode_end(bytes, index, bytes.len()) {
            index = stop;
            continue;
        }
        match bytes[index] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
        index += 1;
    }
    text.len()
}

/// Whether a `(` after `before` opens an openCypher node pattern (and not a V2 edge step
/// `-(..)-`, a call or a grouping).
fn starts_node(before: &str) -> bool {
    let before = before.trim_end();
    before.is_empty()
        || before.ends_with(',')
        || before.ends_with("->")
        || before.ends_with("--")
        || (before.ends_with('-') && before[..before.len() - 1].trim_end().ends_with(']'))
        || trailing_word(before).eq_ignore_ascii_case("from")
}

/// `alias:Type1|Type2 *1..3` of a node or relationship pattern: the alias and the
/// type names (`None` without a colon). Anything else (an expression) is no header.
fn pattern_header(header: &str) -> Option<(&str, Option<Vec<&str>>)> {
    let allowed = |c: char| is_word_char(c) || c.is_whitespace() || matches!(c, ':' | '|' | '@' | '*' | '.');
    if !header.chars().all(allowed) || header.matches(':').count() > 1 {
        return None;
    }
    let header = header.split('*').next().unwrap_or("");
    let (alias, types) = match header.split_once(':') {
        Some((alias, types)) => (alias, Some(types)),
        None => (header, None),
    };
    let alias = alias.trim();
    if !alias.chars().all(is_word_char) {
        return None;
    }
    Some((alias, types.map(|t| t.split('|').map(str::trim).filter(|t| !t.is_empty()).collect())))
}

/// The pattern bracket around `pos`, found from the text: an unclosed `(` of an openCypher
/// node or `-[` of a relationship, and the `{` of a property map between it and `pos`.
struct PatternSlot {
    open: usize,
    bracket: bool,
    brace: Option<usize>,
}

fn pattern_slot(text: &str, pos: usize) -> Option<PatternSlot> {
    let text = &code_only(text, pos).0;
    let bytes = text.as_bytes();
    let (mut paren, mut square, mut curly) = (0, 0, 0);
    let (mut index, mut brace) = (pos, None);
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b')' => paren += 1,
            b']' => square += 1,
            b'}' => curly += 1,
            b'{' if curly > 0 => curly -= 1,
            b'{' if brace.is_none() => brace = Some(index),
            b'(' if paren > 0 => paren -= 1,
            b'[' if square > 0 => square -= 1,
            b'(' | b'[' => {
                let bracket = bytes[index] == b'[';
                let before = text[..index].trim_end();
                let fits = if bracket { before.ends_with('-') } else { starts_node(before) };
                return fits.then_some(PatternSlot { open: index, bracket, brace });
            }
            b';' | b'{' => return None,
            _ => {}
        }
    }
    None
}

/// Completion inside openCypher patterns: labels of `(s:|)`, types of `-[e:|]-` and the
/// keys of a `{..}` property map.
fn cypher_context(snapshot: &Snapshot, text: &str, offset: usize, word_start: usize) -> Option<Context> {
    let slot = pattern_slot(text, word_start)?;
    let header_end = slot.brace.unwrap_or(word_start);
    let (alias, types) = pattern_header(&text[slot.open + 1..header_end])?;
    if let Some(brace) = slot.brace {
        return Some(property_keys(snapshot, text, &slot, brace, word_start, alias, types.as_deref().unwrap_or(&[])));
    }
    types.as_ref()?;
    if !matches!(text[..word_start].trim_end().chars().next_back(), Some(':' | '|')) {
        return None;
    }
    if slot.bracket {
        return Some(bracket_edge_types(snapshot, text, offset, &slot));
    }
    Some(node_label_context(snapshot, text, &slot))
}

/// The edge types of `-[..|]-`: those that touch the node before the arrow.
fn bracket_edge_types(snapshot: &Snapshot, text: &str, offset: usize, slot: &PatternSlot) -> Context {
    let before = text[..slot.open].trim_end();
    let (before, incoming) = match before.strip_suffix("<-") {
        Some(rest) => (rest, true),
        None => (before.strip_suffix('-').unwrap_or(before), false),
    };
    let source = previous_node_types(snapshot, before, slot.open);
    if source.is_empty() || source.iter().any(|t| t == "*") {
        return Context::EdgeType;
    }
    let direction = if incoming {
        Direction::In
    } else {
        let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
        let after = text[offset..line_end].split_once(']').map(|(_, rest)| rest.trim_start());
        if after.is_some_and(|r| r.starts_with("->")) { Direction::Out } else { Direction::Either }
    };
    Context::EdgeStep { source, direction, used: Vec::new() }
}

/// The vertex types of the node pattern `(..)` that `before` ends with.
fn previous_node_types(snapshot: &Snapshot, before: &str, at: usize) -> Vec<String> {
    let Some(inner_end) = before.trim_end().strip_suffix(')').map(str::len) else {
        return Vec::new();
    };
    let Some(open) =
        (0..inner_end).rev().find(|&i| before.as_bytes()[i] == b'(' && matching_close(before, i) == inner_end)
    else {
        return Vec::new();
    };
    let Some((alias, types)) = pattern_header(&before[open + 1..inner_end]) else {
        return Vec::new();
    };
    match types {
        Some(types) if !types.is_empty() => {
            let mut out: Vec<String> = Vec::new();
            for name in types {
                // An unknown label makes the source unknown.
                let Some(found) = named_vertex_types(snapshot, name, at) else {
                    return Vec::new();
                };
                for t in found {
                    if !out.contains(&t) {
                        out.push(t);
                    }
                }
            }
            out
        }
        _ => match alias_type(snapshot, alias, at) {
            Some(Ty::Vertex(types)) => types,
            _ => Vec::new(),
        },
    }
}

/// The label slot of `(..:|)`: the types the preceding edge leads to, or any vertex type.
fn node_label_context(snapshot: &Snapshot, text: &str, slot: &PatternSlot) -> Context {
    let before = text[..slot.open].trim_end();
    let (before, arrow_out) = match before.strip_suffix("->") {
        Some(rest) => (rest, true),
        None => (before.strip_suffix('-').unwrap_or(before), false),
    };
    let Some(inner_end) = before.trim_end().strip_suffix(']').map(str::len) else {
        return Context::VertexSource;
    };
    let Some(open) =
        (0..inner_end).rev().find(|&i| before.as_bytes()[i] == b'[' && matching_close(before, i) == inner_end)
    else {
        return Context::VertexSource;
    };
    let incoming = before[..open].trim_end().ends_with("<-");
    let inner = &before[open + 1..inner_end];
    let Some((_, Some(types))) = pattern_header(inner) else {
        return Context::VertexSource;
    };
    target_types(snapshot, &types.join("|"), incoming, arrow_out).map_or(Context::VertexSource, Context::VertexTarget)
}

/// The key slot of a property map: attributes of the labelled type, or nothing when the
/// type is unknown or the cursor is in a value.
fn property_keys(
    snapshot: &Snapshot,
    text: &str,
    slot: &PatternSlot,
    brace: usize,
    word_start: usize,
    alias: &str,
    types: &[&str],
) -> Context {
    // Strings and comments are blanked: a comma or brace in them splits nothing.
    let code = code_only(text, word_start).0;
    let inside = &code[brace + 1..];
    let mut keys = Vec::new();
    let mut start = 0;
    for (index, byte) in inside.bytes().enumerate() {
        if byte == b',' {
            keys.push(&inside[start..index]);
            start = index + 1;
        }
    }
    // Only whitespace may precede the key being typed.
    if !inside.get(start..).is_some_and(|rest| rest.trim().is_empty()) {
        return Context::Nothing;
    }
    let used = keys.iter().filter_map(|k| k.split(':').next()).map(|k| k.trim().to_string()).collect();
    let (kind, wildcard) = if slot.bracket { (SymbolKind::EdgeType, "_") } else { (SymbolKind::VertexType, "ANY") };
    let mut owners: Vec<String> = types
        .iter()
        .filter(|t| **t != wildcard)
        .filter_map(|t| if slot.bracket { Some(vec![t.to_string()]) } else { named_vertex_types(snapshot, t, brace) })
        .flatten()
        .filter(|t| !snapshot.workspace.find(kind, t).is_empty())
        .collect();
    if types.is_empty() && !slot.bracket && !alias.is_empty() {
        // `(p {..})` of a node declared before: `(p:Person)-[]-(p {..})`.
        if let Some(Ty::Vertex(known)) = alias_type(snapshot, alias, brace) {
            owners = known;
        }
    }
    owners.dedup();
    if owners.is_empty() { Context::Nothing } else { Context::PropertyKeys { owners, used } }
}

/// The type of an alias: from the analysis, else from the FROM clause in the text.
fn alias_type(snapshot: &Snapshot, name: &str, offset: usize) -> Option<Ty> {
    let visible = snapshot.analysis.visible_symbols(offset);
    if let Some(symbol) = visible.iter().find(|s| s.name == name && s.kind == SymbolKind::Alias) {
        return Some(symbol.ty.clone());
    }
    text_aliases(snapshot, offset).into_iter().find(|(n, _)| n == name).map(|(_, ty)| ty)
}

/// Aliases bound by the FROM clause of the statement around `offset`, read from the text:
/// an unfinished clause is a syntax error, and its aliases are not in the analysis.
fn text_aliases(snapshot: &Snapshot, offset: usize) -> Vec<(String, Ty)> {
    let blanked = code_text(snapshot);
    let text = blanked;
    let floor = query_header_before(text, offset);
    let start = text[floor..offset].rfind(';').map_or(floor, |i| floor + i + 1);
    let lower = text[start..offset].to_ascii_lowercase();
    let from = lower.rmatch_indices("from").map(|(i, _)| i).find(|&i| {
        !lower[..i].chars().next_back().is_some_and(is_word_char)
            && !lower[i + 4..].chars().next().is_some_and(is_word_char)
    });
    let Some(from) = from else {
        return Vec::new();
    };
    let clause = &text[start + from + 4..offset];
    let bytes = clause.as_bytes();
    let mut out: Vec<(String, Ty)> = Vec::new();
    let mut add = |name: &str, ty: Ty| {
        if !name.is_empty() && !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), ty));
        }
    };
    let vertex_ty = |name: &str| Ty::Vertex(named_vertex_types(snapshot, name, offset).unwrap_or_default());
    let word_at = |from: usize| {
        let rest = &clause[from..];
        let skipped = rest.len() - rest.trim_start().len();
        let word: String = rest[skipped..].chars().take_while(|c| is_word_char(*c)).collect();
        (word, from + skipped)
    };
    let mut index = 0;
    while index < bytes.len() {
        let c = bytes[index];
        if c == b'(' || c == b'[' {
            let close = matching_close(clause, index);
            let inner = &clause[index + 1..close];
            let before = clause[..index].trim_end();
            if c == b'[' {
                if before.ends_with('-')
                    && let Some((alias, types)) = pattern_header(inner)
                {
                    add(alias, Ty::Edge(edge_names(types.unwrap_or_default().join("|").as_str())));
                }
            } else if starts_node(before) {
                if let Some((alias, types)) = pattern_header(inner) {
                    let mut names: Vec<String> = Vec::new();
                    for t in types.clone().unwrap_or_default() {
                        names.extend(named_vertex_types(snapshot, t, offset).unwrap_or_default());
                    }
                    // Untyped: the far ends of the edge before it.
                    if types.is_none() {
                        names = relationship_far_types(snapshot, before).unwrap_or_default();
                    }
                    names.dedup();
                    add(alias, Ty::Vertex(names));
                }
            } else if before.ends_with('-') && !before.ends_with("--") {
                // A V2 step `-(Type:alias)-`: the alias follows the last top-level colon.
                let mut depth = 0;
                let colon = inner.char_indices().rev().find_map(|(i, ch)| match ch {
                    ')' => {
                        depth += 1;
                        None
                    }
                    '(' => {
                        depth -= 1;
                        None
                    }
                    ':' if depth == 0 => Some(i),
                    _ => None,
                });
                if let Some(colon) = colon {
                    add(inner[colon + 1..].trim(), Ty::Edge(edge_names(&inner[..colon])));
                }
            }
            index = close + 1;
            continue;
        }
        if is_word_char(c as char) {
            let (word, _) = word_at(index);
            let end = index + word.len();
            if CLAUSE_WORDS.iter().any(|w| w.eq_ignore_ascii_case(&word)) {
                break;
            }
            let after = &clause[end..];
            if after.trim_start().starts_with(':') {
                let colon = end + (after.len() - after.trim_start().len());
                let (alias, alias_end) = word_at(colon + 1);
                add(&alias, vertex_ty(&word));
                index = alias_end + alias.len();
                continue;
            }
            index = end;
            continue;
        }
        if c == b':' {
            let (alias, alias_end) = word_at(index + 1);
            let far = match edge_target(snapshot, clause, index) {
                Some(Context::VertexTarget(mut types)) => {
                    types.sort();
                    types.dedup();
                    types
                }
                _ => Vec::new(),
            };
            add(&alias, Ty::Vertex(far));
            index = alias_end + alias.len();
            continue;
        }
        index += 1;
    }
    out
}

/// The vertex types on the far end of the relationship `-[e:A|B]->` that `before` ends
/// with, for the untyped node after it; none for repetitions and edges that are not decided.
fn relationship_far_types(snapshot: &Snapshot, before: &str) -> Option<Vec<String>> {
    let (rest, out) = match before.strip_suffix("->") {
        Some(rest) => (rest, true),
        None => (before.strip_suffix('-')?, false),
    };
    let rest = rest.trim_end().strip_suffix(']')?;
    let open = rest.rfind('[')?;
    let incoming = rest[..open].trim_end().ends_with("<-");
    let inner = &rest[open + 1..];
    if inner.contains('*') || inner.contains('{') {
        return None;
    }
    let (_, edges) = inner.split_once(':')?;
    target_types(snapshot, edges, incoming, out).map(|mut types| {
        types.sort();
        types.dedup();
        types
    })
}

/// The accumulators (`@@name` when `global`, else `@name`) declared at the top level of the
/// query body in `text[header..offset]`, with their declarations, read from the text.
fn text_accumulators(text: &str, header: usize, offset: usize, global: bool) -> Vec<(String, String, bool)> {
    let source = &text[header..offset];
    let bytes = source.as_bytes();
    let skip_space = |mut i: usize| {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let word_end = |from: usize| from + source[from..].find(|c: char| !is_word_char(c)).unwrap_or(source.len() - from);
    let mut out: Vec<(String, String, bool)> = Vec::new();
    let (mut depth, mut i) = (0i32, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'#' => i = source[i..].find('\n').map_or(bytes.len(), |n| i + n),
            b'/' if bytes.get(i + 1) == Some(&b'/') => i = source[i..].find('\n').map_or(bytes.len(), |n| i + n),
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = source[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 3)
            }
            b'{' => depth += 1,
            b'}' => depth -= 1,
            c if is_word_char(c as char) && (i == 0 || !is_word_char(bytes[i - 1] as char)) => {
                let end = word_end(i);
                if depth == 1 && source[i..end].to_ascii_lowercase().ends_with("accum") {
                    let mut j = skip_space(end);
                    for (open, close) in [(b'<', b'>'), (b'(', b')')] {
                        if bytes.get(j) == Some(&open) {
                            let mut nesting = 0;
                            while j < bytes.len() {
                                if bytes[j] == open {
                                    nesting += 1;
                                } else if bytes[j] == close {
                                    nesting -= 1;
                                    if nesting == 0 {
                                        break;
                                    }
                                }
                                j += 1;
                            }
                            // An unclosed bracket runs to the cursor.
                            j = skip_space((j + 1).min(bytes.len()));
                        }
                    }
                    let type_end = j;
                    // `SumAccum<INT> EDGE @w`: attached to edges.
                    let on_edges = source[j..].get(..4).is_some_and(|w| w.eq_ignore_ascii_case("edge"))
                        && !source[j + 4..].starts_with(is_word_char);
                    if on_edges {
                        j = skip_space(j + 4);
                    }
                    // `@@a, @@b`: every name up to the first initialiser.
                    loop {
                        let ats = bytes[j.min(bytes.len())..].iter().take_while(|&&b| b == b'@').count();
                        let name_end = word_end(j + ats);
                        if ats == 0 || ats > 2 || name_end == j + ats {
                            break;
                        }
                        if (ats == 2) == global {
                            let name = source[j..name_end].to_string();
                            if !out.iter().any(|(n, ..)| *n == name) {
                                let edge = if on_edges { " EDGE" } else { "" };
                                out.push((
                                    name,
                                    format!("{}{edge} {}", source[i..type_end].trim_end(), &source[j..name_end]),
                                    on_edges,
                                ));
                            }
                        }
                        let next = skip_space(name_end);
                        if bytes.get(next) != Some(&b',') {
                            j = name_end;
                            break;
                        }
                        j = skip_space(next + 1);
                    }
                    i = j.max(end);
                    continue;
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Words that end the FROM clause of a SELECT block.
const CLAUSE_WORDS: &[&str] = &["WHERE", "ACCUM", "POST_ACCUM", "SAMPLE", "GROUP", "HAVING", "ORDER", "LIMIT", "PER"];

/// The edge type names in `A>|<B|(C>)`; none when any edge may match (`_`, `ANY`).
fn edge_names(text: &str) -> Vec<String> {
    let words: Vec<&str> = text
        .split(|c: char| !is_word_char(c))
        .filter(|w| w.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
        .collect();
    if words.iter().any(|w| *w == "_" || w.eq_ignore_ascii_case("any")) {
        return Vec::new();
    }
    words.into_iter().map(str::to_string).collect()
}

/// Attribute lists of ALTER and INSERT, and the keywords after `ALTER VERTEX name`.
fn schema_lists(text: &str, word_start: usize, tokens: &[String]) -> Option<Context> {
    let t = |i: usize| tokens.get(i).map(String::as_str).unwrap_or("");
    let is_kind = |s: &str| s == "VERTEX" || s == "EDGE";
    if t(0) == "(" || t(0) == "," {
        let open = enclosing_parenthesis(text, word_start)?;
        let p = raw_tokens(text, open, 6);
        let upper = |i: usize| p.get(i).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
        let name = |i: usize| p.get(i).cloned().unwrap_or_default();
        let (columns, drop, owner) =
            if upper(0) == "ATTRIBUTE" && upper(1) == "DROP" && is_kind(&upper(3)) && upper(4) == "ALTER" {
                (false, true, name(2))
            } else if upper(0) == "ON" && upper(2) == "INDEX" && upper(3) == "ADD" && is_kind(&upper(5)) {
                (false, false, name(4))
            } else if (upper(1) == "INTO" && upper(2) == "INSERT")
                || (is_kind(&upper(1)) && upper(2) == "INTO" && upper(3) == "INSERT")
            {
                (true, false, name(0))
            } else {
                return None;
            };
        let used = listed_names(text, open, word_start);
        return Some(Context::Attributes { owner, columns, drop, used });
    }
    if tokens.len() >= 3 && t(2) == "ALTER" && is_kind(t(1)) && ends_operand(t(0)) {
        return Some(Context::Keywords(vec!["ADD", "DROP", "WITH"]));
    }
    if tokens.len() >= 4 && (t(0) == "ADD" || t(0) == "DROP") && is_kind(t(2)) && t(3) == "ALTER" {
        let mut words = vec!["ATTRIBUTE", "INDEX", "VECTOR ATTRIBUTE"];
        if t(2) == "EDGE" {
            words.push("PAIR");
            if t(0) == "ADD" {
                words.extend(["FROM", "TO"]);
            }
        }
        return Some(Context::Keywords(words));
    }
    None
}

/// The parts of `text` between the commas that are not inside brackets or strings.
fn top_level_segments(text: &str) -> Vec<&str> {
    let (mut depth, mut start, mut in_string, mut escaped) = (0i32, 0, false, false);
    let mut parts = Vec::new();
    for (index, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            ',' if depth <= 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The names already written in the list that opens at `open` and is cut at `end`.
fn listed_names(text: &str, open: usize, end: usize) -> Vec<String> {
    let segments = top_level_segments(&text[open + 1..end]);
    let done = &segments[..segments.len() - 1];
    done.iter()
        .filter_map(|s| s.split_whitespace().next())
        .filter(|w| w.chars().all(is_word_char))
        .map(str::to_string)
        .collect()
}

/// Positions in the parenthesised lists of schema definitions: types of attributes,
/// the endpoints of edges, the members of a graph and the endpoint types of INSERT.
fn definition_lists(snapshot: &Snapshot, text: &str, word_start: usize) -> Option<Context> {
    #[derive(PartialEq)]
    enum List {
        Vertex,
        Edge,
        Pairs,
        Graph,
        Attributes,
        Insert,
    }
    let open = enclosing_parenthesis(text, word_start)?;
    let p = raw_tokens(text, open, 8);
    let up = |i: usize| p.get(i).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    let is_kind = |s: &str| s == "VERTEX" || s == "EDGE";
    let list = if up(0) == "VALUES" {
        return insert_values(snapshot, text, open, word_start);
    } else if (up(0) == "ATTRIBUTE" && up(1) == "ADD") || up(0) == "DISCRIMINATOR" {
        List::Attributes
    } else if up(0) == "PAIR" && up(1) == "ADD" {
        List::Pairs
    } else if up(1) == "GRAPH" && up(2) == "CREATE" {
        List::Graph
    } else if is_kind(&up(1)) {
        let mut i = 2;
        while matches!(up(i).as_str(), "DIRECTED" | "UNDIRECTED" | "VIRTUAL" | "GLOBAL") {
            i += 1;
        }
        match up(i).as_str() {
            "CREATE" | "ADD" if up(1) == "VERTEX" => List::Vertex,
            "CREATE" | "ADD" => List::Edge,
            "INTO" if up(i + 1) == "INSERT" => List::Insert,
            _ => return None,
        }
    } else if up(1) == "INTO" && up(2) == "INSERT" {
        List::Insert
    } else {
        return None;
    };
    let segments = top_level_segments(&text[open + 1..word_start]);
    let current = segments.last().copied().unwrap_or("");
    let words: Vec<String> = current.split_whitespace().map(str::to_ascii_uppercase).collect();
    let word = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let piped = current.trim_end().ends_with('|');
    let endpoint_slot = matches!(word(0), "FROM" | "TO") && (words.len() == 1 || piped);
    match list {
        List::Insert => {
            if !endpoint_slot {
                return None;
            }
            let edge = snapshot.workspace.find(SymbolKind::EdgeType, &p[0]).into_iter().next()?;
            let ends = if word(0) == "FROM" { &edge.ends.from } else { &edge.ends.to };
            Some(endpoint_types(ends))
        }
        List::Graph if words.is_empty() => Some(Context::GraphMembers(listed_names(text, open, word_start))),
        List::Graph => Some(Context::Nothing),
        List::Edge | List::Pairs if words.is_empty() => {
            let starts = |s: &&str| s.split_whitespace().next().map(str::to_ascii_uppercase);
            let earlier = &segments[..segments.len() - 1];
            let from = earlier.iter().any(|s| starts(s).as_deref() == Some("FROM"));
            let to = earlier.iter().any(|s| starts(s).as_deref() == Some("TO"));
            Some(match (from, to) {
                (false, _) => Context::Keywords(vec!["FROM"]),
                (true, false) => Context::Keywords(vec!["TO"]),
                _ => Context::Nothing,
            })
        }
        List::Edge | List::Pairs if matches!(word(0), "FROM" | "TO") => {
            Some(if endpoint_slot { Context::VertexType } else { Context::Nothing })
        }
        List::Pairs => Some(Context::Nothing),
        List::Vertex | List::Edge | List::Attributes => {
            // Inside `MAP<..>` a type follows `<` and `,`; otherwise it follows `name` or `PRIMARY_ID name`.
            let angles = current.matches('<').count() > current.matches('>').count();
            let last = previous_tokens(text, word_start, 1);
            let last = last.first().map(String::as_str).unwrap_or("");
            let type_slot = if angles {
                last == "<" || last == ","
            } else {
                (words.len() == 1 && word(0) != "PRIMARY_ID") || (words.len() == 2 && word(0) == "PRIMARY_ID")
            };
            let primary_slot = !angles && words.len() == 2 && word(0) == "PRIMARY_ID";
            Some(if primary_slot {
                Context::ScalarType
            } else if type_slot {
                Context::AttributeType
            } else {
                Context::Nothing
            })
        }
    }
}

/// The endpoint types in an edge definition; any vertex type when they are not known.
fn endpoint_types(ends: &[String]) -> Context {
    Context::Endpoint(if ends.iter().any(|t| t == "*") { Vec::new() } else { ends.to_vec() })
}

/// The vertex type after an id in `INSERT INTO e (..) VALUES (a Person, b City, ..)`.
fn insert_values(snapshot: &Snapshot, text: &str, open: usize, word_start: usize) -> Option<Context> {
    let lower = text[..open].to_ascii_lowercase();
    let start = lower.rfind("insert into")?;
    let head = text[start + "insert into".len()..open].trim();
    if head.contains(';') {
        return None;
    }
    let head = head.get(..head.len().checked_sub("values".len())?)?.trim_end();
    let mut words = head.split(|c: char| !is_word_char(c)).filter(|w| !w.is_empty());
    let mut name = words.next()?;
    if name.eq_ignore_ascii_case("edge") || name.eq_ignore_ascii_case("vertex") {
        name = words.next()?;
    }
    let edge = snapshot.workspace.find(SymbolKind::EdgeType, name).into_iter().next()?;
    let segments = top_level_segments(&text[open + 1..word_start]);
    let current: Vec<&str> = segments.last()?.split_whitespace().collect();
    if current.len() != 1 || !current[0].chars().all(is_word_char) {
        return None;
    }
    match segments.len() {
        1 => Some(endpoint_types(&edge.ends.from)),
        2 => Some(endpoint_types(&edge.ends.to)),
        _ => None,
    }
}

/// The job and the file variables already given in `RUN LOADING JOB job USING ..` around `offset`.
fn run_loading_job(text: &str, offset: usize) -> Option<(String, Vec<String>)> {
    let lower = text[..offset].to_ascii_lowercase();
    let at = lower.rfind("loading job")?;
    let run = lower[..at].trim_end().strip_suffix("run")?;
    if run.chars().next_back().is_some_and(is_word_char) {
        return None;
    }
    let tail = &text[at + "loading job".len()..offset];
    if tail.contains(';') || tail.contains('{') {
        return None;
    }
    // The statement may go on over lines after a comma or before USING.
    let lines: Vec<&str> = tail.split('\n').collect();
    for pair in lines.windows(2) {
        let (line, next) = (pair[0].trim_end().to_ascii_lowercase(), pair[1].trim_start().to_ascii_lowercase());
        if !(line.ends_with(',') || line.ends_with("using") || next.starts_with("using")) {
            return None;
        }
    }
    let using = tail.to_ascii_lowercase().find("using")?;
    let (header, options) = (&tail[..using], &tail[using + "using".len()..]);
    let is_name = |w: &&str| !w.starts_with('-') && w.chars().all(is_word_char) && w.parse::<f64>().is_err();
    let job = header.split_whitespace().find(is_name)?;
    let segments = top_level_segments(options);
    let used = segments[..segments.len() - 1]
        .iter()
        .filter_map(|s| s.split('=').next())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    Some((job.to_string(), used))
}

/// The type of a method call such as `@@heap.top()` that ends right before the `.` at `dot`,
/// or of a parenthesised operand such as `(@@l.get(0))`.
fn call_result(snapshot: &Snapshot, dot: usize) -> MemberOf {
    let blanked = code_text(snapshot);
    let text = blanked;
    let code = code_only(text, dot).0;
    let Some(inner) = code.trim_end().strip_suffix(')') else {
        return MemberOf::Nothing;
    };
    let mut depth = 0;
    let open = inner.char_indices().rev().find_map(|(i, c)| match c {
        ')' => {
            depth += 1;
            None
        }
        '(' if depth == 0 => Some(i),
        '(' => {
            depth -= 1;
            None
        }
        _ => None,
    });
    let Some(open) = open else {
        return MemberOf::Nothing;
    };
    let head = inner[..open].trim_end();
    let method = trailing_word(head);
    let Some(receiver) = head[..head.len() - method.len()].trim_end().strip_suffix('.') else {
        return grouped_operand(snapshot, &inner[open + 1..], open + 1, head, method);
    };
    let MemberOf::Value(ty) = member_type(snapshot, receiver.len()) else {
        return MemberOf::Nothing;
    };
    match ty.method_result(method) {
        Ty::Unknown => MemberOf::Nothing,
        result => MemberOf::Value(result),
    }
}

/// The type of `(content)` when the parenthesis groups a single operand (`head` is what
/// precedes it, `method` its trailing word) and the operand's type is known.
fn grouped_operand(snapshot: &Snapshot, content: &str, content_start: usize, head: &str, method: &str) -> MemberOf {
    let grouping = if method.is_empty() {
        head.is_empty() || head.ends_with(['(', ',', '=', ';', '{'])
    } else {
        OPEN_WORDS.contains(&method.to_ascii_uppercase().as_str())
    };
    if !grouping {
        return MemberOf::Nothing;
    }
    // One operand: a chain of words and calls, without operators or a second word.
    let mut flat = String::new();
    let (mut depth, mut pending_space) = (0, false);
    for c in content.trim().chars() {
        match c {
            '(' => {
                depth += 1;
                if depth == 1 {
                    flat.push('(');
                }
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    flat.push(')');
                }
            }
            _ if depth > 0 => {}
            c if c.is_whitespace() => pending_space = true,
            c if is_word_char(c) || c == '@' => {
                if pending_space && flat.ends_with(|p: char| is_word_char(p) || p == '@' || p == ')') {
                    return MemberOf::Nothing;
                }
                pending_space = false;
                flat.push(c);
            }
            '.' => {
                pending_space = false;
                flat.push(c);
            }
            _ => return MemberOf::Nothing,
        }
    }
    if depth != 0 || flat.is_empty() {
        return MemberOf::Nothing;
    }
    match member_type(snapshot, content_start + content.trim_end().len()) {
        MemberOf::Value(Ty::Unknown) => MemberOf::Nothing,
        other => other,
    }
}

/// The type of the expression that ends right before the `.` at `dot`.
fn member_type(snapshot: &Snapshot, dot: usize) -> MemberOf {
    let blanked = code_text(snapshot);
    let text = blanked;
    let analysis = snapshot.analysis;
    let dot = code_only(text, dot).0.trim_end().len();
    let start = text[..dot]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word_char(*c) || *c == '@')
        .last()
        .map(|(i, _)| i)
        .unwrap_or(dot);
    let object = &text[start..dot];
    if object.is_empty() {
        return call_result(snapshot, dot);
    }
    // An attribute of something (`a.b.`): its type is not tracked.
    if !object.starts_with('@') && code_only(text, start).0.trim_end().ends_with('.') {
        return MemberOf::Nothing;
    }
    let visible = analysis.visible_symbols(dot);
    let find = |name: &str, kinds: &[SymbolKind]| {
        visible.iter().find(|s| s.name == name && kinds.contains(&s.kind)).map(|s| s.ty.clone())
    };
    if object.starts_with("@@") {
        return MemberOf::Value(find(object, &[SymbolKind::GlobalAccumulator]).unwrap_or_default());
    }
    if object.starts_with('@') {
        return MemberOf::Value(find(object, &[SymbolKind::LocalAccumulator]).unwrap_or_default());
    }
    let locals = [
        SymbolKind::Parameter,
        SymbolKind::Variable,
        SymbolKind::VertexSet,
        SymbolKind::Alias,
        SymbolKind::LoopVariable,
        SymbolKind::File,
    ];
    if let Some(ty) = find(object, &locals) {
        return MemberOf::Value(ty);
    }
    if let Some(ty) = unfinished_foreach_variable(snapshot, object, dot) {
        return MemberOf::Value(ty);
    }
    if let Some((_, ty)) = text_aliases(snapshot, dot).into_iter().find(|(name, _)| name == object) {
        return MemberOf::Value(ty);
    }
    if !snapshot.workspace.find(SymbolKind::VertexType, object).is_empty() {
        return MemberOf::VertexTypeName;
    }
    MemberOf::Value(Ty::Unknown)
}

/// The type of `name` as bound by `FOREACH name IN collection DO` before `offset`, read
/// from the text: a loop that is not closed yet is not in the analysis.
fn unfinished_foreach_variable(snapshot: &Snapshot, name: &str, offset: usize) -> Option<Ty> {
    let blanked = code_text(snapshot);
    let text = blanked;
    let header = query_header_before(text, offset);
    // Comments and strings are blanked: a `FOREACH` in a comment binds nothing.
    let code = code_only(text, offset).0;
    let lower = code[header..].to_ascii_lowercase();
    let mut end = lower.len();
    while let Some(index) = lower[..end].rfind("foreach") {
        end = index;
        if lower[..index].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let body = header + index + "foreach".len();
        let mut words = code[body..].split_whitespace();
        let (variable, keyword, collection) = (words.next()?, words.next()?, words.next()?);
        if variable != name || !(keyword.eq_ignore_ascii_case("in") || keyword == ":") || block_closed(&code[body..]) {
            continue;
        }
        let kinds = [
            SymbolKind::Parameter,
            SymbolKind::Variable,
            SymbolKind::VertexSet,
            SymbolKind::GlobalAccumulator,
            SymbolKind::LocalAccumulator,
        ];
        let ty = snapshot
            .analysis
            .visible_symbols(offset)
            .iter()
            .find(|s| s.name == collection && kinds.contains(&s.kind))
            .map(|s| s.ty.element())?;
        return (ty != Ty::Unknown).then_some(ty);
    }
    None
}

/// Whether the block whose body is `code` (comments blanked) has met its `END`: the one
/// that is not matched by an IF, WHILE, FOREACH, CASE or TRY inside the body (`ELSE IF`
/// continues an IF).
fn block_closed(code: &str) -> bool {
    let (mut depth, mut previous) = (0, String::new());
    for word in code.split(|c: char| !is_word_char(c)).filter(|w| !w.is_empty()) {
        let word = word.to_ascii_uppercase();
        match word.as_str() {
            "IF" if previous == "ELSE" => {}
            "IF" | "WHILE" | "FOREACH" | "CASE" | "TRY" => depth += 1,
            "END" if depth == 0 => return true,
            "END" => depth -= 1,
            _ => {}
        }
        previous = word;
    }
    false
}

struct Builder<'a> {
    snapshot: &'a Snapshot<'a>,
    offset: usize,
    snippets: bool,
    items: Vec<CompletionItem>,
    seen: HashSet<(String, u8)>,
    /// Range to replace (covers a typed `@`/`@@` prefix).
    replace: Option<Span>,
    /// The clauses a SELECT block may continue with are already offered.
    clauses_listed: bool,
}

const BASE_TYPES: &[&str] = &[
    "INT",
    "UINT",
    "FLOAT",
    "DOUBLE",
    "BOOL",
    "STRING",
    "DATETIME",
    "VERTEX",
    "EDGE",
    "JSONOBJECT",
    "JSONARRAY",
    "LIST",
    "SET",
    "BAG",
    "MAP",
];

/// The leading entries of `BASE_TYPES` that can be the type of a tuple field or an attribute.
const SCALAR_TYPES: usize = 7;

const SHOW_KINDS: &[&str] = &[
    "VERTEX",
    "EDGE",
    "GRAPH",
    "QUERY",
    "JOB",
    "PACKAGE",
    "USER",
    "ROLE",
    "SECRET",
    "TOKEN",
    "DATA_SOURCE",
    "TAG",
    "PRIVILEGE",
    "GROUP",
    "FUNCTION",
    "SCHEMA",
];

const RUN_OPTIONS: &[(&str, &str)] = &[
    ("CONCURRENCY", "Number of concurrent loading threads: `CONCURRENCY=4`."),
    ("BATCH_SIZE", "Number of lines per batch: `BATCH_SIZE=10000`."),
    ("EOF", "End of the input: `EOF=\"true\"`."),
];

const STATEMENT_KEYWORDS: &[&str] = &[
    "SELECT",
    "IF",
    "WHILE",
    "FOREACH",
    "CASE",
    "PRINT",
    "RETURN",
    "BREAK",
    "CONTINUE",
    "INSERT INTO",
    "DELETE",
    "UPDATE",
    "TYPEDEF TUPLE",
    "RAISE",
    "TRY",
    "LOG",
    "FILE",
    "EXCEPTION",
];

const EXPRESSION_KEYWORDS: &[&str] = &[
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS NULL",
    "IS NOT NULL",
    "LIKE",
    "BETWEEN",
    "TRUE",
    "FALSE",
    "NULL",
    "CASE",
    "UNION",
    "INTERSECT",
    "MINUS",
    "INTERVAL",
    "RANGE",
];

const ORDER_BY_OPERATORS: &[&str] = &["AND", "OR", "NOT", "IN", "IS NULL", "IS NOT NULL", "LIKE", "BETWEEN"];

const SELECT_CLAUSES: &[&str] =
    &["FROM", "WHERE", "ACCUM", "POST-ACCUM", "PER", "GROUP BY", "HAVING", "ORDER BY", "LIMIT", "SAMPLE"];

const TOP_LEVEL_KEYWORDS: &[&str] = &[
    "CREATE",
    "DROP",
    "SHOW",
    "LS",
    "USE GRAPH",
    "USE GLOBAL",
    "INSTALL QUERY",
    "RUN QUERY",
    "RUN LOADING JOB",
    "RUN SCHEMA_CHANGE JOB",
    "RUN GLOBAL SCHEMA_CHANGE JOB",
    "INTERPRET QUERY",
    "GRANT",
    "REVOKE",
    "BEGIN",
    "END",
    "ABORT",
    "EXPORT GRAPH",
    "IMPORT GRAPH",
    "CLEAR GRAPH STORE",
    "TYPEDEF TUPLE",
];

const USING_OPTIONS: &[(&str, &str)] = &[
    ("SEPARATOR", "Column separator, e.g. `SEPARATOR=\",\"`."),
    ("HEADER", "Whether the first line is a header: `HEADER=\"true\"`."),
    ("EOL", "End-of-line character, e.g. `EOL=\"\\n\"`."),
    ("QUOTE", "Quote style: `QUOTE=\"double\"` or `\"single\"`."),
    ("USER_DEFINED_HEADER", "Use a header declared with DEFINE HEADER."),
    ("REJECT_LINE_RULE", "Skip lines matching a DEFINE INPUT_LINE_FILTER."),
    ("JSON_FILE", "Input lines are JSON objects: `JSON_FILE=\"true\"`."),
];

const WITH_OPTIONS: &[(&str, &str)] = &[
    ("primary_id_as_attribute", "Also store the primary id as an attribute: `primary_id_as_attribute=\"true\"`."),
    ("STATS", "Degree statistics: `STATS=\"OUTDEGREE_BY_EDGETYPE\"` or `\"NONE\"`."),
    ("REVERSE_EDGE", "Name of the reverse edge type of a directed edge."),
];

/// Whether the edge type is the reverse edge another one declares with `REVERSE_EDGE`.
fn is_reverse_edge(symbol: &GlobalSymbol) -> bool {
    symbol.detail.starts_with("reverse edge of ")
}

impl Builder<'_> {
    fn push(&mut self, mut item: CompletionItem) {
        if !self.seen.insert((item.label.clone(), item.kind.unwrap_or_default())) {
            return;
        }
        if let Some(span) = self.replace {
            let range = Range::new(self.snapshot.position(span.start), self.snapshot.position(span.end));
            let new_text = item.insert_text.take().unwrap_or_else(|| item.label.clone());
            item.filter_text.get_or_insert_with(|| item.label.clone());
            item.text_edit = Some(TextEdit { range, new_text });
        }
        self.items.push(item);
    }

    fn type_name(&mut self, name: &str) {
        let doc = builtins::primitive_type(name).unwrap_or("");
        self.push(CompletionItem::new(name, kind::TYPE_PARAMETER).documentation(doc).sort(format!("3{name}")));
    }

    fn keyword(&mut self, keyword: &str, sort: &str) {
        let doc = builtins::keyword(keyword.split_whitespace().next().unwrap_or(keyword)).unwrap_or("");
        self.push(CompletionItem::new(keyword, kind::KEYWORD).documentation(doc).sort(format!("{sort}{keyword}")));
    }

    fn snippet(&mut self, label: &str, body: &str, detail: &str) {
        if !self.snippets {
            return;
        }
        self.push(CompletionItem::new(label, kind::SNIPPET).detail(detail).snippet(body).sort(format!("5{label}")));
    }

    fn fill(&mut self, context: &Context) {
        match context {
            Context::Member { ty, local_only } => self.members(ty, *local_only),
            Context::GlobalAccumulator => self.accumulators(SymbolKind::GlobalAccumulator, None),
            Context::LocalAccumulator => self.accumulators(SymbolKind::LocalAccumulator, None),
            Context::VertexType => self.schema(&[SymbolKind::VertexType]),
            Context::EdgeType => {
                self.schema(&[SymbolKind::EdgeType]);
                self.locals(&[SymbolKind::Parameter, SymbolKind::Variable]);
                self.accumulators(SymbolKind::GlobalAccumulator, None);
                self.keyword("ANY", "4");
            }
            Context::EdgeStep { source, direction, used } => {
                let fits = |s: &GlobalSymbol| {
                    if used.contains(&s.name) {
                        return false;
                    }
                    let ends = &s.ends;
                    let touches = |side: &[String]| side.iter().any(|t| t == "*" || source.contains(t));
                    if ends.from.is_empty() && ends.to.is_empty() {
                        return true;
                    }
                    match direction {
                        Direction::Out if ends.directed => touches(&ends.from),
                        Direction::In if ends.directed => touches(&ends.to),
                        _ => touches(&ends.from) || touches(&ends.to),
                    }
                };
                self.schema_where(&[SymbolKind::EdgeType], &fits);
                self.locals(&[SymbolKind::Parameter, SymbolKind::Variable]);
                self.accumulators(SymbolKind::GlobalAccumulator, None);
                self.keyword("ANY", "4");
            }
            Context::VertexTarget(types) => {
                self.locals(&[SymbolKind::VertexSet, SymbolKind::Parameter]);
                self.schema_where(&[SymbolKind::VertexType], &|s| types.contains(&s.name));
                self.accumulators(SymbolKind::GlobalAccumulator, None);
                self.keyword("ANY", "4");
            }
            Context::SelectClauses { stage, only } => {
                let blanked = code_text(self.snapshot);
                let text = blanked;
                let v1 = declares_syntax_v1(text, self.offset);
                let last = trailing_word(&text[..word_start(text, self.offset)]);
                if *stage == 7 && !last.eq_ignore_ascii_case("ASC") && !last.eq_ignore_ascii_case("DESC") {
                    self.keyword("ASC", "0");
                    self.keyword("DESC", "0");
                }
                for clause in clauses_after(*stage, v1) {
                    self.keyword(clause, "0");
                }
                if *stage == 8 && !limit_has_offset(text, self.offset) {
                    self.keyword("OFFSET", "0");
                }
                self.clauses_listed = true;
                if *stage == 7 && !only {
                    // After a sort key only an operator or the next clause fits.
                    for operator in ORDER_BY_OPERATORS {
                        self.keyword(operator, "4");
                    }
                } else if !only {
                    self.expression();
                }
            }
            Context::ScalarType => {
                for name in &BASE_TYPES[..SCALAR_TYPES] {
                    self.type_name(name);
                }
            }
            Context::AttributeType => {
                for name in &BASE_TYPES[..SCALAR_TYPES] {
                    self.type_name(name);
                }
                for name in ["STRING COMPRESS", "FIXED_BINARY", "LIST", "SET", "MAP"] {
                    self.type_name(name);
                }
                self.schema(&[SymbolKind::TupleType]);
            }
            Context::Attributes { owner, columns, drop, used } => {
                let is_used = |name: &str| used.iter().any(|u| u == name);
                let primary_used = used.iter().any(|u| u.eq_ignore_ascii_case("PRIMARY_ID"));
                for attribute in self.snapshot.workspace.attributes(Some(owner)) {
                    // The primary id is written as PRIMARY_ID or by its name, once.
                    let primary = attribute.detail.to_ascii_uppercase().starts_with("PRIMARY_ID");
                    if is_used(&attribute.name) || (primary && (*drop || (*columns && primary_used))) {
                        continue;
                    }
                    self.push(
                        CompletionItem::new(&attribute.name, kind::FIELD)
                            .detail(attribute.detail.clone())
                            .documentation(attribute.doc.clone().unwrap_or_default())
                            .sort(format!("0{}", attribute.name)),
                    );
                }
                if *columns {
                    let named_primary = self
                        .snapshot
                        .workspace
                        .attributes(Some(owner))
                        .iter()
                        .any(|a| a.detail.to_ascii_uppercase().starts_with("PRIMARY_ID") && is_used(&a.name));
                    // (Only a resolved type says which special columns exist.)
                    if !self.snapshot.workspace.find(SymbolKind::EdgeType, owner).is_empty() {
                        for word in ["FROM", "TO"] {
                            if !used.iter().any(|u| u.eq_ignore_ascii_case(word)) {
                                self.keyword(word, "1");
                            }
                        }
                    } else if !self.snapshot.workspace.find(SymbolKind::VertexType, owner).is_empty()
                        && !primary_used
                        && !named_primary
                    {
                        self.keyword("PRIMARY_ID", "1");
                    }
                }
            }
            Context::PropertyKeys { owners, used } => {
                for owner in owners {
                    for attribute in self.snapshot.workspace.attributes(Some(owner)) {
                        if used.contains(&attribute.name) {
                            continue;
                        }
                        self.push(
                            CompletionItem::new(&attribute.name, kind::FIELD)
                                .detail(format!("{owner}.{}", attribute.detail))
                                .documentation(attribute.doc.clone().unwrap_or_default())
                                .sort(format!("0{}", attribute.name)),
                        );
                    }
                }
            }
            Context::Keywords(words) => {
                for word in words {
                    self.keyword(word, "4");
                }
            }
            Context::SchemaType => {
                self.schema(&[SymbolKind::VertexType]);
                self.schema_where(&[SymbolKind::EdgeType], &|s| !is_reverse_edge(s));
            }
            Context::InsertEdge => {
                self.schema_where(&[SymbolKind::EdgeType], &|s| !is_reverse_edge(s));
                self.locals(&[SymbolKind::Parameter, SymbolKind::Variable]);
            }
            Context::Endpoint(types) => {
                self.schema_where(&[SymbolKind::VertexType], &|s| types.is_empty() || types.contains(&s.name));
            }
            Context::GraphMembers(used) => {
                self.schema_where(&[SymbolKind::VertexType], &|s| !used.contains(&s.name));
                self.schema_where(&[SymbolKind::EdgeType], &|s| !is_reverse_edge(s) && !used.contains(&s.name));
            }
            Context::RunOption { job, used } => {
                let mut files: Vec<String> = self
                    .snapshot
                    .workspace
                    .find(SymbolKind::LoadingJob, job)
                    .iter()
                    .flat_map(|s| s.members.clone())
                    .collect();
                files.dedup();
                for file in files.iter().filter(|f| !used.contains(f)) {
                    self.push(
                        CompletionItem::new(file, kind::FILE)
                            .detail(format!("file variable of {job}"))
                            .insert(format!("{file}=")),
                    );
                }
                for (option, doc) in RUN_OPTIONS.iter().filter(|(o, _)| !used.iter().any(|u| u.eq_ignore_ascii_case(o)))
                {
                    self.push(
                        CompletionItem::new(*option, kind::PROPERTY).documentation(*doc).insert(format!("{option}=")),
                    );
                }
            }
            Context::HeaderNames => self.locals(&[SymbolKind::Header]),
            Context::TempTables => self.locals(&[SymbolKind::TempTable]),
            Context::Columns(names) => {
                for name in names {
                    self.push(CompletionItem::new(name, kind::FIELD).detail("header column"));
                }
            }
            Context::VertexSource => {
                self.locals(&[SymbolKind::VertexSet, SymbolKind::Parameter]);
                self.schema(&[SymbolKind::VertexType]);
                self.accumulators(SymbolKind::GlobalAccumulator, None);
                self.keyword("ANY", "4");
            }
            Context::Graph => self.schema(&[SymbolKind::Graph]),
            Context::Query { all } => {
                self.schema(&[SymbolKind::Query]);
                if *all {
                    self.keyword("ALL", "4");
                }
            }
            Context::Job { kinds, all } => {
                self.schema(kinds);
                if *all {
                    self.keyword("ALL", "4");
                }
            }
            Context::LoadSource => {
                self.locals(&[SymbolKind::FilenameVariable, SymbolKind::TempTable]);
                self.keyword("TEMP_TABLE", "4");
            }
            Context::Type => self.types(),
            Context::Statement => self.statement(),
            Context::DmlStatement => {
                self.expression();
                for keyword in ["IF", "FOREACH", "WHILE", "CASE", "INSERT INTO", "DELETE", "BREAK", "CONTINUE"] {
                    self.keyword(keyword, "4");
                }
                self.types();
            }
            Context::TopLevel => self.top_level(),
            Context::LoadingStatement => self.loading_statement(),
            Context::SchemaChangeStatement => self.schema_change_statement(),
            Context::UsingOption => {
                for (option, doc) in USING_OPTIONS {
                    self.push(
                        CompletionItem::new(*option, kind::PROPERTY).documentation(*doc).insert(format!("{option}=")),
                    );
                }
                self.locals(&[SymbolKind::FilenameVariable]);
            }
            Context::WithOption => {
                for (option, doc) in WITH_OPTIONS {
                    self.push(
                        CompletionItem::new(*option, kind::PROPERTY).documentation(*doc).insert(format!("{option}=")),
                    );
                }
            }
            Context::Expression => self.expression(),
            Context::Nothing => {}
        }
    }

    fn members(&mut self, of: &MemberOf, local_only: bool) {
        let ty = match of {
            MemberOf::VertexTypeName => {
                self.push(CompletionItem::new("*", kind::KEYWORD).detail("all vertices of this type"));
                return;
            }
            MemberOf::Nothing => return,
            MemberOf::Value(ty) => ty.clone(),
        };
        // Accumulators attached to edges (`EDGE @w`) belong to edge aliases only.
        match ty {
            Ty::Unknown => self.accumulators(SymbolKind::LocalAccumulator, None),
            Ty::Vertex(_) | Ty::VertexSet(_) => self.accumulators(SymbolKind::LocalAccumulator, Some(false)),
            Ty::Edge(_) => self.accumulators(SymbolKind::LocalAccumulator, Some(true)),
            _ => {}
        }
        if local_only {
            return;
        }
        match &ty {
            Ty::Vertex(types) | Ty::VertexSet(types) | Ty::Edge(types) => {
                let attributes: Vec<_> = if types.is_empty() {
                    let wanted = if matches!(ty, Ty::Edge(_)) { SymbolKind::EdgeType } else { SymbolKind::VertexType };
                    let owners: HashSet<String> =
                        self.snapshot.workspace.of_kind(wanted).into_iter().map(|s| s.name.clone()).collect();
                    self.snapshot
                        .workspace
                        .attributes(None)
                        .into_iter()
                        .filter(|a| a.owner.as_ref().is_some_and(|o| owners.contains(o)))
                        .collect()
                } else {
                    types.iter().flat_map(|t| self.snapshot.workspace.attributes(Some(t))).collect()
                };
                for attribute in attributes {
                    let owner = attribute.owner.clone().unwrap_or_default();
                    self.push(
                        CompletionItem::new(&attribute.name, kind::FIELD)
                            .detail(format!("{owner}.{}", attribute.detail))
                            .documentation(attribute.doc.clone().unwrap_or_default())
                            .sort(format!("0{}", attribute.name)),
                    );
                }
                self.push(CompletionItem::new("type", kind::FIELD).detail("STRING: the type name").sort("1type"));
            }
            Ty::Tuple(tuple) => {
                let fields: Vec<(String, String)> = self
                    .snapshot
                    .analysis
                    .symbols
                    .iter()
                    .filter(|s| s.kind == SymbolKind::TupleField && s.owner.as_deref() == Some(tuple.as_str()))
                    .map(|s| (s.name.clone(), s.detail.clone()))
                    .chain(
                        self.snapshot
                            .workspace
                            .tuple_fields(tuple)
                            .into_iter()
                            .map(|s| (s.name.clone(), s.detail.clone())),
                    )
                    .collect();
                for (name, detail) in fields {
                    self.push(CompletionItem::new(name, kind::FIELD).detail(detail));
                }
            }
            Ty::Unknown => {
                for attribute in self.snapshot.workspace.attributes(None) {
                    self.push(
                        CompletionItem::new(&attribute.name, kind::FIELD)
                            .detail(format!("{}.{}", attribute.owner.clone().unwrap_or_default(), attribute.detail))
                            .sort(format!("0{}", attribute.name)),
                    );
                }
            }
            _ => {}
        }
        let methods = match &ty {
            Ty::Unknown => builtins::VERTEX_METHODS,
            other => methods_for(other),
        };
        for method in methods {
            self.method(method);
        }
        if let Ty::VertexSet(_) = ty {
            for method in builtins::VERTEX_SET_METHODS {
                self.method(method);
            }
        }
    }

    fn method(&mut self, method: &builtins::Method) {
        let mut item = CompletionItem::new(method.name, kind::METHOD)
            .detail(method.signature())
            .documentation(method.doc)
            .sort(format!("2{}", method.name));
        if self.snippets {
            item = if method.params.is_empty() {
                item.snippet(format!("{}()", method.name))
            } else {
                item.snippet(format!("{}($1)", method.name))
            };
        }
        self.push(item);
    }

    /// Accumulators declared in the enclosing query.
    /// `on_edges`: only the accumulators attached to edges (`Some(true)`) or only the
    /// others (`Some(false)`).
    fn accumulators(&mut self, accumulator_kind: SymbolKind, on_edges: Option<bool>) {
        let analysis = self.snapshot.analysis;
        let scope = analysis.scope_at(self.offset);
        // Accumulators are block-scoped: those of the enclosing blocks, up to
        // the query. In unfinished code (`IF @@`) the parser may have ended
        // the query before the cursor; then the latest query before it is used.
        let blanked = code_text(self.snapshot);
        let text = blanked;
        let header = query_header_before(text, self.offset);
        let mut extra: Vec<(String, String, bool)> = Vec::new();
        let symbols: Vec<_> = if analysis.query_scope(scope).is_some() {
            analysis.visible_symbols(self.offset).into_iter().filter(|s| s.kind == accumulator_kind).collect()
        } else {
            // Only a query that starts at or after the latest header is the one around the cursor.
            let latest = (0..analysis.scopes.len()).rev().find(|&s| {
                let span = analysis.scopes[s].span;
                analysis.scopes[s].kind == ScopeKind::Query && span.start <= self.offset && span.start >= header
            });
            let found: Vec<_> = match latest {
                Some(query) => analysis.scopes[query]
                    .symbols
                    .iter()
                    .map(|&id| &analysis.symbols[id])
                    .filter(|s| s.kind == accumulator_kind)
                    .collect(),
                // The whole query was swallowed by a syntax error (an unfinished
                // line in a long query): use what was declared since its header.
                None => analysis
                    .symbols
                    .iter()
                    .filter(|s| s.kind == accumulator_kind && s.span.start >= header && s.span.start < self.offset)
                    .collect(),
            };
            // Declarations the error swallowed are in no scope: read them from the text.
            extra = text_accumulators(text, header, self.offset, accumulator_kind == SymbolKind::GlobalAccumulator);
            extra.retain(|(name, ..)| !found.iter().any(|s| &s.name == name));
            found
        };
        for (name, detail, edge) in extra {
            if on_edges.is_some_and(|wanted| wanted != edge) {
                continue;
            }
            self.push(CompletionItem::new(&name, kind::VARIABLE).detail(detail).sort(format!("1{name}")));
        }
        for symbol in symbols.into_iter().filter(|s| on_edges.is_none_or(|wanted| wanted == s.on_edges)) {
            self.push(
                CompletionItem::new(&symbol.name, kind::VARIABLE)
                    .detail(symbol.detail.clone())
                    .documentation(symbol.doc.clone().unwrap_or_default())
                    .sort(format!("1{}", symbol.name)),
            );
        }
    }

    fn locals(&mut self, kinds: &[SymbolKind]) {
        let symbols: Vec<_> = self
            .snapshot
            .analysis
            .visible_symbols(self.offset)
            .into_iter()
            .filter(|s| kinds.contains(&s.kind))
            .cloned()
            .collect();
        for symbol in symbols {
            let item_kind = match symbol.kind {
                SymbolKind::File | SymbolKind::FilenameVariable => kind::FILE,
                SymbolKind::TupleType => kind::STRUCT,
                SymbolKind::AccumulatorType => kind::CLASS,
                _ => kind::VARIABLE,
            };
            self.push(
                CompletionItem::new(&symbol.name, item_kind)
                    .detail(symbol.detail.clone())
                    .documentation(symbol.doc.clone().unwrap_or_default())
                    .sort(format!("0{}", symbol.name)),
            );
        }
    }

    fn schema(&mut self, kinds: &[SymbolKind]) {
        self.schema_where(kinds, &|_| true);
    }

    fn schema_where(&mut self, kinds: &[SymbolKind], keep: &dyn Fn(&GlobalSymbol) -> bool) {
        for &symbol_kind in kinds {
            let item_kind = match symbol_kind {
                SymbolKind::VertexType => kind::CLASS,
                SymbolKind::EdgeType => kind::INTERFACE,
                SymbolKind::Graph => kind::MODULE,
                SymbolKind::Query | SymbolKind::LoadingJob | SymbolKind::SchemaChangeJob => kind::FUNCTION,
                SymbolKind::TupleType => kind::STRUCT,
                SymbolKind::AccumulatorType => kind::CLASS,
                _ => kind::VALUE,
            };
            for symbol in self.snapshot.workspace.of_kind(symbol_kind).into_iter().filter(|s| keep(s)) {
                self.push(
                    CompletionItem::new(&symbol.name, item_kind)
                        .detail(symbol.detail.clone())
                        .documentation(symbol.doc.clone().unwrap_or_default())
                        .sort(format!("2{}", symbol.name)),
                );
            }
        }
    }

    fn types(&mut self) {
        for name in BASE_TYPES {
            let doc = builtins::primitive_type(name).unwrap_or("");
            self.push(CompletionItem::new(*name, kind::TYPE_PARAMETER).documentation(doc).sort(format!("3{name}")));
        }
        for accumulator in builtins::ACCUMULATORS {
            let mut item = CompletionItem::new(accumulator.name, kind::CLASS)
                .detail(accumulator.syntax)
                .documentation(accumulator.doc)
                .sort(format!("3{}", accumulator.name));
            let takes_arguments = accumulator.syntax.contains('<');
            if self.snippets && takes_arguments {
                item = item.snippet(format!("{}<$1>", accumulator.name));
            }
            self.push(item);
        }
        self.locals(&[SymbolKind::TupleType, SymbolKind::AccumulatorType]);
        self.schema(&[SymbolKind::TupleType, SymbolKind::AccumulatorType]);
    }

    fn functions(&mut self) {
        for function in builtins::FUNCTIONS {
            if !function.in_queries() {
                continue;
            }
            let mut item = CompletionItem::new(function.name, kind::FUNCTION)
                .detail(function.signature())
                .documentation(function.doc)
                .sort(format!("3{}", function.name));
            if self.snippets {
                item = if function.params.is_empty() {
                    item.snippet(format!("{}()", function.name))
                } else {
                    item.snippet(format!("{}($1)", function.name))
                };
            }
            self.push(item);
        }
        for (name, doc) in builtins::CONSTANTS {
            self.push(CompletionItem::new(*name, kind::CONSTANT).documentation(*doc).sort(format!("3{name}")));
        }
    }

    fn loading_functions(&mut self) {
        for function in builtins::FUNCTIONS.iter().filter(|f| f.in_loading_jobs()) {
            let mut item = CompletionItem::new(function.name, kind::FUNCTION)
                .detail(function.signature())
                .documentation(function.doc)
                .sort(format!("3{}", function.name));
            if self.snippets {
                item = item.snippet(format!("{}($1)", function.name));
            }
            self.push(item);
        }
    }

    fn expression(&mut self) {
        let local_kinds = [
            SymbolKind::Parameter,
            SymbolKind::Variable,
            SymbolKind::VertexSet,
            SymbolKind::Alias,
            SymbolKind::LoopVariable,
            SymbolKind::File,
            SymbolKind::Table,
            SymbolKind::FilenameVariable,
            SymbolKind::Header,
            SymbolKind::LineFilter,
        ];
        self.locals(&local_kinds);
        for (name, ty) in text_aliases(self.snapshot, self.offset) {
            self.push(
                CompletionItem::new(&name, kind::VARIABLE)
                    .detail(format!("{name}: {}", ty.display()))
                    .sort(format!("0{name}")),
            );
        }
        self.accumulators(SymbolKind::GlobalAccumulator, None);
        let in_query = self.snapshot.analysis.query_scope(self.snapshot.analysis.scope_at(self.offset)).is_some()
            || in_unclosed_query_body(code_text(self.snapshot), self.offset);
        if in_query {
            self.functions();
            self.schema(&[SymbolKind::Query]);
            self.locals(&[SymbolKind::TupleType]);
            self.schema(&[SymbolKind::TupleType]);
        } else {
            self.loading_functions();
        }
        for keyword in EXPRESSION_KEYWORDS {
            self.keyword(keyword, "4");
        }
        let in_select = ancestors_at(self.snapshot, self.offset).iter().any(|n| n.kind() == "select_statement");
        if in_select && !self.clauses_listed {
            for clause in SELECT_CLAUSES {
                self.keyword(clause, "4");
            }
        }
    }

    fn statement(&mut self) {
        self.expression();
        self.types();
        for keyword in STATEMENT_KEYWORDS {
            self.keyword(keyword, "4");
        }
        self.snippet(
            "SELECT block",
            "${1:Result} = SELECT ${2:t}\n    FROM ${3:Start}:${4:s} -(${5:Edge}:${6:e})- ${7:Target}:${2:t}\n    WHERE ${8:true}\n    ACCUM ${0};",
            "SELECT ... FROM ... WHERE ... ACCUM ...",
        );
        self.snippet("IF", "IF ${1:condition} THEN\n    $0\nEND;", "IF ... THEN ... END");
        self.snippet("IF ELSE", "IF ${1:condition} THEN\n    $2\nELSE\n    $0\nEND;", "IF ... THEN ... ELSE ... END");
        self.snippet("WHILE", "WHILE ${1:condition} LIMIT ${2:10} DO\n    $0\nEND;", "WHILE ... DO ... END");
        self.snippet(
            "FOREACH",
            "FOREACH ${1:item} IN ${2:collection} DO\n    $0\nEND;",
            "FOREACH ... IN ... DO ... END",
        );
        self.snippet(
            "FOREACH RANGE",
            "FOREACH ${1:i} IN RANGE[${2:0}, ${3:n}] DO\n    $0\nEND;",
            "FOREACH i IN RANGE[a, b] DO ... END",
        );
        self.snippet("CASE", "CASE\n    WHEN ${1:condition} THEN $2\n    ELSE $0\nEND;", "CASE WHEN ... END");
        self.snippet(
            "TRY",
            "TRY\n    $1\nEXCEPTION\n    WHEN ${2:exception} THEN $0\nEND;",
            "TRY ... EXCEPTION ... END",
        );
        self.snippet("accumulator", "${1:SumAccum}<${2:INT}> @@${3:name};", "declare a global accumulator");
        self.snippet(
            "edge accumulator",
            "${1:SumAccum}<${2:INT}> EDGE @${3:name};",
            "declare an accumulator attached to edges",
        );
        self.snippet(
            "TYPEDEF HeapAccum",
            "TYPEDEF HeapAccum<${1:Tuple}>(${2:10}, ${3:field} DESC) ${4:Top_Heap};",
            "name a HeapAccum type",
        );
        self.snippet(
            "virtual edge",
            "CREATE DIRECTED VIRTUAL EDGE ${1:Name} (FROM ${2:Source}, TO ${3:Target}${0});",
            "declare an in-memory edge type for this query",
        );
    }

    fn top_level(&mut self) {
        for keyword in TOP_LEVEL_KEYWORDS {
            self.keyword(keyword, "4");
        }
        self.snippet(
            "CREATE QUERY",
            "CREATE QUERY ${1:name}(${2}) FOR GRAPH ${3:graph} {\n    $0\n}",
            "define a query",
        );
        self.snippet(
            "CREATE OR REPLACE QUERY",
            "CREATE OR REPLACE QUERY ${1:name}(${2}) FOR GRAPH ${3:graph} {\n    $0\n}",
            "define or replace a query",
        );
        self.snippet(
            "INTERPRET QUERY",
            "INTERPRET QUERY () FOR GRAPH ${1:graph} {\n    $0\n}",
            "run an anonymous query",
        );
        self.snippet(
            "CREATE VERTEX",
            "CREATE VERTEX ${1:Name} (PRIMARY_ID ${2:id} ${3:STRING}${0})",
            "define a vertex type",
        );
        self.snippet(
            "CREATE DIRECTED EDGE",
            "CREATE DIRECTED EDGE ${1:Name} (FROM ${2:Source}, TO ${3:Target}${0}) WITH REVERSE_EDGE=\"reverse_${1:Name}\"",
            "define a directed edge type",
        );
        self.snippet(
            "CREATE UNDIRECTED EDGE",
            "CREATE UNDIRECTED EDGE ${1:Name} (FROM ${2:Source}, TO ${3:Target}${0})",
            "define an undirected edge type",
        );
        self.snippet("CREATE GRAPH", "CREATE GRAPH ${1:name} (${0:*})", "define a graph");
        self.snippet(
            "CREATE LOADING JOB",
            "CREATE LOADING JOB ${1:name} FOR GRAPH ${2:graph} {\n    DEFINE FILENAME ${3:file1};\n    LOAD ${3:file1} TO VERTEX ${4:Type} VALUES (\\$0, \\$1) USING HEADER=\"true\", SEPARATOR=\",\";\n}",
            "define a loading job",
        );
        self.snippet(
            "CREATE SCHEMA_CHANGE JOB",
            "CREATE SCHEMA_CHANGE JOB ${1:name} FOR GRAPH ${2:graph} {\n    $0\n}",
            "define a schema change job",
        );
        self.snippet(
            "CREATE GLOBAL SCHEMA_CHANGE JOB",
            "CREATE GLOBAL SCHEMA_CHANGE JOB ${1:name} {\n    $0\n}",
            "define a schema change job for global types",
        );
        self.snippet(
            "CREATE DISTRIBUTED QUERY",
            "CREATE DISTRIBUTED QUERY ${1:name}(${2}) FOR GRAPH ${3:graph} {\n    $0\n}",
            "define a query that runs on all machines of a cluster",
        );
        self.snippet("RUN QUERY", "RUN QUERY ${1:name}(${0})", "run an installed query");
        self.snippet(
            "RUN LOADING JOB",
            "RUN LOADING JOB ${1:name} USING ${2:file1}=\"${0:path}\"",
            "run a loading job",
        );
        self.schema(&[SymbolKind::Query]);
    }

    fn loading_statement(&mut self) {
        for keyword in
            ["DEFINE FILENAME", "DEFINE HEADER", "DEFINE INPUT_LINE_FILTER", "LOAD", "DELETE VERTEX", "DELETE EDGE"]
        {
            self.keyword(keyword, "4");
        }
        self.snippet(
            "LOAD TO VERTEX",
            "LOAD ${1:file1} TO VERTEX ${2:Type} VALUES (\\$0, \\$1) USING HEADER=\"${3:true}\", SEPARATOR=\"${4:,}\";",
            "load a file into a vertex type",
        );
        self.snippet(
            "LOAD TO EDGE",
            "LOAD ${1:file1} TO EDGE ${2:Type} VALUES (\\$0, \\$1) USING HEADER=\"${3:true}\", SEPARATOR=\"${4:,}\";",
            "load a file into an edge type",
        );
    }

    fn schema_change_statement(&mut self) {
        for keyword in [
            "ADD VERTEX",
            "ADD DIRECTED EDGE",
            "ADD UNDIRECTED EDGE",
            "ALTER VERTEX",
            "ALTER EDGE",
            "DROP VERTEX",
            "DROP EDGE",
            "ALTER GRAPH",
        ] {
            self.keyword(keyword, "4");
        }
        self.snippet(
            "ADD ATTRIBUTE",
            "ALTER VERTEX ${1:Type} ADD ATTRIBUTE (${2:name} ${3:STRING});",
            "add an attribute to a vertex type",
        );
        self.snippet(
            "ADD TO GRAPH",
            "ADD VERTEX ${1:Type} TO GRAPH ${2:graph};",
            "add global types to a graph (global schema change job)",
        );
        self.snippet(
            "DROP FROM GRAPH",
            "DROP VERTEX ${1:Type} FROM GRAPH ${2:graph};",
            "remove global types from a graph (global schema change job)",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{Fixture, cursor};

    const SCHEMA: &str = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE VERTEX City (PRIMARY_ID id STRING, population INT)\nCREATE DIRECTED EDGE Knows (FROM Person, TO Person, since DATETIME)\nCREATE GRAPH Social (Person, City, Knows)\n";

    fn labels(text: &str) -> Vec<String> {
        let (text, offset) = cursor(text);
        let fixture = Fixture::with_files(&text, &[("file:///test/schema.gsql", SCHEMA)]);
        let snapshot = fixture.snapshot();
        completion(&snapshot, snapshot.position(offset), true).items.into_iter().map(|i| i.label).collect()
    }

    fn has(labels: &[String], wanted: &[&str]) -> bool {
        wanted.iter().all(|w| labels.iter().any(|l| l == w))
    }

    /// Every byte offset of `text` that lies between two tokens where a comment may be written
    /// (not inside a word, a string, an accumulator name or a multi-character operator).
    fn token_gaps(text: &str) -> Vec<usize> {
        let bytes = text.as_bytes();
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let operator = |c: u8| b"-<>=!&|*/+%".contains(&c);
        let mut in_string = false;
        let mut gaps = Vec::new();
        for i in 1..bytes.len() {
            let (a, b) = (bytes[i - 1], bytes[i]);
            if a == b'"' {
                in_string = !in_string;
            }
            let joined = (word(a) && word(b)) || (operator(a) && operator(b)) || a == b'@';
            if !in_string && !joined {
                gaps.push(i);
            }
        }
        gaps
    }

    /// The text with `comment` inserted at `gap`, and the cursor moved along (a cursor at the
    /// gap stays in front of the comment when it is after the word to complete, and moves
    /// behind it when it is on the word to describe).
    fn with_comment(text: &str, cursor: usize, gap: usize, comment: &str, on_word: bool) -> (String, usize) {
        let moved = if gap < cursor || (on_word && gap == cursor) { cursor + comment.len() } else { cursor };
        (format!("{}{comment}{}", &text[..gap], &text[gap..]), moved)
    }

    fn sorted_labels(text: &str, offset: usize) -> Vec<String> {
        let fixture = Fixture::with_files(text, &[("file:///test/schema.gsql", SCHEMA)]);
        let snapshot = fixture.snapshot();
        let mut found: Vec<String> =
            completion(&snapshot, snapshot.position(offset), true).items.into_iter().map(|i| i.label).collect();
        found.sort();
        found
    }

    #[test]
    fn comments_between_tokens_do_not_change_the_completion() {
        let documents = [
            "CREATE QUERY q(VERTEX<Person> p) {\n  Start = {p};\n  R = SELECT t FROM Start|:s -(Knows:e)- Person:t;\n  PRINT R;\n}\n",
            "CREATE QUERY q(VERTEX<Person> p) {\n  Start = {p};\n  R = SELECT t FROM |Start:s;\n  PRINT R;\n}\n",
            "CREATE QUERY q(VERTEX<Person> p) {\n  Start = {p};\n  R = SELECT t FROM Start:s -(Knows:e)- Person:|t;\n  PRINT R;\n}\n",
            "CREATE QUERY q(VERTEX<Person> p) {\n  Start = {p};\n  R = SELECT t FROM Start:s -(Knows:e)- Person:t WHERE t.|;\n  PRINT R;\n}\n",
            "CREATE QUERY q(VERTEX<Person> p) {\n  SumAccum<INT> @cnt;\n  Start = {p};\n  R = SELECT t FROM Start:s -(Knows:e)- Person:t ACCUM t.@|;\n  PRINT R;\n}\n",
            "CREATE QUERY q(VERTEX<Person> p) {\n  Start = {p};\n  R = SELECT t FROM Start:s -(|Knows:e)- Person:t;\n  PRINT R;\n}\n",
            "CREATE QUERY q() {\n  MapAccum<STRING, INT> @@m;\n  FOREACH (k, v) IN @@m DO\n    PRINT k.|;\n  END;\n}\n",
            "CREATE QUERY q() {\n  R = SELECT t FROM (s:Person)-[:Knows]->(t:|);\n}\n",
            "CREATE QUERY q() {\n  R = SELECT t FROM (s:Person)-[e:|]->(t:Person);\n}\n",
            "CREATE QUERY q() {\n  PRINT \"a // b\", |;\n}\n",
        ];
        for document in documents {
            let (text, cursor_at) = cursor(document);
            let expected = sorted_labels(&text, cursor_at);
            for gap in token_gaps(&text) {
                for comment in [" /* x FROM . @ */ ", " // FROM Person:\n"] {
                    let (changed, moved) = with_comment(&text, cursor_at, gap, comment, false);
                    assert_eq!(sorted_labels(&changed, moved), expected, "{comment:?} at {gap} of {text:?}");
                }
            }
        }
    }

    #[test]
    fn comments_inside_patterns_and_tuples_do_not_change_the_types() {
        use crate::features::hover::hover;
        let documents = [
            "CREATE QUERY q(VERTEX<Person> m) {\n  S = {m};\n  F = SELECT t FROM S:s -(Knows:e)- (Person|City):§t;\n  PRINT F;\n}\n",
            "CREATE QUERY q(VERTEX<Person> m) {\n  S = {m};\n  F = SELECT t FROM S:s -((Knows|Knows):§e)- (Person|City):t;\n  PRINT F;\n}\n",
            "CREATE QUERY q() {\n  MapAccum<STRING, ListAccum<STRING>> @@m;\n  FOREACH (§key, val) IN @@m DO\n    PRINT key;\n  END;\n}\n",
            "CREATE QUERY q() {\n  MapAccum<STRING, ListAccum<STRING>> @@m;\n  FOREACH (key, §val) IN @@m DO\n    PRINT key;\n  END;\n}\n",
        ];
        let hover_text = |text: &str, offset: usize| {
            let fixture = Fixture::with_files(text, &[("file:///test/schema.gsql", SCHEMA)]);
            let snapshot = fixture.snapshot();
            format!("{:?}", hover(&snapshot, snapshot.position(offset)).map(|h| h.contents))
        };
        for document in documents {
            let cursor_at = document.find('§').unwrap();
            let text = document.replace('§', "");
            let expected = hover_text(&text, cursor_at);
            assert_ne!(expected, "None", "{document}");
            for gap in token_gaps(&text) {
                for comment in [" /* x */ ", " // x\n"] {
                    let (changed, moved) = with_comment(&text, cursor_at, gap, comment, true);
                    assert_eq!(hover_text(&changed, moved), expected, "{comment:?} at {gap} of {text:?}");
                }
            }
        }
    }

    #[test]
    fn completes_attributes_of_aliases() {
        let found = labels(
            "CREATE QUERY q() {\n  SumAccum<INT> @cnt;\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t WHERE t.|;\n}\n",
        );
        assert!(has(&found, &["name", "age", "@cnt", "outdegree"]), "{found:?}");
        assert!(!found.contains(&"population".to_string()), "{found:?}");
    }

    #[test]
    fn the_declaration_shown_does_not_depend_on_the_indexing_order() {
        let a = ("file:///test/a.gsql", "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\n");
        let b = ("file:///test/b.gsql", "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\n");
        let (text, offset) = cursor("CREATE QUERY q() {\n  S = SELECT s FROM |;\n}\n");
        let details = |files: &[(&str, &str)]| {
            let fixture = Fixture::with_files(&text, files);
            let snapshot = fixture.snapshot();
            let items = completion(&snapshot, snapshot.position(offset), true).items;
            items.into_iter().find(|i| i.label == "Person").and_then(|i| i.detail)
        };
        assert_eq!(details(&[a, b]), details(&[b, a]));
    }

    #[test]
    fn header_words_and_global_accumulators_stay_in_their_place() {
        let found = labels("RUN QUERY q(drop |");
        assert!(!found.contains(&"VERTEX".to_string()), "{found:?}");
        let found = labels("CREATE QUERY q() {\n  S = {Person.*};\n  R = SELECT s FROM S:s ACCUM s.@@|;\n}\n");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn edge_parentheses_need_a_from_pattern() {
        let found = labels("CREATE QUERY q() {\n  INT a = 3 -((|;\n}\n");
        assert!(!found.contains(&"Knows".to_string()), "{found:?}");
        let found = labels("CREATE QUERY q() {\n  R = SELECT t FROM Person:s -((|;\n}\n");
        assert!(found.contains(&"Knows".to_string()), "{found:?}");
    }

    #[test]
    fn a_decimal_point_is_not_a_member_access() {
        let found = labels("CREATE QUERY q() {\n  DOUBLE d = 1. |;\n}\n");
        assert!(!found.contains(&"name".to_string()), "{found:?}");
        let found = labels("CREATE QUERY q() {\n  DOUBLE d = 2.5 + 1.|;\n}\n");
        assert!(!found.contains(&"name".to_string()), "{found:?}");
        // A variable with digits in its name is still an object.
        let found = labels("CREATE QUERY q() {\n  S = {Person.*};\n  R = SELECT t1 FROM S:t1 WHERE t1.|;\n}\n");
        assert!(has(&found, &["name", "age"]), "{found:?}");
    }

    #[test]
    fn job_file_variables_stay_inside_their_job() {
        let job = "CREATE LOADING JOB load1 FOR GRAPH Social {\n  DEFINE FILENAME f1;\n  LOAD f1 TO VERTEX Person VALUES ($0, $1, $2);\n}\n";
        let (text, offset) = cursor("CREATE QUERY q() {\n  INT fx = 1;\n  PRINT f|;\n}\n");
        let fixture =
            Fixture::with_files(&text, &[("file:///test/schema.gsql", SCHEMA), ("file:///test/job.gsql", job)]);
        let snapshot = fixture.snapshot();
        let found: Vec<String> =
            completion(&snapshot, snapshot.position(offset), true).items.into_iter().map(|i| i.label).collect();
        assert!(found.contains(&"fx".to_string()) && !found.contains(&"f1".to_string()), "{found:?}");
        let symbols = crate::features::symbols::workspace_symbols(&fixture.workspace, "f1");
        assert!(symbols.is_empty(), "{symbols:?}");
    }

    #[test]
    fn completes_vector_attributes_added_by_alter() {
        let job = "CREATE SCHEMA_CHANGE JOB j FOR GRAPH Social {\n  ALTER VERTEX Person ADD VECTOR ATTRIBUTE emb(DIMENSION=3, METRIC=\"L2\");\n}\n";
        let query = "CREATE QUERY q() {\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t WHERE t.|;\n}\n";
        let found = labels(&format!("{job}{query}"));
        assert!(has(&found, &["name", "emb"]), "{found:?}");
        let found = labels(&format!("{job}CREATE QUERY q() {{\n  R = SELECT t FROM City:t WHERE t.|;\n}}\n"));
        assert!(!found.contains(&"emb".to_string()), "{found:?}");
    }

    #[test]
    fn completes_edge_attributes() {
        let found = labels("CREATE QUERY q() {\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t WHERE e.|;\n}\n");
        assert!(has(&found, &["since", "isDirected"]), "{found:?}");
    }

    #[test]
    fn completes_accumulators_and_their_methods() {
        let found = labels("CREATE QUERY q() {\n  MapAccum<STRING, INT> @@m;\n  SumAccum<INT> @@n;\n  @@|\n}\n");
        assert!(has(&found, &["@@m", "@@n"]), "{found:?}");
        let found = labels("CREATE QUERY q() {\n  MapAccum<STRING, INT> @@m;\n  PRINT @@m.|;\n}\n");
        assert!(has(&found, &["containsKey", "get", "size"]), "{found:?}");
    }

    #[test]
    fn completes_from_clause_positions() {
        let found = labels("CREATE QUERY q() {\n  S = {Person.*};\n  R = SELECT t FROM |\n}\n");
        assert!(has(&found, &["S", "Person", "City"]), "{found:?}");
        let found = labels("CREATE QUERY q() {\n  R = SELECT t FROM Person:s -(|\n}\n");
        assert!(has(&found, &["Knows"]), "{found:?}");
        assert!(!found.contains(&"Person".to_string()), "{found:?}");
    }

    #[test]
    fn completes_graphs_and_vertex_type_arguments() {
        let found = labels("CREATE QUERY q() FOR GRAPH |");
        assert_eq!(found, vec!["Social"]);
        let found = labels("CREATE QUERY q(VERTEX<|");
        assert!(has(&found, &["Person", "City"]), "{found:?}");
    }

    #[test]
    fn offers_accumulators_when_the_query_is_swallowed_by_a_syntax_error() {
        // Without a closing `END` the parser may treat the rest of the query as one error.
        let mut query =
            String::from("CREATE QUERY q() {\n  SetAccum<STRING> @@targets;\n  ListAccum<STRING> @@slices;\n");
        for i in 0..30 {
            query.push_str(&format!("  INT v{i} = {i};\n"));
        }
        query.push_str("  FOREACH s IN @@targets DO @@|\n  PRINT 1;\n}\n");
        let found = labels(&query);
        assert!(has(&found, &["@@targets", "@@slices"]), "{found:?}");
    }

    #[test]
    fn accumulators_of_a_swallowed_query_are_offered_on_an_empty_expression() {
        let filler: String = (0..30).map(|i| format!("  INT v{i} = {i};\n")).collect();
        let header = "CREATE QUERY other() {\n  SumAccum<INT> @@foreign;\n  PRINT 1;\n}\n";
        let body = "  SetAccum<STRING> @@targets;\n  MapAccum<STRING, ListAccum<INT>> @@m, @@n;\n  HeapAccum<Pair>(3, a DESC) @@h;\n";
        for tail in [
            "  x = SELECT s FROM Person:s WHERE s.age > ;\n  PRINT |\n",
            "  FOREACH i IN RANGE[0, 3] DO\n    PRINT |\n",
            "  WHILE |\n",
            "  IF THEN ELSE (( |\n",
        ] {
            let found = labels(&format!("{header}CREATE QUERY q() {{\n{body}{filler}{tail}  PRINT 1;\n}}\n"));
            assert!(has(&found, &["@@targets", "@@m", "@@n", "@@h"]), "{tail}: {found:?}");
            assert!(!found.iter().any(|l| l == "@@foreign"), "{tail}: {found:?}");
        }
        // Block-scoped declarations of closed blocks are not visible.
        let found = labels(&format!(
            "CREATE QUERY q() {{\n  IF TRUE THEN\n    SumAccum<INT> @@inner;\n  END;\n{filler}  PRINT ((  @@|\n}}\n"
        ));
        assert!(!found.iter().any(|l| l == "@@inner"), "{found:?}");
    }

    #[test]
    fn completes_in_calls_and_parameter_lists_of_swallowed_queries() {
        let mut body = String::new();
        for i in 0..30 {
            body.push_str(&format!("  INT v{i} = {i};\n"));
        }
        let call = format!("CREATE QUERY q() {{\n  SumAccum<INT> @@n;\n{body}  PRINT to_string(|\n  PRINT 1;\n}}\n");
        let found = labels(&call);
        assert!(has(&found, &["v1", "@@n", "abs"]), "{found:?}");
        let found = labels("CREATE QUERY q(|\n  PRINT 1;\n}\n");
        assert!(has(&found, &["INT", "STRING"]), "{found:?}");
    }

    #[test]
    fn completes_types_only_in_type_arguments() {
        let found = labels("CREATE QUERY q() {\n  SumAccum<|\n}\n");
        assert!(has(&found, &["INT", "STRING"]), "{found:?}");
        // After a value, `<` compares.
        let found = labels("CREATE QUERY q(INT threshold) {\n  INT x = 1;\n  IF x < |\n}\n");
        assert!(has(&found, &["threshold", "x"]), "{found:?}");
        assert!(!found.contains(&"SumAccum".to_string()), "{found:?}");
    }

    #[test]
    fn completes_statements_and_top_level_commands() {
        let found = labels("CREATE QUERY q() {\n  INT x = 1;\n  |\n}\n");
        assert!(has(&found, &["SELECT", "IF", "SumAccum", "x", "SELECT block"]), "{found:?}");
        let found = labels("USE GRAPH Social\n|");
        assert!(has(&found, &["CREATE QUERY", "INSTALL QUERY", "USE GRAPH"]), "{found:?}");
    }

    #[test]
    fn no_completion_in_comments_or_strings() {
        assert!(labels("CREATE QUERY q() {\n  // SEL|\n}\n").is_empty());
        assert!(labels("CREATE QUERY q() {\n  PRINT \"ab|c\";\n}\n").is_empty());
    }

    #[test]
    fn completes_inside_unfinished_code() {
        // Member access with nothing after the dot, before the closing brace.
        let found = labels("CREATE QUERY q() {\n  R = SELECT t FROM Person:t WHERE t.|\n}\n");
        assert!(has(&found, &["name", "age"]), "{found:?}");
        // A vertex-attached accumulator being typed in ACCUM.
        let found = labels("CREATE QUERY q() {\n  SumAccum<INT> @deg;\n  R = SELECT t FROM Person:t ACCUM t.@|\n}\n");
        assert!(has(&found, &["@deg"]), "{found:?}");
        assert!(!found.contains(&"age".to_string()), "{found:?}");
        // A new statement after one that is missing its semicolon.
        let found = labels("CREATE QUERY q(INT k) {\n  INT x = k\n  |\n}\n");
        assert!(has(&found, &["k"]), "{found:?}");
        // Edge alternatives.
        let found = labels("CREATE QUERY q() {\n  R = SELECT t FROM Person:s -(Knows|\n}\n");
        assert!(has(&found, &["Knows"]), "{found:?}");
    }

    #[test]
    fn completes_inside_an_unfinished_interpret_body() {
        let found = labels("INTERPRET QUERY () {\n SumAccum<INT> @c;\n S={Person.*};\n R = SELECT t FROM |");
        assert!(has(&found, &["S", "Person"]), "{found:?}");
        let found = labels("INTERPRET QUERY () FOR GRAPH Social {\n  PRINT |");
        assert!(has(&found, &["abs"]), "{found:?}");
        let found = labels("INTERPRET QUERY () {\n  |");
        assert!(has(&found, &["SELECT", "IF"]) && !found.contains(&"DROP".to_string()), "{found:?}");
        let found = labels("INTERPRET QUERY () {\n SumAccum<INT> @c;\n R = SELECT t FROM Person:t ACCUM t.@c += |");
        assert!(has(&found, &["abs"]), "{found:?}");
        let found = labels("INTERPRET OPENCYPHER QUERY () {\n  |");
        assert!(!found.contains(&"DROP".to_string()), "{found:?}");
        // A stored query run with INTERPRET has no body: top level again after it.
        let found = labels("INTERPRET QUERY q(1)\n|");
        assert!(has(&found, &["CREATE QUERY"]), "{found:?}");
    }

    #[test]
    fn completes_keywords_after_definition_and_command_words() {
        let check = |text: &str, wanted: &[&str], unwanted: &[&str]| {
            let found = labels(text);
            assert!(has(&found, wanted), "{text:?}: {found:?}");
            assert!(unwanted.iter().all(|u| !found.contains(&u.to_string())), "{text:?}: {found:?}");
        };
        check("USE Social\nCREATE |", &["QUERY", "VERTEX", "EDGE", "GRAPH", "LOADING", "OR"], &["SELECT"]);
        check("USE |", &["GRAPH", "GLOBAL"], &["QUERY"]);
        check("CREATE OR |", &["REPLACE"], &["QUERY"]);
        check("CREATE OR REPLACE |", &["QUERY", "LOADING"], &["VERTEX"]);
        check("CREATE LOADING |", &["JOB"], &["QUERY"]);
        check("CREATE GLOBAL |", &["SCHEMA_CHANGE"], &["LOADING"]);
        check("CREATE DISTRIBUTED |", &["QUERY"], &["VERTEX"]);
        check("CREATE UNDIRECTED |", &["EDGE"], &["QUERY"]);
        check("DROP |", &["VERTEX", "QUERY", "JOB"], &["LOADING"]);
        check("INSTALL |", &["QUERY"], &["VERTEX"]);
        check("RUN |", &["QUERY", "LOADING", "SCHEMA_CHANGE"], &["VERTEX"]);
        check("RUN GLOBAL |", &["LOADING", "SCHEMA_CHANGE"], &["QUERY"]);
        check("INTERPRET |", &["QUERY", "OPENCYPHER"], &["VERTEX"]);
        check("CREATE QUERY q() |", &["FOR", "RETURNS", "SYNTAX"], &["GRAPH"]);
        check("CREATE QUERY q() FOR |", &["GRAPH"], &["SYNTAX"]);
        check("CREATE QUERY q() FOR GRAPH Social |", &["RETURNS", "SYNTAX"], &["FOR"]);
        check("CREATE QUERY q() FOR GRAPH Social SYNTAX |", &["v1", "v2"], &["FOR"]);
        check("INTERPRET QUERY () |", &["FOR", "SYNTAX"], &[]);
        check("CREATE LOADING JOB j |", &["FOR"], &["GRAPH"]);
        check("CREATE LOADING JOB j FOR |", &["GRAPH"], &["FOR"]);
        check("CREATE SCHEMA_CHANGE JOB j FOR |", &["GRAPH"], &["FOR"]);
        // Not headers: a completed query, and words inside bodies.
        let found = labels("CREATE QUERY q() {\n  PRINT 1;\n}\n|");
        assert!(has(&found, &["CREATE QUERY"]), "{found:?}");
        let found = labels("CREATE QUERY q() FOR GRAPH Social {\n  FOREACH x IN [1] DO\n  |\n}\n");
        assert!(has(&found, &["SELECT"]) && !found.contains(&"GRAPH".to_string()), "{found:?}");
        let found = labels("CREATE VERTEX V (PRIMARY_ID id STRING)\n|");
        assert!(has(&found, &["CREATE QUERY"]), "{found:?}");
    }

    #[test]
    fn completes_edge_attached_accumulators_on_edge_aliases() {
        let decls = "  SumAccum<INT> EDGE @w;\n  SumAccum<INT> @v;\n";
        // Unfinished (the error swallows the query) and complete statements.
        for tail in [
            "  S = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM e.@|",
            "  S = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM e.@|;\n",
            "  S = SELECT t FROM Person:s -(Knows:e)- Person:t WHERE e.@|\n",
        ] {
            let found = labels(&format!("CREATE QUERY q() {{\n{decls}{tail}\n}}\n"));
            assert!(has(&found, &["@w"]), "{tail}: {found:?}");
            assert!(!found.contains(&"@v".to_string()), "{tail}: {found:?}");
        }
        // Vertex aliases get the others, and `@@` is never a member.
        let found = labels(&format!(
            "CREATE QUERY q() {{\n{decls}  S = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM s.@|\n}}\n"
        ));
        assert!(has(&found, &["@v"]) && !found.contains(&"@w".to_string()), "{found:?}");
        // Without the `@` the attributes come along.
        let found = labels(&format!(
            "CREATE QUERY q() {{\n{decls}  S = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM e.|;\n}}\n"
        ));
        assert!(has(&found, &["@w", "since"]), "{found:?}");
    }

    #[test]
    fn completes_loading_jobs() {
        let found = labels("CREATE LOADING JOB j FOR GRAPH Social {\n  DEFINE FILENAME f1;\n  LOAD |\n}\n");
        assert!(has(&found, &["f1", "TEMP_TABLE"]), "{found:?}");
        let found =
            labels("CREATE LOADING JOB j FOR GRAPH Social {\n  DEFINE FILENAME f1;\n  LOAD f1 TO VERTEX |\n}\n");
        assert!(has(&found, &["Person", "City"]), "{found:?}");
        let found =
            labels("CREATE LOADING JOB j FOR GRAPH Social {\n  LOAD f1 TO VERTEX Person VALUES ($0) USING |\n}\n");
        assert!(has(&found, &["SEPARATOR", "HEADER"]), "{found:?}");
    }

    #[test]
    fn job_names_follow_the_command_word() {
        let defs = "CREATE LOADING JOB lj FOR GRAPH Social {\n  DEFINE FILENAME f1;\n}\nCREATE SCHEMA_CHANGE JOB sj FOR GRAPH Social {\n  ADD VERTEX Pet (PRIMARY_ID id STRING);\n}\nCREATE QUERY qq() { PRINT 1; }\n";
        let at = |command: &str| labels(&format!("{defs}{command}|"));
        let found = at("RUN LOADING JOB ");
        assert!(has(&found, &["lj"]) && !found.contains(&"sj".to_string()), "{found:?}");
        let found = at("RUN SCHEMA_CHANGE JOB ");
        assert!(has(&found, &["sj"]) && !found.contains(&"lj".to_string()), "{found:?}");
        let found = at("DROP JOB ");
        assert!(has(&found, &["lj", "sj", "ALL"]), "{found:?}");
        let found = at("SHOW JOB ");
        assert!(has(&found, &["lj", "sj"]) && !found.contains(&"ALL".to_string()), "{found:?}");
        // ALL stands for the queries of INSTALL and DROP only.
        for command in ["INSTALL QUERY ", "DROP QUERY "] {
            let found = at(command);
            assert!(has(&found, &["qq", "ALL"]), "{command}: {found:?}");
        }
        for command in ["RUN QUERY ", "SHOW QUERY "] {
            let found = at(command);
            assert!(has(&found, &["qq"]) && !found.contains(&"ALL".to_string()), "{command}: {found:?}");
        }
    }

    #[test]
    fn completes_queries_for_run_and_install() {
        let found = labels("CREATE QUERY hello() { PRINT 1; }\nINSTALL QUERY |");
        assert!(has(&found, &["hello", "ALL"]), "{found:?}");
    }

    #[test]
    fn previous_tokens_handle_multibyte_text() {
        let text = "PRINT \"世界\" -> 界é ab";
        assert_eq!(previous_tokens(text, text.len(), 6), vec!["AB", "é", "界", "->", "\"\"", "PRINT"]);
    }

    const TRAVEL: &str = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE VERTEX City (PRIMARY_ID id STRING, population INT)\nCREATE VERTEX Country (PRIMARY_ID id STRING)\nCREATE DIRECTED EDGE Lives (FROM Person, TO City, since INT) WITH REVERSE_EDGE=\"Housed\"\nCREATE UNDIRECTED EDGE Friend (FROM Person, TO Person)\nCREATE DIRECTED EDGE In_country (FROM City, TO Country)\nCREATE GRAPH Travel (Person, City, Country, Lives, Friend, In_country)\n";

    /// The cursor is the last `|` (edge alternatives use it too).
    fn travel(text: &str) -> Vec<String> {
        let offset = text.rfind('|').expect("cursor marker");
        let text = format!("{}{}", &text[..offset], &text[offset + 1..]);
        let fixture = Fixture::with_files(&text, &[("file:///test/schema.gsql", TRAVEL)]);
        let snapshot = fixture.snapshot();
        completion(&snapshot, snapshot.position(offset), true).items.into_iter().map(|i| i.label).collect()
    }

    fn lacks(labels: &[String], unwanted: &[&str]) -> bool {
        unwanted.iter().all(|w| !labels.iter().any(|l| l == w))
    }

    #[test]
    fn offers_the_clauses_that_can_follow_a_from_pattern() {
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  x = SELECT s FROM Person:s |\n}\n");
        assert!(has(&found, &["WHERE", "ACCUM", "POST-ACCUM", "GROUP BY", "HAVING", "ORDER BY", "LIMIT"]), "{found:?}");
        assert!(lacks(&found, &["FROM", "abs", "AND"]), "{found:?}");
        // While a clause name is being typed.
        let found = travel("CREATE QUERY q() {\n  x = SELECT s FROM Person:s WH|\n}\n");
        assert!(has(&found, &["WHERE", "LIMIT"]), "{found:?}");
        // Only clauses later than the last one written, in order.
        let found = travel("CREATE QUERY q() {\n  x = SELECT s FROM Person:s WHERE s.age > 3 |\n}\n");
        assert!(has(&found, &["ACCUM", "POST-ACCUM", "ORDER BY", "LIMIT"]), "{found:?}");
        assert!(lacks(&found, &["WHERE", "SAMPLE"]), "{found:?}");
        let found =
            travel("CREATE QUERY q() {\n  SumAccum<INT> @c;\n  x = SELECT s FROM Person:s ACCUM s.@c += 1 |\n}\n");
        assert!(has(&found, &["POST-ACCUM", "LIMIT"]), "{found:?}");
        assert!(lacks(&found, &["ACCUM", "WHERE"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT s FROM Person:s ORDER BY s.age |\n}\n");
        assert!(has(&found, &["LIMIT"]), "{found:?}");
        assert!(lacks(&found, &["ORDER BY", "HAVING", "ACCUM"]), "{found:?}");
    }

    #[test]
    fn group_by_is_not_offered_in_syntax_v1() {
        let found = travel("CREATE QUERY q() SYNTAX v1 {\n  x = SELECT s FROM Person:s |\n}\n");
        assert!(has(&found, &["WHERE", "ORDER BY"]), "{found:?}");
        assert!(lacks(&found, &["GROUP BY"]), "{found:?}");
    }

    #[test]
    fn no_clauses_where_an_operand_is_expected() {
        let found = travel("CREATE QUERY q() {\n  x = SELECT s FROM Person:s WHERE s.age > 3 AND |\n}\n");
        assert!(has(&found, &["abs"]), "{found:?}");
        assert!(lacks(&found, &["ACCUM", "LIMIT"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT s FROM Person:s -(|)- City:c\n}\n");
        assert!(lacks(&found, &["WHERE", "LIMIT"]), "{found:?}");
    }

    #[test]
    fn edge_types_follow_the_source_vertex() {
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(|)- Person:p\n}\n");
        assert!(has(&found, &["Lives", "Housed", "In_country"]), "{found:?}");
        assert!(lacks(&found, &["Friend"]), "{found:?}");
        // Directions: `->` leaves the source, `<` arrives at it.
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(|)-> Person:p\n}\n");
        assert!(has(&found, &["Housed", "In_country"]), "{found:?}");
        assert!(lacks(&found, &["Lives", "Friend"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(<|)- Person:p\n}\n");
        assert!(has(&found, &["Lives"]), "{found:?}");
        assert!(lacks(&found, &["Housed", "In_country", "Friend"]), "{found:?}");
        // Undirected edges fit either way.
        let found = travel("CREATE QUERY q() {\n  x = SELECT p FROM Person:p -(|)-> Person:t\n}\n");
        assert!(has(&found, &["Lives", "Friend"]), "{found:?}");
        assert!(lacks(&found, &["Housed", "In_country"]), "{found:?}");
    }

    #[test]
    fn edge_types_fall_back_to_all_when_the_source_is_unknown() {
        let found = travel("CREATE QUERY q() {\n  x = SELECT t FROM ANY:a -(|)- Person:t\n}\n");
        assert!(has(&found, &["Lives", "Friend", "In_country", "Housed"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT t FROM :a -(|)- Person:t\n}\n");
        assert!(has(&found, &["Lives", "Friend", "In_country", "Housed"]), "{found:?}");
    }

    #[test]
    fn edge_types_follow_vertex_set_variables() {
        let found = travel("CREATE QUERY q() {\n  S = {City.*};\n  x = SELECT c FROM S:c -(|)- Person:p\n}\n");
        assert!(has(&found, &["Lives", "In_country"]), "{found:?}");
        assert!(lacks(&found, &["Friend"]), "{found:?}");
    }

    #[test]
    fn target_vertices_follow_the_edge() {
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM Person:p -(Lives>)- |\n}\n");
        assert!(has(&found, &["City", "ANY"]), "{found:?}");
        assert!(lacks(&found, &["Person", "Country"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(Lives)-> |\n}\n");
        assert!(has(&found, &["City"]), "{found:?}");
        assert!(lacks(&found, &["Person"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(<Lives)- |\n}\n");
        assert!(has(&found, &["Person"]), "{found:?}");
        assert!(lacks(&found, &["City"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(Friend:f)- |\n}\n");
        assert!(has(&found, &["Person"]), "{found:?}");
        assert!(lacks(&found, &["Country"]), "{found:?}");
        // Alternatives: both ends.
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(Lives>|In_country>)- |\n}\n");
        assert!(has(&found, &["City", "Country"]), "{found:?}");
        assert!(lacks(&found, &["Person"]), "{found:?}");
        // An edge that is not known: every vertex type.
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(ANY)- |\n}\n");
        assert!(has(&found, &["Person", "City", "Country"]), "{found:?}");
    }

    #[test]
    fn grouped_edge_steps_offer_edge_types() {
        // After `-((` and after a `|` inside it: the edges of the source, minus those written.
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -((|)- Person:p\n}\n");
        assert!(has(&found, &["Lives", "Housed", "In_country"]), "{found:?}");
        assert!(lacks(&found, &["Friend"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -((Lives| |)- Person:p\n}\n");
        assert!(has(&found, &["Housed", "In_country"]), "{found:?}");
        assert!(lacks(&found, &["Lives", "Friend"]), "{found:?}");
        // The direction after the group is read.
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -((|):e)-> Person:p\n}\n");
        assert!(has(&found, &["Housed", "In_country"]), "{found:?}");
        assert!(lacks(&found, &["Lives", "Friend"]), "{found:?}");
        // Already-written alternatives are not offered again in the plain form either.
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM Person:p -(Lives>| |)- City:c\n}\n");
        assert!(has(&found, &["Friend"]), "{found:?}");
        assert!(lacks(&found, &["Lives"]), "{found:?}");
    }

    #[test]
    fn target_vertices_follow_repeated_and_grouped_edges() {
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM Person:p -(Lives>*1..3)- |\n}\n");
        assert!(has(&found, &["City"]), "{found:?}");
        assert!(lacks(&found, &["Person", "Country"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -(In_country>*2)- |\n}\n");
        assert!(has(&found, &["Country"]), "{found:?}");
        assert!(lacks(&found, &["Person", "City"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -((Lives|In_country>):e)- |\n}\n");
        assert!(has(&found, &["Person", "City", "Country"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -((In_country)>)- |\n}\n");
        assert!(has(&found, &["Country"]), "{found:?}");
        assert!(lacks(&found, &["Person", "City"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  x = SELECT c FROM City:c -((Lives|In_country>)*1..2)- |\n}\n");
        assert!(has(&found, &["Person", "City", "Country"]), "{found:?}");
        // A repetition that may be empty can end at the source: every vertex type.
        for star in ["*", "*0..2", "*..2"] {
            let found =
                travel(&format!("CREATE QUERY q() {{\n  x = SELECT c FROM City:c -(In_country>{star})- |\n}}\n"));
            assert!(has(&found, &["Person", "City", "Country"]), "{star}: {found:?}");
        }
    }

    #[test]
    fn delete_heads_offer_where() {
        let found = travel("CREATE QUERY q() {\n  S = {Person.*};\n  DELETE s FROM S:s |\n}\n");
        assert_eq!(found, ["WHERE"]);
        // After the condition only an expression continues.
        let found = travel("CREATE QUERY q() {\n  S = {Person.*};\n  DELETE s FROM S:s WHERE s.age > 1 |\n}\n");
        assert!(lacks(&found, &["WHERE"]), "{found:?}");
    }

    #[test]
    fn foreach_variables_take_the_element_type() {
        let found = travel("CREATE QUERY q(SET<VERTEX<City>> v) {\n  FOREACH x IN v DO PRINT x.|\n}\n");
        assert!(has(&found, &["population"]), "{found:?}");
        assert!(lacks(&found, &["age"]), "{found:?}");
        let found = travel("CREATE QUERY q(SET<VERTEX<City>> v) {\n  FOREACH x IN v DO PRINT x.|; END;\n}\n");
        assert!(has(&found, &["population"]), "{found:?}");
        assert!(lacks(&found, &["age"]), "{found:?}");
        let found =
            travel("CREATE QUERY q() {\n  ListAccum<VERTEX<City>> @@l;\n  FOREACH x IN @@l DO PRINT x.|; END;\n}\n");
        assert!(has(&found, &["population"]), "{found:?}");
        assert!(lacks(&found, &["age"]), "{found:?}");
        // Unknown collection: everything.
        let found = travel("CREATE QUERY q() {\n  FOREACH x IN foo() DO PRINT x.|; END;\n}\n");
        assert!(has(&found, &["population", "age"]), "{found:?}");
    }

    #[test]
    fn foreach_variables_end_with_their_loop() {
        let head = "CREATE QUERY q(SET<VERTEX<City>> v) {\n";
        // After the loop the variable is unknown (not a City): every attribute.
        let found = travel(&format!("{head}  FOREACH x IN v DO\n    PRINT 1;\n  END;\n  PRINT x.|\n}}\n"));
        assert!(has(&found, &["population", "age"]), "{found:?}");
        // Nested blocks do not end the outer loop; the loop that is not closed still binds.
        let body = "  FOREACH x IN v DO\n    IF TRUE THEN PRINT 1; ELSE IF FALSE THEN PRINT 2; END;\n    WHILE TRUE DO BREAK; END;\n";
        let found = travel(&format!("{head}{body}    PRINT x.|\n}}\n"));
        assert!(has(&found, &["population"]) && lacks(&found, &["age"]), "{found:?}");
        let found = travel(&format!("{head}{body}  END;\n  PRINT x.|\n}}\n"));
        assert!(has(&found, &["population", "age"]), "{found:?}");
        // A later loop with the same name binds again.
        let found = travel(&format!("{head}  FOREACH x IN v DO PRINT 1; END;\n  FOREACH x IN v DO PRINT x.|\n}}\n"));
        assert!(has(&found, &["population"]) && lacks(&found, &["age"]), "{found:?}");
        // A comment or a string is no loop.
        let found = travel(&format!("{head}  // FOREACH x IN v DO\n  PRINT x.|\n}}\n"));
        assert!(has(&found, &["population", "age"]), "{found:?}");
        let found = travel(&format!("{head}  /* FOREACH x IN v DO */ PRINT x.|\n}}\n"));
        assert!(has(&found, &["population", "age"]), "{found:?}");
        let found = travel(&format!("{head}  PRINT \"FOREACH x IN v DO\"; PRINT x.|\n}}\n"));
        assert!(has(&found, &["population", "age"]), "{found:?}");
    }

    #[test]
    fn completes_types_after_returns_and_in_tuple_fields() {
        let found = travel("CREATE QUERY q() FOR GRAPH Travel RETURNS (|) {\n}\n");
        assert!(has(&found, &["INT", "STRING", "SetAccum"]), "{found:?}");
        assert!(lacks(&found, &["abs"]), "{found:?}");
        let found = travel("CREATE QUERY q() FOR GRAPH Travel RETURNS |\n");
        assert!(has(&found, &["INT", "ListAccum"]), "{found:?}");
        let found = travel("TYPEDEF TUPLE<INT a, |> T;\n");
        assert!(has(&found, &["INT", "STRING", "DATETIME"]), "{found:?}");
        assert!(lacks(&found, &["SetAccum"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  TYPEDEF TUPLE<INT a, STRING b, |> T;\n}\n");
        assert!(has(&found, &["INT", "STRING"]), "{found:?}");
        // A comma after an ordinary `<` stays an expression.
        let found = travel("CREATE QUERY q(INT k) {\n  PRINT k < 3, |;\n}\n");
        assert!(lacks(&found, &["DATETIME"]), "{found:?}");
    }

    #[test]
    fn completes_attribute_lists_of_schema_changes_and_inserts() {
        let found =
            travel("CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n  ALTER VERTEX Person DROP ATTRIBUTE (|);\n}\n");
        assert_eq!(found, ["age", "name"], "{found:?}");
        let found =
            travel("CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n  ALTER EDGE Lives DROP ATTRIBUTE (x, |);\n}\n");
        assert_eq!(found, vec!["since"]);
        let found =
            travel("CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n  ALTER VERTEX Person ADD INDEX i ON (|);\n}\n");
        assert!(has(&found, &["name", "age"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  INSERT INTO Person (|) VALUES (1);\n}\n");
        assert!(has(&found, &["name", "age", "PRIMARY_ID"]), "{found:?}");
        assert!(lacks(&found, &["population"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE Lives (|) VALUES (1, 2, 3);\n}\n");
        assert!(has(&found, &["since", "FROM", "TO"]), "{found:?}");
        // The VALUES list is not a column list.
        let found = travel("CREATE QUERY q(INT k) {\n  INSERT INTO Person (id) VALUES (|);\n}\n");
        assert!(lacks(&found, &["name", "PRIMARY_ID"]), "{found:?}");
    }

    #[test]
    fn completes_keywords_after_alter_type() {
        let found = travel("CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n  ALTER VERTEX Person |;\n}\n");
        assert_eq!(found, vec!["ADD", "DROP", "WITH"]);
        let found = travel("CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n  ALTER EDGE Lives DROP |;\n}\n");
        assert!(has(&found, &["ATTRIBUTE", "PAIR"]), "{found:?}");
        let found = travel("CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n  ALTER VERTEX Person ADD |;\n}\n");
        assert!(has(&found, &["ATTRIBUTE"]), "{found:?}");
        assert!(lacks(&found, &["PAIR"]), "{found:?}");
    }

    fn is_empty(found: &[String]) -> bool {
        found.is_empty()
    }

    #[test]
    fn method_call_results_take_the_element_type() {
        let tuple = "CREATE QUERY q() FOR GRAPH Travel {\n  TYPEDEF TUPLE<INT a, STRING b> T;\n";
        let found = travel(&format!("{tuple}  ListAccum<T> @@l;\n  PRINT @@l.get(0).|;\n}}\n"));
        assert_eq!(found, ["a", "b"]);
        let found = travel(&format!("{tuple}  HeapAccum<T>(3, a DESC) @@h;\n  PRINT @@h.top().|;\n}}\n"));
        assert_eq!(found, ["a", "b"]);
        // An unknown result offers nothing, not every attribute of the schema.
        let found = travel("CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  PRINT foo(1).|;\n}\n");
        assert!(is_empty(&found), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  R = SELECT t FROM Person:t WHERE t.name.|;\n}\n");
        assert!(is_empty(&found), "{found:?}");
        // Attributes of an alias are unchanged.
        let found = travel("CREATE QUERY q() {\n  R = SELECT t FROM Person:t WHERE t.|;\n}\n");
        assert!(has(&found, &["name", "age"]), "{found:?}");
    }

    #[test]
    fn dots_after_calls_may_be_spaced_or_on_other_lines() {
        let tuple = "CREATE QUERY q() FOR GRAPH Travel {\n  TYPEDEF TUPLE<INT a, STRING b> T;\n  ListAccum<T> @@l;\n";
        for call in [
            "@@l . get ( 0 ) . |",
            "@@l\n    .get(0)\n    .|",
            "@@l.get(0) .|",
            "(@@l.get(0)).|",
            "( @@l . get(0) ) . |",
            "@@l.get((1)).|",
        ] {
            let found = travel(&format!("{tuple}  PRINT {call};\n}}\n"));
            assert_eq!(found, ["a", "b"], "{call}: {found:?}");
        }
        // A spaced dot after a name completes the members of the name.
        let found = travel("CREATE QUERY q() {\n  R = SELECT t FROM Person:t WHERE t . |\n}\n");
        assert!(has(&found, &["name", "age"]), "{found:?}");
        // Not a member access: a dot that ends a comment, and groups of several operands.
        let found = travel("CREATE QUERY q() {\n  // done.\n  |\n}\n");
        assert!(has(&found, &["abs"]) && lacks(&found, &["name", "age"]), "{found:?}");
        let found = travel(&format!("{tuple}  PRINT (@@l.get(0) + 1).|;\n}}\n"));
        assert!(is_empty(&found), "{found:?}");
        let found = travel(&format!("{tuple}  PRINT (1 @@l.get(0)).|;\n}}\n"));
        assert!(is_empty(&found), "{found:?}");
        let found = travel(&format!("{tuple}  PRINT foo(@@l.get(0)).|;\n}}\n"));
        assert!(is_empty(&found), "{found:?}");
    }

    #[test]
    fn top_and_pop_are_results_of_heaps_only() {
        let tuple = "CREATE QUERY q() FOR GRAPH Travel {\n  TYPEDEF TUPLE<INT a, STRING b> T;\n";
        let found = travel(&format!("{tuple}  HeapAccum<T>(3, a DESC) @@h;\n  PRINT @@h . pop () . |;\n}}\n"));
        assert_eq!(found, ["a", "b"]);
        for kind in ["ListAccum", "SetAccum", "BagAccum"] {
            for method in ["top", "pop"] {
                let found = travel(&format!("{tuple}  {kind}<T> @@l;\n  PRINT @@l.{method}().|;\n}}\n"));
                assert!(is_empty(&found), "{kind}.{method}: {found:?}");
            }
        }
    }

    #[test]
    fn type_arguments_after_a_comma_are_types() {
        let found = travel("CREATE QUERY q() {\n  MapAccum<STRING, |\n}\n");
        assert!(has(&found, &["INT", "SumAccum"]), "{found:?}");
        assert!(lacks(&found, &["abs"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  GroupByAccum<INT a, |\n}\n");
        assert!(has(&found, &["STRING", "SumAccum"]), "{found:?}");
        assert!(lacks(&found, &["abs"]), "{found:?}");
        // The name of a tuple field is new.
        let found = travel("CREATE QUERY q() {\n  TYPEDEF TUPLE<INT a, STRING |\n}\n");
        assert!(is_empty(&found), "{found:?}");
        // The first field has the same types as the later ones.
        let first = travel("CREATE QUERY q() {\n  TYPEDEF TUPLE<|\n}\n");
        let later = travel("CREATE QUERY q() {\n  TYPEDEF TUPLE<INT a, |\n}\n");
        assert_eq!(first, later);
        assert!(has(&first, &["INT", "DATETIME"]), "{first:?}");
        assert!(lacks(&first, &["SumAccum", "VERTEX", "LIST"]), "{first:?}");
    }

    #[test]
    fn completes_schema_definitions() {
        let job = "CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n";
        let found = travel(&format!("{job}  ADD DIRECTED EDGE E (FROM Person, TO |\n"));
        assert_eq!(found, ["City", "Country", "Person"]);
        let found = travel(&format!("{job}  ADD VERTEX V (PRIMARY_ID id |\n"));
        assert!(has(&found, &["STRING", "INT"]), "{found:?}");
        assert!(lacks(&found, &["abs", "SumAccum", "MAP"]), "{found:?}");
        let found = travel(&format!("{job}  ALTER VERTEX Person ADD ATTRIBUTE (x |\n"));
        assert!(has(&found, &["STRING", "DATETIME"]), "{found:?}");
        // A name is new; so is the value after a type.
        let found = travel(&format!("{job}  ALTER VERTEX Person ADD ATTRIBUTE (|\n"));
        assert!(is_empty(&found), "{found:?}");
        let found = travel(&format!("{job}  ALTER VERTEX Person ADD ATTRIBUTE (x INT |\n"));
        assert!(is_empty(&found), "{found:?}");
        let found = travel(&format!("{job}  ALTER EDGE Lives ADD PAIR (FROM |\n"));
        assert!(has(&found, &["Person", "City"]), "{found:?}");
        let found = travel(&format!("{job}  ALTER EDGE Lives ADD PAIR (FROM Person, TO |\n"));
        assert!(has(&found, &["Person", "City"]), "{found:?}");
        let found = travel("CREATE VERTEX V (PRIMARY_ID id STRING, x MAP<STRING, |\n");
        assert!(has(&found, &["INT"]), "{found:?}");
        assert!(lacks(&found, &["SumAccum", "abs"]), "{found:?}");
        let found = travel("CREATE DIRECTED EDGE E (FROM |\n");
        assert!(has(&found, &["Person"]), "{found:?}");
        let found = travel("CREATE DIRECTED EDGE E (FROM Person, |\n");
        assert_eq!(found, ["TO"]);
        let found = travel("CREATE UNDIRECTED EDGE E (FROM Person, TO City, since |\n");
        assert!(has(&found, &["DATETIME"]), "{found:?}");
        // The graph lists types, not the reverse edge another edge declares.
        let found = travel("CREATE GRAPH G2 (|\n");
        assert_eq!(found, ["City", "Country", "Person", "Friend", "In_country", "Lives"]);
        let found = travel("SHOW |\n");
        assert!(has(&found, &["VERTEX", "JOB", "QUERY"]), "{found:?}");
        // Query code is not affected.
        let found = travel("CREATE QUERY q() {\n  PRINT (1, |\n}\n");
        assert!(has(&found, &["abs"]), "{found:?}");
    }

    #[test]
    fn definition_lists_offer_the_fitting_types_and_members() {
        let job = "CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n";
        // The primary id has a scalar type.
        let found = travel(&format!("{job}  ADD VERTEX V (PRIMARY_ID id |\n"));
        assert!(has(&found, &["STRING", "INT", "DATETIME"]), "{found:?}");
        assert!(lacks(&found, &["LIST", "SET", "MAP", "SumAccum"]), "{found:?}");
        let found = travel("CREATE VERTEX V (PRIMARY_ID id |\n");
        assert_eq!(found, ["INT", "UINT", "FLOAT", "DOUBLE", "BOOL", "STRING", "DATETIME"]);
        // An ordinary attribute keeps the collections.
        let found = travel("CREATE VERTEX V (PRIMARY_ID id STRING, x |\n");
        assert!(has(&found, &["LIST", "SET", "MAP"]), "{found:?}");
        // A graph lists each member once.
        let found = travel("CREATE GRAPH G2 (Person, Lives, |\n");
        assert_eq!(found, ["City", "Country", "Friend", "In_country"]);
        let found = travel("CREATE GRAPH G2 (Person,\n  City,\n  Friend,\n  Country, In_country, |\n");
        assert_eq!(found, ["Lives"]);
        let found = travel("CREATE GRAPH G2 (Pe|\n");
        assert!(has(&found, &["Person", "City"]), "{found:?}");
    }

    #[test]
    fn explicit_inserts_offer_no_reverse_edges() {
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE |\n}\n");
        assert!(lacks(&found, &["Housed"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE Lives (|\n}\n");
        assert_eq!(found, ["since", "FROM", "TO"]);
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE Lives (FROM |\n}\n");
        assert_eq!(found, ["Person"]);
    }

    #[test]
    fn completes_insert_targets_and_columns() {
        let found = travel("CREATE QUERY q() {\n  INSERT INTO |\n}\n");
        assert!(has(&found, &["Person", "Lives", "Friend"]), "{found:?}");
        assert!(lacks(&found, &["Housed"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE |\n}\n");
        assert_eq!(found, ["Friend", "In_country", "Lives"]);
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE Lives (FROM Person, TO |\n}\n");
        assert_eq!(found, ["City"]);
        let found = travel("CREATE QUERY q() {\n  INSERT INTO Lives VALUES (a Person, b |\n}\n");
        assert_eq!(found, ["City"]);
        let found = travel("CREATE QUERY q() {\n  INSERT INTO Lives VALUES (a |\n}\n");
        assert_eq!(found, ["Person"]);
        // Columns that are written already are not offered again.
        let found = travel("CREATE QUERY q() {\n  INSERT INTO Person (PRIMARY_ID, name, |\n}\n");
        assert_eq!(found, ["age"]);
        let found = travel("CREATE QUERY q() {\n  INSERT INTO Person (id, |\n}\n");
        assert_eq!(found, ["age", "name"]);
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE Lives (FROM, TO, |\n}\n");
        assert_eq!(found, ["since"]);
        let found = travel("CREATE QUERY q() {\n  INSERT INTO Person (|\n}\n");
        assert!(has(&found, &["PRIMARY_ID", "name", "age"]), "{found:?}");
        // An unknown type has no special columns.
        let found = travel("CREATE QUERY q() {\n  INSERT INTO Nope (|\n}\n");
        assert!(lacks(&found, &["PRIMARY_ID", "FROM", "TO"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  INSERT INTO EDGE Lives (|\n}\n");
        assert!(has(&found, &["FROM", "TO", "since"]), "{found:?}");
    }

    #[test]
    fn order_by_offers_directions_and_limit_offers_offset() {
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  R = SELECT s FROM Person:s ORDER BY s.name |\n}\n");
        assert!(has(&found, &["ASC", "DESC", "LIMIT"]), "{found:?}");
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  R = SELECT s FROM Person:s ORDER BY s.name DESC |\n}\n");
        assert_eq!(found, ["LIMIT"]);
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  R = SELECT s FROM Person:s LIMIT 3 |\n}\n");
        assert_eq!(found, ["OFFSET"]);
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  R = SELECT s FROM Person:s LIMIT 3 OFFSET 1 |\n}\n");
        assert!(is_empty(&found), "{found:?}");
        // ASC is not a clause of a WHERE.
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  R = SELECT s FROM Person:s WHERE s.age > 1 |\n}\n");
        assert!(lacks(&found, &["ASC", "OFFSET"]), "{found:?}");
    }

    #[test]
    fn a_finished_sort_key_offers_directions_clauses_and_operators_only() {
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  R = SELECT s FROM Person:s ORDER BY s.name |\n}\n");
        assert!(has(&found, &["ASC", "DESC", "LIMIT", "AND", "IS NULL"]), "{found:?}");
        assert!(lacks(&found, &["abs", "Person", "s", "WHERE", "ACCUM"]), "{found:?}");
        assert!(found.len() < 20, "{found:?}");
        // Where an operand is expected the expressions stay.
        let found = travel("CREATE QUERY q() SYNTAX v2 {\n  R = SELECT s FROM Person:s ORDER BY |\n}\n");
        assert!(has(&found, &["abs", "s"]), "{found:?}");
    }

    #[test]
    fn reverse_steps_follow_the_endpoints() {
        let found = travel("CREATE QUERY q() {\n  R = SELECT t FROM City:s <-(|)- :t;\n}\n");
        assert!(has(&found, &["Lives"]), "{found:?}");
        assert!(lacks(&found, &["Friend", "In_country"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  R = SELECT t FROM City:s -(|)-> :t;\n}\n");
        assert!(has(&found, &["In_country"]), "{found:?}");
        assert!(lacks(&found, &["Lives"]), "{found:?}");
        // The vertex after a reverse step is the edge's source.
        let found = travel("CREATE QUERY q() {\n  R = SELECT t FROM City:s <-(Lives)- |\n}\n");
        assert!(has(&found, &["Person"]), "{found:?}");
        assert!(lacks(&found, &["Country"]), "{found:?}");
    }

    #[test]
    fn no_completion_in_unterminated_block_comments() {
        assert!(is_empty(&travel("CREATE QUERY q() {\n  /* foo |\n  PRINT 1;\n}\n")));
        assert!(is_empty(&travel("CREATE QUERY q() {\n  /* foo |*/\n  PRINT 1;\n}\n")));
        // After the comment ends, and for a comment-like text in a string or line comment.
        let found = travel("CREATE QUERY q() {\n  /* foo */ |\n}\n");
        assert!(!found.is_empty());
        let found = travel("CREATE QUERY q() {\n  PRINT \"/*\"; |\n}\n");
        assert!(has(&found, &["PRINT"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  // /*\n  |\n}\n");
        assert!(!found.is_empty());
    }

    #[test]
    fn job_bodies_offer_their_own_statements() {
        let found = travel("CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE FILENAME f1;\n  |\n");
        assert!(has(&found, &["DEFINE FILENAME", "LOAD", "DELETE VERTEX"]), "{found:?}");
        assert!(lacks(&found, &["SELECT", "WHILE", "abs"]), "{found:?}");
        let found = travel("CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE FILENAME f1;\n  L|\n");
        assert!(has(&found, &["LOAD"]), "{found:?}");
        let found = travel("CREATE SCHEMA_CHANGE JOB sj FOR GRAPH Travel {\n  |\n");
        assert!(has(&found, &["ALTER VERTEX", "ADD VERTEX", "DROP EDGE"]), "{found:?}");
        assert!(lacks(&found, &["SELECT", "abs"]), "{found:?}");
        let found = travel("CREATE GLOBAL SCHEMA_CHANGE JOB sj {\n  |\n");
        assert!(has(&found, &["ADD VERTEX"]), "{found:?}");
        // After a closed job the file is at the top level or in a query again.
        let found = travel("CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE FILENAME f1;\n}\n|");
        assert!(has(&found, &["CREATE QUERY"]), "{found:?}");
        let found = travel("CREATE LOADING JOB lj FOR GRAPH Travel {\n}\nCREATE QUERY q() {\n  |\n");
        assert!(has(&found, &["SELECT", "abs"]), "{found:?}");
        let found = travel("CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE |\n");
        assert_eq!(found, ["FILENAME", "HEADER", "INPUT_LINE_FILTER"]);
    }

    #[test]
    fn job_headers_in_comments_and_strings_do_not_open_a_job() {
        let found = travel("/* CREATE LOADING JOB lj FOR GRAPH Travel {\n*/\n|");
        assert!(has(&found, &["CREATE QUERY"]), "{found:?}");
        assert!(lacks(&found, &["DEFINE FILENAME", "LOAD"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  // CREATE LOADING JOB lj FOR GRAPH Travel {\n  |\n}\n");
        assert!(has(&found, &["SELECT", "abs"]), "{found:?}");
        assert!(lacks(&found, &["DEFINE FILENAME"]), "{found:?}");
        let found = travel("CREATE QUERY q() {\n  PRINT \"CREATE SCHEMA_CHANGE JOB sj {\";\n  |\n}\n");
        assert!(has(&found, &["SELECT", "abs"]), "{found:?}");
        assert!(lacks(&found, &["ALTER VERTEX", "ADD VERTEX"]), "{found:?}");
        // A commented-out query header does not hide the real job around the cursor.
        let found = travel("CREATE LOADING JOB lj FOR GRAPH Travel {\n  # CREATE QUERY q() {\n  |\n");
        assert!(has(&found, &["DEFINE FILENAME", "LOAD"]), "{found:?}");
    }

    #[test]
    fn run_loading_job_offers_file_variables_and_run_options() {
        let job = "CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE FILENAME f1;\n  DEFINE FILENAME f2;\n}\n";
        let found = travel(&format!("{job}RUN LOADING JOB lj USING |"));
        assert_eq!(found, ["f1", "f2", "CONCURRENCY", "BATCH_SIZE", "EOF"]);
        assert!(lacks(&found, &["SEPARATOR"]), "{found:?}");
        let found = travel(&format!("{job}RUN LOADING JOB -noprint lj USING f1=\"a,b.csv\", |"));
        assert_eq!(found, ["f2", "CONCURRENCY", "BATCH_SIZE", "EOF"]);
        let found = travel(&format!("{job}RUN LOADING JOB lj\n  USING f1=\"x\",\n  |"));
        assert_eq!(found, ["f2", "CONCURRENCY", "BATCH_SIZE", "EOF"]);
        // LOAD statements keep their own options.
        let found =
            travel("CREATE LOADING JOB lj FOR GRAPH Travel {\n  LOAD f1 TO VERTEX Person VALUES ($0) USING |\n}\n");
        assert!(has(&found, &["SEPARATOR"]), "{found:?}");
        // A later command does not continue the RUN.
        let found = travel(&format!("{job}RUN LOADING JOB lj USING f1=\"x\"\nCREATE VERTEX V (a INT, |"));
        assert!(lacks(&found, &["CONCURRENCY"]), "{found:?}");
    }

    #[test]
    fn completes_names_of_headers_temp_tables_and_columns() {
        let job =
            "CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE FILENAME f1;\n  DEFINE HEADER hh = \"id\", \"name\";\n";
        let found = travel(&format!("{job}  LOAD f1 TO VERTEX Person VALUES ($0) USING USER_DEFINED_HEADER=\"|"));
        assert_eq!(found, ["hh"]);
        let found = travel(&format!("{job}  LOAD f1 TO VERTEX Person VALUES ($\"|"));
        assert_eq!(found, ["id", "name"]);
        let found = travel(&format!("{job}  LOAD f1 TO TEMP_TABLE t1 (a) VALUES ($0);\n  LOAD TEMP_TABLE |"));
        assert_eq!(found, ["t1"]);
        let found = travel(&format!("{job}  LOAD f1 TO TEMP_TABLE |"));
        assert!(is_empty(&found), "{found:?}");
    }

    #[test]
    fn a_column_named_load_does_not_end_the_statement() {
        let job = "CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE FILENAME f1;\n  DEFINE HEADER h1 = \"id\", \"name\";\n  DEFINE HEADER hdr2 = \"cc\", \"load\";\n";
        let found = travel(&format!(
            "{job}  LOAD f1 TO VERTEX Person VALUES ($\"load\", $\"|) USING USER_DEFINED_HEADER=\"hdr2\";\n"
        ));
        assert_eq!(found, ["cc", "load"]);
    }

    #[test]
    fn columns_follow_the_header_a_load_selects() {
        let job = "CREATE LOADING JOB lj FOR GRAPH Travel {\n  DEFINE FILENAME f1;\n  DEFINE HEADER h1 = \"id\", \"name\";\n  DEFINE HEADER h2 = \"code\", \"name\";\n";
        let found =
            travel(&format!("{job}  LOAD f1 TO VERTEX Person VALUES ($\"|) USING USER_DEFINED_HEADER=\"h2\";\n"));
        assert_eq!(found, ["code", "name"]);
        let found = travel(&format!(
            "{job}  LOAD f1 TO VERTEX Person VALUES ($0, $\"|) USING user_defined_header = \"h1\", SEPARATOR=\",\";\n"
        ));
        assert_eq!(found, ["id", "name"]);
        // Only the LOAD around the cursor counts.
        let found = travel(&format!(
            "{job}  LOAD f1 TO VERTEX Person VALUES ($0) USING USER_DEFINED_HEADER=\"h1\";\n  LOAD f1 TO VERTEX City VALUES ($\"|);\n"
        ));
        assert_eq!(found, ["id", "name", "code"]);
        let found = travel(&format!(
            "{job}  LOAD f1 TO VERTEX City VALUES ($\"|\n  LOAD f1 TO VERTEX Person VALUES ($0) USING USER_DEFINED_HEADER=\"h1\";\n"
        ));
        assert_eq!(found, ["id", "name", "code"]);
        // An earlier LOAD without its `;` does not select the header of the one at the cursor.
        let found = travel(&format!(
            "{job}  LOAD f1 TO VERTEX Person VALUES ($\"id\") USING USER_DEFINED_HEADER=\"h1\"\n  LOAD f1 TO VERTEX City VALUES ($\"|);\n"
        ));
        assert_eq!(found, ["id", "name", "code"]);
        // Text inside a string does not select anything either.
        let found = travel(&format!(
            "{job}  LOAD f1 TO VERTEX Person VALUES ($\"|) USING SEPARATOR=\"USER_DEFINED_HEADER=\\\"h1\\\"\";\n"
        ));
        assert_eq!(found, ["id", "name", "code"]);
        // An unknown header selects nothing.
        let found =
            travel(&format!("{job}  LOAD f1 TO VERTEX Person VALUES ($\"|) USING USER_DEFINED_HEADER=\"nope\";\n"));
        assert_eq!(found, ["id", "name", "code"]);
    }

    #[test]
    fn replaces_typed_accumulator_prefix() {
        let (text, offset) = cursor("CREATE QUERY q() {\n  SumAccum<INT> @@total;\n  @@to|\n}\n");
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let items = completion(&snapshot, snapshot.position(offset), true).items;
        let item = items.iter().find(|i| i.label == "@@total").unwrap();
        let edit = item.text_edit.as_ref().unwrap();
        assert_eq!(edit.new_text, "@@total");
        assert_eq!(edit.range, Range::new(Position::new(2, 2), Position::new(2, 6)));
    }

    #[test]
    fn an_unclosed_accumulator_type_before_the_cursor_does_not_panic() {
        let text = "CREATE QUERY q() { SumAccum<INT> @@a; SumAccum<";
        assert!(text_accumulators(text, 17, text.len(), true).iter().any(|(n, ..)| n == "@@a"));
    }
}

#[cfg(test)]
mod pattern_tests {
    use super::*;
    use crate::features::test_support::Fixture;

    const TRAVEL: &str = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE VERTEX City (PRIMARY_ID id STRING, population INT)\nCREATE VERTEX Country (PRIMARY_ID id STRING)\nCREATE DIRECTED EDGE Lives (FROM Person, TO City, since INT) WITH REVERSE_EDGE=\"Housed\"\nCREATE UNDIRECTED EDGE Friend (FROM Person, TO Person, weight INT)\nCREATE DIRECTED EDGE In_country (FROM City, TO Country)\nCREATE GRAPH Travel (Person, City, Country, Lives, Friend, In_country)\n";

    /// Completion at `@` in a query body (a marker that patterns do not use).
    fn at(body: &str) -> Vec<String> {
        let marker = body.find('@').expect("cursor marker");
        let head = "CREATE QUERY q() FOR GRAPH Travel SYNTAX v3 {\n";
        let text = format!("{head}{}{}\n}}\n", &body[..marker], &body[marker + 1..]);
        let fixture = Fixture::with_files(&text, &[("file:///test/schema.gsql", TRAVEL)]);
        let snapshot = fixture.snapshot();
        completion(&snapshot, snapshot.position(head.len() + marker), true).items.into_iter().map(|i| i.label).collect()
    }

    fn has(found: &[String], wanted: &[&str]) -> bool {
        wanted.iter().all(|w| found.iter().any(|l| l == w))
    }

    fn lacks(found: &[String], unwanted: &[&str]) -> bool {
        unwanted.iter().all(|w| !found.iter().any(|l| l == w))
    }

    #[test]
    fn bracket_edge_types_follow_the_source_vertex() {
        let found = at("  R = SELECT t FROM (s:Person) -[e:@]- (t);");
        assert!(has(&found, &["Lives", "Friend", "ANY"]), "{found:?}");
        assert!(lacks(&found, &["In_country", "Person", "abs"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s:Person) -[:@]-> (t);");
        assert!(has(&found, &["Lives", "Friend"]), "{found:?}");
        assert!(lacks(&found, &["Housed", "In_country"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s:Person) <-[:@]- (t);");
        assert!(has(&found, &["Housed", "Friend"]), "{found:?}");
        assert!(lacks(&found, &["Lives", "In_country"]), "{found:?}");
        // After `|` and with a prefix typed.
        let found = at("  R = SELECT t FROM (s:Person) -[:Lives|Fr@]-> (t);");
        assert!(has(&found, &["Friend"]), "{found:?}");
        assert!(lacks(&found, &["In_country"]), "{found:?}");
        // The source can be a vertex set variable or unknown.
        let found = at("  S = {City.*};\n  R = SELECT t FROM (s:S) -[:@]-> (t);");
        assert!(has(&found, &["In_country"]), "{found:?}");
        assert!(lacks(&found, &["Friend"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s) -[:@]-> (t);");
        assert!(has(&found, &["Lives", "In_country"]), "{found:?}");
        // Later steps start at the previous node.
        let found = at("  R = SELECT t FROM (s:Person) -[:Lives]-> (c:City) -[:@]-> (t);");
        assert!(has(&found, &["In_country"]), "{found:?}");
        assert!(lacks(&found, &["Friend"]), "{found:?}");
    }

    #[test]
    fn node_labels_follow_the_edge() {
        let found = at("  R = SELECT t FROM (s:Person) -[:Lives]-> (t:@);");
        assert!(has(&found, &["City", "ANY"]), "{found:?}");
        assert!(lacks(&found, &["Person", "Country", "abs"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s:Person) -[:Lives|Friend]-> (t:@);");
        assert!(has(&found, &["City", "Person"]), "{found:?}");
        assert!(lacks(&found, &["Country"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s:City) <-[:Lives]- (t:Pe@);");
        assert!(has(&found, &["Person"]), "{found:?}");
        assert!(lacks(&found, &["Country"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s:Person) -[:Lives]-> (t:City|@);");
        assert!(has(&found, &["City"]), "{found:?}");
        assert!(lacks(&found, &["Country"]), "{found:?}");
    }

    #[test]
    fn first_node_labels_are_vertex_types_and_sets() {
        let found = at("  S = {City.*};\n  R = SELECT t FROM (s:@) -[:Lives]-> (t);");
        assert!(has(&found, &["Person", "City", "Country", "S"]), "{found:?}");
        assert!(lacks(&found, &["Lives", "abs"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s:Person|Ci@) -[:Lives]-> (t);");
        assert!(has(&found, &["City"]), "{found:?}");
        // A comma starts another pattern.
        let found = at("  R = SELECT t FROM (s:Person) -[:Friend]- (t), (u:@);");
        assert!(has(&found, &["Person", "Country"]), "{found:?}");
    }

    #[test]
    fn v2_edge_steps_are_not_taken_for_node_patterns() {
        // `-(Friend:e)-` is a V2 step: the colon names an alias.
        let found = at("  R = SELECT t FROM Person:s -(Friend:@)- Person:t;");
        assert!(found.is_empty(), "{found:?}");
        let found = at("  R = SELECT t FROM Person:s -(@)- Person:t;");
        assert!(has(&found, &["Lives", "Friend"]), "{found:?}");
        let found = at("  PRINT abs(1) + @;");
        assert!(has(&found, &["abs"]), "{found:?}");
    }

    #[test]
    fn property_maps_offer_attributes_of_the_labelled_type() {
        let found = at("  R = SELECT t FROM (p:Person {na@}) -[:Lives]-> (t);");
        assert!(has(&found, &["name", "age", "id"]), "{found:?}");
        assert!(lacks(&found, &["population", "abs", "Person", "p"]), "{found:?}");
        let found = at("  R = SELECT t FROM (p:Person {name: \"x, y\", ag@}) -[:Lives]-> (t);");
        assert!(has(&found, &["age"]), "{found:?}");
        assert!(lacks(&found, &["name", "Person", "City"]), "{found:?}");
        let found = at("  R = SELECT t FROM (p:Person) -[e:Lives {si@}]-> (t);");
        assert!(has(&found, &["since"]), "{found:?}");
        assert!(lacks(&found, &["name", "abs"]), "{found:?}");
        let found = at("  R = SELECT t FROM (p:Person|City {@}) -[:Lives]-> (t);");
        assert!(has(&found, &["name", "population"]), "{found:?}");
        // A value position and an unlabelled node get no attribute names.
        let found = at("  R = SELECT t FROM (p:Person {name: @}) -[:Lives]-> (t);");
        assert!(lacks(&found, &["age"]), "{found:?}");
        let found = at("  R = SELECT t FROM (p {na@}) -[:Lives]-> (t);");
        assert!(lacks(&found, &["name", "abs"]), "{found:?}");
    }

    #[test]
    fn pattern_scans_skip_comments_and_strings() {
        let found = at("  R = SELECT t FROM (p:Person {name: \"a\" /* } */, ag@}) -[:Lives]-> (t);");
        assert!(has(&found, &["age"]), "{found:?}");
        let found = at("  R = SELECT t FROM (p:Person {name: \"a\", // }\n ag@}) -[:Lives]-> (t);");
        assert!(has(&found, &["age"]), "{found:?}");
        // A bracket in a comment does not move the previous node.
        let found = at("  R = SELECT t FROM (s:Person {name: \"x\" /* ) */}) -[:Lives]-> (c:City) -[:@]-> (t);");
        assert!(has(&found, &["In_country"]), "{found:?}");
        assert!(lacks(&found, &["Friend"]), "{found:?}");
    }

    #[test]
    fn dot_on_edge_aliases_offers_edge_members_only() {
        let found = at("  R = SELECT t FROM (s:Person) -[e:Lives]-> (t) ACCUM e.@;");
        assert!(has(&found, &["since", "isDirected", "type"]), "{found:?}");
        assert!(lacks(&found, &["name", "age", "population", "outdegree", "weight"]), "{found:?}");
        let found = at("  R = SELECT t FROM (s:Person) -[e:Lives|Friend]-> (t) ACCUM e.@;");
        assert!(has(&found, &["since", "weight", "isDirected"]), "{found:?}");
        assert!(lacks(&found, &["name", "outdegree", "abs"]), "{found:?}");
        let found = at("  R = SELECT t FROM Person:s -((Lives>|Friend):e)- :t ACCUM e.@;");
        assert!(has(&found, &["since", "weight", "isDirected"]), "{found:?}");
        assert!(lacks(&found, &["name", "outdegree", "abs"]), "{found:?}");
        // Vertex aliases keep their attributes.
        let found = at("  R = SELECT t FROM (s:Person) -[e:Lives]-> (t:City) ACCUM t.@;");
        assert!(has(&found, &["population", "outdegree"]), "{found:?}");
        assert!(lacks(&found, &["since", "name"]), "{found:?}");
    }

    #[test]
    fn dot_behind_an_alternation_offers_the_union_of_the_end_attributes() {
        for body in [
            "  R = SELECT t FROM Person:s -(Lives>|Friend)- :t ACCUM t.@;",
            "  R = SELECT t FROM (s:Person) -[:Lives|Friend]-> (t) ACCUM t.@;",
        ] {
            let found = at(body);
            assert!(has(&found, &["population", "name", "age"]), "{body}: {found:?}");
        }
        for body in [
            "  R = SELECT t FROM Person:s -(Lives>|Friend)- :t ACCUM t.na@;",
            "  R = SELECT t FROM (s:Person) -[:Lives|Friend]-> (t) ACCUM t.na@;",
        ] {
            let found = at(body);
            assert!(has(&found, &["population", "name"]), "{body}: {found:?}");
        }
        // City and Country only: Person attributes are not offered.
        for body in [
            "  R = SELECT t FROM Person:s -(Lives>|In_country>)- :t ACCUM t.@;",
            "  R = SELECT t FROM City:s -(Lives>|In_country>)- :t ACCUM t.na@;",
            "  R = SELECT t FROM (s:Person) -[:Lives|In_country]-> (t) ACCUM t.@;",
            "  R = SELECT t FROM (s:Person) -[e:Lives|In_country]-> (t) ACCUM t.na@;",
        ] {
            let found = at(body);
            assert!(has(&found, &["population"]) && lacks(&found, &["name", "age"]), "{body}: {found:?}");
        }
        // Repetitions and wildcards stay undecided.
        for body in [
            "  R = SELECT t FROM (s:Person) -[:Lives*1..2]-> (t) ACCUM t.@;",
            "  R = SELECT t FROM (s:Person) -[:_]-> (t) ACCUM t.@;",
            "  R = SELECT t FROM Person:s -(_>)- :t ACCUM t.@;",
        ] {
            let found = at(body);
            assert!(has(&found, &["population", "name"]), "{body}: {found:?}");
        }
        // One edge: its end only.
        let found = at("  R = SELECT t FROM (s:Person) -[:Lives]-> (t) ACCUM t.@;");
        assert!(has(&found, &["population"]) && lacks(&found, &["name", "age"]), "{found:?}");
    }

    #[test]
    fn member_access_never_lists_expressions() {
        let found = at("  R = SELECT t FROM Person:s -(Lives>|Friend)- :t ACCUM t.@;");
        assert!(lacks(&found, &["abs", "s", "t"]), "{found:?}");
        let found = at("  R = SELECT t FROM Person:s -(Lives>|Friend)- :t WHERE nothere.@;");
        assert!(lacks(&found, &["abs", "s", "t"]), "{found:?}");
    }

    #[test]
    fn aliases_are_offered_before_an_expression_is_typed() {
        let found = at("  R = SELECT p FROM Person:p\n      WHERE @\n      ;");
        assert!(has(&found, &["p", "abs"]), "{found:?}");
        let found = at("  R = SELECT p FROM Person:p\n      WHERE p.age > 1 AND @\n      ;");
        assert!(has(&found, &["p"]), "{found:?}");
        let found = at("  SumAccum<INT> @@a;\n  R = SELECT m FROM (p:Person)-[e:Lives]->(m:City)\n      WHERE @");
        assert!(has(&found, &["p", "e", "m"]), "{found:?}");
        let found =
            at("  SumAccum<INT> @@a;\n  R = SELECT m FROM (p:Person)-[e:Lives]->(m:City)\n      ACCUM @@a += @");
        assert!(has(&found, &["p", "e", "m"]), "{found:?}");
        // Aliases of an earlier, finished statement are not in scope.
        let found = at("  R = SELECT p FROM Person:p;\n  PRINT @");
        assert!(lacks(&found, &["p"]), "{found:?}");
    }
}
