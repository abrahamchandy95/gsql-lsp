//! Document outline and workspace symbol search.

use tree_sitter::Node;

use crate::analysis::{Symbol, SymbolKind};
use crate::features::Snapshot;
use crate::lsp::types::{DocumentSymbol, Location, SymbolInformation, symbol_kind};
use crate::syntax;
use crate::text::Span;

pub fn lsp_kind(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Graph => symbol_kind::NAMESPACE,
        SymbolKind::VertexType => symbol_kind::CLASS,
        SymbolKind::EdgeType => symbol_kind::INTERFACE,
        SymbolKind::Attribute | SymbolKind::TupleField => symbol_kind::FIELD,
        SymbolKind::TupleType => symbol_kind::STRUCT,
        SymbolKind::AccumulatorType => symbol_kind::CLASS,
        SymbolKind::Query => symbol_kind::FUNCTION,
        SymbolKind::LoadingJob | SymbolKind::SchemaChangeJob => symbol_kind::MODULE,
        SymbolKind::Parameter
        | SymbolKind::Variable
        | SymbolKind::VertexSet
        | SymbolKind::Alias
        | SymbolKind::LoopVariable
        | SymbolKind::Table => symbol_kind::VARIABLE,
        SymbolKind::GlobalAccumulator | SymbolKind::LocalAccumulator => symbol_kind::PROPERTY,
        SymbolKind::File | SymbolKind::FilenameVariable => symbol_kind::FILE,
        SymbolKind::Exception => symbol_kind::EVENT,
        SymbolKind::Header | SymbolKind::LineFilter | SymbolKind::TempTable | SymbolKind::TempColumn => {
            symbol_kind::CONSTANT
        }
        SymbolKind::Package => symbol_kind::PACKAGE,
        SymbolKind::DataSource => symbol_kind::OBJECT,
    }
}

fn in_outline(symbol: &Symbol) -> bool {
    !symbol.copied
        && !matches!(
            symbol.kind,
            SymbolKind::Alias
                | SymbolKind::LoopVariable
                | SymbolKind::Parameter
                | SymbolKind::Table
                | SymbolKind::TempColumn
        )
}

pub fn document_symbols(snapshot: &Snapshot) -> Vec<DocumentSymbol> {
    let analysis = snapshot.analysis;
    let mut symbols: Vec<&Symbol> = analysis.symbols.iter().filter(|s| in_outline(s)).collect();
    symbols.sort_by_key(|s| (s.span.start, std::cmp::Reverse(s.span.end)));

    // Nest by span containment.
    struct Entry<'a> {
        symbol: &'a Symbol,
        children: Vec<usize>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut roots = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for symbol in symbols {
        while let Some(&top) = stack.last() {
            let parent = entries[top].symbol;
            let strictly_inside = parent.span.contains_span(symbol.span) && parent.span != symbol.span;
            if strictly_inside {
                break;
            }
            stack.pop();
        }
        let index = entries.len();
        entries.push(Entry { symbol, children: Vec::new() });
        match stack.last() {
            Some(&parent) => entries[parent].children.push(index),
            None => roots.push(index),
        }
        stack.push(index);
    }

    fn build(snapshot: &Snapshot, entries: &[Entry], index: usize) -> DocumentSymbol {
        let symbol = entries[index].symbol;
        let detail = match symbol.kind {
            SymbolKind::Attribute | SymbolKind::TupleField => Some(symbol.ty.display()),
            SymbolKind::GlobalAccumulator
            | SymbolKind::LocalAccumulator
            | SymbolKind::Variable
            | SymbolKind::VertexSet => Some(symbol.ty.display()),
            SymbolKind::Query => {
                let params: Vec<String> = symbol.params.iter().map(|p| format!("{} {}", p.ty, p.name)).collect();
                Some(format!("({})", params.join(", ")))
            }
            _ => None,
        }
        .filter(|d| d != "unknown");
        DocumentSymbol {
            name: symbol.name.clone(),
            detail,
            kind: lsp_kind(symbol.kind),
            range: snapshot.range(symbol.span),
            selection_range: snapshot.range(symbol.name_span),
            children: entries[index].children.iter().map(|&c| build(snapshot, entries, c)).collect(),
        }
    }
    let mut outline: Vec<DocumentSymbol> = roots.into_iter().map(|r| build(snapshot, &entries, r)).collect();
    let stubs = stub_symbols(snapshot);
    if !stubs.is_empty() {
        outline.extend(stubs);
        outline.sort_by_key(|s| (s.range.start.line, s.range.start.character));
    }
    outline
}

/// The outline as the flat `SymbolInformation[]` form (for clients without
/// hierarchical support): parents first, the parent's name as `containerName`.
pub fn flatten(uri: &str, outline: &[DocumentSymbol]) -> Vec<SymbolInformation> {
    fn add(uri: &str, symbols: &[DocumentSymbol], container: Option<&str>, out: &mut Vec<SymbolInformation>) {
        for symbol in symbols {
            out.push(SymbolInformation {
                name: symbol.name.clone(),
                kind: symbol.kind,
                location: Location { uri: uri.to_string(), range: symbol.range },
                container_name: container.map(String::from),
            });
            add(uri, &symbol.children, Some(&symbol.name), out);
        }
    }
    let mut out = Vec::new();
    add(uri, outline, None, &mut out);
    out
}

