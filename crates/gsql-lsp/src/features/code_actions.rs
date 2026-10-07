//! Quick fixes for diagnostics produced by this server.

use std::collections::BTreeMap;

use tree_sitter::Node;

use crate::analysis::{SymbolKind, Ty};
use crate::features::Snapshot;
use crate::features::autocorrect::similar;
use crate::lsp::types::{CodeAction, Diagnostic, Range, TextEdit, WorkspaceEdit};
use crate::syntax;
use crate::text::Span;

/// Quick fixes for the diagnostics in `range`, and a "fix all" action built
/// from all of the document's diagnostics (`document_diagnostics`, as last
/// published). `only` restricts the kinds of action, as in the LSP request.
pub fn code_actions(
    snapshot: &Snapshot,
    range: Range,
    diagnostics: &[Diagnostic],
    only: Option<&[String]>,
    document_diagnostics: &[Diagnostic],
) -> Vec<CodeAction> {
    let wants =
        |kind: &str| only.is_none_or(|kinds| kinds.iter().any(|k| kind == k || kind.starts_with(&format!("{k}."))));
    let mut actions = Vec::new();
    if wants("quickfix") {
        actions.extend(quick_fixes(snapshot, range, diagnostics));
    }
    if wants("source.fixAll") {
        actions.extend(fix_all(snapshot, document_diagnostics));
    }
    actions
}

/// Fixes attached to a diagnostic's data by the diagnostics feature.
fn attached_fixes(diagnostic: &Diagnostic) -> Vec<(String, Vec<TextEdit>, bool)> {
    let Some(fixes) = diagnostic.data.as_ref().and_then(|d| d.get("fixes")).and_then(|f| f.as_array()) else {
        return Vec::new();
    };
    fixes
        .iter()
        .filter_map(|fix| {
            let title = fix.get("title")?.as_str()?.to_string();
            let edits: Vec<TextEdit> = serde_json::from_value(fix.get("edits")?.clone()).ok()?;
            let safe = fix.get("safe").and_then(|s| s.as_bool()).unwrap_or(false);
            Some((title, edits, safe))
        })
        .collect()
}

/// Applies every fix that is safe without review: accumulator name case,
/// `=`/`<>` outside SYNTAX V3, and the repairs of well-understood syntax
/// mistakes (`ELSEIF`, `POST ACCUM`, `= value` in attribute lists, trailing commas).
/// The edits of the fixes that are certain, without overlaps.
fn certain_edits(document_diagnostics: &[Diagnostic]) -> Vec<TextEdit> {
    let mut edits: Vec<TextEdit> = Vec::new();
    for diagnostic in document_diagnostics {
        match diagnostic.code.as_deref() {
            Some("accumulator-case" | "v3-comparison") => {
                if let Some(replacement) = diagnostic.data.as_ref().and_then(|d| d["replacement"].as_str()) {
                    edits.push(TextEdit { range: diagnostic.range, new_text: replacement.to_string() });
                }
            }
            _ => {
                if let Some((_, fix, _)) = attached_fixes(diagnostic).into_iter().find(|(_, _, safe)| *safe) {
                    edits.extend(fix);
                }
            }
        }
    }
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    edits.dedup();
    let mut kept: Vec<TextEdit> = Vec::new();
    for edit in edits {
        if kept.last().is_none_or(|last| last.range.end <= edit.range.start) {
            kept.push(edit);
        }
    }
    kept
}

/// `text` with `edits` applied.
fn apply_edits(snapshot: &Snapshot, text: &str, source: &crate::text::SourceText, edits: &[TextEdit]) -> String {
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                source.offset(e.range.start, snapshot.encoding),
                source.offset(e.range.end, snapshot.encoding),
                e.new_text.as_str(),
            )
        })
        .collect();
    spans.sort_by_key(|s| std::cmp::Reverse(s.0));
    let mut result = text.to_string();
    for (start, end, new_text) in spans {
        result.replace_range(start..end, new_text);
    }
    result
}

