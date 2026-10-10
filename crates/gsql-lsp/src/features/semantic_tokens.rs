//! Semantic tokens: resolved identifiers, plus (optionally) lexical tokens.

use crate::analysis::{Reference, Role, SymbolKind};
use crate::features::Snapshot;
use crate::features::resolve::{self, Target};
use crate::lsp::types::Range;
use crate::syntax;
use crate::text::Span;

pub const TOKEN_TYPES: &[&str] = &[
    "namespace",
    "type",
    "class",
    "struct",
    "parameter",
    "variable",
    "property",
    "function",
    "method",
    "keyword",
    "comment",
    "string",
    "number",
    "operator",
];

pub const TOKEN_MODIFIERS: &[&str] = &[
    "declaration",
    "readonly",
    "static",
    "defaultLibrary",
    "modification",
];

mod ty {
    pub const NAMESPACE: u32 = 0;
    pub const TYPE: u32 = 1;
    pub const CLASS: u32 = 2;
    pub const STRUCT: u32 = 3;
    pub const PARAMETER: u32 = 4;
    pub const VARIABLE: u32 = 5;
    pub const PROPERTY: u32 = 6;
    pub const FUNCTION: u32 = 7;
    pub const METHOD: u32 = 8;
    pub const KEYWORD: u32 = 9;
    pub const COMMENT: u32 = 10;
    pub const STRING: u32 = 11;
    pub const NUMBER: u32 = 12;
    pub const OPERATOR: u32 = 13;
}

