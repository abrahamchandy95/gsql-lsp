//! Resolves an identifier occurrence to what it denotes: a symbol in this
//! file, a workspace-level declaration, or a built-in.

use crate::analysis::{Reference, Role, SymbolId, SymbolKind, Ty};
use crate::builtins::{self, Function, Method};
use crate::features::Snapshot;
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
                a.kind == b.kind && a.name == b.name && (a.owner.is_none() || b.owner.is_none() || a.owner == b.owner)
            }
            (Target::Function(a), Target::Function(b)) => std::ptr::eq(*a, *b),
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
        Ty::Accumulator(kind, _) => builtins::accumulator(kind).map(|a| a.methods).unwrap_or(&[]),
        Ty::Primitive(name) if name == "JSONOBJECT" => builtins::JSON_OBJECT_METHODS,
        Ty::Primitive(name) if name == "JSONARRAY" => builtins::JSON_ARRAY_METHODS,
        _ => &[],
    }
}

pub fn find_method_anywhere(name: &str) -> Option<&'static Method> {
    let tables: [&'static [Method]; 9] = [
        builtins::VERTEX_METHODS,
        builtins::EDGE_METHODS,
        builtins::VERTEX_SET_METHODS,
        builtins::LIST_METHODS,
        builtins::SET_METHODS,
        builtins::MAP_METHODS,
        builtins::JSON_OBJECT_METHODS,
        builtins::JSON_ARRAY_METHODS,
        builtins::FILE_METHODS,
    ];
    tables
        .into_iter()
        .chain(builtins::ACCUMULATORS.iter().map(|a| a.methods))
        .find_map(|methods| builtins::find_method(methods, name))
}

fn global(kind: SymbolKind, name: &str, owner: Option<String>) -> Target {
    Target::Global(GlobalKey { kind, name: name.to_string(), owner })
}

pub fn target(snapshot: &Snapshot, reference: &Reference) -> Option<Target> {
    let workspace = snapshot.workspace;
    if let Some(id) = reference.target {
        let symbol = &snapshot.analysis.symbols[id];
        let file_level = symbol.scope == 0
            && (symbol.kind.is_global()
                || matches!(symbol.kind, SymbolKind::TupleType | SymbolKind::TupleField | SymbolKind::AccumulatorType));
        // `RUN LOADING JOB` in another file names the file variables of a job.
        let job_file = symbol.kind == SymbolKind::FilenameVariable && symbol.owner.is_some();
        if file_level || job_file {
            return Some(global(symbol.kind, &symbol.name, symbol.owner.clone()));
        }
        return Some(Target::Local(id));
    }
    let name = reference.name.as_str();
    let exists = |kind: SymbolKind| !workspace.find(kind, name).is_empty();
    match &reference.role {
        Role::VertexType | Role::VertexSource => Some(global(SymbolKind::VertexType, name, None)),
        Role::EdgeType | Role::EdgeSource => Some(global(SymbolKind::EdgeType, name, None)),
        Role::SchemaType => {
            if exists(SymbolKind::EdgeType) && !exists(SymbolKind::VertexType) {
                Some(global(SymbolKind::EdgeType, name, None))
            } else {
                Some(global(SymbolKind::VertexType, name, None))
            }
        }
        Role::Graph => Some(global(SymbolKind::Graph, name, None)),
        Role::Query => Some(global(SymbolKind::Query, name, None)),
        Role::Job => {
            if exists(SymbolKind::SchemaChangeJob) {
                Some(global(SymbolKind::SchemaChangeJob, name, None))
            } else {
                Some(global(SymbolKind::LoadingJob, name, None))
            }
        }
        Role::TupleType => {
            if exists(SymbolKind::AccumulatorType) && !exists(SymbolKind::TupleType) {
                Some(global(SymbolKind::AccumulatorType, name, None))
            } else {
                Some(global(SymbolKind::TupleType, name, None))
            }
        }
        Role::TupleField(tuple) => Some(global(SymbolKind::TupleField, name, Some(tuple.clone()))),
        Role::JobFile(job) => Some(global(SymbolKind::FilenameVariable, name, Some(job.clone()))),
        Role::Function => {
            if exists(SymbolKind::Query) {
                Some(global(SymbolKind::Query, name, None))
            } else if exists(SymbolKind::TupleType) {
                Some(global(SymbolKind::TupleType, name, None))
            } else {
                builtins::function(name).map(Target::Function)
            }
        }
        Role::Method(ty) => builtins::find_method(methods_for(ty), name)
            .or_else(|| matches!(ty, Ty::Unknown).then(|| find_method_anywhere(name)).flatten())
            .map(Target::Method),
        Role::Attribute(ty) => {
            let owners: Vec<String> = match ty {
                Ty::Vertex(types) | Ty::Edge(types) | Ty::VertexSet(types) => types.clone(),
                Ty::Tuple(tuple) => return Some(global(SymbolKind::TupleField, name, Some(tuple.clone()))),
                _ => Vec::new(),
            };
            let candidates: Vec<&GlobalSymbol> = workspace
                .find(SymbolKind::Attribute, name)
                .into_iter()
                .filter(|a| owners.is_empty() || owners.iter().any(|o| a.owner.as_deref() == Some(o)))
                .collect();
            match candidates.as_slice() {
                [] => None,
                [single] => Some(global(SymbolKind::Attribute, name, single.owner.clone())),
                _ => Some(global(SymbolKind::Attribute, name, None)),
            }
        }
        Role::Value => {
            if let Some(doc) = builtins::constant(name) {
                let constant = builtins::CONSTANTS.iter().find(|(k, _)| *k == name)?.0;
                return Some(Target::Constant(constant, doc));
            }
            [SymbolKind::VertexType, SymbolKind::EdgeType, SymbolKind::Query, SymbolKind::TupleType]
                .into_iter()
                .find(|&kind| exists(kind))
                .map(|kind| global(kind, name, None))
        }
        Role::GlobalAccumulator
        | Role::LocalAccumulator
        | Role::Exception
        | Role::JobLocal
        | Role::TempColumn(_)
        | Role::Alias => None,
    }
}

/// Workspace declarations matching a global key.
pub fn declarations<'w>(snapshot: &'w Snapshot, key: &GlobalKey) -> Vec<&'w GlobalSymbol> {
    snapshot
        .workspace
        .find(key.kind, &key.name)
        .into_iter()
        .filter(|s| key.owner.is_none() || s.owner == key.owner)
        .collect()
}
