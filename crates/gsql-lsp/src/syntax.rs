//! Tree-sitter helpers shared by the analysis and the language features.

use tree_sitter::{Node, Parser, Tree};

use crate::text::Span;

pub fn language() -> tree_sitter::Language {
    tree_sitter_gsql::LANGUAGE.into()
}

pub fn new_parser() -> Parser {
    let mut parser = Parser::new();
    parser.set_language(&language()).expect(
        "the bundled GSQL grammar is compatible with the tree-sitter runtime",
    );
    parser
}

/// Parses `text`, reusing `old_tree` (already edited) when given.
pub fn parse(
    parser: &mut Parser,
    text: &str,
    old_tree: Option<&Tree>,
) -> Tree {
    parser
        .parse(text, old_tree)
        .or_else(|| parser.parse(text, None))
        .expect(
            "parsing without a timeout or cancellation flag always succeeds",
        )
}

pub fn text<'a>(node: Node, source: &'a str) -> &'a str {
    source
        .get(node.start_byte()..node.end_byte())
        .unwrap_or("")
}

pub fn field_text<'a>(
    node: Node,
    field: &str,
    source: &'a str,
) -> Option<&'a str> {
    node.child_by_field_name(field)
        .map(|child| text(child, source))
}

pub fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

pub fn named_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Named children without comments (which can sit between any two tokens).
pub fn code_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .collect()
}

/// The first named child that is not a comment.
pub fn first_code_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() != "comment")
}

/// The last named child that is not a comment.
pub fn last_code_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let mut children: Vec<Node> = node.named_children(&mut cursor).collect();
    children.reverse();
    children
        .into_iter()
        .find(|c| c.kind() != "comment")
}

/// The first child (named or anonymous) of kind `kind`.
pub fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|c| c.kind() == kind)
}

/// Whether `node` has a child (named or anonymous) of kind `kind`.
pub fn has_child(node: Node, kind: &str) -> bool {
    child_of_kind(node, kind).is_some()
}

/// The nearest preceding sibling, named or anonymous, that is not a comment.
pub fn prev_non_comment_sibling<'tree>(
    node: Node<'tree>,
) -> Option<Node<'tree>> {
    std::iter::successors(node.prev_sibling(), |p| p.prev_sibling())
        .find(|p| p.kind() != "comment")
}

pub fn children_by_field<'tree>(
    node: Node<'tree>,
    field: &str,
) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field, &mut cursor)
        .collect()
}

pub fn ancestors(node: Node) -> impl Iterator<Item = Node> {
    std::iter::successors(node.parent(), |n| n.parent())
}

/// The node itself followed by its ancestors.
pub fn self_and_ancestors(node: Node) -> impl Iterator<Item = Node> {
    std::iter::successors(Some(node), |n| n.parent())
}

/// `node` and its ancestors up to `root`, innermost first. Equivalent to
/// [`self_and_ancestors`], but walks down from the root once instead of once
/// per ancestor (tree-sitter finds a parent by searching from the root).
pub fn lineage<'tree>(
    root: Node<'tree>,
    node: Node<'tree>,
) -> Vec<Node<'tree>> {
    let mut path = vec![root];
    let mut current = root;
    while current != node {
        let Some(child) = current.child_with_descendant(node) else {
            break;
        };
        path.push(child);
        current = child;
    }
    path.reverse();
    path
}

/// `node` itself or its innermost ancestor whose kind is one of `kinds`.
pub fn find_ancestor<'tree>(
    node: Node<'tree>,
    kinds: &[&str],
) -> Option<Node<'tree>> {
    self_and_ancestors(node).find(|n| kinds.contains(&n.kind()))
}

/// The field name under which `node` appears in its parent.
pub fn field_name<'tree>(node: Node<'tree>) -> Option<&'tree str> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        if cursor.node() == node {
            return cursor.field_name();
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

/// The smallest node of any kind that contains `offset`, preferring the
/// node that ends exactly at `offset` when the cursor sits right after a token.
pub fn leaf_at(root: Node, offset: usize) -> Option<Node> {
    let node = root.descendant_for_byte_range(offset, offset)?;
    if node.start_byte() == offset
        && offset > 0
        && let Some(previous) =
            root.descendant_for_byte_range(offset - 1, offset - 1)
        && previous.end_byte() == offset
        && is_word_like(previous)
    {
        return Some(previous);
    }
    Some(node)
}

/// The first leaf under `node` (or `node` itself), comments and MISSING nodes included.
pub fn first_leaf(node: Node) -> Node {
    let mut leaf = node;
    while let Some(child) = leaf.child(0) {
        leaf = child;
    }
    leaf
}

/// The identifier-like token at (or immediately before) `offset`.
pub fn word_at(root: Node, offset: usize) -> Option<Node> {
    let candidates = [Some(offset), offset.checked_sub(1)];
    for candidate in candidates.into_iter().flatten() {
        if let Some(node) =
            root.descendant_for_byte_range(candidate, candidate)
            && is_word_like(node)
            && Span::of(node).contains(offset)
        {
            return Some(node);
        }
    }
    None
}

pub fn is_word_like(node: Node) -> bool {
    matches!(
        node.kind(),
        "identifier"
            | "global_accumulator"
            | "local_accumulator"
            | "accumulator_kind"
    ) || is_keyword(node)
}

/// Anonymous keyword tokens are named after their upper-case spelling.
pub fn is_keyword(node: Node) -> bool {
    !node.is_named()
        && node.kind().len() > 1
        && node.kind().chars().all(|c| {
            c.is_ascii_uppercase() || c == '_' || c == '-' || c == ' '
        })
}

