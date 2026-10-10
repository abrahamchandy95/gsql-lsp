//! Call hierarchy between queries: which queries call a query, and which
//! queries it calls, across the workspace.

use std::collections::BTreeMap;

use crate::analysis::SymbolKind;
use crate::features::Snapshot;
use crate::features::resolve::{self, Target};
use crate::lsp::types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall,
    Position, Range, symbol_kind,
};
use crate::workspace::{GlobalReference, GlobalSymbol, Workspace};

fn item(query: &GlobalSymbol) -> CallHierarchyItem {
    CallHierarchyItem {
        name: query.name.clone(),
        kind: symbol_kind::FUNCTION,
        detail: Some(query.detail.clone()),
        uri: query.uri.clone(),
        range: query.range,
        selection_range: query.selection,
    }
}

/// Whether a workspace reference is a call of (or command on) a query.
fn names_a_query(workspace: &Workspace, reference: &GlobalReference) -> bool {
    reference.kinds.first() == Some(&SymbolKind::Query)
        && !(reference.call
            && workspace.declares(&[SymbolKind::TupleType], &reference.name))
}

/// The query named at `position`: its declaration, a call or a command.
pub fn prepare(
    snapshot: &Snapshot,
    position: Position,
) -> Vec<CallHierarchyItem> {
    let Some(reference) = snapshot.reference_at(position) else {
        return Vec::new();
    };
    // In a call, a tuple type hides a query of the same name.
    let Some(Target::Global(key)) = resolve::target(snapshot, reference)
    else {
        return Vec::new();
    };
    if key.kind != SymbolKind::Query {
        return Vec::new();
    }
    snapshot
        .workspace
        .find(SymbolKind::Query, &key.name)
        .into_iter()
        .map(item)
        .collect()
}

/// The queries whose bodies call `callee`.
pub fn incoming(
    workspace: &Workspace,
    callee: &CallHierarchyItem,
) -> Vec<CallHierarchyIncomingCall> {
    let mut calls: BTreeMap<
        (String, String, Position),
        (&GlobalSymbol, Vec<Range>),
    > = BTreeMap::new();
    for file in workspace.files() {
        let queries: Vec<&GlobalSymbol> = file
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Query)
            .collect();
        for reference in file
            .references
            .iter()
            .filter(|r| r.name == callee.name && names_a_query(workspace, r))
        {
            // Calls outside a query (RUN QUERY, INSTALL QUERY) have no caller.
            let Some(caller) = queries
                .iter()
                .find(|q| q.range.contains_range(reference.range))
            else {
                continue;
            };
            let key =
                (caller.name.clone(), caller.uri.clone(), caller.range.start);
            calls
                .entry(key)
                .or_insert_with(|| (caller, Vec::new()))
                .1
                .push(reference.range);
        }
    }
    calls
        .into_values()
        .map(|(caller, ranges)| CallHierarchyIncomingCall {
            from: item(caller),
            from_ranges: ranges,
        })
        .collect()
}