/// All the certain fixes. A mistake can hide others (a misspelled keyword
/// makes the errors of its statement look like its consequences), so the
/// fixed text is checked again, a few times, and the fixes found then are
/// included: one run gets as far as repeated runs would.
fn fix_all(snapshot: &Snapshot, document_diagnostics: &[Diagnostic]) -> Option<CodeAction> {
    const MAX_ROUNDS: usize = 4;
    let first = certain_edits(document_diagnostics);
    if first.is_empty() {
        return None;
    }
    let original = snapshot.text();
    let mut text = apply_edits(snapshot, original, snapshot.source, &first);
    let mut more = false;
    for _ in 1..MAX_ROUNDS {
        let source = crate::text::SourceText::new(text.clone());
        let tree = syntax::parse(&mut syntax::new_parser(), &text, None);
        let analysis = crate::analysis::analyze(&tree, &text);
        let next = Snapshot { source: &source, tree: &tree, analysis: &analysis, ..*snapshot };
        let edits = certain_edits(&super::diagnostics::diagnostics(&next));
        if edits.is_empty() {
            break;
        }
        text = apply_edits(&next, &text, &source, &edits);
        more = true;
    }
    let kept = if more {
        // One edit from the first to the last difference.
        let prefix = original.bytes().zip(text.bytes()).take_while(|(a, b)| a == b).count();
        let prefix =
            (0..=prefix).rev().find(|&i| original.is_char_boundary(i) && text.is_char_boundary(i)).unwrap_or(0);
        let limit = original.len().min(text.len()) - prefix;
        let suffix = original.bytes().rev().zip(text.bytes().rev()).take(limit).take_while(|(a, b)| a == b).count();
        let suffix = (0..=suffix)
            .rev()
            .find(|&i| original.is_char_boundary(original.len() - i) && text.is_char_boundary(text.len() - i))
            .unwrap_or(0);
        let span = Span::new(prefix, original.len() - suffix);
        vec![TextEdit { range: snapshot.range(span), new_text: text[prefix..text.len() - suffix].to_string() }]
    } else {
        first
    };
    let mut changes = BTreeMap::new();
    changes.insert(snapshot.uri.to_string(), kept);
    Some(CodeAction {
        title: "Fix all auto-fixable problems".into(),
        kind: "source.fixAll",
        diagnostics: Vec::new(),
        is_preferred: None,
        edit: WorkspaceEdit { changes },
    })
}

fn quick_fixes(snapshot: &Snapshot, range: Range, diagnostics: &[Diagnostic]) -> Vec<CodeAction> {
    let mut actions = Vec::new();
    for diagnostic in diagnostics {
        if diagnostic.source.as_deref() != Some("gsql") {
            continue;
        }
        let overlaps = diagnostic.range.start <= range.end && range.start <= diagnostic.range.end;
        if !overlaps {
            continue;
        }
        match diagnostic.code.as_deref() {
            Some("undeclared-accumulator") => {
                if let Some(action) = declare_accumulator(snapshot, diagnostic) {
                    actions.push(action);
                }
            }
            Some("unknown-type" | "unknown-attribute" | "undefined-name") => {
                actions.extend(did_you_mean(snapshot, diagnostic));
            }
            Some("v3-comparison" | "accumulator-case") => {
                if let Some(replacement) = diagnostic.data.as_ref().and_then(|d| d["replacement"].as_str()) {
                    actions.push(CodeAction {
                        title: format!("Replace with `{replacement}`"),
                        kind: "quickfix",
                        diagnostics: vec![diagnostic.clone()],
                        is_preferred: Some(true),
                        edit: edit(snapshot, diagnostic.range, replacement.to_string()),
                    });
                }
                if diagnostic.code.as_deref() == Some("v3-comparison") {
                    actions.extend(use_syntax_v3(snapshot, diagnostic));
                }
            }
            Some("cypher-syntax") => actions.extend(use_syntax_v3(snapshot, diagnostic)),
            Some("float-equality") => actions.extend(tolerance_comparison(snapshot, diagnostic)),
            Some("reserved-word") => actions.extend(rename_reserved(snapshot, diagnostic)),
            Some("unused") => actions.extend(remove_unused(snapshot, diagnostic)),
            _ => {}
        }
        let fixes = attached_fixes(diagnostic);
        let single = fixes.len() == 1;
        for (title, edits, _) in fixes {
            let mut changes = BTreeMap::new();
            changes.insert(snapshot.uri.to_string(), edits);
            actions.push(CodeAction {
                title,
                kind: "quickfix",
                diagnostics: vec![diagnostic.clone()],
                is_preferred: single.then_some(true),
                edit: WorkspaceEdit { changes },
            });
        }
    }
    actions
}

fn edit(snapshot: &Snapshot, range: Range, new_text: String) -> WorkspaceEdit {
    let mut changes = BTreeMap::new();
    changes.insert(snapshot.uri.to_string(), vec![TextEdit { range, new_text }]);
    WorkspaceEdit { changes }
}

fn declare_accumulator(snapshot: &Snapshot, diagnostic: &Diagnostic) -> Option<CodeAction> {
    let offset = snapshot.offset(diagnostic.range.start);
    let reference = snapshot.analysis.reference_at(offset)?;
    let name = reference.name.clone();
    let root = snapshot.root();
    let node = root.descendant_for_byte_range(offset, offset)?;
    let body = syntax::ancestors(node).find(|n| n.kind() == "query_body")?;
    let accumulator_type = guess_accumulator_type(snapshot, body, &name);
    let declaration = format!("{accumulator_type} {name};");
    let (insert_at, text) = declaration_site(snapshot, body, &declaration);
    let position = snapshot.position(insert_at);
    Some(CodeAction {
        title: format!("Declare `{accumulator_type} {name}`"),
        kind: "quickfix",
        diagnostics: vec![diagnostic.clone()],
        is_preferred: Some(true),
        edit: edit(snapshot, Range::new(position, position), text),
    })
}

