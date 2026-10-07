//! Hover documentation for keywords, types, built-ins and symbols.

use crate::analysis::{SymbolKind, Ty};
use crate::builtins;
use crate::features::Snapshot;
use crate::features::resolve::{self, Target};
use crate::lsp::types::{Hover, MarkupContent, Position};
use crate::syntax;
use crate::text::Span;
use crate::workspace::GlobalSymbol;

pub fn hover(snapshot: &Snapshot, position: Position) -> Option<Hover> {
    super::navigation::in_repaired(
        snapshot,
        position,
        hover_here,
        |found| found.is_some(),
        |found, repair, _| {
            found.map(|mut hover| {
                hover.range = hover.range.map(|range| repair.range(range));
                hover
            })
        },
    )
}

fn hover_here(snapshot: &Snapshot, position: Position) -> Option<Hover> {
    let offset = snapshot.offset(position);
    if let Some(reference) = snapshot.analysis.reference_at(offset) {
        let target = resolve::target(snapshot, reference)?;
        let markdown = describe(snapshot, &target)?;
        return Some(Hover {
            contents: MarkupContent::markdown(markdown),
            range: Some(snapshot.range(reference.span)),
        });
    }
    // A value of a VALUES list: the attribute it fills.
    if let Some(node) = snapshot.root().descendant_for_byte_range(offset, offset)
        && matches!(node.kind(), "column_reference" | "wildcard" | "_")
    {
        let lists = super::values::value_lists(snapshot);
        if let Some((list, slot)) = super::values::slot_at(&lists, offset) {
            let verb = if list.load { "Loads into" } else { "Inserts into" };
            return Some(Hover {
                contents: MarkupContent::markdown(format!("{verb} `{}`", slot.detail)),
                range: Some(snapshot.range(Span::of(node))),
            });
        }
    }
    // Keywords and built-in type names.
    let node = syntax::word_at(snapshot.root(), offset)?;
    let text = syntax::text(node, snapshot.text());
    let markdown = match node.kind() {
        "accumulator_kind" => builtins::accumulator(text).map(|a| {
            let doc = crate::builtin_docs::accumulator(a.name).map_or_else(|| a.doc.to_string(), |p| p.markdown());
            format!("```gsql\n{}\n```\n{doc}{}", a.syntax, method_list(a.methods))
        }),
        // `UPDATE p FROM ... SET p.a = 1`: the clause keyword, not the SET collection type.
        _ if node.kind() == "SET" && node.parent().is_some_and(|p| p.kind() == "update_statement") => None,
        _ if syntax::is_keyword(node) => {
            let keyword = node.kind();
            builtins::primitive_type(keyword)
                .map(|doc| format!("```gsql\n{keyword}\n```\n{doc}"))
                .or_else(|| builtins::keyword(keyword).map(|doc| format!("**{keyword}**\n\n{doc}")))
        }
        _ => None,
    }?;
    Some(Hover { contents: MarkupContent::markdown(markdown), range: Some(snapshot.range(Span::of(node))) })
}

fn method_list(methods: &[builtins::Method]) -> String {
    if methods.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = methods.iter().map(|m| format!("- `{}`", m.signature())).collect();
    format!("\n\n**Methods**\n{}", lines.join("\n"))
}

fn code(text: &str) -> String {
    format!("```gsql\n{text}\n```")
}

fn with_doc(mut markdown: String, doc: Option<&str>) -> String {
    if let Some(doc) = doc {
        markdown.push_str("\n\n");
        markdown.push_str(doc);
    }
    markdown
}

pub fn describe(snapshot: &Snapshot, target: &Target) -> Option<String> {
    match target {
        Target::Local(id) => {
            let symbol = &snapshot.analysis.symbols[*id];
            // An unknown type is not worth printing: `i: unknown`.
            let detail = symbol.detail.strip_suffix(": unknown").unwrap_or(&symbol.detail);
            let mut markdown = code(detail);
            markdown.push_str(&format!("\n*{}*", symbol.kind.label()));
            match (&symbol.kind, &symbol.ty) {
                (SymbolKind::Alias | SymbolKind::LoopVariable | SymbolKind::VertexSet, ty) if *ty != Ty::Unknown => {}
                (_, Ty::Accumulator(kind, _)) => {
                    if let Some(accumulator) = builtins::accumulator(kind) {
                        markdown.push_str(&format!("\n\n{}", accumulator.doc));
                    }
                }
                _ => {}
            }
            Some(with_doc(markdown, symbol.doc.as_deref()))
        }
        Target::Global(key) => {
            let declarations = resolve::declarations(snapshot, key);
            let symbol = declarations.first()?;
            if symbol.kind == SymbolKind::Attribute && key.owner.is_none() && declarations.len() > 1 {
                return Some(describe_shared_attribute(&declarations));
            }
            Some(describe_global(snapshot, symbol, declarations.len()))
        }
        Target::Function(function) => {
            let detail =
                crate::builtin_docs::function(function.name).map_or_else(|| function.doc.to_string(), |p| p.markdown());
            Some(format!("{}\n*{}*\n\n{detail}", code(&function.signature()), function.category.label()))
        }
        Target::Method(method) => {
            let detail = crate::builtin_docs::method(method).map_or_else(|| method.doc.to_string(), |p| p.markdown());
            Some(format!("{}\n\n{detail}", code(&method.signature())))
        }
        Target::Constant(name, doc) => Some(format!("{}\n\n{doc}", code(name))),
    }
}