mod modifier {
    pub const DECLARATION: u32 = 1 << 0;
    pub const READONLY: u32 = 1 << 1;
    pub const STATIC: u32 = 1 << 2;
    pub const DEFAULT_LIBRARY: u32 = 1 << 3;
    pub const MODIFICATION: u32 = 1 << 4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Token {
    span: Span,
    kind: u32,
    modifiers: u32,
}

fn kind_token(kind: SymbolKind) -> (u32, u32) {
    use SymbolKind as K;
    match kind {
        K::Graph | K::Package => (ty::NAMESPACE, 0),
        K::VertexType => (ty::CLASS, 0),
        K::EdgeType => (ty::STRUCT, 0),
        K::TupleType | K::AccumulatorType | K::Exception => (ty::TYPE, 0),
        K::Attribute | K::TupleField => (ty::PROPERTY, 0),
        K::Query | K::LoadingJob | K::SchemaChangeJob => (ty::FUNCTION, 0),
        K::Parameter => (ty::PARAMETER, 0),
        K::Alias | K::LoopVariable => (ty::VARIABLE, modifier::READONLY),
        K::GlobalAccumulator => (ty::VARIABLE, modifier::STATIC),
        K::LocalAccumulator => (ty::PROPERTY, modifier::STATIC),
        K::Variable
        | K::VertexSet
        | K::File
        | K::Table
        | K::FilenameVariable
        | K::Header
        | K::LineFilter
        | K::TempTable
        | K::TempColumn
        | K::DataSource => (ty::VARIABLE, 0),
    }
}

fn role_token(role: &Role) -> (u32, u32) {
    match role {
        Role::VertexType | Role::VertexSource | Role::SchemaType => {
            (ty::CLASS, 0)
        }
        Role::EdgeType | Role::EdgeSource => (ty::STRUCT, 0),
        Role::Graph => (ty::NAMESPACE, 0),
        Role::Query | Role::Job | Role::Function => (ty::FUNCTION, 0),
        Role::TupleType | Role::Exception => (ty::TYPE, 0),
        Role::TupleField(_) | Role::Attribute(_) => (ty::PROPERTY, 0),
        Role::Method(_) => (ty::METHOD, 0),
        Role::GlobalAccumulator => (ty::VARIABLE, modifier::STATIC),
        Role::LocalAccumulator => (ty::PROPERTY, modifier::STATIC),
        Role::Value
        | Role::Alias
        | Role::JobLocal
        | Role::JobFile(_)
        | Role::TempColumn(_) => (ty::VARIABLE, 0),
    }
}

fn reference_token(snapshot: &Snapshot, reference: &Reference) -> Token {
    let (kind, mut modifiers) = match resolve::target(snapshot, reference) {
        Some(Target::Local(id)) => {
            kind_token(snapshot.analysis.symbols[id].kind)
        }
        Some(Target::Global(key)) => kind_token(key.kind),
        Some(Target::Function(_)) => {
            (ty::FUNCTION, modifier::DEFAULT_LIBRARY)
        }
        Some(Target::Method(_)) => (ty::METHOD, modifier::DEFAULT_LIBRARY),
        Some(Target::Constant(..)) => {
            (ty::VARIABLE, modifier::READONLY | modifier::DEFAULT_LIBRARY)
        }
        None => role_token(&reference.role),
    };
    if reference.declaration {
        modifiers |= modifier::DECLARATION;
    } else if reference.write {
        modifiers |= modifier::MODIFICATION;
    }
    Token {
        span: reference.span,
        kind,
        modifiers,
    }
}

fn lexical_tokens(snapshot: &Snapshot, out: &mut Vec<Token>) {
    let mut cursor = snapshot.root().walk();
    let mut stack = vec![snapshot.root()];
    while let Some(node) = stack.pop() {
        let token = match node.kind() {
            "comment" => Some((ty::COMMENT, 0)),
            "string" | "column_reference" | "file_include" => {
                Some((ty::STRING, 0))
            }
            "integer" | "float" => Some((ty::NUMBER, 0)),
            "boolean" | "null" => Some((ty::KEYWORD, 0)),
            "accumulator_kind" => Some((ty::TYPE, modifier::DEFAULT_LIBRARY)),
            kind if syntax::is_name_kind(kind) => continue,
            _ if syntax::is_keyword(node) => {
                let in_type = node.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        "primitive_type"
                            | "vertex_type"
                            | "edge_type"
                            | "collection_type"
                            | "tuple_type"
                            | "file_type"
                    )
                });
                if in_type {
                    Some((ty::TYPE, modifier::DEFAULT_LIBRARY))
                } else {
                    Some((ty::KEYWORD, 0))
                }
            }
            _ if !node.is_named() && node.child_count() == 0 => {
                let text = node.kind();
                let is_operator = !text.is_empty()
                    && text
                        .chars()
                        .all(|c| "=+-*/%<>!&|^~'".contains(c));
                is_operator.then_some((ty::OPERATOR, 0))
            }
            _ => None,
        };
        match token {
            Some((kind, modifiers)) => out.push(Token {
                span: Span::of(node),
                kind,
                modifiers,
            }),
            None => {
                let children: Vec<_> = node.children(&mut cursor).collect();
                stack.extend(children.into_iter().rev());
            }
        }
    }
}

fn collect(snapshot: &Snapshot) -> Vec<Token> {
    // A `$"col"` is one string token.
    let mut tokens: Vec<Token> = (snapshot.analysis.references.iter())
        .filter(|r| r.declaration || !matches!(r.role, Role::TempColumn(_)))
        .map(|r| reference_token(snapshot, r))
        .collect();
    if snapshot.config.semantic_tokens_lexical {
        lexical_tokens(snapshot, &mut tokens);
    }
    tokens.sort_by_key(|t| (t.span.start, t.span.end));
    // Drop overlaps (e.g. a reverse edge name inside its string literal).
    let mut result: Vec<Token> = Vec::with_capacity(tokens.len());
    for token in tokens {
        match result.last() {
            Some(last) if token.span.start < last.span.end => {
                // Prefer the narrower (more specific) token.
                if token.span.len() < last.span.len() {
                    result.pop();
                    result.push(token);
                }
            }
            _ => result.push(token),
        }
    }
    result
}

