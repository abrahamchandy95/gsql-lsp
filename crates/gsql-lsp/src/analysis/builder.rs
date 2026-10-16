//! Builds the [`Analysis`]: pass 1 declares symbols and scopes, pass 2 resolves references.
//! Workspace names not defined here stay unresolved for the features to look up.

use std::collections::HashMap;

use tree_sitter::{Node, Tree};

use super::flow;
use super::model::*;
use crate::syntax::{self, field, keyword, kind};
use crate::text::{Span, starts_with_ignore_ascii_case};
use crate::util::{extend_unique, push_unique};
use crate::workspace::Workspace;

impl Analysis {
    /// Runs both passes; a workspace lets schema edges type aliases.
    pub fn from_tree(
        tree: &Tree,
        source: &str,
        workspace: Option<&Workspace>,
    ) -> Self {
        let root = tree.root_node();
        let mut builder = Builder {
            source,
            workspace,
            uses_schema_edges: std::cell::Cell::new(false),
            analysis: Analysis::default(),
            stack: Vec::new(),
            declarations: HashMap::new(),
            select_scopes: HashMap::new(),
            names: Vec::new(),
            error_depth: 0,
            depth: 0,
        };
        builder.push(ScopeKind::File, root);
        builder.declare(root);
        builder.stack.clear();
        builder.analysis.index_scopes();
        builder.resolve(root);
        let mut analysis = builder.analysis;
        analysis.uses_schema_edges = builder.uses_schema_edges.get();
        analysis
            .references
            .sort_by_key(|r| (r.span.start, r.span.end));
        analysis
    }

    /// Parses and analyzes `text` without a workspace.
    pub fn parse(text: &str) -> (Tree, Self) {
        let tree = syntax::parse(&mut syntax::new_parser(), text, None);
        let analysis = Analysis::from_tree(&tree, text, None);
        (tree, analysis)
    }

    /// Analyzes again with `workspace` if alias types depend on its schema edges.
    pub fn schema_changed(
        &mut self,
        tree: &Tree,
        source: &str,
        workspace: &Workspace,
    ) {
        if self.uses_schema_edges {
            *self = Analysis::from_tree(tree, source, Some(workspace));
        }
    }
}

/// Collapses runs of whitespace so declarations render on one line.
pub fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(word);
    }
    result
}

/// The canonical spelling of an accumulator kind (`sumaccum` -> `SumAccum`).
pub fn canonical_accumulator(name: &str) -> String {
    crate::builtins::accumulator(name)
        .map(|a| a.name.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// The static type described by a type node.
pub fn type_of_type_node(node: Node, source: &str) -> Ty {
    match node.kind_id() {
        kind::PRIMITIVE_TYPE => Ty::Primitive(
            collapse_whitespace(syntax::text(node, source)).to_uppercase(),
        ),
        kind::VERTEX_TYPE => Ty::Vertex(field_list(node, "type", source)),
        kind::EDGE_TYPE => Ty::Edge(field_list(node, "type", source)),
        kind::TYPE_IDENTIFIER | kind::IDENTIFIER => {
            Ty::Tuple(syntax::text(node, source).to_string())
        }
        kind::FILE_TYPE => Ty::File,
        kind::COLLECTION_TYPE => {
            let kind = syntax::field_text(node, "kind", source)
                .unwrap_or("LIST")
                .to_uppercase();
            let args = ["element", "key", "value"]
                .iter()
                .filter_map(|field| node.child_by_field_name(field))
                .map(|child| type_of_type_node(child, source))
                .collect();
            Ty::Collection(kind, args)
        }
        kind::ACCUMULATOR_TYPE => {
            let kind = syntax::field_text(node, "kind", source).unwrap_or("");
            let args = syntax::children_by_field(node, "argument")
                .into_iter()
                .map(|arg| match arg.kind_id() {
                    kind::GROUP_BY_FIELD => arg
                        .child_by_field_id(field::TYPE)
                        .map(|t| type_of_type_node(t, source))
                        .unwrap_or_default(),
                    _ => type_of_type_node(arg, source),
                })
                .collect();
            Ty::Accumulator(canonical_accumulator(kind), args)
        }
        _ => Ty::Unknown,
    }
}

/// The primitive type of a literal node kind.
pub fn literal_type(kind: &str) -> Option<&'static str> {
    match kind {
        "integer" => Some("INT"),
        "float" => Some("DOUBLE"),
        "string" => Some("STRING"),
        "boolean" => Some("BOOL"),
        _ => None,
    }
}

fn field_list(node: Node, field: &str, source: &str) -> Vec<String> {
    syntax::children_by_field(node, field)
        .into_iter()
        .map(|child| syntax::text(child, source).to_string())
        .collect()
}

/// Symbol kinds an identifier in an expression can refer to within a query or job.
const LOCAL_VALUES: &[SymbolKind] = &[
    SymbolKind::Parameter,
    SymbolKind::Variable,
    SymbolKind::VertexSet,
    SymbolKind::Alias,
    SymbolKind::LoopVariable,
    SymbolKind::File,
    SymbolKind::Table,
    SymbolKind::FilenameVariable,
    SymbolKind::Header,
    SymbolKind::LineFilter,
    SymbolKind::TempTable,
];

/// Values that can stand in the vertex position of a FROM pattern.
const VERTEX_SOURCE_VALUES: &[SymbolKind] = &[
    SymbolKind::VertexSet,
    SymbolKind::Parameter,
    SymbolKind::Variable,
    SymbolKind::LoopVariable,
    SymbolKind::Alias,
];

/// Values that can hold edge type names in the edge position of a FROM pattern.
const EDGE_SOURCE_VALUES: &[SymbolKind] = &[
    SymbolKind::Parameter,
    SymbolKind::Variable,
    SymbolKind::LoopVariable,
];

struct Builder<'s> {
    source: &'s str,
    workspace: Option<&'s Workspace>,
    uses_schema_edges: std::cell::Cell<bool>,
    analysis: Analysis,
    stack: Vec<ScopeId>,
    /// Name spans of declarations.
    declarations: HashMap<Span, SymbolId>,
    /// Block scopes of SELECT statements, by statement span.
    select_scopes: HashMap<Span, ScopeId>,
    /// Per scope: symbols by name, in declaration order.
    names: Vec<HashMap<String, Vec<SymbolId>>>,
    /// Number of ERROR nodes enclosing the node being declared.
    error_depth: usize,
    /// How deep the declaration pass is in the tree.
    depth: usize,
}

/// A top-level `CREATE` (not `ADD`, not `OR REPLACE`, not a function).
fn is_strict_create(node: Node) -> bool {
    if node
        .parent()
        .is_none_or(|p| p.kind_id() != kind::SOURCE_FILE)
    {
        return false;
    }
    let mut cursor = node.walk();
    let mut children = node.children(&mut cursor);
    children
        .next()
        .is_some_and(|c| c.kind_id() == keyword::CREATE)
        && !children
            .any(|c| matches!(c.kind_id(), keyword::OR | keyword::FUNCTION))
}

/// `WITH primary_id_as_attribute` set to anything but false.
fn has_primary_id_as_attribute(node: Node, source: &str) -> bool {
    let Some(with) = syntax::child_of_kind(node, "with_clause") else {
        return false;
    };
    with.children(&mut with.walk())
        .any(|option| {
            syntax::field_text(option, "key", source).is_some_and(|k| {
                k.eq_ignore_ascii_case("primary_id_as_attribute")
            }) && !syntax::field_text(option, "value", source).is_some_and(
                |v| {
                    v.trim_matches('"')
                        .eq_ignore_ascii_case("false")
                },
            )
        })
}

/// Recursion limit for pass 1 (guards the stack).
const MAX_DEPTH: usize = 4_000;

struct NewSymbol {
    name: String,
    kind: SymbolKind,
    span: Span,
    name_span: Span,
    detail: String,
    ty: Ty,
}

