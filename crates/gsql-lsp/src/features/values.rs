//! What each value of a VALUES list fills, from the schema in the workspace.
//! Used for count checks on LOAD and INSERT statements, inlay hints that
//! name the attribute of each value, and hover on values.

use tree_sitter::Node;

use crate::analysis::SymbolKind;
use crate::features::Snapshot;
use crate::features::diagnostics::diagnostic;
use crate::lsp::types::{Diagnostic, Range, severity};
use crate::syntax;
use crate::text::Span;

/// One position of a VALUES list.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    /// Attribute or column name, or `FROM`/`TO` for edge endpoints.
    pub name: String,
    /// Shown on hover, e.g. `Person.age INT`.
    pub detail: String,
}

/// Where the slots of a VALUES list come from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    /// The attributes of a vertex or edge type (may lag behind the database).
    Schema,
    /// A column list written in the statement (`TO TEMP_TABLE t (a, b)`, `INSERT INTO v (PRIMARY_ID, a)`).
    Columns,
}

#[derive(Debug)]
pub struct ValueList<'t> {
    pub list: Node<'t>,
    /// The values, without comments.
    pub values: Vec<Node<'t>>,
    pub slots: Vec<Slot>,
    pub source: Source,
    /// How the target reads in messages, e.g. "`Person`" or "TEMP_TABLE `t`".
    pub target: String,
    /// A LOAD statement (as opposed to INSERT).
    pub load: bool,
    /// An edge whose endpoints are declared `FROM *, TO *`.
    pub any_endpoint: bool,
    /// The first slot is the primary id of a vertex type.
    pub primary_id_first: bool,
}

/// Every VALUES list of the file whose slots are known.
pub fn value_lists<'t>(snapshot: &Snapshot<'t>) -> Vec<ValueList<'t>> {
    let mut lists = Vec::new();
    syntax::walk(snapshot.tree.root_node(), |node| {
        let list = match node.kind() {
            "load_destination" => load_destination(snapshot, node),
            "insert_statement" => insert(snapshot, node),
            _ => None,
        };
        lists.extend(list);
    });
    lists
}

fn values_of(list: Node) -> Vec<Node> {
    syntax::named_children(list).into_iter().filter(|v| v.kind() != "comment").collect()
}

fn load_destination<'t>(snapshot: &Snapshot<'t>, node: Node<'t>) -> Option<ValueList<'t>> {
    let source = snapshot.text();
    let list = node.child_by_field_name("values")?;
    let target = syntax::field_text(node, "target", source)?;
    let kind = node.child_by_field_name("kind")?.kind();
    let make = |slots: Vec<Slot>, source: Source, target: String, schema: Option<&Schema>| ValueList {
        list,
        values: values_of(list),
        slots,
        source,
        target,
        load: true,
        any_endpoint: schema.is_some_and(|s| s.any_endpoint),
        primary_id_first: schema.is_some_and(|s| s.primary_id_first),
    };
    match kind {
        "TEMP_TABLE" => {
            let columns = node.child_by_field_name("columns")?;
            let slots = syntax::named_children(columns)
                .into_iter()
                .filter(|c| c.kind() == "identifier")
                .map(|c| {
                    let name = syntax::text(c, source).to_string();
                    Slot { detail: format!("column {name} of TEMP_TABLE {target}"), name }
                })
                .collect();
            Some(make(slots, Source::Columns, format!("TEMP_TABLE `{target}`"), None))
        }
        "VERTEX" | "EDGE" => {
            let schema = schema(snapshot, target)?;
            if schema.edge != (kind == "EDGE") {
                return None;
            }
            let slots = schema.slots();
            Some(make(slots, Source::Schema, format!("`{target}`"), Some(&schema)))
        }
        "VECTOR" => {
            let attribute = syntax::field_text(node, "attribute", source)?;
            let slots = vec![
                Slot { name: "id".into(), detail: format!("primary id of {target}") },
                Slot { name: attribute.into(), detail: format!("vector attribute {target}.{attribute}") },
            ];
            Some(make(slots, Source::Columns, format!("vector attribute `{attribute}`"), None))
        }
        _ => None,
    }
}

