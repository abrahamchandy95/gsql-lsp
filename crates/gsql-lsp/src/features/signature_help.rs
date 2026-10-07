//! Signature help for built-in functions, methods, query calls and tuple constructors.

use tree_sitter::Node;

use crate::analysis::SymbolKind;
use crate::builtins;
use crate::features::Snapshot;
use crate::features::resolve::{self, Target};
use crate::lsp::types::{MarkupContent, ParameterInformation, Position, SignatureHelp, SignatureInformation};
use crate::syntax;

struct Signature {
    label: String,
    params: Vec<String>,
    doc: Option<String>,
}

pub fn signature_help(snapshot: &Snapshot, position: Position) -> Option<SignatureHelp> {
    let offset = snapshot.offset(position);
    let from_tree = find_call(snapshot, offset)
        .and_then(|(callee, open_paren)| Some((signature_for(snapshot, callee)?, open_paren)));
    let (signature, open_paren) = match from_tree {
        Some(found) => found,
        // A syntax error can hide the call from the tree (an unfinished
        // line in a long query), and a method may be called on something of
        // unknown type: go by the name in the text.
        None => {
            let (name, open_paren) = call_in_text(snapshot.text(), offset)?;
            (signature_by_name(snapshot, &name)?, open_paren)
        }
    };
    let active = active_argument(snapshot.text(), open_paren, offset);
    let parameters: Vec<ParameterInformation> =
        signature.params.iter().map(|p| ParameterInformation { label: p.clone(), documentation: None }).collect();
    let active_parameter = if signature.params.last().is_some_and(|p| p == "...") {
        active.min(signature.params.len().saturating_sub(1) as u32)
    } else {
        active
    };
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: signature.label,
            documentation: signature.doc.map(MarkupContent::markdown),
            parameters,
            active_parameter: Some(active_parameter),
        }],
        active_signature: 0,
        active_parameter,
    })
}

/// The callee node and the offset of the `(` of the innermost call around `offset`.
fn find_call<'t>(snapshot: &'t Snapshot, offset: usize) -> Option<(Node<'t>, usize)> {
    let root = snapshot.root();
    let leaf = root.descendant_for_byte_range(offset.saturating_sub(1), offset)?;
    let lineage = syntax::lineage(root, leaf);
    for (index, &node) in lineage.iter().enumerate() {
        if node.kind() != "argument_list" || node.start_byte() >= offset {
            continue;
        }
        // Inside the parentheses (or at the end of an unclosed list).
        let closed =
            node.child(node.child_count().saturating_sub(1)).is_some_and(|c| c.kind() == ")" && !c.is_missing());
        if closed && offset >= node.end_byte() {
            continue;
        }
        let parent = *lineage.get(index + 1)?;
        let callee = match parent.kind() {
            "call_expression" => parent.child_by_field_name("function")?,
            "run_query_statement" => parent.child_by_field_name("query")?,
            "interpret_query_statement" | "raise_statement" => {
                parent.child_by_field_name("name").or_else(|| parent.child_by_field_name("exception"))?
            }
            _ => continue,
        };
        return Some((callee, node.start_byte()));
    }
    None
}