impl<'s> Builder<'s> {
    fn text(&self, node: Node) -> &'s str {
        syntax::text(node, self.source)
    }

    fn current_scope(&self) -> ScopeId {
        *self
            .stack
            .last()
            .expect("the file scope is always on the stack")
    }

    fn push(&mut self, kind: ScopeKind, node: Node) -> ScopeId {
        let id = self.analysis.scopes.len();
        self.analysis.scopes.push(Scope {
            kind,
            span: Span::of(node),
            parent: self.stack.last().copied(),
            symbols: Vec::new(),
        });
        self.names.push(HashMap::new());
        self.stack.push(id);
        id
    }

    fn pop(&mut self) {
        self.stack.pop();
    }

    /// The query scope if inside a query, otherwise the current scope.
    fn query_or_current(&self) -> ScopeId {
        self.stack
            .iter()
            .rev()
            .copied()
            .find(|&s| self.analysis.scopes[s].kind == ScopeKind::Query)
            .unwrap_or_else(|| self.current_scope())
    }

    fn define_in(&mut self, scope: ScopeId, new: NewSymbol) -> SymbolId {
        let id = self.analysis.symbols.len();
        self.analysis.symbols.push(Symbol {
            name: new.name,
            kind: new.kind,
            scope,
            span: new.span,
            name_span: new.name_span,
            detail: new.detail,
            doc: None,
            ty: new.ty,
            owner: None,
            graph: None,
            params: Vec::new(),
            returns: None,
            members: Vec::new(),
            ends: EdgeEnds::default(),
            implicit: false,
            on_edges: false,
            in_error: self.error_depth > 0,
            copied: false,
            vector: false,
            id_only: false,
            strict_create: false,
            altered: false,
        });
        self.add_to_scope(scope, id);
        self.declarations
            .insert(self.analysis.symbols[id].name_span, id);
        id
    }

    fn add_to_scope(&mut self, scope: ScopeId, id: SymbolId) {
        self.analysis.scopes[scope].symbols.push(id);
        let name = &self.analysis.symbols[id].name;
        match self.names[scope].get_mut(name) {
            Some(ids) => ids.push(id),
            None => {
                self.names[scope].insert(name.clone(), vec![id]);
            }
        }
    }

    /// Symbols named `name` declared directly in `scope`.
    fn named(&self, scope: ScopeId, name: &str) -> &[SymbolId] {
        self.names[scope]
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Defines a symbol named by `name_node`, documented by comments before `decl`.
    fn define_named(
        &mut self,
        scope: ScopeId,
        kind: SymbolKind,
        decl: Node,
        name_node: Node,
        detail: String,
        ty: Ty,
    ) -> SymbolId {
        let id = self.define_in(
            scope,
            NewSymbol {
                name: self.text(name_node).to_string(),
                kind,
                span: Span::of(decl),
                name_span: Span::of(name_node),
                detail,
                ty,
            },
        );
        // These take no doc comments.
        if !matches!(
            kind,
            SymbolKind::Alias
                | SymbolKind::LoopVariable
                | SymbolKind::Parameter
                | SymbolKind::Table
        ) {
            self.analysis.symbols[id].doc = doc_comment(decl, self.source);
        }
        id
    }

    /// Like [`Self::define_named`], detailed by all of `decl` on one line.
    fn define_declaration(
        &mut self,
        scope: ScopeId,
        kind: SymbolKind,
        decl: Node,
        name_node: Node,
        ty: Ty,
    ) -> SymbolId {
        let detail = collapse_whitespace(self.text(decl));
        self.define_named(scope, kind, decl, name_node, detail, ty)
    }

    /// The innermost symbol named `name` visible from `scope` that satisfies `test`.
    fn lookup_where(
        &self,
        name: &str,
        scope: ScopeId,
        test: impl Fn(&Symbol) -> bool,
    ) -> Option<SymbolId> {
        self.analysis
            .scope_chain(scope)
            .find_map(|scope| {
                self.named(scope, name)
                    .iter()
                    .copied()
                    .find(|&id| test(&self.analysis.symbols[id]))
            })
    }

    fn lookup(
        &self,
        name: &str,
        scope: ScopeId,
        kinds: &[SymbolKind],
    ) -> Option<SymbolId> {
        self.lookup_where(name, scope, |s| kinds.contains(&s.kind))
    }

    /// The edge type that declares `edge` with `WITH REVERSE_EDGE`, if any.
    fn forward_edge(&self, edge: &str, scope: ScopeId) -> Option<&str> {
        let id = self.lookup_where(edge, scope, |s| {
            s.kind == SymbolKind::EdgeType
                && s.detail.starts_with(REVERSE_EDGE_PREFIX)
        })?;
        self.analysis.symbols[id]
            .detail
            .strip_prefix(REVERSE_EDGE_PREFIX)
    }

    fn ty_of(&self, id: Option<SymbolId>) -> Ty {
        id.map(|id| self.analysis.symbols[id].ty.clone())
            .unwrap_or_default()
    }

    /// An attribute, tuple field or temp column named `name` of `owner`.
    fn lookup_owned_by(
        &self,
        name: &str,
        scope: ScopeId,
        kind: SymbolKind,
        owner: &str,
    ) -> Option<SymbolId> {
        self.lookup_where(name, scope, |s| {
            s.kind == kind && s.owner.as_deref() == Some(owner)
        })
    }

    // ------------------------------------------------------------------
    // Pass 1: declarations
    // ------------------------------------------------------------------

    fn declare_children(&mut self, node: Node) {
        for child in syntax::code_children(node) {
            self.declare(child);
        }
    }

    fn declare(&mut self, node: Node) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        self.depth += 1;
        self.declare_node(node);
        self.depth -= 1;
    }

    fn declare_node(&mut self, node: Node) {
        match node.kind_id() {
            kind::ERROR => {
                self.error_depth += 1;
                self.declare_children(node);
                self.error_depth -= 1;
            }
            kind::QUERY_DEFINITION | kind::OPENCYPHER_QUERY_DEFINITION => {
                self.declare_query(node)
            }
            kind::INTERPRET_QUERY_STATEMENT => {
                if let Some(body) = node.child_by_field_id(field::BODY) {
                    self.push(ScopeKind::Query, node);
                    self.declare_parameters(node);
                    self.declare(body);
                    self.pop();
                }
            }
            kind::VERTEX_DEFINITION => self.declare_vertex_type(node),
            kind::EDGE_DEFINITION => self.declare_edge_type(node, FILE_SCOPE),
            kind::DROP_STATEMENT => self.record_drop(node),
            // A virtual edge type exists only while its query runs.
            kind::VIRTUAL_EDGE_DECLARATION => {
                let scope = self.query_or_current();
                self.declare_edge_type(node, scope);
            }
            kind::GRAPH_DEFINITION => self.declare_graph(node),
            kind::TYPEDEF_STATEMENT => self.declare_typedef(node),
            kind::LOADING_JOB_DEFINITION => {
                self.declare_job(node, SymbolKind::LoadingJob)
            }
            kind::SCHEMA_CHANGE_JOB_DEFINITION => {
                self.declare_job(node, SymbolKind::SchemaChangeJob)
            }
            kind::ALTER_TYPE_STATEMENT => self.declare_added_attributes(node),
            kind::DATA_SOURCE_DEFINITION
            | kind::PACKAGE_DEFINITION
            | kind::DEFINE_FILENAME_STATEMENT
            | kind::DEFINE_HEADER_STATEMENT
            | kind::DEFINE_INPUT_LINE_FILTER_STATEMENT
            | kind::EXCEPTION_DECLARATION => {
                let (scope, kind) = match node.kind_id() {
                    kind::DATA_SOURCE_DEFINITION => {
                        (FILE_SCOPE, SymbolKind::DataSource)
                    }
                    kind::PACKAGE_DEFINITION => {
                        (FILE_SCOPE, SymbolKind::Package)
                    }
                    kind::DEFINE_FILENAME_STATEMENT => {
                        (self.current_scope(), SymbolKind::FilenameVariable)
                    }
                    kind::DEFINE_HEADER_STATEMENT => {
                        (self.current_scope(), SymbolKind::Header)
                    }
                    kind::DEFINE_INPUT_LINE_FILTER_STATEMENT => {
                        (self.current_scope(), SymbolKind::LineFilter)
                    }
                    _ => (self.query_or_current(), SymbolKind::Exception),
                };
                if let Some(name) = node.child_by_field_id(field::NAME) {
                    self.define_declaration(
                        scope,
                        kind,
                        node,
                        name,
                        Ty::Unknown,
                    );
                }
            }
            kind::LOAD_DESTINATION => {
                let is_temp_table = kind_of(node) == Some("TEMP_TABLE");
                if let (true, Some(target)) =
                    (is_temp_table, node.child_by_field_id(field::TARGET))
                {
                    let name = self.text(target);
                    let scope = self.current_scope();
                    if self
                        .lookup(name, scope, &[SymbolKind::TempTable])
                        .is_none()
                    {
                        let detail =
                            format!("TEMP_TABLE {}", self.text(target));
                        self.define_named(
                            scope,
                            SymbolKind::TempTable,
                            node,
                            target,
                            detail,
                            Ty::Unknown,
                        );
                        // The columns are declared with the table; a repeated `TO TEMP_TABLE t` adds none.
                        if let Some(columns) =
                            node.child_by_field_id(field::COLUMNS)
                        {
                            let table = self.text(target).to_string();
                            for column in syntax::code_children(columns)
                                .into_iter()
                                .filter(|c| c.kind_id() == kind::IDENTIFIER)
                            {
                                let detail = format!(
                                    "{} (column of TEMP_TABLE {table})",
                                    self.text(column)
                                );
                                let id = self.define_named(
                                    scope,
                                    SymbolKind::TempColumn,
                                    column,
                                    column,
                                    detail,
                                    Ty::Unknown,
                                );
                                self.analysis.symbols[id].owner =
                                    Some(table.clone());
                            }
                        }
                    }
                }
                self.declare_children(node);
            }
            kind::ACCUMULATOR_DECLARATION => self.declare_accumulators(node),
            kind::VARIABLE_DECLARATION => self.declare_variables(node),
            kind::FILE_DECLARATION => {
                if let Some(name) = node.child_by_field_id(field::NAME) {
                    let scope = self.current_scope();
                    let detail = format!("FILE {}", self.text(name));
                    self.define_named(
                        scope,
                        SymbolKind::File,
                        node,
                        name,
                        detail,
                        Ty::File,
                    );
                }
            }
            kind::VERTEX_SET_DECLARATION => {
                if let Some(value) = node.child_by_field_id(field::VALUE) {
                    self.declare(value);
                }
                if let Some(name) = node.child_by_field_id(field::NAME) {
                    // `Seeds (A|B) = ...` has several `type` children.
                    let mut types = Vec::new();
                    let mut ty_name = String::new();
                    for t in syntax::children_by_field(node, "type") {
                        let text = self.text(t);
                        if !ty_name.is_empty() {
                            ty_name.push('|');
                        }
                        ty_name.push_str(text);
                        if t.kind_id() == kind::IDENTIFIER {
                            types.push(text.to_string());
                        }
                    }
                    if ty_name.is_empty() {
                        ty_name.push_str("ANY");
                    }
                    let scope = self.query_or_current();
                    let detail = format!("{} ({ty_name})", self.text(name));
                    self.define_named(
                        scope,
                        SymbolKind::VertexSet,
                        node,
                        name,
                        detail,
                        Ty::VertexSet(types),
                    );
                }
            }
            kind::ASSIGNMENT_STATEMENT => self.declare_assignment(node),
            kind::SELECT_STATEMENT => self.declare_select(node),
            kind::DELETE_STATEMENT | kind::UPDATE_STATEMENT => {
                self.push(ScopeKind::Block, node);
                if let Some(from) = syntax::child_of_kind(node, "from_clause")
                {
                    self.declare(from);
                }
                for child in syntax::code_children(node) {
                    if child.kind_id() != kind::FROM_CLAUSE {
                        self.declare(child);
                    }
                }
                self.pop();
            }
            kind::FOREACH_STATEMENT => self.declare_foreach(node),
            // IF/WHILE/CASE/TRY bodies are scopes.
            kind::BLOCK => {
                self.push(ScopeKind::Block, node);
                self.declare_children(node);
                self.pop();
            }
            kind::VERTEX_PATTERN => self.declare_vertex_alias(node),
            kind::EDGE_PATTERN => self.declare_edge_alias(node),
            kind::NODE_PATTERN => self.declare_node_alias(node),
            kind::RELATIONSHIP_DETAIL => {
                self.declare_relationship_alias(node)
            }
            _ => self.declare_children(node),
        }
    }

    fn declare_query(&mut self, node: Node) {
        let Some(name_node) = node.child_by_field_id(field::NAME) else {
            return self.declare_children(node);
        };
        let detail = header_detail(node, self.source);
        let id = self.define_named(
            FILE_SCOPE,
            SymbolKind::Query,
            node,
            name_node,
            detail,
            Ty::Unknown,
        );
        let symbol = &mut self.analysis.symbols[id];
        symbol.strict_create = is_strict_create(node);
        symbol.graph = for_graph(node, self.source);
        symbol.params = parameters(node, self.source);
        if let Some(returns) = syntax::child_of_kind(node, "returns_clause")
            .and_then(|r| r.child_by_field_id(field::TYPE))
        {
            symbol.ty = type_of_type_node(returns, self.source);
            symbol.returns =
                Some(collapse_whitespace(syntax::text(returns, self.source)));
        }
        self.push(ScopeKind::Query, node);
        self.declare_parameters(node);
        if let Some(body) = node.child_by_field_id(field::BODY) {
            self.declare(body);
        }
        self.pop();
    }

    fn declare_parameters(&mut self, node: Node) {
        let Some(list) = node.child_by_field_id(field::PARAMETERS) else {
            return;
        };
        let scope = self.current_scope();
        for parameter in syntax::code_children(list) {
            if parameter.kind_id() != kind::PARAMETER {
                continue;
            }
            let (Some(name), Some(ty)) = (
                parameter.child_by_field_id(field::NAME),
                parameter.child_by_field_id(field::TYPE),
            ) else {
                continue;
            };
            let ty = type_of_type_node(ty, self.source);
            self.define_declaration(
                scope,
                SymbolKind::Parameter,
                parameter,
                name,
                ty,
            );
        }
    }

    fn declare_vertex_type(&mut self, node: Node) {
        let Some(name) = node.child_by_field_id(field::NAME) else {
            return;
        };
        let type_name = self.text(name).to_string();
        let id = self.define_declaration(
            FILE_SCOPE,
            SymbolKind::VertexType,
            node,
            name,
            Ty::Vertex(vec![type_name.clone()]),
        );
        self.analysis.symbols[id].strict_create = is_strict_create(node);
        let id_as_attribute = has_primary_id_as_attribute(node, self.source);
        if let Some(list) = node.child_by_field_id(field::ATTRIBUTES) {
            for attribute in syntax::code_children(list) {
                if matches!(
                    attribute.kind_id(),
                    kind::PRIMARY_ID_DEFINITION | kind::ATTRIBUTE_DEFINITION
                ) {
                    let id = self
                        .declare_attribute(attribute, &type_name, FILE_SCOPE);
                    if let Some(id) = id.filter(|_| {
                        attribute.kind_id() == kind::PRIMARY_ID_DEFINITION
                            && !id_as_attribute
                    }) {
                        self.analysis.symbols[id].id_only = true;
                    }
                }
            }
        }
    }

    fn declare_attribute(
        &mut self,
        attribute: Node,
        owner: &str,
        scope: ScopeId,
    ) -> Option<SymbolId> {
        let (Some(name), Some(ty)) = (
            attribute.child_by_field_id(field::NAME),
            attribute.child_by_field_id(field::TYPE),
        ) else {
            return None;
        };
        let ty = type_of_type_node(ty, self.source);
        let id = self.define_declaration(
            scope,
            SymbolKind::Attribute,
            attribute,
            name,
            ty,
        );
        self.analysis.symbols[id].owner = Some(owner.to_string());
        Some(id)
    }

    fn declare_edge_type(&mut self, node: Node, scope: ScopeId) {
        let Some(name) = node.child_by_field_id(field::NAME) else {
            return;
        };
        let type_name = self.text(name).to_string();
        let id = self.define_declaration(
            scope,
            SymbolKind::EdgeType,
            node,
            name,
            Ty::Edge(vec![type_name.clone()]),
        );
        self.analysis.symbols[id].strict_create =
            scope == FILE_SCOPE && is_strict_create(node);
        let mut endpoints = Vec::new();
        let mut ends = EdgeEnds {
            directed: syntax::field_text(node, "direction", self.source)
                .is_some_and(|d| d.eq_ignore_ascii_case("directed")),
            ..EdgeEnds::default()
        };
        if let Some(list) = node.child_by_field_id(field::ATTRIBUTES) {
            for child in syntax::code_children(list) {
                match child.kind_id() {
                    kind::EDGE_PAIR => {
                        for field in ["from", "to"] {
                            for endpoint in
                                syntax::children_by_field(child, field)
                            {
                                let text = self.text(endpoint);
                                let side = if field == "from" {
                                    &mut ends.from
                                } else {
                                    &mut ends.to
                                };
                                push_unique(side, text.to_string());
                                push_unique(&mut endpoints, text.to_string());
                            }
                        }
                    }
                    kind::ATTRIBUTE_DEFINITION => {
                        self.declare_attribute(child, &type_name, scope);
                    }
                    kind::DISCRIMINATOR => {
                        for attribute in syntax::code_children(child) {
                            if attribute.kind_id()
                                == kind::ATTRIBUTE_DEFINITION
                            {
                                self.declare_attribute(
                                    attribute, &type_name, scope,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let symbol = &mut self.analysis.symbols[id];
        symbol.members = endpoints.clone();
        symbol.ends = ends.clone();
        // `WITH REVERSE_EDGE="name"` declares a second edge type.
        for option in syntax::code_children(node)
            .into_iter()
            .filter(|c| c.kind_id() == kind::WITH_CLAUSE)
            .flat_map(syntax::code_children)
        {
            let key =
                syntax::field_text(option, "key", self.source).unwrap_or("");
            let Some(value) = option.child_by_field_id(field::VALUE) else {
                continue;
            };
            if !key.eq_ignore_ascii_case("reverse_edge")
                || value.kind_id() != kind::STRING
            {
                continue;
            }
            let raw = self.text(value);
            let reverse = raw.trim_matches('"').to_string();
            if reverse.is_empty() {
                continue;
            }
            // Point at the name inside the quotes.
            let name_span = Span::new(
                value.start_byte() + 1,
                value.end_byte().saturating_sub(1),
            );
            let reverse_id = self.define_in(
                FILE_SCOPE,
                NewSymbol {
                    name: reverse.clone(),
                    kind: SymbolKind::EdgeType,
                    span: Span::of(option),
                    name_span,
                    detail: format!("{REVERSE_EDGE_PREFIX}{type_name}"),
                    ty: Ty::Edge(vec![reverse.clone()]),
                },
            );
            let symbol = &mut self.analysis.symbols[reverse_id];
            symbol.members = endpoints.clone();
            symbol.ends = EdgeEnds {
                from: ends.to.clone(),
                to: ends.from.clone(),
                directed: true,
            };
            // The reverse edge carries the same attributes.
            let attributes: Vec<Symbol> = self
                .analysis
                .symbols_owned_by(SymbolKind::Attribute, &type_name)
                .cloned()
                .collect();
            for mut attribute in attributes {
                attribute.owner = Some(reverse.clone());
                attribute.copied = true;
                let attribute_id = self.analysis.symbols.len();
                self.analysis.symbols.push(attribute);
                self.add_to_scope(FILE_SCOPE, attribute_id);
            }
        }
    }

    fn declare_graph(&mut self, node: Node) {
        let Some(name) = node.child_by_field_id(field::NAME) else {
            return;
        };
        let id = self.define_declaration(
            FILE_SCOPE,
            SymbolKind::Graph,
            node,
            name,
            Ty::Unknown,
        );
        let symbol = &mut self.analysis.symbols[id];
        symbol.strict_create = is_strict_create(node);
        symbol.members = field_list(node, "member", self.source);
    }

    fn declare_typedef(&mut self, node: Node) {
        let Some(name) = node.child_by_field_id(field::NAME) else {
            return;
        };
        let scope = self.query_or_current();
        if let Some(accumulator) = node.child_by_field_id(field::TYPE) {
            let ty = type_of_type_node(accumulator, self.source);
            self.define_declaration(
                scope,
                SymbolKind::AccumulatorType,
                node,
                name,
                ty,
            );
            return;
        }
        let tuple = self.text(name).to_string();
        self.define_declaration(
            scope,
            SymbolKind::TupleType,
            node,
            name,
            Ty::Tuple(tuple.clone()),
        );
        for field in syntax::code_children(node) {
            if field.kind_id() != kind::TUPLE_FIELD {
                continue;
            }
            let (Some(field_name), Some(ty)) = (
                field.child_by_field_id(field::NAME),
                field.child_by_field_id(field::TYPE),
            ) else {
                continue;
            };
            let ty = type_of_type_node(ty, self.source);
            let id = self.define_declaration(
                scope,
                SymbolKind::TupleField,
                field,
                field_name,
                ty,
            );
            self.analysis.symbols[id].owner = Some(tuple.clone());
        }
    }

    fn declare_job(&mut self, node: Node, kind: SymbolKind) {
        let mut job = None;
        if let Some(name) = node.child_by_field_id(field::NAME) {
            let detail = header_detail(node, self.source);
            let id = self.define_named(
                FILE_SCOPE,
                kind,
                node,
                name,
                detail,
                Ty::Unknown,
            );
            self.analysis.symbols[id].graph = for_graph(node, self.source);
            job = Some(id);
        }
        let scope = self.push(ScopeKind::Job, node);
        if let Some(body) = node.child_by_field_id(field::BODY) {
            self.declare(body);
        }
        self.pop();
        // The file variables a `RUN LOADING JOB .. USING` can name.
        if let (Some(job), SymbolKind::LoadingJob) = (job, kind) {
            let job_name = self.analysis.symbols[job].name.clone();
            let mut files = Vec::new();
            for &id in &self.analysis.scopes[scope].symbols {
                let symbol = &mut self.analysis.symbols[id];
                if symbol.kind == SymbolKind::FilenameVariable {
                    symbol.owner = Some(job_name.clone());
                    files.push(symbol.name.clone());
                }
            }
            self.analysis.symbols[job].members = files;
        }
    }

    /// Notes what a DROP removes (`DROP ALL` and `DROP GRAPH` remove everything).
    fn record_drop(&mut self, node: Node) {
        use SymbolKind as K;
        let kind = kind_of(node);
        let names: Vec<String> = syntax::children_by_field(node, "name")
            .into_iter()
            .map(|n| self.text(n).to_string())
            .collect();
        let everything = [K::VertexType, K::EdgeType, K::Graph, K::Query];
        let drops = &mut self.analysis.drops;
        let mut add =
            |kind: K, name: &str| drops.push((kind, name.to_string()));
        match kind {
            Some("VERTEX" | "EDGE") => {
                let kind = if kind == Some("VERTEX") {
                    K::VertexType
                } else {
                    K::EdgeType
                };
                names.iter().for_each(|n| add(kind, n));
            }
            Some("QUERY" | "FUNCTION") if names.is_empty() => {
                add(K::Query, "*")
            }
            Some("QUERY" | "FUNCTION") => {
                names.iter().for_each(|n| add(K::Query, n))
            }
            Some("GRAPH") | None => {
                everything.iter().for_each(|k| add(*k, "*"))
            }
            _ => {}
        }
    }

    fn declare_added_attributes(&mut self, node: Node) {
        let Some(owner) = node
            .child_by_field_id(field::NAME)
            .map(|n| self.text(n).to_string())
        else {
            return;
        };
        for attribute in syntax::code_children(node) {
            if attribute.kind_id() == kind::ATTRIBUTE_DEFINITION
                && let Some(id) =
                    self.declare_attribute(attribute, &owner, FILE_SCOPE)
            {
                self.analysis.symbols[id].altered = true;
            }
        }
        if syntax::has_child(node, "VECTOR")
            && syntax::has_child(node, "ADD")
            && let Some(name) = node.child_by_field_id(field::ATTRIBUTE)
        {
            let after_name = name.end_byte()..node.end_byte();
            let options = collapse_whitespace(
                self.source.get(after_name).unwrap_or(""),
            );
            let detail = format!("{} VECTOR {options}", self.text(name));
            let id = self.define_named(
                FILE_SCOPE,
                SymbolKind::Attribute,
                node,
                name,
                detail,
                Ty::Unknown,
            );
            let symbol = &mut self.analysis.symbols[id];
            symbol.owner = Some(owner);
            symbol.vector = true;
            symbol.altered = true;
        }
    }

    fn declare_accumulators(&mut self, node: Node) {
        let Some(type_node) = node.child_by_field_id(field::TYPE) else {
            return;
        };
        // Accumulators are block-scoped.
        let scope = self.current_scope();
        let ty = match type_node.kind_id() {
            // A TYPEDEF'd accumulator type.
            kind::TYPE_IDENTIFIER => self.ty_of(self.lookup(
                self.text(type_node),
                scope,
                &[SymbolKind::AccumulatorType],
            )),
            _ => type_of_type_node(type_node, self.source),
        };
        let type_text = collapse_whitespace(self.text(type_node));
        let is_static = syntax::has_child(node, "STATIC");
        let on_edges = if syntax::has_child(node, "EDGE") {
            " EDGE"
        } else {
            ""
        };
        for declarator in syntax::code_children(node) {
            if declarator.kind_id() != kind::ACCUMULATOR_DECLARATOR {
                continue;
            }
            let Some(name) = declarator.child_by_field_id(field::NAME) else {
                continue;
            };
            let kind = SymbolKind::accumulator(
                name.kind_id() == kind::GLOBAL_ACCUMULATOR,
            );
            let prefix = if is_static { "STATIC " } else { "" };
            let detail = format!(
                "{prefix}{type_text}{on_edges} {}",
                collapse_whitespace(self.text(declarator))
            );
            let id = self.define_named(
                scope,
                kind,
                declarator,
                name,
                detail,
                ty.clone(),
            );
            let symbol = &mut self.analysis.symbols[id];
            symbol.on_edges = !on_edges.is_empty();
            symbol.span = Span::of(node);
        }
        self.declare_children(node);
    }

    fn declare_variables(&mut self, node: Node) {
        let Some(type_node) = node.child_by_field_id(field::TYPE) else {
            return;
        };
        let ty = type_of_type_node(type_node, self.source);
        let type_text = collapse_whitespace(self.text(type_node));
        let scope = self.current_scope();
        for declarator in syntax::code_children(node) {
            if declarator.kind_id() != kind::VARIABLE_DECLARATOR {
                continue;
            }
            if let Some(value) = declarator.child_by_field_id(field::VALUE) {
                self.declare(value);
            }
            let Some(name) = declarator.child_by_field_id(field::NAME) else {
                continue;
            };
            let detail = format!("{type_text} {}", self.text(name));
            self.define_named(
                scope,
                SymbolKind::Variable,
                declarator,
                name,
                detail,
                ty.clone(),
            );
        }
    }

    fn declare_assignment(&mut self, node: Node) {
        let right = node.child_by_field_id(field::RIGHT);
        if let Some(right) = right {
            self.declare(right);
        }
        let Some(left) = node.child_by_field_id(field::LEFT) else {
            return;
        };
        if left.kind_id() != kind::IDENTIFIER {
            return self.declare(left);
        }
        let name = self.text(left);
        let scope = self.current_scope();
        if self
            .lookup(name, scope, LOCAL_VALUES)
            .is_some()
        {
            return;
        }
        // First assignment declares a vertex set.
        let ty = right
            .map(|r| self.expression_type(r, scope))
            .unwrap_or_default();
        let (kind, ty) = match ty {
            Ty::VertexSet(types) | Ty::Vertex(types) => {
                (SymbolKind::VertexSet, Ty::VertexSet(types))
            }
            _ if right
                .is_some_and(|r| r.kind_id() == kind::SELECT_STATEMENT) =>
            {
                (SymbolKind::VertexSet, Ty::VertexSet(Vec::new()))
            }
            other => (SymbolKind::Variable, other),
        };
        let detail = match &ty {
            Ty::VertexSet(types) if !types.is_empty() => {
                format!("{name} ({})", types.join(" | "))
            }
            Ty::VertexSet(_) => format!("{name} (vertex set)"),
            other => format!("{name}: {}", other.display()),
        };
        let query_scope = self.query_or_current();
        let id = self.define_named(query_scope, kind, node, left, detail, ty);
        self.analysis.symbols[id].implicit = true;
    }

    fn declare_select(&mut self, node: Node) {
        let query_scope = self.query_or_current();
        let scope = self.push(ScopeKind::Block, node);
        self.select_scopes
            .entry(Span::of(node))
            .or_insert(scope);
        let children = syntax::code_children(node);
        // Aliases first: the result list and clauses refer to them.
        for child in children
            .iter()
            .filter(|c| c.kind_id() == kind::FROM_CLAUSE)
        {
            self.declare(*child);
        }
        // `SELECT COUNT(f) AS n ... HAVING n > 3`: result aliases name columns.
        for result in syntax::children_by_field(node, "result") {
            if let (Some(value), Some(alias)) = (
                result.child_by_field_id(field::VALUE),
                result.child_by_field_id(field::ALIAS),
            ) {
                let ty = self.expression_type(value, self.current_scope());
                self.declare_alias(result, alias, ty);
            }
        }
        for child in &children {
            match child.kind_id() {
                kind::FROM_CLAUSE => {}
                kind::INTO_CLAUSE => {
                    for table in syntax::children_by_field(*child, "table") {
                        if self
                            .lookup(
                                self.text(table),
                                query_scope,
                                &[SymbolKind::Table],
                            )
                            .is_none()
                        {
                            let detail =
                                format!("table {}", self.text(table));
                            self.define_named(
                                query_scope,
                                SymbolKind::Table,
                                *child,
                                table,
                                detail,
                                Ty::Unknown,
                            );
                        }
                    }
                }
                _ => self.declare(*child),
            }
        }
        self.pop();
    }

    fn declare_foreach(&mut self, node: Node) {
        let collection = node.child_by_field_id(field::COLLECTION);
        if let Some(collection) = collection {
            self.declare(collection);
        }
        let collection_ty = collection
            .map(|c| self.iterable_type(c, self.current_scope()))
            .unwrap_or_default();
        self.push(ScopeKind::Block, node);
        let scope = self.current_scope();
        match node.child_by_field_id(field::VARIABLE) {
            Some(variable) if variable.kind_id() == kind::IDENTIFIER => {
                let ty = collection_ty.element();
                let detail =
                    format!("{}: {}", self.text(variable), ty.display());
                self.define_named(
                    scope,
                    SymbolKind::LoopVariable,
                    node,
                    variable,
                    detail,
                    ty,
                );
            }
            Some(variables) => {
                let names: Vec<Node> = syntax::code_children(variables);
                let (key, value) =
                    collection_ty.key_value().unwrap_or_default();
                for (index, variable) in names.into_iter().enumerate() {
                    let ty = match index {
                        0 => key.clone(),
                        1 => value.clone(),
                        _ => Ty::Unknown,
                    };
                    let detail =
                        format!("{}: {}", self.text(variable), ty.display());
                    self.define_named(
                        scope,
                        SymbolKind::LoopVariable,
                        node,
                        variable,
                        detail,
                        ty,
                    );
                }
            }
            None => {}
        }
        if let Some(body) = node.child_by_field_id(field::BODY) {
            self.declare(body);
        }
        self.pop();
    }

    /// Type of a FOREACH iterable.
    fn iterable_type(&self, node: Node, scope: ScopeId) -> Ty {
        match node.kind_id() {
            kind::RANGE_EXPRESSION => Ty::Collection(
                "LIST".into(),
                vec![Ty::Primitive("INT".into())],
            ),
            kind::LIST_LITERAL | kind::TUPLE => {
                let mut element: Option<Ty> = None;
                for item in syntax::code_children(node) {
                    let Some(primitive) = literal_type(item.kind()) else {
                        return Ty::Unknown;
                    };
                    let ty = Ty::Primitive(primitive.into());
                    if element.as_ref().is_some_and(|e| *e != ty) {
                        return Ty::Unknown;
                    }
                    element = Some(ty);
                }
                element.map_or(Ty::Unknown, |e| {
                    Ty::Collection("LIST".into(), vec![e])
                })
            }
            _ => self.expression_type(node, scope),
        }
    }

    fn declare_alias(&mut self, decl: Node, alias: Node, ty: Ty) {
        let scope = self.current_scope();
        let name = self.text(alias);
        // openCypher patterns may mention an alias again: `(a)-[]-(b), (b)-[]-(c)`.
        if self
            .lookup(name, scope, &[SymbolKind::Alias])
            .is_some_and(|id| self.analysis.symbols[id].scope == scope)
        {
            return;
        }
        let detail = format!("{name}: {}", ty.display());
        self.define_named(scope, SymbolKind::Alias, decl, alias, detail, ty);
    }

    fn declare_vertex_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_id(field::ALIAS) else {
            return;
        };
        let scope = self.current_scope();
        let types = match node.child_by_field_id(field::TYPE) {
            Some(t) => self.vertex_source_types(t, scope),
            None => self
                .edge_step_target(node, scope)
                .unwrap_or_default(),
        };
        self.declare_alias(node, alias, Ty::Vertex(types));
    }

    /// Vertex types at the far end of a schema edge; `None` if unknown or wildcard.
    fn edge_end_types(
        &self,
        edge: &str,
        outgoing: bool,
        incoming: bool,
        scope: ScopeId,
    ) -> Option<Vec<String>> {
        self.uses_schema_edges.set(true);
        let workspace = self.workspace?;
        // A variable of that name is not a schema edge.
        if self
            .lookup(edge, scope, EDGE_SOURCE_VALUES)
            .is_some()
        {
            return None;
        }
        let declarations = workspace.find(SymbolKind::EdgeType, edge);
        if declarations.is_empty() {
            return None;
        }
        let mut types: Vec<String> = Vec::new();
        for declaration in declarations {
            let ends = &declaration.ends;
            if ends.from.is_empty()
                || ends.to.is_empty()
                || ends
                    .from
                    .iter()
                    .chain(&ends.to)
                    .any(|t| t == "*")
            {
                return None;
            }
            let sides: &[&[String]] =
                match (ends.directed, outgoing, incoming) {
                    (true, true, false) => &[&ends.to],
                    (true, false, true) => &[&ends.from],
                    _ => &[&ends.from, &ends.to],
                };
            extend_unique(
                &mut types,
                sides.iter().copied().flatten().cloned(),
            );
        }
        Some(types)
    }

    /// Type of `:m` in `-(Edge>)- :m`.
    fn edge_step_target(
        &self,
        node: Node,
        scope: ScopeId,
    ) -> Option<Vec<String>> {
        let step = node
            .parent()
            .filter(|p| p.kind_id() == kind::EDGE_STEP)?;
        let pattern = syntax::child_of_kind(step, "edge_pattern")?;
        let closes_directed = syntax::has_child(step, "->");
        let spec = pattern.child_by_field_id(field::TYPE)?;
        let atoms: Vec<Node> = match spec.kind_id() {
            kind::EDGE_ATOM => vec![spec],
            kind::EDGE_ALTERNATION => {
                let atoms = syntax::code_children(spec);
                if atoms
                    .iter()
                    .any(|a| a.kind_id() != kind::EDGE_ATOM)
                {
                    return None;
                }
                atoms
            }
            _ => return None,
        };
        let mut types: Vec<String> = Vec::new();
        for atom in atoms {
            let name = atom
                .child_by_field_id(field::NAME)
                .filter(|n| n.kind_id() == kind::IDENTIFIER)?;
            let mut cursor = atom.walk();
            let (first, last) = first_last_kinds(atom.children(&mut cursor));
            let incoming = first == Some("<");
            let outgoing =
                last == Some(">") || (closes_directed && !incoming);
            extend_unique(
                &mut types,
                self.edge_end_types(
                    self.text(name),
                    outgoing,
                    incoming,
                    scope,
                )?,
            );
        }
        Some(types)
    }

    /// Type of `b` in `(a)-[:Edge]->(b)`.
    fn relationship_target(
        &self,
        node: Node,
        scope: ScopeId,
    ) -> Option<Vec<String>> {
        let relationship = syntax::prev_non_comment_sibling(node)
            .filter(|p| p.kind_id() == kind::RELATIONSHIP_PATTERN)?;
        let mut cursor = relationship.walk();
        let (first, last) =
            first_last_kinds(relationship.children(&mut cursor));
        let incoming = first == Some("<-");
        let outgoing = last == Some("->");
        let detail =
            syntax::child_of_kind(relationship, "relationship_detail")?;
        // `*1..2` reaches any distance.
        if syntax::has_child(detail, "*")
            || syntax::has_child(detail, "repetition_bounds")
        {
            return None;
        }
        let names = self.relationship_types(detail);
        if names.is_empty() {
            return None;
        }
        let mut types: Vec<String> = Vec::new();
        for name in names {
            extend_unique(
                &mut types,
                self.edge_end_types(&name, outgoing, incoming, scope)?,
            );
        }
        Some(types)
    }

    /// Vertex types denoted by the vertex position of a FROM pattern.
    fn vertex_source_types(&self, node: Node, scope: ScopeId) -> Vec<String> {
        match node.kind_id() {
            kind::IDENTIFIER => {
                let name = self.text(node);
                match self.lookup(name, scope, VERTEX_SOURCE_VALUES) {
                    Some(id) => self.analysis.symbols[id]
                        .ty
                        .member_vertex_types(),
                    None => vec![name.to_string()],
                }
            }
            kind::GLOBAL_ACCUMULATOR => self
                .lookup(
                    self.text(node),
                    scope,
                    &[SymbolKind::GlobalAccumulator],
                )
                .map(|id| {
                    self.analysis.symbols[id]
                        .ty
                        .member_vertex_types()
                })
                .unwrap_or_default(),
            kind::VERTEX_TYPE_ALTERNATION => self
                .vertex_alternation_types(syntax::code_children(node), scope),
            _ => Vec::new(),
        }
    }

    /// Vertex types of an alternation (`Person|S`); empty (any) when one is untyped.
    fn vertex_alternation_types(
        &self,
        labels: Vec<Node>,
        scope: ScopeId,
    ) -> Vec<String> {
        let mut types = Vec::new();
        for label in labels {
            let more = self.vertex_source_types(label, scope);
            if more.is_empty() {
                return Vec::new();
            }
            extend_unique(&mut types, more);
        }
        types
    }

    fn declare_edge_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_id(field::ALIAS) else {
            return;
        };
        let scope = self.current_scope();
        let mut types = Vec::new();
        let mut any = false;
        if let Some(pattern) = node.child_by_field_id(field::TYPE) {
            syntax::walk(pattern, |n| {
                if n.kind_id() == kind::EDGE_ATOM {
                    match n.child_by_field_id(field::NAME) {
                        Some(name) if name.kind_id() == kind::IDENTIFIER => {
                            let text = self.text(name);
                            if self
                                .lookup(text, scope, EDGE_SOURCE_VALUES)
                                .is_none()
                            {
                                push_unique(&mut types, text.to_string());
                            } else if !self.held_edge_types(name, &mut types)
                            {
                                any = true;
                            }
                        }
                        Some(name)
                            if name.kind_id() == kind::GLOBAL_ACCUMULATOR =>
                        {
                            if !self.held_edge_types(name, &mut types) {
                                any = true;
                            }
                        }
                        _ => any = true,
                    }
                }
            });
        }
        if any {
            types.clear();
        }
        self.declare_alias(node, alias, Ty::Edge(types));
    }

    /// Appends resolved string literals for `name`; returns false if unknown.
    fn held_edge_types(&self, name: Node, types: &mut Vec<String>) -> bool {
        match flow::string_values(self.text(name), name, self.source) {
            Some(held) => {
                for edge in held {
                    push_unique(types, edge);
                }
                true
            }
            None => false,
        }
    }

    fn declare_node_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_id(field::ALIAS) else {
            return;
        };
        let types = self.node_pattern_types(node, self.current_scope());
        self.declare_alias(node, alias, Ty::Vertex(types));
    }

    /// Vertex types of a node pattern: its labels, else the far end of the
    /// edge before it.
    fn node_pattern_types(&self, node: Node, scope: ScopeId) -> Vec<String> {
        if node.child_by_field_id(field::TYPE).is_none() {
            return self
                .relationship_target(node, scope)
                .unwrap_or_default();
        }
        self.vertex_alternation_types(
            syntax::children_by_field(node, "type"),
            scope,
        )
    }

    fn declare_relationship_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_id(field::ALIAS) else {
            return;
        };
        let types = self.relationship_types(node);
        self.declare_alias(node, alias, Ty::Edge(types));
    }

    /// Edge types of a `[e:A|B]` relationship: empty (any edge) when a label
    /// is `_` or an accumulator.
    fn relationship_types(&self, detail: Node) -> Vec<String> {
        syntax::children_by_field(detail, "type")
            .into_iter()
            .map(|t| {
                (t.kind_id() == kind::IDENTIFIER)
                    .then(|| self.text(t).to_string())
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    }

    /// Best-effort static type of an expression (pass 1 and 2 share it).
    fn expression_type(&self, node: Node, scope: ScopeId) -> Ty {
        match node.kind_id() {
            kind::IDENTIFIER => self.ty_of(self.resolve_name(
                self.text(node),
                &Role::Value,
                scope,
            )),
            kind @ (kind::GLOBAL_ACCUMULATOR | kind::LOCAL_ACCUMULATOR) => {
                let accumulator =
                    SymbolKind::accumulator(kind == kind::GLOBAL_ACCUMULATOR);
                self.ty_of(self.lookup(
                    self.text(node),
                    scope,
                    &[accumulator],
                ))
            }
            kind::MEMBER_EXPRESSION => {
                let Some(property) = node.child_by_field_id(field::PROPERTY)
                else {
                    return Ty::Unknown;
                };
                if property.kind_id() == kind::LOCAL_ACCUMULATOR {
                    return self.expression_type(property, scope);
                }
                let object_ty = node
                    .child_by_field_id(field::OBJECT)
                    .map(|o| self.expression_type(o, scope))
                    .unwrap_or_default();
                match object_ty {
                    Ty::Tuple(tuple) => self.ty_of(self.lookup_owned_by(
                        self.text(property),
                        scope,
                        SymbolKind::TupleField,
                        &tuple,
                    )),
                    _ => Ty::Unknown,
                }
            }
            kind::CALL_EXPRESSION => {
                let Some(function) = node.child_by_field_id(field::FUNCTION)
                else {
                    return Ty::Unknown;
                };
                match function.kind_id() {
                    kind::IDENTIFIER => {
                        let name = self.text(function);
                        if let Some(id) =
                            self.lookup(name, scope, &[SymbolKind::TupleType])
                        {
                            return self.analysis.symbols[id].ty.clone();
                        }
                        match name.to_ascii_lowercase().as_str() {
                            "to_vertex_set" | "selectvertex" => {
                                Ty::VertexSet(Vec::new())
                            }
                            "range" => Ty::Accumulator(
                                "ListAccum".into(),
                                vec![Ty::Primitive("INT".into())],
                            ),
                            "to_vertex" => Ty::Vertex(Vec::new()),
                            "parse_json_object" => {
                                Ty::Primitive("JSONOBJECT".into())
                            }
                            "parse_json_array" => {
                                Ty::Primitive("JSONARRAY".into())
                            }
                            "now" | "to_datetime" | "datetime_add"
                            | "datetime_sub" | "epoch_to_datetime" => {
                                Ty::Primitive("DATETIME".into())
                            }
                            _ => Ty::Unknown,
                        }
                    }
                    kind::MEMBER_EXPRESSION => {
                        let method = function
                            .child_by_field_id(field::PROPERTY)
                            .map(|p| self.text(p).to_ascii_lowercase())
                            .unwrap_or_default();
                        let object_ty = function
                            .child_by_field_id(field::OBJECT)
                            .map(|o| self.expression_type(o, scope))
                            .unwrap_or_default();
                        object_ty.method_result(&method)
                    }
                    _ => Ty::Unknown,
                }
            }
            kind::RANGE_EXPRESSION => Ty::Primitive("INT".into()),
            kind::PARENTHESIZED_EXPRESSION => syntax::code_children(node)
                .first()
                .map(|e| self.expression_type(*e, scope))
                .unwrap_or_default(),
            kind::SELECT_STATEMENT => {
                let select_scope = self
                    .select_scopes
                    .get(&Span::of(node))
                    .copied()
                    .unwrap_or(scope);
                let result = syntax::children_by_field(node, "result");
                match result.as_slice() {
                    [single] if single.kind_id() == kind::IDENTIFIER => {
                        match self.expression_type(*single, select_scope) {
                            Ty::Vertex(types) => Ty::VertexSet(types),
                            _ => Ty::VertexSet(Vec::new()),
                        }
                    }
                    _ => Ty::VertexSet(Vec::new()),
                }
            }
            kind::VERTEX_SET_LITERAL => {
                let mut types = Vec::new();
                for seed in syntax::code_children(node) {
                    let more = match seed.kind_id() {
                        kind::TYPE_WILDCARD => seed
                            .child_by_field_id(field::TYPE)
                            .map(|t| vec![self.text(t).to_string()])
                            .unwrap_or_default(),
                        _ => self
                            .expression_type(seed, scope)
                            .member_vertex_types(),
                    };
                    if more.is_empty() {
                        return Ty::VertexSet(Vec::new());
                    }
                    extend_unique(&mut types, more);
                }
                Ty::VertexSet(types)
            }
            kind::BINARY_EXPRESSION => {
                let operator = node
                    .child_by_field_id(field::OPERATOR)
                    .map(|o| o.kind())
                    .unwrap_or("");
                let left = node
                    .child_by_field_id(field::LEFT)
                    .map(|l| self.expression_type(l, scope));
                let right = node
                    .child_by_field_id(field::RIGHT)
                    .map(|r| self.expression_type(r, scope));
                match operator {
                    "UNION" | "INTERSECT" | "MINUS" => match (left, right) {
                        (
                            Some(Ty::VertexSet(mut a)),
                            Some(Ty::VertexSet(b)),
                        ) => {
                            if a.is_empty() || b.is_empty() {
                                return Ty::VertexSet(Vec::new());
                            }
                            extend_unique(&mut a, b);
                            Ty::VertexSet(a)
                        }
                        (Some(Ty::VertexSet(_)), _)
                        | (_, Some(Ty::VertexSet(_))) => {
                            Ty::VertexSet(Vec::new())
                        }
                        (Some(other), _) => other,
                        _ => Ty::Unknown,
                    },
                    _ => Ty::Unknown,
                }
            }
            _ => literal_type(node.kind())
                .map_or(Ty::Unknown, |t| Ty::Primitive(t.into())),
        }
    }

    // ------------------------------------------------------------------
    // Pass 2: references
    // ------------------------------------------------------------------

    fn resolve(&mut self, root: Node) {
        // Cursor walk with an ancestor stack: `Node::parent` is slow.
        let mut cursor = root.walk();
        let mut stack = vec![Frame {
            node: root,
            field: None,
            context: Context::Command,
            in_error: false,
        }];
        loop {
            let frame = stack.last().expect("non-empty");
            let (node, context, in_error) =
                (frame.node, frame.context, frame.in_error);
            match node.kind_id() {
                kind if syntax::is_name_kind(kind) => {
                    let path = Path { stack: &stack };
                    // Qualified names resolve as a whole.
                    if !path.parent().is_some_and(|p| {
                        p.node().kind_id() == kind::QUALIFIED_IDENTIFIER
                    }) {
                        self.resolve_occurrence(path, context, in_error);
                    }
                }
                kind::COLUMN_REFERENCE => self.column_occurrence(
                    Path { stack: &stack },
                    context,
                    in_error,
                ),
                _ => {}
            }
            if cursor.goto_first_child() {
                stack.push(Frame::enter(
                    &cursor,
                    context,
                    in_error,
                    stack.len(),
                ));
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    let depth = stack.len() - 1;
                    let parent = &stack[depth - 1];
                    stack[depth] = Frame::enter(
                        &cursor,
                        parent.context,
                        parent.in_error,
                        depth,
                    );
                    break;
                }
                if !cursor.goto_parent() {
                    return;
                }
                stack.pop();
            }
        }
    }

    fn resolve_occurrence(
        &mut self,
        path: Path,
        context: Context,
        in_error: bool,
    ) {
        let node = path.node();
        let span = Span::of(node);
        let name = self.text(node);
        let scope = self.analysis.scope_at(span.start);
        if let Some(&id) = self.declarations.get(&span) {
            let role = role_for_kind(self.analysis.symbols[id].kind);
            self.analysis.references.push(Reference {
                span,
                name: name.to_string(),
                role,
                scope,
                target: Some(id),
                declaration: true,
                write: true,
                context,
                qualifier: false,
                in_error,
            });
            return;
        }
        let Some(role) = self.role_of(path, scope) else {
            return;
        };
        let target = self.resolve_name(name, &role, scope);
        let write = is_write(path);
        let qualifier = path.field() == Some("object")
            && path.parent().is_some_and(|member| {
                member.node().kind_id() == kind::MEMBER_EXPRESSION
                    && member.field() == Some("function")
                    && member.parent().is_some_and(|call| {
                        call.node().kind_id() == kind::CALL_EXPRESSION
                    })
            });
        self.analysis.references.push(Reference {
            span,
            name: name.to_string(),
            role,
            scope,
            target,
            declaration: false,
            write,
            context,
            qualifier,
            in_error,
        });
    }

    fn resolve_name(
        &self,
        name: &str,
        role: &Role,
        scope: ScopeId,
    ) -> Option<SymbolId> {
        use SymbolKind as K;
        let global = || self.lookup(name, scope, role.global_kinds()?);
        match role {
            Role::Value => self
                .lookup(name, scope, LOCAL_VALUES)
                .or_else(global),
            Role::GlobalAccumulator => {
                self.lookup(name, scope, &[K::GlobalAccumulator])
            }
            Role::LocalAccumulator => {
                self.lookup(name, scope, &[K::LocalAccumulator])
            }
            Role::VertexSource => self
                .lookup(name, scope, VERTEX_SOURCE_VALUES)
                .or_else(|| self.lookup(name, scope, &[K::VertexType])),
            Role::EdgeSource => self
                .lookup(name, scope, EDGE_SOURCE_VALUES)
                .or_else(|| self.lookup(name, scope, &[K::EdgeType])),
            Role::VertexType => self.lookup(name, scope, &[K::VertexType]),
            Role::EdgeType => self.lookup(name, scope, &[K::EdgeType]),
            Role::Graph => self.lookup(name, scope, &[K::Graph]),
            Role::Query => self.lookup(name, scope, &[K::Query]),
            // INSERT INTO EDGE may name the type through a parameter.
            Role::SchemaType => self
                .lookup(name, scope, &[K::Parameter, K::Variable])
                .or_else(global),
            Role::Function | Role::Job | Role::TupleType => global(),
            Role::TupleField(tuple) => {
                self.lookup_owned_by(name, scope, K::TupleField, tuple)
            }
            Role::Exception => self.lookup(name, scope, &[K::Exception]),
            Role::JobLocal => self.lookup(
                name,
                scope,
                &[
                    K::FilenameVariable,
                    K::Header,
                    K::LineFilter,
                    K::TempTable,
                ],
            ),
            Role::Alias => self.lookup(name, scope, &[K::Alias]),
            Role::JobFile(job) => {
                self.analysis.symbols.iter().position(|s| {
                    s.kind == K::FilenameVariable
                        && s.name == name
                        && s.owner.as_deref() == Some(job.as_str())
                })
            }
            Role::TempColumn(table) => {
                self.lookup_owned_by(name, scope, K::TempColumn, table)
            }
            Role::Attribute(Ty::Tuple(tuple)) => {
                self.lookup_owned_by(name, scope, K::TupleField, tuple)
            }
            // Only resolve within this file when the owner is known. A reverse
            // edge has the attributes of its edge, added ones included.
            Role::Attribute(ty) => {
                ty.schema_owners()?
                    .1
                    .iter()
                    .find_map(|owner| {
                        let owner = self
                            .forward_edge(owner, scope)
                            .unwrap_or(owner);
                        self.lookup_owned_by(name, scope, K::Attribute, owner)
                    })
            }
            Role::Method(_) => None,
        }
    }

    /// `None` for names that refer to nothing (option keys, JSON keys, ...).
    fn role_of(&self, path: Path, scope: ScopeId) -> Option<Role> {
        let node = path.node();
        match node.kind_id() {
            kind::GLOBAL_ACCUMULATOR => {
                return Some(Role::GlobalAccumulator);
            }
            kind::LOCAL_ACCUMULATOR => {
                return Some(Role::LocalAccumulator);
            }
            _ => {}
        }
        let parent_path = path.parent()?;
        let parent = parent_path.node();
        let field = path.field();
        if node.kind_id() == kind::TYPE_IDENTIFIER {
            // `BitwiseOrAccum<len>`: the bit width may be a parameter.
            let bit_width = parent.kind_id() == kind::ACCUMULATOR_TYPE
                && syntax::field_text(parent, "kind", self.source)
                    .is_some_and(|k| {
                        starts_with_ignore_ascii_case(k, "bitwise")
                    });
            return Some(if bit_width {
                Role::Value
            } else {
                Role::TupleType
            });
        }
        let role = match (parent.kind(), field) {
            ("option_assignment", Some("key")) => {
                Role::JobFile(self.run_job_of(parent_path)?)
            }
            ("option_assignment", Some("value")) => {
                let in_using = parent_path.ancestors().any(|a| {
                    matches!(
                        a.kind_id(),
                        kind::USING_CLAUSE | kind::OPTION_CLAUSE
                    )
                });
                if in_using {
                    Role::JobLocal
                } else {
                    return None;
                }
            }
            ("aliased_expression", Some("alias")) => return None,
            ("pair", Some("key")) => {
                // `(p:Person {name: "Adam"})` constrains attributes of the pattern's types.
                let pattern = parent_path
                    .parent()
                    .filter(|p| p.node().kind_id() == kind::PROPERTY_MAP)?
                    .parent()?
                    .node();
                match pattern.kind_id() {
                    kind::NODE_PATTERN => Role::Attribute(Ty::Vertex(
                        self.node_pattern_types(pattern, scope),
                    )),
                    kind::RELATIONSHIP_DETAIL => Role::Attribute(Ty::Edge(
                        self.relationship_types(pattern),
                    )),
                    _ => return None,
                }
            }
            // Anonymous `TUPLE<INT a>` fields.
            ("tuple_field", Some("name")) => return None,
            ("graph_definition", Some("admin"))
            | ("tag_expression" | "tags_clause", _) => {
                return None;
            }
            ("graph_definition", Some("base")) => Role::Graph,
            ("post_accum_clause", Some("alias")) => Role::Alias,
            ("add_to_graph_statement", Some("name")) => {
                vertex_or_edge_type(parent)
            }
            ("add_to_graph_statement" | "drop_statement", Some("graph")) => {
                Role::Graph
            }
            ("alter_type_statement", Some("from" | "to")) => Role::VertexType,
            ("api_clause" | "syntax_clause", _) => return None,
            ("group_by_field", Some("name")) => return None,
            ("alter_type_statement", Some("index")) => {
                return None;
            }
            ("data_source_definition", Some("type" | "config")) => {
                return None;
            }
            ("tag_statement", _) => return None,
            // `SHOW JOB name`
            ("show_statement", None)
                if node.kind_id() == kind::IDENTIFIER
                    && kind_of(parent) == Some("JOB")
                    && !self.text(node).eq_ignore_ascii_case("all") =>
            {
                Role::Job
            }
            // `SHOW QUERY h`
            ("show_statement", None)
                if kind_of(parent) == Some("QUERY")
                    && self.is_shown_query(node) =>
            {
                Role::Query
            }
            (
                "show_statement" | "security_statement" | "shell_command"
                | "grant_statement" | "revoke_statement",
                _,
            ) => {
                return None;
            }
            // Names outside the schema.
            (
                "name_pattern"
                | "description_statement"
                | "row_policy_statement"
                | "data_source_grant_statement"
                | "install_function_statement",
                _,
            ) => return None,
            ("column_list", _) => return None,
            ("define_filename_statement", Some("path")) => {
                return None;
            }
            ("command_option", _) => return None,
            ("graph_definition", Some("member")) => Role::SchemaType,
            ("edge_pair", Some("from" | "to")) => Role::VertexType,
            ("primary_key_constraint", Some("attribute")) => {
                Role::Attribute(owner_type(parent_path, self.source))
            }
            ("discriminator", _) => {
                Role::Attribute(owner_type(parent_path, self.source))
            }
            ("vertex_type", Some("type")) => Role::VertexType,
            ("edge_type", Some("type")) => Role::EdgeType,
            ("sort_key", Some("name")) => {
                // `HeapAccum<T>(n, field DESC)`
                let tuple = parent_path
                    .ancestors()
                    .find(|a| a.kind_id() == kind::ACCUMULATOR_TYPE)
                    .and_then(|a| {
                        syntax::children_by_field(a, "argument")
                            .into_iter()
                            .next()
                    })
                    .filter(|a| a.kind_id() == kind::TYPE_IDENTIFIER)
                    .map(|a| self.text(a).to_string())?;
                Role::TupleField(tuple)
            }
            ("for_graph_clause", _)
            | ("use_statement", _)
            | ("alter_graph_statement", Some("name")) => Role::Graph,
            ("alter_graph_statement", Some("member")) => Role::SchemaType,
            ("install_query_statement" | "run_query_statement", _) => {
                Role::Query
            }
            ("interpret_query_statement", Some("name")) => Role::Query,
            ("run_job_statement", _) => Role::Job,
            ("drop_statement", Some("name")) => match kind_of(parent) {
                Some("VERTEX") => Role::VertexType,
                Some("EDGE") => Role::EdgeType,
                Some("GRAPH") => Role::Graph,
                Some("QUERY") | Some("FUNCTION") => Role::Query,
                Some("JOB") => Role::Job,
                Some("TUPLE") => Role::TupleType,
                _ => return None,
            },
            ("alter_type_statement", Some("name")) => {
                vertex_or_edge_type(parent)
            }
            ("alter_type_statement", Some("attribute")) => {
                let owner = syntax::field_text(parent, "name", self.source)
                    .unwrap_or("")
                    .to_string();
                match kind_of(parent) {
                    Some("EDGE") => Role::Attribute(Ty::Edge(vec![owner])),
                    _ => Role::Attribute(Ty::Vertex(vec![owner])),
                }
            }
            ("insert_statement", Some("target")) => Role::SchemaType,
            ("insert_columns", _) => {
                let insert = parent_path
                    .ancestors()
                    .find(|a| a.kind_id() == kind::INSERT_STATEMENT);
                // Columns cut off from their INSERT by a parse error belong to no type.
                Role::Attribute(insert.map_or_else(
                    || Ty::Vertex(vec![String::new()]),
                    |insert| insert_type(insert, self.source),
                ))
            }
            ("typed_value", Some("type")) => Role::VertexType,
            (
                "delete_statement"
                | "update_statement"
                | "dml_delete_statement",
                Some("alias"),
            ) => Role::Alias,
            ("vertex_pattern", Some("type"))
            | ("vertex_type_alternation", _) => Role::VertexSource,
            ("edge_atom", Some("name")) => Role::EdgeSource,
            ("node_pattern", Some("alias")) => Role::Alias,
            ("node_pattern", Some("type")) => Role::VertexSource,
            ("relationship_detail", Some("type")) => Role::EdgeType,
            ("type_wildcard", Some("type")) => Role::VertexType,
            ("vertex_set_declaration", Some("type")) => Role::VertexType,
            ("raise_statement" | "exception_handler", Some("exception")) => {
                Role::Exception
            }
            ("load_statement", Some("source")) => Role::JobLocal,
            ("load_destination", Some("target")) => match kind_of(parent) {
                Some("TEMP_TABLE") => Role::JobLocal,
                _ => vertex_or_edge_type(parent),
            },
            ("load_destination", Some("attribute")) => {
                let owner = syntax::field_text(parent, "target", self.source)
                    .unwrap_or("")
                    .to_string();
                Role::Attribute(Ty::Vertex(vec![owner]))
            }
            ("loading_delete_statement", Some("target")) => {
                vertex_or_edge_type(parent)
            }
            ("loading_delete_statement", Some("source")) => Role::JobLocal,
            ("member_expression", Some("property")) => {
                let object_ty = parent
                    .child_by_field_id(field::OBJECT)
                    .map(|o| self.expression_type(o, scope))
                    .unwrap_or_default();
                let is_call = parent_path.field() == Some("function")
                    && parent_path.parent().is_some_and(|g| {
                        g.node().kind_id() == kind::CALL_EXPRESSION
                    });
                if is_call {
                    Role::Method(object_ty)
                } else {
                    Role::Attribute(object_ty)
                }
            }
            // `LOG(cond, "msg");` is a statement, not `log(num)`.
            ("call_expression", Some("function"))
                if self.is_log_statement(parent_path, node) =>
            {
                return None;
            }
            ("call_expression", Some("function")) => Role::Function,
            // `SelectVertex(file, $0, Person, ...)`
            ("argument_list", None)
                if self.is_select_vertex_type(parent_path, node) =>
            {
                Role::VertexType
            }
            ("qualified_identifier", _) => return None,
            _ => {
                if node.kind_id() == kind::QUALIFIED_IDENTIFIER {
                    match parent.kind_id() {
                        kind::INSTALL_QUERY_STATEMENT
                        | kind::RUN_QUERY_STATEMENT
                        | kind::DROP_STATEMENT => Role::Query,
                        _ => return None,
                    }
                } else {
                    Role::Value
                }
            }
        };
        Some(role)
    }
}

impl Builder<'_> {
    /// The job of the `RUN LOADING JOB .. USING` holding `assignment`.
    fn run_job_of(&self, assignment: Path) -> Option<String> {
        let using = assignment.parent()?;
        let run = using.parent()?.node();
        let is_loading = run.kind_id() == kind::RUN_JOB_STATEMENT
            && using.node().kind_id() == kind::USING_CLAUSE
            && syntax::has_child(run, "LOADING");
        if !is_loading {
            return None;
        }
        syntax::field_text(run, "job", self.source).map(str::to_string)
    }

    /// A `$"col"` of a `LOAD TEMP_TABLE t` reads a column of `t`.
    fn column_occurrence(
        &mut self,
        path: Path,
        context: Context,
        in_error: bool,
    ) {
        let node = path.node();
        let text = self.text(node);
        let Some(inner) = text
            .strip_prefix("$\"")
            .and_then(|t| t.strip_suffix('"'))
        else {
            return;
        };
        if inner.is_empty() || inner.contains('"') {
            return;
        }
        let Some(load) = path
            .ancestors()
            .find(|a| a.kind_id() == kind::LOAD_STATEMENT)
        else {
            return;
        };
        if !syntax::has_child(load, "TEMP_TABLE") {
            return;
        }
        let Some(table) = syntax::field_text(load, "source", self.source)
        else {
            return;
        };
        let inner = inner.to_string();
        let table = table.to_string();
        let span = Span {
            start: node.start_byte() + 2,
            end: node.end_byte() - 1,
        };
        let scope = self.analysis.scope_at(span.start);
        let role = Role::TempColumn(table);
        let target = self.resolve_name(&inner, &role, scope);
        self.analysis.references.push(Reference {
            span,
            name: inner,
            role,
            scope,
            target,
            declaration: false,
            write: false,
            context,
            qualifier: false,
            in_error,
        });
    }

    fn is_log_statement(&self, call: Path, node: Node) -> bool {
        if !self.text(node).eq_ignore_ascii_case("log")
            || call.parent().is_none_or(|p| {
                p.node().kind_id() != kind::EXPRESSION_STATEMENT
            })
        {
            return false;
        }
        let Some(arguments) = call
            .node()
            .child_by_field_id(field::ARGUMENTS)
        else {
            return false;
        };
        let args = syntax::code_children(arguments);
        args.len() >= 2
            || args
                .first()
                .is_some_and(|a| a.kind_id() == kind::BOOLEAN)
    }

    /// A name in `SHOW QUERY a, b` (same line, not `ALL`).
    fn is_shown_query(&self, node: Node) -> bool {
        node.kind_id() == kind::IDENTIFIER
            && !self.text(node).eq_ignore_ascii_case("all")
            && node.prev_sibling().is_some_and(|p| {
                p.kind() == ","
                    || p.kind_id() == keyword::QUERY
                        && p.end_position().row == node.start_position().row
            })
    }

    fn is_select_vertex_type(&self, arguments: Path, node: Node) -> bool {
        let is_select_vertex = arguments
            .parent()
            .filter(|call| call.node().kind_id() == kind::CALL_EXPRESSION)
            .and_then(|call| {
                call.node()
                    .child_by_field_id(field::FUNCTION)
            })
            .is_some_and(|function| {
                self.text(function)
                    .eq_ignore_ascii_case("selectvertex")
            });
        is_select_vertex
            && syntax::code_children(arguments.node())
                .into_iter()
                .position(|argument| argument.id() == node.id())
                == Some(2)
    }
}

/// The keyword in the `kind` field of a statement (`VERTEX`, `EDGE`, `QUERY`, ...).
fn kind_of<'t>(node: Node<'t>) -> Option<&'t str> {
    node.child_by_field_id(field::KIND)
        .map(|k| k.kind())
}

