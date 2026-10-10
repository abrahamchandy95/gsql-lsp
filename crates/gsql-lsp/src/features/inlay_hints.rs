//! Inlay hints: inferred alias and loop-variable types, query argument names,
//! and the attribute each value of a VALUES list fills.

use crate::analysis::{SymbolKind, Ty};
use crate::features::{Snapshot, resolve};
use crate::lsp::types::{InlayHint, Range, inlay_hint_kind};
use crate::syntax;

pub fn inlay_hints(snapshot: &Snapshot, range: Range) -> Vec<InlayHint> {
    if !snapshot.config.inlay_hints {
        return Vec::new();
    }
    let start = snapshot.offset(range.start);
    let end = snapshot.offset(range.end);
    let mut hints = Vec::new();
    type_hints(snapshot, start, end, &mut hints);
    argument_hints(snapshot, start, end, &mut hints);
    value_hints(snapshot, start, end, &mut hints);
    hints.sort_by_key(|h| h.position);
    hints
}

/// `FROM Start:s` -> `s: Person` when `Start` is a vertex set, and loop
/// variables with a known element type.
fn type_hints(
    snapshot: &Snapshot,
    start: usize,
    end: usize,
    hints: &mut Vec<InlayHint>,
) {
    let source = snapshot.text();
    for reference in &snapshot.analysis.references {
        if !reference.declaration
            || reference.span.end < start
            || reference.span.start > end
        {
            continue;
        }
        let Some(symbol) = snapshot.analysis.target_symbol(reference) else {
            continue;
        };
        let label = match (symbol.kind, &symbol.ty) {
            (SymbolKind::Alias, Ty::Vertex(types) | Ty::Edge(types))
                if !types.is_empty() =>
            {
                // Only when the pattern does not already spell out the type.
                let Some(node) = snapshot.node_at(reference.span) else {
                    continue;
                };
                let Some(pattern) = node.parent() else {
                    continue;
                };
                let spelled = pattern
                    .child_by_field_name("type")
                    .is_some_and(|t| {
                        let text = syntax::text(t, source);
                        types.len() == 1 && types[0] == text
                    });
                let is_vertex_set = pattern
                    .child_by_field_name("type")
                    .is_some_and(|t| {
                        matches!(
                            t.kind(),
                            "identifier" | "global_accumulator"
                        )
                    });
                // An untyped alias has its type from the edge before it.
                let from_edge = pattern.child_by_field_name("type").is_none()
                    && matches!(
                        pattern.kind(),
                        "vertex_pattern" | "node_pattern"
                    );
                if !from_edge
                    && (spelled
                        || pattern.kind() != "vertex_pattern"
                        || !is_vertex_set)
                {
                    continue;
                }
                types.join(" | ")
            }
            (SymbolKind::LoopVariable, ty) if *ty != Ty::Unknown => {
                ty.display()
            }
            _ => continue,
        };
        hints.push(InlayHint {
            position: snapshot.position(reference.span.end),
            label: format!(": {label}"),
            kind: Some(inlay_hint_kind::TYPE),
            padding_left: false,
            padding_right: false,
        });
    }
}

/// Parameter names for positional arguments of query calls.
fn argument_hints(
    snapshot: &Snapshot,
    start: usize,
    end: usize,
    hints: &mut Vec<InlayHint>,
) {
    syntax::walk(snapshot.root(), |node| {
        if node.end_byte() < start || node.start_byte() > end {
            return;
        }
        // A call by plain name may build a tuple instead.
        let Some((query, _, arguments)) =
            resolve::query_called_by(snapshot, node)
        else {
            return;
        };
        let args = syntax::code_children(arguments);
        if args.len() < 2 {
            return;
        }
        for (arg, param) in args.iter().zip(&query.params) {
            hints.push(InlayHint {
                position: snapshot.position(arg.start_byte()),
                label: format!("{}:", param.name),
                kind: Some(inlay_hint_kind::PARAMETER),
                padding_left: false,
                padding_right: true,
            });
        }
    });
}

