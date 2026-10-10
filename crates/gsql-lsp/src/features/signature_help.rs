//! Signature help for built-in functions, methods, query calls and tuple constructors.

use tree_sitter::Node;

use crate::analysis::{Context, Reference, Role, SymbolKind, Ty};
use crate::builtins;
use crate::features::Snapshot;
use crate::features::resolve::{self, Target};
use crate::lsp::types::{
    MarkupContent, ParameterInformation, Position, SignatureHelp,
    SignatureInformation,
};
use crate::syntax;
use crate::text::{Span, is_identifier, is_word_char};

struct Signature {
    label: String,
    params: Vec<String>,
    doc: Option<String>,
}

impl Signature {
    /// A built-in's signature.
    fn builtin(label: String, params: &[&str], doc: &str) -> Self {
        Signature {
            label,
            params: params
                .iter()
                .map(|p| p.to_string())
                .collect(),
            doc: Some(doc.to_string()),
        }
    }

    /// A constructor-like call: `name(a, b)` with no doc (tuple types, exceptions).
    fn constructor(name: &str, params: Vec<String>) -> Self {
        Signature {
            label: format!("{name}({})", params.join(", ")),
            params,
            doc: None,
        }
    }
}

impl From<&builtins::Function> for Signature {
    fn from(function: &builtins::Function) -> Self {
        Signature::builtin(
            function.signature(),
            function.params,
            function.doc,
        )
    }
}

impl From<&builtins::Method> for Signature {
    fn from(method: &builtins::Method) -> Self {
        Signature::builtin(method.signature(), method.params, method.doc)
    }
}

pub fn signature_help(
    snapshot: &Snapshot,
    position: Position,
) -> Option<SignatureHelp> {
    let offset = snapshot.offset(position);
    // Commas and parentheses in comments are not code.
    let text = snapshot.source.code_text();
    let tree_call = find_call(snapshot, offset);
    let text_call = call_in_text(text, offset);
    let (signature, open_paren) = match (tree_call, text_call) {
        // An unclosed call in a list the tree could not parse is lost to the
        // tree (`f(abs(|, 2)`): go by its name in the text.
        (Some((_, list)), Some(call))
            if list.has_error() && call.open_paren > list.start_byte() =>
        {
            (signature_by_name(snapshot, &call)?, call.open_paren)
        }
        // A call the tree resolves to nothing has no signature by name either.
        (Some((callee, list)), _) => {
            (signature_for(snapshot, callee)?, list.start_byte())
        }
        // A syntax error can hide the call from the tree (an unfinished
        // line in a long query): go by the name in the text.
        (None, Some(call)) => {
            (signature_by_name(snapshot, &call)?, call.open_paren)
        }
        (None, None) => return None,
    };
    let active = active_argument(text, open_paren, offset);
    let parameters: Vec<ParameterInformation> = signature
        .params
        .iter()
        .map(|p| ParameterInformation {
            label: p.clone(),
            documentation: None,
        })
        .collect();
    let active_parameter = if signature
        .params
        .last()
        .is_some_and(|p| p == "...")
    {
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

/// The callee node and the argument list of the innermost call around `offset`.
fn find_call<'t>(
    snapshot: &'t Snapshot,
    offset: usize,
) -> Option<(Node<'t>, Node<'t>)> {
    let root = snapshot.root();
    let leaf =
        root.descendant_for_byte_range(offset.saturating_sub(1), offset)?;
    let lineage = syntax::lineage(root, leaf);
    for (index, &node) in lineage.iter().enumerate() {
        if node.kind() != "argument_list" || node.start_byte() >= offset {
            continue;
        }
        // Inside the parentheses (or at the end of an unclosed list).
        let closed = node
            .child(node.child_count().saturating_sub(1))
            .is_some_and(|c| c.kind() == ")" && !c.is_missing());
        if closed && offset >= node.end_byte() {
            continue;
        }
        let parent = *lineage.get(index + 1)?;
        let callee = match parent.kind() {
            "call_expression" => parent.child_by_field_name("function")?,
            "run_query_statement" => parent.child_by_field_name("query")?,
            "interpret_query_statement" | "raise_statement" => parent
                .child_by_field_name("name")
                .or_else(|| parent.child_by_field_name("exception"))?,
            _ => continue,
        };
        return Some((callee, node));
    }
    None
}

/// How a call found in the text names what it calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallForm {
    /// `name(`: a tuple type, a query or a built-in function.
    Plain,
    /// `x.name(`: a method of the receiver.
    Method,
    /// `RUN QUERY name(` or `INTERPRET QUERY name(`: a query.
    Query,
}