/// The type a statement names: an edge type after `EDGE`, else a vertex type.
fn vertex_or_edge_type(statement: Node) -> Role {
    match kind_of(statement) {
        Some("EDGE") => Role::EdgeType,
        _ => Role::VertexType,
    }
}

/// The role a declaration occurrence plays (used for semantic tokens).
fn role_for_kind(kind: SymbolKind) -> Role {
    match kind {
        SymbolKind::GlobalAccumulator => Role::GlobalAccumulator,
        SymbolKind::LocalAccumulator => Role::LocalAccumulator,
        SymbolKind::VertexType => Role::VertexType,
        SymbolKind::EdgeType => Role::EdgeType,
        SymbolKind::Graph => Role::Graph,
        SymbolKind::Query => Role::Query,
        SymbolKind::LoadingJob | SymbolKind::SchemaChangeJob => Role::Job,
        SymbolKind::TupleType => Role::TupleType,
        SymbolKind::Exception => Role::Exception,
        SymbolKind::Attribute => Role::Attribute(Ty::Unknown),
        SymbolKind::TempColumn => Role::TempColumn(String::new()),
        _ => Role::Value,
    }
}

/// A node on the pass 2 stack: its field in the parent, context and error state.
struct Frame<'t> {
    node: Node<'t>,
    field: Option<&'t str>,
    context: Context,
    in_error: bool,
}