/// A token of the code: a leaf with text that is not a comment or another extra.
pub fn is_code_token(node: Node) -> bool {
    node.child_count() == 0
        && !node.is_extra()
        && !node.is_missing()
        && node.end_byte() > node.start_byte()
}

/// Kinds of the name nodes that the analysis resolves.
pub fn is_name_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "global_accumulator"
            | "local_accumulator"
            | "qualified_identifier"
    )
}

/// What a built-in word under the cursor names.
pub enum BuiltinWord<'s> {
    /// An accumulator type name (`SumAccum`), as spelled in the source.
    Accumulator(&'s str),
    /// A keyword or built-in type name, by token kind (`FOREACH`, `DATETIME`).
    Keyword(&'s str),
}

/// The accumulator type name or keyword at (or just before) `offset`, except the
/// SET clause keyword of `UPDATE`.
pub fn builtin_word_at<'t>(
    root: Node<'t>,
    source: &'t str,
    offset: usize,
) -> Option<(Node<'t>, BuiltinWord<'t>)> {
    let node = word_at(root, offset)?;
    if node.kind() == "accumulator_kind" {
        return Some((node, BuiltinWord::Accumulator(text(node, source))));
    }
    let update_set = node.kind() == "SET"
        && node
            .parent()
            .is_some_and(|p| p.kind() == "update_statement");
    (is_keyword(node) && !update_set)
        .then(|| (node, BuiltinWord::Keyword(node.kind())))
}

/// The name and argument list of `RUN QUERY q(..)`, `INTERPRET QUERY q(..)` or `q(..)`;
/// the last also matches built-ins such as `abs(x)`.
pub fn query_call(node: Node) -> Option<(Node, Node)> {
    let name = match node.kind() {
        "run_query_statement" => node.child_by_field_name("query")?,
        "interpret_query_statement" => node.child_by_field_name("name")?,
        "call_expression" => node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "identifier")?,
        _ => return None,
    };
    Some((name, node.child_by_field_name("arguments")?))
}

/// The unquoted versions named by a query's SYNTAX clauses.
pub fn declared_syntax_versions<'a>(
    query: Node,
    source: &'a str,
) -> Vec<&'a str> {
    code_children(query)
        .into_iter()
        .filter(|c| c.kind() == "syntax_clause")
        .filter_map(|c| field_text(c, "version", source))
        .map(|v| v.trim_matches('"'))
        .collect()
}

/// Comments immediately preceding `node` (no blank line in between), with
/// comment markers stripped. Used as documentation for declarations.
pub fn leading_comments(node: Node, source: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut current = node;
    let mut next_start_row = node.start_position().row;
    while let Some(previous) = current.prev_sibling() {
        if previous.kind() != "comment"
            || previous.end_position().row + 1 < next_start_row
        {
            break;
        }
        // A trailing comment on the previous statement's line is not documentation.
        if let Some(before) = previous.prev_sibling()
            && before.end_position().row == previous.start_position().row
        {
            break;
        }
        lines.push(strip_comment(text(previous, source)));
        next_start_row = previous.start_position().row;
        current = previous;
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    let joined = lines.join("\n").trim().to_string();
    (!joined.is_empty()).then_some(joined)
}

fn strip_comment(comment: &str) -> String {
    if let Some(body) = comment.strip_prefix("/*") {
        let body = body.strip_suffix("*/").unwrap_or(body);
        body.lines()
            .map(|line| line.trim().trim_start_matches('*').trim())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else if let Some(body) = comment.strip_prefix("//") {
        body.trim().to_string()
    } else if let Some(body) = comment.strip_prefix('#') {
        body.trim().to_string()
    } else {
        comment.trim().to_string()
    }
}

/// Every node in the tree, in document order.
pub fn walk<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>)) {
    let mut cursor = root.walk();
    loop {
        visit(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Every node in the tree, in document order, and whether it is inside an
/// ERROR node (cheaper than asking each node for its ancestors: tree-sitter
/// finds a parent by walking down from the root).
pub fn walk_with_errors<'tree>(
    root: Node<'tree>,
    mut visit: impl FnMut(Node<'tree>, bool),
) {
    let mut cursor = root.walk();
    // Whether each ancestor of the current node is an ERROR node.
    let mut errors: Vec<bool> = Vec::new();
    let mut inside = 0;
    loop {
        let node = cursor.node();
        visit(node, inside > 0);
        if cursor.goto_first_child() {
            errors.push(node.is_error());
            inside += usize::from(node.is_error());
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            inside -= usize::from(errors.pop().unwrap_or(false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_leading_comments() {
        let source =
            "// Says hello.\n// Twice.\nCREATE QUERY q() { PRINT 1; }\n";
        let tree = parse(&mut new_parser(), source, None);
        let query = tree.root_node().named_child(2).unwrap();
        assert_eq!(query.kind(), "query_definition");
        assert_eq!(
            leading_comments(query, source).as_deref(),
            Some("Says hello.\nTwice.")
        );
    }

    #[test]
    fn ignores_detached_comments() {
        let source = "// detached\n\nUSE GRAPH g\n";
        let tree = parse(&mut new_parser(), source, None);
        let statement = tree.root_node().named_child(1).unwrap();
        assert_eq!(leading_comments(statement, source), None);
    }

    #[test]
    fn finds_field_names() {
        let source = "USE GRAPH g";
        let tree = parse(&mut new_parser(), source, None);
        let identifier = tree
            .root_node()
            .descendant_for_byte_range(10, 10)
            .unwrap();
        assert_eq!(field_name(identifier), Some("graph"));
    }
}