fn insert<'t>(snapshot: &Snapshot<'t>, node: Node<'t>) -> Option<ValueList<'t>> {
    let source = snapshot.text();
    let list = node.child_by_field_name("values")?;
    let target_node = node.child_by_field_name("target")?;
    // `INSERT INTO EDGE type_param` names the type at run time.
    if node.child_by_field_name("kind").is_some() {
        return None;
    }
    let target = syntax::text(target_node, source);
    let make = |slots: Vec<Slot>, source: Source, schema: Option<&Schema>| ValueList {
        list,
        values: values_of(list),
        slots,
        source,
        target: format!("`{target}`"),
        load: false,
        any_endpoint: schema.is_some_and(|s| s.any_endpoint),
        primary_id_first: schema.is_some_and(|s| s.primary_id_first),
    };
    match node.child_by_field_name("columns") {
        Some(columns) => {
            let mut slots = Vec::new();
            syntax::walk(columns, |c| {
                let name = match c.kind() {
                    "PRIMARY_ID" | "FROM" | "TO" => c.kind().to_string(),
                    "identifier" => syntax::text(c, source).to_string(),
                    _ => return,
                };
                slots.push(Slot { detail: format!("{target} column {name}"), name });
            });
            Some(make(slots, Source::Columns, None))
        }
        None => {
            let schema = schema(snapshot, target)?;
            Some(make(schema.slots(), Source::Schema, Some(&schema)))
        }
    }
}

/// The ordered attributes of a vertex or edge type.
struct Schema {
    name: String,
    edge: bool,
    /// (name, detail) in declaration order.
    attributes: Vec<(String, String)>,
    any_endpoint: bool,
    primary_id_first: bool,
}

impl Schema {
    fn slots(&self) -> Vec<Slot> {
        let endpoints = if self.edge {
            vec![
                Slot { name: "FROM".into(), detail: format!("source vertex of {}", self.name) },
                Slot { name: "TO".into(), detail: format!("target vertex of {}", self.name) },
            ]
        } else {
            Vec::new()
        };
        endpoints
            .into_iter()
            .chain(
                self.attributes
                    .iter()
                    .map(|(name, detail)| Slot { name: name.clone(), detail: format!("{}.{detail}", self.name) }),
            )
            .collect()
    }
}

fn contains(outer: Range, inner: Range) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// The attributes of `name` in declaration order, when they are known: the
/// type is declared once, and no ALTER statement elsewhere adds attributes
/// (which would make the order in the database uncertain).
fn schema(snapshot: &Snapshot, name: &str) -> Option<Schema> {
    // Types declared inside a query of this file (virtual edges).
    let analysis = snapshot.analysis;
    let local: Vec<_> = analysis
        .symbols
        .iter()
        .filter(|s| s.scope != 0 && s.name == name && matches!(s.kind, SymbolKind::VertexType | SymbolKind::EdgeType))
        .collect();
    if let [ty] = local.as_slice() {
        let mut attributes: Vec<_> = analysis
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Attribute && !s.vector && s.owner.as_deref() == Some(name))
            .collect();
        if attributes.iter().any(|a| !ty.span.contains(a.span.start)) {
            return None;
        }
        attributes.sort_by_key(|a| a.span.start);
        return Some(Schema {
            name: name.to_string(),
            edge: ty.kind == SymbolKind::EdgeType,
            attributes: attributes.iter().map(|a| (a.name.clone(), a.detail.clone())).collect(),
            any_endpoint: ty.members == ["*"],
            primary_id_first: false,
        });
    }
    let workspace = snapshot.workspace;
    let types = workspace.find_any(&[SymbolKind::VertexType, SymbolKind::EdgeType], name);
    let [ty] = types.as_slice() else {
        return None;
    };
    let mut attributes = workspace.attributes(Some(name));
    attributes.retain(|a| !a.vector);
    if attributes.iter().any(|a| a.uri != ty.uri || !contains(ty.range, a.range)) {
        return None;
    }
    attributes.sort_by_key(|a| a.range.start);
    let primary_id_first = attributes.first().is_some_and(|a| {
        let detail = a.detail.to_ascii_uppercase();
        detail.starts_with("PRIMARY_ID") || detail.contains("PRIMARY KEY")
    });
    Some(Schema {
        name: name.to_string(),
        edge: ty.kind == SymbolKind::EdgeType,
        attributes: attributes.iter().map(|a| (a.name.clone(), a.detail.clone())).collect(),
        any_endpoint: ty.members == ["*"],
        primary_id_first: ty.kind == SymbolKind::VertexType && primary_id_first,
    })
}

