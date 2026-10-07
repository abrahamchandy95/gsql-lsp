//! Call hierarchy between queries: which queries call a query, and which
//! queries it calls, across the workspace.

use std::collections::BTreeMap;

use crate::analysis::{Role, SymbolKind};
use crate::features::Snapshot;
use crate::lsp::types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range, symbol_kind,
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

fn contains(outer: Range, inner: Range) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Whether a workspace reference is a call of (or command on) a query.
fn names_a_query(reference: &GlobalReference) -> bool {
    reference.kinds.first() == Some(&SymbolKind::Query)
}

/// The query named at `position`: its declaration, a call or a command.
pub fn prepare(snapshot: &Snapshot, position: Position) -> Vec<CallHierarchyItem> {
    let Some(reference) = snapshot.analysis.reference_at(snapshot.offset(position)) else {
        return Vec::new();
    };
    let is_query = match reference.target {
        Some(id) => snapshot.analysis.symbols[id].kind == SymbolKind::Query,
        None => matches!(reference.role, Role::Function | Role::Query),
    };
    if !is_query {
        return Vec::new();
    }
    snapshot.workspace.find(SymbolKind::Query, &reference.name).into_iter().map(item).collect()
}

/// The queries whose bodies call `callee`.
pub fn incoming(workspace: &Workspace, callee: &CallHierarchyItem) -> Vec<CallHierarchyIncomingCall> {
    let mut calls: BTreeMap<(String, String, Position), (&GlobalSymbol, Vec<Range>)> = BTreeMap::new();
    for file in workspace.files() {
        let queries: Vec<&GlobalSymbol> = file.symbols.iter().filter(|s| s.kind == SymbolKind::Query).collect();
        for reference in file.references.iter().filter(|r| r.name == callee.name && names_a_query(r)) {
            // Calls outside a query (RUN QUERY, INSTALL QUERY) have no caller.
            let Some(caller) = queries.iter().find(|q| contains(q.range, reference.range)) else {
                continue;
            };
            let key = (caller.name.clone(), caller.uri.clone(), caller.range.start);
            calls.entry(key).or_insert_with(|| (caller, Vec::new())).1.push(reference.range);
        }
    }
    calls
        .into_values()
        .map(|(caller, ranges)| CallHierarchyIncomingCall { from: item(caller), from_ranges: ranges })
        .collect()
}

/// The queries that the body of `caller` calls.
pub fn outgoing(workspace: &Workspace, caller: &CallHierarchyItem) -> Vec<CallHierarchyOutgoingCall> {
    let Some(file) = workspace.file(&caller.uri) else {
        return Vec::new();
    };
    let mut calls: BTreeMap<&str, Vec<Range>> = BTreeMap::new();
    for reference in file.references.iter().filter(|r| names_a_query(r) && contains(caller.range, r.range)) {
        calls.entry(reference.name.as_str()).or_default().push(reference.range);
    }
    calls
        .into_iter()
        .filter_map(|(name, ranges)| {
            let callee = workspace.find(SymbolKind::Query, name).into_iter().next()?;
            Some(CallHierarchyOutgoingCall { to: item(callee), from_ranges: ranges })
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
        let fixture = Fixture::with_files(&text, &[("file:///test/helpers.gsql", HELPERS)]);
        let snapshot = fixture.snapshot();
        let items = prepare(&snapshot, snapshot.position(offset));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "helper");
        assert_eq!(items[0].uri, "file:///test/helpers.gsql");

        let callers = incoming(&fixture.workspace, &items[0]);
        let summary: Vec<(&str, usize)> = callers.iter().map(|c| (c.from.name.as_str(), c.from_ranges.len())).collect();
        assert_eq!(summary, [("main", 1), ("second", 2)]);

        let main = prepare(&snapshot, Position::new(0, 14));
        let callees = outgoing(&fixture.workspace, &main[0]);
        let names: Vec<&str> = callees.iter().map(|c| c.to.name.as_str()).collect();
        assert_eq!(names, ["helper", "other"]);
    }

    #[test]
    fn only_queries_have_a_call_hierarchy() {
        let (text, offset) = cursor("CREATE QUERY q() {\n  INT x|x = 1;\n  PRINT xx;\n}\n");
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        assert!(prepare(&snapshot, snapshot.position(offset)).is_empty());
    }
}