/// The outline of the generated built-in reference file: one entry per
/// `BUILTIN` declaration, the methods of an object nested under it. These
/// are not analysis symbols, so workspace search never lists them.
fn stub_symbols(snapshot: &Snapshot) -> Vec<DocumentSymbol> {
    let source = snapshot.text();
    let range = |node: Node| snapshot.range(Span::new(node.start_byte(), node.end_byte()));
    let entry = |node: Node, name: Node, kind: u8, detail: Option<String>, children| DocumentSymbol {
        name: syntax::text(name, source).to_string(),
        detail: detail.filter(|d| !d.is_empty()),
        kind,
        range: range(node),
        selection_range: range(name),
        children,
    };
    // `(params) -> result`, as written.
    let signature = |node: Node| {
        let signature = child_of_kind(node, "stub_signature")?;
        let params = signature.child_by_field_name("parameters").map_or("", |p| syntax::text(p, source).trim());
        let mut detail = format!("({params})");
        if let Some(returns) = signature.child_by_field_name("returns") {
            detail.push_str(&format!(" -> {}", syntax::text(returns, source).trim()));
        }
        Some(detail)
    };
    let mut result = Vec::new();
    syntax::walk(snapshot.root(), |node| {
        if node.kind() != "stub_declaration" {
            return;
        }
        // CONSTANT, TYPE and KEYWORD name themselves; FUNCTION and OBJECT hold the name.
        let word = |node: Node| {
            let keyword = node.child(1).map(|k| k.kind().to_ascii_uppercase());
            match keyword.as_deref() {
                Some("TYPE") => symbol_kind::TYPE_PARAMETER,
                Some("KEYWORD") => symbol_kind::KEY,
                _ => symbol_kind::CONSTANT,
            }
        };
        if let Some(function) = child_of_kind(node, "stub_function") {
            if let Some(name) = function.child_by_field_name("name") {
                result.push(entry(node, name, symbol_kind::FUNCTION, signature(function), Vec::new()));
            }
        } else if let Some(object) = child_of_kind(node, "stub_object") {
            let Some(name) = object.child_by_field_name("name") else {
                return;
            };
            let mut cursor = object.walk();
            let methods: Vec<DocumentSymbol> = object
                .named_children(&mut cursor)
                .filter(|m| m.kind() == "stub_method")
                .filter_map(|m| {
                    Some(entry(m, m.child_by_field_name("name")?, symbol_kind::METHOD, signature(m), Vec::new()))
                })
                .collect();
            let syntax_text = object.child_by_field_name("syntax").map(|s| syntax::text(s, source).trim().to_string());
            result.push(entry(node, name, symbol_kind::CLASS, syntax_text, methods));
        } else if let Some(name) = node.child_by_field_name("name") {
            result.push(entry(node, name, word(node), None, Vec::new()));
        }
    });
    result
}

fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

/// Case-insensitive subsequence match, as most editors do for symbol search.
fn matches(query: &str, name: &str) -> bool {
    let mut name_chars = name.chars().map(|c| c.to_ascii_lowercase());
    query.chars().map(|c| c.to_ascii_lowercase()).all(|q| name_chars.any(|n| n == q))
}

/// 0 exact, 1 prefix, 2 substring, 3 subsequence (case-insensitive).
fn match_tier(query: &str, name: &str) -> u8 {
    let (query, name) = (query.to_lowercase(), name.to_lowercase());
    if name == query {
        0
    } else if name.starts_with(&query) {
        1
    } else if name.contains(&query) {
        2
    } else {
        3
    }
}