impl<'t> Frame<'t> {
    fn enter(
        cursor: &tree_sitter::TreeCursor<'t>,
        context: Context,
        in_error: bool,
        depth: usize,
    ) -> Self {
        let node = cursor.node();
        Frame {
            node,
            field: cursor.field_name(),
            context: enter_at(context, node, depth),
            in_error: in_error || node.is_error(),
        }
    }
}

/// A node and its ancestors (root first).
#[derive(Clone, Copy)]
struct Path<'a, 't> {
    stack: &'a [Frame<'t>],
}

impl<'a, 't> Path<'a, 't> {
    fn node(&self) -> Node<'t> {
        self.stack[self.stack.len() - 1].node
    }

    fn field(&self) -> Option<&'t str> {
        self.stack[self.stack.len() - 1].field
    }

    fn parent(&self) -> Option<Path<'a, 't>> {
        (self.stack.len() > 1).then(|| Path {
            stack: &self.stack[..self.stack.len() - 1],
        })
    }

    /// Innermost first, excluding the node.
    fn ancestors(&self) -> impl Iterator<Item = Node<'t>> + 'a {
        self.stack[..self.stack.len() - 1]
            .iter()
            .rev()
            .map(|f| f.node)
    }
}

/// The vertex or edge type that encloses an attribute list item.
fn owner_type(path: Path, source: &str) -> Ty {
    for ancestor in std::iter::once(path.node()).chain(path.ancestors()) {
        match ancestor.kind_id() {
            kind::VERTEX_DEFINITION => {
                return Ty::Vertex(
                    syntax::field_text(ancestor, "name", source)
                        .into_iter()
                        .map(String::from)
                        .collect(),
                );
            }
            kind::EDGE_DEFINITION | kind::VIRTUAL_EDGE_DECLARATION => {
                return Ty::Edge(
                    syntax::field_text(ancestor, "name", source)
                        .into_iter()
                        .map(String::from)
                        .collect(),
                );
            }
            // `INSERT INTO e (FROM, TO, DISCRIMINATOR(ts))`
            kind::INSERT_STATEMENT => {
                return insert_type(ancestor, source);
            }
            _ => {}
        }
    }
    Ty::Unknown
}

