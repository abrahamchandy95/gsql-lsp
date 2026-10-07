//! The semantic model of one GSQL file: symbols, scopes and references.

use crate::text::Span;

pub type SymbolId = usize;
pub type ScopeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Graph,
    VertexType,
    EdgeType,
    Attribute,
    TupleType,
    TupleField,
    /// `TYPEDEF HeapAccum<...>(...) name`.
    AccumulatorType,
    Query,
    Parameter,
    Variable,
    VertexSet,
    GlobalAccumulator,
    LocalAccumulator,
    Alias,
    LoopVariable,
    File,
    Exception,
    Table,
    LoadingJob,
    SchemaChangeJob,
    FilenameVariable,
    Header,
    LineFilter,
    TempTable,
    /// A column named in `TO TEMP_TABLE t (a, b)`; the owner is the table.
    TempColumn,
    Package,
    DataSource,
}

impl SymbolKind {
    /// Symbols visible across the workspace rather than within one query or job.
    pub fn is_global(self) -> bool {
        matches!(
            self,
            SymbolKind::Graph
                | SymbolKind::VertexType
                | SymbolKind::EdgeType
                | SymbolKind::Attribute
                | SymbolKind::Query
                | SymbolKind::LoadingJob
                | SymbolKind::SchemaChangeJob
                | SymbolKind::Package
                | SymbolKind::DataSource
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            SymbolKind::Graph => "graph",
            SymbolKind::VertexType => "vertex type",
            SymbolKind::EdgeType => "edge type",
            SymbolKind::Attribute => "attribute",
            SymbolKind::TupleType => "tuple type",
            SymbolKind::TupleField => "tuple field",
            SymbolKind::AccumulatorType => "accumulator type",
            SymbolKind::Query => "query",
            SymbolKind::Parameter => "parameter",
            SymbolKind::Variable => "variable",
            SymbolKind::VertexSet => "vertex set",
            SymbolKind::GlobalAccumulator => "global accumulator",
            SymbolKind::LocalAccumulator => "vertex-attached accumulator",
            SymbolKind::Alias => "alias",
            SymbolKind::LoopVariable => "loop variable",
            SymbolKind::File => "file object",
            SymbolKind::Exception => "exception",
            SymbolKind::Table => "table",
            SymbolKind::LoadingJob => "loading job",
            SymbolKind::SchemaChangeJob => "schema change job",
            SymbolKind::FilenameVariable => "filename variable",
            SymbolKind::Header => "header",
            SymbolKind::LineFilter => "input line filter",
            SymbolKind::TempTable => "temporary table",
            SymbolKind::TempColumn => "temporary table column",
            SymbolKind::Package => "package",
            SymbolKind::DataSource => "data source",
        }
    }
}

/// A (possibly partial) static type.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Ty {
    #[default]
    Unknown,
    /// INT, UINT, FLOAT, DOUBLE, BOOL, STRING, DATETIME, JSONOBJECT, JSONARRAY.
    Primitive(String),
    /// A vertex of one of the listed types; empty means any vertex type.
    Vertex(Vec<String>),
    /// An edge of one of the listed types; empty means any edge type.
    Edge(Vec<String>),
    /// A vertex set whose members have one of the listed types.
    VertexSet(Vec<String>),
    Tuple(String),
    /// LIST, SET, BAG or MAP with their type arguments.
    Collection(String, Vec<Ty>),
    /// An accumulator (canonical name such as `SumAccum`) with its type arguments.
    Accumulator(String, Vec<Ty>),
    File,
}