pub fn workspace_symbols(workspace: &crate::workspace::Workspace, query: &str) -> Vec<SymbolInformation> {
    let mut result: Vec<SymbolInformation> = workspace
        .symbols()
        // A job's file variables are only visible inside the job.
        .filter(|s| !(s.kind == SymbolKind::FilenameVariable && s.owner.is_some()))
        .filter(|s| matches(query, &s.name))
        .map(|s| SymbolInformation {
            name: s.name.clone(),
            kind: lsp_kind(s.kind),
            location: s.location(),
            container_name: s.owner.clone().or_else(|| s.graph.clone()),
        })
        .collect();
    // Exact, prefix, substring, then subsequence matches; shorter names first.
    result.sort_by_cached_key(|s| (match_tier(query, &s.name), s.name.len(), s.name.clone()));
    result.truncate(500);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;

    #[test]
    fn builds_a_nested_outline() {
        let fixture = Fixture::new(
            "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\nCREATE QUERY q(INT k) {\n  SumAccum<INT> @@n;\n  S = {Person.*};\n}\n",
        );
        let outline = document_symbols(&fixture.snapshot());
        let names: Vec<&str> = outline.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Person", "q"]);
        let person: Vec<&str> = outline[0].children.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(person, vec!["id", "age"]);
        let query: Vec<&str> = outline[1].children.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(query, vec!["@@n", "S"]);
        assert_eq!(outline[1].detail.as_deref(), Some("(INT k)"));
    }

    #[test]
    fn flattens_the_outline_with_container_names() {
        let fixture = Fixture::new(
            "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\nCREATE QUERY q() {\n  S = {Person.*};\n}\n",
        );
        let flat = flatten("file:///a.gsql", &document_symbols(&fixture.snapshot()));
        let rows: Vec<(&str, Option<&str>)> =
            flat.iter().map(|s| (s.name.as_str(), s.container_name.as_deref())).collect();
        assert_eq!(
            rows,
            [("Person", None), ("id", Some("Person")), ("age", Some("Person")), ("q", None), ("S", Some("q"))]
        );
        assert!(flat.iter().all(|s| s.location.uri == "file:///a.gsql"));
        assert_eq!(flat[0].kind, symbol_kind::CLASS);
    }

    #[test]
    fn lists_reverse_edge_attributes_once() {
        let fixture = Fixture::new(
            "CREATE DIRECTED EDGE Knows (FROM Person, TO Person, since INT) WITH REVERSE_EDGE=\"Known_by\"\n",
        );
        let outline = document_symbols(&fixture.snapshot());
        assert_eq!(outline.len(), 1);
        let knows: Vec<&str> = outline[0].children.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(knows, vec!["since", "Known_by"]);
    }

    #[test]
    fn searches_workspace_symbols() {
        let fixture =
            Fixture::new("CREATE VERTEX Person (PRIMARY_ID id STRING)\nCREATE QUERY page_rank() { PRINT 1; }\n");
        let found = workspace_symbols(&fixture.workspace, "prk");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "page_rank");
    }

    #[test]
    fn ranks_exact_and_prefix_matches_before_subsequence_matches() {
        let fixture = Fixture::new(
            "CREATE QUERY q129() { PRINT 1; }\nCREATE QUERY q290() { PRINT 1; }\nCREATE QUERY q29() { PRINT 1; }\nCREATE QUERY xq29y() { PRINT 1; }\n",
        );
        let names: Vec<String> = workspace_symbols(&fixture.workspace, "q29").into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["q29", "q290", "xq29y", "q129"]);
    }

    const STUBS: &str = "// header\nBUILTIN FUNCTION abs(x NUMBER) -> NUMBER;\n// Absolute value.\n\nBUILTIN OBJECT SumAccum<INT | STRING> {\n  /* doc\n     more */\n  METHOD size() -> INT;\n  MUTATOR clear();\n}\nBUILTIN TYPE INT;\nBUILTIN KEYWORD ELSE IF;\nBUILTIN CONSTANT GSQL_INT_MAX;\n";

    #[test]
    fn outlines_the_builtin_stubs() {
        let fixture = Fixture::new(STUBS);
        let outline = document_symbols(&fixture.snapshot());
        let top: Vec<(&str, u8)> = outline.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(
            top,
            [
                ("abs", symbol_kind::FUNCTION),
                ("SumAccum", symbol_kind::CLASS),
                ("INT", symbol_kind::TYPE_PARAMETER),
                ("ELSE IF", symbol_kind::KEY),
                ("GSQL_INT_MAX", symbol_kind::CONSTANT),
            ]
        );
        assert_eq!(outline[0].detail.as_deref(), Some("(x NUMBER) -> NUMBER"));
        let methods: Vec<(&str, Option<&str>)> =
            outline[1].children.iter().map(|m| (m.name.as_str(), m.detail.as_deref())).collect();
        assert_eq!(methods, [("size", Some("() -> INT")), ("clear", Some("()"))]);
        assert_eq!(outline[1].range.start.line, 4);
        assert_eq!(outline[1].range.end.line, 9);
    }

    #[test]
    fn stubs_are_not_workspace_symbols_and_fold() {
        let fixture = Fixture::new(STUBS);
        assert!(workspace_symbols(&fixture.workspace, "").is_empty());
        let ranges = crate::features::folding::folding_ranges(&fixture.snapshot());
        assert!(ranges.iter().any(|r| r.start_line == 4 && r.end_line == 8 && r.kind.is_none()), "{ranges:?}");
        assert!(ranges.iter().any(|r| r.start_line == 5 && r.end_line == 6 && r.kind.is_some()), "{ranges:?}");
    }

    #[test]
    fn outlines_the_generated_reference_file() {
        let fixture = Fixture::new(crate::features::reference::text());
        let outline = document_symbols(&fixture.snapshot());
        let object = outline.iter().find(|s| s.name == "vertex").expect("vertex");
        assert!(!object.children.is_empty() && object.children.iter().all(|m| m.kind == symbol_kind::METHOD));
        assert!(outline.iter().any(|s| s.name == "abs" && s.kind == symbol_kind::FUNCTION));
    }
}