/// Where to insert a declaration at the start of a query body, and the text
/// to insert: before the first statement after the TYPEDEFs (tuples must be
/// defined first), together with the comments above that statement.
fn declaration_site(snapshot: &Snapshot, body: Node, declaration: &str) -> (usize, String) {
    let source = snapshot.text();
    let lines = &snapshot.source.lines;
    let children = syntax::children(body);
    let statements: Vec<usize> =
        (0..children.len()).filter(|&i| children[i].is_named() && children[i].kind() != "comment").collect();
    let typedefs = statements.iter().take_while(|&&i| children[i].kind() == "typedef_statement").count();
    let indent_of = |node: Node| {
        let line_start = lines.line_start(lines.line_of(node.start_byte()));
        let before = &source[line_start..node.start_byte()];
        before.trim().is_empty().then(|| (line_start, before.to_string()))
    };
    if let Some(&index) = statements.get(typedefs) {
        // The comments directly above the statement stay with it.
        let mut first = index;
        while first > 0
            && children[first - 1].kind() == "comment"
            && lines.line_of(children[first - 1].end_byte()) + 1 >= lines.line_of(children[first].start_byte())
            && indent_of(children[first - 1]).is_some()
        {
            first -= 1;
        }
        return match indent_of(children[first]) {
            Some((line_start, indent)) => (line_start, format!("{indent}{declaration}\n")),
            // The statement shares its line with the opening brace or a TYPEDEF.
            None => (children[first].start_byte(), format!("{declaration} ")),
        };
    }
    if let Some(&last) = typedefs.checked_sub(1).and_then(|i| statements.get(i)) {
        let end =
            children.get(last + 1).filter(|c| c.kind() == ";").map_or(children[last].end_byte(), |c| c.end_byte());
        let indent = indent_of(children[last]).map(|(_, indent)| indent).unwrap_or_else(|| "  ".into());
        return (end, format!("\n{indent}{declaration}"));
    }
    (body.start_byte() + 1, format!("\n  {declaration}"))
}

/// Guesses an accumulator type from how the accumulator is used in a query.
fn guess_accumulator_type(snapshot: &Snapshot, body: Node, name: &str) -> String {
    let source = snapshot.text();
    let mut guess: Option<String> = None;
    syntax::walk(body, |node| {
        if guess.is_some() || node.kind() != "assignment_statement" {
            return;
        }
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let target = match left.kind() {
            "member_expression" => left.child_by_field_name("property"),
            _ => Some(left),
        };
        if target.map(|t| syntax::text(t, source)) != Some(name) {
            return;
        }
        let Some(right) = node.child_by_field_name("right") else {
            return;
        };
        guess = Some(type_for_value(snapshot, right));
    });
    guess.unwrap_or_else(|| "SumAccum<INT>".to_string())
}

fn scalar_type(snapshot: &Snapshot, node: Node) -> Option<&'static str> {
    match node.kind() {
        "integer" => Some("INT"),
        "float" => Some("DOUBLE"),
        "string" => Some("STRING"),
        "boolean" => Some("BOOL"),
        "binary_expression" => {
            let left = node.child_by_field_name("left").and_then(|l| scalar_type(snapshot, l));
            let right = node.child_by_field_name("right").and_then(|r| scalar_type(snapshot, r));
            match (left, right) {
                (Some("DOUBLE"), _) | (_, Some("DOUBLE")) => Some("DOUBLE"),
                (Some(t), _) | (_, Some(t)) => Some(t),
                _ => None,
            }
        }
        "identifier" => {
            let reference = snapshot.analysis.reference_at(node.start_byte())?;
            let symbol = &snapshot.analysis.symbols[reference.target?];
            match &symbol.ty {
                Ty::Vertex(_) => Some("VERTEX"),
                Ty::Edge(_) => Some("EDGE"),
                Ty::Primitive(p) if p == "INT" || p == "UINT" => Some("INT"),
                Ty::Primitive(p) if p == "FLOAT" || p == "DOUBLE" => Some("DOUBLE"),
                Ty::Primitive(p) if p.starts_with("STRING") => Some("STRING"),
                Ty::Primitive(p) if p == "BOOL" => Some("BOOL"),
                _ => None,
            }
        }
        _ => None,
    }
}

fn type_for_value(snapshot: &Snapshot, value: Node) -> String {
    match value.kind() {
        "key_value_pair" => {
            let children = syntax::code_children(value);
            let key = children.first().and_then(|k| scalar_type(snapshot, *k)).unwrap_or("STRING");
            let value_type = children.last().and_then(|v| scalar_type(snapshot, *v)).unwrap_or("INT");
            let inner = match value_type {
                "VERTEX" | "EDGE" => format!("SetAccum<{value_type}>"),
                "BOOL" => "OrAccum".to_string(),
                other => format!("SumAccum<{other}>"),
            };
            format!("MapAccum<{key}, {inner}>")
        }
        _ => match scalar_type(snapshot, value) {
            Some("VERTEX") => "SetAccum<VERTEX>".into(),
            Some("EDGE") => "SetAccum<EDGE>".into(),
            Some("BOOL") => "OrAccum".into(),
            Some(scalar) => format!("SumAccum<{scalar}>"),
            None => "SumAccum<INT>".into(),
        },
    }
}

