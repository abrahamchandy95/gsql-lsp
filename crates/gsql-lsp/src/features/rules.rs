//! Language-reference rules the grammar does not enforce.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::LazyLock;

use tree_sitter::Node;

use crate::analysis::{
    FILE_SCOPE, Symbol, SymbolKind, Ty, type_of_type_node,
};
use crate::builtins;
use crate::features::diagnostics::{add_fix, diagnostic};
use crate::features::resolve::{self, Callee, Target};
use crate::features::{Snapshot, plural};
use crate::lsp::types::{Diagnostic, diagnostic_tag, severity};
use crate::syntax::{self, field, keyword, kind};
use crate::text::{Span, starts_with_ignore_ascii_case};
use crate::workspace::GlobalSymbol;

/// Words the query compiler rejects as user-defined identifiers.
const QUERY_RESERVED: &str = "\
ACCUM AND ANY API AS ASC AVG BAG BATCH BETWEEN BOOL BOTH BREAK CASE CATCH COALESCE COMPRESS CONTINUE \
COUNT CREATE DATETIME DATETIME_ADD DATETIME_SUB DELETE DESC DISTRIBUTED DO DOUBLE EDGE ELSE END \
ESCAPE EXCEPTION FALSE FILTER FLOAD FOR FOREACH FROM GRAPH HAVING IF IN INT INTERPRET INTERSECT \
INTERVAL INTO IS ISEMPTY JSONARRAY JSONOBJECT LEADING LIKE LIMIT LOADACCUM MAX MIN MINUS NOT NULL \
OFFSET OR ORDER PINNED POST_ACCUM PRIMARY_ID PRINT QUERY RAISE RANGE RETURN RETURNS RUN SAMPLE \
SELECT SELECTVERTEX SET STATIC STRING SUM SYNTAX TARGET TAGS THEN TO TO_CSV TRAILING TRIM TRUE TRY \
TUPLE TYPEDEF UINT UNION VALUES VERTEX WHEN WHILE WITH GSQL_INT_MAX GSQL_INT_MIN GSQL_UINT_MAX \
RESET_COLLECTION_ACCUM";

/// C++ keywords, reserved because queries compile to C++ (case-sensitive).
const CPP_RESERVED: &str = "\
alignas alignof and and_eq asm auto bitand bitor bool break case catch char char16_t char32_t class \
compl concept const constexpr const_cast continue decltype default delete do double dynamic_cast \
else enum explicit export extern false float for friend goto if inline int long mutable namespace \
new noexcept not not_eq nullptr operator or or_eq private protected public register reinterpret_cast \
requires return short signed sizeof static static_assert static_cast struct switch template this \
thread_local throw true try typedef typeid typename union unsigned using virtual void volatile \
wchar_t while xor xor_eq";

/// Names the DDL compiler rejects for types, graphs, tags and attributes.
const DDL_RESERVED: &str = "\
ACCUM ADD ALL ALLOCATE ALTER AND ANY AS ASC AVG BAG BATCH BETWEEN BIGINT BLOB BOOL BOOLEAN BOTH \
BREAK BY CALL CASCADE CASE CATCH CHAR CHARACTER CHECK CLOB COALESCE COMPRESS CONST CONSTRAINT \
CONTINUE COST COUNT CREATE CURRENT_DATE CURRENT_TIME CURRENT_TIMESTAMP CURSOR KAFKA S3 DATETIME \
DATETIME_ADD DATETIME_SUB DAY DATETIME_DIFF DATETIME_TO_EPOCH DATETIME_FORMAT DECIMAL DECLARE DELETE \
DESC DISTRIBUTED DO DOUBLE DROP EDGE ELSE ELSEIF EPOCH_TO_DATETIME END ESCAPE EXCEPTION EXISTS FALSE \
FILE FILTER FIXED_BINARY FLOAT FOR FOREACH FROM GLOBAL GRANTS GRAPH GROUP GROUPBYACCUM HAVING HOUR \
HEADER HEAPACCUM IF IGNORE IN INDEX INPUT_LINE_FILTER INSERT INT INTERSECT INT8 INT16 INT32 INT32_T \
INT64_T INTEGER INTERPRET INTO IS ISEMPTY JOB JOIN JSONARRAY JSONOBJECT KEY LEADING LIKE LIMIT LIST \
LOAD LOADACCUM LOG LONG MAP MINUTE NOBODY NOT NOW NULL NULLABLE OFFSET ON OPENCYPHER OR ORDER PINNED \
POLICY POST_ACCUM PRIMARY PRIMARY_ID PRINT PROXY QUERY QUIT RAISE RANGE REDUCE REPLACE \
RESET_COLLECTION_ACCUM RETURN RETURNS ROW SAMPLE SECOND SELECT SELECTVERTEX SET STATIC STRING SUM \
TARGET TEMP_TABLE THEN TO TO_CSV TO_DATETIME TRAILING TRANSLATESQL TRIM TRUE TRY TUPLE TYPE TYPEDEF \
UINT UINT8 UINT16 UINT32 UINT8_T UINT32_T UINT64_T UNION UPDATE UPSERT USING VALUES VERTEX WHEN \
WHERE WHILE WITH GSQL_SYS_TAG _INTERNAL_ATTR_TAG";

/// Prefix of names reserved for the system.
const RESERVED_PREFIX: &str = "gsql_sys_";

pub(crate) fn is_query_reserved(name: &str) -> bool {
    static QUERY: LazyLock<Vec<&str>> =
        LazyLock::new(|| sorted(QUERY_RESERVED));
    static CPP: LazyLock<HashSet<&str>> =
        LazyLock::new(|| CPP_RESERVED.split_whitespace().collect());
    listed(&QUERY, name) || CPP.contains(name)
}

pub(crate) fn is_ddl_reserved(name: &str) -> bool {
    static DDL: LazyLock<Vec<&str>> = LazyLock::new(|| sorted(DDL_RESERVED));
    listed(&DDL, name) || starts_with_ignore_ascii_case(name, RESERVED_PREFIX)
}

/// `a` and `b` compared as if in upper case.
fn cmp_upper(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_uppercase())
        .cmp(b.bytes().map(|c| c.to_ascii_uppercase()))
}

/// The words of a list, sorted for [`listed`].
fn sorted(words: &'static str) -> Vec<&'static str> {
    let mut sorted: Vec<_> = words.split_whitespace().collect();
    sorted.sort_unstable_by(|a, b| cmp_upper(a, b));
    sorted
}

/// Whether `name` is one of `words`, ignoring ASCII case.
fn listed(words: &[&str], name: &str) -> bool {
    words
        .binary_search_by(|w| cmp_upper(w, name))
        .is_ok()
}

pub fn check(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    reserved_words(snapshot, out);
    let type_symbols = snapshot
        .analysis
        .symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::AccumulatorType | SymbolKind::TupleType
            )
        })
        .collect();
    let mut checker = Checker {
        snapshot,
        out,
        cypher_reported: None,
        arrow_typos: HashSet::new(),
        type_symbols,
    };
    let root = snapshot.root();
    let mut cursor = root.walk();
    // The context of each ancestor of the current node.
    let mut contexts = vec![Context::default()];
    loop {
        let node = cursor.node();
        // Read once: every check asks.
        let kind = node.kind_id();
        let context = enter(
            *contexts.last().expect("never empty"),
            node,
            kind,
            snapshot.text(),
        );
        checker.visit(node, kind, &context);
        if !node.is_error() && cursor.goto_first_child() {
            contexts.push(context);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            contexts.pop();
        }
    }
}

/// How a query runs, which decides what it may use.
#[derive(Debug, Clone, Copy, Default)]
struct Mode {
    interpreted: bool,
    distributed: bool,
    v3: bool,
    /// Declared `SYNTAX V1` or `SYNTAX V2`.
    old_syntax: bool,
    /// Where the query starts, to tell queries apart.
    start: usize,
}

/// Where a node is.
#[derive(Debug, Clone, Copy, Default)]
struct Context {
    /// Inside a query, running in this mode.
    query: Option<Mode>,
    accum: bool,
    post_accum: bool,
    print: bool,
    /// Inside a FOREACH or WHILE loop.
    foreach: bool,
    while_loop: bool,
    insert: bool,
    /// The node is a ListAccum type, or its parent is.
    list_accum: bool,
    in_list_accum: bool,
    returns: Returns,
}

/// What a query returns.
#[derive(Debug, Clone, Copy, Default)]
enum Returns {
    /// Not in a GSQL query.
    #[default]
    Unknown,
    /// No RETURNS clause.
    Missing,
    /// How the RETURNS type reads when a string cannot be returned as it.
    Declared(Option<&'static str>),
}

fn enter(parent: Context, node: Node, kind: u16, source: &str) -> Context {
    let mut context = parent;
    context.in_list_accum = parent.list_accum;
    context.list_accum = kind == kind::ACCUMULATOR_TYPE
        && syntax::field_text(node, "kind", source)
            .is_some_and(|k| k.eq_ignore_ascii_case("listaccum"));
    match kind {
        kind::QUERY_DEFINITION
        | kind::INTERPRET_QUERY_STATEMENT
        | kind::OPENCYPHER_QUERY_DEFINITION => {
            let versions = syntax::declared_syntax_versions(node, source);
            let v3 = versions
                .iter()
                .any(|version| version.eq_ignore_ascii_case("v3"));
            let old_syntax = versions.iter().any(|v| {
                v.eq_ignore_ascii_case("v1") || v.eq_ignore_ascii_case("v2")
            });
            let distributed = node
                .child_by_field_id(field::MODIFIER)
                .is_some_and(|m| m.kind_id() == keyword::DISTRIBUTED);
            let interpreted = kind == kind::INTERPRET_QUERY_STATEMENT;
            context = Context {
                query: Some(Mode {
                    interpreted,
                    distributed,
                    v3,
                    old_syntax,
                    start: node.start_byte(),
                }),
                ..Context::default()
            };
            if kind == kind::QUERY_DEFINITION {
                let clause = syntax::child_of_kind(node, "returns_clause");
                context.returns = match clause {
                    // A query that does not parse cleanly may be misread.
                    None if node.has_error() => Returns::Unknown,
                    None => Returns::Missing,
                    Some(clause) => Returns::Declared(
                        clause
                            .child_by_field_id(field::TYPE)
                            .and_then(|t| {
                                stores_string_badly(&type_of_type_node(
                                    t, source,
                                ))
                            }),
                    ),
                };
            }
        }
        kind::ACCUM_CLAUSE => context.accum = true,
        kind::POST_ACCUM_CLAUSE => context.post_accum = true,
        kind::PRINT_STATEMENT => context.print = true,
        kind::FOREACH_STATEMENT => context.foreach = true,
        kind::WHILE_STATEMENT => context.while_loop = true,
        kind::INSERT_STATEMENT => context.insert = true,
        _ => {}
    }
    context
}

fn reserved_words(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    use SymbolKind as K;
    for symbol in &snapshot.analysis.symbols {
        if symbol.in_error || symbol.copied {
            continue;
        }
        let name = symbol.name.as_str();
        let cannot_name = || {
            let label = with_article(symbol.kind.label());
            (
                severity::ERROR,
                format!(
                    "`{name}` is a reserved word in GSQL and cannot name {label}"
                ),
            )
        };
        let finding = match symbol.kind {
            K::Parameter
            | K::Variable
            | K::VertexSet
            | K::Alias
            | K::LoopVariable
            | K::TupleType
            | K::TupleField
            | K::AccumulatorType
            | K::File
            | K::Exception
            | K::Table
            | K::Query => is_query_reserved(name).then(cannot_name),
            K::VertexType | K::EdgeType | K::Graph | K::Attribute
                if is_ddl_reserved(name) =>
            {
                Some(cannot_name())
            }
            K::VertexType | K::EdgeType if is_query_reserved(name) => Some((
                severity::WARNING,
                format!(
                    "`{name}` is a reserved word in GSQL queries, so queries cannot refer to this {}",
                    symbol.kind.label()
                ),
            )),
            _ => None,
        };
        if let Some((level, message)) = finding {
            out.push(diagnostic(
                snapshot,
                symbol.name_span,
                level,
                "reserved-word",
                message,
            ));
        }
    }
}

/// A MapAccum or GroupByAccum's pairs: types and how messages name them.
struct Pairs<'t> {
    ty: &'t Ty,
    keys: &'t [Ty],
    values: &'t [Ty],
    what: String,
    shape: String,
}

struct Checker<'s, 'a> {
    snapshot: &'s Snapshot<'a>,
    out: &'s mut Vec<Diagnostic>,
    /// The query whose cypher patterns were reported (once per query).
    cypher_reported: Option<usize>,
    /// Mistyped `->` (`(k - v)`) already reported, not to report twice.
    arrow_typos: HashSet<usize>,
    /// The TYPEDEF'd accumulator and tuple types declared in this file.
    type_symbols: Vec<&'s Symbol>,
}

