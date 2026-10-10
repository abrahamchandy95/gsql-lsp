//! Resolves an identifier occurrence to what it denotes: a symbol in this
//! file, a workspace-level declaration, or a built-in.

use tree_sitter::Node;

use crate::analysis::{Param, Reference, Role, SymbolId, SymbolKind, Ty};
use crate::builtins::{self, Function, Method};
use crate::features::Snapshot;
use crate::syntax;
use crate::workspace::GlobalSymbol;

/// Identity of a workspace-level declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalKey {
    pub kind: SymbolKind,
    pub name: String,
    /// Owning type of an attribute or tuple field; `None` when unknown.
    pub owner: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Target {
    /// A symbol scoped to a query or job in this file.
    Local(SymbolId),
    Global(GlobalKey),
    Function(&'static Function),
    Method(&'static Method),
    Constant(&'static str, &'static str),
}

impl Target {
    pub fn same_as(&self, other: &Target) -> bool {
        match (self, other) {
            (Target::Local(a), Target::Local(b)) => a == b,
            (Target::Global(a), Target::Global(b)) => {
                a.kind == b.kind
                    && a.name == b.name
                    && (a.owner.is_none()
                        || b.owner.is_none()
                        || a.owner == b.owner)
            }
            (Target::Function(a), Target::Function(b)) => {
                std::ptr::eq(*a, *b)
            }
            (Target::Method(a), Target::Method(b)) => std::ptr::eq(*a, *b),
            (Target::Constant(a, _), Target::Constant(b, _)) => a == b,
            _ => false,
        }
    }
}

/// The methods available on a value of type `ty`.
pub fn methods_for(ty: &Ty) -> &'static [Method] {
    match ty {
        Ty::Vertex(_) => builtins::VERTEX_METHODS,
        Ty::Edge(_) => builtins::EDGE_METHODS,
        Ty::VertexSet(_) => builtins::VERTEX_SET_METHODS,
        Ty::File => builtins::FILE_METHODS,
        Ty::Collection(name, _) => match name.to_ascii_uppercase().as_str() {
            "LIST" => builtins::LIST_METHODS,
            "SET" | "BAG" => builtins::SET_METHODS,
            "MAP" => builtins::MAP_METHODS,
            _ => &[],
        },
        Ty::Accumulator(kind, _) => builtins::accumulator(kind)
            .map(|a| a.methods)
            .unwrap_or(&[]),
        Ty::Primitive(name) if name == "JSONOBJECT" => {
            builtins::JSON_OBJECT_METHODS
        }
        Ty::Primitive(name) if name == "JSONARRAY" => {
            builtins::JSON_ARRAY_METHODS
        }
        _ => &[],
    }
}

pub fn find_method_anywhere(name: &str) -> Option<&'static Method> {
    builtins::METHOD_GROUPS
        .iter()
        .map(|&(_, _, methods)| methods)
        .chain(
            builtins::ACCUMULATORS
                .iter()
                .map(|a| a.methods),
        )
        .find_map(|methods| builtins::find_method(methods, name))
}

fn global(kind: SymbolKind, name: &str, owner: Option<String>) -> Target {
    Target::Global(GlobalKey {
        kind,
        name: name.to_string(),
        owner,
    })
}