impl Ty {
    /// The type of the elements produced when iterating over a value of this type.
    pub fn element(&self) -> Ty {
        match self {
            Ty::VertexSet(types) => Ty::Vertex(types.clone()),
            Ty::Collection(_, args) | Ty::Accumulator(_, args) => match args.as_slice() {
                [single] => single.clone(),
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    /// The type of `value.method(..)` for the methods whose result type is tracked.
    pub fn method_result(&self, method: &str) -> Ty {
        match (method.to_ascii_lowercase().as_str(), self) {
            ("top" | "pop", Ty::Accumulator(kind, _)) if kind == "HeapAccum" => self.element(),
            ("get", _) => match self.key_value() {
                Some((_, value)) => value,
                None => self.element(),
            },
            ("step", _) => Ty::Primitive("INT".into()),
            _ => Ty::Unknown,
        }
    }

    /// Key and value types of a map-like accumulator or collection.
    pub fn key_value(&self) -> Option<(Ty, Ty)> {
        match self {
            Ty::Collection(name, args) | Ty::Accumulator(name, args)
                if name.eq_ignore_ascii_case("MAP") || name == "MapAccum" =>
            {
                match args.as_slice() {
                    [key, value] => Some((key.clone(), value.clone())),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Vertex types reachable through a value of this type, if it is vertex-like.
    pub fn vertex_types(&self) -> Option<&[String]> {
        match self {
            Ty::Vertex(types) | Ty::VertexSet(types) => Some(types),
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        fn join(types: &[String]) -> String {
            types.join(" | ")
        }
        fn args(args: &[Ty]) -> String {
            args.iter().map(Ty::display).collect::<Vec<_>>().join(", ")
        }
        match self {
            Ty::Unknown => "unknown".into(),
            Ty::Primitive(name) => name.clone(),
            Ty::Vertex(types) if types.is_empty() => "VERTEX".into(),
            Ty::Vertex(types) => format!("VERTEX<{}>", join(types)),
            Ty::Edge(types) if types.is_empty() => "EDGE".into(),
            Ty::Edge(types) => format!("EDGE<{}>", join(types)),
            Ty::VertexSet(types) if types.is_empty() => "vertex set".into(),
            Ty::VertexSet(types) => format!("vertex set of {}", join(types)),
            Ty::Tuple(name) => name.clone(),
            Ty::Collection(name, a) | Ty::Accumulator(name, a) if a.is_empty() => name.clone(),
            Ty::Collection(name, a) | Ty::Accumulator(name, a) => format!("{name}<{}>", args(a)),
            Ty::File => "FILE".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
}

/// Endpoint types of an edge type, kept apart to tell directions.
#[derive(Debug, Clone, Default)]
pub struct EdgeEnds {
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub directed: bool,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub scope: ScopeId,
    /// The whole declaration.
    pub span: Span,
    /// The declared name.
    pub name_span: Span,
    /// One-line rendering of the declaration.
    pub detail: String,
    pub doc: Option<String>,
    pub ty: Ty,
    /// Owning vertex/edge type of an attribute, or tuple of a tuple field.
    pub owner: Option<String>,
    /// Graph of a query or job (`FOR GRAPH`).
    pub graph: Option<String>,
    /// Query parameters.
    pub params: Vec<Param>,
    /// The RETURNS type of a query as written, without the parentheses.
    pub returns: Option<String>,
    /// Members of a graph; source and target vertex types of an edge type.
    pub members: Vec<String>,
    /// For an edge type: source and target vertex types, and whether it is directed.
    pub ends: EdgeEnds,
    /// Declared implicitly by its first assignment.
    pub implicit: bool,
    /// An accumulator attached to edges: `SumAccum<INT> EDGE @weight`.
    pub on_edges: bool,
    /// Declared inside a syntax error, where error recovery may have guessed wrong.
    pub in_error: bool,
    /// A copy of another declaration rather than one written out: the
    /// attributes of a reverse edge (`WITH REVERSE_EDGE="..."`).
    pub copied: bool,
    /// A vector attribute (`ALTER VERTEX v ADD VECTOR ATTRIBUTE emb(...)`): it
    /// takes part in no VALUES list of the vertex type.
    pub vector: bool,
    /// The `PRIMARY_ID` of a vertex type declared without
    /// `WITH primary_id_as_attribute="true"`: a query cannot read it as an attribute.
    pub id_only: bool,
    /// A top-level `CREATE` of a vertex type, edge type, graph or query
    /// without `OR REPLACE`: it fails when the name exists.
    pub strict_create: bool,
    /// An attribute added by `ALTER ... ADD`: listed after those of the `CREATE`.
    pub altered: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    File,
    Query,
    Block,
    Job,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub kind: ScopeKind,
    pub span: Span,
    pub parent: Option<ScopeId>,
    pub symbols: Vec<SymbolId>,
}

/// What an identifier occurrence refers to, as determined by its syntactic position.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    /// An identifier in an expression.
    Value,
    GlobalAccumulator,
    LocalAccumulator,
    VertexType,
    EdgeType,
    /// A vertex or edge type (graph members, INSERT targets).
    SchemaType,
    /// The vertex position of a FROM pattern: a vertex set or a vertex type.
    VertexSource,
    /// The edge position of a FROM pattern: an edge type or a variable holding type names.
    EdgeSource,
    /// `x.name` where `x` has the given type.
    Attribute(Ty),
    /// `x.name(...)` where `x` has the given type.
    Method(Ty),
    /// The callee of `name(...)`.
    Function,
    Graph,
    Query,
    Job,
    TupleType,
    TupleField(String),
    Exception,
    /// A name local to a loading job (filename variable, header, filter, temp table).
    JobLocal,
    /// A file variable of the named loading job, in `RUN LOADING JOB job USING f1="path"`.
    JobFile(String),
    /// `$"col"` reading a column of the named TEMP_TABLE.
    TempColumn(String),
    /// The alias of a DELETE or UPDATE statement.
    Alias,
}

/// The kind of construct an occurrence appears in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Context {
    Query,
    LoadingJob,
    /// Schema change jobs, DROP and ALTER: these commonly name types that
    /// exist only in the database.
    SchemaEdit,
    /// Top-level vertex, edge, graph and tuple definitions.
    Definition,
    #[default]
    Command,
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub span: Span,
    pub name: String,
    pub role: Role,
    pub scope: ScopeId,
    /// The symbol in this file the occurrence resolves to.
    pub target: Option<SymbolId>,
    /// The occurrence is the declaration of `target`.
    pub declaration: bool,
    /// The occurrence is assigned to.
    pub write: bool,
    pub context: Context,
    /// The occurrence qualifies a member call, like `lib` in `lib.query(...)`.
    pub qualifier: bool,
    /// The occurrence is inside a syntax error.
    pub in_error: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub symbols: Vec<Symbol>,
    pub scopes: Vec<Scope>,
    /// Every identifier-like occurrence, sorted by position.
    pub references: Vec<Reference>,
    /// What `DROP` statements remove: the kind and the name (`*` for all of them).
    pub drops: Vec<(SymbolKind, String)>,
    /// Scope ids ordered by start offset (outer scopes first on ties).
    scope_order: Vec<ScopeId>,
    /// An untyped pattern alias follows a named edge: its type depends on the
    /// workspace schema, so the analysis is redone when the schema changes.
    pub uses_schema_edges: bool,
}

impl Analysis {
    pub fn reference_at(&self, offset: usize) -> Option<&Reference> {
        let index = self.references.partition_point(|r| r.span.end < offset);
        self.references[index..].iter().take_while(|r| r.span.start <= offset).find(|r| r.span.contains(offset))
    }

    /// Indexes the scopes for [`Analysis::scope_at`]; call after adding scopes.
    pub fn index_scopes(&mut self) {
        let mut order: Vec<ScopeId> = (0..self.scopes.len()).collect();
        order.sort_by_key(|&id| (self.scopes[id].span.start, std::cmp::Reverse(self.scopes[id].span.end)));
        self.scope_order = order;
    }

    /// The innermost scope containing `offset`.
    pub fn scope_at(&self, offset: usize) -> ScopeId {
        if self.scope_order.len() != self.scopes.len() {
            // Not indexed (only while the model is being built): linear scan.
            let mut best = 0;
            for (id, scope) in self.scopes.iter().enumerate() {
                if scope.span.contains(offset) && scope.span.len() <= self.scopes[best].span.len() {
                    best = id;
                }
            }
            return best;
        }
        // Scopes nest properly, so the innermost scope containing `offset` is
        // the last one starting at or before it, or one of its ancestors.
        let index = self.scope_order.partition_point(|&id| self.scopes[id].span.start <= offset);
        let Some(&candidate) = index.checked_sub(1).and_then(|i| self.scope_order.get(i)) else {
            return 0;
        };
        self.scope_chain(candidate).find(|&id| self.scopes[id].span.contains(offset)).unwrap_or(0)
    }

    pub fn scope_chain(&self, scope: ScopeId) -> impl Iterator<Item = ScopeId> + '_ {
        std::iter::successors(Some(scope), |&s| self.scopes[s].parent)
    }

    /// The innermost query scope enclosing `scope`.
    pub fn query_scope(&self, scope: ScopeId) -> Option<ScopeId> {
        self.scope_chain(scope).find(|&s| self.scopes[s].kind == ScopeKind::Query)
    }

    /// Symbols visible from `offset`, innermost scopes first.
    pub fn visible_symbols(&self, offset: usize) -> Vec<&Symbol> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for scope in self.scope_chain(self.scope_at(offset)) {
            for &id in &self.scopes[scope].symbols {
                let symbol = &self.symbols[id];
                if seen.insert((symbol.name.clone(), symbol.kind)) {
                    result.push(symbol);
                }
            }
        }
        result
    }

    pub fn references_to(&self, symbol: SymbolId) -> impl Iterator<Item = &Reference> {
        self.references.iter().filter(move |r| r.target == Some(symbol))
    }
}