impl<'s> Checker<'s, '_> {
    fn text(&self, node: Node) -> &str {
        syntax::text(node, self.snapshot.text())
    }

    fn report(&mut self, node: Node, level: u8, code: &str, message: String) {
        self.out.push(diagnostic(
            self.snapshot,
            Span::of(node),
            level,
            code,
            message,
        ));
    }

    /// The symbol `node` resolves to in this file.
    fn symbol_of(&self, node: Node) -> Option<&Symbol> {
        let reference = self
            .snapshot
            .analysis
            .reference_at(node.start_byte())?;
        (reference.span == Span::of(node)).then_some(())?;
        self.snapshot
            .analysis
            .target_symbol(reference)
    }

    /// Whether `method` modifies the accumulator `accumulator` names.
    fn calls_mutator(&self, accumulator: Node, method: Node) -> bool {
        let Some(Ty::Accumulator(kind, _)) =
            self.symbol_of(accumulator).map(|s| &s.ty)
        else {
            return false;
        };
        builtins::accumulator(kind)
            .and_then(|a| builtins::find_method(a.methods, self.text(method)))
            .is_some_and(|m| m.mutator)
    }

    fn visit(&mut self, node: Node, kind: u16, context: &Context) {
        if matches!(
            kind,
            kind::ASSIGNMENT_STATEMENT | kind::ACCUMULATOR_DECLARATOR
        ) {
            self.accumulator_input(node);
        }
        self.type_checks(node, kind, context);
        match kind {
            kind::ASSIGNMENT_STATEMENT
                if context.accum || context.post_accum =>
            {
                self.accumulator_assignment(node)
            }
            kind::LOCAL_ACCUMULATOR if context.post_accum => {
                self.edge_accumulator_in_post_accum(node)
            }
            kind::ACCUMULATOR_DECLARATION
                if context.foreach || context.while_loop =>
            {
                self.attached_accumulator_in_loop(
                    node,
                    if context.foreach { "FOREACH" } else { "WHILE" },
                )
            }
            kind::VIRTUAL_EDGE_DECLARATION => self.virtual_edge(node),
            kind::FOREACH_STATEMENT if !node.has_error() => {
                self.foreach_variables(node)
            }
            // An accumulator parameter: an error, a warning for a subquery.
            kind::PARAMETER => {
                if let Some(ty) = node
                    .child_by_field_id(field::TYPE)
                    .filter(|t| t.kind_id() == kind::ACCUMULATOR_TYPE)
                {
                    let (level, message) = if matches!(
                        context.returns,
                        Returns::Missing
                    ) {
                        (
                            severity::ERROR,
                            "A query parameter cannot be an accumulator; declare the accumulator in the query body",
                        )
                    } else {
                        (
                            severity::WARNING,
                            "A query parameter is usually not an accumulator; pass a SET, BAG, LIST or MAP instead",
                        )
                    };
                    self.report(
                        ty,
                        level,
                        "accumulator-type",
                        message.to_string(),
                    );
                }
            }
            kind::EDGE_ALTERNATION | kind::RELATIONSHIP_DETAIL => {
                self.mixed_virtual_edges(node)
            }
            // An accumulator as a parameter type is reported once, as that.
            kind::ACCUMULATOR_TYPE
                if node
                    .parent()
                    .is_none_or(|p| p.kind_id() != kind::PARAMETER) =>
            {
                self.accumulator_type(node, context)
            }
            kind::ACCUMULATOR_KIND => self.accumulator_case(node),
            kind::BINARY_EXPRESSION => {
                if context.query.is_some() {
                    self.compared_condition(node);
                }
                if let Some(mode) = context.query.filter(|_| {
                    self.snapshot
                        .config
                        .diagnostics_float_equality
                }) {
                    self.float_equality(node, mode);
                }
            }
            kind::CALL_EXPRESSION => {
                if context.query.is_some() {
                    self.set_expression_method(node);
                }
                self.mutator(node, context);
                self.accumulator_method(node);
                if context.query.is_some() {
                    self.subquery_call(node);
                }
            }
            kind::CYPHER_PATTERN => self.cypher_syntax(node, context),
            kind::BREAK_STATEMENT | kind::CONTINUE_STATEMENT
                if context.query.is_some()
                    && !context.foreach
                    && !context.while_loop =>
            {
                let message = format!(
                    "{} is only valid inside a WHILE or FOREACH loop",
                    node.kind()
                        .replace("_statement", "")
                        .to_uppercase()
                );
                self.report(node, severity::ERROR, "loop-control", message);
            }
            kind::RETURN_STATEMENT => self.return_value(node, context),
            kind::LIMIT_CLAUSE => self.offset_without_order(node),
            kind::SELECT_STATEMENT if !node.has_error() => {
                self.select_aliases(node)
            }
            kind::ACCUM_CLAUSE if !node.has_error() => {
                self.read_after_write(node)
            }
            kind::EDGE_PATTERN if !node.has_error() => {
                self.aliased_repetition(node)
            }
            kind::TAG_STATEMENT
            | kind::TAG_EXPRESSION
            | kind::TAGS_CLAUSE => self.reserved_tags(node),
            _ => {}
        }
        self.deprecated(node, kind);
        if let Some(mode) = context.query {
            if mode.interpreted {
                self.interpreted(node, kind, context);
            }
            self.distributed(node, kind, mode, context);
        }
    }

    /// Global accumulators may only be assigned outside SELECT blocks.
    fn accumulator_assignment(&mut self, node: Node) {
        let Some(left) = node.child_by_field_id(field::LEFT) else {
            return;
        };
        let assigns = node
            .child_by_field_id(field::OPERATOR)
            .is_some_and(|o| o.kind() == "=");
        let global = match left.kind_id() {
            kind::GLOBAL_ACCUMULATOR => true,
            kind::SUBSCRIPT_EXPRESSION => left
                .child_by_field_id(field::OBJECT)
                .is_some_and(|o| o.kind_id() == kind::GLOBAL_ACCUMULATOR),
            _ => false,
        };
        if assigns && global {
            self.report(
                left,
                severity::ERROR,
                "accumulator-assignment",
                "Global accumulators cannot be assigned with `=` inside ACCUM or POST-ACCUM; accumulate with `+=` or assign outside the SELECT block".into(),
            );
        }
    }

    fn edge_accumulator_in_post_accum(&mut self, node: Node) {
        if self
            .symbol_of(node)
            .is_some_and(|s| s.on_edges)
        {
            let message = format!(
                "Edge accumulators such as `{}` cannot be used in POST-ACCUM",
                self.text(node)
            );
            self.report(node, severity::ERROR, "edge-accumulator", message);
        }
    }

    /// A read after a write in one ACCUM clause: reads see the old value.
    fn read_after_write(&mut self, clause: Node) {
        let mut writes = Vec::new();
        let mut reads = Vec::new();
        syntax::walk(clause, |n| match n.kind_id() {
            kind::ASSIGNMENT_STATEMENT => writes.push(n),
            kind::GLOBAL_ACCUMULATOR => reads.push(n),
            kind::MEMBER_EXPRESSION
                if n.child_by_field_id(field::PROPERTY)
                    .is_some_and(|p| {
                        p.kind_id() == kind::LOCAL_ACCUMULATOR
                    }) =>
            {
                reads.push(n)
            }
            _ => {}
        });
        // The accumulator a write targets is not a read.
        let targets: Vec<(usize, usize)> = writes
            .iter()
            .filter_map(|w| w.child_by_field_id(field::LEFT))
            .filter_map(|left| {
                self.accumulator_key(left)
                    .map(|_| accumulator_target(left))
            })
            .map(|t| (t.start_byte(), t.end_byte()))
            .collect();
        let mut reported = Vec::new();
        for write in writes {
            let Some(key) = write
                .child_by_field_id(field::LEFT)
                .and_then(|l| self.accumulator_key(l))
            else {
                continue;
            };
            // Later reads in the same block (not another IF branch).
            let Some(scope) = write.parent().filter(|p| {
                matches!(p.kind_id(), kind::BLOCK | kind::ACCUM_CLAUSE)
            }) else {
                continue;
            };
            for read in &reads {
                let span = (read.start_byte(), read.end_byte());
                if read.start_byte() < write.end_byte()
                    || read.end_byte() > scope.end_byte()
                    || targets.contains(&span)
                    || reported.contains(&span)
                    || self.is_mutated(*read)
                    || self.accumulator_key(*read).as_deref()
                        != Some(key.as_str())
                {
                    continue;
                }
                reported.push(span);
                let message = format!(
                    "`{}` is read in the same ACCUM clause that updates it; the read sees the value from before the clause, not the running total (read it in POST-ACCUM)",
                    self.text(*read)
                );
                // Only a hint for a plain `=` (the paper's ACCUM has `+=`).
                let level = if write
                    .child_by_field_id(field::OPERATOR)
                    .is_some_and(|o| o.kind() == "=")
                {
                    severity::HINT
                } else {
                    severity::WARNING
                };
                self.report(
                    *read,
                    level,
                    "accumulator-read-after-write",
                    message,
                );
            }
        }
    }

    /// `read` is modified by a method call (`.clear()`, `.add(x)`).
    fn is_mutated(&self, read: Node) -> bool {
        let Some(member) = read
            .parent()
            .filter(|p| p.kind_id() == kind::MEMBER_EXPRESSION)
        else {
            return false;
        };
        let (Some(object), Some(method)) = (
            member.child_by_field_id(field::OBJECT),
            member.child_by_field_id(field::PROPERTY),
        ) else {
            return false;
        };
        if object.id() != read.id()
            || member
                .parent()
                .is_none_or(|c| c.kind_id() != kind::CALL_EXPRESSION)
        {
            return false;
        }
        let accumulator = if read.kind_id() == kind::MEMBER_EXPRESSION {
            read.child_by_field_id(field::PROPERTY)
                .unwrap_or(read)
        } else {
            read
        };
        self.calls_mutator(accumulator, method)
    }

    /// `@@name` or `alias.@name`, subscripted or not.
    fn accumulator_key(&self, node: Node) -> Option<String> {
        let node = accumulator_target(node);
        match node.kind_id() {
            kind::GLOBAL_ACCUMULATOR => Some(self.text(node).to_string()),
            kind::MEMBER_EXPRESSION => {
                let object = node
                    .child_by_field_id(field::OBJECT)
                    .filter(|o| o.kind_id() == kind::IDENTIFIER)?;
                let property = node
                    .child_by_field_id(field::PROPERTY)
                    .filter(|p| p.kind_id() == kind::LOCAL_ACCUMULATOR)?;
                Some(format!("{}.{}", self.text(object), self.text(property)))
            }
            _ => None,
        }
    }

    /// `*2..2`: equal bounds are an exact count.
    fn same_bounds(&self, bounds: Node) -> bool {
        match (
            bounds.child_by_field_id(field::MIN),
            bounds.child_by_field_id(field::MAX),
        ) {
            (Some(min), Some(max)) => self.text(min) == self.text(max),
            _ => false,
        }
    }

    /// An edge alias on a repeated edge pattern binds no single edge.
    fn aliased_repetition(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_id(field::ALIAS) else {
            return;
        };
        let mut star = false;
        if let Some(kind) = node.child_by_field_id(field::TYPE) {
            syntax::walk(kind, |n| {
                // An exact count (`*3`) or a condition is left alone.
                if n.kind_id() == kind::EDGE_REPETITION
                    && !syntax::named_children(n).iter().any(|b| {
                        b.kind_id() == kind::REPETITION_BOUNDS
                            && (b.child_by_field_id(field::EXACT).is_some()
                                || b.child_by_field_id(field::CONDITION)
                                    .is_some()
                                || self.same_bounds(*b))
                    })
                {
                    star = true;
                }
            });
        }
        if star {
            let message = format!(
                "The edge alias `{}` cannot be used with a `*` repetition: the number of edges varies, so it is bound to no single edge",
                self.text(alias)
            );
            self.report(alias, severity::ERROR, "kleene-edge-alias", message);
        }
    }

    /// Rules on a SELECT block's aliases: HAVING, PER and FROM patterns.
    fn select_aliases(&mut self, node: Node) {
        let children = syntax::named_children(node);
        let Some(from) = children
            .iter()
            .find(|c| c.kind_id() == kind::FROM_CLAUSE)
        else {
            return;
        };
        let patterns: Vec<Node> = syntax::named_children(*from)
            .into_iter()
            .filter(|c| {
                matches!(
                    c.kind_id(),
                    kind::PATH_PATTERN | kind::CYPHER_PATTERN
                )
            })
            .collect();
        let vertices: Vec<Vec<Node>> = patterns
            .iter()
            .map(|p| pattern_vertices(*p))
            .collect();
        self.disjoint_patterns(&patterns, &vertices);
        let source = self.snapshot.text();
        let aliases: Vec<&str> = vertices
            .iter()
            .flatten()
            .filter_map(|v| syntax::field_text(*v, "alias", source))
            .collect();
        let results = syntax::children_by_field(node, "result");
        // The vertex aliases the block selects.
        let selected: Vec<Node> = results
            .iter()
            .copied()
            .filter(|r| {
                r.kind_id() == kind::IDENTIFIER
                    && aliases.contains(&self.text(*r))
            })
            .collect();
        for clause in children
            .iter()
            .filter(|c| c.kind_id() == kind::POST_ACCUM_CLAUSE)
        {
            self.non_binding_post_accum(*clause, &aliases, &selected);
        }
        // HAVING applies to the selected vertices: judged for one alias only.
        if let ([one], 1, None, Some(having)) = (
            selected.as_slice(),
            results.len(),
            children
                .iter()
                .find(|c| c.kind_id() == kind::GROUP_BY_CLAUSE),
            children
                .iter()
                .find(|c| c.kind_id() == kind::HAVING_CLAUSE),
        ) {
            let name = self.text(*one).to_string();
            for object in member_objects(*having) {
                let used = self.text(object).to_string();
                if used != name && aliases.contains(&used.as_str()) {
                    let message = format!(
                        "The SELECT block selects `{name}`, but the HAVING clause uses `{used}`"
                    );
                    self.report(
                        object,
                        severity::ERROR,
                        "having-alias",
                        message,
                    );
                }
            }
        }
        // With PER, SELECT and the ACCUM clauses may only use PER's aliases.
        let per = children
            .iter()
            .copied()
            .chain(
                children
                    .iter()
                    .filter(|c| c.kind_id() == kind::ACCUM_CLAUSE)
                    .flat_map(|a| syntax::named_children(*a)),
            )
            .find(|c| c.kind_id() == kind::PER_CLAUSE);
        let Some(per) = per else {
            return;
        };
        let listed: Vec<String> = syntax::named_children(per)
            .into_iter()
            .map(|c| self.text(c).to_string())
            .collect();
        let mut uses = selected;
        for clause in children.iter().filter(|c| {
            matches!(
                c.kind_id(),
                kind::ACCUM_CLAUSE | kind::POST_ACCUM_CLAUSE
            )
        }) {
            uses.extend(
                member_objects(*clause)
                    .into_iter()
                    .filter(|o| aliases.contains(&self.text(*o))),
            );
        }
        for used in uses {
            let name = self.text(used).to_string();
            if !listed.contains(&name) {
                let message = format!(
                    "`{name}` is used here but does not appear in the PER clause"
                );
                self.report(used, severity::ERROR, "per-alias", message);
            }
        }
    }

    /// A POST-ACCUM using no FROM alias binds to none; the compiler warns.
    fn non_binding_post_accum(
        &mut self,
        clause: Node,
        aliases: &[&str],
        selected: &[Node],
    ) {
        if aliases.is_empty()
            || clause
                .child_by_field_id(field::ALIAS)
                .is_some()
        {
            return;
        }
        let mut binds = false;
        syntax::walk(clause, |n| {
            binds |= n.kind_id() == kind::IDENTIFIER
                && aliases.contains(&self.text(n))
        });
        let Some(keyword) = clause.child(0).filter(|_| !binds) else {
            return;
        };
        let mut d = diagnostic(
            self.snapshot,
            Span::of(keyword),
            severity::WARNING,
            "non-binding-post-accum",
            "This POST-ACCUM clause uses no vertex alias; the query compiler warns that non-binding POST-ACCUM clauses will be deprecated soon (WARN-7). Write it as `POST-ACCUM (selectAlias) ...`".into(),
        );
        if let [one] = selected {
            let name = self.text(*one);
            let edit = self
                .snapshot
                .insert(keyword.end_byte(), format!(" ({name})"));
            add_fix(
                &mut d,
                format!("Bind the clause to `{name}`: `POST-ACCUM ({name})`"),
                vec![edit],
                false,
            );
        }
        self.out.push(d);
    }

    /// FROM patterns that share no vertex alias cannot be joined. Judged when
    /// every pattern has an edge and every vertex an alias.
    fn disjoint_patterns(
        &mut self,
        patterns: &[Node],
        vertices: &[Vec<Node>],
    ) {
        if patterns.len() < 2 || vertices.iter().any(|v| v.len() < 2) {
            return;
        }
        let source = self.snapshot.text();
        let names: Vec<Vec<&str>> = vertices
            .iter()
            .map(|v| {
                v.iter()
                    .filter_map(|n| syntax::field_text(*n, "alias", source))
                    .collect()
            })
            .collect();
        if names
            .iter()
            .zip(vertices)
            .any(|(n, v)| n.len() != v.len())
        {
            return;
        }
        // The patterns joined to the first through shared aliases.
        let mut joined = vec![false; patterns.len()];
        joined[0] = true;
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..patterns.len() {
                let shares =
                    |j: usize| names[i].iter().any(|n| names[j].contains(n));
                if !joined[i]
                    && (0..patterns.len()).any(|j| joined[j] && shares(j))
                {
                    joined[i] = true;
                    changed = true;
                }
            }
        }
        if let Some(i) = joined.iter().position(|j| !j) {
            let message = "This pattern shares no vertex alias with the patterns before it, so they cannot be naturally joined";
            self.report(
                patterns[i],
                severity::WARNING,
                "pattern-join",
                message.into(),
            );
        }
    }

    /// OFFSET without ORDER BY: the compiler rejects it.
    fn offset_without_order(&mut self, node: Node) {
        let Some(offset) = syntax::child_of_kind(node, "OFFSET") else {
            return;
        };
        let select = node
            .parent()
            .filter(|p| p.kind_id() == kind::SELECT_STATEMENT);
        if select.is_some_and(|s| !syntax::has_child(s, "order_by_clause")) {
            self.report(
                offset,
                severity::ERROR,
                "limit-offset",
                "OFFSET needs an ORDER BY clause".into(),
            );
        }
    }

    /// Vertex- and edge-attached accumulators cannot be declared in loops.
    fn attached_accumulator_in_loop(&mut self, node: Node, keyword: &str) {
        let attached = if syntax::has_child(node, "EDGE") {
            "Edge"
        } else {
            "Vertex"
        };
        for declarator in syntax::named_children(node)
            .into_iter()
            .filter(|c| c.kind_id() == kind::ACCUMULATOR_DECLARATOR)
        {
            if let Some(name) = declarator
                .child_by_field_id(field::NAME)
                .filter(|n| n.kind_id() == kind::LOCAL_ACCUMULATOR)
            {
                self.report(
                    name,
                    severity::ERROR,
                    "accumulator-declaration",
                    format!("{attached}-attached accumulators cannot be declared inside a {keyword} loop"),
                );
            }
        }
    }

    fn virtual_edge(&mut self, node: Node) {
        let at_top_level = node
            .parent()
            .is_some_and(|p| p.kind_id() == kind::QUERY_BODY);
        let name = node
            .child_by_field_id(field::NAME)
            .unwrap_or(node);
        if !at_top_level {
            self.report(
                name,
                severity::ERROR,
                "virtual-edge",
                "Virtual edge types can only be declared at the top level of a query body".into(),
            );
        }
        if let Some(attributes) = node.child_by_field_id(field::ATTRIBUTES) {
            for discriminator in syntax::named_children(attributes)
                .into_iter()
                .filter(|c| c.kind_id() == kind::DISCRIMINATOR)
            {
                self.report(
                    discriminator,
                    severity::ERROR,
                    "virtual-edge",
                    "Virtual edges cannot have a DISCRIMINATOR".into(),
                );
            }
        }
    }

    /// A virtual edge type cannot share an edge pattern with other types.
    fn mixed_virtual_edges(&mut self, node: Node) {
        let names: Vec<Node> = match node.kind() {
            "relationship_detail" => syntax::children_by_field(node, "type")
                .into_iter()
                .filter(|t| t.kind_id() == kind::IDENTIFIER)
                .collect(),
            // A nested alternation is checked with the outermost one.
            _ if syntax::ancestors(node)
                .take_while(|a| a.kind_id() != kind::EDGE_PATTERN)
                .any(|a| a.kind_id() == kind::EDGE_ALTERNATION) =>
            {
                return;
            }
            _ => {
                let mut names = Vec::new();
                syntax::walk(node, |n| {
                    if n.kind_id() == kind::EDGE_ATOM
                        && let Some(name) = n.child_by_field_id(field::NAME)
                    {
                        names.push(name);
                    }
                });
                names
            }
        };
        // One type used more than once, as in `Near>|<Near`, is not a mix.
        let distinct: HashSet<&str> =
            names.iter().map(|n| self.text(*n)).collect();
        if distinct.len() < 2 {
            return;
        }
        let is_virtual = |name: &Node| {
            self.symbol_of(*name).is_some_and(|s| {
                s.kind == SymbolKind::EdgeType && s.scope != FILE_SCOPE
            })
        };
        let virtual_edges: Vec<Node> = names
            .into_iter()
            .filter(is_virtual)
            .collect();
        for virtual_edge in virtual_edges {
            let message = format!(
                "The virtual edge type `{}` cannot be combined with other edge types",
                self.text(virtual_edge)
            );
            self.report(
                virtual_edge,
                severity::ERROR,
                "virtual-edge",
                message,
            );
        }
    }

    fn accumulator_type(&mut self, node: Node, context: &Context) {
        let kind = syntax::field_text(node, "kind", self.snapshot.text())
            .unwrap_or("")
            .to_ascii_lowercase();
        let arguments = syntax::children_by_field(node, "argument");
        self.type_arguments(node, &kind, &arguments);
        if kind == "listaccum" {
            // Only a ListAccum may be nested in a ListAccum.
            for argument in arguments
                .iter()
                .filter(|a| a.kind_id() == kind::ACCUMULATOR_TYPE)
            {
                if !is_list_accum(*argument, self) {
                    self.report(
                        *argument,
                        severity::ERROR,
                        "accumulator-type",
                        "Only a ListAccum can be nested within a ListAccum"
                            .into(),
                    );
                }
            }
        }
        match kind.as_str() {
            "groupbyaccum" => {
                for argument in arguments
                    .iter()
                    .filter(|a| a.kind_id() != kind::GROUP_BY_FIELD)
                {
                    self.report(
                        *argument,
                        severity::ERROR,
                        "accumulator-type",
                        "Each GroupByAccum key and accumulator needs a name, e.g. `INT age` or `SumAccum<INT> total`"
                            .into(),
                    );
                }
            }
            "mapaccum" => {
                // A map key cannot be an accumulator.
                if let Some(key) = arguments
                    .first()
                    .filter(|k| k.kind_id() == kind::ACCUMULATOR_TYPE)
                {
                    self.report(
                        *key,
                        severity::ERROR,
                        "accumulator-type",
                        "A MapAccum key cannot be an accumulator".into(),
                    );
                }
                let heap_value = arguments.get(1).is_some_and(|value| {
                    value.kind_id() == kind::ACCUMULATOR_TYPE
                        && syntax::field_text(
                            *value,
                            "kind",
                            self.snapshot.text(),
                        )
                        .is_some_and(|k| k.eq_ignore_ascii_case("heapaccum"))
                });
                if heap_value {
                    self.report(
                        arguments[1],
                        severity::ERROR,
                        "accumulator-type",
                        "A MapAccum value cannot be a HeapAccum".into(),
                    );
                }
            }
            // Only List, Array, Map and GroupBy accumulators may nest others.
            "setaccum" | "bagaccum" => {
                let name =
                    builtins::accumulator(&kind).map_or("", |a| a.name);
                for argument in arguments
                    .iter()
                    .filter(|a| a.kind_id() == kind::ACCUMULATOR_TYPE)
                {
                    self.report(
                        *argument,
                        severity::ERROR,
                        "accumulator-type",
                        format!("A {name} cannot contain accumulators; only ListAccum, ArrayAccum, MapAccum and GroupByAccum can"),
                    );
                }
            }
            // ArrayAccum holds any accumulator but Heap, Map and GroupBy.
            "arrayaccum" => {
                for argument in arguments
                    .iter()
                    .filter(|a| a.kind_id() == kind::ACCUMULATOR_TYPE)
                {
                    let element = syntax::field_text(
                        *argument,
                        "kind",
                        self.snapshot.text(),
                    )
                    .unwrap_or("");
                    if ["heapaccum", "mapaccum", "groupbyaccum"]
                        .iter()
                        .any(|k| element.eq_ignore_ascii_case(k))
                    {
                        self.report(
                            *argument,
                            severity::ERROR,
                            "accumulator-type",
                            "An ArrayAccum cannot contain a HeapAccum, MapAccum or GroupByAccum".into(),
                        );
                    }
                }
            }
            // Report once, at the outermost ListAccum.
            "listaccum"
                if !context.in_list_accum && list_depth(node, self) > 3 =>
            {
                self.report(
                    node,
                    severity::ERROR,
                    "accumulator-type",
                    "ListAccum can be nested at most three levels deep"
                        .into(),
                );
            }
            _ => {}
        }
    }

    /// The number and form of an accumulator type's arguments.
    fn type_arguments(&mut self, node: Node, kind: &str, arguments: &[Node]) {
        let Some(name) = builtins::accumulator(kind).map(|a| a.name) else {
            return;
        };
        let count = arguments.len();
        let bitwise = matches!(kind, "bitwiseoraccum" | "bitwiseandaccum");
        let problem = match kind {
            "mapaccum" => (count != 2)
                .then(|| format!("MapAccum takes two type arguments, `MapAccum<key_type, value_type>`, not {count}")),
            "heapaccum" => (count != 1).then(|| {
                format!(
                    "HeapAccum takes one type argument, its tuple type (`HeapAccum<tuple_type>(capacity, field ASC|DESC)`), not {count}"
                )
            }),
            "sumaccum" | "maxaccum" | "minaccum" | "listaccum" | "setaccum" | "bagaccum" | "arrayaccum" => {
                let what = if kind == "arrayaccum" { "accumulator type" } else { "element type" };
                (count != 1)
                    .then(|| format!("{name} takes one type argument, its {what} (`{name}<type>`), not {count}"))
            }
            // AvgAccum takes no type (a warning: rejection is undocumented).
            "avgaccum" | "deviationaccum" | "deviationpaccum" => {
                if count > 0 {
                    self.untyped_accumulator(node, name);
                }
                return;
            }
            "groupbyaccum" => {
                if !self.group_by_arguments(node, arguments) {
                    return;
                }
                None
            }
            // `OrAccum`, `AndAccum`; `BitwiseOrAccum<128>` takes a length.
            _ if count > 1 => Some(format!("{name} takes at most one type argument, not {count}")),
            _ => None,
        };
        if let Some(message) = problem {
            self.report(node, severity::ERROR, "accumulator-type", message);
            return;
        }
        for argument in arguments {
            if bitwise {
                // `BitwiseAndAccum<128>`; a warning: old docs omit the size.
                if !matches!(
                    argument.kind_id(),
                    kind::INTEGER | kind::IDENTIFIER | kind::TYPE_IDENTIFIER
                ) {
                    let message = format!(
                        "{name} takes a bit length (a number or an INT parameter), not a type"
                    );
                    self.report(
                        *argument,
                        severity::WARNING,
                        "accumulator-type",
                        message,
                    );
                }
            } else if argument.kind_id() == kind::INTEGER {
                let message = if matches!(kind, "oraccum" | "andaccum") {
                    format!(
                        "{name} takes no bit length; only BitwiseOrAccum and BitwiseAndAccum do"
                    )
                } else {
                    format!(
                        "{name} takes a type, not a number; only BitwiseOrAccum and BitwiseAndAccum take a bit length"
                    )
                };
                self.report(
                    *argument,
                    severity::ERROR,
                    "accumulator-type",
                    message,
                );
            } else if argument.kind_id() == kind::GROUP_BY_FIELD
                && kind != "groupbyaccum"
            {
                let message = format!(
                    "Only GroupByAccum names its type arguments; write `{name}<type>`"
                );
                self.report(
                    *argument,
                    severity::ERROR,
                    "accumulator-type",
                    message,
                );
            } else if kind == "sumaccum" && !self.sums(*argument) {
                // Only INT, UINT, FLOAT, DOUBLE or STRING values.
                let given =
                    type_of_type_node(*argument, self.snapshot.text())
                        .display();
                let message = format!(
                    "SumAccum takes INT, UINT, FLOAT, DOUBLE or STRING, not {given}"
                );
                self.report(
                    *argument,
                    severity::ERROR,
                    "accumulator-type",
                    message,
                );
            }
        }
    }

    /// Whether a SumAccum can hold `argument` (TYPEDEF names are not judged).
    fn sums(&self, argument: Node) -> bool {
        match argument.kind_id() {
            kind::PRIMITIVE_TYPE => matches!(
                type_of_type_node(argument, self.snapshot.text()),
                // `STRING COMPRESS` too (reported as deprecated).
                Ty::Primitive(p) if matches!(p.as_str(), "INT" | "UINT" | "FLOAT" | "DOUBLE") || p.starts_with("STRING")
            ),
            kind::VERTEX_TYPE
            | kind::EDGE_TYPE
            | kind::COLLECTION_TYPE
            | kind::ACCUMULATOR_TYPE => false,
            _ => true,
        }
    }

    /// `AvgAccum<DOUBLE>`: the type is not written; a fix removes it.
    fn untyped_accumulator(&mut self, node: Node, name: &str) {
        let children = syntax::children(node);
        let (Some(open), Some(close)) = (
            children.iter().find(|c| c.kind() == "<"),
            children
                .iter()
                .rev()
                .find(|c| c.kind() == ">"),
        ) else {
            return;
        };
        let message = format!(
            "{name} is declared without a type argument; it accepts INT, UINT, FLOAT and DOUBLE inputs"
        );
        let mut d = diagnostic(
            self.snapshot,
            Span::of(node),
            severity::WARNING,
            "accumulator-type",
            message,
        );
        let span = Span::new(open.start_byte(), close.end_byte());
        let edit = self.snapshot.edit(span, "");
        add_fix(
            &mut d,
            format!("Write `{name}` without a type argument"),
            vec![edit],
            true,
        );
        self.out.push(d);
    }

    /// GroupByAccum fields: keys first, at least one of each, distinct names.
    /// Whether the arguments are complete enough to check one by one.
    fn group_by_arguments(&mut self, node: Node, arguments: &[Node]) -> bool {
        let accumulators: Vec<bool> = arguments
            .iter()
            .map(|a| {
                let ty = a
                    .child_by_field_id(field::TYPE)
                    .unwrap_or(*a);
                ty.kind_id() == kind::ACCUMULATOR_TYPE
                    || (ty.kind_id() == kind::TYPE_IDENTIFIER
                        && self
                            .accumulator_typedef(
                                self.text(ty),
                                ty.start_byte(),
                            )
                            .is_some())
            })
            .collect();
        let count = accumulators.iter().filter(|a| **a).count();
        if count == 0 || count == arguments.len() {
            let message = "GroupByAccum needs at least one key and one accumulator, e.g. `GroupByAccum<INT age, SumAccum<INT> total>`";
            self.report(
                node,
                severity::ERROR,
                "accumulator-type",
                message.into(),
            );
            return false;
        }
        if let Some(i) = accumulators
            .windows(2)
            .position(|w| w[0] && !w[1])
        {
            self.report(
                arguments[i + 1],
                severity::ERROR,
                "accumulator-type",
                "GroupByAccum keys come before its accumulators, e.g. `GroupByAccum<INT age, SumAccum<INT> total>`"
                    .into(),
            );
        }
        let mut seen = Vec::new();
        for field in arguments
            .iter()
            .filter_map(|a| a.child_by_field_id(field::NAME))
        {
            let name = self.text(field).to_string();
            if seen.contains(&name) {
                let message = format!(
                    "GroupByAccum already has a field named `{name}`"
                );
                self.report(
                    field,
                    severity::ERROR,
                    "accumulator-type",
                    message,
                );
            }
            seen.push(name);
        }
        true
    }

    /// The type a TYPEDEF'd accumulator name visible at `offset` stands for.
    fn accumulator_typedef(&self, name: &str, offset: usize) -> Option<Ty> {
        self.visible_type(SymbolKind::AccumulatorType, name, offset)
            .map(|s| s.ty.clone())
    }

    /// The innermost TYPEDEF of `kind` named `name` visible at `offset`.
    fn visible_type(
        &self,
        kind: SymbolKind,
        name: &str,
        offset: usize,
    ) -> Option<&'s Symbol> {
        let candidates: Vec<&'s Symbol> = self
            .type_symbols
            .iter()
            .copied()
            .filter(|s| s.kind == kind && s.name == name)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let analysis = &self.snapshot.analysis;
        analysis
            .scope_chain(analysis.scope_at(offset))
            .find_map(|scope| {
                candidates
                    .iter()
                    .find(|s| s.scope == scope)
                    .copied()
            })
    }

    /// `ty` with TYPEDEF'd accumulator names resolved to their types.
    fn resolved(&self, ty: &Ty, offset: usize) -> Ty {
        match ty {
            Ty::Tuple(name) => self
                .accumulator_typedef(name, offset)
                .unwrap_or_else(|| ty.clone()),
            Ty::Accumulator(kind, args) => Ty::Accumulator(
                kind.clone(),
                args.iter()
                    .map(|a| self.resolved(a, offset))
                    .collect(),
            ),
            _ => ty.clone(),
        }
    }

    /// Whether `name` is a tuple type at `offset` or in the workspace.
    fn is_tuple_type(&self, name: &str, offset: usize) -> bool {
        self.visible_type(SymbolKind::TupleType, name, offset)
            .is_some()
            || self
                .snapshot
                .workspace
                .declares(&[SymbolKind::TupleType], name)
    }

    /// A value written to an accumulator, judged against its type.
    fn accumulator_input(&mut self, node: Node) {
        if node.has_error() {
            return;
        }
        let (ty, value) = if node.kind_id() == kind::ACCUMULATOR_DECLARATOR {
            // ArrayAccum elements are written through subscripts.
            if syntax::has_child(node, "array_dimension") {
                return;
            }
            let ty = node
                .parent()
                .and_then(|d| d.child_by_field_id(field::TYPE))
                // A TYPEDEF'd accumulator type resolves below.
                .filter(|t| {
                    matches!(
                        t.kind_id(),
                        kind::ACCUMULATOR_TYPE | kind::TYPE_IDENTIFIER
                    )
                })
                .map(|t| type_of_type_node(t, self.snapshot.text()));
            (ty, node.child_by_field_id(field::VALUE))
        } else {
            let ty = node
                .child_by_field_id(field::LEFT)
                .filter(|left| left.kind_id() != kind::IDENTIFIER)
                .and_then(|left| self.operand_type(left));
            (ty, node.child_by_field_id(field::RIGHT))
        };
        if let (Some(ty), Some(value)) = (ty, value) {
            let ty = self.resolved(&ty, node.start_byte());
            let assigned = node.kind_id() == kind::ACCUMULATOR_DECLARATOR
                || node
                    .child_by_field_id(field::OPERATOR)
                    .is_some_and(|o| o.kind() == "=");
            self.accumulator_value(value, &ty, assigned);
        }
    }

    /// `value` given to an accumulator `ty` (`assigned`: `=`, not `+=`).
    fn accumulator_value(&mut self, value: Node, ty: &Ty, assigned: bool) {
        let Ty::Accumulator(kind, args) = ty else {
            return;
        };
        match kind.as_str() {
            "MapAccum" => {
                if let [key, item] = args.as_slice() {
                    self.pair_input(
                        value,
                        ty,
                        std::slice::from_ref(key),
                        std::slice::from_ref(item),
                    );
                }
            }
            "GroupByAccum" => {
                let (values, keys): (Vec<Ty>, Vec<Ty>) = args
                    .iter()
                    .cloned()
                    .partition(|a| matches!(a, Ty::Accumulator(..)));
                if !keys.is_empty() && !values.is_empty() {
                    self.pair_input(value, ty, &keys, &values);
                }
            }
            _ => {
                let inner = unparenthesized(value);
                let what = capitalized(&with_article(&ty.display()));
                if matches!(
                    inner.kind_id(),
                    kind::KEY_VALUE_PAIR | kind::MAP_LITERAL
                ) {
                    let message = format!(
                        "{what} takes no `(key -> value)` pairs; only MapAccum and GroupByAccum do"
                    );
                    self.report(
                        inner,
                        severity::ERROR,
                        "accumulator-input",
                        message,
                    );
                    return;
                }
                let expected = accumulator_input_kind(ty);
                // `SumAccum<INT>`, `OrAccum`: one value at a time.
                let single = expected.is_some()
                    && !matches!(
                        kind.as_str(),
                        "SetAccum" | "BagAccum" | "ListAccum" | "ArrayAccum"
                    );
                if single
                    && matches!(
                        inner.kind_id(),
                        kind::TUPLE | kind::LIST_LITERAL
                    )
                {
                    let shape = if inner.kind_id() == kind::TUPLE {
                        "a tuple"
                    } else {
                        "a list"
                    };
                    let message =
                        format!("{what} takes a single value, not {shape}");
                    self.report(
                        value,
                        severity::ERROR,
                        "accumulator-input",
                        message,
                    );
                    return;
                }
                let given = match self.given(value) {
                    Some(Given::Kind(given))
                        if expected.is_some_and(|e| e != given) =>
                    {
                        given.described().to_string()
                    }
                    Some(Given::Other(given)) if single => given,
                    _ => return,
                };
                let verb = if assigned { "stored in" } else { "added to" };
                let message = format!(
                    "{} cannot be {verb} {}",
                    capitalized(&given),
                    with_article(&ty.display())
                );
                self.report(
                    value,
                    severity::WARNING,
                    "type-mismatch",
                    message,
                );
            }
        }
    }

    /// The input of a MapAccum or GroupByAccum: keys and values.
    fn pair_input(
        &mut self,
        value: Node,
        ty: &Ty,
        keys: &[Ty],
        values: &[Ty],
    ) {
        let inner = unparenthesized(value);
        let what = capitalized(&with_article(&ty.display()));
        let map =
            matches!(ty, Ty::Accumulator(kind, _) if kind == "MapAccum");
        let shape = if map {
            "`(key -> value)`".to_string()
        } else {
            let names = |single: &str, prefix: &str, n: usize| match n {
                1 => single.to_string(),
                _ => (1..=n)
                    .map(|i| format!("{prefix}{i}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            format!(
                "`({} -> {})`",
                names("key", "k", keys.len()),
                names("value", "v", values.len())
            )
        };
        let pairs = Pairs {
            ty,
            keys,
            values,
            what,
            shape,
        };
        let (what, shape) = (&pairs.what, &pairs.shape);
        match inner.kind_id() {
            kind::KEY_VALUE_PAIR => {
                let arrow = syntax::child_of_kind(inner, "->");
                let Some(arrow) = arrow else { return };
                let (before, after): (Vec<Node>, Vec<Node>) =
                    syntax::code_children(inner)
                        .into_iter()
                        .partition(|c| c.end_byte() <= arrow.start_byte());
                self.pair(inner, &before, &after, &pairs);
            }
            kind::MAP_LITERAL => {
                for entry in syntax::code_children(inner)
                    .into_iter()
                    .filter(|e| e.kind_id() == kind::MAP_ENTRY)
                {
                    let (Some(key), Some(item)) = (
                        entry.child_by_field_id(field::KEY),
                        entry.child_by_field_id(field::VALUE),
                    ) else {
                        continue;
                    };
                    self.pair(entry, &[key], &[item], &pairs);
                }
            }
            kind::TUPLE => {
                let elements = syntax::code_children(inner);
                let message = format!(
                    "{what} takes {shape} pairs, not a tuple; separate the keys from the values with `->`"
                );
                let mut d = diagnostic(
                    self.snapshot,
                    Span::of(inner),
                    severity::ERROR,
                    "accumulator-input",
                    message,
                );
                // `("a", 1)` -> `("a" -> 1)`
                if elements.len() == keys.len() + values.len()
                    && elements.len() >= 2
                {
                    let last_key = elements[keys.len() - 1];
                    let first_value = elements[keys.len()];
                    let comma =
                        syntax::children(inner)
                            .into_iter()
                            .find(|c| {
                                c.kind() == ","
                                    && c.start_byte() >= last_key.end_byte()
                                    && c.end_byte()
                                        <= first_value.start_byte()
                            });
                    if let Some(comma) = comma {
                        let edit = self.snapshot.edit(Span::of(comma), " ->");
                        add_fix(
                            &mut d,
                            "Replace `,` with `->`",
                            vec![edit],
                            false,
                        );
                    }
                }
                self.out.push(d);
            }
            // `(k - v)`, `(k > v)`, `(k >> v)`: a mistyped `->`.
            kind::BINARY_EXPRESSION
                if inner.id() != value.id()
                    && keys.len() == 1
                    && values.len() == 1
                    && inner
                        .child_by_field_id(field::OPERATOR)
                        .is_some_and(|o| {
                            matches!(o.kind(), "-" | ">" | ">>")
                        }) =>
            {
                let Some(operator) = inner.child_by_field_id(field::OPERATOR)
                else {
                    return;
                };
                self.arrow_typos.insert(inner.id());
                let message = format!(
                    "{what} takes {shape} pairs; did you mean `->` instead of `{}`?",
                    operator.kind()
                );
                let mut d = diagnostic(
                    self.snapshot,
                    Span::of(value),
                    severity::ERROR,
                    "accumulator-input",
                    message,
                );
                let edit = self.snapshot.edit(Span::of(operator), "->");
                add_fix(
                    &mut d,
                    format!("Replace `{}` with `->`", operator.kind()),
                    vec![edit],
                    false,
                );
                self.out.push(d);
            }
            kind::LIST_LITERAL => {
                self.report(
                    value,
                    severity::ERROR,
                    "accumulator-input",
                    format!("{what} takes {shape} pairs"),
                );
            }
            _ => {
                if let Some(source) = self.operand_type(inner)
                    && self.pair_source(value, inner, &source, &pairs)
                {
                    return;
                }
                // Only literals and values of a known scalar kind.
                if self.value_kind(inner).is_some() {
                    // One error: no other warning on `p >= 1`.
                    self.arrow_typos.insert(inner.id());
                    self.report(
                        value,
                        severity::ERROR,
                        "accumulator-input",
                        format!("{what} takes {shape} pairs"),
                    );
                }
            }
        }
    }

    /// A collection given to a MapAccum or GroupByAccum `ty`: another map
    /// merges (its keys must fit); others are wrong. Whether it was judged.
    fn pair_source(
        &mut self,
        value: Node,
        operand: Node,
        source: &Ty,
        pairs: &Pairs,
    ) -> bool {
        let &Pairs {
            ty,
            keys,
            ref what,
            ref shape,
            ..
        } = pairs;
        let Ty::Accumulator(target, _) = ty else {
            return false;
        };
        let source_key = match source {
            Ty::Accumulator(kind, args) if kind == target => {
                if kind != "MapAccum" {
                    return true;
                }
                args.first()
            }
            Ty::Collection(kind, args)
                if kind == "MAP" && target == "MapAccum" =>
            {
                args.first()
            }
            Ty::Accumulator(..)
            | Ty::Collection(..)
            | Ty::Vertex(_)
            | Ty::Edge(_)
            | Ty::VertexSet(_) => {
                let message = format!(
                    "{what} takes {shape} pairs, not {}",
                    described_type(source)
                );
                self.report(
                    value,
                    severity::ERROR,
                    "accumulator-input",
                    message,
                );
                return true;
            }
            _ => return false,
        };
        if let (Some(given), Some(declared)) = (source_key, keys.first())
            && let (Some(a), Some(b)) =
                (value_kind_of(given), value_kind_of(declared))
            && a != b
        {
            let message = format!(
                "`{}` has {} keys, but the keys of {} are {}",
                self.text(operand),
                given.display(),
                ty.display(),
                declared.display()
            );
            self.report(value, severity::WARNING, "type-mismatch", message);
        }
        true
    }

    /// One `(keys -> values)` pair of a MapAccum or GroupByAccum.
    fn pair(
        &mut self,
        pair: Node,
        given_keys: &[Node],
        given_values: &[Node],
        pairs: &Pairs,
    ) {
        let &Pairs {
            ty,
            keys,
            values,
            ref shape,
            ..
        } = pairs;
        if given_keys.len() != keys.len()
            || given_values.len() != values.len()
        {
            let count =
                |n: usize, what: &str| format!("{n} {what}{}", plural(n));
            let message = format!(
                "{} takes {} pairs ({} and {}); this pair has {} and {}",
                pairs.what,
                shape,
                count(keys.len(), "key"),
                count(values.len(), "value"),
                count(given_keys.len(), "key"),
                count(given_values.len(), "value"),
            );
            self.report(pair, severity::ERROR, "accumulator-input", message);
            return;
        }
        for (key, key_ty) in given_keys.iter().zip(keys) {
            self.scalar_input(*key, key_ty, ty, "keys");
        }
        for (item, item_ty) in given_values.iter().zip(values) {
            match item_ty {
                Ty::Accumulator(..) => {
                    self.accumulator_value(*item, item_ty, false)
                }
                _ => self.scalar_input(*item, item_ty, ty, "values"),
            }
        }
    }

    /// A map key or plain value of the wrong kind, or an unexpected pair.
    fn scalar_input(
        &mut self,
        node: Node,
        declared: &Ty,
        ty: &Ty,
        role: &str,
    ) {
        let inner = unparenthesized(node);
        if matches!(inner.kind_id(), kind::KEY_VALUE_PAIR | kind::MAP_LITERAL)
        {
            if matches!(declared, Ty::Primitive(_)) {
                let message = format!(
                    "The {role} of {} are {}, not `(key -> value)` pairs",
                    ty.display(),
                    declared.display()
                );
                self.report(
                    inner,
                    severity::ERROR,
                    "accumulator-input",
                    message,
                );
            }
            return;
        }
        let Some(given) = self.given(node) else {
            return;
        };
        let wrong = match (declared, &given) {
            (Ty::Primitive(_), Given::Kind(given)) => {
                value_kind_of(declared).is_some_and(|e| e != *given)
            }
            (Ty::Primitive(_), Given::Other(_)) => {
                value_kind_of(declared).is_some()
            }
            (Ty::Tuple(name), Given::Kind(_)) => {
                self.is_tuple_type(name, node.start_byte())
            }
            _ => false,
        };
        if wrong {
            let what = match given {
                Given::Kind(kind) => kind.described().to_string(),
                Given::Other(what) => what,
            };
            let message = format!(
                "The {role} of {} are {}, not {what}",
                ty.display(),
                declared.display()
            );
            self.report(node, severity::WARNING, "type-mismatch", message);
        }
    }

    /// The declared type of a variable, parameter, alias or accumulator.
    fn operand_type(&self, node: Node) -> Option<Ty> {
        let symbol = match node.kind_id() {
            kind::IDENTIFIER
            | kind::GLOBAL_ACCUMULATOR
            | kind::LOCAL_ACCUMULATOR => self.symbol_of(node),
            kind::MEMBER_EXPRESSION => node
                .child_by_field_id(field::PROPERTY)
                .filter(|p| p.kind_id() == kind::LOCAL_ACCUMULATOR)
                .and_then(|p| self.symbol_of(p)),
            _ => None,
        }?;
        Some(self.resolved(&symbol.ty, node.start_byte()))
    }

    /// What a value certainly is: a scalar, or something of several values.
    fn given(&self, node: Node) -> Option<Given> {
        if let Some(kind) = self.value_kind(node) {
            return Some(Given::Kind(kind));
        }
        match self.operand_type(unparenthesized(node))? {
            ty @ (Ty::Vertex(_)
            | Ty::Edge(_)
            | Ty::VertexSet(_)
            | Ty::Collection(..)) => Some(Given::Other(described_type(&ty))),
            // Only accumulators that surely hold several values.
            ty @ Ty::Accumulator(..) if holds_several_values(&ty) => {
                Some(Given::Other(described_type(&ty)))
            }
            _ => None,
        }
    }

    /// Keys given to `get`, `containsKey` and `remove` of a map accumulator.
    fn accumulator_method(&mut self, node: Node) {
        let (Some(function), Some(arguments)) = (
            node.child_by_field_id(field::FUNCTION),
            node.child_by_field_id(field::ARGUMENTS),
        ) else {
            return;
        };
        let (Some(object), Some(method)) = (
            function
                .child_by_field_id(field::OBJECT)
                .filter(|_| function.kind_id() == kind::MEMBER_EXPRESSION),
            function
                .child_by_field_id(field::PROPERTY)
                .filter(|p| p.kind_id() == kind::IDENTIFIER),
        ) else {
            return;
        };
        let method = self.text(method).to_string();
        if !["get", "containsKey", "remove"]
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&method))
        {
            return;
        }
        let Some(ty) = self.operand_type(object) else {
            return;
        };
        let Ty::Accumulator(kind, args) = &ty else {
            return;
        };
        let keys: Vec<Ty> = match kind.as_str() {
            // The key count is checked with the other built-in methods.
            "MapAccum" => args.first().cloned().into_iter().collect(),
            "GroupByAccum" => args
                .iter()
                .filter(|a| !matches!(a, Ty::Accumulator(..)))
                .cloned()
                .collect(),
            _ => return,
        };
        let given = syntax::code_children(arguments);
        if kind == "GroupByAccum"
            && given.len() != keys.len()
            && !keys.is_empty()
        {
            let message = format!(
                "`{method}` expects {} argument{}, the keys of {}, but {} given",
                keys.len(),
                plural(keys.len()),
                ty.display(),
                given.len()
            );
            self.report(
                arguments,
                severity::WARNING,
                "argument-count",
                message,
            );
            return;
        }
        for (argument, key) in given.iter().zip(&keys) {
            if let (Some(expected), Some(Given::Kind(found))) =
                (value_kind_of(key), self.given(*argument))
                && expected != found
            {
                let message = format!(
                    "The keys of {} are {}, not {}",
                    ty.display(),
                    key.display(),
                    found.described()
                );
                self.report(
                    *argument,
                    severity::WARNING,
                    "type-mismatch",
                    message,
                );
            }
        }
    }

    /// `FOREACH (k, v) IN @@map`: maps bind a key and value, others one.
    fn foreach_variables(&mut self, node: Node) {
        let (Some(variables), Some(collection)) = (
            node.child_by_field_id(field::VARIABLE),
            node.child_by_field_id(field::COLLECTION),
        ) else {
            return;
        };
        let count = if variables.kind_id() == kind::FOREACH_VARIABLES {
            syntax::named_children(variables)
                .into_iter()
                .filter(|c| c.kind_id() == kind::IDENTIFIER)
                .count()
        } else {
            1
        };
        let Some(ty) = self.operand_type(unparenthesized(collection)) else {
            return;
        };
        let message = match &ty {
            Ty::Accumulator(kind, _) | Ty::Collection(kind, _)
                if (kind == "MapAccum" || kind == "MAP") && count != 2 =>
            {
                format!(
                    "Iterating over {} binds a key and a value, `FOREACH (k, v) IN ...`, not {count} variable{}",
                    with_article(&ty.display()),
                    plural(count)
                )
            }
            Ty::Accumulator(kind, args) | Ty::Collection(kind, args)
                if matches!(
                    kind.as_str(),
                    "SetAccum"
                        | "BagAccum"
                        | "ListAccum"
                        | "SET"
                        | "BAG"
                        | "LIST"
                ) && count > 1
                    && args.first().is_some_and(|e| {
                        value_kind_of(e).is_some()
                            || matches!(e, Ty::Vertex(_) | Ty::Edge(_))
                    }) =>
            {
                format!(
                    "{} yields one element at a time; write `FOREACH x IN ...`, not {count} variables",
                    capitalized(&with_article(&ty.display()))
                )
            }
            Ty::VertexSet(_) if count > 1 => {
                format!(
                    "A vertex set yields one vertex at a time; write `FOREACH x IN ...`, not {count} variables"
                )
            }
            _ => return,
        };
        self.report(variables, severity::ERROR, "foreach-variables", message);
    }

    /// Accumulator type names are case-sensitive.
    fn accumulator_case(&mut self, node: Node) {
        let text = self.text(node).to_string();
        let Some(accumulator) = builtins::accumulator(&text) else {
            return;
        };
        if accumulator.name != text {
            let mut d = diagnostic(
                self.snapshot,
                Span::of(node),
                severity::WARNING,
                "accumulator-case",
                format!(
                    "Accumulator type names are case-sensitive: write `{}`",
                    accumulator.name
                ),
            );
            add_fix(
                &mut d,
                format!("Replace with `{}`", accumulator.name),
                vec![
                    self.snapshot
                        .edit(Span::of(node), accumulator.name),
                ],
                true,
            );
            self.out.push(d);
        }
    }

    /// openCypher patterns need SYNTAX V3 (only V1/V2 queries are judged).
    fn cypher_syntax(&mut self, node: Node, context: &Context) {
        let Some(mode) = context
            .query
            .filter(|m| m.old_syntax && !m.v3 && !m.interpreted)
        else {
            return;
        };
        if self.cypher_reported == Some(mode.start) {
            return;
        }
        self.cypher_reported = Some(mode.start);
        self.report(
            node,
            severity::WARNING,
            "cypher-syntax",
            "openCypher patterns need a SYNTAX V3 declaration; V1 and V2 queries reject them".into(),
        );
    }

    /// A parenthesized condition cannot be compared (`(a < b) != c`); `>` is
    /// left out, as the parser accepts it there.
    fn compared_condition(&mut self, node: Node) {
        let Some(operator) = node.child_by_field_id(field::OPERATOR) else {
            return;
        };
        if !matches!(
            operator.kind(),
            "==" | "!=" | "<>" | "=" | "<" | "<=" | ">="
        ) {
            return;
        }
        let Some(left) = node
            .child_by_field_id(field::LEFT)
            .filter(|l| l.kind_id() == kind::PARENTHESIZED_EXPRESSION)
        else {
            return;
        };
        let inner = unparenthesized(left);
        let condition = match inner.kind_id() {
            kind::BINARY_EXPRESSION => inner
                .child_by_field_id(field::OPERATOR)
                .is_some_and(|o| {
                    matches!(
                        o.kind(),
                        "==" | "!=" | "<>" | "=" | "<" | "<=" | ">" | ">="
                    ) || matches!(
                        self.text(o).to_ascii_uppercase().as_str(),
                        "AND" | "OR"
                    )
                }),
            kind::UNARY_EXPRESSION => inner
                .child_by_field_id(field::OPERATOR)
                .is_some_and(|o| self.text(o).eq_ignore_ascii_case("NOT")),
            kind::IN_EXPRESSION
            | kind::LIKE_EXPRESSION
            | kind::IS_EXPRESSION
            | kind::BETWEEN_EXPRESSION => true,
            _ => false,
        };
        if condition {
            let message = format!(
                "GSQL cannot compare a parenthesized condition with `{}`; combine the conditions with AND, OR and NOT instead (for \"exactly one of A and B\": `(A AND NOT B) OR (NOT A AND B)`)",
                self.text(operator)
            );
            self.report(
                operator,
                severity::ERROR,
                "compared-condition",
                message,
            );
        }
    }

    /// No method on a parenthesized set expression, `(@@a MINUS @@b).size()`.
    fn set_expression_method(&mut self, node: Node) {
        let Some(function) = node
            .child_by_field_id(field::FUNCTION)
            .filter(|f| f.kind_id() == kind::MEMBER_EXPRESSION)
        else {
            return;
        };
        let Some(object) = function
            .child_by_field_id(field::OBJECT)
            .filter(|o| o.kind_id() == kind::PARENTHESIZED_EXPRESSION)
        else {
            return;
        };
        let inner = unparenthesized(object);
        let Some(operator) = inner
            .child_by_field_id(field::OPERATOR)
            .filter(|_| inner.kind_id() == kind::BINARY_EXPRESSION)
        else {
            return;
        };
        let operator = self.text(operator).to_ascii_uppercase();
        if !matches!(operator.as_str(), "UNION" | "INTERSECT" | "MINUS") {
            return;
        }
        let Some(method) = function.child_by_field_id(field::PROPERTY) else {
            return;
        };
        let message = format!(
            "GSQL cannot call `{}()` on a parenthesized {operator} expression; store the result in an accumulator or variable first and call the method on that",
            self.text(method)
        );
        self.report(
            method,
            severity::ERROR,
            "set-expression-method",
            message,
        );
    }

    /// The compiler warns about exact floating-point comparison.
    fn float_equality(&mut self, node: Node, mode: Mode) {
        let Some(operator) = node.child_by_field_id(field::OPERATOR) else {
            return;
        };
        let equality =
            operator.kind() == "==" || (operator.kind() == "=" && mode.v3);
        if !equality {
            return;
        }
        let (Some(left), Some(right)) = (
            node.child_by_field_id(field::LEFT),
            node.child_by_field_id(field::RIGHT),
        ) else {
            return;
        };
        // Whole numbers are exact: `x == 0` and `x == floor(x)` are reliable.
        let exact = self.is_whole_literal(left)
            || self.is_whole_literal(right)
            || self.is_rounding_of(right, left)
            || self.is_rounding_of(left, right);
        if !exact
            && [left, right]
                .into_iter()
                .any(|operand| self.is_float(operand))
        {
            self.report(
                operator,
                severity::WARNING,
                "float-equality",
                "Comparing FLOAT or DOUBLE values for exact equality is unreliable; compare with a tolerance, e.g. `abs(a - b) < 0.0001`"
                    .into(),
            );
        }
    }

    /// `0`, `-1`, `2.0`: a literal without a fractional part.
    fn is_whole_literal(&self, node: Node) -> bool {
        match node.kind_id() {
            kind::INTEGER => true,
            kind::FLOAT => self
                .text(node)
                .parse::<f64>()
                .is_ok_and(|v| v.fract() == 0.0 && v.abs() < 9e15),
            kind::PARENTHESIZED_EXPRESSION => syntax::first_code_child(node)
                .is_some_and(|inner| self.is_whole_literal(inner)),
            kind::UNARY_EXPRESSION => {
                node.child_by_field_id(field::OPERATOR)
                    .is_some_and(|o| o.kind() == "-")
                    && node
                        .child_by_field_id(field::OPERAND)
                        .is_some_and(|inner| self.is_whole_literal(inner))
            }
            _ => false,
        }
    }

    /// Whether `call` rounds `operand` (`floor(x)`, `round(x)`, ...).
    fn is_rounding_of(&self, call: Node, operand: Node) -> bool {
        let mut call = call;
        while call.kind_id() == kind::PARENTHESIZED_EXPRESSION {
            let Some(inner) = syntax::first_code_child(call) else {
                return false;
            };
            call = inner;
        }
        if call.kind_id() != kind::CALL_EXPRESSION {
            return false;
        }
        // A query or tuple type of that name replaces the built-in.
        let Some(Callee::Builtin(function)) = self.callee(call) else {
            return false;
        };
        if !["float_to_int", "floor", "ceil", "round", "trunc"]
            .contains(&function.name)
        {
            return false;
        }
        // The tokens without whitespace and comments.
        let squash = |node: Node| -> String {
            let mut out = String::new();
            syntax::walk(node, |n| {
                if n.child_count() == 0 && n.kind_id() != kind::COMMENT {
                    out.extend(self.text(n).split_whitespace());
                }
            });
            out
        };
        let arguments = call
            .child_by_field_id(field::ARGUMENTS)
            .map(syntax::code_children)
            .unwrap_or_default();
        arguments
            .first()
            .is_some_and(|first| squash(*first) == squash(operand))
    }

    /// Whether an expression is FLOAT or DOUBLE (iterative: chains are long).
    fn is_float(&self, node: Node) -> bool {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            let float = match node.kind_id() {
                kind::FLOAT => true,
                kind::PARENTHESIZED_EXPRESSION => {
                    pending.extend(syntax::first_code_child(node));
                    false
                }
                kind::UNARY_EXPRESSION => {
                    pending.extend(node.child_by_field_id(field::OPERAND));
                    false
                }
                kind::BINARY_EXPRESSION => {
                    let arithmetic = node
                        .child_by_field_id(field::OPERATOR)
                        .is_some_and(|o| {
                            matches!(o.kind(), "+" | "-" | "*" | "/")
                        });
                    if arithmetic {
                        pending.extend(node.child_by_field_id(field::LEFT));
                        pending.extend(node.child_by_field_id(field::RIGHT));
                    }
                    false
                }
                kind::IDENTIFIER
                | kind::GLOBAL_ACCUMULATOR
                | kind::LOCAL_ACCUMULATOR => self
                    .symbol_of(node)
                    .is_some_and(|s| is_float_type(&s.ty)),
                kind::MEMBER_EXPRESSION => node
                    .child_by_field_id(field::PROPERTY)
                    .is_some_and(|p| self.is_float_member(p)),
                kind::CALL_EXPRESSION => {
                    self.callee(node).is_some_and(|callee| {
                        match callee {
                            // The type of its RETURNS clause.
                            Callee::Query(query) => is_float_type(&query.ty),
                            Callee::Tuple => false,
                            Callee::Builtin(function) => {
                                matches!(function.returns, "FLOAT" | "DOUBLE")
                            }
                        }
                    })
                }
                _ => false,
            };
            if float {
                return true;
            }
        }
        false
    }

    /// Whether `x.property` holds a FLOAT or DOUBLE.
    fn is_float_member(&self, property: Node) -> bool {
        self.member_types(property)
            .is_some_and(|types| {
                !types.is_empty() && types.iter().all(is_float_type)
            })
    }

    /// The declared types of a member, `None` when unknown.
    fn member_types(&self, property: Node) -> Option<Vec<Ty>> {
        if property.kind_id() == kind::LOCAL_ACCUMULATOR {
            return self
                .symbol_of(property)
                .map(|s| vec![s.ty.clone()]);
        }
        let reference = self
            .snapshot
            .analysis
            .reference_at(property.start_byte())?;
        match resolve::target(self.snapshot, reference)? {
            Target::Local(id) => {
                Some(vec![self.snapshot.analysis.symbols[id].ty.clone()])
            }
            Target::Global(key) if key.kind == SymbolKind::Attribute => Some(
                resolve::declarations(self.snapshot, &key)
                    .iter()
                    .map(|d| d.ty.clone())
                    .collect(),
            ),
            _ => None,
        }
    }

    /// What a call by plain name, `name(...)`, calls.
    fn callee(&self, call: Node) -> Option<Callee<'s>> {
        let function = call
            .child_by_field_id(field::FUNCTION)
            .filter(|f| f.kind_id() == kind::IDENTIFIER)?;
        resolve::callee(self.snapshot, function)
    }

    /// The query `name(...)` calls, when the call is in well-formed code.
    fn user_query(&self, call: Node) -> Option<&'s GlobalSymbol> {
        let Callee::Query(query) = self.callee(call)? else {
            return None;
        };
        // Only broken files need this; capped, as `parent()` costs depth.
        if self.snapshot.root().has_error() {
            let mut ancestors = syntax::self_and_ancestors(call);
            if ancestors
                .by_ref()
                .take(MAX_CALL_DEPTH)
                .any(|a| a.is_error())
                || ancestors.next().is_some()
            {
                return None;
            }
        }
        (!query.in_error).then_some(query)
    }

    /// Subquery calls: a missing return value, and unfit arguments.
    fn subquery_call(&mut self, node: Node) {
        let Some(query) = self.user_query(node) else {
            return;
        };
        let Some(function) = node.child_by_field_id(field::FUNCTION) else {
            return;
        };
        let used_as_value =
            node.parent()
                .is_some_and(|p| match p.kind_id() {
                    kind::VARIABLE_DECLARATOR => {
                        p.child_by_field_id(field::VALUE) == Some(node)
                    }
                    kind::ASSIGNMENT_STATEMENT => {
                        p.child_by_field_id(field::RIGHT) == Some(node)
                    }
                    _ => false,
                });
        if query.returns.is_none() && used_as_value {
            let message = format!(
                "Query `{}` has no RETURNS clause, so it returns no value",
                self.text(function)
            );
            self.report(node, severity::WARNING, "type-mismatch", message);
        }
        let Some(arguments) = node
            .child_by_field_id(field::ARGUMENTS)
            .filter(|a| !a.has_error())
        else {
            return;
        };
        let values = syntax::code_children(arguments);
        for (value, param) in values.into_iter().zip(&query.params) {
            // The `argument-type` check judges literals.
            if resolve::literal_argument(value).is_some() {
                continue;
            }
            let Some(given) = self.value_kind(value) else {
                continue;
            };
            let accepted = match resolve::param_type(param).as_str() {
                "BOOL" => &[ValueKind::Bool][..],
                "INT" | "UINT" | "FLOAT" | "DOUBLE" => &[ValueKind::Number],
                "STRING" | "DATETIME" => {
                    &[ValueKind::Number, ValueKind::Text]
                }
                _ => continue,
            };
            if !accepted.contains(&given) {
                let what = match given {
                    ValueKind::Number => "a number",
                    ValueKind::Text => "a string",
                    ValueKind::Bool => "a boolean",
                };
                let message = format!(
                    "Query `{}` expects {} for `{}`, not {what}",
                    self.text(function),
                    param.ty,
                    param.name
                );
                self.report(
                    value,
                    severity::WARNING,
                    "argument-type",
                    message,
                );
            }
        }
    }

    /// What kind of value an expression certainly has, when that is known.
    fn value_kind(&self, node: Node) -> Option<ValueKind> {
        match node.kind_id() {
            kind::INTEGER | kind::FLOAT => Some(ValueKind::Number),
            kind::STRING => Some(ValueKind::Text),
            kind::BOOLEAN => Some(ValueKind::Bool),
            kind::PARENTHESIZED_EXPRESSION => syntax::first_code_child(node)
                .and_then(|inner| self.value_kind(inner)),
            kind::UNARY_EXPRESSION => {
                let operand = node
                    .child_by_field_id(field::OPERAND)
                    .and_then(|o| self.value_kind(o));
                match node
                    .child_by_field_id(field::OPERATOR)
                    .map(|o| o.kind())
                {
                    Some("-") => operand.filter(|k| *k == ValueKind::Number),
                    Some("~") => Some(ValueKind::Number),
                    Some("NOT") => Some(ValueKind::Bool),
                    _ => None,
                }
            }
            kind::BINARY_EXPRESSION => {
                let operator = node
                    .child_by_field_id(field::OPERATOR)
                    .map(|o| o.kind())?;
                let side = |field: &str| {
                    node.child_by_field_name(field)
                        .and_then(|n| self.value_kind(n))
                };
                match operator {
                    "-" | "*" | "/" | "%" | "<<" | ">>" | "&" | "|" | "^" => {
                        Some(ValueKind::Number)
                    }
                    "+" => match (side("left"), side("right")) {
                        (
                            Some(ValueKind::Number),
                            Some(ValueKind::Number),
                        ) => Some(ValueKind::Number),
                        (Some(ValueKind::Text), _)
                        | (_, Some(ValueKind::Text)) => Some(ValueKind::Text),
                        _ => None,
                    },
                    "==" | "!=" | "<>" | "=" | "<" | "<=" | ">" | ">="
                    | "AND" | "OR" => Some(ValueKind::Bool),
                    _ => None,
                }
            }
            kind::IN_EXPRESSION
            | kind::LIKE_EXPRESSION
            | kind::IS_EXPRESSION
            | kind::BETWEEN_EXPRESSION => Some(ValueKind::Bool),
            kind::IDENTIFIER
            | kind::GLOBAL_ACCUMULATOR
            | kind::LOCAL_ACCUMULATOR => self
                .symbol_of(node)
                .and_then(|s| value_kind_of(&s.ty)),
            // A subquery returning a plain number, string or BOOL.
            kind::CALL_EXPRESSION => {
                let query = self.user_query(node)?;
                match &query.ty {
                    Ty::Primitive(_) if query.returns.is_some() => {
                        value_kind_of(&query.ty)
                    }
                    _ => None,
                }
            }
            kind::MEMBER_EXPRESSION => {
                let types = self
                    .member_types(node.child_by_field_id(field::PROPERTY)?)?;
                let kinds: Vec<Option<ValueKind>> =
                    types.iter().map(value_kind_of).collect();
                let first = (*kinds.first()?)?;
                kinds
                    .iter()
                    .all(|k| *k == Some(first))
                    .then_some(first)
            }
            _ => None,
        }
    }

    /// Certain type mismatches: strings in numeric or BOOL places.
    fn type_checks(&mut self, node: Node, kind: u16, context: &Context) {
        match kind {
            kind::ASSIGNMENT_STATEMENT => {
                let (Some(left), Some(right), Some(operator)) = (
                    node.child_by_field_id(field::LEFT),
                    node.child_by_field_id(field::RIGHT),
                    node.child_by_field_id(field::OPERATOR),
                ) else {
                    return;
                };
                if self.value_kind(right) != Some(ValueKind::Text) {
                    return;
                }
                if operator.kind() == "="
                    && left.kind_id() == kind::IDENTIFIER
                    && let Some(symbol) = self.symbol_of(left).filter(|s| {
                        matches!(
                            s.kind,
                            SymbolKind::Variable
                                | SymbolKind::Parameter
                                | SymbolKind::LoopVariable
                        )
                    })
                    && let Some(what) = stores_string_badly(&symbol.ty)
                {
                    let message = format!(
                        "A string cannot be stored in {what} variable ({})",
                        symbol.ty.display()
                    );
                    self.report(
                        right,
                        severity::WARNING,
                        "type-mismatch",
                        message,
                    );
                }
                // `+=` into an accumulator: see `accumulator_value`.
            }
            kind::VARIABLE_DECLARATION => {
                let Some(declared) = node
                    .child_by_field_id(field::TYPE)
                    .map(|t| type_of_type_node(t, self.snapshot.text()))
                else {
                    return;
                };
                let Some(what) = stores_string_badly(&declared) else {
                    return;
                };
                for declarator in syntax::named_children(node)
                    .into_iter()
                    .filter(|c| c.kind_id() == kind::VARIABLE_DECLARATOR)
                {
                    if let Some(value) =
                        declarator.child_by_field_id(field::VALUE)
                        && self.value_kind(value) == Some(ValueKind::Text)
                    {
                        let message = format!(
                            "A string cannot be stored in {what} variable ({})",
                            declared.display()
                        );
                        self.report(
                            value,
                            severity::WARNING,
                            "type-mismatch",
                            message,
                        );
                    }
                }
            }
            kind::BINARY_EXPRESSION => {
                let Some(operator) = node.child_by_field_id(field::OPERATOR)
                else {
                    return;
                };
                // `(k - v)` meant as `(k -> v)` is reported as that.
                if self.arrow_typos.contains(&node.id()) {
                    return;
                }
                // `=` compares only in SYNTAX V3 (else it assigns).
                let v3_equals = operator.kind() == "="
                    && context.query.is_some_and(|mode| mode.v3)
                    && !syntax::self_and_ancestors(node)
                        .any(|a| a.is_error());
                if v3_equals
                    || matches!(
                        operator.kind(),
                        "==" | "!=" | "<>" | "<" | "<=" | ">" | ">="
                    )
                {
                    let side = |field: &str| node.child_by_field_name(field);
                    if let (Some(left), Some(right)) =
                        (side("left"), side("right"))
                        && let (Some(a), Some(b)) =
                            (self.value_kind(left), self.value_kind(right))
                        && matches!(
                            (a, b),
                            (ValueKind::Number, ValueKind::Text)
                                | (ValueKind::Text, ValueKind::Number)
                        )
                    {
                        let text =
                            if a == ValueKind::Text { left } else { right };
                        let message = format!(
                            "`{}` compares a number with a string",
                            operator.kind()
                        );
                        self.report(
                            text,
                            severity::WARNING,
                            "type-mismatch",
                            message,
                        );
                    }
                    return;
                }
                if !matches!(operator.kind(), "-" | "*" | "/" | "%") {
                    return;
                }
                for field in ["left", "right"] {
                    if let Some(operand) = node.child_by_field_name(field)
                        && self.value_kind(operand) == Some(ValueKind::Text)
                    {
                        let message = format!(
                            "Arithmetic with `{}` needs numbers, not a string",
                            operator.kind()
                        );
                        self.report(
                            operand,
                            severity::WARNING,
                            "type-mismatch",
                            message,
                        );
                    }
                }
            }
            // `-"a"`
            kind::UNARY_EXPRESSION => {
                if node
                    .child_by_field_id(field::OPERATOR)
                    .is_some_and(|o| o.kind() == "-")
                    && let Some(operand) =
                        node.child_by_field_id(field::OPERAND)
                    && self.value_kind(operand) == Some(ValueKind::Text)
                {
                    let message =
                        "Arithmetic with `-` needs numbers, not a string"
                            .to_string();
                    self.report(
                        operand,
                        severity::WARNING,
                        "type-mismatch",
                        message,
                    );
                }
            }
            // `(p:Person {age: "abc"})` means `p.age == "abc"`.
            kind::PAIR
                if node
                    .parent()
                    .is_some_and(|p| p.kind_id() == kind::PROPERTY_MAP) =>
            {
                let (Some(key), Some(value)) = (
                    node.child_by_field_id(field::KEY),
                    node.child_by_field_id(field::VALUE),
                ) else {
                    return;
                };
                let (Some(given), true) = (
                    self.value_kind(value),
                    key.kind_id() == kind::IDENTIFIER,
                ) else {
                    return;
                };
                let declared = self.member_types(key).unwrap_or_default();
                let declared = declared
                    .first()
                    .and_then(value_kind_of)
                    .filter(|first| {
                        declared
                            .iter()
                            .all(|t| value_kind_of(t) == Some(*first))
                    });
                if let Some(declared) = declared
                    && matches!(
                        (given, declared),
                        (ValueKind::Number, ValueKind::Text)
                            | (ValueKind::Text, ValueKind::Number)
                    )
                {
                    let (what, other) = if given == ValueKind::Text {
                        ("a number", "a string")
                    } else {
                        ("a string", "a number")
                    };
                    let message = format!(
                        "`{}` is {what}, but is compared with {other}",
                        self.text(key)
                    );
                    self.report(
                        value,
                        severity::WARNING,
                        "type-mismatch",
                        message,
                    );
                }
            }
            _ => {}
        }
    }

    /// `RETURN` needs RETURNS, and a string is no number or BOOL.
    fn return_value(&mut self, node: Node, context: &Context) {
        let Some(value) = node.child_by_field_id(field::VALUE) else {
            return;
        };
        match context.returns {
            Returns::Unknown => {}
            Returns::Missing => self.report(
                value,
                severity::WARNING,
                "type-mismatch",
                "RETURN has a value, but the query has no RETURNS clause"
                    .into(),
            ),
            Returns::Declared(Some(what))
                if self.value_kind(value) == Some(ValueKind::Text) =>
            {
                let message = format!(
                    "A string cannot be returned by a query that returns {what}"
                );
                self.report(
                    value,
                    severity::WARNING,
                    "type-mismatch",
                    message,
                );
            }
            Returns::Declared(_) => {}
        }
    }

    /// Methods that modify an accumulator are restricted by clause.
    fn mutator(&mut self, node: Node, context: &Context) {
        let Some(function) = node
            .child_by_field_id(field::FUNCTION)
            .filter(|f| f.kind_id() == kind::MEMBER_EXPRESSION)
        else {
            return;
        };
        let (Some(object), Some(method)) = (
            function.child_by_field_id(field::OBJECT),
            function.child_by_field_id(field::PROPERTY),
        ) else {
            return;
        };
        // `@@acc.method()` or `v.@acc.method()`
        let accumulator = match object.kind_id() {
            kind::GLOBAL_ACCUMULATOR => object,
            kind::MEMBER_EXPRESSION => {
                match object.child_by_field_id(field::PROPERTY) {
                    Some(property)
                        if property.kind_id() == kind::LOCAL_ACCUMULATOR =>
                    {
                        property
                    }
                    _ => return,
                }
            }
            _ => return,
        };
        if !self.calls_mutator(accumulator, method) {
            return;
        }
        let call = format!(".{}()", self.text(method));
        let problem = if context.print {
            Some(format!(
                "The accumulator mutator `{call}` cannot be used in PRINT"
            ))
        } else if accumulator.kind_id() == kind::GLOBAL_ACCUMULATOR
            && (context.accum || context.post_accum)
        {
            Some(format!(
                "Mutators of global accumulators such as `{call}` can only be called at the query-body level, not inside ACCUM or POST-ACCUM"
            ))
        } else if accumulator.kind_id() == kind::LOCAL_ACCUMULATOR
            && !context.post_accum
        {
            Some(format!(
                "Mutators of vertex-attached accumulators such as `{call}` can only be called in POST-ACCUM"
            ))
        } else {
            None
        };
        if let Some(message) = problem {
            self.report(
                method,
                severity::ERROR,
                "accumulator-mutator",
                message,
            );
        }
    }

    /// Features the reference marks as deprecated.
    fn deprecated(&mut self, node: Node, kind: u16) {
        let message = match kind {
            keyword::COMPRESS => {
                "`STRING COMPRESS` is deprecated since TigerGraph 3.0 and cannot be used in new schemas"
            }
            kind::TAG_STATEMENT | kind::TAGS_CLAUSE => {
                "Tag-based access control is deprecated"
            }
            keyword::TAGS
                if node.parent().is_some_and(|p| {
                    p.kind_id() == kind::PRINT_STATEMENT
                }) =>
            {
                "`PRINT ... WITH TAGS` belongs to tag-based graphs, which are deprecated"
            }
            _ => return,
        };
        let span_node = if node.kind_id() == keyword::COMPRESS {
            node
        } else {
            node.child(0).unwrap_or(node)
        };
        let mut d = diagnostic(
            self.snapshot,
            Span::of(span_node),
            severity::HINT,
            "deprecated",
            message.into(),
        );
        d.tags = vec![diagnostic_tag::DEPRECATED];
        self.out.push(d);
    }

    fn reserved_tags(&mut self, node: Node) {
        let field = if node.kind_id() == kind::TAG_STATEMENT {
            "name"
        } else {
            "tag"
        };
        for tag in syntax::children_by_field(node, field) {
            let name = self.text(tag).to_string();
            if is_ddl_reserved(&name) {
                let message = format!(
                    "`{name}` is a reserved word in GSQL and cannot name a tag"
                );
                self.report(tag, severity::ERROR, "reserved-word", message);
            }
        }
    }

    /// Features the GSQL interpreter does not support (`INTERPRET QUERY`).
    fn interpreted(&mut self, node: Node, kind: u16, context: &Context) {
        let what: Option<String> = match kind {
            kind::FILE_DECLARATION => Some("FILE objects".into()),
            kind::PARAMETER => {
                node.child_by_field_id(field::TYPE)
                    .and_then(|t| match t.kind_id() {
                        kind::FILE_TYPE => {
                            Some("FILE parameters".to_string())
                        }
                        kind::COLLECTION_TYPE => {
                            let bag = t
                                .child_by_field_id(field::KIND)
                                .is_some_and(|k| k.kind_id() == keyword::BAG);
                            bag.then(|| "BAG parameters".to_string())
                        }
                        _ => None,
                    })
            }
            kind::PRINT_STATEMENT => syntax::has_child(node, "TO_CSV")
                .then(|| "PRINT ... TO_CSV".to_string()),
            kind::RAISE_STATEMENT => Some("RAISE".into()),
            kind::TRY_STATEMENT => Some("TRY ... EXCEPTION".into()),
            kind::RETURN_STATEMENT => Some("RETURN".into()),
            kind::ACCUMULATOR_KIND
                if self
                    .text(node)
                    .eq_ignore_ascii_case("arrayaccum") =>
            {
                Some("ArrayAccum".into())
            }
            kind::MEMBER_EXPRESSION
                if node
                    .child_by_field_id(field::PRIME)
                    .is_some() =>
            {
                Some("the previous-value operator `'`".into())
            }
            kind::IS_EXPRESSION
                if node
                    .child_by_field_id(field::RIGHT)
                    .is_some_and(|r| r.kind_id() == kind::NULL) =>
            {
                Some("IS NULL".into())
            }
            kind::PRIMITIVE_TYPE => {
                let text = self.text(node).to_ascii_uppercase();
                matches!(text.as_str(), "JSONOBJECT" | "JSONARRAY")
                    .then_some(text)
            }
            kind::TYPED_VALUE if context.insert => {
                Some("vertex types in INSERT values".into())
            }
            kind::CALL_EXPRESSION => node
                .child_by_field_id(field::FUNCTION)
                .and_then(|f| match f.kind_id() {
                    kind::IDENTIFIER => {
                        let name = self.text(f).to_ascii_lowercase();
                        let unsupported = [
                            "loadaccum",
                            "selectvertex",
                            "coalesce",
                            "evaluate",
                            "datetime_format",
                            "parse_json_object",
                            "parse_json_array",
                        ];
                        unsupported
                            .contains(&name.as_str())
                            .then(|| format!("`{}()`", self.text(f)))
                    }
                    kind::MEMBER_EXPRESSION => f
                        .child_by_field_id(field::PROPERTY)
                        .and_then(|p| {
                            let name = self.text(p);
                            ["neighbors", "neighborAttribute"]
                                .iter()
                                .any(|m| m.eq_ignore_ascii_case(name))
                                .then(|| format!("`.{name}()`"))
                        }),
                    _ => None,
                }),
            kind::PATH_PATTERN => syntax::first_code_child(node)
                .filter(|v| v.kind_id() == kind::VERTEX_PATTERN)
                .and_then(|source| {
                    let any = source
                        .child_by_field_id(field::TYPE)
                        .is_some_and(|t| {
                            matches!(t.kind_id(), kind::WILDCARD | kind::ANY)
                        });
                    any.then(|| {
                        "`_` or `ANY` as the source vertex type".to_string()
                    })
                }),
            kind::INTERPRET_QUERY_STATEMENT => {
                self.shared_names(node);
                None
            }
            _ => None,
        };
        if let Some(what) = what {
            let message = format!(
                "Interpreted queries do not support {what}; create and install the query instead"
            );
            let span_node = match node.kind_id() {
                kind::PRINT_STATEMENT
                | kind::TRY_STATEMENT
                | kind::RAISE_STATEMENT
                | kind::RETURN_STATEMENT
                | kind::FILE_DECLARATION => node.child(0).unwrap_or(node),
                _ => node,
            };
            self.report(
                span_node,
                severity::WARNING,
                "interpreted-mode",
                message,
            );
        }
    }

    /// Interpreted queries: parameters and global accumulators share no name.
    fn shared_names(&mut self, query: Node) {
        let Some(body) = query.child_by_field_id(field::BODY) else {
            return;
        };
        let analysis = self.snapshot.analysis;
        let Some(query_scope) = analysis.query_scope_at(body.start_byte())
        else {
            return;
        };
        let symbols: Vec<&Symbol> =
            analysis.scope_symbols(query_scope).collect();
        for accumulator in symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::GlobalAccumulator)
        {
            let bare = accumulator.name.trim_start_matches('@');
            if symbols.iter().any(|s| {
                matches!(s.kind, SymbolKind::Parameter | SymbolKind::Variable)
                    && s.name == bare
            }) {
                let message = format!(
                    "`{}` has the same name as a parameter or variable, which interpreted queries do not support",
                    accumulator.name
                );
                self.out.push(diagnostic(
                    self.snapshot,
                    accumulator.name_span,
                    severity::WARNING,
                    "interpreted-mode",
                    message,
                ));
            }
        }
    }

    /// Distributed query limits, and calls to and from them.
    fn distributed(
        &mut self,
        node: Node,
        kind: u16,
        mode: Mode,
        context: &Context,
    ) {
        if kind != kind::CALL_EXPRESSION {
            return;
        }
        let Some(function) = node.child_by_field_id(field::FUNCTION) else {
            return;
        };
        let message = match function.kind_id() {
            kind::IDENTIFIER => {
                let name = self.text(function).to_string();
                if mode.distributed && name.eq_ignore_ascii_case("evaluate") {
                    Some("Distributed queries do not support `evaluate()`".to_string())
                } else if self.is_distributed_query(&name) {
                    if mode.distributed {
                        Some(format!(
                            "A distributed query cannot call the distributed query `{name}`"
                        ))
                    } else if context.accum || context.post_accum {
                        Some(format!(
                            "The distributed query `{name}` cannot be called inside ACCUM or POST-ACCUM"
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            kind::MEMBER_EXPRESSION if mode.distributed => {
                function.child_by_field_id(field::PROPERTY).and_then(|p| {
                    let name = self.text(p);
                    ["neighbors", "neighborAttribute", "edgeAttribute"]
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case(name))
                        .then(|| {
                            format!("Distributed queries do not support `.{name}()`")
                        })
                })
            }
            _ => None,
        };
        if let Some(message) = message {
            self.report(
                function,
                severity::WARNING,
                "distributed-mode",
                message,
            );
        }
    }

    fn is_distributed_query(&self, name: &str) -> bool {
        self.snapshot
            .workspace
            .find(SymbolKind::Query, name)
            .iter()
            .any(|q| {
                q.detail
                    .split_whitespace()
                    .take_while(|w| !w.eq_ignore_ascii_case("QUERY"))
                    .any(|w| w.eq_ignore_ascii_case("DISTRIBUTED"))
            })
    }
}

/// The vertex patterns of a FROM pattern.
fn pattern_vertices(pattern: Node) -> Vec<Node> {
    let mut found = Vec::new();
    syntax::walk(pattern, |n| {
        if matches!(n.kind_id(), kind::VERTEX_PATTERN | kind::NODE_PATTERN) {
            found.push(n);
        }
    });
    found
}

/// The expression inside any parentheses around `node`.
fn unparenthesized(node: Node) -> Node {
    let mut node = node;
    while node.kind_id() == kind::PARENTHESIZED_EXPRESSION {
        match syntax::first_code_child(node) {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// The accumulator a subscript is on (`@@m[k]`, `t.@m[k]`).
fn accumulator_target(node: Node) -> Node {
    match node.kind_id() {
        kind::SUBSCRIPT_EXPRESSION => node
            .child_by_field_id(field::OBJECT)
            .map(accumulator_target)
            .unwrap_or(node),
        _ => node,
    }
}

/// Identifiers used as member-access objects (`t.name`, `t.@acc`).
fn member_objects(root: Node) -> Vec<Node> {
    let mut found = Vec::new();
    syntax::walk(root, |n| {
        if n.kind_id() == kind::MEMBER_EXPRESSION {
            found.extend(
                n.child_by_field_id(field::OBJECT)
                    .filter(|o| o.kind_id() == kind::IDENTIFIER),
            );
        }
    });
    found
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// `vertex type` -> `a vertex type`, `attribute` -> `an attribute`.
fn with_article(label: &str) -> String {
    let article =
        if label.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O']) {
            "an"
        } else {
            "a"
        };
    format!("{article} {label}")
}

/// How far up a call is checked for being in error code.
const MAX_CALL_DEPTH: usize = 64;

/// The kind of value a variable, attribute or accumulator holds.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ValueKind {
    Number,
    Text,
    Bool,
}

impl ValueKind {
    fn described(self) -> &'static str {
        match self {
            ValueKind::Number => "a number",
            ValueKind::Text => "a string",
            ValueKind::Bool => "a boolean",
        }
    }
}

/// What a value given to an accumulator certainly is.
enum Given {
    Kind(ValueKind),
    /// Something of several values, described.
    Other(String),
}

/// `a vertex`, `an edge`, `a SET<STRING>`, `a MapAccum<STRING, INT>`.
fn described_type(ty: &Ty) -> String {
    match ty {
        Ty::Vertex(_) => "a vertex".into(),
        Ty::Edge(_) => "an edge".into(),
        Ty::VertexSet(_) => "a vertex set".into(),
        _ => with_article(&ty.display()),
    }
}

/// What a non-map accumulator accumulates, when certain.
fn accumulator_input_kind(ty: &Ty) -> Option<ValueKind> {
    let Ty::Accumulator(kind, args) = ty else {
        return None;
    };
    match kind.as_str() {
        "AvgAccum" | "DeviationAccum" | "DeviationPAccum"
        | "BitwiseOrAccum" | "BitwiseAndAccum" => Some(ValueKind::Number),
        "OrAccum" | "AndAccum" => Some(ValueKind::Bool),
        "SumAccum" | "MaxAccum" | "MinAccum" | "SetAccum" | "BagAccum"
        | "ListAccum" => args.first().and_then(value_kind_of),
        _ => None,
    }
}

/// An accumulator that certainly holds several values.
fn holds_several_values(ty: &Ty) -> bool {
    matches!(ty, Ty::Accumulator(kind, _) if matches!(
        kind.as_str(),
        "SetAccum" | "BagAccum" | "ListAccum" | "MapAccum" | "ArrayAccum" | "HeapAccum" | "GroupByAccum"
    ))
}

fn value_kind_of(ty: &Ty) -> Option<ValueKind> {
    match ty {
        Ty::Primitive(name) => match name.as_str() {
            "INT" | "UINT" | "FLOAT" | "DOUBLE" => Some(ValueKind::Number),
            "STRING" | "STRING COMPRESS" => Some(ValueKind::Text),
            "BOOL" => Some(ValueKind::Bool),
            _ => None,
        },
        // A Deviation accumulator reads as DOUBLE, a bitwise one as INT.
        Ty::Accumulator(kind, args) => match kind.as_str() {
            "AvgAccum" | "DeviationAccum" | "DeviationPAccum"
            | "BitwiseOrAccum" | "BitwiseAndAccum" => Some(ValueKind::Number),
            "OrAccum" | "AndAccum" => Some(ValueKind::Bool),
            "SumAccum" | "MaxAccum" | "MinAccum" => {
                args.first().and_then(value_kind_of)
            }
            _ => None,
        },
        _ => None,
    }
}

/// How a numeric or BOOL variable of `ty` reads in messages.
fn stores_string_badly(ty: &Ty) -> Option<&'static str> {
    match value_kind_of(ty)? {
        ValueKind::Number => Some("a number"),
        ValueKind::Bool => Some("a BOOL"),
        ValueKind::Text => None,
    }
}

fn is_float_type(ty: &Ty) -> bool {
    match ty {
        Ty::Primitive(name) => name == "FLOAT" || name == "DOUBLE",
        Ty::Accumulator(kind, args) => match kind.as_str() {
            "AvgAccum" | "DeviationAccum" | "DeviationPAccum" => true,
            "SumAccum" | "MaxAccum" | "MinAccum" => {
                args.first().is_some_and(is_float_type)
            }
            _ => false,
        },
        _ => false,
    }
}

fn is_list_accum(node: Node, checker: &Checker) -> bool {
    syntax::field_text(node, "kind", checker.snapshot.text())
        .is_some_and(|k| k.eq_ignore_ascii_case("listaccum"))
}

/// How many ListAccums are nested from `node`.
fn list_depth(node: Node, checker: &Checker) -> usize {
    let mut depth = 0;
    let mut current = Some(node);
    while let Some(list) = current.filter(|n| {
        n.kind_id() == kind::ACCUMULATOR_TYPE && is_list_accum(*n, checker)
    }) {
        depth += 1;
        current = list.child_by_field_id(field::ARGUMENT);
    }
    depth
}

#[cfg(test)]
mod tests {
    use crate::features::code_actions::code_actions;
    use crate::features::diagnostics::diagnostics;
    use crate::features::test_support::{
        Fixture, SCHEMA_URI, findings, with_code,
    };
    use crate::lsp::types::{Position, Range};

    #[test]
    fn reserved_words_cannot_be_names() {
        let found = findings(
            "CREATE VERTEX Order (PRIMARY_ID id STRING, type STRING)\nCREATE DIRECTED EDGE friend (FROM Order, TO Order)\nCREATE QUERY filter(INT count, STRING Class) {\n  INT new = 1;\n  PRINT count, new, Class;\n}\n",
            &[],
        );
        let reserved = with_code(&found, "reserved-word");
        for expected in [
            "`Order` is a reserved word in GSQL and cannot name a vertex type",
            "`type` is a reserved word in GSQL and cannot name an attribute",
            "`friend` is a reserved word in GSQL queries, so queries cannot refer to this edge type",
            "`filter` is a reserved word in GSQL and cannot name a query",
            "`count` is a reserved word in GSQL and cannot name a parameter",
            "`new` is a reserved word in GSQL and cannot name a variable",
        ] {
            assert!(
                reserved.contains(&expected),
                "{expected:?} not in {reserved:?}"
            );
        }
        // C++ keywords are case-sensitive.
        assert_eq!(reserved.len(), 6, "{reserved:?}");
    }

    #[test]
    fn flags_deprecated_features() {
        let found = findings(
            "CREATE VERTEX P (PRIMARY_ID id STRING, name STRING COMPRESS)\nCREATE QUERY q() {\n  res = {P.*};\n  PRINT res WITH TAGS;\n  PRINT res WITH VECTOR;\n}\n",
            &[],
        );
        assert_eq!(
            with_code(&found, "deprecated"),
            [
                "`STRING COMPRESS` is deprecated since TigerGraph 3.0 and cannot be used in new schemas",
                "`PRINT ... WITH TAGS` belongs to tag-based graphs, which are deprecated",
            ]
        );
    }

    #[test]
    fn requires_order_by_for_offset() {
        let found = findings(
            "CREATE QUERY q() {\n  R = SELECT v FROM S:v LIMIT 5 OFFSET 2;\n  T = SELECT v FROM S:v ORDER BY v.id LIMIT 5 OFFSET 2;\n  U = SELECT v FROM S:v LIMIT 2, 5;\n  PRINT R, T, U;\n}\n",
            &[],
        );
        assert_eq!(
            with_code(&found, "limit-offset"),
            ["OFFSET needs an ORDER BY clause"]
        );
    }

    #[test]
    fn reports_reverse_edge_attributes_once() {
        let found = findings(
            "CREATE DIRECTED EDGE Knows (FROM Person, TO Person, type STRING) WITH REVERSE_EDGE=\"Known_by\"\n",
            &[],
        );
        assert_eq!(with_code(&found, "reserved-word").len(), 1, "{found:?}");
    }

    #[test]
    fn accumulators_are_assigned_and_modified_where_allowed() {
        let found = findings(
            "CREATE QUERY q() {\n  SumAccum<INT> @@total;\n  ListAccum<INT> @@seen;\n  ListAccum<INT> @list;\n  SumAccum<INT> EDGE @w;\n  FOREACH i IN RANGE[1, 2] DO\n    SumAccum<INT> @inner;\n  END;\n  WHILE TRUE LIMIT 1 DO\n    SumAccum<INT> EDGE @looped;\n  END;\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t\n      ACCUM @@total = 1, @@seen.clear(), s.@list.clear(), e.@w += 1\n      POST-ACCUM @@total += e.@w, t.@list.clear();\n  @@seen.clear();\n  PRINT @@seen.size(), @@seen.clear(), R;\n}\n",
            &[],
        );
        assert_eq!(
            with_code(&found, "accumulator-declaration"),
            [
                "Vertex-attached accumulators cannot be declared inside a FOREACH loop",
                "Edge-attached accumulators cannot be declared inside a WHILE loop",
            ]
        );
        assert_eq!(
            with_code(&found, "accumulator-assignment").len(),
            1,
            "{found:?}"
        );
        assert_eq!(
            with_code(&found, "edge-accumulator"),
            ["Edge accumulators such as `@w` cannot be used in POST-ACCUM"]
        );
        let mutators = with_code(&found, "accumulator-mutator");
        assert_eq!(mutators.len(), 3, "{mutators:?}");
        assert!(
            mutators
                .iter()
                .any(|m| m.contains("global accumulators")
                    && m.contains("query-body level"))
        );
        assert!(
            mutators
                .iter()
                .any(|m| m.contains("vertex-attached accumulators")
                    && m.contains("POST-ACCUM"))
        );
        assert!(
            mutators
                .iter()
                .any(|m| m.contains("cannot be used in PRINT"))
        );
    }

    #[test]
    fn virtual_edges_are_declared_at_the_top_level_and_used_alone() {
        let found = findings(
            "CREATE QUERY q(BOOL b) {\n  CREATE DIRECTED VIRTUAL EDGE Near (FROM Person, TO Person);\n  IF b THEN\n    CREATE DIRECTED VIRTUAL EDGE Far (FROM Person, TO Person, DISCRIMINATOR(ts INT));\n  END;\n  R = SELECT t FROM Person:s -((Near>|Knows>):e)- Person:t;\n  S = SELECT t FROM Person:s -(Near>:e)- Person:t;\n  PRINT R, S;\n}\n",
            &[],
        );
        let messages = with_code(&found, "virtual-edge");
        assert_eq!(
            messages,
            [
                "Virtual edge types can only be declared at the top level of a query body",
                "Virtual edges cannot have a DISCRIMINATOR",
                "The virtual edge type `Near` cannot be combined with other edge types",
            ]
        );
    }

    #[test]
    fn a_virtual_edge_in_nested_alternations_is_reported_once() {
        let found = findings(
            "CREATE QUERY q() {\n  CREATE DIRECTED VIRTUAL EDGE Near (FROM Person, TO Person);\n  R = SELECT t FROM Person:s -((Near>|Knows>)|Far>)- Person:t;\n  PRINT R;\n}\n",
            &[],
        );
        assert_eq!(
            with_code(&found, "virtual-edge"),
            [
                "The virtual edge type `Near` cannot be combined with other edge types"
            ]
        );
    }

    #[test]
    fn every_virtual_edge_in_a_mixed_alternation_is_reported() {
        let declare = |name: &str| {
            format!(
                "  CREATE DIRECTED VIRTUAL EDGE {name} (FROM Person, TO Person);\n"
            )
        };
        let header = format!(
            "CREATE QUERY q() {{\n{}{}",
            declare("Near"),
            declare("Near2")
        );
        let expected = [
            "The virtual edge type `Near` cannot be combined with other edge types",
            "The virtual edge type `Near2` cannot be combined with other edge types",
        ];
        // The nested and the flat form of one alternation, and a V3 list of types.
        for pattern in [
            "Person:s -((Knows>|Near>)|(Far>|Near2>))- Person:t",
            "Person:s -(Knows>|Near>|Far>|Near2>)- Person:t",
            "(s:Person)-[:Knows|Near|Far|Near2]->(t:Person)",
        ] {
            assert_eq!(
                virtual_edge_messages(&header, pattern),
                expected,
                "{pattern}"
            );
        }
    }

    /// The `virtual-edge` messages for `pattern` in a query that starts with `header`.
    fn virtual_edge_messages(header: &str, pattern: &str) -> Vec<String> {
        let text = format!(
            "{header}  R = SELECT t FROM {pattern};\n  PRINT R;\n}}\n"
        );
        let found = findings(&text, &[]);
        with_code(&found, "virtual-edge")
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn a_virtual_edge_repeated_alone_is_not_combined() {
        let header = "CREATE QUERY q() {\n  \
                      CREATE DIRECTED VIRTUAL EDGE Near (FROM Person, TO Person);\n";
        let mixed = "The virtual edge type `Near` cannot be combined with other edge types";
        // Both directions of one type, and one type listed twice.
        for (pattern, expected) in [
            ("Person:s -(Near>|<Near)- Person:t", vec![]),
            ("(s:Person)-[:Near|Near]->(t:Person)", vec![]),
            (
                "Person:s -(Near>|<Near|Knows>)- Person:t",
                vec![mixed, mixed],
            ),
        ] {
            assert_eq!(
                virtual_edge_messages(header, pattern),
                expected,
                "{pattern}"
            );
        }
    }

    #[test]
    fn nested_accumulators_follow_the_documented_rules() {
        let illegal = "CREATE QUERY q() {\n  ListAccum<SetAccum<INT>> @@a;\n  MapAccum<SetAccum<INT>, INT> @@b;\n  SetAccum<SetAccum<INT>> @@c;\n  BagAccum<ListAccum<INT>> @@d;\n  PRINT @@a, @@b, @@c, @@d;\n}\n";
        let found = findings(illegal, &[]);
        assert_eq!(
            with_code(&found, "accumulator-type"),
            [
                "Only a ListAccum can be nested within a ListAccum",
                "A MapAccum key cannot be an accumulator",
                "A SetAccum cannot contain accumulators; only ListAccum, ArrayAccum, MapAccum and GroupByAccum can",
                "A BagAccum cannot contain accumulators; only ListAccum, ArrayAccum, MapAccum and GroupByAccum can",
            ]
        );
        let legal = "CREATE QUERY q() {\n  ListAccum<ListAccum<INT>> @@a;\n  ListAccum<ListAccum<ListAccum<INT>>> @@b;\n  MapAccum<STRING, ListAccum<INT>> @@c;\n  MapAccum<INT, MapAccum<INT, STRING>> @@d;\n  MapAccum<VERTEX, SumAccum<INT>> @@e;\n  MapAccum<STRING, SetAccum<VERTEX>> @@f;\n  MapAccum<STRING, GroupByAccum<VERTEX a, MaxAccum<INT> maxs>> @@g;\n  GroupByAccum<INT a, STRING b, MaxAccum<INT> maxs, ListAccum<ListAccum<INT>> lists> @@h;\n  ArrayAccum<SetAccum<INT>> @@i[3];\n  SetAccum<INT> @@j;\n  PRINT @@a, @@b, @@c, @@d, @@e, @@f, @@g, @@h, @@i, @@j;\n}\n";
        assert!(
            with_code(&findings(legal, &[]), "accumulator-type").is_empty()
        );
    }

    #[test]
    fn nesting_messages_spell_the_accumulator_type_canonically() {
        let found = findings(
            "CREATE QUERY q() {\n  setaccum<SetAccum<INT>> @@a;\n  BAGACCUM<SetAccum<INT>> @@b;\n  PRINT @@a, @@b;\n}\n",
            &[],
        );
        assert_eq!(
            with_code(&found, "accumulator-type"),
            [
                "A SetAccum cannot contain accumulators; only ListAccum, ArrayAccum, MapAccum and GroupByAccum can",
                "A BagAccum cannot contain accumulators; only ListAccum, ArrayAccum, MapAccum and GroupByAccum can",
            ]
        );
    }

    #[test]
    fn array_accum_elements_exclude_heap_map_and_groupby() {
        let illegal = "CREATE QUERY q() {\n  ArrayAccum<MapAccum<INT, INT>> @@b[2];\n  ArrayAccum<GroupByAccum<INT a, SumAccum<INT> s>> @@c[2];\n  ArrayAccum<HeapAccum<Rec>(3, v DESC)> @@a[2];\n  PRINT @@a, @@b, @@c;\n}\n";
        let found = findings(illegal, &[]);
        assert_eq!(
            with_code(&found, "accumulator-type"),
            ["An ArrayAccum cannot contain a HeapAccum, MapAccum or GroupByAccum";
                3]
        );
        let legal = "CREATE QUERY q() {\n  ArrayAccum<SumAccum<INT>> @@a[2];\n  ArrayAccum<ListAccum<INT>> @@b[2];\n  ArrayAccum<SetAccum<STRING>> @@names[10];\n  ArrayAccum<SetAccum<INT>> @@ids[][];\n  ArrayAccum<ArrayAccum<SumAccum<INT>>> @@d[2][2];\n  PRINT @@a, @@b, @@names, @@ids, @@d;\n}\n";
        assert!(
            with_code(&findings(legal, &[]), "accumulator-type").is_empty()
        );
    }

    #[test]
    fn checks_accumulator_types() {
        let text = "CREATE QUERY q() {\n  sumaccum<INT> @@a;\n  GroupByAccum<INT k, SumAccum<INT>> @@g;\n  MapAccum<INT, HeapAccum<Rec>(3, v DESC)> @@m;\n  ListAccum<ListAccum<ListAccum<ListAccum<INT>>>> @@deep;\n  ListAccum<ListAccum<ListAccum<INT>>> @@ok;\n  PRINT @@a, @@g, @@m, @@deep, @@ok;\n}\n";
        let found = findings(text, &[]);
        assert_eq!(
            with_code(&found, "accumulator-type"),
            [
                "Each GroupByAccum key and accumulator needs a name, e.g. `INT age` or `SumAccum<INT> total`",
                "A MapAccum value cannot be a HeapAccum",
                "ListAccum can be nested at most three levels deep",
            ]
        );
        assert_eq!(
            with_code(&found, "accumulator-case"),
            ["Accumulator type names are case-sensitive: write `SumAccum`"]
        );
        // The quick fix restores the canonical spelling.
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let all = diagnostics(&snapshot);
        let line = Range::new(Position::new(1, 0), Position::new(1, 20));
        let actions = code_actions(&snapshot, line, &all, None, &all, false);
        assert!(
            actions
                .iter()
                .any(|a| a.title == "Replace with `SumAccum`"),
            "{actions:?}"
        );
    }

    #[test]
    fn warns_about_exact_float_comparison() {
        let schema = "CREATE VERTEX Account (PRIMARY_ID id STRING, balance DOUBLE, limit_ DOUBLE, n INT)\n";
        let query = "CREATE QUERY q(FLOAT x, FLOAT y) {\n  AvgAccum @@avg;\n  S = {Account.*};\n  R = SELECT a FROM S:a WHERE a.balance == a.limit_ OR a.n == 1;\n  IF x == y OR @@avg == 0.5 OR x + 1 > 2 OR 1 == 1 OR sqrt(2) == x THEN PRINT R; END;\n}\n";
        let found = findings(query, &[(SCHEMA_URI, schema)]);
        // a.balance == a.limit_, x == y, @@avg == 0.5 and sqrt(2) == x; not a.n, 1 == 1 or `>`.
        assert_eq!(with_code(&found, "float-equality").len(), 4, "{found:?}");
    }

    #[test]
    fn flags_strings_where_numbers_are_needed() {
        let schema = "CREATE VERTEX Account (PRIMARY_ID id STRING, name STRING, balance DOUBLE, n INT)\n";
        let query = "CREATE QUERY q(STRING label) {\n  SumAccum<INT> @@total;\n  SumAccum<STRING> @@names;\n  SumAccum<DOUBLE> @sum;\n  SetAccum<INT> @@ids;\n  INT bad = \"x\";\n  BOOL also = \"true\";\n  STRING fine = \"x\";\n  S = {Account.*};\n  R = SELECT a FROM S:a\n      ACCUM @@total += a.name, @@total += a.n, @@names += a.name, a.@sum += label, a.@sum += a.balance * 2, @@ids += a.name,\n            @@total += \"1\" + a.n;\n  INT x = a.n - \"2\";\n  PRINT R, bad, also, fine, x;\n}\n";
        let found = findings(query, &[(SCHEMA_URI, schema)]);
        let mismatches = with_code(&found, "type-mismatch");
        assert_eq!(
            mismatches,
            [
                "A string cannot be stored in a number variable (INT)",
                "A string cannot be stored in a BOOL variable (BOOL)",
                "A string cannot be added to a SumAccum<INT>",
                "A string cannot be added to a SumAccum<DOUBLE>",
                "A string cannot be added to a SetAccum<INT>",
                "A string cannot be added to a SumAccum<INT>",
                "Arithmetic with `-` needs numbers, not a string",
            ],
            "{found:?}"
        );
    }

    #[test]
    fn flags_strings_assigned_to_numeric_variables() {
        let query = "CREATE QUERY q(STRING s, INT p) {\n  INT i = 0;\n  STRING t = \"a\";\n  BOOL b = true;\n  SumAccum<INT> @@acc;\n  i = \"abc\";\n  b = \"x\" + s;\n  p = \"1\";\n  t = 5;\n  t = \"b\";\n  i = 3;\n  i = s;\n  @@acc = 2;\n  FOREACH k IN RANGE[1, 3] DO k = \"z\"; END;\n  PRINT i, t, b, @@acc;\n}\n";
        let found = findings(query, &[]);
        assert_eq!(
            with_code(&found, "type-mismatch"),
            [
                "A string cannot be stored in a number variable (INT)",
                "A string cannot be stored in a BOOL variable (BOOL)",
                "A string cannot be stored in a number variable (INT)",
                "A string cannot be stored in a number variable (INT)",
                // The loop variable of `RANGE[1, 3]` is an INT.
                "A string cannot be stored in a number variable (INT)",
            ],
            "{found:?}"
        );
    }

    #[test]
    fn break_and_continue_need_a_loop() {
        let query = "CREATE QUERY q() {\n  INT i = 0;\n  IF i == 0 THEN BREAK; END;\n  CONTINUE;\n  WHILE i < 3 DO\n    IF i == 1 THEN CONTINUE; ELSE BREAK; END;\n    CASE WHEN i == 2 THEN BREAK; END;\n  END;\n  FOREACH j IN RANGE[1, 2] DO BREAK; END;\n  S = {ANY};\n  R = SELECT s FROM S:s ACCUM FOREACH j IN RANGE[1, 2] DO CONTINUE; END;\n  PRINT i;\n}\n";
        let found = findings(query, &[]);
        assert_eq!(
            with_code(&found, "loop-control"),
            [
                "BREAK is only valid inside a WHILE or FOREACH loop",
                "CONTINUE is only valid inside a WHILE or FOREACH loop",
            ],
            "{found:?}"
        );
    }

    #[test]
    fn checks_the_value_of_return_against_returns() {
        let query = "CREATE QUERY a() RETURNS (INT) { RETURN \"x\"; }\nCREATE QUERY b() { RETURN 1; }\nCREATE QUERY c(STRING s) RETURNS (STRING) { RETURN s; }\nCREATE QUERY d() RETURNS (DOUBLE) { RETURN 1; }\nCREATE QUERY e() { RETURN; }\nCREATE QUERY f() RETURNS (BOOL) { IF true THEN RETURN \"y\"; END; RETURN true; }\n";
        let found = findings(query, &[]);
        assert_eq!(
            with_code(&found, "type-mismatch"),
            [
                "A string cannot be returned by a query that returns a number",
                "RETURN has a value, but the query has no RETURNS clause",
                "A string cannot be returned by a query that returns a BOOL",
            ],
            "{found:?}"
        );
    }

    #[test]
    fn flags_comparing_numbers_with_strings() {
        let schema = "CREATE VERTEX Account (PRIMARY_ID id STRING, name STRING, n INT)\n";
        let query = "CREATE QUERY q(INT k, STRING label) {\n  S = {Account.*};\n  R = SELECT a FROM S:a WHERE a.n == \"7\" OR a.name == 7 OR k > \"1\" OR label != 1 OR a.n == k OR a.name == label OR a.name == \"x\" OR a.n >= 1;\n  PRINT R;\n}\n";
        let found = findings(query, &[(SCHEMA_URI, schema)]);
        assert_eq!(with_code(&found, "type-mismatch").len(), 4, "{found:?}");
    }

    #[test]
    fn conditions_cannot_be_compared() {
        let query = "CREATE QUERY q(INT a, INT b, BOOL x) {\n  IF (a < 1 AND b > 2) != (a > 3) THEN PRINT a; END;\n  IF (NOT x) == TRUE OR ((a IN (1, 2))) <= x THEN PRINT a; END;\n  IF x == (a < b) OR (a + 1) != b OR (x) == TRUE OR (a < b) > x OR a < b != x THEN PRINT a; END;\n}\n";
        let found = findings(query, &[]);
        let messages = with_code(&found, "compared-condition");
        // Not: a condition on the right, arithmetic or a plain name in parentheses,
        // `>`, or a comparison without parentheses.
        assert_eq!(messages.len(), 3, "{found:?}");
        assert!(
            messages[0].starts_with(
                "GSQL cannot compare a parenthesized condition with `!=`"
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn methods_need_a_stored_set_expression() {
        let query = "CREATE QUERY q() {\n  SetAccum<INT> @@a, @@b, @@c;\n  PRINT (@@a MINUS @@b).size(), ((@@a union @@b)).contains(1), (@@a INTERSECT @@b).size();\n  @@c = @@a MINUS @@b;\n  PRINT @@c.size(), (@@c).size();\n}\n";
        let found = findings(query, &[]);
        let messages = with_code(&found, "set-expression-method");
        // Not: the stored result, or a plain accumulator in parentheses.
        assert_eq!(messages.len(), 3, "{found:?}");
        assert!(
            messages[0].starts_with(
                "GSQL cannot call `size()` on a parenthesized MINUS expression"
            ),
            "{messages:?}"
        );
        assert!(
            messages[1].starts_with(
                "GSQL cannot call `contains()` on a parenthesized UNION expression"
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn post_accum_clauses_bind_to_an_alias() {
        let query = "CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  SumAccum<INT> @c;\n  S = {ANY};\n  R = SELECT v FROM S:v POST-ACCUM @@n += 1 POST-ACCUM v.@c += 1 POST-ACCUM (v) @@n += 1;\n  T = SELECT v FROM S:v POST_ACCUM @@n += v.@c;\n  PRINT R, T;\n}\n";
        let fixture = Fixture::new(query);
        let snapshot = fixture.snapshot();
        let found: Vec<_> = diagnostics(&snapshot)
            .into_iter()
            .filter(|d| d.code.as_deref() == Some("non-binding-post-accum"))
            .collect();
        // Only the first clause: the others use `v` or are bound to it.
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].range.start,
            Position {
                line: 4,
                character: 24
            }
        );
        let actions = code_actions(
            &snapshot,
            found[0].range,
            std::slice::from_ref(&found[0]),
            None,
            &found,
            false,
        );
        let titles: Vec<&str> = actions
            .iter()
            .map(|a| a.title.as_str())
            .collect();
        assert_eq!(titles, ["Bind the clause to `v`: `POST-ACCUM (v)`"]);
    }

    #[test]
    fn whole_number_comparisons_are_exact() {
        let schema =
            "CREATE VERTEX Account (PRIMARY_ID id STRING, balance DOUBLE)\n";
        let query = "CREATE QUERY q(DOUBLE val) {\n  S = {Account.*};\n  R = SELECT a FROM S:a WHERE a.balance == 0 OR a.balance == -1 OR 2.0 == a.balance;\n  BOOL whole = val == float_to_int(val);\n  BOOL also = (floor(val)) == val OR val == round( val );\n  BOOL flagged = val == float_to_int(val + 1) OR val == 0.1;\n  PRINT R, whole, also, flagged;\n}\n";
        let found = findings(query, &[(SCHEMA_URI, schema)]);
        // Only `val == float_to_int(val + 1)` (another value) and `val == 0.1`.
        assert_eq!(with_code(&found, "float-equality").len(), 2, "{found:?}");
    }

    /// A query comparing an INT with `sqrt`, a builtin that returns a float.
    const SQRT_CALLER: &str = "CREATE QUERY caller() {\n  INT x = 2;\n  \
                               IF x == sqrt(4) THEN PRINT x; END;\n}\n";

    /// The number of float-equality warnings in `text`.
    fn float_equality(text: &str, others: &[(&str, &str)]) -> usize {
        with_code(&findings(text, others), "float-equality").len()
    }

    #[test]
    fn a_query_or_tuple_type_named_like_a_float_builtin_is_not_a_float() {
        assert_eq!(float_equality(SQRT_CALLER, &[]), 1);
        let query = "CREATE QUERY sqrt(INT a) RETURNS (INT) { RETURN a; }\n";
        let tuple = "TYPEDEF TUPLE <INT n> sqrt;\n";
        for shadow in [query, tuple] {
            let other = [("file:///test/other.gsql", shadow)];
            assert_eq!(float_equality(SQRT_CALLER, &other), 0, "{shadow}");
            let text = format!("{shadow}{SQRT_CALLER}");
            assert_eq!(float_equality(&text, &[]), 0, "{shadow}");
        }
    }

    #[test]
    fn a_query_named_like_a_builtin_has_the_value_its_returns_clause_names() {
        let query =
            "CREATE QUERY sqrt(INT a) RETURNS (FLOAT) { RETURN a * 1.5; }\n";
        let other = [("file:///test/other.gsql", query)];
        assert_eq!(float_equality(SQRT_CALLER, &other), 1);
        assert_eq!(float_equality(&format!("{query}{SQRT_CALLER}"), &[]), 1);
        // A tuple type of that name shadows the query.
        let tuple =
            ("file:///test/tuple.gsql", "TYPEDEF TUPLE <INT n> sqrt;\n");
        assert_eq!(float_equality(SQRT_CALLER, &[other[0], tuple]), 0);
    }

    #[test]
    fn a_query_or_tuple_type_named_like_a_rounding_builtin_does_not_round() {
        let caller = "CREATE QUERY q(DOUBLE val) {\n  \
                      BOOL b = val == round(val);\n  PRINT b;\n}\n";
        assert_eq!(float_equality(caller, &[]), 0);
        let query = "CREATE QUERY round(DOUBLE v) RETURNS (DOUBLE) { RETURN v * 1.5; }\n";
        let tuple = "TYPEDEF TUPLE <DOUBLE d> round;\n";
        for shadow in [query, tuple] {
            let other = [("file:///test/other.gsql", shadow)];
            assert_eq!(float_equality(caller, &other), 1, "{shadow}");
            assert_eq!(
                float_equality(&format!("{shadow}{caller}"), &[]),
                1,
                "{shadow}"
            );
        }
        // A tuple type declared in the query.
        let local =
            caller.replace("{\n", "{\n  TYPEDEF TUPLE <DOUBLE d> round;\n");
        assert_eq!(float_equality(&local, &[]), 1);
    }

    #[test]
    fn float_equality_has_its_own_switch() {
        let query = "CREATE QUERY q(FLOAT x, FLOAT y) {\n  IF x == y THEN PRINT x; END;\n}\n";
        let mut fixture = Fixture::new(query);
        assert_eq!(float_equality(query, &[]), 1);
        fixture.config.update(
            &serde_json::json!({ "diagnostics": { "floatEquality": false } }),
        );
        let found: Vec<String> = diagnostics(&fixture.snapshot())
            .into_iter()
            .filter_map(|d| d.code)
            .collect();
        assert!(!found.contains(&"float-equality".to_string()), "{found:?}");
        // The other language rules stay on.
        let reserved = "CREATE QUERY q(FLOAT x, FLOAT y) {\n  INT count = 1;\n  IF x == y THEN PRINT count; END;\n}\n";
        let mut fixture = Fixture::new(reserved);
        fixture.config.update(
            &serde_json::json!({ "diagnostics": { "floatEquality": false } }),
        );
        let found: Vec<String> = diagnostics(&fixture.snapshot())
            .into_iter()
            .filter_map(|d| d.code)
            .collect();
        assert!(
            found.contains(&"reserved-word".to_string())
                && !found.contains(&"float-equality".to_string()),
            "{found:?}"
        );
    }

    #[test]
    fn checks_interpreted_and_distributed_limitations() {
        let interpreted = "INTERPRET QUERY (FILE f, INT n) {\n  SumAccum<INT> @@n;\n  R = SELECT s FROM _:s WHERE s.x IS NULL;\n  PRINT R TO_CSV f;\n  RETURN 1;\n}\n";
        let found = findings(interpreted, &[]);
        let messages = with_code(&found, "interpreted-mode");
        assert_eq!(messages.len(), 6, "{messages:?}");
        assert!(
            messages
                .iter()
                .all(|m| m.contains("not support")
                    || m.contains("same name")),
            "{messages:?}"
        );
        // The same body is fine in an installed query.
        let installed =
            interpreted.replace("INTERPRET QUERY (", "CREATE QUERY q(");
        assert!(
            with_code(&findings(&installed, &[]), "interpreted-mode")
                .is_empty()
        );

        let distributed = "CREATE DISTRIBUTED QUERY helper() { PRINT 1; }\nCREATE DISTRIBUTED QUERY d(VERTEX v) {\n  S = {v};\n  R = SELECT s FROM S:s WHERE s.neighbors().size() > 1;\n  helper();\n  PRINT R;\n}\nCREATE QUERY plain() {\n  S = {Person.*};\n  R = SELECT s FROM S:s ACCUM helper();\n  helper();\n  PRINT R;\n}\n";
        let found = findings(distributed, &[]);
        assert_eq!(
            with_code(&found, "distributed-mode"),
            [
                "Distributed queries do not support `.neighbors()`",
                "A distributed query cannot call the distributed query `helper`",
                "The distributed query `helper` cannot be called inside ACCUM or POST-ACCUM",
            ]
        );
    }

    #[test]
    fn rules_can_be_turned_off() {
        let mut fixture =
            Fixture::new("CREATE QUERY q(INT count) { PRINT count; }\n");
        fixture.config.diagnostics_language_rules = false;
        assert!(diagnostics(&fixture.snapshot()).is_empty());
    }

    #[test]
    fn number_and_string_comparisons_with_v3_operators() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE UNDIRECTED EDGE Knows (FROM Person, TO Person, since INT)\n";
        let v3 = "CREATE QUERY q() FOR GRAPH G SYNTAX V3 {\n  INT n = 1;\n  SELECT p INTO T FROM (p:Person {age: \"abc\", name: 5, name: \"x\", age: 7})-[:Knows {since: \"1\"}]-(q:Person) WHERE q.age = \"7\" AND q.name = \"a\" AND q.age <> 7;\n  IF n = \"a\" THEN PRINT n; END;\n  IF n <> \"a\" THEN PRINT n; END;\n  IF n = 1 THEN PRINT n; END;\n  PRINT T;\n}\n";
        let found = findings(v3, &[(SCHEMA_URI, schema)]);
        assert_eq!(
            with_code(&found, "type-mismatch"),
            [
                "`age` is a number, but is compared with a string",
                "`name` is a string, but is compared with a number",
                "`since` is a number, but is compared with a string",
                "`=` compares a number with a string",
                "`=` compares a number with a string",
                "`<>` compares a number with a string",
            ],
            "{found:?}"
        );
        // Before SYNTAX V3, `=` is not a comparison (it has its own diagnostic).
        let v2 = "CREATE QUERY q(INT n) {\n  IF n = \"a\" THEN PRINT n; END;\n  IF n <> \"a\" THEN PRINT n; END;\n}\n";
        let found = findings(v2, &[]);
        assert_eq!(
            with_code(&found, "type-mismatch"),
            ["`<>` compares a number with a string"],
            "{found:?}"
        );
    }

    #[test]
    fn cypher_patterns_need_an_explicit_v3_only_when_v1_or_v2_is_declared() {
        let pattern = "SELECT p.name INTO T FROM (p:Person)-[e:Knows]-(q:Person);\n  SELECT q.name INTO T2 FROM (p:Person)-[e:Knows]-(q:Person);\n";
        let query = |clause: &str| {
            format!(
                "CREATE QUERY q() FOR GRAPH G {clause}{{\n  {pattern}  PRINT T;\n}}\n"
            )
        };
        let reported = |text: &str| {
            with_code(&findings(text, &[]), "cypher-syntax").len()
        };
        // Once per query.
        assert_eq!(reported(&query("SYNTAX V2 ")), 1);
        assert_eq!(reported(&query("SYNTAX v1 ")), 1);
        assert_eq!(reported(&query("SYNTAX V3 ")), 0);
        // The reference writes patterns in queries of the default syntax.
        assert_eq!(reported(&query("")), 0);
        let text = query("SYNTAX V2 ");
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let all = diagnostics(&snapshot);
        let line = Range::new(Position::new(1, 0), Position::new(1, 30));
        let actions = code_actions(&snapshot, line, &all, None, &all, false);
        assert!(
            actions
                .iter()
                .any(|a| a.title == "Change the query to SYNTAX V3"),
            "{actions:?}"
        );
    }

    const SUBQUERIES: &str = "CREATE QUERY count_of(INT n, STRING label, BOOL flag) FOR GRAPH G RETURNS (INT) { RETURN n; }\n\
        CREATE QUERY name_of() FOR GRAPH G RETURNS (STRING) { RETURN \"a\"; }\n\
        CREATE QUERY verts() FOR GRAPH G RETURNS (SET<VERTEX>) { SetAccum<VERTEX> @@s; RETURN @@s; }\n\
        CREATE QUERY plain(INT n) FOR GRAPH G { PRINT n; }\n";

    fn caller(body: &str) -> String {
        format!(
            "CREATE QUERY q(STRING s, INT i, BOOL b) FOR GRAPH G {{\n{body}\n}}\n"
        )
    }

    #[test]
    fn subquery_result_kind_must_fit_the_variable() {
        let found = findings(
            &caller(
                "  INT a = name_of();\n  BOOL c = name_of();\n  a = name_of();\n  STRING d = count_of(1, \"x\", TRUE);\n  INT e = count_of(1, \"x\", TRUE);\n  FLOAT f = count_of(1, \"x\", TRUE);\n  SET<VERTEX> v = verts();\n  PRINT a, c, d, e, f, v;",
            ),
            &[("file:///test/s.gsql", SUBQUERIES)],
        );
        assert_eq!(
            with_code(&found, "type-mismatch"),
            [
                "A string cannot be stored in a number variable (INT)",
                "A string cannot be stored in a BOOL variable (BOOL)",
                "A string cannot be stored in a number variable (INT)",
            ],
            "{found:?}"
        );
    }

    #[test]
    fn subquery_without_returns_gives_no_value() {
        let found = findings(
            &caller(
                "  INT a = plain(1);\n  a = plain(2);\n  INT c = count_of(1, \"x\", TRUE);\n  PRINT a, c;",
            ),
            &[("file:///test/s.gsql", SUBQUERIES)],
        );
        let expected =
            "Query `plain` has no RETURNS clause, so it returns no value";
        assert_eq!(
            with_code(&found, "type-mismatch"),
            [expected, expected],
            "{found:?}"
        );
        // Without the query in the workspace, or with two of that name, nothing is known.
        assert!(
            with_code(
                &findings(&caller("  INT a = plain(1);\n  PRINT a;"), &[]),
                "type-mismatch"
            )
            .is_empty()
        );
        let twice = format!(
            "{SUBQUERIES}CREATE QUERY plain(INT n) FOR GRAPH G RETURNS (INT) {{ RETURN n; }}\n"
        );
        let found = findings(
            &caller("  INT a = plain(1);\n  PRINT a;"),
            &[("file:///test/s.gsql", &twice)],
        );
        assert!(with_code(&found, "type-mismatch").is_empty(), "{found:?}");
    }

    #[test]
    fn a_tuple_type_shadows_a_subquery_of_the_same_name() {
        // `plain` takes an INT and returns nothing.
        let call = |typedef: &str| {
            caller(&format!(
                "  {typedef}ListAccum<plain> @@l;\n  @@l += plain(s);\n  PRINT @@l;"
            ))
        };
        let subquery_findings =
            |text: &str, others: &[(&str, &str)]| -> Vec<String> {
                let found = findings(text, others);
                let mut messages = with_code(&found, "type-mismatch");
                messages.extend(with_code(&found, "argument-type"));
                messages
                    .into_iter()
                    .map(String::from)
                    .collect()
            };
        let subqueries = ("file:///test/s.gsql", SUBQUERIES);
        assert_eq!(
            subquery_findings(&call(""), &[subqueries]),
            [
                "Query `plain` has no RETURNS clause, so it returns no value",
                "Query `plain` expects INT for `n`, not a string",
            ]
        );
        let tuple = "TYPEDEF TUPLE <STRING label> plain;\n";
        // In another file, and in the calling query.
        let others = [("file:///test/t.gsql", tuple), subqueries];
        assert!(subquery_findings(&call(""), &others).is_empty());
        assert!(subquery_findings(&call(tuple), &[subqueries]).is_empty());
    }

    #[test]
    fn subquery_arguments_of_certain_kind_must_fit_the_parameter() {
        let found = findings(
            &caller(
                "  INT a = count_of(s, s, b);\n  INT c = count_of(b, i, i);\n  INT d = count_of(i, i, i);\n  INT e = count_of(i + 1, name_of(), b);\n  INT g = count_of(name_of(), s, b);\n  PRINT a, c, d, e, g;",
            ),
            &[("file:///test/s.gsql", SUBQUERIES)],
        );
        assert_eq!(
            with_code(&found, "argument-type"),
            [
                "Query `count_of` expects INT for `n`, not a string",
                "Query `count_of` expects INT for `n`, not a boolean",
                "Query `count_of` expects BOOL for `flag`, not a number",
                "Query `count_of` expects BOOL for `flag`, not a number",
                "Query `count_of` expects INT for `n`, not a string",
            ],
            "{found:?}"
        );
    }

    #[test]
    fn subquery_literal_arguments_are_reported_once() {
        let found = findings(
            &caller("  INT a = count_of(\"x\", 1, TRUE);\n  PRINT a;"),
            &[("file:///test/s.gsql", SUBQUERIES)],
        );
        assert_eq!(with_code(&found, "argument-type").len(), 1, "{found:?}");
    }

    #[test]
    fn subquery_arguments_with_tilde_or_not_are_judged_like_other_values() {
        let found = findings(
            &caller(
                "  INT a = count_of(1, \"x\", -1);\n  INT c = count_of(1, \"x\", ~1);\n  INT d = count_of(NOT 1, \"x\", TRUE);\n  PRINT a, c, d;",
            ),
            &[("file:///test/s.gsql", SUBQUERIES)],
        );
        assert_eq!(
            with_code(&found, "argument-type"),
            [
                "Query `count_of` expects BOOL for `flag`, not a negative integer",
                "Query `count_of` expects BOOL for `flag`, not a number",
                "Query `count_of` expects INT for `n`, not a boolean",
            ],
            "{found:?}"
        );
    }

    fn select_block(select: &str) -> String {
        format!(
            "CREATE QUERY q() FOR GRAPH G {{\n  SumAccum<INT> @cnt;\n  SumAccum<INT> @@n;\n  S = {{Person.*}};\n  R = {select};\n  PRINT R;\n}}\n"
        )
    }

    fn select_messages(select: &str, code: &str) -> Vec<String> {
        let found = findings(&select_block(select), &[]);
        with_code(&found, code)
            .iter()
            .map(|m| m.to_string())
            .collect()
    }

    #[test]
    fn having_uses_the_selected_alias() {
        let messages = |select: &str| select_messages(select, "having-alias");
        // The documentation's example, in both patterns syntaxes.
        let docs = "SELECT v FROM (v:S) -[e]- (tgt:Post) HAVING tgt.subject == \"cats\"";
        assert_eq!(
            messages(docs),
            [
                "The SELECT block selects `v`, but the HAVING clause uses `tgt`"
            ]
        );
        let v1 = "SELECT v FROM S:v -(:e)- Post:tgt HAVING tgt.subject == \"cats\"";
        assert_eq!(messages(v1).len(), 1);
        // Accumulators of another alias count too.
        assert_eq!(
            messages("SELECT v FROM S:v -(E)- Post:t HAVING v.@cnt > 1 AND t.@cnt > 1")
                .len(),
            1
        );
        // Valid: the selected alias, non-alias names, several results, GROUP BY.
        for ok in [
            "SELECT v FROM (v:S) -[e]- (tgt:Post) HAVING v.@cnt > 1",
            "SELECT v FROM (v:S) -[e]- (tgt:Post) HAVING @@n > 1 AND v.name == \"a\"",
            "SELECT tgt FROM (v:S) -[e]- (tgt:Post) HAVING tgt.subject == \"cats\"",
            "SELECT v, tgt FROM (v:S) -[e]- (tgt:Post) HAVING tgt.subject == \"cats\"",
            "SELECT v.name FROM (v:S) -[e]- (tgt:Post) HAVING tgt.subject == \"cats\"",
            "SELECT v FROM S:v -(E:e)- Post:tgt HAVING v.@cnt > 1 ORDER BY tgt.x",
        ] {
            assert!(messages(ok).is_empty(), "{ok}");
        }
    }

    #[test]
    fn per_confines_the_aliases_of_select_and_accumulation() {
        let from = "FROM S:s - (E1:edge1) - M:m - (E2:edge2) - T:t";
        let count = |tail: &str| {
            select_messages(&format!("SELECT {tail}"), "per-alias")
        };
        // The three illegal examples of the documentation.
        assert_eq!(
            count(&format!("t {from} PER (s, m) ACCUM @@n += 1")),
            ["`t` is used here but does not appear in the PER clause"]
        );
        assert_eq!(
            count(&format!("t {from} PER (s, m) ACCUM t.@cnt += 1")).len(),
            2
        );
        assert_eq!(
            count(&format!(
                "s {from} PER (s) ACCUM s.@cnt += 1 POST-ACCUM t.@cnt = 1"
            ))
            .len(),
            1
        );
        assert_eq!(
            count("v FROM (v:S) -[e]- (t:Post) PER (v) ACCUM t.@cnt += 1")
                .len(),
            1
        );
        // Valid: the documentation's legal examples, and names that are no pattern vertices.
        for ok in [
            format!("s {from} PER (s) ACCUM @@n += 1"),
            format!("t {from} PER (s, t) ACCUM @@n += 1"),
            format!("t {from} PER (s, m, t) ACCUM t.@cnt += 1, s.@cnt += 1"),
            format!(
                "s {from} PER (s) ACCUM s.@cnt += edge1.x POST-ACCUM s.@cnt += 1"
            ),
            format!("t {from} ACCUM t.@cnt += 1"),
        ] {
            assert!(count(&ok).is_empty(), "{ok}");
        }
    }

    #[test]
    fn conjunctive_patterns_must_share_a_vertex_alias() {
        let count = |from: &str| {
            select_messages(&format!("SELECT p {from}"), "pattern-join").len()
        };
        // The documentation's invalid example, and its V3 form.
        assert_eq!(
            count(
                "FROM Person:p - (KNOWS) - Person:tgt, Post:s - (<LIKES) - Person:t"
            ),
            1
        );
        assert_eq!(
            count(
                "FROM (p:Person) - [:Friend] - (tgt:Person), (s:Post) - [:Likes] - (t:Person)"
            ),
            1
        );
        // The third pattern joins the second but not the first.
        assert_eq!(
            count("FROM X:p - (E) - Y:a, Z:b - (E) - W:c, Z:b - (E) - U:d"),
            1
        );
        // Valid: joined directly, transitively, through an alias-only vertex.
        for ok in [
            "FROM Person:p - (KNOWS) - :tgt, Post:s - (<LIKES) - :tgt",
            "FROM Person:p - (KNOWS) - :f - (LIKES>) - Post:tgt, :f - (LIKES>) - Comment:c",
            "FROM X:p - (E) - Y:a, Y:a - (E) - Z:b, Z:b - (E) - W:c",
            "FROM (p:Person) - [:Friend] - (q:Person), (q) - [:Likes] - (m:Post)",
            "FROM (p:Person) - [:Friend] - (q:Person)",
            // Undecidable or another form: an unnamed vertex, plain vertex sets.
            "FROM X:p - (E) - Y, Z:b - (E) - W:c",
            "FROM X:p, Z:b",
            "FROM X:p, Z:b - (E) - W:c",
        ] {
            assert_eq!(count(ok), 0, "{ok}");
        }
    }

    #[test]
    fn a_plain_assignment_before_the_read_is_only_a_hint_and_equal_bounds_are_a_count()
     {
        let severities = |text: &str, code: &str| -> Vec<Option<u8>> {
            let fixture = Fixture::new(text);
            crate::features::diagnostics::diagnostics(&fixture.snapshot())
                .into_iter()
                .filter(|d| d.code.as_deref() == Some(code))
                .map(|d| d.severity)
                .collect()
        };
        let query = |tail: &str| {
            format!(
                "CREATE QUERY q() {{\n  SumAccum<INT> @c;\n  SumAccum<INT> @@n;\n  S = {{P.*}};\n  R = SELECT s FROM S:s -(E:e)- P:t {tail};\n  PRINT R;\n}}\n"
            )
        };
        assert_eq!(
            severities(
                &query("ACCUM s.@c = 1, @@n += s.@c"),
                "accumulator-read-after-write"
            ),
            [Some(crate::lsp::types::severity::HINT)]
        );
        assert_eq!(
            severities(
                &query("ACCUM s.@c += 1, @@n += s.@c"),
                "accumulator-read-after-write"
            ),
            [Some(crate::lsp::types::severity::WARNING)]
        );
        let edge = |bounds: &str| {
            format!(
                "CREATE QUERY q() {{\n  S = {{P.*}};\n  R = SELECT t FROM S:s -(E>*{bounds}:e)- P:t;\n  PRINT R;\n}}\n"
            )
        };
        for exact in ["3", "2..2"] {
            assert!(
                severities(&edge(exact), "kleene-edge-alias").is_empty(),
                "*{exact}"
            );
        }
        assert_eq!(severities(&edge("1..3"), "kleene-edge-alias").len(), 1);
    }

    #[test]
    fn accumulators_are_read_from_the_snapshot_of_the_accum_clause() {
        let count = |tail: &str| {
            select_messages(
                &format!("SELECT s FROM S:s -(E:e)- P:t {tail}"),
                "accumulator-read-after-write",
            )
        };
        // The documentation's wrong example, and its global and subscripted forms.
        assert_eq!(
            count("ACCUM s.@cnt += 1, @@n += s.@cnt"),
            [
                "`s.@cnt` is read in the same ACCUM clause that updates it; the read sees the value from before the clause, not the running total (read it in POST-ACCUM)"
            ]
        );
        assert_eq!(count("ACCUM @@n += 1, t.@cnt += @@n").len(), 1);
        assert_eq!(count("ACCUM t.@cnt += 1, s.@cnt += t.@cnt").len(), 1);
        assert_eq!(
            count("ACCUM s.@cnt = 1, @@n += s.@cnt + s.@cnt").len(),
            2
        );
        assert_eq!(
            count("ACCUM @@n += 1, IF @@n > 3 THEN t.@cnt += 1 END").len(),
            1
        );
        assert_eq!(
            count("ACCUM IF e.x > 1 THEN @@n += 1, t.@cnt += @@n END").len(),
            1
        );
        // Valid: the read moves to POST-ACCUM, comes first, or sits in the write itself.
        for ok in [
            "ACCUM s.@cnt += 1 POST-ACCUM @@n += s.@cnt",
            "ACCUM @@n += s.@cnt, s.@cnt += 1",
            "ACCUM s.@cnt += s.@cnt",
            "ACCUM IF s.@cnt == 0 THEN s.@cnt += 1, @@n += 1 END",
            "ACCUM s.@cnt += t.@cnt, t.@cnt += 1",
            "ACCUM IF e.x > 1 THEN @@n += 1 ELSE t.@cnt += @@n END",
            "ACCUM @@n += 1, t.@cnt += @@n2",
            "POST-ACCUM s.@cnt += 1, @@n += s.@cnt",
            "ACCUM s.@cnt += 1 POST-ACCUM s.@cnt += 1, t.@cnt += s.@cnt",
        ] {
            assert!(count(ok).is_empty(), "{ok}");
        }
    }

    #[test]
    fn subscripts_and_mutators_are_no_reads() {
        let found = findings(
            "CREATE QUERY q() FOR GRAPH G {\n  ListAccum<INT> @@l;\n  MapAccum<INT, INT> @@m;\n  S = {Person.*};\n  R = SELECT s FROM S:s -(E)- P:t ACCUM @@m[1] += 1, @@m[2] += 1, @@l.clear();\n}\n",
            &[],
        );
        assert!(
            with_code(&found, "accumulator-read-after-write").is_empty(),
            "{found:?}"
        );
    }

    #[test]
    fn edge_aliases_need_a_single_edge() {
        let count = |from: &str| {
            select_messages(&format!("SELECT t {from}"), "kleene-edge-alias")
        };
        assert_eq!(
            count("FROM S:s -(E>*:e)- P:t"),
            [
                "The edge alias `e` cannot be used with a `*` repetition: the number of edges varies, so it is bound to no single edge"
            ]
        );
        assert_eq!(count("FROM S:s -(E>*1..3:e)- P:t").len(), 1);
        assert_eq!(count("FROM S:s -((E>|F>)*:e)- P:t").len(), 1);
        assert_eq!(count("FROM S:s -(A>.B>*:e)- P:t").len(), 1);
        // Valid: no alias, no repetition, an exact count.
        for ok in [
            "FROM S:s -(E>*)- P:t",
            "FROM S:s -(E>*1..3)- P:t",
            "FROM S:s -(E>:e)- P:t",
            "FROM S:s -(E>*2:e)- P:t",
        ] {
            assert!(count(ok).is_empty(), "{ok}");
        }
    }

    const MAP_DECLARATIONS: &str = "  MapAccum<STRING, SumAccum<INT>> @@m;\n  MapAccum<STRING, MapAccum<INT, SumAccum<INT>>> @@n;\n  MapAccum<STRING, INT> @@plain;\n  SetAccum<INT> @@s;\n  ListAccum<INT> @@l;\n  SumAccum<INT> @@sum;\n  GroupByAccum<STRING k, SumAccum<INT> v> @@g;\n  GroupByAccum<STRING a, INT b, SumAccum<INT> total, MaxAccum<INT> most> @@g2;\n  MapAccum<STRING, SumAccum<INT>> @mv;\n";

    /// The findings of `body` in a query with the accumulators of `MAP_DECLARATIONS`.
    fn map_findings(body: &str) -> Vec<(String, String)> {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE UNDIRECTED EDGE Knows (FROM Person, TO Person)\nCREATE GRAPH G (Person, Knows)\n";
        let text = format!(
            "CREATE QUERY q(STRING p, INT n) FOR GRAPH G {{\n{MAP_DECLARATIONS}{body}\n  PRINT @@m, @@n, @@plain, @@s, @@l, @@sum, @@g, @@g2;\n}}\n"
        );
        findings(&text, &[(SCHEMA_URI, schema)])
            .into_iter()
            .filter(|(code, _)| code != "unused")
            .collect()
    }

    #[test]
    fn map_accumulators_take_key_value_pairs() {
        let cases = [
            // The `>` of `->` deleted: arithmetic, reported once as the arrow.
            (
                "  @@m += (p - 1);",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs; did you mean `->` instead of `-`?",
            ),
            (
                "  @@m += (p > 1);",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs; did you mean `->` instead of `>`?",
            ),
            (
                "  R = SELECT s FROM Person:s ACCUM s.@mv += (s.name - 1), @@m += (s.name - 1);",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs; did you mean `->` instead of `-`?",
            ),
            (
                "  @@m += (\"a\", 1);",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs, not a tuple; separate the keys from the values with `->`",
            ),
            (
                "  @@m += (p -> 1, 2);",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs (1 key and 1 value); this pair has 1 key and 2 values",
            ),
            (
                "  @@m += (\"a\" -> \"x\");",
                "type-mismatch",
                "A string cannot be added to a SumAccum<INT>",
            ),
            (
                "  @@m += (1 -> 1);",
                "type-mismatch",
                "The keys of MapAccum<STRING, SumAccum<INT>> are STRING, not a number",
            ),
            (
                "  @@plain += (\"a\" -> p);",
                "type-mismatch",
                "The values of MapAccum<STRING, INT> are INT, not a string",
            ),
            (
                "  @@plain += (\"a\" -> (1 -> 2));",
                "accumulator-input",
                "The values of MapAccum<STRING, INT> are INT, not `(key -> value)` pairs",
            ),
            (
                "  @@m += 1;",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs",
            ),
            (
                "  @@m += p;",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs",
            ),
            (
                "  @@m = \"a\";",
                "accumulator-input",
                "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs",
            ),
            // A nested MapAccum value is itself a pair.
            (
                "  @@n += (\"a\" -> 1);",
                "accumulator-input",
                "A MapAccum<INT, SumAccum<INT>> takes `(key -> value)` pairs",
            ),
            (
                "  @@n += (\"a\" -> (1 - 2));",
                "accumulator-input",
                "A MapAccum<INT, SumAccum<INT>> takes `(key -> value)` pairs; did you mean `->` instead of `-`?",
            ),
            // Pairs belong to MapAccum and GroupByAccum only.
            (
                "  @@s += (\"a\" -> 1);",
                "accumulator-input",
                "A SetAccum<INT> takes no `(key -> value)` pairs; only MapAccum and GroupByAccum do",
            ),
            (
                "  @@sum += (1 -> 1);",
                "accumulator-input",
                "A SumAccum<INT> takes no `(key -> value)` pairs; only MapAccum and GroupByAccum do",
            ),
            (
                "  @@g += (\"a\" - 1);",
                "accumulator-input",
                "A GroupByAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs; did you mean `->` instead of `-`?",
            ),
            (
                "  @@g += (\"a\" -> 1, 2);",
                "accumulator-input",
                "A GroupByAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs (1 key and 1 value); this pair has 1 key and 2 values",
            ),
            (
                "  @@g2 += (\"a\" -> 1, 2);",
                "accumulator-input",
                "A GroupByAccum<STRING, INT, SumAccum<INT>, MaxAccum<INT>> takes `(k1, k2 -> v1, v2)` pairs (2 keys and 2 values); this pair has 1 key and 2 values",
            ),
            (
                "  @@g2 += (\"a\", 1, 2, 3);",
                "accumulator-input",
                "A GroupByAccum<STRING, INT, SumAccum<INT>, MaxAccum<INT>> takes `(k1, k2 -> v1, v2)` pairs, not a tuple; separate the keys from the values with `->`",
            ),
            (
                "  @@g2 += (\"a\", 1 -> \"x\", 3);",
                "type-mismatch",
                "A string cannot be added to a SumAccum<INT>",
            ),
        ];
        for (body, code, message) in cases {
            let found = map_findings(body);
            let messages = with_code(&found, code);
            assert!(
                messages.contains(&message),
                "{body}: {message:?} not in {found:?}"
            );
            // One clear error: no generic arithmetic or comparison warning besides it.
            let others: Vec<_> = found
                .iter()
                .filter(|(c, m)| {
                    (c, m.as_str()) != (&code.to_string(), message)
                })
                .collect();
            assert!(others.is_empty(), "{body}: {others:?}");
        }
    }

    #[test]
    fn valid_map_accumulator_input_is_clean() {
        let body = "  TYPEDEF MapAccum<STRING, SumAccum<INT>> Counts;\n  Counts @@typed;\n  MapAccum<STRING, SumAccum<INT>> @@m2 = (\"x\" -> 1);\n  MapAccum<INT, ListAccum<STRING>> @@lists;\n  BitwiseOrAccum<128> @@bits;\n  @@m += (\"a\" -> 1);\n  @@m += (p -> n);\n  @@m += ((p -> n));\n  @@m += (\"a\" -> 1, \"b\" -> 2);\n  @@m += @@m2;\n  @@m += (lower(p) -> length(p));\n  @@m = (\"a\" -> 1);\n  @@n += (\"a\" -> (1 -> 2));\n  @@plain += (\"a\" -> n);\n  @@lists += (1 -> \"x\");\n  @@g += (p -> n);\n  @@g2 += (\"a\", 1 -> 2, 3);\n  @@typed += (\"a\" -> 1);\n  @@bits += 1;\n  @@s += 1;\n  @@s += (1, 2);\n  @@l += (1, 2);\n  @@l += [1, 2];\n  @@sum += n - 1;\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t\n      ACCUM t.@mv += (s.name -> 1), @@m += (s.name -> s.age), @@g += (s.name -> 1)\n      POST-ACCUM t.@mv += (\"a\" -> 1);\n  PRINT @@typed, @@m2, @@lists, @@bits, R;";
        let found = map_findings(body);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn the_mistyped_arrow_has_a_quick_fix() {
        let text = "CREATE QUERY q(STRING p) FOR GRAPH G {\n  MapAccum<STRING, SumAccum<INT>> @@m;\n  @@m += (p - 1);\n  @@m += (p, 1);\n  PRINT @@m;\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let all = diagnostics(&snapshot);
        let whole = Range::new(Position::new(0, 0), Position::new(6, 0));
        let actions = code_actions(&snapshot, whole, &all, None, &all, false);
        let edits = |title: &str| {
            let action = actions
                .iter()
                .find(|a| a.title == title)
                .unwrap_or_else(|| panic!("{title}: {actions:?}"));
            action
                .edit
                .changes
                .values()
                .flatten()
                .map(|e| (e.range, e.new_text.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            edits("Replace `-` with `->`"),
            [(
                Range::new(Position::new(2, 12), Position::new(2, 13)),
                "->".to_string()
            )]
        );
        assert_eq!(
            edits("Replace `,` with `->`"),
            [(
                Range::new(Position::new(3, 11), Position::new(3, 12)),
                " ->".to_string()
            )]
        );
    }

    #[test]
    fn accumulator_types_take_the_right_number_of_arguments() {
        let text = "CREATE QUERY q() {\n  MapAccum<STRING> @@a;\n  MapAccum<STRING, INT, INT> @@b;\n  MapAccum @@c;\n  SumAccum<INT, INT> @@d;\n  SumAccum @@e;\n  ListAccum<128> @@f;\n  GroupByAccum<INT a, STRING b> @@g;\n  SetAccum<INT x> @@h;\n  AvgAccum<INT, INT> @@i;\n  AvgAccum<INT> @@j;\n  DeviationPAccum<DOUBLE> @@k;\n  HeapAccum @@l;\n  BitwiseOrAccum<STRING> @@m;\n  BitwiseOrAccum<SumAccum<INT>> @@n;\n  OrAccum<1> @@o;\n  SumAccum<BOOL> @@p;\n  SumAccum<VERTEX> @@q;\n  GroupByAccum<SumAccum<INT> v, STRING k> @@r;\n  GroupByAccum<STRING k, STRING k, SumAccum<INT> v> @@s;\n  MapAccum<STRING, SumAccum<BOOL>> @@t;\n  PRINT @@a, @@b, @@c, @@d, @@e, @@f, @@g, @@h, @@i, @@j, @@k, @@l, @@m, @@n, @@o, @@p, @@q, @@r, @@s, @@t;\n}\n";
        let found = findings(text, &[]);
        assert_eq!(
            with_code(&found, "accumulator-type"),
            [
                "MapAccum takes two type arguments, `MapAccum<key_type, value_type>`, not 1",
                "MapAccum takes two type arguments, `MapAccum<key_type, value_type>`, not 3",
                "MapAccum takes two type arguments, `MapAccum<key_type, value_type>`, not 0",
                "SumAccum takes one type argument, its element type (`SumAccum<type>`), not 2",
                "SumAccum takes one type argument, its element type (`SumAccum<type>`), not 0",
                "ListAccum takes a type, not a number; only BitwiseOrAccum and BitwiseAndAccum take a bit length",
                "GroupByAccum needs at least one key and one accumulator, e.g. `GroupByAccum<INT age, SumAccum<INT> total>`",
                "Only GroupByAccum names its type arguments; write `SetAccum<type>`",
                "AvgAccum is declared without a type argument; it accepts INT, UINT, FLOAT and DOUBLE inputs",
                "AvgAccum is declared without a type argument; it accepts INT, UINT, FLOAT and DOUBLE inputs",
                "DeviationPAccum is declared without a type argument; it accepts INT, UINT, FLOAT and DOUBLE inputs",
                "HeapAccum takes one type argument, its tuple type (`HeapAccum<tuple_type>(capacity, field ASC|DESC)`), not 0",
                "BitwiseOrAccum takes a bit length (a number or an INT parameter), not a type",
                "BitwiseOrAccum takes a bit length (a number or an INT parameter), not a type",
                "OrAccum takes no bit length; only BitwiseOrAccum and BitwiseAndAccum do",
                "SumAccum takes INT, UINT, FLOAT, DOUBLE or STRING, not BOOL",
                "SumAccum takes INT, UINT, FLOAT, DOUBLE or STRING, not VERTEX",
                "GroupByAccum keys come before its accumulators, e.g. `GroupByAccum<INT age, SumAccum<INT> total>`",
                "GroupByAccum already has a field named `k`",
                "SumAccum takes INT, UINT, FLOAT, DOUBLE or STRING, not BOOL",
            ]
        );
        let valid = "TYPEDEF TUPLE<STRING name, INT score> Rec;\nCREATE QUERY ok(INT len) {\n  TYPEDEF HeapAccum<Rec>(10, score DESC) Top;\n  Top @@top;\n  HeapAccum<Rec>(5, score ASC) @@heap;\n  BitwiseOrAccum<128> @@bits;\n  BitwiseAndAccum<len> @@band;\n  BitwiseAndAccum @@b64;\n  AvgAccum @@avg;\n  OrAccum @@or;\n  AndAccum @@and;\n  DeviationAccum @@dev;\n  ArrayAccum<SumAccum<INT>> @@arr[3];\n  GroupByAccum<INT a, STRING b, MaxAccum<INT> m, ListAccum<STRING> l> @@g;\n  TYPEDEF SumAccum<INT> Cnt;\n  GroupByAccum<STRING k, Top t> @@g1;\n  GroupByAccum<STRING k, Cnt c> @@g2;\n  GroupByAccum<STRING k, Cnt c, SumAccum<INT> s> @@g3;\n  MapAccum<STRING, Cnt> @@m;\n  OrAccum<BOOL> @@or2;\n  SumAccum<STRING> @@text;\n  SumAccum<UINT> @@u;\n  @@g1 += (\"a\" -> Rec(\"x\", 1));\n  @@g2 += (\"a\" -> 1);\n  @@g3 += (\"a\" -> 1, 2);\n  @@m += (\"a\" -> 1);\n  PRINT @@top, @@heap, @@bits, @@band, @@b64, @@avg, @@or, @@and, @@dev, @@arr, @@g, @@g1, @@g2, @@g3, @@m, @@or2, @@text, @@u;\n}\n";
        let found: Vec<_> = findings(valid, &[])
            .into_iter()
            .filter(|(code, _)| code != "unused")
            .collect();
        assert!(found.is_empty(), "{found:?}");
    }

    /// The findings (without `unused`) of a query over a Person/Knows schema.
    fn graph_findings(header: &str, body: &str) -> Vec<(String, String)> {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE UNDIRECTED EDGE Knows (FROM Person, TO Person, w INT)\nCREATE GRAPH G (Person, Knows)\nTYPEDEF TUPLE<STRING name, INT score> Rec;\n";
        let text =
            format!("CREATE QUERY q({header}) FOR GRAPH G {{\n{body}\n}}\n");
        findings(&text, &[(SCHEMA_URI, schema)])
            .into_iter()
            .filter(|(code, _)| code != "unused")
            .collect()
    }

    #[test]
    fn map_input_is_judged_by_shape_kind_and_source() {
        let declarations = "  MapAccum<STRING, SumAccum<INT>> @@m;\n  MapAccum<STRING, SumAccum<INT>> @mv;\n  MapAccum<INT, SumAccum<INT>> @@k;\n  MapAccum<STRING, Rec> @@mt;\n  MapAccum<Rec, INT> @@tk;\n  MapAccum<STRING, OrAccum> @@mo;\n  MapAccum<STRING, ListAccum<STRING>> @@ml;\n  SetAccum<INT> @@s;\n  SetAccum<STRING> @@ss;\n  ListAccum<INT> @@l;\n  SumAccum<INT> @@sum;\n  OrAccum @@or;\n  AvgAccum @@avg;\n  TYPEDEF SumAccum<INT> Cnt;\n  GroupByAccum<STRING k, Cnt c, SumAccum<INT> s> @@g;\n";
        let m =
            "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs";
        let cases: Vec<(&str, &str, String)> = vec![
            // Scalar operators: no pair; `>>` is a mistyped `->`.
            ("@@m += (\"a\" >> 1);", "accumulator-input", format!("{m}; did you mean `->` instead of `>>`?")),
            ("@@m += (\"a\" << 1);", "accumulator-input", m.into()),
            ("@@m += (\"a\" & 1);", "accumulator-input", m.into()),
            ("@@m += (\"a\" | 1);", "accumulator-input", m.into()),
            ("@@m += (\"a\" ^ 1);", "accumulator-input", m.into()),
            ("@@m += (p IN ps);", "accumulator-input", m.into()),
            ("@@m += (\"a\" LIKE \"b\");", "accumulator-input", m.into()),
            ("@@m += (\"a\" IS NULL);", "accumulator-input", m.into()),
            // `>=` is no mistyped arrow, and the comparison is not reported besides.
            ("@@m += (p >= 1);", "accumulator-input", m.into()),
            // Collections and other accumulators are no pairs.
            ("@@m += @@s;", "accumulator-input", format!("{m}, not a SetAccum<INT>")),
            ("@@m += @@l;", "accumulator-input", format!("{m}, not a ListAccum<INT>")),
            ("@@m += ps;", "accumulator-input", format!("{m}, not a SET<STRING>")),
            ("@@m += @@sum;", "accumulator-input", format!("{m}, not a SumAccum<INT>")),
            (
                "@@m += @@k;",
                "type-mismatch",
                "`@@k` has INT keys, but the keys of MapAccum<STRING, SumAccum<INT>> are STRING".into(),
            ),
            // Kinds of keys and values.
            ("@@m += (\"a\" -> TRUE);", "type-mismatch", "A boolean cannot be added to a SumAccum<INT>".into()),
            (
                "@@m += (TRUE -> 1);",
                "type-mismatch",
                "The keys of MapAccum<STRING, SumAccum<INT>> are STRING, not a boolean".into(),
            ),
            ("@@m += (\"a\" -> (1, 2));", "accumulator-input", "A SumAccum<INT> takes a single value, not a tuple".into()),
            ("@@m += (\"a\" -> [1, 2]);", "accumulator-input", "A SumAccum<INT> takes a single value, not a list".into()),
            ("@@mt += (\"a\" -> 1);", "type-mismatch", "The values of MapAccum<STRING, Rec> are Rec, not a number".into()),
            ("@@tk += (\"a\" -> 1);", "type-mismatch", "The keys of MapAccum<Rec, INT> are Rec, not a string".into()),
            ("@@m += (\"a\" -> @@m);", "type-mismatch", "A MapAccum<STRING, SumAccum<INT>> cannot be added to a SumAccum<INT>".into()),
            ("@@m += (\"a\" -> @@ss);", "type-mismatch", "A SetAccum<STRING> cannot be added to a SumAccum<INT>".into()),
            ("@@mo += (\"a\" -> \"x\");", "type-mismatch", "A string cannot be added to an OrAccum".into()),
            ("@@ml += (\"a\" -> 1);", "type-mismatch", "A number cannot be added to a ListAccum<STRING>".into()),
            ("@@or += \"a\";", "type-mismatch", "A string cannot be added to an OrAccum".into()),
            ("@@sum += TRUE;", "type-mismatch", "A boolean cannot be added to a SumAccum<INT>".into()),
            ("@@m += (-\"a\" -> 1);", "type-mismatch", "Arithmetic with `-` needs numbers, not a string".into()),
            // Articles of accumulator names.
            ("@@avg += (\"a\" -> 1);", "accumulator-input", "An AvgAccum takes no `(key -> value)` pairs; only MapAccum and GroupByAccum do".into()),
            ("@@or += (1 -> TRUE);", "accumulator-input", "An OrAccum takes no `(key -> value)` pairs; only MapAccum and GroupByAccum do".into()),
            // A TYPEDEF'd accumulator is a GroupByAccum accumulator.
            (
                "@@g += (\"a\" -> 1);",
                "accumulator-input",
                "A GroupByAccum<STRING, SumAccum<INT>, SumAccum<INT>> takes `(key -> v1, v2)` pairs (1 key and 2 values); this pair has 1 key and 1 value".into(),
            ),
            // Initial values of declarations.
            ("SumAccum<INT> @@init = (\"a\" -> 1);\n  PRINT @@init;", "accumulator-input", "A SumAccum<INT> takes no `(key -> value)` pairs; only MapAccum and GroupByAccum do".into()),
            ("MapAccum<STRING, INT> @@ok = (\"a\" -> 1), @@init = 5;\n  PRINT @@ok, @@init;", "accumulator-input", "A MapAccum<STRING, INT> takes `(key -> value)` pairs".into()),
            ("MapAccum<STRING, INT> @@init = (\"a\", 1);\n  PRINT @@init;", "accumulator-input", "A MapAccum<STRING, INT> takes `(key -> value)` pairs, not a tuple; separate the keys from the values with `->`".into()),
            ("SumAccum<INT> @init = \"a\";\n  PRINT @@m;", "type-mismatch", "A string cannot be stored in a SumAccum<INT>".into()),
            ("@@sum = \"a\";", "type-mismatch", "A string cannot be stored in a SumAccum<INT>".into()),
            // A TYPEDEF'd declaration's initial value.
            ("TYPEDEF MapAccum<STRING, SumAccum<INT>> Counts;\n  Counts @@typed = (\"a\", 1);\n  PRINT @@typed;", "accumulator-input", "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs, not a tuple; separate the keys from the values with `->`".into()),
            // Unparenthesized: one error, no comparison or arithmetic warning besides.
            ("@@m += p >= 1;", "accumulator-input", m.into()),
            ("@@m += \"a\" - 1;", "accumulator-input", m.into()),
            // Vertex and edge aliases.
            ("R = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM @@m += (s -> 1);\n  PRINT R;", "type-mismatch", "The keys of MapAccum<STRING, SumAccum<INT>> are STRING, not a vertex".into()),
            ("R = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM t.@mv += (e -> 1);\n  PRINT R;", "type-mismatch", "The keys of MapAccum<STRING, SumAccum<INT>> are STRING, not an edge".into()),
            ("R = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM @@m += (s.name -> s);\n  PRINT R;", "type-mismatch", "A vertex cannot be added to a SumAccum<INT>".into()),
            ("R = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM @@m += (s.name -> e.w > 1);\n  PRINT R;", "type-mismatch", "A boolean cannot be added to a SumAccum<INT>".into()),
        ];
        let print = "\n  PRINT @@m, @@k, @@mt, @@tk, @@mo, @@ml, @@s, @@ss, @@l, @@sum, @@or, @@avg, @@g;";
        for (statement, code, message) in &cases {
            let found = graph_findings(
                "STRING p, SET<STRING> ps",
                &format!("{declarations}  {statement}{print}"),
            );
            let messages = with_code(&found, code);
            assert!(
                messages.contains(&message.as_str()),
                "{statement}: {message:?} not in {found:?}"
            );
            let others: Vec<_> = found
                .iter()
                .filter(|(c, m)| (c.as_str(), m) != (*code, message))
                .collect();
            assert!(others.is_empty(), "{statement}: {others:?}");
        }
        let valid = "  @@m += (\"a\" -> 1);\n  @@m += (p -> @@sum);\n  @@m += (\"a\" -> (1 + 2));\n  @@g += (\"a\" -> 1, 2);\n  @@l += (1, 2);\n  @@l += [1, 2];\n  @@s += @@s;\n  @@ml += (\"a\" -> \"x\");\n  @@ml += (\"a\" -> [\"x\"]);\n  @@or += TRUE;\n  @@or += p IN ps;\n  @@avg += 1;\n  @@mt += (\"a\" -> Rec(\"x\", 1));\n  @@m += @@m;\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM @@m += (s.name -> e.w), t.@mv += (t.name -> 1);\n  PRINT R;";
        let found = graph_findings(
            "STRING p, SET<STRING> ps",
            &format!("{declarations}{valid}{print}"),
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn map_methods_and_iteration_follow_the_declared_keys() {
        let declarations = "  MapAccum<STRING, SumAccum<INT>> @@m;\n  SetAccum<INT> @@s;\n  GroupByAccum<INT k, SumAccum<INT> v> @@g;\n  GroupByAccum<INT a, STRING b, SumAccum<INT> v> @@g2;\n";
        let found = graph_findings(
            "",
            &format!(
                "{declarations}  PRINT @@m.get(1), @@m.containsKey(1);\n  @@m.remove(1);\n  PRINT @@g.get(\"a\"), @@g.get(1, 2), @@g2.get(1);\n  FOREACH (k, v, w) IN @@m DO PRINT k; END;\n  FOREACH k IN @@m DO PRINT k; END;\n  FOREACH (k, v) IN @@s DO PRINT k; END;\n  PRINT @@s;"
            ),
        );
        assert_eq!(
            found
                .iter()
                .map(|(c, m)| format!("{c}: {m}"))
                .collect::<Vec<_>>(),
            [
                "type-mismatch: The keys of MapAccum<STRING, SumAccum<INT>> are STRING, not a number",
                "type-mismatch: The keys of MapAccum<STRING, SumAccum<INT>> are STRING, not a number",
                "type-mismatch: The keys of MapAccum<STRING, SumAccum<INT>> are STRING, not a number",
                "type-mismatch: The keys of GroupByAccum<INT, SumAccum<INT>> are INT, not a string",
                "argument-count: `get` expects 1 argument, the keys of GroupByAccum<INT, SumAccum<INT>>, but 2 given",
                "argument-count: `get` expects 2 arguments, the keys of GroupByAccum<INT, STRING, SumAccum<INT>>, but 1 given",
                "foreach-variables: Iterating over a MapAccum<STRING, SumAccum<INT>> binds a key and a value, `FOREACH (k, v) IN ...`, not 3 variables",
                "foreach-variables: Iterating over a MapAccum<STRING, SumAccum<INT>> binds a key and a value, `FOREACH (k, v) IN ...`, not 1 variable",
                "foreach-variables: A SetAccum<INT> yields one element at a time; write `FOREACH x IN ...`, not 2 variables",
            ]
        );
        let valid = graph_findings(
            "",
            &format!(
                "{declarations}  PRINT @@m.get(\"a\"), @@m.containsKey(\"a\"), @@g.get(1), @@g2.get(1, \"x\");\n  FOREACH (k, v) IN @@m DO PRINT k, v; END;\n  FOREACH x IN @@s DO PRINT x; END;\n  PRINT @@s;"
            ),
        );
        assert!(valid.is_empty(), "{valid:?}");
    }

    #[test]
    fn accumulators_are_no_query_parameters() {
        let found = graph_findings("MapAccum<STRING, INT> m", "  PRINT m;");
        assert_eq!(
            with_code(&found, "accumulator-type"),
            [
                "A query parameter cannot be an accumulator; declare the accumulator in the query body"
            ]
        );
        // Reported once, not also for its arguments.
        let found = graph_findings("MapAccum<STRING> m", "  PRINT m;");
        assert_eq!(
            with_code(&found, "accumulator-type"),
            [
                "A query parameter cannot be an accumulator; declare the accumulator in the query body"
            ]
        );
        // A subquery may be allowed to take one: only a warning.
        let text = "CREATE QUERY sub(ListAccum<INT> xs) FOR GRAPH G RETURNS (INT) {\n  RETURN xs.size();\n}\n";
        let fixture = Fixture::new(text);
        let found: Vec<_> = diagnostics(&fixture.snapshot())
            .into_iter()
            .filter(|d| d.code.as_deref() == Some("accumulator-type"))
            .map(|d| (d.severity, d.message))
            .collect();
        assert_eq!(
            found,
            [(
                Some(crate::lsp::types::severity::WARNING),
                "A query parameter is usually not an accumulator; pass a SET, BAG, LIST or MAP instead".to_string()
            )]
        );
    }

    #[test]
    fn single_number_accumulators_read_as_numbers() {
        let body = "  BitwiseOrAccum @@b;\n  BitwiseOrAccum @@b2;\n  BitwiseAndAccum @@ba;\n  SumAccum<INT> @@s;\n  MaxAccum<INT> @@mx;\n  DeviationAccum @@d;\n  DeviationAccum @@d2;\n  DeviationPAccum @@dp;\n  AvgAccum @@avg;\n  SumAccum<DOUBLE> @@sd;\n  MapAccum<STRING, SumAccum<INT>> @@m;\n  MapAccum<STRING, BitwiseOrAccum> @@mb;\n  MapAccum<STRING, DOUBLE> @@md;\n  MaxAccum<VERTEX> @@mv;\n  SetAccum<VERTEX> @@sv;\n  @@b2 += @@b;\n  @@b2 = @@b;\n  @@s += @@b;\n  @@mx = @@ba;\n  @@sd += @@d;\n  @@d = @@d2;\n  @@avg += @@dp;\n  @@m += (\"a\" -> @@b);\n  @@mb += (\"a\" -> @@b2);\n  @@md += (\"a\" -> @@d);\n  @@sv += @@mv;\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM @@b2 += @@b;\n  PRINT @@b, @@b2, @@ba, @@s, @@mx, @@d, @@d2, @@dp, @@avg, @@sd, @@m, @@mb, @@md, @@mv, @@sv, R;";
        let found = graph_findings("", body);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn doubtful_accumulator_type_arguments_are_warnings() {
        let text = "CREATE QUERY q() {\n  AvgAccum<DOUBLE> @@a;\n  BitwiseOrAccum<INT> @@b;\n  SumAccum<STRING COMPRESS> @@c;\n  GroupByAccum<STRING a, SumAccum<INT> A> @@g;\n  GroupByAccum<STRING Name, STRING name, SumAccum<INT> c> @@h;\n  PRINT @@a, @@b, @@c, @@g, @@h;\n}\n";
        let fixture = Fixture::new(text);
        let snapshot = fixture.snapshot();
        let all = diagnostics(&snapshot);
        let found: Vec<_> = all
            .iter()
            .filter(|d| d.code.as_deref() == Some("accumulator-type"))
            .map(|d| (d.severity, d.message.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    Some(crate::lsp::types::severity::WARNING),
                    "AvgAccum is declared without a type argument; it accepts INT, UINT, FLOAT and DOUBLE inputs"
                ),
                (
                    Some(crate::lsp::types::severity::WARNING),
                    "BitwiseOrAccum takes a bit length (a number or an INT parameter), not a type"
                ),
            ]
        );
        let whole = Range::new(Position::new(0, 0), Position::new(7, 0));
        let actions = code_actions(&snapshot, whole, &all, None, &all, false);
        let action = actions
            .iter()
            .find(|a| a.title == "Write `AvgAccum` without a type argument")
            .unwrap_or_else(|| panic!("{actions:?}"));
        let edits: Vec<_> = action
            .edit
            .changes
            .values()
            .flatten()
            .map(|e| (e.range, e.new_text.clone()))
            .collect();
        assert_eq!(
            edits,
            [(
                Range::new(Position::new(1, 10), Position::new(1, 18)),
                String::new()
            )]
        );
    }
}