/// `a == b` -> `abs(a - b) < 0.0001` for floating-point values.
fn tolerance_comparison(snapshot: &Snapshot, diagnostic: &Diagnostic) -> Option<CodeAction> {
    let source = snapshot.text();
    let offset = snapshot.offset(diagnostic.range.start);
    let operator = snapshot.root().descendant_for_byte_range(offset, offset)?;
    let comparison = operator.parent().filter(|p| p.kind() == "binary_expression")?;
    let operand = |node: Node| {
        let text = syntax::text(node, source);
        if matches!(node.kind(), "binary_expression" | "unary_expression") { format!("({text})") } else { text.into() }
    };
    let new_text = format!(
        "abs({} - {}) < 0.0001",
        operand(comparison.child_by_field_name("left")?),
        operand(comparison.child_by_field_name("right")?)
    );
    Some(CodeAction {
        title: format!("Compare with a tolerance: `{new_text}`"),
        kind: "quickfix",
        diagnostics: vec![diagnostic.clone()],
        is_preferred: Some(true),
        edit: edit(snapshot, snapshot.range(Span::of(comparison)), new_text),
    })
}

/// Declares the enclosing query `SYNTAX V3` (or changes its SYNTAX clause).
fn use_syntax_v3(snapshot: &Snapshot, diagnostic: &Diagnostic) -> Option<CodeAction> {
    let source = snapshot.text();
    let offset = snapshot.offset(diagnostic.range.start);
    let node = snapshot.root().descendant_for_byte_range(offset, offset)?;
    let query =
        syntax::ancestors(node).find(|a| matches!(a.kind(), "query_definition" | "interpret_query_statement"))?;
    let existing = syntax::named_children(query).into_iter().find(|c| c.kind() == "syntax_clause");
    let (title, range, new_text) = match existing {
        Some(clause) => {
            let version = clause.child_by_field_name("version")?;
            let text = if version.kind() == "string" { "\"v3\"" } else { "V3" };
            ("Change the query to SYNTAX V3", snapshot.range(Span::of(version)), text.to_string())
        }
        None => {
            let body = query.child_by_field_name("body")?;
            let at = body.start_byte();
            let spaced = source[..at].ends_with(char::is_whitespace);
            let text = if spaced { "SYNTAX V3 " } else { " SYNTAX V3 " };
            let position = snapshot.position(at);
            ("Declare the query SYNTAX V3", Range::new(position, position), text.to_string())
        }
    };
    Some(CodeAction {
        title: title.into(),
        kind: "quickfix",
        diagnostics: vec![diagnostic.clone()],
        is_preferred: None,
        edit: edit(snapshot, range, new_text),
    })
}

/// Renames a name that is a reserved word, everywhere it is used.
fn rename_reserved(snapshot: &Snapshot, diagnostic: &Diagnostic) -> Option<CodeAction> {
    let reference = snapshot.analysis.reference_at(snapshot.offset(diagnostic.range.start))?;
    let new_name = format!("{}_", reference.name);
    let edit = crate::features::navigation::rename(snapshot, diagnostic.range.start, &new_name).ok()?;
    Some(CodeAction {
        title: format!("Rename to `{new_name}`"),
        kind: "quickfix",
        diagnostics: vec![diagnostic.clone()],
        is_preferred: None,
        edit,
    })
}

/// Removes an unused accumulator or variable declaration.
fn remove_unused(snapshot: &Snapshot, diagnostic: &Diagnostic) -> Option<CodeAction> {
    let source = snapshot.text();
    let offset = snapshot.offset(diagnostic.range.start);
    let name = snapshot.root().descendant_for_byte_range(offset, offset)?;
    let declarator = syntax::self_and_ancestors(name)
        .find(|a| matches!(a.kind(), "accumulator_declarator" | "variable_declarator"))?;
    let declaration = declarator.parent()?;
    let declarators: Vec<Node> =
        syntax::named_children(declaration).into_iter().filter(|c| c.kind() == declarator.kind()).collect();
    let index = declarators.iter().position(|d| d.id() == declarator.id())?;
    let span = if declarators.len() > 1 {
        // `SumAccum<INT> @@a, @@b;`: remove the declarator and a comma.
        match index {
            0 => Span::new(declarator.start_byte(), declarators[1].start_byte()),
            _ => Span::new(declarators[index - 1].end_byte(), declarator.end_byte()),
        }
    } else {
        // The whole statement; only statements ending in `;` (not ACCUM items).
        let semicolon = declaration.next_sibling().filter(|n| n.kind() == ";")?;
        let lines = &snapshot.source.lines;
        let line_start = lines.line_start(lines.line_of(declaration.start_byte()));
        let row_end = lines.line_of(semicolon.end_byte());
        let line_end = lines.line_end(source, row_end);
        let alone = source[line_start..declaration.start_byte()].trim().is_empty()
            && source[semicolon.end_byte()..line_end].trim().is_empty();
        if alone {
            let next_line = if row_end + 1 < lines.line_count() { lines.line_start(row_end + 1) } else { line_end };
            Span::new(line_start, next_line)
        } else {
            // `INT x = 1; INT y = 2;`: take the blanks up to the next statement too.
            let after = &source[semicolon.end_byte()..line_end];
            let blanks = after.len() - after.trim_start_matches([' ', '\t']).len();
            let end = if blanks < after.len() { semicolon.end_byte() + blanks } else { semicolon.end_byte() };
            Span::new(declaration.start_byte(), end)
        }
    };
    Some(CodeAction {
        title: format!("Remove unused `{}`", syntax::text(name, source)),
        kind: "quickfix",
        diagnostics: vec![diagnostic.clone()],
        is_preferred: None,
        edit: edit(snapshot, snapshot.range(span), String::new()),
    })
}

