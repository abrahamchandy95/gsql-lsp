//! Rules from the GSQL language reference that the grammar does not enforce:
//! reserved words, where accumulators may be assigned or modified, virtual
//! edge restrictions, constraints on accumulator types, interpreted and
//! distributed query limitations, and the compiler's warning about exact
//! comparison of floating-point values.

use std::collections::HashSet;

use serde_json::json;
use tree_sitter::Node;

use crate::analysis::{Symbol, SymbolKind, Ty, type_of_type_node};
use crate::builtins;
use crate::features::Snapshot;
use crate::features::diagnostics::{add_fix, diagnostic};
use crate::features::resolve::{self, Target};
use crate::lsp::types::{Diagnostic, TextEdit, diagnostic_tag, severity};
use crate::syntax;
use crate::text::Span;
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

/// Words the DDL compiler rejects as names of vertex types, edge types,
/// graphs, tags and attributes.
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
    QUERY_RESERVED.split_whitespace().any(|w| w.eq_ignore_ascii_case(name))
        || CPP_RESERVED.split_whitespace().any(|w| w == name)
}

pub(crate) fn is_ddl_reserved(name: &str) -> bool {
    DDL_RESERVED.split_whitespace().any(|w| w.eq_ignore_ascii_case(name))
        || name.get(..RESERVED_PREFIX.len()).is_some_and(|p| p.eq_ignore_ascii_case(RESERVED_PREFIX))
}

pub fn check(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    reserved_words(snapshot, out);
    let type_symbols = snapshot
        .analysis
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::AccumulatorType | SymbolKind::TupleType))
        .collect();
    let mut checker = Checker { snapshot, out, cypher_reported: None, arrow_typos: HashSet::new(), type_symbols };
    let root = snapshot.root();
    let mut cursor = root.walk();
    // The context of each ancestor of the current node.
    let mut contexts = vec![Context::default()];
    loop {
        let node = cursor.node();
        let context = enter(*contexts.last().expect("never empty"), node, snapshot.text());
        checker.visit(node, &context);
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
    /// The RETURNS type, as "a number" or "a BOOL" when a string cannot be returned as it.
    Declared(Option<&'static str>),
}

fn enter(parent: Context, node: Node, source: &str) -> Context {
    let mut context = parent;
    context.in_list_accum = parent.list_accum;
    context.list_accum = node.kind() == "accumulator_type"
        && syntax::field_text(node, "kind", source).is_some_and(|k| k.eq_ignore_ascii_case("listaccum"));
    match node.kind() {
        "query_definition" | "interpret_query_statement" | "opencypher_query_definition" => {
            let versions: Vec<&str> = syntax::named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "syntax_clause")
                .filter_map(|c| syntax::field_text(c, "version", source))
                .map(|version| version.trim_matches('"'))
                .collect();
            let v3 = versions.iter().any(|version| version.eq_ignore_ascii_case("v3"));
            let old_syntax = versions.iter().any(|v| v.eq_ignore_ascii_case("v1") || v.eq_ignore_ascii_case("v2"));
            let distributed = node.child_by_field_name("modifier").is_some_and(|m| m.kind() == "DISTRIBUTED");
            let interpreted = node.kind() == "interpret_query_statement";
            context = Context {
                query: Some(Mode { interpreted, distributed, v3, old_syntax, start: node.start_byte() }),
                ..Context::default()
            };
            if node.kind() == "query_definition" {
                let clause = syntax::children(node).into_iter().find(|c| c.kind() == "returns_clause");
                context.returns = match clause {
                    // A query that does not parse cleanly may be misread.
                    None if node.has_error() => Returns::Unknown,
                    None => Returns::Missing,
                    Some(clause) => Returns::Declared(
                        clause
                            .child_by_field_name("type")
                            .and_then(|t| stores_string_badly(&type_of_type_node(t, source))),
                    ),
                };
            }
        }
        "accum_clause" => context.accum = true,
        "post_accum_clause" => context.post_accum = true,
        "print_statement" => context.print = true,
        "foreach_statement" | "dml_foreach_statement" => context.foreach = true,
        "while_statement" | "dml_while_statement" => context.while_loop = true,
        "insert_statement" => context.insert = true,
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
        let label = with_article(symbol.kind.label());
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
            | K::Query => is_query_reserved(name)
                .then(|| (severity::ERROR, format!("`{name}` is a reserved word in GSQL and cannot name {label}"))),
            K::VertexType | K::EdgeType | K::Graph | K::Attribute if is_ddl_reserved(name) => {
                Some((severity::ERROR, format!("`{name}` is a reserved word in GSQL and cannot name {label}")))
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
            out.push(diagnostic(snapshot, symbol.name_span, level, "reserved-word", message));
        }
    }
}

/// The pairs a MapAccum or GroupByAccum takes: its type, key and value types, and how
/// messages name it (`A MapAccum<STRING, INT>`) and its pairs (`` `(key -> value)` ``).
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
    /// Expressions reported as a mistyped `->` (`(k - v)`), so that the
    /// arithmetic is not reported again.
    arrow_typos: HashSet<usize>,
    /// The TYPEDEF'd accumulator and tuple types declared in this file.
    type_symbols: Vec<&'s Symbol>,
}