pub fn target(snapshot: &Snapshot, reference: &Reference) -> Option<Target> {
    let workspace = snapshot.workspace;
    let name = reference.name.as_str();
    let exists = |kind: SymbolKind| workspace.declares(&[kind], name);
    // In a call, a tuple type shadows a query of the same name.
    let callee = |kind: SymbolKind| match kind {
        SymbolKind::Query if exists(SymbolKind::TupleType) => {
            SymbolKind::TupleType
        }
        kind => kind,
    };
    if let Some(id) = reference.target {
        let symbol = &snapshot.analysis.symbols[id];
        // `RUN LOADING JOB` in another file names the file variables of a job.
        if symbol.is_exported() {
            let kind = match reference.role {
                Role::Function => callee(symbol.kind),
                _ => symbol.kind,
            };
            return Some(global(kind, &symbol.name, symbol.owner.clone()));
        }
        return Some(Target::Local(id));
    }
    let kinds = reference
        .role
        .global_kinds()
        .unwrap_or_default();
    let first_declared = || {
        kinds
            .iter()
            .copied()
            .find(|&kind| exists(kind))
    };
    match &reference.role {
        Role::VertexType
        | Role::VertexSource
        | Role::EdgeType
        | Role::EdgeSource
        | Role::SchemaType
        | Role::Graph
        | Role::Query
        | Role::TupleType => {
            Some(global(first_declared().unwrap_or(kinds[0]), name, None))
        }
        Role::Job => {
            if exists(SymbolKind::SchemaChangeJob) {
                Some(global(SymbolKind::SchemaChangeJob, name, None))
            } else {
                Some(global(SymbolKind::LoadingJob, name, None))
            }
        }
        // `x.name` on a tuple names one of its fields.
        Role::TupleField(tuple) | Role::Attribute(Ty::Tuple(tuple)) => {
            Some(global(SymbolKind::TupleField, name, Some(tuple.clone())))
        }
        Role::JobFile(job) => Some(global(
            SymbolKind::FilenameVariable,
            name,
            Some(job.clone()),
        )),
        Role::Function => first_declared()
            .map(|kind| global(callee(kind), name, None))
            .or_else(|| builtins::function(name).map(Target::Function)),
        Role::Method(ty) => builtins::find_method(methods_for(ty), name)
            .or_else(|| {
                matches!(ty, Ty::Unknown)
                    .then(|| find_method_anywhere(name))
                    .flatten()
            })
            .map(Target::Method),
        Role::Attribute(ty) => {
            let owners = ty.attribute_owners()?;
            let candidates: Vec<&GlobalSymbol> = workspace
                .find(SymbolKind::Attribute, name)
                .into_iter()
                .filter(|a| {
                    owners.is_empty()
                        || owners
                            .iter()
                            .any(|o| a.owner.as_deref() == Some(o))
                })
                .collect();
            match candidates.as_slice() {
                [] => None,
                [single] => Some(global(
                    SymbolKind::Attribute,
                    name,
                    single.owner.clone(),
                )),
                _ => Some(global(SymbolKind::Attribute, name, None)),
            }
        }
        Role::Value => {
            if let Some(doc) = builtins::constant(name) {
                let constant = builtins::CONSTANTS
                    .iter()
                    .find(|(k, _)| *k == name)?
                    .0;
                return Some(Target::Constant(constant, doc));
            }
            first_declared().map(|kind| global(kind, name, None))
        }
        Role::GlobalAccumulator
        | Role::LocalAccumulator
        | Role::Exception
        | Role::JobLocal
        | Role::TempColumn(_)
        | Role::Alias => None,
    }
}

/// What a call by plain name, `name(..)`, calls.
#[derive(Debug, Clone, Copy)]
pub enum Callee<'w> {
    /// The one workspace query of that name.
    Query(&'w GlobalSymbol),
    /// A tuple type: the call builds a tuple.
    Tuple,
    Builtin(&'static Function),
}

/// What `name(..)` calls: a tuple type of that name, else a query, else a built-in.
/// `None` when nothing has the name, or more than one query has it.
pub fn callee<'w>(
    snapshot: &'w Snapshot,
    function: Node,
) -> Option<Callee<'w>> {
    let reference = snapshot
        .analysis
        .reference_at(function.start_byte())
        .filter(|r| r.role == Role::Function)?;
    match target(snapshot, reference)? {
        Target::Global(key) => match key.kind {
            SymbolKind::Query => snapshot
                .workspace
                .find_unique(&[SymbolKind::Query], &key.name)
                .map(Callee::Query),
            SymbolKind::TupleType => Some(Callee::Tuple),
            _ => None,
        },
        Target::Local(id) => {
            let symbol = &snapshot.analysis.symbols[id];
            (symbol.kind == SymbolKind::TupleType).then_some(Callee::Tuple)
        }
        Target::Function(function) => Some(Callee::Builtin(function)),
        Target::Method(_) | Target::Constant(..) => None,
    }
}

