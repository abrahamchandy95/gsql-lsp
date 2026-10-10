//! Go to definition, find references, rename and document highlights.

use std::collections::BTreeMap;

use crate::analysis::{Reference, Role, ScopeId, SymbolId, SymbolKind};
use crate::features::Snapshot;
use crate::features::resolve::{self, Target};
use crate::lsp::types::{
    DocumentHighlight, Location, Position, Range, TextEdit, WorkspaceEdit,
    highlight_kind,
};
use crate::syntax::BuiltinWord;
use crate::text::is_identifier;

/// Runs a feature on the document as written and, when that finds nothing
/// and a misspelled keyword has thrown the parser off (so that a variable
/// declared in the broken statement is unknown), on the document as
/// corrected, with the result placed back in the written one.
pub(crate) fn in_repaired<R>(
    snapshot: &Snapshot,
    position: Position,
    run: impl Fn(&Snapshot, Position) -> R,
    found: impl Fn(&R) -> bool,
    back: impl Fn(R, &crate::features::autocorrect::Repair, &str) -> R,
) -> R {
    let result = run(snapshot, position);
    if found(&result) {
        return result;
    }
    let Some(repair) = crate::features::autocorrect::repair_of(snapshot)
    else {
        return result;
    };
    let analysis = repair.analysis(snapshot.workspace);
    let repaired =
        snapshot.with_document(&repair.source, &repair.tree, &analysis);
    let again = run(&repaired, repair.to_repaired(position));
    if found(&again) {
        back(again, &repair, snapshot.uri)
    } else {
        result
    }
}

fn back_locations(
    mut locations: Vec<Location>,
    repair: &crate::features::autocorrect::Repair,
    uri: &str,
) -> Vec<Location> {
    for location in locations.iter_mut().filter(|l| l.uri == uri) {
        location.range = repair.range(location.range);
    }
    locations
}

pub fn definition(snapshot: &Snapshot, position: Position) -> Vec<Location> {
    in_repaired(
        snapshot,
        position,
        definition_here,
        |locations| !locations.is_empty(),
        back_locations,
    )
}

fn definition_here(snapshot: &Snapshot, position: Position) -> Vec<Location> {
    let Some(reference) = snapshot.reference_at(position) else {
        return super::links::definition(snapshot, position)
            .or_else(|| builtin_word(snapshot, position))
            .into_iter()
            .collect();
    };
    match resolve::target(snapshot, reference) {
        Some(Target::Local(id)) => {
            let symbol = &snapshot.analysis.symbols[id];
            vec![snapshot.location(symbol.name_span)]
        }
        Some(Target::Global(key)) => resolve::declarations(snapshot, &key)
            .into_iter()
            .map(|s| s.location())
            .collect(),
        // Built-ins have no source: their documentation entry stands in.
        Some(Target::Function(function)) => {
            super::reference::function(function.name)
                .into_iter()
                .collect()
        }
        Some(Target::Method(method)) => super::reference::method(method)
            .into_iter()
            .collect(),
        Some(Target::Constant(name, _)) => super::reference::constant(name)
            .into_iter()
            .collect(),
        None => Vec::new(),
    }
}

/// The documentation entry of a keyword, built-in type or accumulator type
/// under the cursor.
fn builtin_word(snapshot: &Snapshot, position: Position) -> Option<Location> {
    let (_, word) = crate::syntax::builtin_word_at(
        snapshot.root(),
        snapshot.text(),
        snapshot.offset(position),
    )?;
    match word {
        BuiltinWord::Accumulator(name) => super::reference::accumulator(name),
        BuiltinWord::Keyword(keyword) => super::reference::keyword(keyword),
    }
}

/// Go to the type of a variable, alias or parameter.
pub fn type_definition(
    snapshot: &Snapshot,
    position: Position,
) -> Vec<Location> {
    let Some(reference) = snapshot.reference_at(position) else {
        return Vec::new();
    };
    let Some(Target::Local(id)) = resolve::target(snapshot, reference) else {
        return Vec::new();
    };
    let ty = &snapshot.analysis.symbols[id].ty;
    let (kind, names) = match ty {
        crate::analysis::Ty::Tuple(name) => {
            (SymbolKind::TupleType, vec![name.clone()])
        }
        _ => match ty.schema_owners() {
            Some((kind, types)) => (kind, types.to_vec()),
            None => return Vec::new(),
        },
    };
    let mut locations: Vec<Location> = names
        .iter()
        .flat_map(|name| snapshot.workspace.find(kind, name))
        .map(|s| s.location())
        .collect();
    if locations.is_empty() {
        // Tuples declared inside the query live in this file only.
        for symbol in &snapshot.analysis.symbols {
            if symbol.kind == kind && names.contains(&symbol.name) {
                locations.push(snapshot.location(symbol.name_span));
            }
        }
    }
    locations
}

pub fn references(
    snapshot: &Snapshot,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    in_repaired(
        snapshot,
        position,
        |s, p| references_here(s, p, include_declaration),
        |locations| !locations.is_empty(),
        back_locations,
    )
}