/// A call found in the text.
struct TextCall {
    name: String,
    name_start: usize,
    open_paren: usize,
    form: CallForm,
}

/// The call whose `(` is the innermost unclosed one before `offset`. Looks
/// back over at most a few lines, stopping at a statement end.
fn call_in_text(text: &str, offset: usize) -> Option<TextCall> {
    let start = lookback_start(text, offset);
    let mut depth = 0i32;
    // Depth of closed `{..}` groups (a set or map literal argument) being skipped.
    let mut braces = 0i32;
    let bytes = text.as_bytes();
    // The cursor may be inside a string (`f.println("a", "b|`): then the
    // closing quote is still to come, and the backwards scan starts inside it.
    let line_start = text[..offset]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let quotes = (line_start..offset)
        .filter(|&i| bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\'))
        .count();
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
                let (name_start, name) = last_word(&text[..index]);
                if !is_identifier(name) {
                    return None;
                }
                let before_name = text[..name_start].trim_end();
                let form = if before_name.ends_with('.') {
                    CallForm::Method
                } else if runs_a_query(before_name) {
                    CallForm::Query
                } else {
                    CallForm::Plain
                };
                let name = name.to_string();
                return Some(TextCall {
                    name,
                    name_start,
                    open_paren: index,
                    form,
                });
            }
            b';' | b'{' => return None,
            _ => {}
        }
    }
    None
}

/// Where a backwards scan from `offset` stops: a few lines back.
fn lookback_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .char_indices()
        .rev()
        .nth(1500)
        .map_or(0, |(i, _)| i)
}

/// The word that `text` ends with, after any whitespace, and where it starts.
fn last_word(text: &str) -> (usize, &str) {
    let text = text.trim_end();
    let start = text
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_word_char(c))
        .last()
        .map_or(text.len(), |(i, _)| i);
    (start, &text[start..])
}

/// The type of `x` in a call `x.name(`, where the analysis gives `x` the type `ty`.
/// In its projection `PRINT S[S.outdegree()]`, the set `S` stands for each vertex.
fn receiver_type(text: &str, name_start: usize, ty: &Ty) -> Ty {
    let projected = matches!(ty, Ty::VertexSet(_))
        && text[..name_start]
            .trim_end()
            .strip_suffix('.')
            .map(last_word)
            .is_some_and(|(start, receiver)| {
                !receiver.is_empty()
                    && projected_set(text, start) == Some(receiver)
            });
    if projected { ty.element() } else { ty.clone() }
}