impl<'s> Checker<'s, '_> {
    fn text(&self, node: Node) -> &str {
        syntax::text(node, self.snapshot.text())
    }

    fn report(&mut self, node: Node, level: u8, code: &str, message: String) {
        self.out.push(diagnostic(self.snapshot, Span::of(node), level, code, message));
    }

    /// The symbol an identifier or accumulator occurrence resolves to in this file.
    fn symbol_of(&self, node: Node) -> Option<&Symbol> {
        let reference = self.snapshot.analysis.reference_at(node.start_byte())?;
        (reference.span == Span::of(node)).then_some(())?;
        reference.target.map(|id| &self.snapshot.analysis.symbols[id])
    }

    fn visit(&mut self, node: Node, context: &Context) {
        if matches!(node.kind(), "assignment_statement" | "dml_assignment_statement" | "accumulator_declarator") {
            self.accumulator_input(node);
        }
        self.type_checks(node, context);
        match node.kind() {
            "assignment_statement" if context.accum || context.post_accum => self.accumulator_assignment(node),
            "local_accumulator" if context.post_accum => self.edge_accumulator_in_post_accum(node),
            "accumulator_declaration" if context.foreach || context.while_loop => {
                self.attached_accumulator_in_loop(node, if context.foreach { "FOREACH" } else { "WHILE" })
            }
            "virtual_edge_declaration" => self.virtual_edge(node),
            "foreach_statement" if !node.has_error() => self.foreach_variables(node),
            // `CREATE QUERY q(MapAccum<STRING, INT> m)`. The arguments of a query run on its own
            // come from outside and cannot be accumulators; whether a subquery (one with
            // RETURNS) may take one is not documented, so that is only a warning.
            "parameter" => {
                if let Some(ty) = node.child_by_field_name("type").filter(|t| t.kind() == "accumulator_type") {
                    let (level, message) = if matches!(context.returns, Returns::Missing) {
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
                    self.report(ty, level, "accumulator-type", message.to_string());
                }
            }
            "edge_alternation" | "relationship_detail" => self.mixed_virtual_edges(node),
            // An accumulator as a parameter type is reported once, as that.
            "accumulator_type" if node.parent().is_none_or(|p| p.kind() != "parameter") => {
                self.accumulator_type(node, context)
            }
            "accumulator_kind" => self.accumulator_case(node),
            "binary_expression" => {
                if let Some(mode) = context.query.filter(|_| self.snapshot.config.diagnostics_float_equality) {
                    self.float_equality(node, mode);
                }
            }
            "call_expression" => {
                self.mutator(node, context);
                self.accumulator_method(node);
                if context.query.is_some() {
                    self.subquery_call(node);
                }
            }
            "cypher_pattern" => self.cypher_syntax(node, context),
            "break_statement" | "continue_statement"
                if context.query.is_some() && !context.foreach && !context.while_loop =>
            {
                let message = format!(
                    "{} is only valid inside a WHILE or FOREACH loop",
                    node.kind().replace("_statement", "").to_uppercase()
                );
                self.report(node, severity::ERROR, "loop-control", message);
            }
            "return_statement" => self.return_value(node, context),
            "limit_clause" => self.offset_without_order(node),
            "select_statement" if !node.has_error() => self.select_aliases(node),
            "accum_clause" if !node.has_error() => self.read_after_write(node),
            "edge_pattern" if !node.has_error() => self.aliased_repetition(node),
            "tag_statement" | "tag_expression" | "tags_clause" => self.reserved_tags(node),
            _ => {}
        }
        self.deprecated(node);
        if let Some(mode) = context.query {
            if mode.interpreted {
                self.interpreted(node, context);
            }
            self.distributed(node, mode, context);
        }
    }

    /// Global accumulators may only be assigned outside SELECT blocks.
    fn accumulator_assignment(&mut self, node: Node) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let assigns = node.child_by_field_name("operator").is_some_and(|o| o.kind() == "=");
        let global = match left.kind() {
            "global_accumulator" => true,
            "subscript_expression" => {
                left.child_by_field_name("object").is_some_and(|o| o.kind() == "global_accumulator")
            }
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
        if self.symbol_of(node).is_some_and(|s| s.on_edges) {
            let message = format!("Edge accumulators such as `{}` cannot be used in POST-ACCUM", self.text(node));
            self.report(node, severity::ERROR, "edge-accumulator", message);
        }
    }

    /// An accumulator read in a later statement of the ACCUM clause that writes it. Every
    /// execution of the clause starts from the same snapshot of the accumulators and the
    /// inputs are only aggregated when all executions are done (Sec. 4.3 of the GSQL paper).
    fn read_after_write(&mut self, clause: Node) {
        let mut writes = Vec::new();
        let mut reads = Vec::new();
        syntax::walk(clause, |n| match n.kind() {
            "assignment_statement" => writes.push(n),
            "global_accumulator" => reads.push(n),
            "member_expression"
                if n.child_by_field_name("property").is_some_and(|p| p.kind() == "local_accumulator") =>
            {
                reads.push(n)
            }
            _ => {}
        });
        // The accumulator a write targets is not a read.
        let targets: Vec<(usize, usize)> = writes
            .iter()
            .filter_map(|w| w.child_by_field_name("left"))
            .filter_map(|left| self.accumulator_key(left).map(|_| accumulator_target(left)))
            .map(|t| (t.start_byte(), t.end_byte()))
            .collect();
        let mut reported = Vec::new();
        for write in writes {
            let Some(key) = write.child_by_field_name("left").and_then(|l| self.accumulator_key(l)) else {
                continue;
            };
            // Only reads in the same block (not the other branch of an IF) and after the write.
            let Some(scope) = write.parent().filter(|p| matches!(p.kind(), "block" | "accum_clause")) else {
                continue;
            };
            for read in &reads {
                let span = (read.start_byte(), read.end_byte());
                if read.start_byte() < write.end_byte()
                    || read.end_byte() > scope.end_byte()
                    || targets.contains(&span)
                    || reported.contains(&span)
                    || self.is_mutated(*read)
                    || self.accumulator_key(*read).as_deref() != Some(key.as_str())
                {
                    continue;
                }
                reported.push(span);
                let message = format!(
                    "`{}` is read in the same ACCUM clause that updates it; the read sees the value from before the clause, not the running total (read it in POST-ACCUM)",
                    self.text(*read)
                );
                // (The paper's ACCUM only has `+=`: a plain `=` write is an extrapolation, a hint.)
                let level = if write.child_by_field_name("operator").is_some_and(|o| o.kind() == "=") {
                    severity::HINT
                } else {
                    severity::WARNING
                };
                self.report(*read, level, "accumulator-read-after-write", message);
            }
        }
    }

    /// The accumulator is the object of a call to a method that modifies it (`.clear()`, `.add(x)`).
    fn is_mutated(&self, read: Node) -> bool {
        let Some(member) = read.parent().filter(|p| p.kind() == "member_expression") else {
            return false;
        };
        let (Some(object), Some(method)) =
            (member.child_by_field_name("object"), member.child_by_field_name("property"))
        else {
            return false;
        };
        if object.id() != read.id() || !member.parent().is_some_and(|c| c.kind() == "call_expression") {
            return false;
        }
        let accumulator = if read.kind() == "member_expression" {
            read.child_by_field_name("property").unwrap_or(read)
        } else {
            read
        };
        let Some(Ty::Accumulator(kind, _)) = self.symbol_of(accumulator).map(|s| &s.ty) else {
            return false;
        };
        builtins::accumulator(kind)
            .and_then(|a| builtins::find_method(a.methods, self.text(method)))
            .is_some_and(|m| m.mutator)
    }

    /// `@@name` or `alias.@name` for a global accumulator or a vertex accumulator of an alias,
    /// also when subscripted.
    fn accumulator_key(&self, node: Node) -> Option<String> {
        let node = accumulator_target(node);
        match node.kind() {
            "global_accumulator" => Some(self.text(node).to_string()),
            "member_expression" => {
                let object = node.child_by_field_name("object").filter(|o| o.kind() == "identifier")?;
                let property = node.child_by_field_name("property").filter(|p| p.kind() == "local_accumulator")?;
                Some(format!("{}.{}", self.text(object), self.text(property)))
            }
            _ => None,
        }
    }

    /// An edge alias on a repeated edge pattern binds no single edge: the number of edges
    /// varies (Sec. 7 of the GSQL paper; the TigerGraph documentation forbids it).
    /// `*2..2`: equal bounds are an exact count.
    fn same_bounds(&self, bounds: Node) -> bool {
        match (bounds.child_by_field_name("min"), bounds.child_by_field_name("max")) {
            (Some(min), Some(max)) => self.text(min) == self.text(max),
            _ => false,
        }
    }

    fn aliased_repetition(&mut self, node: Node) {
        let Some(alias) = node.child_by_field_name("alias") else {
            return;
        };
        let mut star = false;
        if let Some(kind) = node.child_by_field_name("type") {
            syntax::walk(kind, |n| {
                // An exact count (`*3`) or a condition is left alone.
                if n.kind() == "edge_repetition"
                    && !syntax::named_children(n).iter().any(|b| {
                        b.kind() == "repetition_bounds"
                            && (b.child_by_field_name("exact").is_some()
                                || b.child_by_field_name("condition").is_some()
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

    /// Rules about the aliases of a SELECT block: HAVING, PER, and the
    /// patterns of the FROM clause.
    fn select_aliases(&mut self, node: Node) {
        let children = syntax::named_children(node);
        let Some(from) = children.iter().find(|c| c.kind() == "from_clause") else {
            return;
        };
        let patterns: Vec<Node> = syntax::named_children(*from)
            .into_iter()
            .filter(|c| matches!(c.kind(), "path_pattern" | "cypher_pattern"))
            .collect();
        let vertices: Vec<Vec<Node>> = patterns.iter().map(|p| pattern_vertices(*p)).collect();
        self.disjoint_patterns(&patterns, &vertices);
        let source = self.snapshot.text();
        let aliases: Vec<&str> =
            vertices.iter().flatten().filter_map(|v| syntax::field_text(*v, "alias", source)).collect();
        let results = syntax::children_by_field(node, "result");
        // The vertex aliases the block selects.
        let selected: Vec<Node> =
            results.iter().copied().filter(|r| r.kind() == "identifier" && aliases.contains(&self.text(*r))).collect();
        // HAVING: "The SELECT block selects src, but the HAVING clause uses tgt"
        // (SEM-50, the HAVING section of the SELECT statement: "The condition
        // in a HAVING clause is applied to each vertex in the SELECT set").
        // Only a block selecting a single vertex alias is judged.
        if let ([one], 1, None, Some(having)) = (
            selected.as_slice(),
            results.len(),
            children.iter().find(|c| c.kind() == "group_by_clause"),
            children.iter().find(|c| c.kind() == "having_clause"),
        ) {
            let name = self.text(*one).to_string();
            for object in member_objects(*having) {
                let used = self.text(object).to_string();
                if used != name && aliases.contains(&used.as_str()) {
                    let message = format!("The SELECT block selects `{name}`, but the HAVING clause uses `{used}`");
                    self.report(object, severity::ERROR, "having-alias", message);
                }
            }
        }
        // PER: "If the PER Clause is used in a SELECT query block, then the
        // vertex aliases used in the SELECT, ACCUM, and POST-ACCUM clauses must
        // be confined to the aliases that appear in the PER clause." (PER
        // clause page of the pattern matching tutorial; the illegal examples
        // are SELECT t, ACCUM t.@cnt and POST-ACCUM t.@cnt with t missing.)
        let per = children
            .iter()
            .copied()
            .chain(children.iter().filter(|c| c.kind() == "accum_clause").flat_map(|a| syntax::named_children(*a)))
            .find(|c| c.kind() == "per_clause");
        let Some(per) = per else {
            return;
        };
        let listed: Vec<String> = syntax::named_children(per).into_iter().map(|c| self.text(c).to_string()).collect();
        let mut uses = selected;
        for clause in children.iter().filter(|c| matches!(c.kind(), "accum_clause" | "post_accum_clause")) {
            uses.extend(member_objects(*clause).into_iter().filter(|o| aliases.contains(&self.text(*o))));
        }
        for used in uses {
            let name = self.text(used).to_string();
            if !listed.contains(&name) {
                let message = format!("`{name}` is used here but does not appear in the PER clause");
                self.report(used, severity::ERROR, "per-alias", message);
            }
        }
    }

    /// Conjunctive patterns: "If the match tables of the patterns in a FROM
    /// clause can be naturally joined into one match table, then the FROM
    /// clause has a valid CPM input. Otherwise, the FROM clause has an invalid
    /// pattern input list." ("an invalid CPM, since the two patterns do not
    /// share any vertex variables, they cannot be naturally joined", the
    /// conjunctive pattern matching page.) Judged only when every pattern has
    /// an edge (lists of plain vertex sets are another form) and every vertex
    /// has an alias, so that what is shared is known.
    fn disjoint_patterns(&mut self, patterns: &[Node], vertices: &[Vec<Node>]) {
        if patterns.len() < 2 || vertices.iter().any(|v| v.len() < 2) {
            return;
        }
        let source = self.snapshot.text();
        let names: Vec<Vec<&str>> = vertices
            .iter()
            .map(|v| v.iter().filter_map(|n| syntax::field_text(*n, "alias", source)).collect())
            .collect();
        if names.iter().zip(vertices).any(|(n, v)| n.len() != v.len()) {
            return;
        }
        // The patterns joined to the first through shared aliases.
        let mut joined = vec![false; patterns.len()];
        joined[0] = true;
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..patterns.len() {
                let shares = |j: usize| names[i].iter().any(|n| names[j].contains(n));
                if !joined[i] && (0..patterns.len()).any(|j| joined[j] && shares(j)) {
                    joined[i] = true;
                    changed = true;
                }
            }
        }
        if let Some(i) = joined.iter().position(|j| !j) {
            let message =
                "This pattern shares no vertex alias with the patterns before it, so they cannot be naturally joined";
            self.report(patterns[i], severity::WARNING, "pattern-join", message.into());
        }
    }

    /// `LIMIT k OFFSET j` skips results in a known order only; the query
    /// compiler rejects OFFSET without ORDER BY.
    fn offset_without_order(&mut self, node: Node) {
        let Some(offset) = syntax::children(node).into_iter().find(|c| c.kind() == "OFFSET") else {
            return;
        };
        let select = node.parent().filter(|p| p.kind() == "select_statement");
        if select.is_some_and(|s| syntax::named_children(s).iter().all(|c| c.kind() != "order_by_clause")) {
            self.report(offset, severity::ERROR, "limit-offset", "OFFSET needs an ORDER BY clause".into());
        }
    }

    /// Vertex- and edge-attached accumulators cannot be declared in loops.
    fn attached_accumulator_in_loop(&mut self, node: Node, keyword: &str) {
        let attached = if syntax::children(node).iter().any(|c| c.kind() == "EDGE") { "Edge" } else { "Vertex" };
        for declarator in syntax::named_children(node).into_iter().filter(|c| c.kind() == "accumulator_declarator") {
            if let Some(name) = declarator.child_by_field_name("name").filter(|n| n.kind() == "local_accumulator") {
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
        let at_top_level = node.parent().is_some_and(|p| p.kind() == "query_body");
        let name = node.child_by_field_name("name").unwrap_or(node);
        if !at_top_level {
            self.report(
                name,
                severity::ERROR,
                "virtual-edge",
                "Virtual edge types can only be declared at the top level of a query body".into(),
            );
        }
        if let Some(attributes) = node.child_by_field_name("attributes") {
            for discriminator in syntax::named_children(attributes).into_iter().filter(|c| c.kind() == "discriminator")
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

    /// A virtual edge type cannot share an edge pattern with other edge types.
    fn mixed_virtual_edges(&mut self, node: Node) {
        let names: Vec<Node> = match node.kind() {
            "relationship_detail" => {
                syntax::children_by_field(node, "type").into_iter().filter(|t| t.kind() == "identifier").collect()
            }
            _ => {
                let mut names = Vec::new();
                syntax::walk(node, |n| {
                    if n.kind() == "edge_atom"
                        && let Some(name) = n.child_by_field_name("name")
                    {
                        names.push(name);
                    }
                });
                names
            }
        };
        if names.len() < 2 {
            return;
        }
        let is_virtual =
            |name: &Node| self.symbol_of(*name).is_some_and(|s| s.kind == SymbolKind::EdgeType && s.scope != 0);
        if let Some(virtual_edge) = names.iter().find(|n| is_virtual(n)) {
            let message = format!(
                "The virtual edge type `{}` cannot be combined with other edge types",
                self.text(*virtual_edge)
            );
            self.report(*virtual_edge, severity::ERROR, "virtual-edge", message);
        }
    }

    fn accumulator_type(&mut self, node: Node, context: &Context) {
        let kind = syntax::field_text(node, "kind", self.snapshot.text()).unwrap_or("").to_ascii_lowercase();
        let arguments = syntax::children_by_field(node, "argument");
        self.type_arguments(node, &kind, &arguments);
        if kind == "listaccum" {
            // "ListAccum is the only accumulator type that can be nested within ListAccum"
            for argument in arguments.iter().filter(|a| a.kind() == "accumulator_type") {
                if !is_list_accum(*argument, self) {
                    self.report(
                        *argument,
                        severity::ERROR,
                        "accumulator-type",
                        "Only a ListAccum can be nested within a ListAccum".into(),
                    );
                }
            }
        }
        match kind.as_str() {
            "groupbyaccum" => {
                for argument in arguments.iter().filter(|a| a.kind() != "group_by_field") {
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
                // "MapAccum<SetAccum<INT>, INT> # illegal" (querying/accumulators, Nested Accumulators)
                if let Some(key) = arguments.first().filter(|k| k.kind() == "accumulator_type") {
                    self.report(
                        *key,
                        severity::ERROR,
                        "accumulator-type",
                        "A MapAccum key cannot be an accumulator".into(),
                    );
                }
                let heap_value = arguments.get(1).is_some_and(|value| {
                    value.kind() == "accumulator_type"
                        && syntax::field_text(*value, "kind", self.snapshot.text())
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
            // "Only ListAccum, ArrayAccum, MapAccum, and GroupByAccum can contain other accumulators."
            // (HeapAccum arguments are tuple types, never accumulator types.)
            "setaccum" | "bagaccum" => {
                for argument in arguments.iter().filter(|a| a.kind() == "accumulator_type") {
                    let name = syntax::field_text(node, "kind", self.snapshot.text()).unwrap_or("");
                    self.report(
                        *argument,
                        severity::ERROR,
                        "accumulator-type",
                        format!("A {name} cannot contain accumulators; only ListAccum, ArrayAccum, MapAccum and GroupByAccum can"),
                    );
                }
            }
            // "All accumulators, except HeapAccum, MapAccum, and GroupByAccum, can be used."
            // (querying/accumulators, ArrayAccum)
            "arrayaccum" => {
                for argument in arguments.iter().filter(|a| a.kind() == "accumulator_type") {
                    let element = syntax::field_text(*argument, "kind", self.snapshot.text()).unwrap_or("");
                    if ["heapaccum", "mapaccum", "groupbyaccum"].iter().any(|k| element.eq_ignore_ascii_case(k)) {
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
            "listaccum" if !context.in_list_accum && list_depth(node, self) > 3 => {
                self.report(
                    node,
                    severity::ERROR,
                    "accumulator-type",
                    "ListAccum can be nested at most three levels deep".into(),
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
            // "The data type of an AvgAccum variable is not declared" (also DeviationAccum).
            // Only a warning: whether a compiler rejects `AvgAccum<DOUBLE>` is not documented.
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
            // `OrAccum`, `AndAccum`; `BitwiseOrAccum<128>` takes a bit length.
            _ if count > 1 => Some(format!("{name} takes at most one type argument, not {count}")),
            _ => None,
        };
        if let Some(message) = problem {
            self.report(node, severity::ERROR, "accumulator-type", message);
            return;
        }
        for argument in arguments {
            if bitwise {
                // `BitwiseAndAccum<128>` or `BitwiseAndAccum<len>` (a parameter).
                // A warning: older references write only the bare `BitwiseOrAccum`.
                if !matches!(argument.kind(), "integer" | "identifier" | "type_identifier") {
                    let message = format!("{name} takes a bit length (a number or an INT parameter), not a type");
                    self.report(*argument, severity::WARNING, "accumulator-type", message);
                }
            } else if argument.kind() == "integer" {
                let message = if matches!(kind, "oraccum" | "andaccum") {
                    format!("{name} takes no bit length; only BitwiseOrAccum and BitwiseAndAccum do")
                } else {
                    format!(
                        "{name} takes a type, not a number; only BitwiseOrAccum and BitwiseAndAccum take a bit length"
                    )
                };
                self.report(*argument, severity::ERROR, "accumulator-type", message);
            } else if argument.kind() == "group_by_field" && kind != "groupbyaccum" {
                let message = format!("Only GroupByAccum names its type arguments; write `{name}<type>`");
                self.report(*argument, severity::ERROR, "accumulator-type", message);
            } else if kind == "sumaccum" && !self.sums(*argument) {
                // "operate on values of type INT, UINT, FLOAT, DOUBLE, or STRING only"
                let given = type_of_type_node(*argument, self.snapshot.text()).display();
                let message = format!("SumAccum takes INT, UINT, FLOAT, DOUBLE or STRING, not {given}");
                self.report(*argument, severity::ERROR, "accumulator-type", message);
            }
        }
    }

    /// Whether a SumAccum can hold values of the type `argument` (a TYPEDEF name is not judged).
    fn sums(&self, argument: Node) -> bool {
        match argument.kind() {
            "primitive_type" => matches!(
                type_of_type_node(argument, self.snapshot.text()),
                // `STRING COMPRESS` is listed too (and reported as deprecated).
                Ty::Primitive(p) if matches!(p.as_str(), "INT" | "UINT" | "FLOAT" | "DOUBLE") || p.starts_with("STRING")
            ),
            "vertex_type" | "edge_type" | "collection_type" | "accumulator_type" => false,
            _ => true,
        }
    }

    /// `AvgAccum<DOUBLE>`: the type argument is not written; a quick fix removes it.
    fn untyped_accumulator(&mut self, node: Node, name: &str) {
        let children = syntax::children(node);
        let (Some(open), Some(close)) =
            (children.iter().find(|c| c.kind() == "<"), children.iter().rev().find(|c| c.kind() == ">"))
        else {
            return;
        };
        let message =
            format!("{name} is declared without a type argument; it accepts INT, UINT, FLOAT and DOUBLE inputs");
        let mut d = diagnostic(self.snapshot, Span::of(node), severity::WARNING, "accumulator-type", message);
        let span = Span::new(open.start_byte(), close.end_byte());
        let edit = TextEdit { range: self.snapshot.range(span), new_text: String::new() };
        add_fix(&mut d, format!("Write `{name}` without a type argument"), vec![edit], true);
        self.out.push(d);
    }

    /// The fields of `GroupByAccum<key, ..., accumulator, ...>`: keys first, at least one
    /// of each, and distinct names (identifiers are case-sensitive). Whether the arguments
    /// are complete enough to be checked one by one.
    fn group_by_arguments(&mut self, node: Node, arguments: &[Node]) -> bool {
        let accumulators: Vec<bool> = arguments
            .iter()
            .map(|a| {
                let ty = a.child_by_field_name("type").unwrap_or(*a);
                ty.kind() == "accumulator_type"
                    || (ty.kind() == "type_identifier"
                        && self.accumulator_typedef(self.text(ty), ty.start_byte()).is_some())
            })
            .collect();
        let count = accumulators.iter().filter(|a| **a).count();
        if count == 0 || count == arguments.len() {
            let message = "GroupByAccum needs at least one key and one accumulator, e.g. `GroupByAccum<INT age, SumAccum<INT> total>`";
            self.report(node, severity::ERROR, "accumulator-type", message.into());
            return false;
        }
        if let Some(i) = accumulators.windows(2).position(|w| w[0] && !w[1]) {
            self.report(
                arguments[i + 1],
                severity::ERROR,
                "accumulator-type",
                "GroupByAccum keys come before its accumulators, e.g. `GroupByAccum<INT age, SumAccum<INT> total>`"
                    .into(),
            );
        }
        let mut seen = Vec::new();
        for field in arguments.iter().filter_map(|a| a.child_by_field_name("name")) {
            let name = self.text(field).to_string();
            if seen.contains(&name) {
                let message = format!("GroupByAccum already has a field named `{name}`");
                self.report(field, severity::ERROR, "accumulator-type", message);
            }
            seen.push(name);
        }
        true
    }

    /// The type a TYPEDEF'd accumulator name visible at `offset` stands for.
    fn accumulator_typedef(&self, name: &str, offset: usize) -> Option<Ty> {
        self.visible_type(SymbolKind::AccumulatorType, name, offset).map(|s| s.ty.clone())
    }

    /// The innermost TYPEDEF of `kind` named `name` visible at `offset` (without listing
    /// every visible symbol: this runs for each accumulator write).
    fn visible_type(&self, kind: SymbolKind, name: &str, offset: usize) -> Option<&'s Symbol> {
        let candidates: Vec<&'s Symbol> =
            self.type_symbols.iter().copied().filter(|s| s.kind == kind && s.name == name).collect();
        if candidates.is_empty() {
            return None;
        }
        let analysis = &self.snapshot.analysis;
        analysis
            .scope_chain(analysis.scope_at(offset))
            .find_map(|scope| candidates.iter().find(|s| s.scope == scope).copied())
    }

    /// `ty` with the TYPEDEF'd accumulator names among its arguments (which read as
    /// tuple names) replaced by their accumulator types.
    fn resolved(&self, ty: &Ty, offset: usize) -> Ty {
        match ty {
            Ty::Tuple(name) => self.accumulator_typedef(name, offset).unwrap_or_else(|| ty.clone()),
            Ty::Accumulator(kind, args) => {
                Ty::Accumulator(kind.clone(), args.iter().map(|a| self.resolved(a, offset)).collect())
            }
            _ => ty.clone(),
        }
    }

    /// Whether `name` is a tuple type visible at `offset` (or declared in the workspace).
    fn is_tuple_type(&self, name: &str, offset: usize) -> bool {
        self.visible_type(SymbolKind::TupleType, name, offset).is_some()
            || !self.snapshot.workspace.find_any(&[SymbolKind::TupleType], name).is_empty()
    }

    /// The value written to an accumulator with `+=` or `=` (also in ACCUM and POST-ACCUM,
    /// and the initial value of a declaration), judged against the accumulator's type.
    fn accumulator_input(&mut self, node: Node) {
        if node.has_error() {
            return;
        }
        let (ty, value) = if node.kind() == "accumulator_declarator" {
            // ArrayAccum elements are written through subscripts.
            if syntax::named_children(node).iter().any(|c| c.kind() == "array_dimension") {
                return;
            }
            let ty = node
                .parent()
                .and_then(|d| d.child_by_field_name("type"))
                // `TYPEDEF MapAccum<...> Counts; Counts @@c = ...;` resolves below.
                .filter(|t| matches!(t.kind(), "accumulator_type" | "type_identifier"))
                .map(|t| type_of_type_node(t, self.snapshot.text()));
            (ty, node.child_by_field_name("value"))
        } else {
            let ty = node
                .child_by_field_name("left")
                .filter(|left| left.kind() != "identifier")
                .and_then(|left| self.operand_type(left));
            (ty, node.child_by_field_name("right"))
        };
        if let (Some(ty), Some(value)) = (ty, value) {
            let ty = self.resolved(&ty, node.start_byte());
            let assigned = node.kind() == "accumulator_declarator"
                || node.child_by_field_name("operator").is_some_and(|o| o.kind() == "=");
            self.accumulator_value(value, &ty, assigned);
        }
    }

    /// `value` given to an accumulator of type `ty` (for a pair `(k -> v)`, `v` is
    /// accumulated into the accumulator stored for `k`); `assigned` for `=` and an
    /// initial value rather than `+=`.
    fn accumulator_value(&mut self, value: Node, ty: &Ty, assigned: bool) {
        let Ty::Accumulator(kind, args) = ty else {
            return;
        };
        match kind.as_str() {
            "MapAccum" => {
                if let [key, item] = args.as_slice() {
                    self.pair_input(value, ty, std::slice::from_ref(key), std::slice::from_ref(item));
                }
            }
            "GroupByAccum" => {
                let (values, keys): (Vec<Ty>, Vec<Ty>) =
                    args.iter().cloned().partition(|a| matches!(a, Ty::Accumulator(..)));
                if !keys.is_empty() && !values.is_empty() {
                    self.pair_input(value, ty, &keys, &values);
                }
            }
            _ => {
                let inner = unparenthesized(value);
                let what = capitalized(&with_article(&ty.display()));
                if matches!(inner.kind(), "key_value_pair" | "map_literal") {
                    let message = format!("{what} takes no `(key -> value)` pairs; only MapAccum and GroupByAccum do");
                    self.report(inner, severity::ERROR, "accumulator-input", message);
                    return;
                }
                let expected = accumulator_input_kind(ty);
                // `SumAccum<INT>`, `OrAccum`: one value at a time.
                let single = expected.is_some()
                    && !matches!(kind.as_str(), "SetAccum" | "BagAccum" | "ListAccum" | "ArrayAccum");
                if single && matches!(inner.kind(), "tuple" | "list_literal") {
                    let shape = if inner.kind() == "tuple" { "a tuple" } else { "a list" };
                    let message = format!("{what} takes a single value, not {shape}");
                    self.report(value, severity::ERROR, "accumulator-input", message);
                    return;
                }
                let given = match self.given(value) {
                    Some(Given::Kind(given)) if expected.is_some_and(|e| e != given) => given.described().to_string(),
                    Some(Given::Other(given)) if single => given,
                    _ => return,
                };
                let verb = if assigned { "stored in" } else { "added to" };
                let message = format!("{} cannot be {verb} {}", capitalized(&given), with_article(&ty.display()));
                self.report(value, severity::WARNING, "type-mismatch", message);
            }
        }
    }

    /// The input of a MapAccum (one key, one value) or a GroupByAccum (its keys and accumulators).
    fn pair_input(&mut self, value: Node, ty: &Ty, keys: &[Ty], values: &[Ty]) {
        let inner = unparenthesized(value);
        let what = capitalized(&with_article(&ty.display()));
        let map = matches!(ty, Ty::Accumulator(kind, _) if kind == "MapAccum");
        let shape = if map {
            "`(key -> value)`".to_string()
        } else {
            let names = |single: &str, prefix: &str, n: usize| match n {
                1 => single.to_string(),
                _ => (1..=n).map(|i| format!("{prefix}{i}")).collect::<Vec<_>>().join(", "),
            };
            format!("`({} -> {})`", names("key", "k", keys.len()), names("value", "v", values.len()))
        };
        let pairs = Pairs { ty, keys, values, what, shape };
        let (what, shape) = (&pairs.what, &pairs.shape);
        match inner.kind() {
            "key_value_pair" => {
                let arrow = syntax::children(inner).into_iter().find(|c| c.kind() == "->");
                let Some(arrow) = arrow else { return };
                let (before, after): (Vec<Node>, Vec<Node>) =
                    syntax::code_children(inner).into_iter().partition(|c| c.end_byte() <= arrow.start_byte());
                self.pair(inner, &before, &after, &pairs);
            }
            "map_literal" => {
                for entry in syntax::code_children(inner).into_iter().filter(|e| e.kind() == "map_entry") {
                    let (Some(key), Some(item)) =
                        (entry.child_by_field_name("key"), entry.child_by_field_name("value"))
                    else {
                        continue;
                    };
                    self.pair(entry, &[key], &[item], &pairs);
                }
            }
            "tuple" => {
                let elements = syntax::code_children(inner);
                let message =
                    format!("{what} takes {shape} pairs, not a tuple; separate the keys from the values with `->`");
                let mut d = diagnostic(self.snapshot, Span::of(inner), severity::ERROR, "accumulator-input", message);
                // `("a", 1)` -> `("a" -> 1)`
                if elements.len() == keys.len() + values.len() && elements.len() >= 2 {
                    let last_key = elements[keys.len() - 1];
                    let first_value = elements[keys.len()];
                    let comma = syntax::children(inner).into_iter().find(|c| {
                        c.kind() == ","
                            && c.start_byte() >= last_key.end_byte()
                            && c.end_byte() <= first_value.start_byte()
                    });
                    if let Some(comma) = comma {
                        let edit = TextEdit { range: self.snapshot.range(Span::of(comma)), new_text: " ->".into() };
                        add_fix(&mut d, "Replace `,` with `->`", vec![edit], false);
                    }
                }
                self.out.push(d);
            }
            // `(k - v)`, `(k > v)` or `(k >> v)`: the `->` lost or mistyped a character.
            "binary_expression"
                if inner.id() != value.id()
                    && keys.len() == 1
                    && values.len() == 1
                    && inner.child_by_field_name("operator").is_some_and(|o| matches!(o.kind(), "-" | ">" | ">>")) =>
            {
                let Some(operator) = inner.child_by_field_name("operator") else { return };
                self.arrow_typos.insert(inner.id());
                let message =
                    format!("{what} takes {shape} pairs; did you mean `->` instead of `{}`?", operator.kind());
                let mut d = diagnostic(self.snapshot, Span::of(value), severity::ERROR, "accumulator-input", message);
                let edit = TextEdit { range: self.snapshot.range(Span::of(operator)), new_text: "->".into() };
                add_fix(&mut d, format!("Replace `{}` with `->`", operator.kind()), vec![edit], false);
                self.out.push(d);
            }
            "list_literal" => {
                self.report(value, severity::ERROR, "accumulator-input", format!("{what} takes {shape} pairs"));
            }
            _ => {
                if let Some(source) = self.operand_type(inner)
                    && self.pair_source(value, inner, &source, &pairs)
                {
                    return;
                }
                // A literal, or a value of a known scalar kind (identifiers of unknown or
                // map type and calls are left alone).
                if self.value_kind(inner).is_some() {
                    // One error: no comparison or arithmetic warning on `p >= 1` or
                    // `(p >= 1)` besides it.
                    self.arrow_typos.insert(inner.id());
                    self.report(value, severity::ERROR, "accumulator-input", format!("{what} takes {shape} pairs"));
                }
            }
        }
    }

    /// A variable or accumulator of type `source` given to the MapAccum or GroupByAccum `ty`:
    /// another map is merged (its keys must fit), anything else that holds several
    /// values is wrong. Whether it was judged.
    fn pair_source(&mut self, value: Node, operand: Node, source: &Ty, pairs: &Pairs) -> bool {
        let &Pairs { ty, keys, ref what, ref shape, .. } = pairs;
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
            Ty::Collection(kind, args) if kind == "MAP" && target == "MapAccum" => args.first(),
            Ty::Accumulator(..) | Ty::Collection(..) | Ty::Vertex(_) | Ty::Edge(_) | Ty::VertexSet(_) => {
                let message = format!("{what} takes {shape} pairs, not {}", described_type(source));
                self.report(value, severity::ERROR, "accumulator-input", message);
                return true;
            }
            _ => return false,
        };
        if let (Some(given), Some(declared)) = (source_key, keys.first())
            && let (Some(a), Some(b)) = (value_kind_of(given), value_kind_of(declared))
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
    fn pair(&mut self, pair: Node, given_keys: &[Node], given_values: &[Node], pairs: &Pairs) {
        let &Pairs { ty, keys, values, ref shape, .. } = pairs;
        if given_keys.len() != keys.len() || given_values.len() != values.len() {
            let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
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
                Ty::Accumulator(..) => self.accumulator_value(*item, item_ty, false),
                _ => self.scalar_input(*item, item_ty, ty, "values"),
            }
        }
    }

    /// A key or a plain value of a map: a value of another kind than declared (a number
    /// where a string is declared, a vertex or a set where a string is, a number where a
    /// tuple is), and a pair where no accumulator takes one.
    fn scalar_input(&mut self, node: Node, declared: &Ty, ty: &Ty, role: &str) {
        let inner = unparenthesized(node);
        if matches!(inner.kind(), "key_value_pair" | "map_literal") {
            if matches!(declared, Ty::Primitive(_)) {
                let message =
                    format!("The {role} of {} are {}, not `(key -> value)` pairs", ty.display(), declared.display());
                self.report(inner, severity::ERROR, "accumulator-input", message);
            }
            return;
        }
        let Some(given) = self.given(node) else {
            return;
        };
        let wrong = match (declared, &given) {
            (Ty::Primitive(_), Given::Kind(given)) => value_kind_of(declared).is_some_and(|e| e != *given),
            (Ty::Primitive(_), Given::Other(_)) => value_kind_of(declared).is_some(),
            (Ty::Tuple(name), Given::Kind(_)) => self.is_tuple_type(name, node.start_byte()),
            _ => false,
        };
        if wrong {
            let what = match given {
                Given::Kind(kind) => kind.described().to_string(),
                Given::Other(what) => what,
            };
            let message = format!("The {role} of {} are {}, not {what}", ty.display(), declared.display());
            self.report(node, severity::WARNING, "type-mismatch", message);
        }
    }

    /// The declared type of a variable, parameter, alias or accumulator (`@@a`, `@a`, `t.@a`).
    fn operand_type(&self, node: Node) -> Option<Ty> {
        let symbol = match node.kind() {
            "identifier" | "global_accumulator" | "local_accumulator" => self.symbol_of(node),
            "member_expression" => node
                .child_by_field_name("property")
                .filter(|p| p.kind() == "local_accumulator")
                .and_then(|p| self.symbol_of(p)),
            _ => None,
        }?;
        Some(self.resolved(&symbol.ty, node.start_byte()))
    }

    /// What a value certainly is: a number, string or BOOL, or a vertex, edge, collection or
    /// accumulator that holds several values.
    fn given(&self, node: Node) -> Option<Given> {
        if let Some(kind) = self.value_kind(node) {
            return Some(Given::Kind(kind));
        }
        match self.operand_type(unparenthesized(node))? {
            ty @ (Ty::Vertex(_) | Ty::Edge(_) | Ty::VertexSet(_) | Ty::Collection(..)) => {
                Some(Given::Other(described_type(&ty)))
            }
            // Only accumulators that certainly hold several values (`MaxAccum<VERTEX>` reads
            // as one vertex, `MaxAccum<tuple>` as one tuple).
            ty @ Ty::Accumulator(..) if holds_several_values(&ty) => Some(Given::Other(described_type(&ty))),
            _ => None,
        }
    }

    /// The keys given to `get`, `containsKey` and `remove` of a MapAccum or GroupByAccum.
    fn accumulator_method(&mut self, node: Node) {
        let (Some(function), Some(arguments)) =
            (node.child_by_field_name("function"), node.child_by_field_name("arguments"))
        else {
            return;
        };
        let (Some(object), Some(method)) = (
            function.child_by_field_name("object").filter(|_| function.kind() == "member_expression"),
            function.child_by_field_name("property").filter(|p| p.kind() == "identifier"),
        ) else {
            return;
        };
        let method = self.text(method).to_string();
        if !["get", "containsKey", "remove"].iter().any(|m| m.eq_ignore_ascii_case(&method)) {
            return;
        }
        let Some(ty) = self.operand_type(object) else {
            return;
        };
        let Ty::Accumulator(kind, args) = &ty else {
            return;
        };
        let keys: Vec<Ty> = match kind.as_str() {
            // The count of a MapAccum's keys is checked with the other built-in methods.
            "MapAccum" => args.first().cloned().into_iter().collect(),
            "GroupByAccum" => args.iter().filter(|a| !matches!(a, Ty::Accumulator(..))).cloned().collect(),
            _ => return,
        };
        let given = syntax::named_children(arguments).into_iter().filter(|c| c.kind() != "comment").collect::<Vec<_>>();
        if kind == "GroupByAccum" && given.len() != keys.len() && !keys.is_empty() {
            let plural = if keys.len() == 1 { "" } else { "s" };
            let message = format!(
                "`{method}` expects {} argument{plural}, the keys of {}, but {} given",
                keys.len(),
                ty.display(),
                given.len()
            );
            self.report(arguments, severity::WARNING, "argument-count", message);
            return;
        }
        for (argument, key) in given.iter().zip(&keys) {
            if let (Some(expected), Some(Given::Kind(found))) = (value_kind_of(key), self.given(*argument))
                && expected != found
            {
                let message = format!("The keys of {} are {}, not {}", ty.display(), key.display(), found.described());
                self.report(*argument, severity::WARNING, "type-mismatch", message);
            }
        }
    }

    /// `FOREACH (k, v) IN @@map`: a MapAccum or MAP binds a key and a value; a set, bag or
    /// list of plain values one element.
    fn foreach_variables(&mut self, node: Node) {
        let (Some(variables), Some(collection)) =
            (node.child_by_field_name("variable"), node.child_by_field_name("collection"))
        else {
            return;
        };
        let count = if variables.kind() == "foreach_variables" {
            syntax::named_children(variables).into_iter().filter(|c| c.kind() == "identifier").count()
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
                    if count == 1 { "" } else { "s" }
                )
            }
            Ty::Accumulator(kind, args) | Ty::Collection(kind, args)
                if matches!(kind.as_str(), "SetAccum" | "BagAccum" | "ListAccum" | "SET" | "BAG" | "LIST")
                    && count > 1
                    && args
                        .first()
                        .is_some_and(|e| value_kind_of(e).is_some() || matches!(e, Ty::Vertex(_) | Ty::Edge(_))) =>
            {
                format!(
                    "{} yields one element at a time; write `FOREACH x IN ...`, not {count} variables",
                    capitalized(&with_article(&ty.display()))
                )
            }
            Ty::VertexSet(_) if count > 1 => {
                format!("A vertex set yields one vertex at a time; write `FOREACH x IN ...`, not {count} variables")
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
                format!("Accumulator type names are case-sensitive: write `{}`", accumulator.name),
            );
            d.data = Some(json!({ "replacement": accumulator.name }));
            self.out.push(d);
        }
    }

    /// openCypher patterns need `SYNTAX V3`: a query that declares V1 or V2 is
    /// rejected. Queries without a declaration are not judged: the 4.2 and 4.3
    /// reference writes patterns such as `FROM (v:Post)` in default-syntax
    /// queries. Reported once per query.
    fn cypher_syntax(&mut self, node: Node, context: &Context) {
        let Some(mode) = context.query.filter(|m| m.old_syntax && !m.v3 && !m.interpreted) else {
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

    /// The query compiler warns about exact comparison of floating-point values.
    fn float_equality(&mut self, node: Node, mode: Mode) {
        let Some(operator) = node.child_by_field_name("operator") else {
            return;
        };
        let equality = operator.kind() == "==" || (operator.kind() == "=" && mode.v3);
        if !equality {
            return;
        }
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
            return;
        };
        // Whole numbers are exact in floating point: `x == 0`, and the
        // `x == float_to_int(x)` test for a whole value, are reliable.
        let exact = self.is_whole_literal(left)
            || self.is_whole_literal(right)
            || self.is_rounding_of(right, left)
            || self.is_rounding_of(left, right);
        if !exact && [left, right].into_iter().any(|operand| self.is_float(operand)) {
            self.report(
                operator,
                severity::WARNING,
                "float-equality",
                "Comparing FLOAT or DOUBLE values for exact equality is unreliable; compare with a tolerance, e.g. `abs(a - b) < 0.0001`"
                    .into(),
            );
        }
    }

    /// Whether an expression has a FLOAT or DOUBLE value (iterative: operator
    /// chains can be tens of thousands of terms long).
    /// `0`, `-1`, `2.0`: a literal without a fractional part.
    fn is_whole_literal(&self, node: Node) -> bool {
        match node.kind() {
            "integer" => true,
            "float" => self.text(node).parse::<f64>().is_ok_and(|v| v.fract() == 0.0 && v.abs() < 9e15),
            "parenthesized_expression" => {
                syntax::first_code_child(node).is_some_and(|inner| self.is_whole_literal(inner))
            }
            "unary_expression" => {
                node.child_by_field_name("operator").is_some_and(|o| o.kind() == "-")
                    && node.child_by_field_name("operand").is_some_and(|inner| self.is_whole_literal(inner))
            }
            _ => false,
        }
    }

    /// Whether `call` is `float_to_int(x)`, `floor(x)`, `ceil(x)`, `round(x)` or
    /// `trunc(x)` of the same expression as `operand`.
    fn is_rounding_of(&self, call: Node, operand: Node) -> bool {
        let mut call = call;
        while call.kind() == "parenthesized_expression" {
            let Some(inner) = syntax::first_code_child(call) else { return false };
            call = inner;
        }
        if call.kind() != "call_expression" {
            return false;
        }
        let name = call.child_by_field_name("function").map(|f| self.text(f).to_ascii_lowercase());
        if !matches!(name.as_deref(), Some("float_to_int" | "floor" | "ceil" | "round" | "trunc")) {
            return false;
        }
        // The tokens without whitespace and comments.
        let squash = |node: Node| -> String {
            let mut out = String::new();
            syntax::walk(node, |n| {
                if n.child_count() == 0 && n.kind() != "comment" {
                    out.extend(self.text(n).split_whitespace());
                }
            });
            out
        };
        let arguments = call.child_by_field_name("arguments").map(syntax::code_children).unwrap_or_default();
        arguments.first().is_some_and(|first| squash(*first) == squash(operand))
    }

    fn is_float(&self, node: Node) -> bool {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            let float = match node.kind() {
                "float" => true,
                "parenthesized_expression" => {
                    pending.extend(syntax::first_code_child(node));
                    false
                }
                "unary_expression" => {
                    pending.extend(node.child_by_field_name("operand"));
                    false
                }
                "binary_expression" => {
                    let arithmetic =
                        node.child_by_field_name("operator").is_some_and(|o| matches!(o.kind(), "+" | "-" | "*" | "/"));
                    if arithmetic {
                        pending.extend(node.child_by_field_name("left"));
                        pending.extend(node.child_by_field_name("right"));
                    }
                    false
                }
                "identifier" | "global_accumulator" | "local_accumulator" => {
                    self.symbol_of(node).is_some_and(|s| is_float_type(&s.ty))
                }
                "member_expression" => node.child_by_field_name("property").is_some_and(|p| self.is_float_member(p)),
                "call_expression" => node
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "identifier")
                    .and_then(|f| builtins::function(self.text(f)))
                    .is_some_and(|f| matches!(f.returns, "FLOAT" | "DOUBLE")),
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
        self.member_types(property).is_some_and(|types| !types.is_empty() && types.iter().all(is_float_type))
    }

    /// The declared types of a member: the accumulator or local variable, or
    /// the attribute in every type that declares it. `None` when unknown.
    fn member_types(&self, property: Node) -> Option<Vec<Ty>> {
        if property.kind() == "local_accumulator" {
            return self.symbol_of(property).map(|s| vec![s.ty.clone()]);
        }
        let reference = self.snapshot.analysis.reference_at(property.start_byte())?;
        match resolve::target(self.snapshot, reference)? {
            Target::Local(id) => Some(vec![self.snapshot.analysis.symbols[id].ty.clone()]),
            Target::Global(key) if key.kind == SymbolKind::Attribute => {
                Some(resolve::declarations(self.snapshot, &key).iter().map(|d| d.ty.clone()).collect())
            }
            _ => None,
        }
    }

    /// The query `name(...)` calls, when exactly one workspace query has that
    /// name and the call is in well-formed code.
    fn user_query(&self, call: Node) -> Option<&'s GlobalSymbol> {
        let function = call.child_by_field_name("function").filter(|f| f.kind() == "identifier")?;
        // Only a file with a syntax error can have a call inside code the parser gave
        // up on. (`Node::parent` costs the depth of the node, so the walk is capped
        // and skipped for clean files: it is quadratic on a long chain of calls.)
        if self.snapshot.root().has_error() {
            let mut ancestors = syntax::self_and_ancestors(call);
            if ancestors.by_ref().take(MAX_CALL_DEPTH).any(|a| a.is_error()) || ancestors.next().is_some() {
                return None;
            }
        }
        let name = self.text(function);
        let snapshot: &'s Snapshot = self.snapshot;
        let queries = snapshot.workspace.find(SymbolKind::Query, name);
        let [query] = queries.as_slice() else {
            return None;
        };
        let shadowed = !snapshot.workspace.find(SymbolKind::TupleType, name).is_empty();
        (!query.in_error && !shadowed).then_some(*query)
    }

    /// A subquery call whose value is used where the query returns none, and
    /// arguments of certain kind that the parameter cannot take.
    fn subquery_call(&mut self, node: Node) {
        let Some(query) = self.user_query(node) else {
            return;
        };
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let used_as_value = node.parent().is_some_and(|p| match p.kind() {
            "variable_declarator" => p.child_by_field_name("value") == Some(node),
            "assignment_statement" => p.child_by_field_name("right") == Some(node),
            _ => false,
        });
        if query.returns.is_none() && used_as_value {
            let message = format!("Query `{}` has no RETURNS clause, so it returns no value", self.text(function));
            self.report(node, severity::WARNING, "type-mismatch", message);
        }
        let Some(arguments) = node.child_by_field_name("arguments").filter(|a| !a.has_error()) else {
            return;
        };
        let values = syntax::named_children(arguments).into_iter().filter(|c| c.kind() != "comment");
        for (value, param) in values.zip(&query.params) {
            // Literals are judged by the `argument-type` check of the query call.
            let literal = match value.kind() {
                "integer" | "float" | "string" | "boolean" | "list_literal" => true,
                "unary_expression" => {
                    value.child_by_field_name("operand").is_some_and(|o| matches!(o.kind(), "integer" | "float"))
                }
                _ => false,
            };
            let Some(given) = self.value_kind(value).filter(|_| !literal) else {
                continue;
            };
            let ty: String = param.ty.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
            let accepted = match ty.as_str() {
                "BOOL" => &[ValueKind::Bool][..],
                "INT" | "UINT" | "FLOAT" | "DOUBLE" => &[ValueKind::Number],
                "STRING" | "DATETIME" => &[ValueKind::Number, ValueKind::Text],
                _ => continue,
            };
            if !accepted.contains(&given) {
                let what = match given {
                    ValueKind::Number => "a number",
                    ValueKind::Text => "a string",
                    ValueKind::Bool => "a boolean",
                };
                let message =
                    format!("Query `{}` expects {} for `{}`, not {what}", self.text(function), param.ty, param.name);
                self.report(value, severity::WARNING, "argument-type", message);
            }
        }
    }

    /// What kind of value an expression certainly has, when that is known.
    fn value_kind(&self, node: Node) -> Option<ValueKind> {
        match node.kind() {
            "integer" | "float" => Some(ValueKind::Number),
            "string" => Some(ValueKind::Text),
            "boolean" => Some(ValueKind::Bool),
            "parenthesized_expression" => syntax::first_code_child(node).and_then(|inner| self.value_kind(inner)),
            "unary_expression" => {
                let operand = node.child_by_field_name("operand").and_then(|o| self.value_kind(o));
                match node.child_by_field_name("operator").map(|o| o.kind()) {
                    Some("-") => operand.filter(|k| *k == ValueKind::Number),
                    Some("~") => Some(ValueKind::Number),
                    Some("NOT") => Some(ValueKind::Bool),
                    _ => None,
                }
            }
            "binary_expression" => {
                let operator = node.child_by_field_name("operator").map(|o| o.kind())?;
                let side = |field: &str| node.child_by_field_name(field).and_then(|n| self.value_kind(n));
                match operator {
                    "-" | "*" | "/" | "%" | "<<" | ">>" | "&" | "|" | "^" => Some(ValueKind::Number),
                    "+" => match (side("left"), side("right")) {
                        (Some(ValueKind::Number), Some(ValueKind::Number)) => Some(ValueKind::Number),
                        (Some(ValueKind::Text), _) | (_, Some(ValueKind::Text)) => Some(ValueKind::Text),
                        _ => None,
                    },
                    "==" | "!=" | "<>" | "=" | "<" | "<=" | ">" | ">=" | "AND" | "OR" => Some(ValueKind::Bool),
                    _ => None,
                }
            }
            "in_expression" | "like_expression" | "is_expression" | "between_expression" => Some(ValueKind::Bool),
            "identifier" | "global_accumulator" | "local_accumulator" => {
                self.symbol_of(node).and_then(|s| value_kind_of(&s.ty))
            }
            // A subquery returning a plain number, string or BOOL.
            "call_expression" => {
                let query = self.user_query(node)?;
                match &query.ty {
                    Ty::Primitive(_) if query.returns.is_some() => value_kind_of(&query.ty),
                    _ => None,
                }
            }
            "member_expression" => {
                let types = self.member_types(node.child_by_field_name("property")?)?;
                let kinds: Vec<Option<ValueKind>> = types.iter().map(value_kind_of).collect();
                let first = (*kinds.first()?)?;
                kinds.iter().all(|k| *k == Some(first)).then_some(first)
            }
            _ => None,
        }
    }

    /// Mismatches that cannot be right: a string added to a numeric
    /// accumulator or stored in a numeric or BOOL variable, and arithmetic on
    /// a string. Only values of certain kind are judged.
    fn type_checks(&mut self, node: Node, context: &Context) {
        match node.kind() {
            "assignment_statement" => {
                let (Some(left), Some(right), Some(operator)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                    node.child_by_field_name("operator"),
                ) else {
                    return;
                };
                if self.value_kind(right) != Some(ValueKind::Text) {
                    return;
                }
                if operator.kind() == "="
                    && left.kind() == "identifier"
                    && let Some(symbol) = self.symbol_of(left).filter(|s| {
                        matches!(s.kind, SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::LoopVariable)
                    })
                    && let Some(what) = stores_string_badly(&symbol.ty)
                {
                    let message = format!("A string cannot be stored in {what} variable ({})", symbol.ty.display());
                    self.report(right, severity::WARNING, "type-mismatch", message);
                }
                // `+=` into an accumulator: see `accumulator_value`.
            }
            "variable_declaration" => {
                let Some(declared) =
                    node.child_by_field_name("type").map(|t| type_of_type_node(t, self.snapshot.text()))
                else {
                    return;
                };
                let Some(what) = stores_string_badly(&declared) else {
                    return;
                };
                for declarator in syntax::named_children(node).into_iter().filter(|c| c.kind() == "variable_declarator")
                {
                    if let Some(value) = declarator.child_by_field_name("value")
                        && self.value_kind(value) == Some(ValueKind::Text)
                    {
                        let message = format!("A string cannot be stored in {what} variable ({})", declared.display());
                        self.report(value, severity::WARNING, "type-mismatch", message);
                    }
                }
            }
            "binary_expression" => {
                let Some(operator) = node.child_by_field_name("operator") else {
                    return;
                };
                // `(k - v)` meant as `(k -> v)` is reported as that.
                if self.arrow_typos.contains(&node.id()) {
                    return;
                }
                // `=` compares only in SYNTAX V3 queries (elsewhere it is an assignment).
                let v3_equals = operator.kind() == "="
                    && context.query.is_some_and(|mode| mode.v3)
                    && !syntax::self_and_ancestors(node).any(|a| a.is_error());
                if v3_equals || matches!(operator.kind(), "==" | "!=" | "<>" | "<" | "<=" | ">" | ">=") {
                    let side = |field: &str| node.child_by_field_name(field);
                    if let (Some(left), Some(right)) = (side("left"), side("right"))
                        && let (Some(a), Some(b)) = (self.value_kind(left), self.value_kind(right))
                        && matches!((a, b), (ValueKind::Number, ValueKind::Text) | (ValueKind::Text, ValueKind::Number))
                    {
                        let text = if a == ValueKind::Text { left } else { right };
                        let message = format!("`{}` compares a number with a string", operator.kind());
                        self.report(text, severity::WARNING, "type-mismatch", message);
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
                        let message = format!("Arithmetic with `{}` needs numbers, not a string", operator.kind());
                        self.report(operand, severity::WARNING, "type-mismatch", message);
                    }
                }
            }
            // `-"a"`
            "unary_expression" => {
                if node.child_by_field_name("operator").is_some_and(|o| o.kind() == "-")
                    && let Some(operand) = node.child_by_field_name("operand")
                    && self.value_kind(operand) == Some(ValueKind::Text)
                {
                    let message = "Arithmetic with `-` needs numbers, not a string".to_string();
                    self.report(operand, severity::WARNING, "type-mismatch", message);
                }
            }
            // `(p:Person {age: "abc"})` means `p.age == "abc"`.
            "pair" if node.parent().is_some_and(|p| p.kind() == "property_map") => {
                let (Some(key), Some(value)) = (node.child_by_field_name("key"), node.child_by_field_name("value"))
                else {
                    return;
                };
                let (Some(given), true) = (self.value_kind(value), key.kind() == "identifier") else {
                    return;
                };
                let declared = self.member_types(key).unwrap_or_default();
                let declared = declared
                    .first()
                    .and_then(value_kind_of)
                    .filter(|first| declared.iter().all(|t| value_kind_of(t) == Some(*first)));
                if let Some(declared) = declared
                    && matches!(
                        (given, declared),
                        (ValueKind::Number, ValueKind::Text) | (ValueKind::Text, ValueKind::Number)
                    )
                {
                    let (what, other) =
                        if given == ValueKind::Text { ("a number", "a string") } else { ("a string", "a number") };
                    let message = format!("`{}` is {what}, but is compared with {other}", self.text(key));
                    self.report(value, severity::WARNING, "type-mismatch", message);
                }
            }
            _ => {}
        }
    }

    /// `RETURN value` needs a RETURNS clause, and a string cannot be returned
    /// as a number or BOOL.
    fn return_value(&mut self, node: Node, context: &Context) {
        let Some(value) = node.child_by_field_name("value") else {
            return;
        };
        match context.returns {
            Returns::Unknown => {}
            Returns::Missing => self.report(
                value,
                severity::WARNING,
                "type-mismatch",
                "RETURN has a value, but the query has no RETURNS clause".into(),
            ),
            Returns::Declared(Some(what)) if self.value_kind(value) == Some(ValueKind::Text) => {
                let message = format!("A string cannot be returned by a query that returns {what}");
                self.report(value, severity::WARNING, "type-mismatch", message);
            }
            Returns::Declared(_) => {}
        }
    }

    /// Accumulator methods that modify the accumulator are restricted by clause.
    fn mutator(&mut self, node: Node, context: &Context) {
        let Some(function) = node.child_by_field_name("function").filter(|f| f.kind() == "member_expression") else {
            return;
        };
        let (Some(object), Some(method)) =
            (function.child_by_field_name("object"), function.child_by_field_name("property"))
        else {
            return;
        };
        // `@@acc.method()` or `v.@acc.method()`
        let accumulator = match object.kind() {
            "global_accumulator" => object,
            "member_expression" => match object.child_by_field_name("property") {
                Some(property) if property.kind() == "local_accumulator" => property,
                _ => return,
            },
            _ => return,
        };
        let Some(Ty::Accumulator(kind, _)) = self.symbol_of(accumulator).map(|s| &s.ty) else {
            return;
        };
        let is_mutator = builtins::accumulator(kind)
            .and_then(|a| builtins::find_method(a.methods, self.text(method)))
            .is_some_and(|m| m.mutator);
        if !is_mutator {
            return;
        }
        let call = format!(".{}()", self.text(method));
        let problem = if context.print {
            Some(format!("The accumulator mutator `{call}` cannot be used in PRINT"))
        } else if accumulator.kind() == "global_accumulator" && (context.accum || context.post_accum) {
            Some(format!(
                "Mutators of global accumulators such as `{call}` can only be called at the query-body level, not inside ACCUM or POST-ACCUM"
            ))
        } else if accumulator.kind() == "local_accumulator" && !context.post_accum {
            Some(format!("Mutators of vertex-attached accumulators such as `{call}` can only be called in POST-ACCUM"))
        } else {
            None
        };
        if let Some(message) = problem {
            self.report(method, severity::ERROR, "accumulator-mutator", message);
        }
    }

    /// Features the reference marks as deprecated.
    fn deprecated(&mut self, node: Node) {
        let message = match node.kind() {
            "COMPRESS" => "`STRING COMPRESS` is deprecated since TigerGraph 3.0 and cannot be used in new schemas",
            "tag_statement" | "tags_clause" => "Tag-based access control is deprecated",
            "TAGS" if node.parent().is_some_and(|p| p.kind() == "print_statement") => {
                "`PRINT ... WITH TAGS` belongs to tag-based graphs, which are deprecated"
            }
            _ => return,
        };
        let span_node = if node.kind() == "COMPRESS" { node } else { node.child(0).unwrap_or(node) };
        let mut d = diagnostic(self.snapshot, Span::of(span_node), severity::HINT, "deprecated", message.into());
        d.tags = vec![diagnostic_tag::DEPRECATED];
        self.out.push(d);
    }

    fn reserved_tags(&mut self, node: Node) {
        let field = if node.kind() == "tag_statement" { "name" } else { "tag" };
        for tag in syntax::children_by_field(node, field) {
            let name = self.text(tag).to_string();
            if is_ddl_reserved(&name) {
                let message = format!("`{name}` is a reserved word in GSQL and cannot name a tag");
                self.report(tag, severity::ERROR, "reserved-word", message);
            }
        }
    }

    /// Features the GSQL interpreter does not support (`INTERPRET QUERY`).
    fn interpreted(&mut self, node: Node, context: &Context) {
        let what: Option<String> = match node.kind() {
            "file_declaration" => Some("FILE objects".into()),
            "parameter" => node.child_by_field_name("type").and_then(|t| match t.kind() {
                "file_type" => Some("FILE parameters".to_string()),
                "collection_type" => {
                    let bag = t.child_by_field_name("kind").is_some_and(|k| k.kind() == "BAG");
                    bag.then(|| "BAG parameters".to_string())
                }
                _ => None,
            }),
            "print_statement" => {
                syntax::children(node).iter().any(|c| c.kind() == "TO_CSV").then(|| "PRINT ... TO_CSV".to_string())
            }
            "raise_statement" => Some("RAISE".into()),
            "try_statement" => Some("TRY ... EXCEPTION".into()),
            "return_statement" => Some("RETURN".into()),
            "accumulator_kind" if self.text(node).eq_ignore_ascii_case("arrayaccum") => Some("ArrayAccum".into()),
            "member_expression" if node.child_by_field_name("prime").is_some() => {
                Some("the previous-value operator `'`".into())
            }
            "is_expression" if node.child_by_field_name("right").is_some_and(|r| r.kind() == "null") => {
                Some("IS NULL".into())
            }
            "primitive_type" => {
                let text = self.text(node).to_ascii_uppercase();
                matches!(text.as_str(), "JSONOBJECT" | "JSONARRAY").then_some(text)
            }
            "typed_value" if context.insert => Some("vertex types in INSERT values".into()),
            "call_expression" => node.child_by_field_name("function").and_then(|f| match f.kind() {
                "identifier" => {
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
                    unsupported.contains(&name.as_str()).then(|| format!("`{}()`", self.text(f)))
                }
                "member_expression" => f.child_by_field_name("property").and_then(|p| {
                    let name = self.text(p);
                    ["neighbors", "neighborAttribute"]
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case(name))
                        .then(|| format!("`.{name}()`"))
                }),
                _ => None,
            }),
            "path_pattern" => {
                syntax::first_code_child(node).filter(|v| v.kind() == "vertex_pattern").and_then(|source| {
                    let any =
                        source.child_by_field_name("type").is_some_and(|t| matches!(t.kind(), "wildcard" | "any"));
                    any.then(|| "`_` or `ANY` as the source vertex type".to_string())
                })
            }
            "interpret_query_statement" => {
                self.shared_names(node);
                None
            }
            _ => None,
        };
        if let Some(what) = what {
            let message = format!("Interpreted queries do not support {what}; create and install the query instead");
            let span_node = match node.kind() {
                "print_statement" | "try_statement" | "raise_statement" | "return_statement" | "file_declaration" => {
                    node.child(0).unwrap_or(node)
                }
                _ => node,
            };
            self.report(span_node, severity::WARNING, "interpreted-mode", message);
        }
    }

    /// In interpreted queries a parameter and a global accumulator cannot share a name.
    fn shared_names(&mut self, query: Node) {
        let Some(body) = query.child_by_field_name("body") else {
            return;
        };
        let analysis = self.snapshot.analysis;
        let scope = analysis.scope_at(body.start_byte());
        let Some(query_scope) = analysis.query_scope(scope) else {
            return;
        };
        let symbols: Vec<&Symbol> =
            analysis.scopes[query_scope].symbols.iter().map(|&id| &analysis.symbols[id]).collect();
        for accumulator in symbols.iter().filter(|s| s.kind == SymbolKind::GlobalAccumulator) {
            let bare = accumulator.name.trim_start_matches('@');
            if symbols.iter().any(|s| matches!(s.kind, SymbolKind::Parameter | SymbolKind::Variable) && s.name == bare)
            {
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

    /// Features unsupported in distributed queries, and calls between
    /// distributed and other queries.
    fn distributed(&mut self, node: Node, mode: Mode, context: &Context) {
        if node.kind() != "call_expression" {
            return;
        }
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let message = match function.kind() {
            "identifier" => {
                let name = self.text(function).to_string();
                if mode.distributed && name.eq_ignore_ascii_case("evaluate") {
                    Some("Distributed queries do not support `evaluate()`".to_string())
                } else if self.is_distributed_query(&name) {
                    if mode.distributed {
                        Some(format!("A distributed query cannot call the distributed query `{name}`"))
                    } else if context.accum || context.post_accum {
                        Some(format!("The distributed query `{name}` cannot be called inside ACCUM or POST-ACCUM"))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            "member_expression" if mode.distributed => function.child_by_field_name("property").and_then(|p| {
                let name = self.text(p);
                ["neighbors", "neighborAttribute", "edgeAttribute"]
                    .iter()
                    .any(|m| m.eq_ignore_ascii_case(name))
                    .then(|| format!("Distributed queries do not support `.{name}()`"))
            }),
            _ => None,
        };
        if let Some(message) = message {
            self.report(function, severity::WARNING, "distributed-mode", message);
        }
    }

    fn is_distributed_query(&self, name: &str) -> bool {
        self.snapshot.workspace.find(SymbolKind::Query, name).iter().any(|q| {
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
        if matches!(n.kind(), "vertex_pattern" | "node_pattern") {
            found.push(n);
        }
    });
    found
}

/// The expression inside any parentheses around `node`.
fn unparenthesized(node: Node) -> Node {
    let mut node = node;
    while node.kind() == "parenthesized_expression" {
        match syntax::first_code_child(node) {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// The accumulator a write or read is about: the object of a subscript (`@@m[k]`, `t.@m[k]`).
fn accumulator_target(node: Node) -> Node {
    match node.kind() {
        "subscript_expression" => node.child_by_field_name("object").map(accumulator_target).unwrap_or(node),
        _ => node,
    }
}

/// The identifiers that are the object of a member access (`t.name`, `t.@acc`) below `root`.
fn member_objects(root: Node) -> Vec<Node> {
    let mut found = Vec::new();
    syntax::walk(root, |n| {
        if n.kind() == "member_expression" {
            found.extend(n.child_by_field_name("object").filter(|o| o.kind() == "identifier"));
        }
    });
    found
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// `vertex type` -> `a vertex type`, `attribute` -> `an attribute`, `AvgAccum` -> `an AvgAccum`
/// (but `a UINT`).
fn with_article(label: &str) -> String {
    let article = if label.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O']) { "an" } else { "a" };
    format!("{article} {label}")
}

/// How far up the tree a call is checked for being inside code the parser gave up on.
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
    /// A vertex, an edge, a collection or an accumulator of several values, described.
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

/// The kind of value an accumulator that is not a map accumulates, when certain.
fn accumulator_input_kind(ty: &Ty) -> Option<ValueKind> {
    let Ty::Accumulator(kind, args) = ty else {
        return None;
    };
    match kind.as_str() {
        "AvgAccum" | "DeviationAccum" | "DeviationPAccum" | "BitwiseOrAccum" | "BitwiseAndAccum" => {
            Some(ValueKind::Number)
        }
        "OrAccum" | "AndAccum" => Some(ValueKind::Bool),
        "SumAccum" | "MaxAccum" | "MinAccum" | "SetAccum" | "BagAccum" | "ListAccum" => {
            args.first().and_then(value_kind_of)
        }
        _ => None,
    }
}

/// An accumulator that certainly holds several values (`MaxAccum<VERTEX>` reads as one
/// vertex, `SumAccum<INT>` or `BitwiseOrAccum` as one number).
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
        // Reading a Deviation accumulator gives a DOUBLE, a bitwise one an INT.
        Ty::Accumulator(kind, args) => match kind.as_str() {
            "AvgAccum" | "DeviationAccum" | "DeviationPAccum" | "BitwiseOrAccum" | "BitwiseAndAccum" => {
                Some(ValueKind::Number)
            }
            "OrAccum" | "AndAccum" => Some(ValueKind::Bool),
            "SumAccum" | "MaxAccum" | "MinAccum" => args.first().and_then(value_kind_of),
            _ => None,
        },
        _ => None,
    }
}

/// How a variable of this type reads in "a string cannot be stored in ...",
/// when it is numeric or BOOL.
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
            "SumAccum" | "MaxAccum" | "MinAccum" => args.first().is_some_and(is_float_type),
            _ => false,
        },
        _ => false,
    }
}

fn is_list_accum(node: Node, checker: &Checker) -> bool {
    syntax::field_text(node, "kind", checker.snapshot.text()).is_some_and(|k| k.eq_ignore_ascii_case("listaccum"))
}

/// How many ListAccums are nested starting at `node` (a ListAccum has one
/// type argument).
fn list_depth(node: Node, checker: &Checker) -> usize {
    let mut depth = 0;
    let mut current = Some(node);
    while let Some(list) = current.filter(|n| n.kind() == "accumulator_type" && is_list_accum(*n, checker)) {
        depth += 1;
        current = list.child_by_field_name("argument");
    }
    depth
}

#[cfg(test)]
mod tests {
    use crate::features::code_actions::code_actions;
    use crate::features::diagnostics::diagnostics;
    use crate::features::test_support::Fixture;
    use crate::lsp::types::{Position, Range};

    /// (code, message) of every diagnostic.
    fn findings(text: &str, others: &[(&str, &str)]) -> Vec<(String, String)> {
        let fixture = Fixture::with_files(text, others);
        diagnostics(&fixture.snapshot()).into_iter().filter_map(|d| Some((d.code?, d.message))).collect()
    }

    fn with_code<'a>(found: &'a [(String, String)], code: &str) -> Vec<&'a str> {
        found.iter().filter(|(c, _)| c == code).map(|(_, m)| m.as_str()).collect()
    }

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
            assert!(reserved.contains(&expected), "{expected:?} not in {reserved:?}");
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
        assert_eq!(with_code(&found, "limit-offset"), ["OFFSET needs an ORDER BY clause"]);
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
        assert_eq!(with_code(&found, "accumulator-assignment").len(), 1, "{found:?}");
        assert_eq!(
            with_code(&found, "edge-accumulator"),
            ["Edge accumulators such as `@w` cannot be used in POST-ACCUM"]
        );
        let mutators = with_code(&found, "accumulator-mutator");
        assert_eq!(mutators.len(), 3, "{mutators:?}");
        assert!(mutators.iter().any(|m| m.contains("global accumulators") && m.contains("query-body level")));
        assert!(mutators.iter().any(|m| m.contains("vertex-attached accumulators") && m.contains("POST-ACCUM")));
        assert!(mutators.iter().any(|m| m.contains("cannot be used in PRINT")));
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
        assert!(with_code(&findings(legal, &[]), "accumulator-type").is_empty());
    }

    #[test]
    fn array_accum_elements_exclude_heap_map_and_groupby() {
        let illegal = "CREATE QUERY q() {\n  ArrayAccum<MapAccum<INT, INT>> @@b[2];\n  ArrayAccum<GroupByAccum<INT a, SumAccum<INT> s>> @@c[2];\n  ArrayAccum<HeapAccum<Rec>(3, v DESC)> @@a[2];\n  PRINT @@a, @@b, @@c;\n}\n";
        let found = findings(illegal, &[]);
        assert_eq!(
            with_code(&found, "accumulator-type"),
            ["An ArrayAccum cannot contain a HeapAccum, MapAccum or GroupByAccum"; 3]
        );
        let legal = "CREATE QUERY q() {\n  ArrayAccum<SumAccum<INT>> @@a[2];\n  ArrayAccum<ListAccum<INT>> @@b[2];\n  ArrayAccum<SetAccum<STRING>> @@names[10];\n  ArrayAccum<SetAccum<INT>> @@ids[][];\n  ArrayAccum<ArrayAccum<SumAccum<INT>>> @@d[2][2];\n  PRINT @@a, @@b, @@names, @@ids, @@d;\n}\n";
        assert!(with_code(&findings(legal, &[]), "accumulator-type").is_empty());
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
        let actions = code_actions(&snapshot, line, &all, None, &all);
        assert!(actions.iter().any(|a| a.title == "Replace with `SumAccum`"), "{actions:?}");
    }

    #[test]
    fn warns_about_exact_float_comparison() {
        let schema = "CREATE VERTEX Account (PRIMARY_ID id STRING, balance DOUBLE, limit_ DOUBLE, n INT)\n";
        let query = "CREATE QUERY q(FLOAT x, FLOAT y) {\n  AvgAccum @@avg;\n  S = {Account.*};\n  R = SELECT a FROM S:a WHERE a.balance == a.limit_ OR a.n == 1;\n  IF x == y OR @@avg == 0.5 OR x + 1 > 2 OR 1 == 1 OR sqrt(2) == x THEN PRINT R; END;\n}\n";
        let found = findings(query, &[("file:///test/schema.gsql", schema)]);
        // a.balance == a.limit_, x == y, @@avg == 0.5 and sqrt(2) == x; not a.n, 1 == 1 or `>`.
        assert_eq!(with_code(&found, "float-equality").len(), 4, "{found:?}");
    }

    #[test]
    fn flags_strings_where_numbers_are_needed() {
        let schema = "CREATE VERTEX Account (PRIMARY_ID id STRING, name STRING, balance DOUBLE, n INT)\n";
        let query = "CREATE QUERY q(STRING label) {\n  SumAccum<INT> @@total;\n  SumAccum<STRING> @@names;\n  SumAccum<DOUBLE> @sum;\n  SetAccum<INT> @@ids;\n  INT bad = \"x\";\n  BOOL also = \"true\";\n  STRING fine = \"x\";\n  S = {Account.*};\n  R = SELECT a FROM S:a\n      ACCUM @@total += a.name, @@total += a.n, @@names += a.name, a.@sum += label, a.@sum += a.balance * 2, @@ids += a.name,\n            @@total += \"1\" + a.n;\n  INT x = a.n - \"2\";\n  PRINT R, bad, also, fine, x;\n}\n";
        let found = findings(query, &[("file:///test/schema.gsql", schema)]);
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
        let found = findings(query, &[("file:///test/schema.gsql", schema)]);
        assert_eq!(with_code(&found, "type-mismatch").len(), 4, "{found:?}");
    }

    #[test]
    fn whole_number_comparisons_are_exact() {
        let schema = "CREATE VERTEX Account (PRIMARY_ID id STRING, balance DOUBLE)\n";
        let query = "CREATE QUERY q(DOUBLE val) {\n  S = {Account.*};\n  R = SELECT a FROM S:a WHERE a.balance == 0 OR a.balance == -1 OR 2.0 == a.balance;\n  BOOL whole = val == float_to_int(val);\n  BOOL also = (floor(val)) == val OR val == round( val );\n  BOOL flagged = val == float_to_int(val + 1) OR val == 0.1;\n  PRINT R, whole, also, flagged;\n}\n";
        let found = findings(query, &[("file:///test/schema.gsql", schema)]);
        // Only `val == float_to_int(val + 1)` (another value) and `val == 0.1`.
        assert_eq!(with_code(&found, "float-equality").len(), 2, "{found:?}");
    }

    #[test]
    fn float_equality_has_its_own_switch() {
        let query = "CREATE QUERY q(FLOAT x, FLOAT y) {\n  IF x == y THEN PRINT x; END;\n}\n";
        let mut fixture = Fixture::new(query);
        assert_eq!(with_code(&findings(query, &[]), "float-equality").len(), 1);
        fixture.config.update(&serde_json::json!({ "diagnostics": { "floatEquality": false } }));
        let found: Vec<String> = diagnostics(&fixture.snapshot()).into_iter().filter_map(|d| d.code).collect();
        assert!(!found.contains(&"float-equality".to_string()), "{found:?}");
        // The other language rules stay on.
        let reserved = "CREATE QUERY q(FLOAT x, FLOAT y) {\n  INT count = 1;\n  IF x == y THEN PRINT count; END;\n}\n";
        let mut fixture = Fixture::new(reserved);
        fixture.config.update(&serde_json::json!({ "diagnostics": { "floatEquality": false } }));
        let found: Vec<String> = diagnostics(&fixture.snapshot()).into_iter().filter_map(|d| d.code).collect();
        assert!(
            found.contains(&"reserved-word".to_string()) && !found.contains(&"float-equality".to_string()),
            "{found:?}"
        );
    }

    #[test]
    fn checks_interpreted_and_distributed_limitations() {
        let interpreted = "INTERPRET QUERY (FILE f, INT n) {\n  SumAccum<INT> @@n;\n  R = SELECT s FROM _:s WHERE s.x IS NULL;\n  PRINT R TO_CSV f;\n  RETURN 1;\n}\n";
        let found = findings(interpreted, &[]);
        let messages = with_code(&found, "interpreted-mode");
        assert_eq!(messages.len(), 6, "{messages:?}");
        assert!(messages.iter().all(|m| m.contains("not support") || m.contains("same name")), "{messages:?}");
        // The same body is fine in an installed query.
        let installed = interpreted.replace("INTERPRET QUERY (", "CREATE QUERY q(");
        assert!(with_code(&findings(&installed, &[]), "interpreted-mode").is_empty());

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
        let mut fixture = Fixture::new("CREATE QUERY q(INT count) { PRINT count; }\n");
        fixture.config.diagnostics_language_rules = false;
        assert!(diagnostics(&fixture.snapshot()).is_empty());
    }

    #[test]
    fn number_and_string_comparisons_with_v3_operators() {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE UNDIRECTED EDGE Knows (FROM Person, TO Person, since INT)\n";
        let v3 = "CREATE QUERY q() FOR GRAPH G SYNTAX V3 {\n  INT n = 1;\n  SELECT p INTO T FROM (p:Person {age: \"abc\", name: 5, name: \"x\", age: 7})-[:Knows {since: \"1\"}]-(q:Person) WHERE q.age = \"7\" AND q.name = \"a\" AND q.age <> 7;\n  IF n = \"a\" THEN PRINT n; END;\n  IF n <> \"a\" THEN PRINT n; END;\n  IF n = 1 THEN PRINT n; END;\n  PRINT T;\n}\n";
        let found = findings(v3, &[("file:///test/schema.gsql", schema)]);
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
        assert_eq!(with_code(&found, "type-mismatch"), ["`<>` compares a number with a string"], "{found:?}");
    }

    #[test]
    fn cypher_patterns_need_an_explicit_v3_only_when_v1_or_v2_is_declared() {
        let pattern = "SELECT p.name INTO T FROM (p:Person)-[e:Knows]-(q:Person);\n  SELECT q.name INTO T2 FROM (p:Person)-[e:Knows]-(q:Person);\n";
        let query = |clause: &str| format!("CREATE QUERY q() FOR GRAPH G {clause}{{\n  {pattern}  PRINT T;\n}}\n");
        let reported = |text: &str| with_code(&findings(text, &[]), "cypher-syntax").len();
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
        let actions = code_actions(&snapshot, line, &all, None, &all);
        assert!(actions.iter().any(|a| a.title == "Change the query to SYNTAX V3"), "{actions:?}");
    }

    const SUBQUERIES: &str = "CREATE QUERY count_of(INT n, STRING label, BOOL flag) FOR GRAPH G RETURNS (INT) { RETURN n; }\n\
        CREATE QUERY name_of() FOR GRAPH G RETURNS (STRING) { RETURN \"a\"; }\n\
        CREATE QUERY verts() FOR GRAPH G RETURNS (SET<VERTEX>) { SetAccum<VERTEX> @@s; RETURN @@s; }\n\
        CREATE QUERY plain(INT n) FOR GRAPH G { PRINT n; }\n";

    fn caller(body: &str) -> String {
        format!("CREATE QUERY q(STRING s, INT i, BOOL b) FOR GRAPH G {{\n{body}\n}}\n")
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
            &caller("  INT a = plain(1);\n  a = plain(2);\n  INT c = count_of(1, \"x\", TRUE);\n  PRINT a, c;"),
            &[("file:///test/s.gsql", SUBQUERIES)],
        );
        let expected = "Query `plain` has no RETURNS clause, so it returns no value";
        assert_eq!(with_code(&found, "type-mismatch"), [expected, expected], "{found:?}");
        // Without the query in the workspace, or with two of that name, nothing is known.
        assert!(with_code(&findings(&caller("  INT a = plain(1);\n  PRINT a;"), &[]), "type-mismatch").is_empty());
        let twice = format!("{SUBQUERIES}CREATE QUERY plain(INT n) FOR GRAPH G RETURNS (INT) {{ RETURN n; }}\n");
        let found = findings(&caller("  INT a = plain(1);\n  PRINT a;"), &[("file:///test/s.gsql", &twice)]);
        assert!(with_code(&found, "type-mismatch").is_empty(), "{found:?}");
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

    fn select_block(select: &str) -> String {
        format!(
            "CREATE QUERY q() FOR GRAPH G {{\n  SumAccum<INT> @cnt;\n  SumAccum<INT> @@n;\n  S = {{Person.*}};\n  R = {select};\n  PRINT R;\n}}\n"
        )
    }

    fn select_messages(select: &str, code: &str) -> Vec<String> {
        let found = findings(&select_block(select), &[]);
        with_code(&found, code).iter().map(|m| m.to_string()).collect()
    }

    #[test]
    fn having_uses_the_selected_alias() {
        let messages = |select: &str| select_messages(select, "having-alias");
        // The documentation's example, in both patterns syntaxes.
        let docs = "SELECT v FROM (v:S) -[e]- (tgt:Post) HAVING tgt.subject == \"cats\"";
        assert_eq!(messages(docs), ["The SELECT block selects `v`, but the HAVING clause uses `tgt`"]);
        let v1 = "SELECT v FROM S:v -(:e)- Post:tgt HAVING tgt.subject == \"cats\"";
        assert_eq!(messages(v1).len(), 1);
        // Accumulators of another alias count too.
        assert_eq!(messages("SELECT v FROM S:v -(E)- Post:t HAVING v.@cnt > 1 AND t.@cnt > 1").len(), 1);
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
        let count = |tail: &str| select_messages(&format!("SELECT {tail}"), "per-alias");
        // The three illegal examples of the documentation.
        assert_eq!(
            count(&format!("t {from} PER (s, m) ACCUM @@n += 1")),
            ["`t` is used here but does not appear in the PER clause"]
        );
        assert_eq!(count(&format!("t {from} PER (s, m) ACCUM t.@cnt += 1")).len(), 2);
        assert_eq!(count(&format!("s {from} PER (s) ACCUM s.@cnt += 1 POST-ACCUM t.@cnt = 1")).len(), 1);
        assert_eq!(count("v FROM (v:S) -[e]- (t:Post) PER (v) ACCUM t.@cnt += 1").len(), 1);
        // Valid: the documentation's legal examples, and names that are no pattern vertices.
        for ok in [
            format!("s {from} PER (s) ACCUM @@n += 1"),
            format!("t {from} PER (s, t) ACCUM @@n += 1"),
            format!("t {from} PER (s, m, t) ACCUM t.@cnt += 1, s.@cnt += 1"),
            format!("s {from} PER (s) ACCUM s.@cnt += edge1.x POST-ACCUM s.@cnt += 1"),
            format!("t {from} ACCUM t.@cnt += 1"),
        ] {
            assert!(count(&ok).is_empty(), "{ok}");
        }
    }

    #[test]
    fn conjunctive_patterns_must_share_a_vertex_alias() {
        let count = |from: &str| select_messages(&format!("SELECT p {from}"), "pattern-join").len();
        // The documentation's invalid example, and its V3 form.
        assert_eq!(count("FROM Person:p - (KNOWS) - Person:tgt, Post:s - (<LIKES) - Person:t"), 1);
        assert_eq!(count("FROM (p:Person) - [:Friend] - (tgt:Person), (s:Post) - [:Likes] - (t:Person)"), 1);
        // The third pattern joins the second but not the first.
        assert_eq!(count("FROM X:p - (E) - Y:a, Z:b - (E) - W:c, Z:b - (E) - U:d"), 1);
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
    fn a_plain_assignment_before_the_read_is_only_a_hint_and_equal_bounds_are_a_count() {
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
            severities(&query("ACCUM s.@c = 1, @@n += s.@c"), "accumulator-read-after-write"),
            [Some(crate::lsp::types::severity::HINT)]
        );
        assert_eq!(
            severities(&query("ACCUM s.@c += 1, @@n += s.@c"), "accumulator-read-after-write"),
            [Some(crate::lsp::types::severity::WARNING)]
        );
        let edge = |bounds: &str| {
            format!(
                "CREATE QUERY q() {{\n  S = {{P.*}};\n  R = SELECT t FROM S:s -(E>*{bounds}:e)- P:t;\n  PRINT R;\n}}\n"
            )
        };
        for exact in ["3", "2..2"] {
            assert!(severities(&edge(exact), "kleene-edge-alias").is_empty(), "*{exact}");
        }
        assert_eq!(severities(&edge("1..3"), "kleene-edge-alias").len(), 1);
    }

    #[test]
    fn accumulators_are_read_from_the_snapshot_of_the_accum_clause() {
        let count = |tail: &str| {
            select_messages(&format!("SELECT s FROM S:s -(E:e)- P:t {tail}"), "accumulator-read-after-write")
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
        assert_eq!(count("ACCUM s.@cnt = 1, @@n += s.@cnt + s.@cnt").len(), 2);
        assert_eq!(count("ACCUM @@n += 1, IF @@n > 3 THEN t.@cnt += 1 END").len(), 1);
        assert_eq!(count("ACCUM IF e.x > 1 THEN @@n += 1, t.@cnt += @@n END").len(), 1);
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
        assert!(with_code(&found, "accumulator-read-after-write").is_empty(), "{found:?}");
    }

    #[test]
    fn edge_aliases_need_a_single_edge() {
        let count = |from: &str| select_messages(&format!("SELECT t {from}"), "kleene-edge-alias");
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
        for ok in
            ["FROM S:s -(E>*)- P:t", "FROM S:s -(E>*1..3)- P:t", "FROM S:s -(E>:e)- P:t", "FROM S:s -(E>*2:e)- P:t"]
        {
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
        findings(&text, &[("file:///test/schema.gsql", schema)])
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
            ("  @@m += (\"a\" -> \"x\");", "type-mismatch", "A string cannot be added to a SumAccum<INT>"),
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
            ("  @@m += 1;", "accumulator-input", "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs"),
            ("  @@m += p;", "accumulator-input", "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs"),
            ("  @@m = \"a\";", "accumulator-input", "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs"),
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
            ("  @@g2 += (\"a\", 1 -> \"x\", 3);", "type-mismatch", "A string cannot be added to a SumAccum<INT>"),
        ];
        for (body, code, message) in cases {
            let found = map_findings(body);
            let messages = with_code(&found, code);
            assert!(messages.contains(&message), "{body}: {message:?} not in {found:?}");
            // One clear error: no generic arithmetic or comparison warning besides it.
            let others: Vec<_> =
                found.iter().filter(|(c, m)| (c, m.as_str()) != (&code.to_string(), message)).collect();
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
        let actions = code_actions(&snapshot, whole, &all, None, &all);
        let edits = |title: &str| {
            let action = actions.iter().find(|a| a.title == title).unwrap_or_else(|| panic!("{title}: {actions:?}"));
            action.edit.changes.values().flatten().map(|e| (e.range, e.new_text.clone())).collect::<Vec<_>>()
        };
        assert_eq!(
            edits("Replace `-` with `->`"),
            [(Range::new(Position::new(2, 12), Position::new(2, 13)), "->".to_string())]
        );
        assert_eq!(
            edits("Replace `,` with `->`"),
            [(Range::new(Position::new(3, 11), Position::new(3, 12)), " ->".to_string())]
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
        let found: Vec<_> = findings(valid, &[]).into_iter().filter(|(code, _)| code != "unused").collect();
        assert!(found.is_empty(), "{found:?}");
    }

    /// The findings (without `unused`) of a query over a Person/Knows schema.
    fn graph_findings(header: &str, body: &str) -> Vec<(String, String)> {
        let schema = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\nCREATE UNDIRECTED EDGE Knows (FROM Person, TO Person, w INT)\nCREATE GRAPH G (Person, Knows)\nTYPEDEF TUPLE<STRING name, INT score> Rec;\n";
        let text = format!("CREATE QUERY q({header}) FOR GRAPH G {{\n{body}\n}}\n");
        findings(&text, &[("file:///test/schema.gsql", schema)])
            .into_iter()
            .filter(|(code, _)| code != "unused")
            .collect()
    }

    #[test]
    fn map_input_is_judged_by_shape_kind_and_source() {
        let declarations = "  MapAccum<STRING, SumAccum<INT>> @@m;\n  MapAccum<STRING, SumAccum<INT>> @mv;\n  MapAccum<INT, SumAccum<INT>> @@k;\n  MapAccum<STRING, Rec> @@mt;\n  MapAccum<Rec, INT> @@tk;\n  MapAccum<STRING, OrAccum> @@mo;\n  MapAccum<STRING, ListAccum<STRING>> @@ml;\n  SetAccum<INT> @@s;\n  SetAccum<STRING> @@ss;\n  ListAccum<INT> @@l;\n  SumAccum<INT> @@sum;\n  OrAccum @@or;\n  AvgAccum @@avg;\n  TYPEDEF SumAccum<INT> Cnt;\n  GroupByAccum<STRING k, Cnt c, SumAccum<INT> s> @@g;\n";
        let m = "A MapAccum<STRING, SumAccum<INT>> takes `(key -> value)` pairs";
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
            let found = graph_findings("STRING p, SET<STRING> ps", &format!("{declarations}  {statement}{print}"));
            let messages = with_code(&found, code);
            assert!(messages.contains(&message.as_str()), "{statement}: {message:?} not in {found:?}");
            let others: Vec<_> = found.iter().filter(|(c, m)| (c.as_str(), m) != (*code, message)).collect();
            assert!(others.is_empty(), "{statement}: {others:?}");
        }
        let valid = "  @@m += (\"a\" -> 1);\n  @@m += (p -> @@sum);\n  @@m += (\"a\" -> (1 + 2));\n  @@g += (\"a\" -> 1, 2);\n  @@l += (1, 2);\n  @@l += [1, 2];\n  @@s += @@s;\n  @@ml += (\"a\" -> \"x\");\n  @@ml += (\"a\" -> [\"x\"]);\n  @@or += TRUE;\n  @@or += p IN ps;\n  @@avg += 1;\n  @@mt += (\"a\" -> Rec(\"x\", 1));\n  @@m += @@m;\n  R = SELECT t FROM Person:s -(Knows:e)- Person:t ACCUM @@m += (s.name -> e.w), t.@mv += (t.name -> 1);\n  PRINT R;";
        let found = graph_findings("STRING p, SET<STRING> ps", &format!("{declarations}{valid}{print}"));
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
            found.iter().map(|(c, m)| format!("{c}: {m}")).collect::<Vec<_>>(),
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
            ["A query parameter cannot be an accumulator; declare the accumulator in the query body"]
        );
        // Reported once, not also for its arguments.
        let found = graph_findings("MapAccum<STRING> m", "  PRINT m;");
        assert_eq!(
            with_code(&found, "accumulator-type"),
            ["A query parameter cannot be an accumulator; declare the accumulator in the query body"]
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
        let actions = code_actions(&snapshot, whole, &all, None, &all);
        let action = actions
            .iter()
            .find(|a| a.title == "Write `AvgAccum` without a type argument")
            .unwrap_or_else(|| panic!("{actions:?}"));
        let edits: Vec<_> = action.edit.changes.values().flatten().map(|e| (e.range, e.new_text.clone())).collect();
        assert_eq!(edits, [(Range::new(Position::new(1, 10), Position::new(1, 18)), String::new())]);
    }
}