/// The type an `INSERT INTO` writes: an edge when it says `EDGE` or its
/// columns name the `FROM` endpoint, else a vertex.
fn insert_type(insert: Node, source: &str) -> Ty {
    let target = vec![
        syntax::field_text(insert, "target", source)
            .unwrap_or("")
            .to_string(),
    ];
    let edge = kind_of(insert) == Some("EDGE")
        || insert
            .child_by_field_id(field::COLUMNS)
            .is_some_and(|columns| syntax::has_child(columns, "FROM"));
    if edge {
        Ty::Edge(target)
    } else {
        Ty::Vertex(target)
    }
}

/// Like [`enter`]; contexts only change at depth <= 3.
fn enter_at(outer: Context, node: Node, depth: usize) -> Context {
    if depth > 3 {
        outer
    } else {
        enter(outer, node.kind())
    }
}

/// The context of a node of kind `kind` whose parent is in `outer`.
fn enter(outer: Context, kind: &str) -> Context {
    match (outer, kind) {
        // Schema edits win over everything they contain.
        (
            _,
            "schema_change_job_definition"
            | "drop_statement"
            | "alter_type_statement"
            | "alter_graph_statement",
        ) => Context::SchemaEdit,
        (Context::SchemaEdit, _) => Context::SchemaEdit,
        (
            _,
            "query_definition"
            | "interpret_query_statement"
            | "opencypher_query_definition",
        ) => Context::Query,
        (_, "loading_job_definition") => Context::LoadingJob,
        (
            Context::Command,
            "vertex_definition" | "edge_definition" | "graph_definition"
            | "typedef_statement",
        ) => Context::Definition,
        (outer, _) => outer,
    }
}