fn references_here(
    snapshot: &Snapshot,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    let Some(reference) = snapshot.reference_at(position) else {
        return Vec::new();
    };
    let mut locations = match resolve::target(snapshot, reference) {
        Some(Target::Local(id)) => snapshot
            .analysis
            .references_to(id)
            .filter(|r| include_declaration || !r.declaration)
            .map(|r| snapshot.location(r.span))
            .collect(),
        Some(Target::Global(key)) => {
            let mut locations = snapshot.workspace.references(
                key.kind,
                &key.name,
                key.owner.as_deref(),
                true,
            );
            if include_declaration {
                locations.extend(
                    resolve::declarations(snapshot, &key)
                        .into_iter()
                        .map(|s| s.location()),
                );
            }
            locations
        }
        Some(
            target @ (Target::Function(_)
            | Target::Method(_)
            | Target::Constant(..)),
        ) => same_target_in_document(snapshot, &target)
            .into_iter()
            .map(|r| snapshot.location(r.span))
            .collect(),
        None => Vec::new(),
    };
    locations.sort_by(|a, b| {
        (&a.uri, a.range.start).cmp(&(&b.uri, b.range.start))
    });
    locations.dedup();
    locations
}

fn same_target_in_document<'a>(
    snapshot: &'a Snapshot,
    target: &Target,
) -> Vec<&'a Reference> {
    snapshot
        .analysis
        .references
        .iter()
        .filter(|r| {
            resolve::target(snapshot, r).is_some_and(|t| t.same_as(target))
        })
        .collect()
}

pub fn document_highlight(
    snapshot: &Snapshot,
    position: Position,
) -> Vec<DocumentHighlight> {
    in_repaired(
        snapshot,
        position,
        highlight_here,
        |found| !found.is_empty(),
        |mut found, repair, _| {
            for highlight in &mut found {
                highlight.range = repair.range(highlight.range);
            }
            found
        },
    )
}

fn highlight_here(
    snapshot: &Snapshot,
    position: Position,
) -> Vec<DocumentHighlight> {
    let Some(reference) = snapshot.reference_at(position) else {
        return Vec::new();
    };
    let Some(target) = resolve::target(snapshot, reference) else {
        return Vec::new();
    };
    let matches: Vec<&Reference> = match &target {
        Target::Local(id) => snapshot
            .analysis
            .references_to(*id)
            .collect(),
        _ => same_target_in_document(snapshot, &target),
    };
    matches
        .into_iter()
        .map(|r| DocumentHighlight {
            range: snapshot.range(r.span),
            kind: if r.write || r.declaration {
                highlight_kind::WRITE
            } else {
                highlight_kind::READ
            },
        })
        .collect()
}

/// The range and current name of a renamable symbol at `position`.
pub fn prepare_rename(
    snapshot: &Snapshot,
    position: Position,
) -> Result<(Range, String), String> {
    let reference = snapshot
        .reference_at(position)
        .ok_or("No symbol to rename at this position")?;
    match resolve::target(snapshot, reference) {
        Some(Target::Local(_)) => {
            Ok((snapshot.range(reference.span), reference.name.clone()))
        }
        Some(Target::Global(key)) => {
            if key.owner.is_none()
                && matches!(
                    key.kind,
                    SymbolKind::Attribute | SymbolKind::TupleField
                )
            {
                return Err(
                    "The owning type of this attribute is ambiguous".into()
                );
            }
            if resolve::declarations(snapshot, &key).is_empty() {
                return Err(format!(
                    "`{}` is not declared in the workspace",
                    key.name
                ));
            }
            Ok((snapshot.range(reference.span), reference.name.clone()))
        }
        Some(_) => Err("Built-ins cannot be renamed".into()),
        None => Err("No symbol to rename at this position".into()),
    }
}

/// Refuses a new name that would change what a name means in a nested or
/// enclosing scope: a declaration between a reference and the symbol would
/// capture the reference, and the symbol would capture the references of an
/// outer declaration of the same name that are made inside its own scope.
fn check_capture(
    snapshot: &Snapshot,
    id: SymbolId,
    name: &str,
) -> Result<(), String> {
    let analysis = &snapshot.analysis;
    let symbol = &analysis.symbols[id];
    let rivals = |scope: ScopeId| {
        analysis.scopes[scope]
            .symbols
            .iter()
            .copied()
            .filter(move |&other| {
                let s = &analysis.symbols[other];
                other != id
                    && s.name.trim_start_matches('@') == name
                    && s.kind.is_accumulator() == symbol.kind.is_accumulator()
                    && !matches!(
                        s.kind,
                        SymbolKind::Attribute | SymbolKind::TupleField
                    )
            })
    };
    let inside = |scope: ScopeId| {
        analysis
            .scope_chain(scope)
            .any(|s| s == symbol.scope)
    };
    let mut seen = Vec::new();
    for r in analysis.references_to(id) {
        if seen.contains(&r.scope) {
            continue;
        }
        seen.push(r.scope);
        let between = analysis
            .scope_chain(r.scope)
            .take_while(|&s| s != symbol.scope);
        if between
            .into_iter()
            .any(|scope| rivals(scope).next().is_some())
        {
            return Err(format!(
                "`{name}` is declared in a nested scope where `{}` is used",
                symbol.name
            ));
        }
    }
    let outer: Vec<SymbolId> = analysis
        .scope_chain(symbol.scope)
        .skip(1)
        .flat_map(rivals)
        .collect();
    let captures = analysis.references.iter().any(|r| {
        inside(r.scope)
            && match r.target {
                Some(target) => outer.contains(&target),
                None => {
                    !symbol.kind.is_accumulator()
                        && r.name == name
                        && matches!(
                            r.role,
                            Role::Value
                                | Role::VertexSource
                                | Role::EdgeSource
                                | Role::SchemaType
                        )
                }
            }
    });
    if captures {
        return Err(format!(
            "`{name}` is already used in this scope for another declaration"
        ));
    }
    Ok(())
}