/// An attribute declared by several types: the declaring types and the type
/// of each; one line per type only when the declarations differ.
fn describe_shared_attribute(declarations: &[&GlobalSymbol]) -> String {
    let split = |s: &GlobalSymbol| {
        let owner = s.owner.clone().unwrap_or_else(|| "?".to_string());
        (owner, s.detail.clone())
    };
    let (_, first) = split(declarations[0]);
    let text = if declarations.iter().all(|d| split(d).1 == first) {
        let owners: Vec<String> = declarations.iter().map(|d| split(d).0).collect();
        let names =
            owners.iter().map(|o| format!("{o}.{}", first.split_whitespace().next().unwrap_or(""))).collect::<Vec<_>>();
        let ty = first.split_once(char::is_whitespace).map_or("", |(_, t)| t.trim());
        format!("{}: {ty}", names.join(", "))
    } else {
        declarations.iter().map(|d| format!("{}.{}", split(d).0, d.detail)).collect::<Vec<_>>().join("\n")
    };
    format!("{}\n*attribute* ({} declarations)", code(&text), declarations.len())
}

fn describe_global(snapshot: &Snapshot, symbol: &GlobalSymbol, count: usize) -> String {
    let workspace = snapshot.workspace;
    let mut markdown = match symbol.kind {
        SymbolKind::VertexType | SymbolKind::EdgeType => {
            let attributes = workspace.attributes_declared(&symbol.name);
            let header = if symbol.kind == SymbolKind::VertexType {
                format!("VERTEX {}", symbol.name)
            } else if symbol.detail.starts_with("reverse edge of") {
                format!("EDGE {} -- {}", symbol.name, symbol.detail)
            } else {
                let direction =
                    if symbol.detail.to_ascii_uppercase().contains("UNDIRECTED") { "UNDIRECTED" } else { "DIRECTED" };
                format!("{direction} EDGE {}", symbol.name)
            };
            let mut lines = vec![header];
            if symbol.kind == SymbolKind::EdgeType && !symbol.members.is_empty() {
                lines.push(format!("  -- connects {}", symbol.members.join(", ")));
            }
            for attribute in attributes {
                lines.push(format!("  {}", attribute.detail));
            }
            code(&lines.join("\n"))
        }
        SymbolKind::Attribute => {
            let owner = symbol.owner.as_deref().unwrap_or("?");
            code(&format!("{owner}.{}", symbol.detail))
        }
        SymbolKind::Graph => {
            let mut text = format!("GRAPH {}", symbol.name);
            if !symbol.members.is_empty() {
                text.push_str(&format!(" ({})", symbol.members.join(", ")));
            }
            code(&text)
        }
        SymbolKind::Query => {
            let mut lines = vec![symbol.detail.clone()];
            if !symbol.params.is_empty() {
                lines.push(String::new());
                for param in &symbol.params {
                    let default = param.default.as_ref().map(|d| format!(" = {d}")).unwrap_or_default();
                    lines.push(format!("  {} {}{default}", param.ty, param.name));
                }
            }
            code(&lines.join("\n"))
        }
        _ => code(&symbol.detail),
    };
    markdown.push_str(&format!("\n*{}*", symbol.kind.label()));
    if count > 1 {
        markdown.push_str(&format!(" ({count} declarations)"));
    }
    with_doc(markdown, symbol.doc.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{Fixture, cursor};

    fn hover_at(text: &str, others: &[(&str, &str)]) -> Option<String> {
        let (text, offset) = cursor(text);
        let fixture = Fixture::with_files(&text, others);
        let snapshot = fixture.snapshot();
        let position = snapshot.position(offset);
        hover(&snapshot, position).map(|h| h.contents.value)
    }

    #[test]
    fn hovers_a_tuple_type_named_after_a_soft_keyword() {
        let text = "TYPEDEF TUPLE<INT n, STRING s> list\nCREATE QUERY q() FOR GRAPH G {\n  ListAccum<li|st> @@l;\n  PRINT @@l;\n}\n";
        let markdown = hover_at(text, &[]).expect("hover on the tuple type");
        assert!(markdown.contains("list"), "{markdown}");
    }

    #[test]
    fn hovers_query_calls_with_the_return_type() {
        let other = "CREATE QUERY helper(INT a) FOR GRAPH G RETURNS (INT) { RETURN a; }";
        let markdown = hover_at("CREATE QUERY q() { INT x = hel|per(1); }", &[("file:///test/h.gsql", other)]).unwrap();
        assert!(markdown.contains("RETURNS (INT)"), "{markdown}");
    }

    #[test]
    fn hovers_accumulators_with_docs() {
        let markdown = hover_at(
            "CREATE QUERY q() {\n  // Number of visits.\n  SumAccum<INT> @@visits;\n  @@vi|sits += 1;\n}\n",
            &[],
        )
        .unwrap();
        assert!(markdown.contains("SumAccum<INT> @@visits"), "{markdown}");
        assert!(markdown.contains("Number of visits."), "{markdown}");
    }

    #[test]
    fn hovers_keywords_and_builtins() {
        let markdown = hover_at("CREATE QUERY q() { PRI|NT abs(1); }", &[]).unwrap();
        assert!(markdown.contains("PRINT"), "{markdown}");
        let markdown = hover_at("CREATE QUERY q() { PRINT ab|s(1); }", &[]).unwrap();
        assert!(markdown.contains("abs(num) -> number"), "{markdown}");
        let markdown = hover_at("CREATE QUERY q() { Sum|Accum<INT> @@a; }", &[]).unwrap();
        assert!(markdown.contains("cumulative sum"), "{markdown}");
    }

    #[test]
    fn hovers_schema_types_across_files() {
        let schema = "// A human.\nCREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\n";
        let markdown =
            hover_at("CREATE QUERY q() { S = {Per|son.*}; PRINT S; }", &[("file:///test/schema.gsql", schema)])
                .unwrap();
        assert!(markdown.contains("VERTEX Person"), "{markdown}");
        assert!(markdown.contains("age INT"), "{markdown}");
        assert!(markdown.contains("A human."), "{markdown}");
    }

    #[test]
    fn lists_attributes_in_declaration_order() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, zeta INT, alpha INT, mid DOUBLE)\n";
        let markdown =
            hover_at("CREATE QUERY q() { S = {Per|son.*}; PRINT S; }", &[("file:///test/schema.gsql", schema)])
                .unwrap();
        let order: Vec<usize> =
            ["PRIMARY_ID id", "zeta", "alpha", "mid"].iter().map(|a| markdown.find(a).unwrap()).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{markdown}");
    }

    #[test]
    fn hovers_accumulator_typedefs() {
        let markdown = hover_at(
            "CREATE QUERY q() {\n  TYPEDEF TUPLE<STRING k, INT v> Rec;\n  TYPEDEF HeapAccum<Rec>(3, v DESC) Top;\n  To|p @@top;\n  PRINT @@top;\n}\n",
            &[],
        )
        .unwrap();
        assert!(markdown.contains("TYPEDEF HeapAccum<Rec>(3, v DESC) Top"), "{markdown}");
        let markdown = hover_at(
            "CREATE QUERY q() {\n  TYPEDEF TUPLE<STRING k, INT v> Rec;\n  TYPEDEF HeapAccum<Rec>(3, v DESC) Top;\n  Top @@top;\n  PRINT @@t|op;\n}\n",
            &[],
        )
        .unwrap();
        // The accumulator's type comes from the TYPEDEF.
        assert!(markdown.contains("Top @@top") && markdown.contains("Priority queue"), "{markdown}");
    }

    #[test]
    fn hovers_values_with_their_attribute() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\n";
        let markdown = hover_at(
            "CREATE LOADING JOB j FOR GRAPH g {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX Person VALUES ($0, $1, $|2);\n}\n",
            &[("file:///test/schema.gsql", schema)],
        )
        .unwrap();
        assert_eq!(markdown, "Loads into `Person.age INT`");
    }

    #[test]
    fn hovers_attributes_through_aliases() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\n";
        let markdown = hover_at(
            "CREATE QUERY q() { R = SELECT s FROM Person:s WHERE s.a|ge > 1; PRINT R; }",
            &[("file:///test/schema.gsql", schema)],
        )
        .unwrap();
        assert!(markdown.contains("Person.age INT"), "{markdown}");
    }

    #[test]
    fn hovers_the_loop_variable_of_a_range() {
        let markdown = hover_at("CREATE QUERY q() {\n  FOREACH i| IN RANGE[0, 3] DO PRINT i; END;\n}\n", &[]).unwrap();
        assert!(markdown.contains("i: INT"), "{markdown}");
    }

    #[test]
    fn hover_does_not_print_an_unknown_type() {
        let markdown = hover_at("CREATE QUERY q(INT k) {\n  FOREACH x| IN k DO PRINT x; END;\n}\n", &[]).unwrap();
        assert!(!markdown.contains("unknown"), "{markdown}");
        assert!(markdown.contains("x\n"), "{markdown}");
    }

    #[test]
    fn hovers_methods_of_collection_parameters() {
        let head = "CREATE QUERY q(SET<STRING> ss, LIST<INT> l, MAP<STRING, INT> m, STRING s) {\n  PRINT ";
        let markdown = hover_at(&format!("{head}ss.cont|ains(\"a\");\n}}\n"), &[]).unwrap();
        assert!(markdown.contains("contains(value)"), "{markdown}");
        let markdown = hover_at(&format!("{head}m.containsK|ey(\"a\");\n}}\n"), &[]).unwrap();
        assert!(markdown.contains("containsKey(key)"), "{markdown}");
        let markdown = hover_at(&format!("{head}m.ge|t(\"a\");\n}}\n"), &[]).unwrap();
        assert!(markdown.contains("get(key)"), "{markdown}");
        let markdown = hover_at(&format!("{head}l.si|ze();\n}}\n"), &[]).unwrap();
        assert!(markdown.contains("size()"), "{markdown}");
        // Not a documented method of any collection.
        assert!(hover_at(&format!("{head}ss.fo|o();\n}}\n"), &[]).is_none());
    }

    #[test]
    fn hovers_an_attribute_shared_by_several_types() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE VERTEX City (PRIMARY_ID id STRING, name STRING, age UINT)\n";
        let text = "CREATE QUERY q() FOR GRAPH G {\n  R = SELECT t FROM (Person:City):t WHERE t.name == \"a\" AND t.age > 1;\n}\n";
        let text = text.replace(":City", "|City");
        let others = [("file:///test/schema.gsql", schema)];
        let hover_on = |needle: &str| {
            let fixture = Fixture::with_files(&text, &others);
            let snapshot = fixture.snapshot();
            let position = snapshot.position(text.find(needle).unwrap() + 3);
            hover(&snapshot, position).map(|h| h.contents.value).expect("hover")
        };
        let markdown = hover_on("t.name");
        assert!(markdown.contains("Person.name, City.name: STRING"), "{markdown}");
        assert!(markdown.contains("(2 declarations)"), "{markdown}");
        let markdown = hover_on("t.age");
        assert!(markdown.contains("Person.age INT\nCity.age UINT"), "{markdown}");
    }

    #[test]
    fn hovers_an_alias_typed_by_its_edge() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\nCREATE VERTEX Movie (PRIMARY_ID id STRING)\nCREATE DIRECTED EDGE ACTED_IN (FROM Person, TO Movie)\n";
        let query = "CREATE QUERY q() FOR GRAPH G {\n  A = SELECT m FROM Person:p -(ACTED_IN>)- :m| ;\n  PRINT A;\n}\n";
        let markdown = hover_at(query, &[("file:///test/schema.gsql", schema)]).unwrap();
        assert!(markdown.contains("m: VERTEX<Movie>"), "{markdown}");
    }

    #[test]
    fn update_set_is_not_the_set_collection_type() {
        let text =
            "CREATE QUERY q() FOR GRAPH G {\n  UPDATE p FROM Person:p S|ET p.name = \"x\";\n  SetAccum<INT> @@s;\n}\n";
        let markdown = hover_at(text, &[]);
        assert!(markdown.as_deref().is_none_or(|m| !m.contains("Collection")), "{markdown:?}");
        // The SET type keeps its hover.
        let text = "CREATE QUERY q() FOR GRAPH G {\n  S|et<INT> @@s;\n}\n".replace("S|et", "S|ET");
        assert!(hover_at(&text, &[]).is_some_and(|m| m.contains("Collection")));
    }
}