fn is_write(path: Path) -> bool {
    let mut current = path;
    while let Some(parent) = current.parent() {
        match parent.node().kind_id() {
            kind::ASSIGNMENT_STATEMENT => {
                return current.field() == Some("left");
            }
            kind::MEMBER_EXPRESSION
                if current.field() == Some("property") =>
            {
                current = parent
            }
            kind::SUBSCRIPT_EXPRESSION
                if current.field() == Some("object") =>
            {
                current = parent
            }
            _ => return false,
        }
    }
    false
}

/// A query or job declaration up to its body, on one line.
fn header_detail(node: Node, source: &str) -> String {
    let end = node
        .child_by_field_id(field::BODY)
        .map_or(node.end_byte(), |b| b.start_byte());
    collapse_whitespace(
        source
            .get(node.start_byte()..end)
            .unwrap_or(""),
    )
}

fn for_graph(node: Node, source: &str) -> Option<String> {
    syntax::child_of_kind(node, "for_graph_clause")
        .and_then(|c| syntax::field_text(c, "graph", source))
        .map(String::from)
}

fn parameters(node: Node, source: &str) -> Vec<Param> {
    let Some(list) = node.child_by_field_id(field::PARAMETERS) else {
        return Vec::new();
    };
    syntax::code_children(list)
        .into_iter()
        .filter(|p| p.kind_id() == kind::PARAMETER)
        .map(|p| Param {
            name: syntax::field_text(p, "name", source)
                .unwrap_or("")
                .to_string(),
            ty: p
                .child_by_field_id(field::TYPE)
                .map(|t| collapse_whitespace(syntax::text(t, source)))
                .unwrap_or_default(),
            default: p
                .child_by_field_id(field::DEFAULT)
                .map(|d| collapse_whitespace(syntax::text(d, source))),
        })
        .collect()
}