/// The queries that the body of `caller` calls.
pub fn outgoing(
    workspace: &Workspace,
    caller: &CallHierarchyItem,
) -> Vec<CallHierarchyOutgoingCall> {
    let Some(file) = workspace.file(&caller.uri) else {
        return Vec::new();
    };
    let mut calls: BTreeMap<&str, Vec<Range>> = BTreeMap::new();
    for reference in file.references.iter().filter(|r| {
        names_a_query(workspace, r) && caller.range.contains_range(r.range)
    }) {
        calls
            .entry(reference.name.as_str())
            .or_default()
            .push(reference.range);
    }
    calls
        .into_iter()
        .filter_map(|(name, ranges)| {
            let callee = workspace
                .find(SymbolKind::Query, name)
                .into_iter()
                .next()?;
            Some(CallHierarchyOutgoingCall {
                to: item(callee),
                from_ranges: ranges,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{Fixture, cursor};

    const HELPERS: &str = "CREATE QUERY helper(INT n) { PRINT n; }\nCREATE QUERY other() { PRINT 1; }\n";

    #[test]
    fn finds_callers_and_callees_across_files() {
        let (text, offset) = cursor(
            "CREATE QUERY main() {\n  helper(1);\n  other();\n}\nCREATE QUERY second() {\n  INT x = 2;\n  hel|per(x);\n  helper(3);\n}\nRUN QUERY helper(4)\n",
        );
        let fixture = Fixture::with_files(
            &text,
            &[("file:///test/helpers.gsql", HELPERS)],
        );
        let snapshot = fixture.snapshot();
        let items = prepare(&snapshot, snapshot.position(offset));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "helper");
        assert_eq!(items[0].uri, "file:///test/helpers.gsql");

        let callers = incoming(&fixture.workspace, &items[0]);
        let summary: Vec<(&str, usize)> = callers
            .iter()
            .map(|c| (c.from.name.as_str(), c.from_ranges.len()))
            .collect();
        assert_eq!(summary, [("main", 1), ("second", 2)]);

        let main = prepare(&snapshot, Position::new(0, 14));
        let callees = outgoing(&fixture.workspace, &main[0]);
        let names: Vec<&str> = callees
            .iter()
            .map(|c| c.to.name.as_str())
            .collect();
        assert_eq!(names, ["helper", "other"]);
    }

    #[test]
    fn a_tuple_type_hides_a_query_of_the_same_name_in_a_call() {
        let query = "CREATE QUERY Pair(STRING a, STRING b) { PRINT a; }\n";
        let tuple = "TYPEDEF TUPLE <INT x, INT y> Pair;\n";
        let c = |typedef: &str| {
            format!(
                "CREATE QUERY c() {{\n{typedef}  ListAccum<Pair> @@l;\n  \
                 @@l += Pa|ir(1, 2);\n  helper(1);\n  PRINT @@l;\n}}\n"
            )
        };
        let helpers = ("file:///test/helpers.gsql", HELPERS);
        // The queries at the cursor, those `c` calls, and the callers of `Pair`.
        let hierarchy = |text: &str, others: &[(&str, &str)]| {
            let (text, offset) = cursor(text);
            let fixture = Fixture::with_files(&text, others);
            let snapshot = fixture.snapshot();
            let names = |items: Vec<CallHierarchyItem>| {
                items
                    .into_iter()
                    .map(|i| i.name)
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let here = prepare(&snapshot, snapshot.position(offset));
            let c = prepare(&snapshot, Position::new(0, 14));
            let callees = outgoing(&fixture.workspace, &c[0]).into_iter();
            let pair = fixture
                .workspace
                .find(SymbolKind::Query, "Pair");
            let callers =
                incoming(&fixture.workspace, &item(pair[0])).into_iter();
            [
                names(here),
                names(callees.map(|call| call.to).collect()),
                names(callers.map(|call| call.from).collect()),
            ]
        };
        let query_file = ("file:///test/q.gsql", query);
        let tuple_file = ("file:///test/t.gsql", tuple);
        // Without a tuple type the call is a call of the query.
        assert_eq!(
            hierarchy(&c(""), &[query_file, helpers]),
            ["Pair", "Pair helper", "c"]
        );
        // A tuple type in the workspace: the call builds a tuple.
        let builds_a_tuple = ["", "helper", ""];
        assert_eq!(
            hierarchy(&c(""), &[query_file, tuple_file, helpers]),
            builds_a_tuple
        );
        let text = format!("{}{query}", c(""));
        assert_eq!(hierarchy(&text, &[tuple_file, helpers]), builds_a_tuple);
        // A tuple type in the caller's body.
        let typedef = format!("  {tuple}");
        assert_eq!(
            hierarchy(&c(&typedef), &[query_file, helpers]),
            builds_a_tuple
        );
    }

    #[test]
    fn only_queries_have_a_call_hierarchy() {
        let (text, offset) =
            cursor("CREATE QUERY q() {\n  INT x|x = 1;\n  PRINT xx;\n}\n");
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        assert!(prepare(&snapshot, snapshot.position(offset)).is_empty());
    }
}
