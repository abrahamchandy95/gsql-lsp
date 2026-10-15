//! Resolves string literals reachable by a variable or accumulator (e.g.,
//! for `(@@edge_types:e)`). Names are matched by text within one query.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::syntax::{self, field, kind};
use crate::util::extend_unique;

/// Max recursion depth for assignment chains.
const MAX_DEPTH: usize = 8;

/// Returns all string literals `name` can hold at `at`, or `None` if any
/// assigned value is unknown, or nothing is assigned.
pub fn string_values(
    name: &str,
    at: Node,
    source: &str,
) -> Option<Vec<String>> {
    let mut scope = at;
    for ancestor in syntax::ancestors(at) {
        scope = ancestor;
        if matches!(
            ancestor.kind_id(),
            kind::QUERY_DEFINITION
                | kind::OPENCYPHER_QUERY_DEFINITION
                | kind::INTERPRET_QUERY_STATEMENT
        ) {
            break;
        }
    }

    let values = Values::new(source, scope);
    let mut found = Vec::new();
    values.of_name(name, 0, &mut found)?;

    (!found.is_empty()).then(|| {
        found
            .into_iter()
            .map(str::to_owned)
            .collect()
    })
}

struct Values<'a, 't> {
    source: &'a str,
    /// Variable/accumulator name -> assigned RHS nodes within the query.
    assignments: HashMap<&'a str, Vec<Node<'t>>>,
}

impl<'a, 't> Values<'a, 't> {
    fn new(source: &'a str, scope: Node<'t>) -> Self {
        let mut assignments: HashMap<&'a str, Vec<Node<'t>>> = HashMap::new();
        syntax::walk(scope, |n| {
            let (target, value) = match n.kind_id() {
                kind::ASSIGNMENT_STATEMENT => (
                    n.child_by_field_id(field::LEFT),
                    n.child_by_field_id(field::RIGHT),
                ),
                kind::VARIABLE_DECLARATOR | kind::ACCUMULATOR_DECLARATOR => (
                    n.child_by_field_id(field::NAME),
                    n.child_by_field_id(field::VALUE),
                ),
                _ => return,
            };
            if let (Some(target), Some(value)) = (target, value) {
                assignments
                    .entry(&source[target.byte_range()])
                    .or_default()
                    .push(value);
            }
        });
        Self {
            source,
            assignments,
        }
    }

    fn text(&self, node: Node) -> &'a str {
        &self.source[node.byte_range()]
    }

    fn eval(
        &self,
        expr: Node<'t>,
        depth: usize,
        out: &mut Vec<&'a str>,
    ) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        match expr.kind_id() {
            kind::STRING => {
                extend_unique(out, [self.text(expr).trim_matches('"')]);
                Some(())
            }
            kind::PARENTHESIZED_EXPRESSION => {
                self.eval(syntax::first_code_child(expr)?, depth + 1, out)
            }
            kind::IDENTIFIER
            | kind::GLOBAL_ACCUMULATOR
            | kind::LOCAL_ACCUMULATOR => {
                self.of_name(self.text(expr), depth + 1, out)
            }
            kind::CALL_EXPRESSION => {
                for value in self.map_get(expr, depth)? {
                    self.eval(value, depth + 1, out)?;
                }
                Some(())
            }
            _ => None,
        }
    }

    /// Evaluates all values assigned to `name`.
    fn of_name(
        &self,
        name: &str,
        depth: usize,
        out: &mut Vec<&'a str>,
    ) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        for &right in self.assignments.get(name)? {
            self.eval(right, depth + 1, out)?;
        }
        Some(())
    }

    /// Resolves candidate value nodes for `m.get(key)`. Unknown keys match
    /// all entries.
    fn map_get(&self, call: Node<'t>, depth: usize) -> Option<Vec<Node<'t>>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let function = call.child_by_field_id(field::FUNCTION)?;
        if function.kind_id() != kind::MEMBER_EXPRESSION
            || self.text(function.child_by_field_id(field::PROPERTY)?)
                != "get"
        {
            return None;
        }

        let map = function.child_by_field_id(field::OBJECT)?;
        let containers = match map.kind_id() {
            kind::IDENTIFIER
            | kind::GLOBAL_ACCUMULATOR
            | kind::LOCAL_ACCUMULATOR => {
                self.assignments.get(self.text(map))?.clone()
            }
            kind::CALL_EXPRESSION => self.map_get(map, depth + 1)?,
            _ => return None,
        };

        let arguments =
            syntax::code_children(call.child_by_field_id(field::ARGUMENTS)?);
        let [key] = arguments.as_slice() else {
            return None;
        };

        let mut keys = Vec::new();
        let has_known_keys = self
            .eval(*key, depth + 1, &mut keys)
            .is_some();

        let mut values = Vec::new();
        let mut entry_keys = Vec::new();

        for container in containers {
            for (entry_key, entry_value) in self.entries(container)? {
                let matches = if has_known_keys {
                    entry_keys.clear();
                    if self
                        .eval(entry_key, depth + 1, &mut entry_keys)
                        .is_some()
                    {
                        entry_keys.iter().any(|k| keys.contains(k))
                    } else {
                        true
                    }
                } else {
                    true
                };
                if matches {
                    values.push(entry_value);
                }
            }
        }
        Some(values)
    }

    /// Extracts `(key, value)` pairs from a map entry or literal.
    fn entries(&self, right: Node<'t>) -> Option<Vec<(Node<'t>, Node<'t>)>> {
        match right.kind_id() {
            kind::KEY_VALUE_PAIR => match syntax::code_children(right)[..] {
                [key, value] => Some(vec![(key, value)]),
                _ => None,
            },
            kind::MAP_LITERAL => syntax::code_children(right)
                .into_iter()
                .map(|entry| {
                    Some((
                        entry.child_by_field_id(field::KEY)?,
                        entry.child_by_field_id(field::VALUE)?,
                    ))
                })
                .collect(),
            _ => None,
        }
    }
}