/// Encodes tokens in the LSP relative format, splitting multi-line tokens.
fn encode(
    snapshot: &Snapshot,
    tokens: &[Token],
    range: Option<Range>,
) -> Vec<u32> {
    let text = snapshot.text();
    let lines = &snapshot.source.lines;
    let mut data = Vec::with_capacity(tokens.len() * 5);
    let (mut previous_line, mut previous_start) = (0u32, 0u32);
    for token in tokens {
        let mut start = token.span.start;
        while start < token.span.end {
            let line = lines.line_of(start);
            let end = token
                .span
                .end
                .min(lines.line_end(text, line).max(start));
            let start_position = snapshot.position(start);
            let end_position = snapshot.position(end);
            let next_line_start = lines.line_start(line + 1);
            let within = range.is_none_or(|r| r.contains(start_position));
            let length = end_position
                .character
                .saturating_sub(start_position.character);
            if within && length > 0 {
                let delta_line = start_position.line - previous_line;
                let delta_start = if delta_line == 0 {
                    start_position.character - previous_start
                } else {
                    start_position.character
                };
                data.extend([
                    delta_line,
                    delta_start,
                    length,
                    token.kind,
                    token.modifiers,
                ]);
                previous_line = start_position.line;
                previous_start = start_position.character;
            }
            if next_line_start <= start {
                break;
            }
            start = next_line_start;
        }
    }
    data
}

pub fn semantic_tokens(
    snapshot: &Snapshot,
    range: Option<Range>,
) -> Vec<u32> {
    let tokens = collect(snapshot);
    encode(snapshot, &tokens, range)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;

    fn decode(data: &[u32]) -> Vec<(u32, u32, u32, &'static str, u32)> {
        let mut line = 0;
        let mut start = 0;
        data.chunks(5)
            .map(|chunk| {
                if chunk[0] > 0 {
                    start = 0;
                }
                line += chunk[0];
                start += chunk[1];
                (
                    line,
                    start,
                    chunk[2],
                    TOKEN_TYPES[chunk[3] as usize],
                    chunk[4],
                )
            })
            .collect()
    }

    #[test]
    fn classifies_identifiers() {
        let fixture = Fixture::new(
            "CREATE VERTEX Person (PRIMARY_ID id STRING)\nCREATE QUERY q(INT k) {\n  SumAccum<INT> @@n;\n  S = {Person.*};\n  @@n += abs(k);\n}\n",
        );
        let tokens = decode(&semantic_tokens(&fixture.snapshot(), None));
        let kinds: Vec<&str> = tokens.iter().map(|t| t.3).collect();
        assert_eq!(
            kinds,
            vec![
                "class",
                "property",
                "function",
                "parameter",
                "variable",
                "variable",
                "class",
                "variable",
                "function",
                "parameter"
            ]
        );
        // `@@n` is static; `abs` comes from the default library.
        assert_eq!(tokens[4].4 & modifier::STATIC, modifier::STATIC);
        assert_eq!(
            tokens[8].4 & modifier::DEFAULT_LIBRARY,
            modifier::DEFAULT_LIBRARY
        );
    }

    #[test]
    fn splits_multiline_lexical_tokens() {
        let mut fixture = Fixture::new("/* a\n   b */\nLS\n");
        fixture.config.semantic_tokens_lexical = true;
        let tokens = decode(&semantic_tokens(&fixture.snapshot(), None));
        assert_eq!(
            tokens,
            vec![
                (0, 0, 4, "comment", 0),
                (1, 0, 7, "comment", 0),
                (2, 0, 2, "keyword", 0)
            ]
        );
    }

    #[test]
    fn methods_of_collections_are_default_library() {
        let fixture = Fixture::new(
            "CREATE QUERY q(SET<STRING> ss, MAP<STRING, INT> m) {\n  PRINT ss.contains(\"a\"), m.containsKey(\"a\"), ss.foo();\n}\n",
        );
        let tokens = decode(&semantic_tokens(&fixture.snapshot(), None));
        let methods: Vec<(u32, bool)> = tokens
            .iter()
            .filter(|t| t.3 == "method")
            .map(|t| (t.2, t.4 & modifier::DEFAULT_LIBRARY != 0))
            .collect();
        // `foo` is not a built-in.
        assert_eq!(methods, vec![(8, true), (11, true), (3, false)]);
    }
}
