//! Builds the semantic model ([`Analysis`]) from a syntax tree.
//!
//! The first pass declares symbols and scopes; the second pass classifies
//! every identifier by its syntactic role and resolves it against the scope
//! chain. Names that live in the workspace (vertex types, queries, ...) and
//! are not defined in this file stay unresolved here and are looked up in the
//! workspace index by the language features.

use std::collections::HashMap;

use tree_sitter::{Node, Tree};

use super::model::*;
use crate::syntax;
use crate::text::Span;
use crate::workspace::Workspace;

pub fn analyze(tree: &Tree, source: &str) -> Analysis {
    analyze_in(tree, source, None)
}

/// Like `analyze`; with a workspace, an alias reached through a schema edge
/// takes the vertex type the edge determines.
pub fn analyze_in(tree: &Tree, source: &str, workspace: Option<&Workspace>) -> Analysis {
    let root = tree.root_node();
    let mut builder = Builder {
        source,
        workspace,
        uses_schema_edges: std::cell::Cell::new(false),
        analysis: Analysis::default(),
        stack: Vec::new(),
        declarations: HashMap::new(),
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
    analysis.references.sort_by_key(|r| (r.span.start, r.span.end));
    analysis
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
    crate::builtins::accumulator(name).map(|a| a.name.to_string()).unwrap_or_else(|| name.to_string())
}

/// The static type described by a type node.
pub fn type_of_type_node(node: Node, source: &str) -> Ty {
    match node.kind() {
        "primitive_type" => Ty::Primitive(collapse_whitespace(syntax::text(node, source)).to_uppercase()),
        "vertex_type" => Ty::Vertex(field_list(node, "type", source)),
        "edge_type" => Ty::Edge(field_list(node, "type", source)),
        "type_identifier" | "identifier" => Ty::Tuple(syntax::text(node, source).to_string()),
        "file_type" => Ty::File,
        "collection_type" => {
            let kind = syntax::field_text(node, "kind", source).unwrap_or("LIST").to_uppercase();
            let args = ["element", "key", "value"]
                .iter()
                .filter_map(|field| node.child_by_field_name(field))
                .map(|child| type_of_type_node(child, source))
                .collect();
            Ty::Collection(kind, args)
        }
        "accumulator_type" => {
            let kind = syntax::field_text(node, "kind", source).unwrap_or("");
            let args = syntax::children_by_field(node, "argument")
                .into_iter()
                .map(|arg| match arg.kind() {
                    "group_by_field" => {
                        arg.child_by_field_name("type").map(|t| type_of_type_node(t, source)).unwrap_or_default()
                    }
                    _ => type_of_type_node(arg, source),
                })
                .collect();
            Ty::Accumulator(canonical_accumulator(kind), args)
        }
        _ => Ty::Unknown,
    }
}

fn field_list(node: Node, field: &str, source: &str) -> Vec<String> {
    syntax::children_by_field(node, field).into_iter().map(|child| syntax::text(child, source).to_string()).collect()
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

struct Builder<'s> {
    source: &'s str,
    workspace: Option<&'s Workspace>,
    uses_schema_edges: std::cell::Cell<bool>,
    analysis: Analysis,
    stack: Vec<ScopeId>,
    /// Name spans of declarations.
    declarations: HashMap<Span, SymbolId>,
    /// Per scope: symbols by name, in declaration order.
    names: Vec<HashMap<String, Vec<SymbolId>>>,
    /// Number of ERROR nodes enclosing the node being declared.
    error_depth: usize,
    /// How deep the declaration pass is in the tree.
    depth: usize,
}

/// A top-level `CREATE` (not `ADD`, not `OR REPLACE`, not a function).
fn is_strict_create(node: Node) -> bool {
    node.parent().is_some_and(|p| p.kind() == "source_file")
        && syntax::children(node).first().is_some_and(|c| c.kind() == "CREATE")
        && !syntax::children(node).iter().any(|c| matches!(c.kind(), "OR" | "FUNCTION"))
}

/// Whether the vertex definition has `WITH primary_id_as_attribute="true"`
/// (any other value than a literal false counts, as the option is only read
/// to know whether the id is an attribute).
fn has_primary_id_as_attribute(node: Node, source: &str) -> bool {
    let Some(with) = syntax::code_children(node).into_iter().find(|c| c.kind() == "with_clause") else {
        return false;
    };
    syntax::code_children(with).into_iter().any(|option| {
        syntax::field_text(option, "key", source).is_some_and(|k| k.eq_ignore_ascii_case("primary_id_as_attribute"))
            && !syntax::field_text(option, "value", source)
                .is_some_and(|v| v.trim_matches('"').eq_ignore_ascii_case("false"))
    })
}

/// The declaration pass recurses; deeper nodes (only machine-made code nests
/// this deep) are not analyzed, so that the stack cannot overflow.
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
        *self.stack.last().expect("the file scope is always on the stack")
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
        self.declarations.insert(self.analysis.symbols[id].name_span, id);
        id
    }

    fn add_to_scope(&mut self, scope: ScopeId, id: SymbolId) {
        self.analysis.scopes[scope].symbols.push(id);
        let name = self.analysis.symbols[id].name.clone();
        self.names[scope].entry(name).or_default().push(id);
    }

    /// Symbols named `name` declared directly in `scope`.
    fn named(&self, scope: ScopeId, name: &str) -> &[SymbolId] {
        self.names[scope].get(name).map(Vec::as_slice).unwrap_or(&[])
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
        // Aliases, loop variables and parameters are not documented by the
        // comments above them (and looking for comments is not free).
        if !matches!(kind, SymbolKind::Alias | SymbolKind::LoopVariable | SymbolKind::Parameter | SymbolKind::Table) {
            self.analysis.symbols[id].doc = doc_comment(decl, self.source);
        }
        id
    }

    fn lookup(&self, name: &str, scope: ScopeId, kinds: &[SymbolKind]) -> Option<SymbolId> {
        self.analysis.scope_chain(scope).find_map(|scope| {
            self.named(scope, name)
                .iter()
                .copied()
                .find(|&id| kinds.is_empty() || kinds.contains(&self.analysis.symbols[id].kind))
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
        match node.kind() {
            "ERROR" => {
                self.error_depth += 1;
                self.declare_children(node);
                self.error_depth -= 1;
            }
            "query_definition" | "opencypher_query_definition" => self.declare_query(node),
            "interpret_query_statement" => {
                if node.child_by_field_name("body").is_some() {
                    self.push(ScopeKind::Query, node);
                    self.declare_parameters(node);
                    if let Some(body) = node.child_by_field_name("body") {
                        self.declare(body);
                    }
                    self.pop();
                }
            }
            "vertex_definition" => self.declare_vertex_type(node),
            "edge_definition" => self.declare_edge_type(node, 0),
            "drop_statement" => self.record_drop(node),
            // A virtual edge type exists only while its query runs.
            "virtual_edge_declaration" => {
                let scope = self.query_or_current();
                self.declare_edge_type(node, scope);
            }
            "graph_definition" => self.declare_graph(node),
            "typedef_statement" => self.declare_typedef(node),
            "loading_job_definition" => self.declare_job(node, SymbolKind::LoadingJob),
            "schema_change_job_definition" => self.declare_job(node, SymbolKind::SchemaChangeJob),
            "alter_type_statement" => self.declare_added_attributes(node),
            "data_source_definition" | "package_definition" => {
                let kind =
                    if node.kind() == "package_definition" { SymbolKind::Package } else { SymbolKind::DataSource };
                if let Some(name) = node.child_by_field_name("name") {
                    let detail = collapse_whitespace(self.text(node));
                    self.define_named(0, kind, node, name, detail, Ty::Unknown);
                }
            }
            "define_filename_statement" | "define_header_statement" | "define_input_line_filter_statement" => {
                let kind = match node.kind() {
                    "define_filename_statement" => SymbolKind::FilenameVariable,
                    "define_header_statement" => SymbolKind::Header,
                    _ => SymbolKind::LineFilter,
                };
                if let Some(name) = node.child_by_field_name("name") {
                    let detail = collapse_whitespace(self.text(node));
                    let scope = self.current_scope();
                    self.define_named(scope, kind, node, name, detail, Ty::Unknown);
                }
            }
            "load_destination" => {
                let is_temp_table = node.child_by_field_name("kind").is_some_and(|k| k.kind() == "TEMP_TABLE");
                if let (true, Some(target)) = (is_temp_table, node.child_by_field_name("target")) {
                    let name = self.text(target);
                    let scope = self.current_scope();
                    if self.lookup(name, scope, &[SymbolKind::TempTable]).is_none() {
                        let detail = format!("TEMP_TABLE {}", self.text(target));
                        self.define_named(scope, SymbolKind::TempTable, node, target, detail, Ty::Unknown);
                        // The columns are declared with the table; a repeated `TO TEMP_TABLE t` adds none.
                        if let Some(columns) = node.child_by_field_name("columns") {
                            let table = self.text(target).to_string();
                            for column in
                                syntax::code_children(columns).into_iter().filter(|c| c.kind() == "identifier")
                            {
                                let detail = format!("{} (column of TEMP_TABLE {table})", self.text(column));
                                let id = self.define_named(
                                    scope,
                                    SymbolKind::TempColumn,
                                    column,
                                    column,
                                    detail,
                                    Ty::Unknown,
                                );
                                self.analysis.symbols[id].owner = Some(table.clone());
                            }
                        }
                    }
                }
                self.declare_children(node);
            }
            "accumulator_declaration" => self.declare_accumulators(node),
            "variable_declaration" => self.declare_variables(node),
            "file_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let scope = self.current_scope();
                    let detail = format!("FILE {}", self.text(name));
                    self.define_named(scope, SymbolKind::File, node, name, detail, Ty::File);
                }
            }
            "exception_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let scope = self.query_or_current();
                    let detail = collapse_whitespace(self.text(node));
                    self.define_named(scope, SymbolKind::Exception, node, name, detail, Ty::Unknown);
                }
            }
            "vertex_set_declaration" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.declare(value);
                }
                if let Some(name) = node.child_by_field_name("name") {
                    let types = match node.child_by_field_name("type") {
                        Some(t) if t.kind() == "identifier" => vec![self.text(t).to_string()],
                        _ => Vec::new(),
                    };
                    let scope = self.query_or_current();
                    let ty_name = node.child_by_field_name("type").map(|t| self.text(t)).unwrap_or("ANY");
                    let detail = format!("{} ({ty_name})", self.text(name));
                    self.define_named(scope, SymbolKind::VertexSet, node, name, detail, Ty::VertexSet(types));
                }
            }
            "assignment_statement" => self.declare_assignment(node),
            "select_statement" => self.declare_select(node),
            "delete_statement" | "update_statement" => {
                self.push(ScopeKind::Block, node);
                if let Some(from) = syntax::code_children(node).into_iter().find(|c| c.kind() == "from_clause") {
                    self.declare(from);
                }
                for child in syntax::code_children(node) {
                    if child.kind() != "from_clause" {
                        self.declare(child);
                    }
                }
                self.pop();
            }
            "foreach_statement" => self.declare_foreach(node),
            // The bodies of IF, ELSE, WHILE, CASE and TRY are blocks: what is
            // declared in one is visible only there.
            "block" => {
                self.push(ScopeKind::Block, node);
                self.declare_children(node);
                self.pop();
            }
            "vertex_pattern" => self.declare_vertex_alias(node),
            "edge_pattern" => self.declare_edge_alias(node),
            "node_pattern" => self.declare_node_alias(node),
            "relationship_detail" => self.declare_relationship_alias(node),
            _ => self.declare_children(node),
        }
    }

    fn declare_query(&mut self, node: Node) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return self.declare_children(node);
        };
        // Header text up to (not including) the body.
        let header_end = node.child_by_field_name("body").map(|b| b.start_byte()).unwrap_or(node.end_byte());
        let detail = collapse_whitespace(&self.source[node.start_byte()..header_end]);
        let id = self.define_named(0, SymbolKind::Query, node, name_node, detail, Ty::Unknown);
        self.analysis.symbols[id].strict_create = is_strict_create(node);
        self.analysis.symbols[id].graph = for_graph(node, self.source);
        self.analysis.symbols[id].params = parameters(node, self.source);
        if let Some(returns) = syntax::code_children(node)
            .into_iter()
            .find(|c| c.kind() == "returns_clause")
            .and_then(|r| r.child_by_field_name("type"))
        {
            self.analysis.symbols[id].ty = type_of_type_node(returns, self.source);
            self.analysis.symbols[id].returns = Some(collapse_whitespace(syntax::text(returns, self.source)));
        }
        self.push(ScopeKind::Query, node);
        self.declare_parameters(node);
        if let Some(body) = node.child_by_field_name("body") {
            self.declare(body);
        }
        self.pop();
    }

    fn declare_parameters(&mut self, node: Node) {
        let Some(list) = node.child_by_field_name("parameters") else {
            return;
        };
        let scope = self.current_scope();
        for parameter in syntax::code_children(list) {
            if parameter.kind() != "parameter" {
                continue;
            }
            let (Some(name), Some(ty)) = (parameter.child_by_field_name("name"), parameter.child_by_field_name("type"))
            else {
                continue;
            };
            let detail = collapse_whitespace(self.text(parameter));
            let ty = type_of_type_node(ty, self.source);
            self.define_named(scope, SymbolKind::Parameter, parameter, name, detail, ty);
        }
    }

    fn declare_vertex_type(&mut self, node: Node) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let type_name = self.text(name).to_string();
        let detail = collapse_whitespace(self.text(node));
        let id = self.define_named(0, SymbolKind::VertexType, node, name, detail, Ty::Vertex(vec![type_name.clone()]));
        self.analysis.symbols[id].strict_create = is_strict_create(node);
        let id_as_attribute = has_primary_id_as_attribute(node, self.source);
        if let Some(list) = node.child_by_field_name("attributes") {
            for attribute in syntax::code_children(list) {
                if matches!(attribute.kind(), "primary_id_definition" | "attribute_definition") {
                    let id = self.declare_attribute(attribute, &type_name, 0);
                    if let Some(id) = id.filter(|_| attribute.kind() == "primary_id_definition" && !id_as_attribute) {
                        self.analysis.symbols[id].id_only = true;
                    }
                }
            }
        }
    }

    fn declare_attribute(&mut self, attribute: Node, owner: &str, scope: ScopeId) -> Option<SymbolId> {
        let (Some(name), Some(ty)) = (attribute.child_by_field_name("name"), attribute.child_by_field_name("type"))
        else {
            return None;
        };
        let detail = collapse_whitespace(self.text(attribute));
        let ty = type_of_type_node(ty, self.source);
        let id = self.define_named(scope, SymbolKind::Attribute, attribute, name, detail, ty);
        self.analysis.symbols[id].owner = Some(owner.to_string());
        Some(id)
    }

    fn declare_edge_type(&mut self, node: Node, scope: ScopeId) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let type_name = self.text(name).to_string();
        let detail = collapse_whitespace(self.text(node));
        let id = self.define_named(
            scope,
            SymbolKind::EdgeType,
            node,
            name,
            detail.clone(),
            Ty::Edge(vec![type_name.clone()]),
        );
        self.analysis.symbols[id].strict_create = scope == 0 && is_strict_create(node);
        let mut endpoints = Vec::new();
        let mut ends = EdgeEnds {
            directed: syntax::field_text(node, "direction", self.source)
                .is_some_and(|d| d.eq_ignore_ascii_case("directed")),
            ..EdgeEnds::default()
        };
        if let Some(list) = node.child_by_field_name("attributes") {
            for child in syntax::code_children(list) {
                match child.kind() {
                    "edge_pair" => {
                        for field in ["from", "to"] {
                            for endpoint in syntax::children_by_field(child, field) {
                                let text = self.text(endpoint).to_string();
                                let side = if field == "from" { &mut ends.from } else { &mut ends.to };
                                if !side.contains(&text) {
                                    side.push(text.clone());
                                }
                                if !endpoints.contains(&text) {
                                    endpoints.push(text);
                                }
                            }
                        }
                    }
                    "attribute_definition" => {
                        self.declare_attribute(child, &type_name, scope);
                    }
                    "discriminator" => {
                        for attribute in syntax::code_children(child) {
                            if attribute.kind() == "attribute_definition" {
                                self.declare_attribute(attribute, &type_name, scope);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.analysis.symbols[id].members = endpoints.clone();
        self.analysis.symbols[id].ends = ends.clone();
        // `WITH REVERSE_EDGE="name"` declares a second edge type.
        for option in syntax::code_children(node)
            .into_iter()
            .filter(|c| c.kind() == "with_clause")
            .flat_map(syntax::code_children)
        {
            let key = syntax::field_text(option, "key", self.source).unwrap_or("");
            let Some(value) = option.child_by_field_name("value") else {
                continue;
            };
            if !key.eq_ignore_ascii_case("reverse_edge") || value.kind() != "string" {
                continue;
            }
            let raw = self.text(value);
            let reverse = raw.trim_matches('"').to_string();
            if reverse.is_empty() {
                continue;
            }
            // Point at the name inside the quotes.
            let name_span = Span::new(value.start_byte() + 1, value.end_byte().saturating_sub(1));
            let reverse_id = self.define_in(
                0,
                NewSymbol {
                    name: reverse.clone(),
                    kind: SymbolKind::EdgeType,
                    // Not the whole statement: the attributes belong to the edge it names.
                    span: Span::of(option),
                    name_span,
                    detail: format!("reverse edge of {type_name}"),
                    ty: Ty::Edge(vec![reverse.clone()]),
                },
            );
            self.analysis.symbols[reverse_id].members = endpoints.clone();
            self.analysis.symbols[reverse_id].ends =
                EdgeEnds { from: ends.to.clone(), to: ends.from.clone(), directed: true };
            // The reverse edge carries the same attributes.
            let attributes: Vec<Symbol> = self
                .analysis
                .symbols
                .iter()
                .filter(|s| s.kind == SymbolKind::Attribute && s.owner.as_deref() == Some(type_name.as_str()))
                .cloned()
                .collect();
            for mut attribute in attributes {
                attribute.owner = Some(reverse.clone());
                attribute.copied = true;
                let attribute_id = self.analysis.symbols.len();
                self.analysis.symbols.push(attribute);
                self.add_to_scope(0, attribute_id);
            }
        }
    }

    fn declare_graph(&mut self, node: Node) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let detail = collapse_whitespace(self.text(node));
        let id = self.define_named(0, SymbolKind::Graph, node, name, detail, Ty::Unknown);
        self.analysis.symbols[id].strict_create = is_strict_create(node);
        self.analysis.symbols[id].members = field_list(node, "member", self.source);
    }

    fn declare_typedef(&mut self, node: Node) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let scope = self.query_or_current();
        let detail = collapse_whitespace(self.text(node));
        if let Some(accumulator) = node.child_by_field_name("type") {
            let ty = type_of_type_node(accumulator, self.source);
            self.define_named(scope, SymbolKind::AccumulatorType, node, name, detail, ty);
            return;
        }
        let tuple = self.text(name).to_string();
        self.define_named(scope, SymbolKind::TupleType, node, name, detail, Ty::Tuple(tuple.clone()));
        for field in syntax::code_children(node) {
            if field.kind() != "tuple_field" {
                continue;
            }
            let (Some(field_name), Some(ty)) = (field.child_by_field_name("name"), field.child_by_field_name("type"))
            else {
                continue;
            };
            let detail = collapse_whitespace(self.text(field));
            let ty = type_of_type_node(ty, self.source);
            let id = self.define_named(scope, SymbolKind::TupleField, field, field_name, detail, ty);
            self.analysis.symbols[id].owner = Some(tuple.clone());
        }
    }

    fn declare_job(&mut self, node: Node, kind: SymbolKind) {
        let mut job = None;
        if let Some(name) = node.child_by_field_name("name") {
            let header_end = node.child_by_field_name("body").map(|b| b.start_byte()).unwrap_or(node.end_byte());
            let detail = collapse_whitespace(&self.source[node.start_byte()..header_end]);
            let id = self.define_named(0, kind, node, name, detail, Ty::Unknown);
            self.analysis.symbols[id].graph = for_graph(node, self.source);
            job = Some(id);
        }
        let scope = self.push(ScopeKind::Job, node);
        if let Some(body) = node.child_by_field_name("body") {
            self.declare(body);
        }
        self.pop();
        // The file variables a `RUN LOADING JOB .. USING` can name.
        if let (Some(job), SymbolKind::LoadingJob) = (job, kind) {
            let job_name = self.analysis.symbols[job].name.clone();
            for index in 0..self.analysis.scopes[scope].symbols.len() {
                let id = self.analysis.scopes[scope].symbols[index];
                if self.analysis.symbols[id].kind == SymbolKind::FilenameVariable {
                    self.analysis.symbols[id].owner = Some(job_name.clone());
                }
            }
            let files = self.analysis.scopes[scope]
                .symbols
                .iter()
                .map(|&id| &self.analysis.symbols[id])
                .filter(|s| s.kind == SymbolKind::FilenameVariable)
                .map(|s| s.name.clone())
                .collect();
            self.analysis.symbols[job].members = files;
        }
    }

    /// Notes what a DROP removes (`DROP ALL` and `DROP GRAPH` remove everything).
    fn record_drop(&mut self, node: Node) {
        use SymbolKind as K;
        let kind = node.child_by_field_name("kind").map(|k| k.kind());
        let names: Vec<String> =
            syntax::children_by_field(node, "name").into_iter().map(|n| self.text(n).to_string()).collect();
        let everything = [K::VertexType, K::EdgeType, K::Graph, K::Query];
        let drops = &mut self.analysis.drops;
        let mut add = |kind: K, name: &str| drops.push((kind, name.to_string()));
        match kind {
            Some("VERTEX" | "EDGE") => {
                let kind = if kind == Some("VERTEX") { K::VertexType } else { K::EdgeType };
                names.iter().for_each(|n| add(kind, n));
            }
            Some("QUERY" | "FUNCTION") if names.is_empty() => add(K::Query, "*"),
            Some("QUERY" | "FUNCTION") => names.iter().for_each(|n| add(K::Query, n)),
            Some("GRAPH") | None => everything.iter().for_each(|k| add(*k, "*")),
            _ => {}
        }
    }

    fn declare_added_attributes(&mut self, node: Node) {
        let Some(owner) = node.child_by_field_name("name").map(|n| self.text(n).to_string()) else {
            return;
        };
        for attribute in syntax::code_children(node) {
            if attribute.kind() == "attribute_definition"
                && let Some(id) = self.declare_attribute(attribute, &owner, 0)
            {
                self.analysis.symbols[id].altered = true;
            }
        }
        let keywords: Vec<&str> = syntax::children(node).iter().map(|c| c.kind()).collect();
        if keywords.contains(&"VECTOR")
            && keywords.contains(&"ADD")
            && let Some(name) = node.child_by_field_name("attribute")
        {
            let options = collapse_whitespace(&self.source[name.end_byte()..node.end_byte()]);
            let detail = format!("{} VECTOR {options}", self.text(name));
            let id = self.define_named(0, SymbolKind::Attribute, node, name, detail, Ty::Unknown);
            self.analysis.symbols[id].owner = Some(owner);
            self.analysis.symbols[id].vector = true;
            self.analysis.symbols[id].altered = true;
        }
    }

    fn declare_accumulators(&mut self, node: Node) {
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        // Accumulators are block-scoped.
        let scope = self.current_scope();
        let ty = match type_node.kind() {
            // A TYPEDEF'd accumulator type.
            "type_identifier" => self
                .lookup(self.text(type_node), scope, &[SymbolKind::AccumulatorType])
                .map(|id| self.analysis.symbols[id].ty.clone())
                .unwrap_or_default(),
            _ => type_of_type_node(type_node, self.source),
        };
        let type_text = collapse_whitespace(self.text(type_node));
        let keywords: Vec<&str> = syntax::children(node).iter().map(|c| c.kind()).collect();
        let is_static = keywords.contains(&"STATIC");
        let on_edges = if keywords.contains(&"EDGE") { " EDGE" } else { "" };
        for declarator in syntax::code_children(node) {
            if declarator.kind() != "accumulator_declarator" {
                continue;
            }
            let Some(name) = declarator.child_by_field_name("name") else {
                continue;
            };
            let kind = if name.kind() == "global_accumulator" {
                SymbolKind::GlobalAccumulator
            } else {
                SymbolKind::LocalAccumulator
            };
            let prefix = if is_static { "STATIC " } else { "" };
            let detail = format!("{prefix}{type_text}{on_edges} {}", collapse_whitespace(self.text(declarator)));
            let id = self.define_named(scope, kind, declarator, name, detail, ty.clone());
            self.analysis.symbols[id].on_edges = !on_edges.is_empty();
            // Document every declarator with the declaration's comments.
            if self.analysis.symbols[id].doc.is_none() {
                self.analysis.symbols[id].doc = doc_comment(node, self.source);
            }
            self.analysis.symbols[id].span = Span::of(node);
        }
        self.declare_children(node);
    }

    fn declare_variables(&mut self, node: Node) {
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        let ty = type_of_type_node(type_node, self.source);
        let type_text = collapse_whitespace(self.text(type_node));
        let scope = self.current_scope();
        for declarator in syntax::code_children(node) {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            if let Some(value) = declarator.child_by_field_name("value") {
                self.declare(value);
            }
            let Some(name) = declarator.child_by_field_name("name") else {
                continue;
            };
            let detail = format!("{type_text} {}", self.text(name));
            let id = self.define_named(scope, SymbolKind::Variable, declarator, name, detail, ty.clone());
            if self.analysis.symbols[id].doc.is_none() {
                self.analysis.symbols[id].doc = doc_comment(node, self.source);
            }
        }
    }

    fn declare_assignment(&mut self, node: Node) {
        let right = node.child_by_field_name("right");
        if let Some(right) = right {
            self.declare(right);
        }
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        if left.kind() != "identifier" {
            return self.declare(left);
        }
        let name = self.text(left);
        let scope = self.current_scope();
        if self.lookup(name, scope, LOCAL_VALUES).is_some() {
            return;
        }
        // The first assignment to an undeclared name declares a vertex set
        // (GSQL vertex-set variables need no declaration).
        let ty = right.map(|r| self.expression_type(r, scope)).unwrap_or_default();
        let (kind, ty) = match ty {
            Ty::VertexSet(types) | Ty::Vertex(types) => (SymbolKind::VertexSet, Ty::VertexSet(types)),
            _ if right.is_some_and(|r| r.kind() == "select_statement") => {
                (SymbolKind::VertexSet, Ty::VertexSet(Vec::new()))
            }
            other => (SymbolKind::Variable, other),
        };
        let detail = match &ty {
            Ty::VertexSet(types) if !types.is_empty() => format!("{name} ({})", types.join(" | ")),
            Ty::VertexSet(_) => format!("{name} (vertex set)"),
            other => format!("{name}: {}", other.display()),
        };
        let query_scope = self.query_or_current();
        let id = self.define_named(query_scope, kind, node, left, detail, ty);
        self.analysis.symbols[id].implicit = true;
    }

    fn declare_select(&mut self, node: Node) {
        let query_scope = self.query_or_current();
        self.push(ScopeKind::Block, node);
        let children = syntax::code_children(node);
        // Aliases first: the result list and clauses refer to them.
        for child in children.iter().filter(|c| c.kind() == "from_clause") {
            self.declare(*child);
        }
        // `SELECT COUNT(f) AS n ... HAVING n > 3`: result aliases name columns.
        for result in syntax::children_by_field(node, "result") {
            if let (Some(value), Some(alias)) =
                (result.child_by_field_name("value"), result.child_by_field_name("alias"))
            {
                let ty = self.expression_type(value, self.current_scope());
                self.declare_alias(result, alias, ty);
            }
        }
        for child in &children {
            match child.kind() {
                "from_clause" => {}
                "into_clause" => {
                    for table in syntax::children_by_field(*child, "table") {
                        if self.lookup(self.text(table), query_scope, &[SymbolKind::Table]).is_none() {
                            let detail = format!("table {}", self.text(table));
                            self.define_named(query_scope, SymbolKind::Table, *child, table, detail, Ty::Unknown);
                        }
                    }
                }
                _ => self.declare(*child),
            }
        }
        self.pop();
    }

    fn declare_foreach(&mut self, node: Node) {
        let collection = node.child_by_field_name("collection");
        if let Some(collection) = collection {
            self.declare(collection);
        }
        let collection_ty = collection.map(|c| self.iterable_type(c, self.current_scope())).unwrap_or_default();
        self.push(ScopeKind::Block, node);
        let scope = self.current_scope();
        match node.child_by_field_name("variable") {
            Some(variable) if variable.kind() == "identifier" => {
                let ty = collection_ty.element();
                let detail = format!("{}: {}", self.text(variable), ty.display());
                self.define_named(scope, SymbolKind::LoopVariable, node, variable, detail, ty);
            }
            Some(variables) => {
                let names: Vec<Node> = syntax::code_children(variables);
                let (key, value) = collection_ty.key_value().unwrap_or_default();
                for (index, variable) in names.into_iter().enumerate() {
                    let ty = match index {
                        0 => key.clone(),
                        1 => value.clone(),
                        _ => Ty::Unknown,
                    };
                    let detail = format!("{}: {}", self.text(variable), ty.display());
                    self.define_named(scope, SymbolKind::LoopVariable, node, variable, detail, ty);
                }
            }
            None => {}
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.declare(body);
        }
        self.pop();
    }

    /// The type of a FOREACH iterable: `RANGE[a, b]` and literal lists or tuples
    /// are collections of INT (or the type of their literal elements).
    fn iterable_type(&self, node: Node, scope: ScopeId) -> Ty {
        match node.kind() {
            "range_expression" => Ty::Collection("LIST".into(), vec![Ty::Primitive("INT".into())]),
            "list_literal" | "tuple" => {
                let mut element: Option<Ty> = None;
                for item in syntax::code_children(node) {
                    if item.kind() == "comment" {
                        continue;
                    }
                    let literal = matches!(item.kind(), "integer" | "float" | "string" | "boolean");
                    let ty = self.expression_type(item, scope);
                    if !literal || element.as_ref().is_some_and(|e| *e != ty) {
                        return Ty::Unknown;
                    }
                    element = Some(ty);
                }
                element.map_or(Ty::Unknown, |e| Ty::Collection("LIST".into(), vec![e]))
            }
            _ => self.expression_type(node, scope),
        }
    }

    fn declare_alias(&mut self, decl: Node, alias: Node, ty: Ty) {
        let scope = self.current_scope();
        let name = self.text(alias);
        // openCypher patterns may mention an alias again: `(a)-[]-(b), (b)-[]-(c)`.
        if self.lookup(name, scope, &[SymbolKind::Alias]).is_some_and(|id| self.analysis.symbols[id].scope == scope) {
            return;
        }
        let detail = format!("{name}: {}", ty.display());
        self.define_named(scope, SymbolKind::Alias, decl, alias, detail, ty);
    }

    fn declare_vertex_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_name("alias") else {
            return;
        };
        let scope = self.current_scope();
        let types = match node.child_by_field_name("type") {
            Some(t) => self.vertex_source_types(t, scope),
            None => self.edge_step_target(node, scope).unwrap_or_default(),
        };
        self.declare_alias(node, alias, Ty::Vertex(types));
    }

    /// The vertex types at the far end of the named schema edge, or `None`
    /// when it is unknown or has a wildcard end. A direction that does not
    /// fit the edge leaves both ends possible (the union is taken).
    fn edge_end_types(&self, edge: &str, outgoing: bool, incoming: bool, scope: ScopeId) -> Option<Vec<String>> {
        self.uses_schema_edges.set(true);
        let workspace = self.workspace?;
        // A variable of that name is not a schema edge.
        if self.lookup(edge, scope, &[]).is_some() {
            return None;
        }
        let declarations = workspace.find(SymbolKind::EdgeType, edge);
        if declarations.is_empty() {
            return None;
        }
        let mut types: Vec<String> = Vec::new();
        for declaration in declarations {
            let ends = &declaration.ends;
            if ends.from.is_empty() || ends.to.is_empty() || ends.from.iter().chain(&ends.to).any(|t| t == "*") {
                return None;
            }
            let sides: Vec<&Vec<String>> = match (ends.directed, outgoing, incoming) {
                (true, true, false) => vec![&ends.to],
                (true, false, true) => vec![&ends.from],
                _ => vec![&ends.from, &ends.to],
            };
            for t in sides.into_iter().flatten() {
                if !types.contains(t) {
                    types.push(t.clone());
                }
            }
        }
        Some(types)
    }

    /// The type of an untyped vertex of a FROM pattern, from the edge step before it:
    /// `-(Edge>)- :m`; single edge atoms and alternations of them only.
    fn edge_step_target(&self, node: Node, scope: ScopeId) -> Option<Vec<String>> {
        let step = node.parent().filter(|p| p.kind() == "edge_step")?;
        let mut cursor = step.walk();
        let children: Vec<Node> = step.children(&mut cursor).collect();
        let pattern = children.iter().find(|c| c.kind() == "edge_pattern")?;
        let closes_directed = children.iter().any(|c| c.kind() == "->");
        let spec = pattern.child_by_field_name("type")?;
        let atoms: Vec<Node> = match spec.kind() {
            "edge_atom" => vec![spec],
            "edge_alternation" => {
                let atoms = syntax::code_children(spec);
                if atoms.iter().any(|a| a.kind() != "edge_atom") {
                    return None;
                }
                atoms
            }
            _ => return None,
        };
        let mut types: Vec<String> = Vec::new();
        for atom in atoms {
            let name = atom.child_by_field_name("name").filter(|n| n.kind() == "identifier")?;
            let mut cursor = atom.walk();
            let kinds: Vec<&str> = atom.children(&mut cursor).map(|c| c.kind()).filter(|k| *k != "comment").collect();
            let incoming = kinds.first() == Some(&"<");
            let outgoing = kinds.last() == Some(&">") || (closes_directed && !incoming);
            for t in self.edge_end_types(self.text(name), outgoing, incoming, scope)? {
                if !types.contains(&t) {
                    types.push(t);
                }
            }
        }
        Some(types)
    }

    /// The type of an untyped node of a `(a)-[:Edge]->(b)` pattern.
    fn relationship_target(&self, node: Node, scope: ScopeId) -> Option<Vec<String>> {
        let mut previous = node.prev_named_sibling();
        while previous.is_some_and(|p| p.kind() == "comment") {
            previous = previous.and_then(|p| p.prev_named_sibling());
        }
        let relationship = previous.filter(|p| p.kind() == "relationship_pattern")?;
        let mut cursor = relationship.walk();
        let tokens: Vec<&str> =
            relationship.children(&mut cursor).map(|c| c.kind()).filter(|k| *k != "comment").collect();
        let incoming = tokens.first() == Some(&"<-");
        let outgoing = tokens.last() == Some(&"->");
        let detail = syntax::code_children(relationship).into_iter().find(|c| c.kind() == "relationship_detail")?;
        // `*1..2` reaches any distance.
        let mut cursor = detail.walk();
        if detail.children(&mut cursor).any(|c| c.kind() == "*" || c.kind() == "repetition_bounds") {
            return None;
        }
        let names = syntax::children_by_field(detail, "type");
        if names.is_empty() || names.iter().any(|n| n.kind() != "identifier") {
            return None;
        }
        let mut types: Vec<String> = Vec::new();
        for name in names {
            for t in self.edge_end_types(self.text(name), outgoing, incoming, scope)? {
                if !types.contains(&t) {
                    types.push(t);
                }
            }
        }
        Some(types)
    }

    /// Vertex types denoted by the vertex position of a FROM pattern.
    fn vertex_source_types(&self, node: Node, scope: ScopeId) -> Vec<String> {
        match node.kind() {
            "identifier" => {
                let name = self.text(node);
                match self.lookup(name, scope, &[]) {
                    Some(id) => match &self.analysis.symbols[id].ty {
                        Ty::Vertex(types) | Ty::VertexSet(types) => types.clone(),
                        other => other.element().vertex_types().map(<[String]>::to_vec).unwrap_or_default(),
                    },
                    None => vec![name.to_string()],
                }
            }
            "global_accumulator" => self
                .lookup(self.text(node), scope, &[SymbolKind::GlobalAccumulator])
                .and_then(|id| self.analysis.symbols[id].ty.element().vertex_types().map(<[String]>::to_vec))
                .unwrap_or_default(),
            "vertex_type_alternation" => {
                let mut types = Vec::new();
                for child in syntax::code_children(node) {
                    let more = self.vertex_source_types(child, scope);
                    if more.is_empty() {
                        return Vec::new();
                    }
                    types.extend(more);
                }
                types
            }
            _ => Vec::new(),
        }
    }

    fn declare_edge_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_name("alias") else {
            return;
        };
        let scope = self.current_scope();
        let mut types = Vec::new();
        let mut any = false;
        if let Some(pattern) = node.child_by_field_name("type") {
            syntax::walk(pattern, |n| {
                if n.kind() == "edge_atom" {
                    match n.child_by_field_name("name") {
                        Some(name) if name.kind() == "identifier" => {
                            let text = self.text(name);
                            if self.lookup(text, scope, &[]).is_some() {
                                any = true;
                            } else if !types.iter().any(|t| t == text) {
                                types.push(text.to_string());
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

    fn declare_node_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_name("alias") else {
            return;
        };
        // The type may name a vertex set variable: `(t:workers)`.
        let scope = self.current_scope();
        let mut types: Vec<String> = Vec::new();
        for t in syntax::children_by_field(node, "type") {
            for name in self.vertex_source_types(t, scope) {
                if !types.contains(&name) {
                    types.push(name);
                }
            }
        }
        if node.child_by_field_name("type").is_none() {
            types = self.relationship_target(node, scope).unwrap_or_default();
        }
        self.declare_alias(node, alias, Ty::Vertex(types));
    }

    fn declare_relationship_alias(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_name("alias") else {
            return;
        };
        // `[e:_]` matches any edge type.
        let types = if syntax::children_by_field(node, "type").iter().any(|t| t.kind() == "wildcard") {
            Vec::new()
        } else {
            field_list(node, "type", self.source)
        };
        self.declare_alias(node, alias, Ty::Edge(types));
    }

    /// Best-effort static type of an expression (pass 1 and 2 share it).
    fn expression_type(&self, node: Node, scope: ScopeId) -> Ty {
        match node.kind() {
            "identifier" => self
                .lookup(self.text(node), scope, &[])
                .map(|id| self.analysis.symbols[id].ty.clone())
                .unwrap_or_default(),
            "global_accumulator" => self
                .lookup(self.text(node), scope, &[SymbolKind::GlobalAccumulator])
                .map(|id| self.analysis.symbols[id].ty.clone())
                .unwrap_or_default(),
            "local_accumulator" => self
                .lookup(self.text(node), scope, &[SymbolKind::LocalAccumulator])
                .map(|id| self.analysis.symbols[id].ty.clone())
                .unwrap_or_default(),
            "member_expression" => {
                let Some(property) = node.child_by_field_name("property") else {
                    return Ty::Unknown;
                };
                if property.kind() == "local_accumulator" {
                    return self.expression_type(property, scope);
                }
                let object_ty =
                    node.child_by_field_name("object").map(|o| self.expression_type(o, scope)).unwrap_or_default();
                match object_ty {
                    Ty::Tuple(tuple) => self
                        .lookup_field(&tuple, self.text(property), scope)
                        .map(|id| self.analysis.symbols[id].ty.clone())
                        .unwrap_or_default(),
                    _ => Ty::Unknown,
                }
            }
            "call_expression" => {
                let Some(function) = node.child_by_field_name("function") else {
                    return Ty::Unknown;
                };
                match function.kind() {
                    "identifier" => {
                        let name = self.text(function);
                        if let Some(id) = self.lookup(name, scope, &[SymbolKind::TupleType]) {
                            return self.analysis.symbols[id].ty.clone();
                        }
                        match name.to_ascii_lowercase().as_str() {
                            "to_vertex_set" | "selectvertex" => Ty::VertexSet(Vec::new()),
                            "range" => Ty::Accumulator("ListAccum".into(), vec![Ty::Primitive("INT".into())]),
                            "to_vertex" => Ty::Vertex(Vec::new()),
                            "parse_json_object" => Ty::Primitive("JSONOBJECT".into()),
                            "parse_json_array" => Ty::Primitive("JSONARRAY".into()),
                            "now" | "to_datetime" | "datetime_add" | "datetime_sub" | "epoch_to_datetime" => {
                                Ty::Primitive("DATETIME".into())
                            }
                            _ => Ty::Unknown,
                        }
                    }
                    "member_expression" => {
                        let method = function
                            .child_by_field_name("property")
                            .map(|p| self.text(p).to_ascii_lowercase())
                            .unwrap_or_default();
                        let object_ty = function
                            .child_by_field_name("object")
                            .map(|o| self.expression_type(o, scope))
                            .unwrap_or_default();
                        object_ty.method_result(&method)
                    }
                    _ => Ty::Unknown,
                }
            }
            "range_expression" => Ty::Primitive("INT".into()),
            "parenthesized_expression" => {
                syntax::code_children(node).first().map(|e| self.expression_type(*e, scope)).unwrap_or_default()
            }
            "select_statement" => {
                // The result alias determines the element type of the vertex set.
                let select_scope = self
                    .analysis
                    .scopes
                    .iter()
                    .position(|s| s.span == Span::of(node) && s.kind == ScopeKind::Block)
                    .unwrap_or(scope);
                let result = syntax::children_by_field(node, "result");
                match result.as_slice() {
                    [single] if single.kind() == "identifier" => match self.expression_type(*single, select_scope) {
                        Ty::Vertex(types) => Ty::VertexSet(types),
                        _ => Ty::VertexSet(Vec::new()),
                    },
                    _ => Ty::VertexSet(Vec::new()),
                }
            }
            "vertex_set_literal" => {
                let mut types = Vec::new();
                for seed in syntax::code_children(node) {
                    let more = match seed.kind() {
                        "type_wildcard" => {
                            seed.child_by_field_name("type").map(|t| vec![self.text(t).to_string()]).unwrap_or_default()
                        }
                        "comment" => continue,
                        _ => match self.expression_type(seed, scope) {
                            Ty::Vertex(types) | Ty::VertexSet(types) => types,
                            other => other.element().vertex_types().map(<[String]>::to_vec).unwrap_or_default(),
                        },
                    };
                    if more.is_empty() {
                        return Ty::VertexSet(Vec::new());
                    }
                    for t in more {
                        if !types.contains(&t) {
                            types.push(t);
                        }
                    }
                }
                Ty::VertexSet(types)
            }
            "binary_expression" => {
                let operator = node.child_by_field_name("operator").map(|o| o.kind().to_string()).unwrap_or_default();
                let left = node.child_by_field_name("left").map(|l| self.expression_type(l, scope));
                let right = node.child_by_field_name("right").map(|r| self.expression_type(r, scope));
                match operator.as_str() {
                    "UNION" | "INTERSECT" | "MINUS" => match (left, right) {
                        (Some(Ty::VertexSet(mut a)), Some(Ty::VertexSet(b))) => {
                            if a.is_empty() || b.is_empty() {
                                return Ty::VertexSet(Vec::new());
                            }
                            for t in b {
                                if !a.contains(&t) {
                                    a.push(t);
                                }
                            }
                            Ty::VertexSet(a)
                        }
                        (Some(Ty::VertexSet(_)), _) | (_, Some(Ty::VertexSet(_))) => Ty::VertexSet(Vec::new()),
                        (Some(other), _) => other,
                        _ => Ty::Unknown,
                    },
                    _ => Ty::Unknown,
                }
            }
            "integer" => Ty::Primitive("INT".into()),
            "float" => Ty::Primitive("DOUBLE".into()),
            "string" => Ty::Primitive("STRING".into()),
            "boolean" => Ty::Primitive("BOOL".into()),
            _ => Ty::Unknown,
        }
    }

    fn lookup_field(&self, tuple: &str, field: &str, scope: ScopeId) -> Option<SymbolId> {
        self.analysis.scope_chain(scope).find_map(|scope| {
            self.named(scope, field).iter().copied().find(|&id| {
                let s = &self.analysis.symbols[id];
                s.kind == SymbolKind::TupleField && s.owner.as_deref() == Some(tuple)
            })
        })
    }

    // ------------------------------------------------------------------
    // Pass 2: references
    // ------------------------------------------------------------------

    fn resolve(&mut self, root: Node) {
        // Walk with a cursor and keep the ancestors on a stack: `Node::parent`
        // searches down from the root, which is slow in large files.
        let mut cursor = root.walk();
        let mut stack = vec![(root, None)];
        // The construct each stack entry is in, and whether it is inside a
        // syntax error, maintained alongside the stack.
        let mut contexts = vec![Context::Command];
        let mut errors = vec![false];
        loop {
            let node = cursor.node();
            if matches!(
                node.kind(),
                "identifier" | "type_identifier" | "global_accumulator" | "local_accumulator" | "qualified_identifier"
            ) {
                let path = Path { stack: &stack };
                // Parts of a qualified name are handled with the whole name.
                let in_qualified = path.parent().is_some_and(|p| p.node().kind() == "qualified_identifier");
                if !in_qualified {
                    let context = *contexts.last().expect("parallel to the stack");
                    let in_error = *errors.last().expect("parallel to the stack");
                    self.resolve_occurrence(path, context, in_error);
                }
            }
            if node.kind() == "column_reference" {
                let context = *contexts.last().expect("parallel to the stack");
                let in_error = *errors.last().expect("parallel to the stack");
                self.column_occurrence(Path { stack: &stack }, context, in_error);
            }
            if cursor.goto_first_child() {
                let parent_context = *contexts.last().expect("parallel to the stack");
                let parent_error = *errors.last().expect("parallel to the stack");
                stack.push((cursor.node(), cursor.field_name()));
                contexts.push(enter_at(parent_context, cursor.node(), stack.len() - 1));
                errors.push(parent_error || cursor.node().is_error());
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    stack.pop();
                    contexts.pop();
                    errors.pop();
                    let parent_context = *contexts.last().expect("parallel to the stack");
                    let parent_error = *errors.last().expect("parallel to the stack");
                    stack.push((cursor.node(), cursor.field_name()));
                    contexts.push(enter_at(parent_context, cursor.node(), stack.len() - 1));
                    errors.push(parent_error || cursor.node().is_error());
                    break;
                }
                if !cursor.goto_parent() {
                    return;
                }
                stack.pop();
                contexts.pop();
                errors.pop();
            }
        }
    }

    fn resolve_occurrence(&mut self, path: Path, context: Context, in_error: bool) {
        let node = path.node();
        let span = Span::of(node);
        let name = self.text(node).to_string();
        let scope = self.analysis.scope_at(span.start);
        if let Some(&id) = self.declarations.get(&span) {
            let role = role_for_kind(self.analysis.symbols[id].kind);
            self.analysis.references.push(Reference {
                span,
                name,
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
        let target = self.resolve_name(&name, &role, scope);
        let write = is_write(path);
        let qualifier = path.field() == Some("object")
            && path.parent().is_some_and(|member| {
                member.node().kind() == "member_expression"
                    && member.field() == Some("function")
                    && member.parent().is_some_and(|call| call.node().kind() == "call_expression")
            });
        self.analysis.references.push(Reference {
            span,
            name,
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

    fn resolve_name(&self, name: &str, role: &Role, scope: ScopeId) -> Option<SymbolId> {
        use SymbolKind as K;
        match role {
            Role::Value => self
                .lookup(name, scope, LOCAL_VALUES)
                .or_else(|| self.lookup(name, scope, &[K::VertexType, K::EdgeType, K::TupleType, K::Query])),
            Role::GlobalAccumulator => self.lookup(name, scope, &[K::GlobalAccumulator]),
            Role::LocalAccumulator => self.lookup(name, scope, &[K::LocalAccumulator]),
            Role::VertexSource => self
                .lookup(name, scope, &[K::VertexSet, K::Parameter, K::Variable, K::LoopVariable, K::Alias])
                .or_else(|| self.lookup(name, scope, &[K::VertexType])),
            Role::EdgeSource => self
                .lookup(name, scope, &[K::Parameter, K::Variable, K::LoopVariable])
                .or_else(|| self.lookup(name, scope, &[K::EdgeType])),
            Role::VertexType => self.lookup(name, scope, &[K::VertexType]),
            Role::EdgeType => self.lookup(name, scope, &[K::EdgeType]),
            // INSERT INTO EDGE may name the type through a parameter.
            Role::SchemaType => self
                .lookup(name, scope, &[K::Parameter, K::Variable])
                .or_else(|| self.lookup(name, scope, &[K::VertexType, K::EdgeType])),
            Role::Function => self.lookup(name, scope, &[K::TupleType, K::Query]),
            Role::Graph => self.lookup(name, scope, &[K::Graph]),
            Role::Query => self.lookup(name, scope, &[K::Query]),
            Role::Job => self.lookup(name, scope, &[K::LoadingJob, K::SchemaChangeJob]),
            Role::TupleType => self.lookup(name, scope, &[K::TupleType, K::AccumulatorType]),
            Role::TupleField(tuple) => self.lookup_field(tuple, name, scope),
            Role::Exception => self.lookup(name, scope, &[K::Exception]),
            Role::JobLocal => self.lookup(name, scope, &[K::FilenameVariable, K::Header, K::LineFilter, K::TempTable]),
            Role::Alias => self.lookup(name, scope, &[K::Alias]),
            Role::JobFile(job) => self.analysis.symbols.iter().position(|s| {
                s.kind == K::FilenameVariable && s.name == name && s.owner.as_deref() == Some(job.as_str())
            }),
            Role::TempColumn(table) => self.analysis.scope_chain(scope).find_map(|scope| {
                self.named(scope, name).iter().copied().find(|&id| {
                    let s = &self.analysis.symbols[id];
                    s.kind == K::TempColumn && s.owner.as_deref() == Some(table.as_str())
                })
            }),
            Role::Attribute(ty) => {
                let owners = match ty {
                    Ty::Vertex(types) | Ty::Edge(types) | Ty::VertexSet(types) => types.clone(),
                    Ty::Tuple(tuple) => return self.lookup_field(tuple, name, scope),
                    _ => Vec::new(),
                };
                // Only resolve within this file when the owner is known.
                owners.iter().find_map(|owner| {
                    self.analysis.scope_chain(scope).find_map(|scope| {
                        self.named(scope, name).iter().copied().find(|&id| {
                            let s = &self.analysis.symbols[id];
                            s.kind == K::Attribute && s.owner.as_deref() == Some(owner.as_str())
                        })
                    })
                })
            }
            Role::Method(_) => None,
        }
    }

    /// The role of an identifier occurrence, or `None` for names that do not
    /// refer to anything (option keys, JSON keys, ...).
    fn role_of(&self, path: Path, scope: ScopeId) -> Option<Role> {
        let node = path.node();
        match node.kind() {
            "global_accumulator" => return Some(Role::GlobalAccumulator),
            "local_accumulator" => return Some(Role::LocalAccumulator),
            _ => {}
        }
        let parent_path = path.parent()?;
        let parent = parent_path.node();
        let field = path.field();
        if node.kind() == "type_identifier" {
            // `BitwiseOrAccum<len>`: the bit width may be a parameter.
            let bit_width = parent.kind() == "accumulator_type"
                && syntax::field_text(parent, "kind", self.source)
                    .is_some_and(|k| k.to_ascii_lowercase().starts_with("bitwise"));
            return Some(if bit_width { Role::Value } else { Role::TupleType });
        }
        let kind_of = |n: Node| n.child_by_field_name("kind").map(|k| k.kind().to_string());
        let role = match (parent.kind(), field) {
            ("option_assignment", Some("key")) => Role::JobFile(self.run_job_of(parent_path)?),
            ("option_assignment", Some("value")) => {
                let in_using = parent_path.ancestors().any(|a| matches!(a.kind(), "using_clause" | "option_clause"));
                if in_using { Role::JobLocal } else { return None }
            }
            ("aliased_expression", Some("alias")) => return None,
            ("pair", Some("key")) => {
                // `(p:Person {name: "Adam"})` constrains attributes of the pattern's types.
                let pattern = parent_path.parent().filter(|p| p.node().kind() == "property_map")?.parent()?.node();
                let types: Vec<String> = syntax::children_by_field(pattern, "type")
                    .into_iter()
                    .filter(|t| t.kind() == "identifier")
                    .map(|t| self.text(t).to_string())
                    .collect();
                match pattern.kind() {
                    "node_pattern" => Role::Attribute(Ty::Vertex(types)),
                    "relationship_detail" => Role::Attribute(Ty::Edge(types)),
                    _ => return None,
                }
            }
            // Fields of an anonymous `TUPLE<INT a, STRING b>` (TYPEDEF fields are declarations).
            ("tuple_field", Some("name")) => return None,
            ("graph_definition", Some("admin")) | ("tag_expression" | "tags_clause", _) => return None,
            ("graph_definition", Some("base")) => Role::Graph,
            ("post_accum_clause", Some("alias")) => Role::Alias,
            ("add_to_graph_statement", Some("name")) => match kind_of(parent).as_deref() {
                Some("EDGE") => Role::EdgeType,
                _ => Role::VertexType,
            },
            ("add_to_graph_statement" | "drop_statement", Some("graph")) => Role::Graph,
            ("alter_type_statement", Some("from" | "to")) => Role::VertexType,
            ("api_clause" | "syntax_clause", _) => return None,
            ("group_by_field", Some("name")) => return None,
            ("alter_type_statement", Some("index")) => return None,
            ("data_source_definition", Some("type" | "config")) => return None,
            ("tag_statement", _) => return None,
            // `SHOW JOB name`: the name of a loading job or schema change job.
            ("show_statement", None)
                if node.kind() == "identifier"
                    && kind_of(parent).as_deref() == Some("JOB")
                    && !self.text(node).eq_ignore_ascii_case("all") =>
            {
                Role::Job
            }
            // `SHOW QUERY h`: the names after the kind are queries.
            ("show_statement", None) if kind_of(parent).as_deref() == Some("QUERY") && self.is_shown_query(node) => {
                Role::Query
            }
            ("show_statement" | "security_statement" | "shell_command" | "grant_statement" | "revoke_statement", _) => {
                return None;
            }
            // Function and description patterns, row policies and DATA_SOURCE grants name things outside the schema.
            (
                "name_pattern"
                | "description_statement"
                | "row_policy_statement"
                | "data_source_grant_statement"
                | "install_function_statement",
                _,
            ) => return None,
            ("column_list", _) => return None,
            ("define_filename_statement", Some("path")) => return None,
            ("command_option", _) => return None,
            ("graph_definition", Some("member")) => Role::SchemaType,
            ("edge_pair", Some("from" | "to")) => Role::VertexType,
            ("primary_key_constraint", Some("attribute")) => Role::Attribute(owner_type(parent_path, self.source)),
            ("discriminator", _) => Role::Attribute(owner_type(parent_path, self.source)),
            ("vertex_type", Some("type")) => Role::VertexType,
            ("edge_type", Some("type")) => Role::EdgeType,
            ("sort_key", Some("name")) => {
                // HeapAccum<Tuple>(n, field DESC): the field belongs to the tuple.
                let tuple = parent_path
                    .ancestors()
                    .find(|a| a.kind() == "accumulator_type")
                    .and_then(|a| syntax::children_by_field(a, "argument").into_iter().next())
                    .filter(|a| a.kind() == "type_identifier")
                    .map(|a| self.text(a).to_string())?;
                Role::TupleField(tuple)
            }
            ("for_graph_clause", _) | ("use_statement", _) | ("alter_graph_statement", Some("name")) => Role::Graph,
            ("alter_graph_statement", Some("member")) => Role::SchemaType,
            ("install_query_statement" | "run_query_statement", _) => Role::Query,
            ("interpret_query_statement", Some("name")) => Role::Query,
            ("run_job_statement", _) => Role::Job,
            ("drop_statement", Some("name")) => match kind_of(parent).as_deref() {
                Some("VERTEX") => Role::VertexType,
                Some("EDGE") => Role::EdgeType,
                Some("GRAPH") => Role::Graph,
                Some("QUERY") | Some("FUNCTION") => Role::Query,
                Some("JOB") => Role::Job,
                Some("TUPLE") => Role::TupleType,
                _ => return None,
            },
            ("alter_type_statement", Some("name")) => match kind_of(parent).as_deref() {
                Some("EDGE") => Role::EdgeType,
                _ => Role::VertexType,
            },
            ("alter_type_statement", Some("attribute")) => {
                let owner = syntax::field_text(parent, "name", self.source).unwrap_or("").to_string();
                match kind_of(parent).as_deref() {
                    Some("EDGE") => Role::Attribute(Ty::Edge(vec![owner])),
                    _ => Role::Attribute(Ty::Vertex(vec![owner])),
                }
            }
            ("insert_statement", Some("target")) => Role::SchemaType,
            ("insert_columns", _) => {
                let target = parent_path
                    .ancestors()
                    .find(|a| a.kind() == "insert_statement")
                    .and_then(|i| syntax::field_text(i, "target", self.source))
                    .unwrap_or("")
                    .to_string();
                Role::Attribute(Ty::Vertex(vec![target]))
            }
            ("typed_value", Some("type")) => Role::VertexType,
            ("delete_statement" | "update_statement" | "dml_delete_statement", Some("alias")) => Role::Alias,
            ("vertex_pattern", Some("type")) | ("vertex_type_alternation", _) => Role::VertexSource,
            ("edge_atom", Some("name")) => Role::EdgeSource,
            ("node_pattern", Some("alias")) => Role::Alias,
            ("node_pattern", Some("type")) => Role::VertexSource,
            ("relationship_detail", Some("type")) => Role::EdgeType,
            ("type_wildcard", Some("type")) => Role::VertexType,
            ("vertex_set_declaration", Some("type")) => Role::VertexType,
            ("raise_statement" | "exception_handler", Some("exception")) => Role::Exception,
            ("load_statement", Some("source")) => Role::JobLocal,
            ("load_destination", Some("target")) => match kind_of(parent).as_deref() {
                Some("EDGE") => Role::EdgeType,
                Some("TEMP_TABLE") => Role::JobLocal,
                _ => Role::VertexType,
            },
            ("load_destination", Some("attribute")) => {
                let owner = syntax::field_text(parent, "target", self.source).unwrap_or("").to_string();
                Role::Attribute(Ty::Vertex(vec![owner]))
            }
            ("loading_delete_statement", Some("target")) => match kind_of(parent).as_deref() {
                Some("EDGE") => Role::EdgeType,
                _ => Role::VertexType,
            },
            ("loading_delete_statement", Some("source")) => Role::JobLocal,
            ("member_expression", Some("property")) => {
                let object_ty =
                    parent.child_by_field_name("object").map(|o| self.expression_type(o, scope)).unwrap_or_default();
                let is_call = parent_path.field() == Some("function")
                    && parent_path.parent().is_some_and(|g| g.node().kind() == "call_expression");
                if is_call { Role::Method(object_ty) } else { Role::Attribute(object_ty) }
            }
            // `LOG(cond, "message");` is the LOG statement, a different construct from the
            // `log(num)` function: a statement-position call with a second argument or a
            // BOOL first argument must not resolve to the built-in function.
            ("call_expression", Some("function")) if self.is_log_statement(parent_path, node) => return None,
            ("call_expression", Some("function")) => Role::Function,
            // `SelectVertex(file, $0, Person, ",", true)`: the third argument may name a vertex type.
            ("argument_list", None) if self.is_select_vertex_type(parent_path, node) => Role::VertexType,
            ("qualified_identifier", _) => return None,
            _ => {
                if node.kind() == "qualified_identifier" {
                    match parent.kind() {
                        "install_query_statement" | "run_query_statement" | "drop_statement" => Role::Query,
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
    /// The job named by the `RUN LOADING JOB` whose USING clause holds the
    /// option assignment `assignment`.
    fn run_job_of(&self, assignment: Path) -> Option<String> {
        let using = assignment.parent()?;
        let run = using.parent()?.node();
        let is_loading = run.kind() == "run_job_statement"
            && using.node().kind() == "using_clause"
            && run.children(&mut run.walk()).any(|c| c.kind() == "LOADING");
        if !is_loading {
            return None;
        }
        syntax::field_text(run, "job", self.source).map(str::to_string)
    }

    /// A `$"col"` of a `LOAD TEMP_TABLE t` reads a column of `t`.
    fn column_occurrence(&mut self, path: Path, context: Context, in_error: bool) {
        let node = path.node();
        let text = self.text(node);
        let Some(inner) = text.strip_prefix("$\"").and_then(|t| t.strip_suffix('"')) else {
            return;
        };
        if inner.is_empty() || inner.contains('"') {
            return;
        }
        let Some(load) = path.ancestors().find(|a| a.kind() == "load_statement") else {
            return;
        };
        if !load.children(&mut load.walk()).any(|c| c.kind() == "TEMP_TABLE") {
            return;
        }
        let Some(table) = syntax::field_text(load, "source", self.source) else {
            return;
        };
        let inner = inner.to_string();
        let table = table.to_string();
        let span = Span { start: node.start_byte() + 2, end: node.end_byte() - 1 };
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

    /// Whether the call at `call` whose function name is `node` is the LOG statement.
    fn is_log_statement(&self, call: Path, node: Node) -> bool {
        if !self.text(node).eq_ignore_ascii_case("log")
            || !call.parent().is_some_and(|p| p.node().kind() == "expression_statement")
        {
            return false;
        }
        let Some(arguments) = call.node().child_by_field_name("arguments") else { return false };
        let args: Vec<Node> = syntax::code_children(arguments).into_iter().filter(|a| a.kind() != "comment").collect();
        args.len() >= 2 || args.first().is_some_and(|a| a.kind() == "boolean")
    }

    /// Whether `node` is a query name of `SHOW QUERY a, b`. The statement takes free-form
    /// arguments and may run into the next line, so only a name right after the kind or a
    /// comma counts (not `ALL`, nor the words of a following command).
    fn is_shown_query(&self, node: Node) -> bool {
        node.kind() == "identifier"
            && !self.text(node).eq_ignore_ascii_case("all")
            && node.prev_sibling().is_some_and(|p| {
                p.kind() == "," || p.kind() == "QUERY" && p.end_position().row == node.start_position().row
            })
    }

    fn is_select_vertex_type(&self, arguments: Path, node: Node) -> bool {
        let is_select_vertex = arguments
            .parent()
            .filter(|call| call.node().kind() == "call_expression")
            .and_then(|call| call.node().child_by_field_name("function"))
            .is_some_and(|function| self.text(function).eq_ignore_ascii_case("selectvertex"));
        is_select_vertex
            && syntax::code_children(arguments.node())
                .into_iter()
                .filter(|argument| argument.kind() != "comment")
                .position(|argument| argument.id() == node.id())
                == Some(2)
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

/// A node and its ancestors, as tracked by a cursor walk.
#[derive(Clone, Copy)]
struct Path<'a, 't> {
    /// Root first; each entry holds a node and its field name in the parent.
    stack: &'a [(Node<'t>, Option<&'t str>)],
}

impl<'a, 't> Path<'a, 't> {
    fn node(&self) -> Node<'t> {
        self.stack[self.stack.len() - 1].0
    }

    fn field(&self) -> Option<&'t str> {
        self.stack[self.stack.len() - 1].1
    }

    fn parent(&self) -> Option<Path<'a, 't>> {
        (self.stack.len() > 1).then(|| Path { stack: &self.stack[..self.stack.len() - 1] })
    }

    /// Ancestors, innermost first (excluding the node itself).
    fn ancestors(&self) -> impl Iterator<Item = Node<'t>> + 'a {
        self.stack[..self.stack.len() - 1].iter().rev().map(|(node, _)| *node)
    }
}

/// The vertex or edge type that encloses an attribute list item.
fn owner_type(path: Path, source: &str) -> Ty {
    for ancestor in std::iter::once(path.node()).chain(path.ancestors()) {
        match ancestor.kind() {
            "vertex_definition" => {
                return Ty::Vertex(
                    syntax::field_text(ancestor, "name", source).into_iter().map(String::from).collect(),
                );
            }
            "edge_definition" | "virtual_edge_declaration" => {
                return Ty::Edge(syntax::field_text(ancestor, "name", source).into_iter().map(String::from).collect());
            }
            // `INSERT INTO e (FROM, TO, DISCRIMINATOR(ts))`
            "insert_statement" => {
                return Ty::Edge(
                    syntax::field_text(ancestor, "target", source).into_iter().map(String::from).collect(),
                );
            }
            _ => {}
        }
    }
    Ty::Unknown
}

/// Like [`enter`], for a node at `depth` in the tree. Constructs that change
/// the context are top-level statements or statements of a job body (depth
/// 3 at most), so deeper nodes inherit their parent's context.
fn enter_at(outer: Context, node: Node, depth: usize) -> Context {
    if depth > 3 { outer } else { enter(outer, node.kind()) }
}

/// The context of a node of kind `kind` whose parent is in `outer`.
fn enter(outer: Context, kind: &str) -> Context {
    match (outer, kind) {
        // Schema edits win over everything they contain.
        (_, "schema_change_job_definition" | "drop_statement" | "alter_type_statement" | "alter_graph_statement") => {
            Context::SchemaEdit
        }
        (Context::SchemaEdit, _) => Context::SchemaEdit,
        (_, "query_definition" | "interpret_query_statement" | "opencypher_query_definition") => Context::Query,
        (_, "loading_job_definition") => Context::LoadingJob,
        (Context::Command, "vertex_definition" | "edge_definition" | "graph_definition" | "typedef_statement") => {
            Context::Definition
        }
        (outer, _) => outer,
    }
}

fn is_write(path: Path) -> bool {
    let mut current = path;
    while let Some(parent) = current.parent() {
        match parent.node().kind() {
            "assignment_statement" => return current.field() == Some("left"),
            "member_expression" if current.field() == Some("property") => current = parent,
            "subscript_expression" if current.field() == Some("object") => current = parent,
            _ => return false,
        }
    }
    false
}

fn for_graph(node: Node, source: &str) -> Option<String> {
    syntax::code_children(node)
        .into_iter()
        .find(|c| c.kind() == "for_graph_clause")
        .and_then(|c| syntax::field_text(c, "graph", source))
        .map(String::from)
}

fn parameters(node: Node, source: &str) -> Vec<Param> {
    let Some(list) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    syntax::code_children(list)
        .into_iter()
        .filter(|p| p.kind() == "parameter")
        .map(|p| Param {
            name: syntax::field_text(p, "name", source).unwrap_or("").to_string(),
            ty: p.child_by_field_name("type").map(|t| collapse_whitespace(syntax::text(t, source))).unwrap_or_default(),
            default: p.child_by_field_name("default").map(|d| collapse_whitespace(syntax::text(d, source))),
        })
        .collect()
}

/// Comments documenting a declaration: those preceding it, or for
/// statement-level declarations, those preceding the enclosing statement.
fn doc_comment(node: Node, source: &str) -> Option<String> {
    syntax::leading_comments(node, source).or_else(|| {
        let parent = node.parent()?;
        if matches!(parent.kind(), "variable_declaration" | "accumulator_declaration") {
            syntax::leading_comments(parent, source)
        } else {
            None
        }
    })
}