/// `VALUES ($0, $1, $2)` -> `id: $0, name: $1, age: $2`, unless a value
/// already names its attribute (`$"name"`).
fn value_hints(
    snapshot: &Snapshot,
    start: usize,
    end: usize,
    hints: &mut Vec<InlayHint>,
) {
    let source = snapshot.text();
    for list in super::values::value_lists(snapshot) {
        if list.list.end_byte() < start
            || list.list.start_byte() > end
            || list.values.len() < 2
        {
            continue;
        }
        for (value, slot) in list.values.iter().zip(&list.slots) {
            if super::values::names_its_slot(*value, slot, source) {
                continue;
            }
            hints.push(InlayHint {
                position: snapshot.position(value.start_byte()),
                label: format!("{}:", slot.name),
                kind: Some(inlay_hint_kind::PARAMETER),
                padding_left: false,
                padding_right: true,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;
    use crate::lsp::types::Position;

    #[test]
    fn hints_alias_and_loop_types() {
        let fixture = Fixture::new(
            "CREATE QUERY q() {\n  SetAccum<INT> @@ids;\n  S = {Person.*};\n  R = SELECT t FROM S:s -(E:e)- Person:t;\n  FOREACH i IN @@ids DO PRINT i; END;\n}\n",
        );
        let snapshot = fixture.snapshot();
        let hints = inlay_hints(
            &snapshot,
            Range::new(Position::new(0, 0), Position::new(10, 0)),
        );
        let labels: Vec<&str> = hints
            .iter()
            .map(|h| h.label.as_str())
            .collect();
        assert_eq!(labels, vec![": Person", ": INT"]);
    }

    #[test]
    fn hints_the_attributes_of_values() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\n";
        let fixture = Fixture::with_schema(
            "CREATE LOADING JOB j FOR GRAPH g {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX Person VALUES ($0, $\"name\", _);\n}\n",
            schema,
        );
        let snapshot = fixture.snapshot();
        let hints = inlay_hints(
            &snapshot,
            Range::new(Position::new(0, 0), Position::new(5, 0)),
        );
        let labels: Vec<&str> = hints
            .iter()
            .map(|h| h.label.as_str())
            .collect();
        assert_eq!(labels, vec!["id:", "age:"]);
    }

    #[test]
    fn hints_query_argument_names() {
        let fixture = Fixture::new(
            "CREATE QUERY helper(INT a, STRING b) { PRINT a, b; }\nRUN QUERY helper(1, \"x\")\n",
        );
        let snapshot = fixture.snapshot();
        let hints = inlay_hints(
            &snapshot,
            Range::new(Position::new(0, 0), Position::new(5, 0)),
        );
        let labels: Vec<&str> = hints
            .iter()
            .map(|h| h.label.as_str())
            .collect();
        assert_eq!(labels, vec!["a:", "b:"]);
    }

    #[test]
    fn hints_argument_names_of_interpret_query() {
        let fixture = Fixture::new(
            "CREATE QUERY helper(INT a, STRING b) { PRINT a, b; }\nINTERPRET QUERY helper(1, \"x\")\n",
        );
        let snapshot = fixture.snapshot();
        let hints = inlay_hints(
            &snapshot,
            Range::new(Position::new(0, 0), Position::new(5, 0)),
        );
        let labels: Vec<&str> = hints
            .iter()
            .map(|h| h.label.as_str())
            .collect();
        assert_eq!(labels, vec!["a:", "b:"]);
    }

    /// The labels of every hint in `text`, with `others` in the workspace.
    fn labels(text: &str, others: &[(&str, &str)]) -> Vec<String> {
        let fixture = Fixture::with_files(text, others);
        let snapshot = fixture.snapshot();
        let range = Range::new(Position::new(0, 0), Position::new(100, 0));
        inlay_hints(&snapshot, range)
            .into_iter()
            .map(|h| h.label)
            .collect()
    }

    #[test]
    fn hints_argument_names_of_a_query_called_by_name() {
        let helper = (
            "file:///test/h.gsql",
            "CREATE QUERY helper(INT a, INT b) RETURNS (INT) { RETURN a; }\n",
        );
        let text =
            "CREATE QUERY c() {\n  INT n = helper(1, 2);\n  PRINT n;\n}\n";
        assert_eq!(labels(text, &[helper]), vec!["a:", "b:"]);
    }

    #[test]
    fn a_tuple_type_hides_a_query_of_the_same_name_in_a_call() {
        let query = (
            "file:///test/q.gsql",
            "CREATE QUERY Pair(STRING a, STRING b) { PRINT a; }\n",
        );
        // A tuple type in the workspace; `RUN QUERY` still names the query.
        let tuple = (
            "file:///test/t.gsql",
            "TYPEDEF TUPLE <INT x, INT y> Pair;\n",
        );
        let text = concat!(
            "CREATE QUERY c() {\n",
            "  ListAccum<Pair> @@l;\n",
            "  @@l += Pair(1, 2);\n",
            "  PRINT @@l;\n",
            "}\n",
            "RUN QUERY Pair(\"s\", \"t\")\n",
        );
        assert_eq!(labels(text, &[query, tuple]), vec!["a:", "b:"]);
        // A tuple type in the query body.
        let text = concat!(
            "CREATE QUERY c() {\n",
            "  TYPEDEF TUPLE <INT x, INT y> Pair;\n",
            "  ListAccum<Pair> @@l;\n",
            "  @@l += Pair(1, 2);\n",
            "  PRINT @@l;\n",
            "}\n",
        );
        assert_eq!(labels(text, &[query]), Vec::<String>::new());
    }

    #[test]
    fn hints_the_loop_variable_of_ranges_and_literals() {
        let fixture = Fixture::new(
            "CREATE QUERY q(INT n) {\n  FOREACH i IN RANGE[0, n] DO PRINT i; END;\n  FOREACH c IN [1, 2] DO PRINT c; END;\n  FOREACH s IN (\"a\", \"b\") DO PRINT s; END;\n  FOREACH m IN [1, \"a\"] DO PRINT m; END;\n}\n",
        );
        let snapshot = fixture.snapshot();
        let hints = inlay_hints(
            &snapshot,
            Range::new(Position::new(0, 0), Position::new(10, 0)),
        );
        let labels: Vec<&str> = hints
            .iter()
            .map(|h| h.label.as_str())
            .collect();
        // Mixed literals stay untyped.
        assert_eq!(labels, vec![": INT", ": INT", ": STRING"]);
    }

    #[test]
    fn hints_alias_through_a_schema_edge() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\nCREATE VERTEX Movie (PRIMARY_ID id STRING)\nCREATE DIRECTED EDGE ACTED_IN (FROM Person, TO Movie)\n";
        let fixture = Fixture::with_schema(
            "CREATE QUERY q() FOR GRAPH G SYNTAX V3 {\n  SELECT m INTO T FROM (p:Person)-[:ACTED_IN]->(m);\n  PRINT T;\n}\n",
            schema,
        );
        let snapshot = fixture.snapshot();
        let hints = inlay_hints(
            &snapshot,
            Range::new(Position::new(0, 0), Position::new(10, 0)),
        );
        let labels: Vec<&str> = hints
            .iter()
            .map(|h| h.label.as_str())
            .collect();
        assert!(labels.contains(&": Movie"), "{labels:?}");
    }
}