/// Refuses names that GSQL reserves and names that another declaration of
/// the same kind already has in the same scope (or workspace).
fn check_rename_conflicts(
    snapshot: &Snapshot,
    target: &Target,
    name: &str,
) -> Result<(), String> {
    use super::rules::{is_ddl_reserved, is_query_reserved};
    match target {
        Target::Local(id) => {
            let symbol = &snapshot.analysis.symbols[*id];
            if !symbol.kind.is_accumulator() && is_query_reserved(name) {
                return Err(format!("`{name}` is a reserved word in GSQL"));
            }
            let taken = snapshot.analysis.symbols.iter().enumerate().any(|(other, s)| {
                other != *id
                    && s.scope == symbol.scope
                    && s.name.trim_start_matches('@') == name
                    && s.kind.is_accumulator() == symbol.kind.is_accumulator()
                    // Tuple fields are only reached as `t.field`: they share a
                    // namespace with the other fields of their tuple, not with
                    // variables and parameters.
                    && (s.kind == SymbolKind::TupleField) == (symbol.kind == SymbolKind::TupleField)
                    && (s.kind != SymbolKind::TupleField || s.owner == symbol.owner)
            });
            if taken {
                return Err(format!(
                    "`{name}` is already declared in this scope"
                ));
            }
            check_capture(snapshot, *id, name)?;
        }
        Target::Global(key) => {
            if is_ddl_reserved(name) || is_query_reserved(name) {
                return Err(format!("`{name}` is a reserved word in GSQL"));
            }
            let kinds = match key.kind {
                SymbolKind::VertexType | SymbolKind::EdgeType => {
                    vec![SymbolKind::VertexType, SymbolKind::EdgeType]
                }
                kind => vec![kind],
            };
            let clash = snapshot
                .workspace
                .find_any(&kinds, name)
                .into_iter()
                .any(|s| s.owner == key.owner && s.name != key.name);
            if clash {
                return Err(format!(
                    "`{name}` is already declared in the workspace"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn rename(
    snapshot: &Snapshot,
    position: Position,
    new_name: &str,
) -> Result<WorkspaceEdit, String> {
    prepare_rename(snapshot, position)?;
    let reference = snapshot
        .reference_at(position)
        .ok_or("No symbol to rename at this position")?;
    let target = resolve::target(snapshot, reference)
        .ok_or("No symbol to rename at this position")?;
    // Accumulators keep their `@` / `@@` prefix.
    let prefix: String = reference
        .name
        .chars()
        .take_while(|&c| c == '@')
        .collect();
    let bare = new_name.trim_start_matches('@');
    if !is_identifier(bare) {
        return Err(format!("`{new_name}` is not a valid GSQL identifier"));
    }
    if prefix.is_empty() && new_name.starts_with('@') {
        return Err(format!(
            "`{new_name}`: only accumulators have names starting with `@`"
        ));
    }
    let replacement = format!("{prefix}{bare}");
    check_rename_conflicts(snapshot, &target, bare)?;
    let mut changes: BTreeMap<String, Vec<TextEdit>> = BTreeMap::new();
    match target {
        Target::Local(id) => {
            for r in snapshot.analysis.references_to(id) {
                changes
                    .entry(snapshot.uri.to_string())
                    .or_default()
                    .push(snapshot.edit(r.span, replacement.clone()));
            }
        }
        Target::Global(key) => {
            let mut locations = snapshot.workspace.references(
                key.kind,
                &key.name,
                key.owner.as_deref(),
                false,
            );
            locations.extend(
                resolve::declarations(snapshot, &key)
                    .into_iter()
                    .map(|s| s.location()),
            );
            locations.sort_by(|a, b| {
                (&a.uri, a.range.start).cmp(&(&b.uri, b.range.start))
            });
            locations.dedup();
            for location in locations {
                changes
                    .entry(location.uri)
                    .or_default()
                    .push(TextEdit {
                        range: location.range,
                        new_text: replacement.clone(),
                    });
            }
        }
        _ => return Err("Built-ins cannot be renamed".into()),
    }
    Ok(WorkspaceEdit { changes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{
        Fixture, SCHEMA_URI, TYPO_BEFORE_AN_EDGE, acted_in, cursor, index,
    };

    #[test]
    fn tuple_field_names_do_not_conflict_with_variables() {
        let text = "CREATE QUERY q(STRING zz) {\n  TYPEDEF TUPLE<site STRING, alt STRING> T;\n  PRINT zz;\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let at = |needle: &str| snapshot.position(text.find(needle).unwrap());
        // Parameter to a tuple field's name, and back.
        let forward = rename(&snapshot, at("zz"), "site").unwrap();
        let edits = forward.changes.values().next().unwrap();
        assert_eq!(edits.len(), 2);
        let renamed = "CREATE QUERY q(STRING site) {\n  TYPEDEF TUPLE<site STRING, alt STRING> T;\n  PRINT site;\n}\n";
        let fixture = Fixture::new(renamed);
        let snapshot = fixture.snapshot();
        let back = rename(
            &snapshot,
            snapshot.position(renamed.find("site").unwrap()),
            "zz",
        )
        .unwrap();
        assert_eq!(back.changes.values().next().unwrap().len(), 2);
        // A tuple field to a parameter's name.
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        assert!(rename(&snapshot, at("site"), "zz").is_ok());
        // Still refused: another field of the same tuple, and another variable.
        assert!(rename(&snapshot, at("site"), "alt").is_err());
        let two = "CREATE QUERY q(STRING a, STRING b) {\n  PRINT a;\n}\n";
        let fixture = Fixture::new(two);
        let snapshot = fixture.snapshot();
        assert!(
            rename(
                &snapshot,
                snapshot.position(two.find("a,").unwrap()),
                "b"
            )
            .is_err()
        );
    }

    #[test]
    fn a_tuple_named_after_a_soft_keyword_resolves_to_its_typedef() {
        let text = "TYPEDEF TUPLE<list INT, map STRING> list\n\
                    CREATE QUERY q() FOR GRAPH G {\n  ListAccum<list> @@l;\n  list x = list(1, \"a\");\n  \
                    PRINT x.list, x.map;\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let definition_at =
            |offset: usize| definition(&snapshot, snapshot.position(offset));
        let typedef_name = text.find("> list").unwrap() + 2;
        let expected = snapshot.position(typedef_name);
        // used as the element type of an accumulator, and as the type of a declaration
        for needle in ["ListAccum<list", "  list x"] {
            let offset = text.find(needle).unwrap() + needle.len()
                - if needle.ends_with('x') { 3 } else { 0 }
                - 1;
            let found = definition_at(offset);
            assert_eq!(found.len(), 1, "{needle}");
            assert_eq!(found[0].range.start, expected, "{needle}");
        }
        // the field names still resolve to the fields
        let field = text.find("TUPLE<list").unwrap() + 6;
        let found = definition_at(text.find("x.list").unwrap() + 3);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, snapshot.position(field));
        let found = definition_at(text.find("x.map").unwrap() + 3);
        assert_eq!(
            found[0].range.start,
            snapshot.position(text.find("map STRING").unwrap())
        );
    }

    #[test]
    fn jobs_named_after_soft_keywords_resolve_to_their_definition() {
        let text = "CREATE LOADING JOB update FOR GRAPH g {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX P VALUES ($0, $1);\n}\n\
                    RUN LOADING JOB update\nDROP JOB update\nSHOW LOADING STATUS list\nLIST WORKLOAD QUEUE\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let tree = &fixture.tree;
        let kinds: Vec<_> = {
            let mut cursor = tree.walk();
            tree.root_node()
                .children(&mut cursor)
                .map(|n| n.kind())
                .collect()
        };
        assert_eq!(
            kinds,
            [
                "loading_job_definition",
                "run_job_statement",
                "drop_statement",
                "show_statement",
                "shell_command"
            ]
        );
        let found = definition(
            &snapshot,
            snapshot.position(text.find("JOB update\nDROP").unwrap() + 5),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].range.start,
            snapshot.position(text.find("update").unwrap())
        );
        let rename = rename(
            &snapshot,
            snapshot.position(text.find("update").unwrap()),
            "load_it",
        );
        assert!(rename.is_ok(), "{rename:?}");
    }

    #[test]
    fn finds_local_definitions_and_references() {
        let (text, offset) = cursor(
            "CREATE QUERY q(INT k) {\n  INT x = k;\n  PRINT x + |k;\n}\n",
        );
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let position = snapshot.position(offset);
        let definition = definition(&snapshot, position);
        assert_eq!(definition.len(), 1);
        assert_eq!(definition[0].range.start, Position::new(0, 19));
        assert_eq!(references(&snapshot, position, true).len(), 3);
        assert_eq!(references(&snapshot, position, false).len(), 2);
    }

    #[test]
    fn finds_definitions_in_statements_with_a_misspelled_keyword() {
        // A keyword typo (here also a one-letter `D` for `DO`) throws the
        // parser off, but a variable declared in the statement still resolves.
        for (opener, closer) in [("DO", "THE"), ("D", "THEN"), ("DO", "TEN")]
        {
            let body = format!(
                "CREATE QUERY q(INT n) {{\n  FOREACH i IN RANGE[0, n] {opener}\n    INT r = i;\n    IF r > 1 {closer} PRINT |r; END;\n  END;\n}}\n"
            );
            let (text, offset) = cursor(&body);
            let fixture = Fixture::new(&text);
            let snapshot = fixture.snapshot();
            let position = snapshot.position(offset);
            let found = definition(&snapshot, position);
            assert_eq!(found.len(), 1, "{opener} {closer}");
            assert_eq!(
                found[0].range.start,
                Position::new(2, 8),
                "{opener} {closer}"
            );
            assert!(
                references(&snapshot, position, true).len() >= 3,
                "{opener} {closer}"
            );
        }
    }

    #[test]
    fn update_set_does_not_lead_to_the_set_type() {
        let query = "CREATE QUERY q() FOR GRAPH G {\n  UPDATE p FROM Person:p SET p.name = \"x\";\n  SET<INT> t;\n}\n";
        let fixture = Fixture::new(query);
        let snapshot = fixture.snapshot();
        let at = |needle: &str| {
            definition(
                &snapshot,
                snapshot.position(query.find(needle).unwrap() + 1),
            )
        };
        assert!(at("SET p.name").is_empty());
        assert_eq!(
            at("SET<INT>").len(),
            1,
            "the SET type keeps its definition"
        );
    }

    #[test]
    fn built_ins_lead_to_their_documentation() {
        let query = "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  MapAccum<INT, INT> @@m;\n  @@m += (1 -> 2);\n  FOREACH i IN RANGE[0, 2] DO @@n += abs(i) + @@m.size(); END;\n}\n";
        let fixture = Fixture::new(query);
        let snapshot = fixture.snapshot();
        for (needle, heading) in [
            ("SumAccum", "BUILTIN OBJECT SumAccum<"),
            ("abs(", "BUILTIN FUNCTION abs(num) -> number;"),
            ("size(", "  METHOD size() -> INT;"),
            ("FOREACH", "BUILTIN KEYWORD FOREACH;"),
        ] {
            let offset = query.find(needle).unwrap() + 1;
            let found = definition(&snapshot, snapshot.position(offset));
            assert_eq!(found.len(), 1, "{needle}");
            assert!(found[0].uri.ends_with(".gsql"), "{needle}");
            let text = std::fs::read_to_string(
                crate::uri::to_path(&found[0].uri).unwrap(),
            )
            .unwrap();
            let line = text
                .lines()
                .nth(found[0].range.start.line as usize)
                .unwrap();
            assert!(line.starts_with(heading), "{needle}: {line}");
            if needle == "size(" {
                // The receiver is a MapAccum: its own entry, not another type's.
                let before: Vec<&str> = text
                    .lines()
                    .take(found[0].range.start.line as usize)
                    .collect();
                let owner = before
                    .iter()
                    .rev()
                    .find(|l| l.starts_with("BUILTIN OBJECT"))
                    .copied();
                assert!(
                    owner.is_some_and(
                        |l| l.starts_with("BUILTIN OBJECT MapAccum<")
                    ),
                    "{owner:?}"
                );
            }
        }
    }

    /// `TYPO_BEFORE_AN_EDGE` on the schema `acted_in(to)`, the cursor on the alias `m`.
    fn typo_before_an_edge(to: &str) -> (Fixture, Position) {
        let (text, offset) = cursor(TYPO_BEFORE_AN_EDGE);
        let fixture = Fixture::with_schema(&text, &acted_in(to));
        let position = fixture.snapshot().position(offset);
        (fixture, position)
    }

    fn hover_text(fixture: &Fixture, position: Position) -> String {
        let hover =
            crate::features::hover::hover(&fixture.snapshot(), position);
        hover.expect("hover").contents.value
    }

    #[test]
    fn a_keyword_typo_keeps_the_edge_end_type_of_an_alias() {
        // `THN` throws the parser off, so `m` is looked up in the corrected document.
        let (fixture, position) = typo_before_an_edge("Movie");
        assert!(definition_here(&fixture.snapshot(), position).is_empty());
        let hover = hover_text(&fixture, position);
        assert!(hover.contains("m: VERTEX<Movie>"), "{hover}");
    }

    #[test]
    fn the_corrected_document_is_analyzed_once_for_a_schema() {
        let (fixture, _) = typo_before_an_edge("Movie");
        let snapshot = fixture.snapshot();
        let repair = crate::features::autocorrect::repair_of(&snapshot)
            .expect("repair");
        assert!(repair.analysis.uses_schema_edges);
        let first = repair.analysis(snapshot.workspace);
        let again = repair.analysis(snapshot.workspace);
        assert!(std::ptr::eq(&*first, &*again), "analyzed again");
    }

    #[test]
    fn the_corrected_document_follows_the_schema() {
        for to in ["Movie", "Show"] {
            let (fixture, position) = typo_before_an_edge(to);
            let hover = hover_text(&fixture, position);
            assert!(hover.contains(&format!("m: VERTEX<{to}>")), "{hover}");
        }
        // The schema edited in the same workspace.
        let (mut fixture, position) = typo_before_an_edge("Movie");
        assert!(hover_text(&fixture, position).contains("m: VERTEX<Movie>"));
        let Fixture {
            tree,
            source,
            analysis,
            workspace,
            ..
        } = &mut fixture;
        workspace.update(index(SCHEMA_URI, &acted_in("Show")));
        analysis.schema_changed(tree, &source.text, workspace);
        let hover = hover_text(&fixture, position);
        assert!(hover.contains("m: VERTEX<Show>"), "{hover}");
    }

    #[test]
    fn jumps_from_aliases_to_their_vertex_type() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\n";
        let (text, offset) = cursor(
            "CREATE QUERY q() {\n  S = {Person.*};\n  R = SELECT t FROM S:t WHERE |t.id == \"a\";\n  PRINT R;\n}\n",
        );
        let fixture = Fixture::with_schema(&text, schema);
        let snapshot = fixture.snapshot();
        let locations = type_definition(&snapshot, snapshot.position(offset));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].uri, SCHEMA_URI);
        assert_eq!(locations[0].range.start, Position::new(0, 14));
    }

    #[test]
    fn renames_accumulators_keeping_the_prefix() {
        let (text, offset) = cursor(
            "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  @@|n += 1;\n  PRINT @@n;\n}\n",
        );
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let edit =
            rename(&snapshot, snapshot.position(offset), "count").unwrap();
        let edits = &edit.changes[&fixture.uri];
        assert_eq!(edits.len(), 3);
        assert!(edits.iter().all(|e| e.new_text == "@@count"));
        assert!(
            rename(&snapshot, snapshot.position(offset), "1bad").is_err()
        );
    }

    #[test]
    fn renames_only_the_variable_of_its_own_block() {
        let (text, offset) = cursor(
            "CREATE QUERY q(BOOL b) {\n  IF b THEN\n    INT x = 1;\n    PRINT x;\n  ELSE\n    INT |x = 2;\n    PRINT x;\n  END;\n}\n",
        );
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let edit = rename(&snapshot, snapshot.position(offset), "y").unwrap();
        let lines: Vec<u32> = edit.changes[&fixture.uri]
            .iter()
            .map(|e| e.range.start.line)
            .collect();
        assert_eq!(lines, [5, 6]);
    }

    /// Renames the `occurrence`-th `needle` of `text` to `new`.
    fn rename_at(
        text: &str,
        needle: &str,
        occurrence: usize,
        new: &str,
    ) -> Result<WorkspaceEdit, String> {
        let schema =
            "CREATE VERTEX SPV (PRIMARY_ID id STRING, site STRING)\n";
        let fixture = Fixture::with_schema(text, schema);
        let snapshot = fixture.snapshot();
        let offset = text
            .match_indices(needle)
            .nth(occurrence)
            .unwrap()
            .0;
        rename(&snapshot, snapshot.position(offset), new)
    }

    const CAPTURE: &str = "CREATE QUERY m(STRING s) FOR GRAPH G {\n  INT x = 0;\n  Start = {SPV.*};\n  R = SELECT t FROM Start:t WHERE t.site == s ACCUM x += 1;\n  FOREACH i IN RANGE[0, 2] DO x = x + i; END;\n  PRINT R, x;\n}\n";

    #[test]
    fn refuses_names_that_capture_or_are_captured_in_nested_scopes() {
        // The alias takes the name of a parameter or variable used inside its block.
        assert!(rename_at(CAPTURE, "t FROM", 0, "s").is_err());
        assert!(rename_at(CAPTURE, "t FROM", 0, "x").is_err());
        // A variable or parameter takes the name of an alias or loop variable
        // that is in scope where it is used.
        assert!(rename_at(CAPTURE, "x = 0", 0, "t").is_err());
        assert!(rename_at(CAPTURE, "x = 0", 0, "i").is_err());
        assert!(rename_at(CAPTURE, "s)", 0, "t").is_err());
        assert!(rename_at(CAPTURE, "s)", 0, "i").is_ok());
        // The loop variable takes the name of a variable used in the loop.
        assert!(rename_at(CAPTURE, "i IN", 0, "x").is_err());
        assert!(rename_at(CAPTURE, "i IN", 0, "s").is_ok());
    }

    #[test]
    fn allows_renames_that_change_no_meaning() {
        assert!(rename_at(CAPTURE, "t FROM", 0, "u").is_ok());
        assert!(rename_at(CAPTURE, "x = 0", 0, "total").is_ok());
        assert!(rename_at(CAPTURE, "s)", 0, "site_name").is_ok());
        assert!(rename_at(CAPTURE, "i IN", 0, "k").is_ok());
        // An alias may take a name that nothing in its block uses: `i` is a
        // loop variable of another block, `x` is not used in this one.
        let text = "CREATE QUERY m() FOR GRAPH G {\n  INT x = 0;\n  Start = {SPV.*};\n  R = SELECT t FROM Start:t;\n  PRINT R;\n}\n";
        assert!(rename_at(text, "t FROM", 0, "x").is_ok());
        // Sibling blocks do not see each other.
        let text = "CREATE QUERY m() FOR GRAPH G {\n  FOREACH i IN RANGE[0, 2] DO PRINT i; END;\n  FOREACH j IN RANGE[0, 2] DO PRINT j; END;\n}\n";
        assert!(rename_at(text, "i IN", 0, "j").is_ok());
    }

    #[test]
    fn keeps_accumulators_apart_from_values() {
        let text = "CREATE QUERY m() FOR GRAPH G {\n  SumAccum<INT> @@n;\n  Start = {SPV.*};\n  R = SELECT t FROM Start:t ACCUM @@n += 1;\n  PRINT R;\n}\n";
        // An accumulator has its own namespace: no clash with the alias.
        assert!(rename_at(text, "@@n;", 0, "t").is_ok());
    }

    #[test]
    fn refuses_conflicting_and_reserved_names() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\nCREATE VERTEX Human (PRIMARY_ID id STRING)\n";
        let (text, offset) = cursor(
            "CREATE QUERY q(INT k) {\n  INT |x = 1;\n  INT y = 2;\n  SumAccum<INT> @@n;\n  PRINT x, y, k, @@n;\n}\n",
        );
        let fixture = Fixture::with_schema(&text, schema);
        let snapshot = fixture.snapshot();
        let at = snapshot.position(offset);
        for bad in ["y", "k", "SELECT", "int", "@x"] {
            assert!(rename(&snapshot, at, bad).is_err(), "{bad}");
        }
        // Accumulators and variables have separate namespaces.
        assert!(rename(&snapshot, at, "n").is_ok());
        let (text, offset) =
            cursor("CREATE QUERY q() { S = {Pe|rson.*}; PRINT S; }\n");
        let fixture = Fixture::with_schema(&text, schema);
        let snapshot = fixture.snapshot();
        assert!(
            rename(&snapshot, snapshot.position(offset), "Human").is_err()
        );
        assert!(
            rename(&snapshot, snapshot.position(offset), "Order").is_err()
        );
        assert!(
            rename(&snapshot, snapshot.position(offset), "Citizen").is_ok()
        );
    }

    #[test]
    fn renames_vertex_types_across_files() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING)\n";
        let (text, offset) = cursor(
            "CREATE QUERY q() { S = {Pe|rson.*}; R = SELECT s FROM Person:s; PRINT R; }\n",
        );
        let fixture = Fixture::with_schema(&text, schema);
        let snapshot = fixture.snapshot();
        let edit =
            rename(&snapshot, snapshot.position(offset), "Human").unwrap();
        assert_eq!(edit.changes["file:///test/main.gsql"].len(), 2);
        assert_eq!(edit.changes[SCHEMA_URI].len(), 1);
    }

    const JOBS: &str = "CREATE LOADING JOB lj FOR GRAPH G {\n  DEFINE FILENAME f1;\n  LOAD f1 TO VERTEX SPV VALUES ($0, $1);\n}\nCREATE LOADING JOB lj2 FOR GRAPH G {\n  DEFINE FILENAME f1 = \"x.csv\";\n  LOAD f1 TO VERTEX SPV VALUES ($0, $1);\n}\n";

    fn lines(locations: &[Location]) -> Vec<(String, u32)> {
        locations
            .iter()
            .map(|l| (crate::uri::file_name(&l.uri), l.range.start.line))
            .collect()
    }

    #[test]
    fn run_using_names_the_file_variable_of_its_job() {
        let (text, offset) = cursor(
            "RUN LOADING JOB lj2 USING |f1=\"a.csv\", EOF=\"true\"\nRUN LOADING JOB lj USING f1=\"b.csv\"\n",
        );
        let fixture =
            Fixture::with_files(&text, &[("file:///test/jobs.gsql", JOBS)]);
        let snapshot = fixture.snapshot();
        let at = snapshot.position(offset);
        // The job that is run decides, not the first job defining `f1`.
        assert_eq!(
            lines(&definition(&snapshot, at)),
            [("jobs.gsql".to_string(), 5)]
        );
        let found = lines(&references(&snapshot, at, true));
        assert_eq!(
            found,
            [
                ("jobs.gsql".to_string(), 5),
                ("jobs.gsql".to_string(), 6),
                ("main.gsql".to_string(), 0)
            ]
        );
        let found = lines(&references(&snapshot, at, false));
        assert_eq!(
            found,
            [("jobs.gsql".to_string(), 6), ("main.gsql".to_string(), 0)]
        );
        let hover = crate::features::hover::hover(&snapshot, at).unwrap();
        assert!(
            format!("{hover:?}").contains("DEFINE FILENAME f1"),
            "{hover:?}"
        );
        // Another option of the same clause is no file variable.
        let eof = snapshot.position(text.find("EOF").unwrap());
        assert!(definition(&snapshot, eof).is_empty());
        assert!(rename(&snapshot, eof, "x").is_err());
        let edit = rename(&snapshot, at, "data").unwrap();
        assert_eq!(edit.changes.len(), 2);
        assert_eq!(edit.changes["file:///test/main.gsql"].len(), 1);
        assert_eq!(
            edit.changes["file:///test/main.gsql"][0]
                .range
                .start
                .line,
            0
        );
        let in_jobs: Vec<u32> = edit.changes["file:///test/jobs.gsql"]
            .iter()
            .map(|e| e.range.start.line)
            .collect();
        assert_eq!(in_jobs, [5, 6]);
        assert_eq!(document_highlight(&snapshot, at).len(), 1);
    }

    #[test]
    fn file_variable_references_reach_run_statements_in_other_files() {
        let (text, offset) = cursor(
            "CREATE LOADING JOB lj FOR GRAPH G {\n  DEFINE FILENAME |f1;\n  LOAD f1 TO VERTEX SPV VALUES ($0, $1);\n}\n",
        );
        let run = "RUN LOADING JOB lj USING f1=\"a\"\nRUN LOADING JOB lj2 USING f1=\"b\"\nRUN LOADING JOB lj USING f1=\"c\"\n";
        let fixture =
            Fixture::with_files(&text, &[("file:///test/run.gsql", run)]);
        let snapshot = fixture.snapshot();
        let at = snapshot.position(offset);
        let found = lines(&references(&snapshot, at, true));
        assert_eq!(
            found,
            [
                ("main.gsql".to_string(), 1),
                ("main.gsql".to_string(), 2),
                ("run.gsql".to_string(), 0),
                ("run.gsql".to_string(), 2)
            ]
        );
        let edit = rename(&snapshot, at, "data").unwrap();
        assert_eq!(edit.changes["file:///test/run.gsql"].len(), 2);
        assert_eq!(edit.changes["file:///test/main.gsql"].len(), 2);
        // A name taken by another file variable of the job is refused.
        let two = "CREATE LOADING JOB lj FOR GRAPH G {\n  DEFINE FILENAME |f1;\n  DEFINE FILENAME f2;\n}\n";
        let (text, offset) = cursor(two);
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        assert!(rename(&snapshot, snapshot.position(offset), "f2").is_err());
        assert!(rename(&snapshot, snapshot.position(offset), "f3").is_ok());
    }

    #[test]
    fn job_names_rename_in_run_drop_and_show() {
        let (text, offset) = cursor(
            "CREATE LOADING JOB |lj FOR GRAPH G {\n  DEFINE FILENAME f1;\n}\n",
        );
        let others = "RUN LOADING JOB lj USING f1=\"a\"\nSHOW JOB lj\nSHOW JOB ALL\nDROP JOB lj\nRUN LOADING JOB lj2\n";
        let fixture =
            Fixture::with_files(&text, &[("file:///test/run.gsql", others)]);
        let snapshot = fixture.snapshot();
        let edit =
            rename(&snapshot, snapshot.position(offset), "load_people")
                .unwrap();
        let in_run: Vec<u32> = edit.changes["file:///test/run.gsql"]
            .iter()
            .map(|e| e.range.start.line)
            .collect();
        assert_eq!(in_run, [0, 1, 3]);
        assert_eq!(edit.changes["file:///test/main.gsql"].len(), 1);
    }

    const TEMP: &str = "CREATE LOADING JOB lj FOR GRAPH G {\n  DEFINE FILENAME f1;\n  LOAD f1 TO TEMP_TABLE t (a, b) VALUES ($0, $1);\n  LOAD TEMP_TABLE t TO VERTEX SPV VALUES ($\"a\", $\"b\", $\"zz\", $1);\n}\n";

    #[test]
    fn temp_table_columns_resolve_from_their_references() {
        let fixture = Fixture::new(TEMP);
        let snapshot = fixture.snapshot();
        let at = |needle: &str, extra: usize| {
            let offset = TEMP.find(needle).unwrap() + extra;
            snapshot.position(offset)
        };
        let column = at("$\"a\"", 2);
        let found = definition(&snapshot, column);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, Position::new(2, 27));
        assert_eq!(references(&snapshot, column, true).len(), 2);
        let hover = crate::features::hover::hover(&snapshot, column).unwrap();
        assert!(
            format!("{hover:?}").contains("column of TEMP_TABLE t"),
            "{hover:?}"
        );
        // Not a column of the table, or not a name.
        assert!(definition(&snapshot, at("zz", 0)).is_empty());
        assert!(definition(&snapshot, at("$1);", 1)).is_empty());
        // The temp table itself, from its use.
        let table = at("TEMP_TABLE t TO", 11);
        assert_eq!(
            definition(&snapshot, table)[0].range.start,
            Position::new(2, 24)
        );
        assert_eq!(references(&snapshot, table, true).len(), 2);
        // Renaming the column updates the declaration and the `$"name"`.
        let edit = rename(&snapshot, column, "c").unwrap();
        let edits = &edit.changes["file:///test/main.gsql"];
        assert_eq!(edits.len(), 2);
        let renamed = crate::text::SourceText::new(TEMP.to_string())
            .apply_edits(edits, crate::text::PositionEncoding::Utf16);
        assert!(
            renamed.contains("(c, b)") && renamed.contains("$\"c\", $\"b\""),
            "{renamed}"
        );
    }

    #[test]
    fn temp_table_columns_stay_quiet_elsewhere() {
        // `$"name"` reading a file with a header, or a column of another table.
        let text = "CREATE LOADING JOB lj FOR GRAPH G {\n  DEFINE FILENAME f1;\n  LOAD f1 TO TEMP_TABLE t (a) VALUES ($0);\n  LOAD f1 TO VERTEX SPV VALUES ($\"a\", $\"b\":\"c\") USING HEADER=\"true\";\n  LOAD TEMP_TABLE t TO VERTEX SPV VALUES ($\"b\");\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let columns: Vec<(&str, bool)> = (snapshot
            .analysis
            .references
            .iter())
        .filter(|r| matches!(r.role, Role::TempColumn(_)) && !r.declaration)
        .map(|r| (r.name.as_str(), r.target.is_some()))
        .collect();
        // Only the read of the temp table counts, and `b` is no column of `t`.
        assert_eq!(columns, [("b", false)]);
    }

    #[test]
    fn show_query_names_the_query() {
        let (text, offset) = cursor(
            "CREATE QUERY |h() {}\nINSTALL QUERY h\nSHOW QUERY h\nSHOW QUERY ALL\nSHOW VERTEX h\n",
        );
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let show = text.find("SHOW QUERY h").unwrap() + 11;
        let found = definition(&snapshot, snapshot.position(show));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, snapshot.position(offset));
        assert_eq!(
            references(&snapshot, snapshot.position(show), true).len(),
            3
        );
        let edit = rename(&snapshot, snapshot.position(show), "g").unwrap();
        assert_eq!(edit.changes["file:///test/main.gsql"].len(), 3);
        // Other SHOW commands keep their names out of the query namespace.
        let vertex = text.find("SHOW VERTEX h").unwrap() + 12;
        assert!(definition(&snapshot, snapshot.position(vertex)).is_empty());
    }

    #[test]
    fn the_log_statement_does_not_resolve_to_the_log_function() {
        let text = "CREATE QUERY q() FOR GRAPH G {\n  LOG(TRUE, \"x\");\n  LOG(1 > 0, \"y\");\n  DOUBLE d = LOG(2.0);\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        for (needle, expected) in
            [("LOG(TRUE", false), ("LOG(1", false), ("LOG(2.0", true)]
        {
            let position = snapshot.position(text.find(needle).unwrap() + 1);
            assert_eq!(
                definition(&snapshot, position).len(),
                usize::from(expected),
                "{needle}"
            );
            assert_eq!(
                crate::features::hover::hover(&snapshot, position).is_some(),
                expected,
                "{needle}"
            );
        }
    }

    #[test]
    fn highlights_reads_and_writes() {
        let (text, offset) =
            cursor("CREATE QUERY q() {\n  INT |x = 1;\n  x = x + 1;\n}\n");
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let highlights =
            document_highlight(&snapshot, snapshot.position(offset));
        let kinds: Vec<u8> = highlights.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            vec![
                highlight_kind::WRITE,
                highlight_kind::WRITE,
                highlight_kind::READ
            ]
        );
    }
}