/// The name before the innermost unclosed `(` before `offset`, and where that
/// `(` is. Looks back over at most a few lines, stopping at a statement end.
fn call_in_text(text: &str, offset: usize) -> Option<(String, usize)> {
    let start = text[..offset].char_indices().rev().nth(1500).map_or(0, |(i, _)| i);
    let mut depth = 0i32;
    // Depth of closed `{..}` groups (a set or map literal argument) being skipped.
    let mut braces = 0i32;
    let bytes = text.as_bytes();
    // The cursor may be inside a string (`f.println("a", "b|`): then the
    // closing quote is still to come, and the backwards scan starts inside it.
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let quotes = (line_start..offset).filter(|&i| bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\')).count();
    let mut in_string = quotes % 2 == 1;
    let mut index = offset;
    while index > start {
        index -= 1;
        let c = bytes[index];
        if c == b'"' && (index == 0 || bytes[index - 1] != b'\\') {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match c {
            b'}' => braces += 1,
            b'{' if braces > 0 => braces -= 1,
            _ if braces > 0 => {}
            b')' | b']' => depth += 1,
            b'[' => depth -= 1,
            b'(' if depth > 0 => depth -= 1,
            b'(' => {
                let before = text[..index].trim_end();
                let name: String = before
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return None;
                }
                return Some((name, index));
            }
            b';' | b'{' => return None,
            _ => {}
        }
    }
    None
}

/// `name(params) RETURNS (type)` of a query.
fn query_signature(query: &crate::workspace::GlobalSymbol) -> Signature {
    let params: Vec<String> = query
        .params
        .iter()
        .map(|p| match &p.default {
            Some(default) => format!("{} {} = {default}", p.ty, p.name),
            None => format!("{} {}", p.ty, p.name),
        })
        .collect();
    let returns = query.returns.as_ref().map(|r| format!(" RETURNS ({r})")).unwrap_or_default();
    Signature { label: format!("{}({}){returns}", query.name, params.join(", ")), params, doc: query.doc.clone() }
}

/// The signature of a function, query or method known by its name alone.
fn signature_by_name(snapshot: &Snapshot, name: &str) -> Option<Signature> {
    if let Some(function) = builtins::function(name) {
        return Some(Signature {
            label: function.signature(),
            params: function.params.iter().map(|p| p.to_string()).collect(),
            doc: Some(function.doc.to_string()),
        });
    }
    if let Some(query) = snapshot.workspace.find(SymbolKind::Query, name).into_iter().next() {
        return Some(query_signature(query));
    }
    let method = resolve::find_method_anywhere(name)?;
    Some(Signature {
        label: method.signature(),
        params: method.params.iter().map(|p| p.to_string()).collect(),
        doc: Some(method.doc.to_string()),
    })
}

/// Index of the argument containing `offset`: top-level commas since `(`.
fn active_argument(text: &str, open_paren: usize, offset: usize) -> u32 {
    let mut depth = 0i32;
    let mut index = 0u32;
    let mut in_string = false;
    let mut escaped = false;
    for c in text[open_paren + 1..offset.max(open_paren + 1)].chars() {
        if in_string {
            match (escaped, c) {
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => escaped = false,
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => index += 1,
            _ => {}
        }
    }
    index
}

fn signature_for(snapshot: &Snapshot, callee: Node) -> Option<Signature> {
    let source = snapshot.text();
    // Use the reference analysis when the callee is a resolved identifier.
    let name_node = match callee.kind() {
        "member_expression" => callee.child_by_field_name("property")?,
        "qualified_identifier" => callee,
        _ => callee,
    };
    let name = syntax::text(name_node, source);
    if let Some(reference) = snapshot.analysis.reference_at(name_node.start_byte()) {
        match resolve::target(snapshot, reference) {
            Some(Target::Function(function)) => {
                return Some(Signature {
                    label: function.signature(),
                    params: function.params.iter().map(|p| p.to_string()).collect(),
                    doc: Some(function.doc.to_string()),
                });
            }
            Some(Target::Method(method)) => {
                return Some(Signature {
                    label: method.signature(),
                    params: method.params.iter().map(|p| p.to_string()).collect(),
                    doc: Some(method.doc.to_string()),
                });
            }
            Some(Target::Global(key)) if key.kind == SymbolKind::Query => {
                let query = resolve::declarations(snapshot, &key).into_iter().next()?;
                return Some(query_signature(query));
            }
            Some(Target::Global(key)) if key.kind == SymbolKind::TupleType => {
                let fields: Vec<String> =
                    snapshot.workspace.tuple_fields(&key.name).into_iter().map(|f| f.detail.clone()).collect();
                return Some(Signature {
                    label: format!("{}({})", key.name, fields.join(", ")),
                    params: fields,
                    doc: None,
                });
            }
            Some(Target::Local(id)) if snapshot.analysis.symbols[id].kind == SymbolKind::TupleType => {
                let fields: Vec<String> = snapshot
                    .analysis
                    .symbols
                    .iter()
                    .filter(|s| s.kind == SymbolKind::TupleField && s.owner.as_deref() == Some(name))
                    .map(|s| s.detail.clone())
                    .collect();
                return Some(Signature { label: format!("{name}({})", fields.join(", ")), params: fields, doc: None });
            }
            Some(Target::Local(id)) if snapshot.analysis.symbols[id].kind == SymbolKind::Exception => {
                return Some(Signature {
                    label: format!("{name}(message)"),
                    params: vec!["message".into()],
                    doc: None,
                });
            }
            _ => {}
        }
    }
    builtins::function(name).map(|function| Signature {
        label: function.signature(),
        params: function.params.iter().map(|p| p.to_string()).collect(),
        doc: Some(function.doc.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{Fixture, cursor};

    fn help(text: &str, others: &[(&str, &str)]) -> Option<SignatureHelp> {
        let (text, offset) = cursor(text);
        let fixture = Fixture::with_files(&text, others);
        let snapshot = fixture.snapshot();
        signature_help(&snapshot, snapshot.position(offset))
    }

    #[test]
    fn finds_the_call_when_a_syntax_error_hides_it() {
        let mut body = String::new();
        for i in 0..30 {
            body.push_str(&format!("  INT v{i} = {i};\n"));
        }
        let text = format!("CREATE QUERY q() {{\n{body}  PRINT pow(2, |\n  PRINT 1;\n}}\n");
        let help = help(&text, &[]).unwrap();
        assert_eq!(help.signatures[0].label, "pow(base, exp) -> FLOAT");
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn works_inside_a_string_argument() {
        let help = help("CREATE QUERY q(FILE f) { f.println(\"a\", \"b|\"); }", &[]).unwrap();
        assert!(help.signatures[0].label.contains("println("), "{}", help.signatures[0].label);
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn survives_a_closed_literal_argument() {
        for text in [
            "CREATE QUERY q() FOR GRAPH g {\n PRINT coalesce({1}, |",
            "CREATE QUERY q() FOR GRAPH g {\n PRINT coalesce({\"a\": {1, 2}}, |",
            "CREATE QUERY q() FOR GRAPH g {\n PRINT coalesce({1}, [2], |",
        ] {
            let help = help(text, &[]).unwrap_or_else(|| panic!("no help for {text}"));
            assert!(help.signatures[0].label.starts_with("coalesce("), "{}", help.signatures[0].label);
            assert!(help.active_parameter >= 1, "{text}");
        }
        // A statement end still stops the search.
        assert!(help("CREATE QUERY q() {\n f(1); {1}, |", &[]).is_none());
    }

    #[test]
    fn builtin_function_signature() {
        let help = help("CREATE QUERY q() { PRINT pow(2, |); }", &[]).unwrap();
        assert_eq!(help.signatures[0].label, "pow(base, exp) -> FLOAT");
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn query_call_signature() {
        let other = "CREATE QUERY helper(INT a, STRING b = \"x\") { PRINT a; }";
        let help = help("RUN QUERY helper(1, |)", &[("file:///test/h.gsql", other)]).unwrap();
        assert_eq!(help.signatures[0].label, "helper(INT a, STRING b = \"x\")");
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn query_call_signature_shows_the_return_type() {
        let other = "CREATE QUERY helper(INT a) RETURNS (INT) { RETURN a; }";
        let help = help("CREATE QUERY q() { INT x = helper(|); }", &[("file:///test/h.gsql", other)]).unwrap();
        assert_eq!(help.signatures[0].label, "helper(INT a) RETURNS (INT)");
    }

    #[test]
    fn method_signature_uses_receiver_type() {
        let help = help("CREATE QUERY q() { MapAccum<STRING, INT> @@m; PRINT @@m.containsKey(|); }", &[]).unwrap();
        assert_eq!(help.signatures[0].label, ".containsKey(key) -> BOOL");
    }

    #[test]
    fn ignores_commas_in_nested_calls_and_strings() {
        let help = help("CREATE QUERY q() { PRINT pow(abs(1, 2), \"a,b\"|); }", &[]).unwrap();
        assert_eq!(help.active_parameter, 1);
    }
}
