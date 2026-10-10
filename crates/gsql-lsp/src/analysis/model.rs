//! The semantic model of one GSQL file: symbols, scopes and references.

use crate::text::Span;

pub type SymbolId = usize;
pub type ScopeId = usize;

/// The scope of the whole file.
pub const FILE_SCOPE: ScopeId = 0;

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

    /// Kinds shared with the workspace when declared at file level.
    pub fn is_exported(self) -> bool {
        self.is_global()
            || matches!(
                self,
                SymbolKind::TupleType
                    | SymbolKind::TupleField
                    | SymbolKind::AccumulatorType
            )
    }

    pub fn is_accumulator(self) -> bool {
        matches!(
            self,
            SymbolKind::GlobalAccumulator | SymbolKind::LocalAccumulator
        )
    }

    /// The kind of a global (`@@name`) or vertex-attached (`@name`) accumulator.
    pub fn accumulator(global: bool) -> SymbolKind {
        if global {
            SymbolKind::GlobalAccumulator
        } else {
            SymbolKind::LocalAccumulator
        }
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
            _ => self
                .element_ref()
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// The single type argument of a collection or accumulator.
    fn element_ref(&self) -> Option<&Ty> {
        match self {
            Ty::Collection(_, args) | Ty::Accumulator(_, args) => {
                match args.as_slice() {
                    [single] => Some(single),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The type of `value.method(..)` for the methods whose result type is tracked.
    pub fn method_result(&self, method: &str) -> Ty {
        match (method.to_ascii_lowercase().as_str(), self) {
            ("top" | "pop", Ty::Accumulator(kind, _))
                if kind == "HeapAccum" =>
            {
                self.element()
            }
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

    /// The vertex or edge types of a vertex, vertex set or edge, with their kind;
    pub fn schema_owners(&self) -> Option<(SymbolKind, &[String])> {
        match self {
            Ty::Vertex(types) | Ty::VertexSet(types) => {
                Some((SymbolKind::VertexType, types))
            }
            Ty::Edge(types) => Some((SymbolKind::EdgeType, types)),
            _ => None,
        }
    }

    /// The vertex and edge types whose attributes `x.name` can name when `x` has
    /// this type; empty means any. `None` for a type without attributes.
    pub fn attribute_owners(&self) -> Option<&[String]> {
        match self {
            Ty::Unknown => Some(&[]),
            _ => self
                .schema_owners()
                .map(|(_, owners)| owners),
        }
    }

    /// Vertex types of a vertex, a vertex set, or a collection of vertices.
    pub fn member_vertex_types(&self) -> Vec<String> {
        self.vertex_types()
            .or_else(|| self.element_ref()?.vertex_types())
            .map(<[String]>::to_vec)
            .unwrap_or_default()
    }

    pub fn display(&self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Unknown => f.write_str("unknown"),
            Ty::Primitive(name) | Ty::Tuple(name) => f.write_str(name),
            Ty::Vertex(types) if types.is_empty() => f.write_str("VERTEX"),
            Ty::Vertex(types) => {
                write!(f, "VERTEX<{}>", Joined(types, " | "))
            }
            Ty::Edge(types) if types.is_empty() => f.write_str("EDGE"),
            Ty::Edge(types) => write!(f, "EDGE<{}>", Joined(types, " | ")),
            Ty::VertexSet(types) if types.is_empty() => {
                f.write_str("vertex set")
            }
            Ty::VertexSet(types) => {
                write!(f, "vertex set of {}", Joined(types, " | "))
            }
            Ty::Collection(name, a) | Ty::Accumulator(name, a)
                if a.is_empty() =>
            {
                f.write_str(name)
            }
            Ty::Collection(name, a) | Ty::Accumulator(name, a) => {
                write!(f, "{name}<{}>", Joined(a, ", "))
            }
            Ty::File => f.write_str("FILE"),
        }
    }
}

/// Items written with a separator between them, without building a string.
struct Joined<'a, T>(&'a [T], &'a str);

impl<T: std::fmt::Display> std::fmt::Display for Joined<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, item) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(self.1)?;
            }
            write!(f, "{item}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
}

impl std::fmt::Display for Param {
    /// `TYPE name`, with ` = default` when there is one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.ty, self.name)?;
        if let Some(default) = &self.default {
            write!(f, " = {default}")?;
        }
        Ok(())
    }
}

/// Endpoint types of an edge type, kept apart to tell directions.
#[derive(Debug, Clone, Default)]
pub struct EdgeEnds {
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub directed: bool,
}

/// Start of a reverse edge's detail; the name of the edge that declares it follows.
pub const REVERSE_EDGE_PREFIX: &str = "reverse edge of ";

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

impl Symbol {
    /// A file variable of a loading job.
    pub fn is_job_file(&self) -> bool {
        self.kind == SymbolKind::FilenameVariable && self.owner.is_some()
    }

    /// Shared with the workspace: a file-level exported kind or a job file variable.
    pub fn is_exported(&self) -> bool {
        (self.scope == FILE_SCOPE && self.kind.is_exported())
            || self.is_job_file()
    }
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

impl Role {
    /// The kinds of file-level declaration a name in this role can refer to; the
    /// order matters. `None` for roles that name locals or owned symbols.
    pub fn global_kinds(&self) -> Option<&'static [SymbolKind]> {
        use SymbolKind as K;
        Some(match self {
            Role::VertexType | Role::VertexSource => &[K::VertexType],
            Role::EdgeType | Role::EdgeSource => &[K::EdgeType],
            Role::SchemaType => &[K::VertexType, K::EdgeType],
            Role::Graph => &[K::Graph],
            Role::Query => &[K::Query],
            Role::Job => &[K::LoadingJob, K::SchemaChangeJob],
            Role::TupleType => &[K::TupleType, K::AccumulatorType],
            Role::Function => &[K::Query, K::TupleType],
            Role::Value => {
                &[K::VertexType, K::EdgeType, K::Query, K::TupleType]
            }
            // Listed one by one so that a new Role forces a decision here.
            Role::GlobalAccumulator
            | Role::LocalAccumulator
            | Role::Exception
            | Role::JobLocal
            | Role::Alias
            | Role::Attribute(_)
            | Role::Method(_)
            | Role::TupleField(_)
            | Role::JobFile(_)
            | Role::TempColumn(_) => return None,
        })
    }
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
    /// Every identifier-like occurrence, sorted by position; they
    /// never overlap.
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
        let index = self
            .references
            .partition_point(|r| r.span.end < offset);
        self.references[index..]
            .iter()
            .take_while(|r| r.span.start <= offset)
            .find(|r| r.span.contains(offset))
    }

    /// The symbol in this file that `reference` resolves to.
    pub fn target_symbol(&self, reference: &Reference) -> Option<&Symbol> {
        reference.target.map(|id| &self.symbols[id])
    }

    /// Indexes the scopes for [`Analysis::scope_at`]; call after adding scopes.
    pub(super) fn index_scopes(&mut self) {
        let mut order: Vec<ScopeId> = (0..self.scopes.len()).collect();
        order.sort_by_key(|&id| {
            (
                self.scopes[id].span.start,
                std::cmp::Reverse(self.scopes[id].span.end),
            )
        });
        self.scope_order = order;
    }

    /// The innermost scope containing `offset`.
    pub fn scope_at(&self, offset: usize) -> ScopeId {
        debug_assert_eq!(
            self.scope_order.len(),
            self.scopes.len(),
            "scopes not indexed"
        );
        // Scopes nest properly, so the innermost scope containing `offset` is
        // the last one starting at or before it, or one of its ancestors.
        let index = self
            .scope_order
            .partition_point(|&id| self.scopes[id].span.start <= offset);
        let Some(&candidate) = index
            .checked_sub(1)
            .and_then(|i| self.scope_order.get(i))
        else {
            return FILE_SCOPE;
        };
        self.scope_chain(candidate)
            .find(|&id| self.scopes[id].span.contains(offset))
            .unwrap_or(FILE_SCOPE)
    }

    pub fn scope_chain(
        &self,
        scope: ScopeId,
    ) -> impl Iterator<Item = ScopeId> + '_ {
        std::iter::successors(Some(scope), |&s| self.scopes[s].parent)
    }

    /// Symbols declared directly in `scope`, in order.
    pub fn scope_symbols(
        &self,
        scope: ScopeId,
    ) -> impl Iterator<Item = &Symbol> + '_ {
        self.scopes[scope]
            .symbols
            .iter()
            .map(|&id| &self.symbols[id])
    }

    /// The innermost query scope enclosing `scope`.
    pub fn query_scope(&self, scope: ScopeId) -> Option<ScopeId> {
        self.scope_chain(scope)
            .find(|&s| self.scopes[s].kind == ScopeKind::Query)
    }

    /// The innermost query scope containing `offset`.
    pub fn query_scope_at(&self, offset: usize) -> Option<ScopeId> {
        self.query_scope(self.scope_at(offset))
    }

    /// Symbols visible from `offset`, innermost scopes first.
    pub fn visible_symbols(&self, offset: usize) -> Vec<&Symbol> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for scope in self.scope_chain(self.scope_at(offset)) {
            for symbol in self.scope_symbols(scope) {
                if seen.insert((symbol.name.as_str(), symbol.kind)) {
                    result.push(symbol);
                }
            }
        }
        result
    }

    /// The innermost symbol named `name` of one of `kinds` visible from `offset`.
    pub fn visible_symbol(
        &self,
        offset: usize,
        name: &str,
        kinds: &[SymbolKind],
    ) -> Option<&Symbol> {
        self.scope_chain(self.scope_at(offset))
            .find_map(|scope| {
                self.scope_symbols(scope)
                    .find(|s| s.name == name && kinds.contains(&s.kind))
            })
    }

    pub fn references_to(
        &self,
        symbol: SymbolId,
    ) -> impl Iterator<Item = &Reference> {
        self.references
            .iter()
            .filter(move |r| r.target == Some(symbol))
    }

    /// Uses outside syntax errors that resolve to no symbol in this file.
    pub fn unresolved_uses(&self) -> impl Iterator<Item = &Reference> {
        self.references
            .iter()
            .filter(|r| r.target.is_none() && !r.declaration && !r.in_error)
    }

    /// Symbols of `kind` owned by `owner`, from every scope, in declaration order.
    pub fn symbols_owned_by(
        &self,
        kind: SymbolKind,
        owner: &str,
    ) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().filter(move |s| {
            s.kind == kind && s.owner.as_deref() == Some(owner)
        })
    }
}