fn did_you_mean(snapshot: &Snapshot, diagnostic: &Diagnostic) -> Vec<CodeAction> {
    let offset = snapshot.offset(diagnostic.range.start);
    let Some(reference) = snapshot.analysis.reference_at(offset) else {
        return Vec::new();
    };
    let workspace = snapshot.workspace;
    let candidates: Vec<String> = match diagnostic.code.as_deref() {
        Some("unknown-type") => {
            let mut names: Vec<String> = Vec::new();
            for kind in [SymbolKind::VertexType, SymbolKind::EdgeType] {
                names.extend(workspace.of_kind(kind).into_iter().map(|s| s.name.clone()));
            }
            names.extend(
                snapshot
                    .analysis
                    .visible_symbols(offset)
                    .into_iter()
                    .filter(|s| s.kind == SymbolKind::VertexSet)
                    .map(|s| s.name.clone()),
            );
            names
        }
        Some("unknown-attribute") => match &reference.role {
            crate::analysis::Role::Attribute(Ty::Vertex(owners) | Ty::VertexSet(owners) | Ty::Edge(owners)) => {
                owners.iter().flat_map(|o| workspace.attributes(Some(o))).map(|a| a.name.clone()).collect()
            }
            _ => Vec::new(),
        },
        _ => snapshot
            .analysis
            .visible_symbols(offset)
            .into_iter()
            .filter(|s| !s.name.starts_with('@'))
            .map(|s| s.name.clone())
            .collect(),
    };
    // The ranking of the diagnostics' "did you mean", so a suggestion always
    // has its fix (transpositions included).
    similar(&reference.name, candidates.iter().map(String::as_str))
        .into_iter()
        .take(3)
        .map(|name| CodeAction {
            title: format!("Change to `{name}`"),
            kind: "quickfix",
            diagnostics: vec![diagnostic.clone()],
            is_preferred: None,
            edit: edit(snapshot, snapshot.range(reference.span), name.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::diagnostics::diagnostics;
    use crate::features::test_support::Fixture;

    fn actions(text: &str, others: &[(&str, &str)]) -> Vec<CodeAction> {
        let fixture = Fixture::with_files(text, others);
        let snapshot = fixture.snapshot();
        let found = diagnostics(&snapshot);
        let whole = Range::new(snapshot.position(0), snapshot.position(text.len()));
        code_actions(&snapshot, whole, &found, None, &found)
    }

    /// The text after applying the edits of the action titled `title`.
    fn apply(text: &str, title: &str) -> String {
        let found = actions(text, &[]);
        let action = found.iter().find(|a| a.title == title).unwrap_or_else(|| {
            panic!("no action {title:?} in {:?}", found.iter().map(|a| &a.title).collect::<Vec<_>>())
        });
        let source = crate::text::SourceText::new(text.to_string());
        let mut edits: Vec<(usize, usize, &str)> = action.edit.changes["file:///test/main.gsql"]
            .iter()
            .map(|e| {
                let start = source.offset(e.range.start, crate::text::PositionEncoding::Utf16);
                let end = source.offset(e.range.end, crate::text::PositionEncoding::Utf16);
                (start, end, e.new_text.as_str())
            })
            .collect();
        edits.sort_by_key(|e| std::cmp::Reverse(e.0));
        let mut result = text.to_string();
        for (start, end, new_text) in edits {
            result.replace_range(start..end, new_text);
        }
        result
    }

    #[test]
    fn compares_floats_with_a_tolerance() {
        let text = "CREATE QUERY q(FLOAT x) {\n  IF x + 1 == 1.5 THEN PRINT x; END;\n}\n";
        let fixed = apply(text, "Compare with a tolerance: `abs((x + 1) - 1.5) < 0.0001`");
        assert_eq!(fixed, "CREATE QUERY q(FLOAT x) {\n  IF abs((x + 1) - 1.5) < 0.0001 THEN PRINT x; END;\n}\n");
    }

    #[test]
    fn declares_syntax_v3_for_equality_operators() {
        let text = "CREATE QUERY q(INT x) {\n  IF x = 1 THEN PRINT x; END;\n}\n";
        assert_eq!(apply(text, "Replace with `==`"), "CREATE QUERY q(INT x) {\n  IF x == 1 THEN PRINT x; END;\n}\n");
        assert_eq!(
            apply(text, "Declare the query SYNTAX V3"),
            "CREATE QUERY q(INT x) SYNTAX V3 {\n  IF x = 1 THEN PRINT x; END;\n}\n"
        );
        let v2 = "CREATE QUERY q(INT x) SYNTAX v2 {\n  IF x <> 1 THEN PRINT x; END;\n}\n";
        assert_eq!(
            apply(v2, "Change the query to SYNTAX V3"),
            "CREATE QUERY q(INT x) SYNTAX V3 {\n  IF x <> 1 THEN PRINT x; END;\n}\n"
        );
    }

    #[test]
    fn renames_reserved_words() {
        let text = "CREATE QUERY q(INT count) {\n  PRINT count + 1;\n}\n";
        assert_eq!(apply(text, "Rename to `count_`"), "CREATE QUERY q(INT count_) {\n  PRINT count_ + 1;\n}\n");
    }

    #[test]
    fn removes_unused_declarations() {
        let text = "CREATE QUERY q() {\n  SumAccum<INT> @@a, @@b;\n  INT x = 1;\n  PRINT @@a;\n}\n";
        assert_eq!(
            apply(text, "Remove unused `@@b`"),
            "CREATE QUERY q() {\n  SumAccum<INT> @@a;\n  INT x = 1;\n  PRINT @@a;\n}\n"
        );
        assert_eq!(
            apply(text, "Remove unused `x`"),
            "CREATE QUERY q() {\n  SumAccum<INT> @@a, @@b;\n  PRINT @@a;\n}\n"
        );
    }

    #[test]
    fn corrects_misspelled_keywords() {
        let text = "CREATE QUERY q() {\n  R = SELECT s FORM P:s;\n  PRINT R;\n}\n";
        let fixture = Fixture::new(text);
        let found = diagnostics(&fixture.snapshot());
        let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, ["Syntax error: did you mean `FROM` instead of `FORM`?"]);
        assert_eq!(apply(text, "Change to `FROM`"), "CREATE QUERY q() {\n  R = SELECT s FROM P:s;\n  PRINT R;\n}\n");
    }

    #[test]
    fn inserts_missing_tokens() {
        let text = "CREATE QUERY q() {\n  INT x = 1\n  PRINT x;\n}\n";
        assert_eq!(apply(text, "Insert `;`"), "CREATE QUERY q() {\n  INT x = 1;\n  PRINT x;\n}\n");
        let accum = "CREATE QUERY q() {\n  SumAccum<INT> @@a, @@b;\n  R = SELECT s FROM P:s\n      ACCUM @@a += 1\n            @@b += 1;\n  PRINT R;\n}\n";
        assert!(apply(accum, "Insert `,`").contains("ACCUM @@a += 1,\n"));
    }

    #[test]
    fn removes_a_stray_token_or_inserts_a_missing_bracket() {
        let stray = "CREATE QUERY q() {\n  INT x = 1;\n  PRINT x x;\n}\n";
        assert_eq!(apply(stray, "Remove `x`"), "CREATE QUERY q() {\n  INT x = 1;\n  PRINT x;\n}\n");
        let open = "CREATE QUERY q() {\n  INT x = abs(1;\n  PRINT x;\n}\n";
        assert_eq!(apply(open, "Insert `)`"), "CREATE QUERY q() {\n  INT x = abs(1);\n  PRINT x;\n}\n");
        let index = "CREATE QUERY q() {\n  ListAccum<INT> @@l;\n  INT y = @@l[0;\n  PRINT y;\n}\n";
        assert_eq!(
            apply(index, "Insert `]`"),
            "CREATE QUERY q() {\n  ListAccum<INT> @@l;\n  INT y = @@l[0];\n  PRINT y;\n}\n"
        );
        // Guesses: never part of fix all.
        for text in [stray, open, index] {
            let fixture = Fixture::with_files(text, &[]);
            let snapshot = fixture.snapshot();
            let found = diagnostics(&snapshot);
            let whole = Range::new(snapshot.position(0), snapshot.position(text.len()));
            let all = code_actions(&snapshot, whole, &found, Some(&["source.fixAll".to_string()]), &found);
            assert!(
                all.iter().all(|a| !a.title.starts_with("Remove `") && !a.title.starts_with("Insert `")),
                "{all:?}"
            );
        }
    }

    #[test]
    fn inserts_a_missing_end_before_the_closing_brace() {
        let text = "CREATE QUERY q(INT a) FOR GRAPH G {\n  PRINT 0;\n  IF a == 1 THEN\n    PRINT 1;\n}\n";
        assert_eq!(
            apply(text, "Insert `END;`"),
            "CREATE QUERY q(INT a) FOR GRAPH G {\n  PRINT 0;\n  IF a == 1 THEN\n    PRINT 1;\n  END;\n}\n"
        );
        // A guess: never part of fix all.
        let fixture = Fixture::with_files(text, &[]);
        let snapshot = fixture.snapshot();
        let found = diagnostics(&snapshot);
        let whole = Range::new(snapshot.position(0), snapshot.position(text.len()));
        let all = code_actions(&snapshot, whole, &found, Some(&["source.fixAll".to_string()]), &found);
        assert!(all.iter().all(|a| !a.title.contains("END")), "{all:?}");
        // No fix when the closing brace shares its line.
        let inline = "CREATE QUERY q(INT a) {\n  IF a == 1 THEN\n    PRINT 1; }\n";
        assert!(actions(inline, &[]).iter().all(|a| a.title != "Insert `END;`"));
    }

    #[test]
    fn repairs_explained_syntax_mistakes() {
        let elseif = "CREATE QUERY q(INT x) {\n  IF x > 1 THEN PRINT 1; ELSEIF x > 0 THEN PRINT 2; END;\n}\n";
        assert!(apply(elseif, "Change to `ELSE IF`").contains("ELSE IF x > 0"));
        let post = "CREATE QUERY q() {\n  SumAccum<INT> @x;\n  R = SELECT s FROM P:s ACCUM s.@x += 1 POST ACCUM s.@x = 2;\n  PRINT R;\n}\n";
        assert!(apply(post, "Write `POST-ACCUM`").contains(" POST-ACCUM s.@x = 2;"));
        let default = "CREATE VERTEX P (PRIMARY_ID id STRING, name STRING=\"x\")\n";
        assert_eq!(
            apply(default, "Use `DEFAULT`"),
            "CREATE VERTEX P (PRIMARY_ID id STRING, name STRING DEFAULT \"x\")\n"
        );
        let comma = "CREATE QUERY q(INT a, ) {\n  PRINT a;\n}\n";
        assert_eq!(apply(comma, "Remove the trailing `,`"), "CREATE QUERY q(INT a ) {\n  PRINT a;\n}\n");
        let accum = "CREATE QUERY q() {\n  SumAccum<INT> @@s;\n  R = SELECT p FROM P:p ACCUM @@s += 1,\n  POST-ACCUM @@s += 2;\n}\n";
        assert!(apply(accum, "Remove the trailing `,`").contains("ACCUM @@s += 1\n  POST-ACCUM"));
        let then = "CREATE QUERY q(INT a) {\n  IF a == 1 PRINT a; END;\n}\n";
        assert_eq!(apply(then, "Insert `THEN`"), "CREATE QUERY q(INT a) {\n  IF a == 1 THEN PRINT a; END;\n}\n");
        let keyword = "CREATE QUERY q(INT a) {\n  WHILE a < 1\n    PRINT a;\n  END;\n}\n";
        assert!(apply(keyword, "Insert `DO`").contains("WHILE a < 1 DO\n"));
    }

    #[test]
    fn fix_all_finds_the_fixes_that_other_mistakes_hid() {
        // `=` on the line of an `ELSEIF` only shows once that line parses.
        let text = "CREATE QUERY q(INT x) {\n  IF x = 1 THEN PRINT 1; ELSEIF x = 2 THEN PRINT 2; END;\n}\n";
        let fixed = apply(text, "Fix all auto-fixable problems");
        assert_eq!(fixed, "CREATE QUERY q(INT x) {\n  IF x == 1 THEN PRINT 1; ELSE IF x == 2 THEN PRINT 2; END;\n}\n");
        // A second run has nothing left to do.
        let fixture = Fixture::new(&fixed);
        let snapshot = fixture.snapshot();
        let all = diagnostics(&snapshot);
        let whole = Range::new(snapshot.position(0), snapshot.position(fixed.len()));
        let only = ["source.fixAll".to_string()];
        assert!(code_actions(&snapshot, whole, &all, Some(&only), &all).is_empty());
    }

    #[test]
    fn fix_all_repairs_a_chain_of_elseif() {
        for count in [2, 3, 6, 9] {
            let branches: String = (1..=count).map(|i| format!("  ELSEIF a == {i} THEN PRINT {i};\n")).collect();
            let text = format!("CREATE QUERY q(INT a) {{\n  IF a == 0 THEN PRINT 0;\n{branches}  END;\n}}\n");
            let fixed = apply(&text, "Fix all auto-fixable problems");
            assert_eq!(fixed, text.replace("ELSEIF", "ELSE IF"), "{count} branches");
        }
    }

    #[test]
    fn fix_all_repairs_a_chain_of_elseif_on_one_line() {
        for word in ["ELSEIF", "ELSIF", "ELIF", "elseif"] {
            for (head, eol) in [("", "\n"), ("", "\r\n"), ("/* \u{e9}\u{1f600} */ ", "\n")] {
                for count in [1, 2, 3, 5] {
                    let branches: String = (1..=count).map(|i| format!(" {word} a == {i} THEN PRINT {i};")).collect();
                    let text = format!(
                        "CREATE QUERY q(INT a) {{{eol}  {head}IF a == 0 THEN PRINT 0;{branches} ELSE PRINT 9; END;{eol}}}{eol}"
                    );
                    let want = text.replace(word, if word == "elseif" { "else if" } else { "ELSE IF" });
                    let fixed = apply(&text, "Fix all auto-fixable problems");
                    assert_eq!(fixed, want, "{word} x{count}");
                }
            }
        }
    }

    #[test]
    fn offers_a_fix_for_transposed_names() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\n\
                      CREATE DIRECTED EDGE KNOWS (FROM Person, TO Person)\nCREATE GRAPH g (Person, KNOWS)\n";
        let text = "CREATE QUERY q() FOR GRAPH g {\n  S = {Person.*};\n  T = SELECT t FROM S:s -(KNWOS:e)- Person:t;\n  \
                    U = SELECT s FROM S:s WHERE s.nmae == \"x\";\n  PRINT T, U;\n}\n";
        let found = actions(text, &[("file:///test/s.gsql", schema)]);
        let titles: Vec<&str> = found.iter().map(|a| a.title.as_str()).collect();
        assert!(titles.contains(&"Change to `KNOWS`"), "{titles:?}");
        assert!(titles.contains(&"Change to `name`"), "{titles:?}");
    }

    #[test]
    fn fixes_all_safe_problems_at_once() {
        let text = "CREATE QUERY q(INT x) {\n  sumaccum<INT> @@n;\n  IF x = 1 THEN @@n += 1; END;\n  IF x > 1 THEN @@n += 1; ELSEIF x > 2 THEN @@n += 2; END;\n  PRINT @@n;\n}\n";
        let fixed = apply(text, "Fix all auto-fixable problems");
        assert_eq!(
            fixed,
            "CREATE QUERY q(INT x) {\n  SumAccum<INT> @@n;\n  IF x == 1 THEN @@n += 1; END;\n  IF x > 1 THEN @@n += 1; ELSE IF x > 2 THEN @@n += 2; END;\n  PRINT @@n;\n}\n"
        );
        // Only the fix-all action when the client asks for source actions.
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let whole = Range::new(snapshot.position(0), snapshot.position(text.len()));
        let only = ["source.fixAll".to_string()];
        let all = diagnostics(&snapshot);
        let found = code_actions(&snapshot, whole, &all, Some(&only), &all);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, "source.fixAll");
    }

    #[test]
    fn declares_accumulators_after_typedefs_and_on_the_brace_line() {
        let text = "CREATE QUERY q() { @@total += 1; PRINT @@total; }\n";
        assert_eq!(
            apply(text, "Declare `SumAccum<INT> @@total`"),
            "CREATE QUERY q() { SumAccum<INT> @@total; @@total += 1; PRINT @@total; }\n"
        );
        let text =
            "CREATE QUERY q() {\n  TYPEDEF TUPLE<INT a> T;\n  // Counts.\n  @@total += 1;\n  PRINT @@total;\n}\n";
        assert_eq!(
            apply(text, "Declare `SumAccum<INT> @@total`"),
            "CREATE QUERY q() {\n  TYPEDEF TUPLE<INT a> T;\n  SumAccum<INT> @@total;\n  // Counts.\n  @@total += 1;\n  PRINT @@total;\n}\n"
        );
    }

    #[test]
    fn removing_an_unused_declaration_takes_the_blanks_after_it() {
        let text = "CREATE QUERY q() {\n  INT x = 1; INT y = 2;\n  PRINT y;\n}\n";
        assert_eq!(apply(text, "Remove unused `x`"), "CREATE QUERY q() {\n  INT y = 2;\n  PRINT y;\n}\n");
    }

    #[test]
    fn declares_accumulators_with_guessed_types() {
        let found = actions("CREATE QUERY q() {\n  PRINT 1;\n  @@counts += (\"a\" -> 1);\n  @@total += 1.5;\n}\n", &[]);
        let titles: Vec<&str> = found.iter().map(|a| a.title.as_str()).collect();
        assert!(titles.contains(&"Declare `MapAccum<STRING, SumAccum<INT>> @@counts`"), "{titles:?}");
        assert!(titles.contains(&"Declare `SumAccum<DOUBLE> @@total`"), "{titles:?}");
        let edit = &found[0].edit.changes["file:///test/main.gsql"][0];
        assert!(edit.new_text.starts_with("  MapAccum"), "{:?}", edit.new_text);
        assert_eq!(edit.range.start.line, 1);
    }

    #[test]
    fn suggests_similar_names() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\n";
        let found = actions(
            "CREATE QUERY q() {\n  R = SELECT s FROM Persn:s;\n  T = SELECT s FROM Person:s WHERE s.agee > 1;\n  PRINT R, T;\n}\n",
            &[("file:///test/schema.gsql", schema)],
        );
        let titles: Vec<&str> = found.iter().map(|a| a.title.as_str()).collect();
        assert!(titles.contains(&"Change to `Person`"), "{titles:?}");
        assert!(titles.contains(&"Change to `age`"), "{titles:?}");
    }
}