/// The workspace query that a call by plain name, `name(..)`, calls.
pub fn called_query<'w>(
    snapshot: &'w Snapshot,
    function: Node,
) -> Option<&'w GlobalSymbol> {
    match callee(snapshot, function)? {
        Callee::Query(query) => Some(query),
        Callee::Tuple | Callee::Builtin(_) => None,
    }
}

/// The workspace query that `node` calls, with its name and argument list. `None` when
/// no single query has the name, or a plain call `q(..)` builds a tuple.
pub fn query_called_by<'w, 't>(
    snapshot: &'w Snapshot,
    node: Node<'t>,
) -> Option<(&'w GlobalSymbol, Node<'t>, Node<'t>)> {
    let (name, arguments) = syntax::query_call(node)?;
    let query = match node.kind() {
        "call_expression" => called_query(snapshot, name)?,
        _ => snapshot.workspace.find_unique(
            &[SymbolKind::Query],
            syntax::text(name, snapshot.text()),
        )?,
    };
    Some((query, name, arguments))
}

/// Workspace declarations matching a global key.
pub fn declarations<'w>(
    snapshot: &'w Snapshot,
    key: &GlobalKey,
) -> Vec<&'w GlobalSymbol> {
    snapshot
        .workspace
        .find(key.kind, &key.name)
        .into_iter()
        .filter(|s| key.owner.is_none() || s.owner == key.owner)
        .collect()
}

/// How a message names a literal passed to a query parameter, else `None`.
pub fn literal_argument(value: Node) -> Option<&'static str> {
    Some(match value.kind() {
        "integer" => "an integer",
        "float" => "a number with a fraction",
        "string" => "a string",
        "boolean" => "a boolean",
        "list_literal" => "a list",
        "unary_expression"
            if value
                .child_by_field_name("operator")
                .is_some_and(|o| o.kind() == "-") =>
        {
            match value.child_by_field_name("operand")?.kind() {
                "integer" => "a negative integer",
                "float" => "a number with a fraction",
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// The type of a query parameter in upper case without spaces, `SET<INT>`.
pub fn param_type(param: &Param) -> String {
    param
        .ty
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;

    #[test]
    fn a_call_names_a_tuple_type_rather_than_a_query_of_the_same_name() {
        let text = "CREATE QUERY main() {\n  ListAccum<Pair> @@l;\n  @@l += Pair(1, \"x\");\n  PRINT @@l;\n}\n";
        let tuple = (
            "file:///test/t.gsql",
            "TYPEDEF TUPLE <INT a, STRING b> Pair;\n",
        );
        let query =
            ("file:///test/p.gsql", "CREATE QUERY Pair() { PRINT 1; }\n");
        let callee = |others: &[(&str, &str)]| {
            let fixture = Fixture::with_files(text, others);
            let snapshot = fixture.snapshot();
            let at = text.find("Pair(").unwrap();
            let reference = snapshot.analysis.reference_at(at).unwrap();
            match target(&snapshot, reference) {
                Some(Target::Global(key)) => Some(key.kind),
                _ => None,
            }
        };
        assert_eq!(callee(&[query]), Some(SymbolKind::Query));
        assert_eq!(callee(&[query, tuple]), Some(SymbolKind::TupleType));
    }

    #[test]
    fn an_attribute_of_a_value_that_is_not_a_vertex_or_edge_names_nothing() {
        let schema =
            "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\n";
        let owner = |body: &str| {
            let text = format!("CREATE QUERY q() {{\n  {body}\n}}\n");
            let fixture = Fixture::with_schema(&text, schema);
            let snapshot = fixture.snapshot();
            let reference = snapshot
                .analysis
                .reference_at(text.rfind("name").unwrap())
                .unwrap();
            match target(&snapshot, reference) {
                Some(Target::Global(key)) => Some(key.owner),
                _ => None,
            }
        };
        assert_eq!(owner("STRING s = \"x\";\n  PRINT s.name;"), None);
        assert_eq!(owner("ListAccum<INT> @@l;\n  PRINT @@l.name;"), None);
        // An object of unknown type may be any vertex or edge.
        assert_eq!(owner("PRINT zz.name;"), Some(Some("Person".to_string())));
    }
}