/// The name before the innermost unclosed `[` before `offset`, past any unclosed
/// `(`: the set of a projection `PRINT S[..`. Stops at a statement end.
fn projected_set(text: &str, offset: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    for index in (lookback_start(text, offset)..offset).rev() {
        match bytes[index] {
            b'"' if index == 0 || bytes[index - 1] != b'\\' => {
                in_string = !in_string
            }
            _ if in_string => {}
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
            // A call or a group inside the projection.
            b'(' => {}
            b'[' => return Some(last_word(&text[..index]).1),
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
        .map(|p| p.to_string())
        .collect();
    let returns = query
        .returns
        .as_ref()
        .map(|r| format!(" RETURNS ({r})"))
        .unwrap_or_default();
    Signature {
        label: format!("{}({}){returns}", query.name, params.join(", ")),
        params,
        doc: query.doc.clone(),
    }
}

/// Whether `text` ends with `RUN QUERY` or `INTERPRET QUERY` and any options,
/// in any case. Words end only at whitespace or `;`.
fn runs_a_query(text: &str) -> bool {
    let is_flag = |w: &str| {
        w.strip_prefix('-')
            .is_some_and(is_identifier)
    };
    let mut words = text
        .split(|c: char| c.is_whitespace() || c == ';')
        .rev()
        .filter(|w| !w.is_empty())
        .peekable();
    // Skip the options: a flag, or the value just after one.
    let query = loop {
        match words.next() {
            Some(w)
                if is_flag(w) || words.peek().is_some_and(|p| is_flag(p)) => {
            }
            word => break word,
        }
    };
    let is = |word: Option<&str>, names: &[&str]| {
        word.is_some_and(|w| {
            names
                .iter()
                .any(|n| w.eq_ignore_ascii_case(n))
        })
    };
    is(query, &["QUERY"]) && is(words.next(), &["RUN", "INTERPRET"])
}

/// The signature of a call found in the text, resolved as the same call in the tree is.
fn signature_by_name(
    snapshot: &Snapshot,
    call: &TextCall,
) -> Option<Signature> {
    let role = match call.form {
        CallForm::Plain => Role::Function,
        CallForm::Query => Role::Query,
        // The analysis reads `x.name` as a member of `x`, with its type.
        CallForm::Method => {
            let member = snapshot
                .analysis
                .reference_at(call.name_start)
                .filter(|r| r.name == call.name);
            let ty = match member.map(|r| &r.role) {
                Some(Role::Method(ty) | Role::Attribute(ty)) => ty,
                _ => &Ty::Unknown,
            };
            let text = snapshot.source.code_text();
            Role::Method(receiver_type(text, call.name_start, ty))
        }
    };
    let reference = Reference {
        span: Span::default(),
        name: call.name.clone(),
        role,
        scope: 0,
        target: None,
        declaration: false,
        write: false,
        context: Context::Query,
        qualifier: false,
        in_error: false,
    };
    let target = resolve::target(snapshot, &reference)?;
    signature_of(snapshot, &target, &call.name)
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
    // Use the reference analysis when the callee is a resolved identifier.
    let member = callee.kind() == "member_expression";
    let name_node = if member {
        callee.child_by_field_name("property")?
    } else {
        callee
    };
    let name = syntax::text(name_node, snapshot.text());
    let start = name_node.start_byte();
    let found = snapshot
        .analysis
        .reference_at(start)
        .and_then(|reference| match &reference.role {
            Role::Method(ty) => {
                let ty =
                    receiver_type(snapshot.source.code_text(), start, ty);
                let role = Role::Method(ty);
                resolve::target(
                    snapshot,
                    &Reference {
                        role,
                        ..reference.clone()
                    },
                )
            }
            _ => resolve::target(snapshot, reference),
        })
        .and_then(|target| signature_of(snapshot, &target, name));
    if found.is_some() || member {
        // A method the receiver does not have is not the function of that name.
        return found;
    }
    builtins::function(name).map(Signature::from)
}

/// The signature of a call of `target`, named `name` at the call.
fn signature_of(
    snapshot: &Snapshot,
    target: &Target,
    name: &str,
) -> Option<Signature> {
    let symbols = &snapshot.analysis.symbols;
    match target {
        Target::Function(function) => Some((*function).into()),
        Target::Method(method) => Some((*method).into()),
        Target::Global(key) if key.kind == SymbolKind::Query => {
            let query = resolve::declarations(snapshot, key)
                .into_iter()
                .next()?;
            Some(query_signature(query))
        }
        Target::Global(key) if key.kind == SymbolKind::TupleType => {
            let fields: Vec<String> = snapshot
                .workspace
                .tuple_fields(&key.name)
                .into_iter()
                .map(|f| f.detail.clone())
                .collect();
            Some(Signature::constructor(&key.name, fields))
        }
        Target::Local(id) if symbols[*id].kind == SymbolKind::TupleType => {
            let fields: Vec<String> = snapshot
                .analysis
                .symbols_owned_by(SymbolKind::TupleField, name)
                .map(|s| s.detail.clone())
                .collect();
            Some(Signature::constructor(name, fields))
        }
        Target::Local(id) if symbols[*id].kind == SymbolKind::Exception => {
            Some(Signature::constructor(name, vec!["message".into()]))
        }
        _ => None,
    }
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
        let text = format!(
            "CREATE QUERY q() {{\n{body}  PRINT pow(2, |\n  PRINT 1;\n}}\n"
        );
        let help = help(&text, &[]).unwrap();
        assert_eq!(help.signatures[0].label, "pow(base, exp) -> FLOAT");
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn works_inside_a_string_argument() {
        let help =
            help("CREATE QUERY q(FILE f) { f.println(\"a\", \"b|\"); }", &[])
                .unwrap();
        assert!(
            help.signatures[0].label.contains("println("),
            "{}",
            help.signatures[0].label
        );
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn survives_a_closed_literal_argument() {
        for text in [
            "CREATE QUERY q() FOR GRAPH g {\n PRINT coalesce({1}, |",
            "CREATE QUERY q() FOR GRAPH g {\n PRINT coalesce({\"a\": {1, 2}}, |",
            "CREATE QUERY q() FOR GRAPH g {\n PRINT coalesce({1}, [2], |",
        ] {
            let help = help(text, &[])
                .unwrap_or_else(|| panic!("no help for {text}"));
            assert!(
                help.signatures[0]
                    .label
                    .starts_with("coalesce("),
                "{}",
                help.signatures[0].label
            );
            assert!(help.active_parameter >= 1, "{text}");
        }
        // A statement end still stops the search.
        assert!(help("CREATE QUERY q() {\n f(1); {1}, |", &[]).is_none());
    }

    #[test]
    fn builtin_function_signature() {
        let help =
            help("CREATE QUERY q() { PRINT pow(2, |); }", &[]).unwrap();
        assert_eq!(help.signatures[0].label, "pow(base, exp) -> FLOAT");
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn query_call_signature() {
        let other =
            "CREATE QUERY helper(INT a, STRING b = \"x\") { PRINT a; }";
        let help =
            help("RUN QUERY helper(1, |)", &[("file:///test/h.gsql", other)])
                .unwrap();
        assert_eq!(
            help.signatures[0].label,
            "helper(INT a, STRING b = \"x\")"
        );
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn query_call_signature_shows_the_return_type() {
        let other = "CREATE QUERY helper(INT a) RETURNS (INT) { RETURN a; }";
        let help = help(
            "CREATE QUERY q() { INT x = helper(|); }",
            &[("file:///test/h.gsql", other)],
        )
        .unwrap();
        assert_eq!(help.signatures[0].label, "helper(INT a) RETURNS (INT)");
    }

    #[test]
    fn method_signature_uses_receiver_type() {
        let help = help(
            "CREATE QUERY q() { MapAccum<STRING, INT> @@m; PRINT @@m.containsKey(|); }",
            &[],
        )
        .unwrap();
        assert_eq!(help.signatures[0].label, ".containsKey(key) -> BOOL");
    }

    #[test]
    fn ignores_commas_in_nested_calls_and_strings() {
        let help =
            help("CREATE QUERY q() { PRINT pow(abs(1, 2), \"a,b\"|); }", &[])
                .unwrap();
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn ignores_commas_and_parentheses_in_comments() {
        for text in [
            "CREATE QUERY q() { PRINT pow(2 /* a, b */, |); }",
            "CREATE QUERY q() {\n  PRINT pow(2, // see f(x\n    |",
        ] {
            let help = help(text, &[])
                .unwrap_or_else(|| panic!("no help for {text}"));
            assert_eq!(
                help.signatures[0].label, "pow(base, exp) -> FLOAT",
                "{text}"
            );
            assert_eq!(help.active_parameter, 1, "{text}");
        }
    }

    #[test]
    fn an_unfinished_call_prefers_a_query_to_a_builtin_of_the_same_name() {
        let text = concat!(
            "CREATE QUERY abs(INT a, INT b) { PRINT a; }\n",
            "CREATE QUERY c() {\n  INT x = abs(1, |",
        );
        let found = help(text, &[]).unwrap();
        assert_eq!(found.signatures[0].label, "abs(INT a, INT b)");
        assert_eq!(found.active_parameter, 1);
        // An unfinished call of a tuple type.
        let tuple = (
            "file:///test/t.gsql",
            "TYPEDEF TUPLE <INT a, STRING b> Pair;\n",
        );
        let text =
            "CREATE QUERY c() {\n  ListAccum<Pair> @@l;\n  @@l += Pair(1, |";
        let label =
            help(text, &[tuple]).map(|h| h.signatures[0].label.clone());
        assert_eq!(label.as_deref(), Some("Pair(INT a, STRING b)"));
    }

    #[test]
    fn a_method_call_never_shows_a_function_signature() {
        for text in [
            "CREATE QUERY q() { STRING s = \"x\"; PRINT s.lower(|); }",
            "CREATE QUERY q() {\n  STRING s = \"x\";\n  PRINT s.lower(|",
            "CREATE QUERY q() { INT s = 1; PRINT s.pow(1, |); }",
        ] {
            let found =
                help(text, &[]).map(|h| h.signatures[0].label.clone());
            assert_eq!(found, None, "{text}");
        }
        // A plain call of the same name is the function.
        let help = help("CREATE QUERY q() { PRINT lower(|); }", &[]).unwrap();
        assert_eq!(help.signatures[0].label, "lower(str) -> STRING");
    }

    #[test]
    fn a_method_call_shows_only_a_method_of_the_receiver() {
        // The receiver has no such method, though another type has.
        for text in [
            "CREATE QUERY q() { SumAccum<INT> @@s; PRINT @@s.size(|); }",
            "CREATE QUERY q() {\n  SumAccum<INT> @@s;\n  PRINT @@s.size(|",
            "CREATE QUERY q() { STRING s = \"x\"; PRINT s.size(|); }",
            "CREATE QUERY q() {\n  STRING s = \"x\";\n  PRINT s.size(|",
            "CREATE QUERY q() { INT n = 1; PRINT n.get(|); }",
            "CREATE QUERY q() {\n  INT n = 1;\n  PRINT n.get(|",
        ] {
            let found =
                help(text, &[]).map(|h| h.signatures[0].label.clone());
            assert_eq!(found, None, "{text}");
        }
        // The receiver's own method, or any method of a receiver of unknown type.
        for text in [
            "CREATE QUERY q() { MapAccum<STRING, INT> @@m; PRINT @@m.containsKey(|); }",
            "CREATE QUERY q() {\n  MapAccum<STRING, INT> @@m;\n  PRINT @@m.containsKey(|",
            "CREATE QUERY q() { PRINT x.containsKey(|); }",
            "CREATE QUERY q() {\n  PRINT x.containsKey(|",
        ] {
            let found =
                help(text, &[]).map(|h| h.signatures[0].label.clone());
            assert_eq!(
                found.as_deref(),
                Some(".containsKey(key) -> BOOL"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_plain_call_never_shows_a_method_signature() {
        for text in [
            "CREATE QUERY q() { PRINT containsKey(|); }",
            "CREATE QUERY q() {\n  PRINT containsKey(|",
        ] {
            let found =
                help(text, &[]).map(|h| h.signatures[0].label.clone());
            assert_eq!(found, None, "{text}");
        }
    }

    #[test]
    fn run_query_calls_the_query_that_a_tuple_type_shares_a_name_with() {
        let tuple = (
            "file:///test/t.gsql",
            "TYPEDEF TUPLE <INT x, INT y> Pair;\n",
        );
        let query = (
            "file:///test/q.gsql",
            "CREATE QUERY Pair(STRING a, STRING b) { PRINT a; }\n",
        );
        for text in [
            "RUN QUERY Pair(1, |",
            "INTERPRET QUERY Pair(1, |",
            "run query Pair(1, |",
            "RUN QUERY -d Pair(1, |",
            "RUN QUERY -PROFILE BASIC Pair(1, |",
            "RUN QUERY -queue wq -d Pair(1, |",
            "RUN QUERY\n  Pair(1, |",
            "PRINT 1;RUN QUERY Pair(1, |",
            "CREATE QUERY c() {\n  RUN QUERY Pair(1, |",
            "RUN QUERY Pair(\"a\", |)",
        ] {
            let found = help(text, &[tuple, query])
                .map(|h| h.signatures[0].label.clone());
            assert_eq!(
                found.as_deref(),
                Some("Pair(STRING a, STRING b)"),
                "{text}"
            );
        }
        // A plain call builds the tuple, even after the words `run query`.
        for text in [
            "CREATE QUERY c() {\n  ListAccum<Pair> @@l;\n  @@l += Pair(1, |",
            "CREATE QUERY c() {\n  ListAccum<Pair> @@l;\n  @@l += Pair(1, |);\n}",
            "CREATE QUERY c() {\n  PRINT \"run query\", Pair(1, |",
            "CREATE QUERY c() {\n  PRINT \"run query \", Pair(1, |",
            "CREATE QUERY c() {\n  PRINT \"could not run query -d\", Pair(1, |",
        ] {
            let found = help(text, &[tuple, query])
                .map(|h| h.signatures[0].label.clone());
            assert_eq!(
                found.as_deref(),
                Some("Pair(INT x, INT y)"),
                "{text}"
            );
        }
    }

    #[test]
    fn an_unfinished_inner_call_shows_its_own_signature() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\n";
        for body in [
            "PRINT my_udf(abs(|, 2);",
            "PRINT my_udf(1, abs(|, 2);",
            "SumAccum<INT> @@s;\n  PRINT @@s.foo(abs(|, 2);",
            "R = SELECT s FROM Person:s WHERE my_udf(abs(|, s.age) > 1;",
            // The outer call resolves, but the cursor is in the inner one.
            "PRINT pow(abs(|, 2);",
        ] {
            let text = &format!("CREATE QUERY q() {{\n  {body}\n}}\n");
            let found = help(text, &[("file:///test/s.gsql", schema)]);
            let label = found
                .as_ref()
                .map(|h| h.signatures[0].label.as_str());
            assert_eq!(label, Some("abs(num) -> number"), "{text}");
            assert_eq!(found.unwrap().active_parameter, 0, "{text}");
        }
        // An unknown inner call shows nothing, not the outer call.
        let text = "CREATE QUERY q() {\n  PRINT pow(my_udf(|, 2);\n}\n";
        assert!(help(text, &[]).is_none());
        // A group, even after a word, is not a call.
        for text in [
            "CREATE QUERY q() {\n  PRINT pow((1 + |), 2);\n}\n",
            "CREATE QUERY q() {\n  PRINT pow(x IN (|y), 2);\n}\n",
        ] {
            let label =
                help(text, &[]).map(|h| h.signatures[0].label.clone());
            assert_eq!(
                label.as_deref(),
                Some("pow(base, exp) -> FLOAT"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_vertex_method_in_a_print_projection_of_the_set() {
        let schema = concat!(
            "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\n",
            "CREATE DIRECTED EDGE Knows (FROM Person, TO Person, w DOUBLE)\n",
            "CREATE GRAPH G (*)\n",
        );
        let head = "CREATE QUERY q() FOR GRAPH G {\n  S = {Person.*};\n";
        for (body, label) in [
            (
                "  PRINT S[S.outdegree(|)];\n}\n",
                ".outdegree([edgeType]) -> INT",
            ),
            ("  PRINT S[S.outdegree(|", ".outdegree([edgeType]) -> INT"),
            (
                "  PRINT S[S.name, S.outdegree(|) AS deg];\n}\n",
                ".outdegree([edgeType]) -> INT",
            ),
            (
                "  PRINT S[S.getAttr(|)];\n}\n",
                ".getAttr(attrName, attrType) -> any",
            ),
            (
                "  PRINT S[abs(S.outdegree(|)) AS d];\n}\n",
                ".outdegree([edgeType]) -> INT",
            ),
        ] {
            let text = format!("{head}{body}");
            let found = help(&text, &[("file:///test/s.gsql", schema)]);
            let found = found.map(|h| h.signatures[0].label.clone());
            assert_eq!(found.as_deref(), Some(label), "{text}");
        }
        // Outside a projection of that set, the set has only its own methods.
        for body in [
            "  PRINT S.outdegree(|);\n}\n",
            "  T = {Person.*};\n  PRINT T[S.outdegree(|)];\n}\n",
        ] {
            let text = format!("{head}{body}");
            let found = help(&text, &[("file:///test/s.gsql", schema)]);
            let found = found.map(|h| h.signatures[0].label.clone());
            assert_eq!(found, None, "{text}");
        }
    }

    #[test]
    fn a_string_that_ends_in_run_query_does_not_make_a_query_call() {
        for (text, label) in [
            (
                "CREATE QUERY c() {\n  LOG(TRUE, \"could not run query\", abs(|",
                "abs(num) -> number",
            ),
            // `-d",` is not an option flag.
            (
                "CREATE QUERY c() {\n  LOG(TRUE, \"could not run query -d\", abs(|",
                "abs(num) -> number",
            ),
            (
                "CREATE QUERY c() {\n  PRINT \"run query\" + to_string(|",
                "to_string(num) -> STRING",
            ),
            (
                "CREATE QUERY c() {\n  PRINT \"interpret query\" + to_string(|",
                "to_string(num) -> STRING",
            ),
        ] {
            let found =
                help(text, &[]).map(|h| h.signatures[0].label.clone());
            assert_eq!(found.as_deref(), Some(label), "{text}");
        }
    }
}