/// How many columns a value fills: `flatten(column, separator, n)` fills
/// `n` and `flatten_json_array(column, field, ...)` one per field. `None`
/// when that is not known.
fn width(value: Node, source: &str) -> Option<usize> {
    let function = value
        .child_by_field_name("function")
        .filter(|f| value.kind() == "call_expression" && f.kind() == "identifier")
        .map(|f| syntax::text(f, source).to_ascii_lowercase());
    let arguments: Vec<Node> = value
        .child_by_field_name("arguments")
        .map(|a| syntax::named_children(a).into_iter().filter(|n| n.kind() != "comment").collect())
        .unwrap_or_default();
    match function.as_deref() {
        Some("flatten") => {
            let last = arguments.last().filter(|a| a.kind() == "integer")?;
            syntax::text(*last, source).parse().ok()
        }
        Some("flatten_json_array") => Some(arguments.len().saturating_sub(1).max(1)),
        _ => Some(1),
    }
}

/// Count checks and the documented LOAD rules.
pub fn check(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let source = snapshot.text();
    for list in value_lists(snapshot) {
        if list.list.has_error() {
            continue;
        }
        let Some(given) = list.values.iter().map(|v| width(*v, source)).sum::<Option<usize>>() else {
            continue;
        };
        let expected = list.slots.len();
        if given != expected {
            let names: Vec<&str> = list.slots.iter().map(|s| s.name.as_str()).collect();
            let message = match list.source {
                Source::Schema => format!(
                    "{} takes {expected} value{} ({}), but {given} {} given",
                    list.target,
                    plural(expected),
                    names.join(", "),
                    if given == 1 { "is" } else { "are" },
                ),
                Source::Columns => format!(
                    "{} has {expected} column{}, but {given} value{} given",
                    list.target,
                    plural(expected),
                    if given == 1 { " is" } else { "s are" },
                ),
            };
            let level = if list.source == Source::Columns { severity::ERROR } else { severity::WARNING };
            out.push(diagnostic(snapshot, Span::of(list.list), level, "value-count", message));
            continue;
        }
        if !list.load {
            continue;
        }
        // "You can not skip the primary key attributes for vertices."
        if list.primary_id_first && list.values.first().is_some_and(|v| v.kind() == "wildcard") {
            out.push(diagnostic(
                snapshot,
                Span::of(list.values[0]),
                severity::ERROR,
                "value-skip",
                "The primary id cannot be skipped with `_`".into(),
            ));
        }
        // Edges declared FROM *, TO * need the vertex type of each endpoint.
        if list.any_endpoint {
            for value in list.values.iter().take(2).filter(|v| v.kind() != "typed_value") {
                out.push(diagnostic(
                    snapshot,
                    Span::of(*value),
                    severity::WARNING,
                    "endpoint-type",
                    format!(
                        "{} connects any vertex types, so give the vertex type after the id, e.g. `{} Person`",
                        list.target,
                        syntax::text(*value, snapshot.text())
                    ),
                ));
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The slot that the value containing `offset` fills.
pub fn slot_at<'s>(lists: &'s [ValueList], offset: usize) -> Option<(&'s ValueList<'s>, &'s Slot)> {
    lists.iter().find_map(|list| {
        let index = list.values.iter().position(|v| Span::of(*v).contains(offset))?;
        Some((list, list.slots.get(index)?))
    })
}

/// Whether a value already spells out its slot, like `$"name"` for `name`.
pub fn names_its_slot(value: Node, slot: &Slot, source: &str) -> bool {
    let text = syntax::text(value, source);
    let bare = text.trim_start_matches('$').trim_matches('"');
    bare.eq_ignore_ascii_case(&slot.name)
}

#[cfg(test)]
mod tests {
    use crate::features::diagnostics::diagnostics;
    use crate::features::test_support::Fixture;

    const SCHEMA: &str = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE DIRECTED EDGE Knows (FROM Person, TO Person, since DATETIME)\nCREATE UNDIRECTED EDGE Purchase (FROM *, TO *)\n";

    fn messages(text: &str) -> Vec<String> {
        let fixture = Fixture::with_files(text, &[("file:///test/schema.gsql", SCHEMA)]);
        diagnostics(&fixture.snapshot())
            .into_iter()
            .filter(|d| matches!(d.code.as_deref(), Some("value-count" | "value-skip" | "endpoint-type")))
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn checks_value_counts_against_the_schema() {
        let found = messages(
            "CREATE LOADING JOB j FOR GRAPH g {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX Person VALUES ($0, $1),\n         TO EDGE Knows VALUES ($0, $1, $2),\n         TO TEMP_TABLE t (a, b) VALUES ($0);\n}\n",
        );
        assert_eq!(
            found,
            [
                "`Person` takes 3 values (id, name, age), but 2 are given",
                "TEMP_TABLE `t` has 2 columns, but 1 value is given",
            ]
        );
    }

    #[test]
    fn counts_the_columns_that_flatten_fills() {
        let found = messages(
            "CREATE LOADING JOB j FOR GRAPH g {\n  LOAD \"a.json\" TO TEMP_TABLE t (id, name, size) VALUES ($0, flatten_json_array($\"items\", $\"name\", $\"size\"));\n  LOAD \"b.csv\" TO TEMP_TABLE u (id, a, b) VALUES ($0, flatten($1, \"|\", \":\", 2));\n  LOAD \"c.csv\" TO TEMP_TABLE w (id, a) VALUES ($0, flatten($1, \"|\", \":\", 2));\n}\n",
        );
        assert_eq!(found, ["TEMP_TABLE `w` has 2 columns, but 3 values are given"]);
    }

    #[test]
    fn checks_insert_values() {
        let found = messages(
            "CREATE QUERY q() {\n  INSERT INTO Person VALUES (\"p\", \"x\", 1, 2);\n  INSERT INTO Person (PRIMARY_ID, name) VALUES (\"p\", \"x\");\n  INSERT INTO Knows (FROM, TO) VALUES (\"a\", \"b\", now());\n}\n",
        );
        assert_eq!(
            found,
            [
                "`Person` takes 3 values (id, name, age), but 4 are given",
                "`Knows` has 2 columns, but 3 values are given",
            ]
        );
    }

    #[test]
    fn checks_documented_load_rules() {
        let found = messages(
            "CREATE LOADING JOB j FOR GRAPH g {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX Person VALUES (_, $1, $2),\n         TO EDGE Purchase VALUES ($0 Person, $1);\n}\n",
        );
        assert_eq!(
            found,
            [
                "The primary id cannot be skipped with `_`",
                "`Purchase` connects any vertex types, so give the vertex type after the id, e.g. `$1 Person`",
            ]
        );
    }

    #[test]
    fn skips_types_changed_elsewhere() {
        let alter = "CREATE GLOBAL SCHEMA_CHANGE JOB j { ALTER VERTEX Person ADD ATTRIBUTE (city STRING); }\n";
        let fixture = Fixture::with_files(
            "CREATE LOADING JOB l FOR GRAPH g {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX Person VALUES ($0, $1, $2, $3);\n}\n",
            &[("file:///test/schema.gsql", SCHEMA), ("file:///test/alter.gsql", alter)],
        );
        let found: Vec<String> = diagnostics(&fixture.snapshot()).into_iter().map(|d| d.message).collect();
        assert!(!found.iter().any(|m| m.contains("takes")), "{found:?}");
    }
}