/// First and last non-comment child kinds.
fn first_last_kinds<'t>(
    children: impl Iterator<Item = Node<'t>>,
) -> (Option<&'t str>, Option<&'t str>) {
    let mut kinds = children
        .map(|c| c.kind())
        .filter(|k| *k != "comment");
    let first = kinds.next();
    (first, kinds.last().or(first))
}

/// Comments before the declaration or its statement.
fn doc_comment(node: Node, source: &str) -> Option<String> {
    syntax::leading_comments(node, source).or_else(|| {
        let parent = node.parent()?;
        if matches!(
            parent.kind_id(),
            kind::VARIABLE_DECLARATION | kind::ACCUMULATOR_DECLARATION
        ) {
            syntax::leading_comments(parent, source)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;

    /// Person has an attribute named like the Company vertex type, declared first.
    const SCHEMA: &str = "CREATE VERTEX Person (PRIMARY_ID id STRING, Company STRING, follows STRING)\nCREATE VERTEX Company (PRIMARY_ID id STRING)\nCREATE DIRECTED EDGE follows (FROM Person, TO Person)\n";

    fn query(body: &str) -> String {
        format!("{SCHEMA}CREATE QUERY q() {{\n  {body}\n  PRINT R;\n}}\n")
    }

    fn alias_type(analysis: &Analysis, name: &str) -> Ty {
        analysis
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Alias && s.name == name)
            .map(|s| s.ty.clone())
            .unwrap_or_else(|| panic!("no alias {name}"))
    }

    #[test]
    fn an_edge_type_in_the_same_file_types_the_edge_alias() {
        let (_, analysis) = Analysis::parse(&query(
            "R = SELECT t FROM Person:s -(follows:e)- Person:t;",
        ));
        assert_eq!(
            alias_type(&analysis, "e"),
            Ty::Edge(vec!["follows".into()])
        );
    }

    #[test]
    fn an_edge_type_in_the_same_file_types_the_far_end() {
        let fixture = Fixture::new(&query(
            "R = SELECT m FROM Person:s -(follows>)- :m;",
        ));
        assert_eq!(
            alias_type(&fixture.analysis, "m"),
            Ty::Vertex(vec!["Person".into()])
        );
    }

    #[test]
    fn an_attribute_does_not_shadow_a_vertex_type() {
        let (_, analysis) =
            Analysis::parse(&query("R = SELECT c FROM Company:c;"));
        assert_eq!(
            alias_type(&analysis, "c"),
            Ty::Vertex(vec!["Company".into()])
        );
    }

    #[test]
    fn a_tuple_field_does_not_type_a_variable_of_the_same_name() {
        let (_, analysis) = Analysis::parse(
            "CREATE QUERY q() {\n  TYPEDEF TUPLE<INT name> Info;\n  STRING name = \"a\";\n  y = name;\n  PRINT y;\n}\n",
        );
        let y = analysis
            .symbols
            .iter()
            .find(|s| s.name == "y")
            .expect("y");
        assert_eq!(y.ty, Ty::Primitive("STRING".into()));
    }

    #[test]
    fn a_parameter_still_shadows_an_edge_type() {
        let text = format!(
            "{SCHEMA}CREATE QUERY q(STRING follows) {{\n  R = SELECT t FROM Person:s -(follows:e)- Person:t;\n  PRINT R;\n}}\n"
        );
        let (_, analysis) = Analysis::parse(&text);
        assert_eq!(alias_type(&analysis, "e"), Ty::Edge(Vec::new()));
    }

    #[test]
    fn an_accumulator_filled_from_a_map_of_literals_types_the_edge_alias() {
        let text = format!(
            "{SCHEMA}CREATE QUERY q() {{\n  STRING KEY = \"k\";\n  STRING OTHER = \"o\";\n  MapAccum<STRING, STRING> @@m;\n  SetAccum<STRING> @@types;\n  @@m += (KEY -> \"follows\");\n  @@m += (OTHER -> \"likes\");\n  @@types += @@m.get(KEY);\n  R = SELECT t FROM Person:s -(@@types:e)- Person:t;\n  PRINT R;\n}}\n"
        );
        let (_, analysis) = Analysis::parse(&text);
        assert_eq!(
            alias_type(&analysis, "e"),
            Ty::Edge(vec!["follows".into()])
        );
    }

    #[test]
    fn a_nested_map_read_with_an_unknown_key_types_the_edge_alias() {
        let text = format!(
            "{SCHEMA}CREATE QUERY q(STRING kind) {{\n  STRING EDGE_KEY = \"e\";\n  MapAccum<STRING, MapAccum<STRING, STRING>> @@m;\n  SetAccum<STRING> @@types;\n  @@m += (\"a\" -> (EDGE_KEY -> \"follows\"));\n  @@m += (\"b\" -> (EDGE_KEY -> \"likes\"));\n  @@types += @@m.get(kind).get(EDGE_KEY);\n  R = SELECT t FROM Person:s -(@@types:e)- Person:t;\n  PRINT R;\n}}\n"
        );
        let (_, analysis) = Analysis::parse(&text);
        assert_eq!(
            alias_type(&analysis, "e"),
            Ty::Edge(vec!["follows".into(), "likes".into()])
        );
    }

    #[test]
    fn an_accumulator_with_an_unknown_value_leaves_the_edge_alias_untyped() {
        let text = format!(
            "{SCHEMA}CREATE QUERY q(STRING given) {{\n  SetAccum<STRING> @@types;\n  @@types += \"follows\";\n  @@types += given;\n  R = SELECT t FROM Person:s -(@@types:e)- Person:t;\n  PRINT R;\n}}\n"
        );
        let (_, analysis) = Analysis::parse(&text);
        assert_eq!(alias_type(&analysis, "e"), Ty::Edge(Vec::new()));
    }

    #[test]
    fn an_interpret_query_ignores_values_assigned_in_other_queries() {
        let text = format!(
            "{SCHEMA}CREATE QUERY other() {{\n  SetAccum<STRING> @@types;\n  @@types += \"likes\";\n  PRINT @@types;\n}}\nINTERPRET QUERY () {{\n  SetAccum<STRING> @@types;\n  @@types += \"follows\";\n  R = SELECT t FROM Person:s -(@@types:e)- Person:t;\n  PRINT R;\n}}\n"
        );
        let (_, analysis) = Analysis::parse(&text);
        assert_eq!(
            alias_type(&analysis, "e"),
            Ty::Edge(vec!["follows".into()])
        );
    }

    #[test]
    fn an_untyped_vertex_set_label_makes_the_node_alias_any_vertex() {
        let text = format!(
            "{SCHEMA}CREATE QUERY q(SET<VERTEX> S) SYNTAX V3 {{\n  R = SELECT t FROM (t:Person|S);\n  PRINT R;\n}}\n"
        );
        let (_, analysis) = Analysis::parse(&text);
        assert_eq!(alias_type(&analysis, "t"), Ty::Vertex(Vec::new()));
    }

    /// Vertex and edge types that differ in their attributes.
    const PEOPLE: &str = "\
        CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\n\
        CREATE VERTEX Company (PRIMARY_ID id STRING, companyName STRING)\n\
        CREATE DIRECTED EDGE Knows (FROM Person, TO Person, since INT)\n\
        CREATE DIRECTED EDGE Works (FROM Person, TO Company, role STRING)\n\
        CREATE GRAPH g (*)\n";

    /// Global accumulators used as pattern labels.
    const COMPANIES: &str = "SetAccum<VERTEX<Company>> @@c;";
    const VERTICES: &str = "SetAccum<VERTEX> @@c;";
    const WORKS: &str = "SetAccum<EDGE<Works>> @@es;";

    /// A SYNTAX v3 query that declares `accumulators` and selects from `pattern`.
    fn v3_select(params: &str, accumulators: &str, pattern: &str) -> String {
        format!(
            "CREATE QUERY q({params}) FOR GRAPH g SYNTAX v3 {{\n  {accumulators}\n  \
             R = SELECT t FROM {pattern};\n  PRINT R;\n}}\n"
        )
    }

    /// The unknown-attribute messages of `text` against [`PEOPLE`].
    fn unknown_attributes(text: &str) -> Vec<String> {
        let schema = [(crate::features::test_support::SCHEMA_URI, PEOPLE)];
        let found = crate::features::test_support::findings(text, &schema);
        crate::features::test_support::with_code(&found, "unknown-attribute")
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// The role of the property map key `key`, and the type of `alias`.
    fn key_and_alias(
        text: &str,
        key: &str,
        alias: &str,
    ) -> (Option<Role>, Ty) {
        let fixture = Fixture::with_schema(text, PEOPLE);
        let role = fixture
            .analysis
            .references
            .iter()
            .find(|r| r.name == key)
            .map(|r| r.role.clone());
        (role, alias_type(&fixture.analysis, alias))
    }

    #[test]
    fn a_property_map_is_checked_against_the_type_of_its_alias() {
        let vertex = |types: &[&str]| {
            Ty::Vertex(types.iter().map(|t| t.to_string()).collect())
        };
        let cases = [
            (
                v3_select("", COMPANIES, "(t:Person|@@c {name: \"x\"})"),
                "name",
                "t",
                vertex(&["Person", "Company"]),
            ),
            (
                v3_select("", VERTICES, "(t:Person|@@c {name: \"x\"})"),
                "name",
                "t",
                vertex(&[]),
            ),
            (
                v3_select(
                    "SET<VERTEX<Company>> S",
                    "",
                    "(t:S {companyName: \"x\"})",
                ),
                "companyName",
                "t",
                vertex(&["Company"]),
            ),
            (
                v3_select(
                    "",
                    "",
                    "(s:Person)-[:Works]->(t {companyName: \"x\"})",
                ),
                "companyName",
                "t",
                vertex(&["Company"]),
            ),
            (
                v3_select(
                    "",
                    "",
                    "(s:Person)-[:Works|_]->(t {companyName: \"x\"})",
                ),
                "companyName",
                "t",
                vertex(&[]),
            ),
            (
                v3_select(
                    "",
                    WORKS,
                    "(s:Person)-[e:Knows|@@es {since: 1}]->(t)",
                ),
                "since",
                "e",
                Ty::Edge(Vec::new()),
            ),
            (
                v3_select("", "", "(s:Person)-[e:Knows {since: 1}]->(t)"),
                "since",
                "e",
                Ty::Edge(vec!["Knows".into()]),
            ),
        ];
        for (text, key, alias, expected) in cases {
            let (role, ty) = key_and_alias(&text, key, alias);
            assert_eq!(ty, expected, "{text}");
            assert_eq!(role, Some(Role::Attribute(ty)), "{text}");
        }
    }

    #[test]
    fn an_accumulator_label_widens_the_property_map() {
        for text in [
            v3_select("", COMPANIES, "(t:Person|@@c {companyName: \"x\"})"),
            v3_select("", VERTICES, "(t:Person|@@c {companyName: \"x\"})"),
            v3_select(
                "",
                COMPANIES,
                "(s:Person)-[:Works]->(t:Person|@@c {companyName: \"x\"})",
            ),
            v3_select(
                "",
                WORKS,
                "(s:Person)-[e:Knows|@@es {role: \"x\"}]->(t)",
            ),
        ] {
            assert_eq!(
                unknown_attributes(&text),
                Vec::<String>::new(),
                "{text}"
            );
        }
        let typo = v3_select("", COMPANIES, "(t:Person|@@c {nme: \"x\"})");
        assert_eq!(
            unknown_attributes(&typo),
            vec![
                "None of Person, Company has an attribute `nme` (did you mean `name`?)"
            ]
        );
    }

    #[test]
    fn a_vertex_set_label_types_the_property_map() {
        let text =
            v3_select("SET<VERTEX<Company>> S", "", "(t:S {nme: \"x\"})");
        assert_eq!(
            unknown_attributes(&text),
            vec!["`Company` has no attribute `nme`"]
        );
    }

    #[test]
    fn the_columns_of_an_edge_insert_are_edge_attributes() {
        let (_, analysis) = Analysis::parse(&query(
            "INSERT INTO follows (FROM, TO, since) VALUES (\"a\" Person, \"b\" Person, 3);\n  INSERT INTO Person (PRIMARY_ID, name) VALUES (\"a\", \"x\");",
        ));
        let role = |name: &str| {
            let reference = analysis
                .references
                .iter()
                .find(|r| r.name == name);
            reference.map(|r| r.role.clone())
        };
        assert_eq!(
            role("since"),
            Some(Role::Attribute(Ty::Edge(vec!["follows".into()])))
        );
        assert_eq!(
            role("name"),
            Some(Role::Attribute(Ty::Vertex(vec!["Person".into()])))
        );
    }

    /// An edge with a reverse edge, and `jobs` after it.
    fn reversed(jobs: &str, attribute: &str) -> String {
        format!(
            "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\n\
             CREATE DIRECTED EDGE Knows (FROM Person, TO Person, since INT) \
             WITH REVERSE_EDGE=\"rev_knows\"\n\
             CREATE GRAPH g (*)\n{jobs}\
             CREATE QUERY q() FOR GRAPH g {{\n  Start = {{Person.*}};\n  \
             R1 = SELECT t FROM Start:s -(Knows>:e)- Person:t \
             WHERE e.{attribute} > 1;\n  \
             R2 = SELECT t FROM Start:s -(rev_knows>:e2)- Person:t \
             WHERE e2.{attribute} > 1;\n  PRINT R1, R2;\n}}\n"
        )
    }

    #[test]
    fn a_reverse_edge_attribute_is_the_attribute_of_its_edge() {
        let text = reversed("", "since");
        let (_, analysis) = Analysis::parse(&text);
        assert_eq!(
            alias_type(&analysis, "e2"),
            Ty::Edge(vec!["rev_knows".into()])
        );
        let targets: Vec<_> = analysis
            .references
            .iter()
            .filter(|r| r.name == "since")
            .map(|r| {
                r.target
                    .map(|id| analysis.symbols[id].name_span)
            })
            .collect();
        let declared = text.find("since INT").unwrap();
        let since = Some(Span::new(declared, declared + "since".len()));
        assert_eq!(targets, [since, since, since]);
        let owners: Vec<_> = analysis
            .references
            .iter()
            .filter(|r| r.name == "since")
            .filter_map(|r| analysis.target_symbol(r)?.owner.as_deref())
            .collect();
        assert_eq!(owners, ["Knows", "Knows", "Knows"]);
    }

    #[test]
    fn renaming_an_edge_attribute_renames_it_on_the_reverse_edge() {
        use crate::features::navigation::{references, rename};
        use crate::features::test_support::{MAIN_URI, findings, with_code};
        use crate::text::PositionEncoding;
        let text = reversed("", "since");
        let expected =
            reversed("", "since2").replace("since INT", "since2 INT");
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        for needle in ["since INT", "e.since", "e2.since"] {
            let at =
                text.find(needle).unwrap() + needle.find("since").unwrap();
            let position = snapshot.position(at);
            assert_eq!(
                references(&snapshot, position, true).len(),
                3,
                "{needle}"
            );
            let edit = rename(&snapshot, position, "since2").unwrap();
            let edits = &edit.changes[MAIN_URI];
            let renamed = fixture
                .source
                .apply_edits(edits, PositionEncoding::Utf16);
            assert_eq!(renamed, expected);
            let found = findings(&renamed, &[]);
            assert!(
                with_code(&found, "unknown-attribute").is_empty(),
                "{found:?}"
            );
        }
    }

    #[test]
    fn a_reverse_edge_alias_reads_attributes_added_to_its_edge() {
        use crate::features::test_support::{findings, with_code};
        let job = "CREATE GLOBAL SCHEMA_CHANGE JOB addw {\n  \
                   ALTER EDGE Knows ADD ATTRIBUTE (w INT);\n}\n\
                   RUN GLOBAL SCHEMA_CHANGE JOB addw\n";
        let found = findings(&reversed(job, "w"), &[]);
        assert!(
            with_code(&found, "unknown-attribute").is_empty(),
            "{found:?}"
        );
        let found = findings(&reversed(job, "z"), &[]);
        assert_eq!(
            with_code(&found, "unknown-attribute"),
            [
                "`Knows` has no attribute `z`",
                "`rev_knows` has no attribute `z`"
            ]
        );
    }
}
