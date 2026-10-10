//! Language features. Each feature works on a [`Snapshot`] of one document
//! together with the workspace index.

pub mod autocorrect;
pub mod call_hierarchy;
pub mod code_actions;
pub mod completion;
pub mod diagnostics;
pub mod folding;
pub mod formatting;
pub mod hover;
pub mod inlay_hints;
pub mod links;
pub mod navigation;
pub mod reference;
pub mod resolve;
pub mod rules;
pub mod selection;
pub mod semantic_tokens;
pub mod signature_help;
pub mod style;
pub mod symbols;
pub mod values;

use serde_json::Value;
use tree_sitter::Tree;

use crate::analysis::{Analysis, Reference};
use crate::lsp::types::{Location, Position, Range, TextEdit};
use crate::syntax;
use crate::text::{
    PositionEncoding, SourceText, Span, cypher_literals, noncode_until,
};
use crate::workspace::Workspace;

/// How the formatter treats keyword case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeywordCase {
    #[default]
    Preserve,
    Upper,
    Lower,
}

impl KeywordCase {
    /// The setting value (`format.keywordCase`) and CLI value (`--keyword-case`).
    pub fn as_str(self) -> &'static str {
        match self {
            KeywordCase::Preserve => "preserve",
            KeywordCase::Upper => "upper",
            KeywordCase::Lower => "lower",
        }
    }

    /// Exact (case-sensitive) inverse of `as_str`.
    pub fn from_name(name: &str) -> Option<KeywordCase> {
        match name {
            "preserve" => Some(KeywordCase::Preserve),
            "upper" => Some(KeywordCase::Upper),
            "lower" => Some(KeywordCase::Lower),
            _ => None,
        }
    }
}

/// User-configurable behavior, read from `initializationOptions` and the
/// `gsql` section of `workspace/didChangeConfiguration`.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Also emit semantic tokens for keywords, literals, comments and operators.
    pub semantic_tokens_lexical: bool,
    pub diagnostics_unknown_types: bool,
    pub diagnostics_unknown_attributes: bool,
    pub diagnostics_undefined_names: bool,
    pub diagnostics_unused: bool,
    /// Rules from the language reference (reserved words, accumulator usage, query modes, ...).
    pub diagnostics_language_rules: bool,
    /// Exact equality of FLOAT/DOUBLE values (part of the language rules, with its own switch).
    pub diagnostics_float_equality: bool,
    /// Say so when no schema is found (type and attribute checks are then off).
    pub diagnostics_no_schema_notice: bool,
    /// The `duplicate-definition` code (same file and other files).
    pub diagnostics_duplicate_definitions: bool,
    /// Hints for deviations from the GSQL Style Guide (keywords in all caps, `//` comments).
    pub diagnostics_style: bool,
    pub format_keyword_case: KeywordCase,
    pub inlay_hints: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            semantic_tokens_lexical: false,
            diagnostics_unknown_types: true,
            diagnostics_unknown_attributes: true,
            diagnostics_undefined_names: true,
            diagnostics_unused: true,
            diagnostics_language_rules: true,
            diagnostics_float_equality: true,
            diagnostics_no_schema_notice: true,
            diagnostics_duplicate_definitions: true,
            diagnostics_style: true,
            format_keyword_case: KeywordCase::Preserve,
            inlay_hints: true,
        }
    }
}

impl Config {
    /// Applies settings from a JSON object such as
    /// `{"diagnostics": {"unknownTypes": false}, "format": {"keywordCase": "upper"}}`.
    /// A top-level `gsql` key is unwrapped. Unknown keys are ignored.
    pub fn update(&mut self, value: &Value) {
        let value = value.get("gsql").unwrap_or(value);
        let flag = |path: &[&str]| -> Option<bool> {
            let mut current = value;
            for key in path {
                current = current.get(key)?;
            }
            current.as_bool()
        };
        if let Some(v) = flag(&["semanticTokens", "lexical"]) {
            self.semantic_tokens_lexical = v;
        }
        if let Some(v) = flag(&["diagnostics", "unknownTypes"]) {
            self.diagnostics_unknown_types = v;
        }
        if let Some(v) = flag(&["diagnostics", "unknownAttributes"]) {
            self.diagnostics_unknown_attributes = v;
        }
        if let Some(v) = flag(&["diagnostics", "undefinedNames"]) {
            self.diagnostics_undefined_names = v;
        }
        if let Some(v) = flag(&["diagnostics", "unused"]) {
            self.diagnostics_unused = v;
        }
        if let Some(v) = flag(&["diagnostics", "languageRules"]) {
            self.diagnostics_language_rules = v;
        }
        if let Some(v) = flag(&["diagnostics", "noSchemaNotice"]) {
            self.diagnostics_no_schema_notice = v;
        }
        if let Some(v) = flag(&["diagnostics", "duplicateDefinitions"]) {
            self.diagnostics_duplicate_definitions = v;
        }
        if let Some(v) = flag(&["diagnostics", "style"]) {
            self.diagnostics_style = v;
        }
        if let Some(v) = flag(&["diagnostics", "floatEquality"]) {
            self.diagnostics_float_equality = v;
        }
        if let Some(v) = flag(&["inlayHints", "enabled"]) {
            self.inlay_hints = v;
        }
        if let Some(case) = value
            .get("format")
            .and_then(|f| f.get("keywordCase"))
            .and_then(Value::as_str)
        {
            self.format_keyword_case =
                KeywordCase::from_name(&case.to_ascii_lowercase())
                    .unwrap_or_default();
        }
    }
}

/// Everything a feature needs to know about one document.
pub struct Snapshot<'a> {
    pub uri: &'a str,
    pub source: &'a SourceText,
    pub tree: &'a Tree,
    pub analysis: &'a Analysis,
    pub workspace: &'a Workspace,
    pub encoding: PositionEncoding,
    pub config: &'a Config,
}

impl<'a> Snapshot<'a> {
    pub fn text(&self) -> &str {
        &self.source.text
    }

    pub fn range(&self, span: Span) -> Range {
        self.source.range(span, self.encoding)
    }

    /// The location of `span` in this document.
    pub fn location(&self, span: Span) -> Location {
        Location {
            uri: self.uri.to_string(),
            range: self.range(span),
        }
    }

    /// An edit replacing `span` with `new_text` verbatim.
    pub fn edit(&self, span: Span, new_text: impl Into<String>) -> TextEdit {
        TextEdit {
            range: self.range(span),
            new_text: new_text.into(),
        }
    }

    /// An edit inserting `text` verbatim at `offset`.
    pub fn insert(&self, offset: usize, text: impl Into<String>) -> TextEdit {
        self.edit(Span::new(offset, offset), text)
    }

    pub fn offset(&self, position: Position) -> usize {
        self.source.offset(position, self.encoding)
    }

    pub fn position(&self, offset: usize) -> Position {
        self.source.position(offset, self.encoding)
    }

    /// The reference at `position`.
    pub fn reference_at(&self, position: Position) -> Option<&'a Reference> {
        self.analysis
            .reference_at(self.offset(position))
    }

    pub fn root(&self) -> tree_sitter::Node<'a> {
        self.tree.root_node()
    }

    /// The smallest node that spans `span`.
    pub fn node_at(&self, span: Span) -> Option<tree_sitter::Node<'a>> {
        self.root()
            .descendant_for_byte_range(span.start, span.end)
    }

    /// The text with its comments, as the tree reads them, blanked out (offsets and line
    /// breaks kept). An unclosed `/*` outside openCypher blanks the rest of the text.
    pub fn parsed_code_text(&self) -> &'a str {
        let (root, text) = (self.root(), self.source.text.as_str());
        self.source.parsed_code_text(|| {
            parsed_literals(root, text)
                .into_iter()
                .filter(|s| {
                    matches!(text.as_bytes().get(s.start), Some(b'#' | b'/'))
                })
                .collect()
        })
    }

    /// The same snapshot, of another text of the document.
    pub fn with_document(
        &self,
        source: &'a SourceText,
        tree: &'a Tree,
        analysis: &'a Analysis,
    ) -> Snapshot<'a> {
        Snapshot {
            source,
            tree,
            analysis,
            ..*self
        }
    }
}

/// Byte ranges of the strings and comments of `text` as `root` reads them; an unclosed
/// `/*`, which has no node, is read lexically.
pub(crate) fn parsed_literals(
    root: tree_sitter::Node,
    text: &str,
) -> Vec<Span> {
    let mut spans = Vec::new();
    // How far the text is read.
    let mut done = 0;
    syntax::walk(root, |n| {
        let kind = n.kind();
        // A file include (`@/tmp/*.gsql`) or column reference (`$"a#b"`) is one
        // token: a `/*`, `#` or `"` in it starts nothing.
        let literal = matches!(
            kind,
            "string" | "string_content" | "comment" | "cypher_text"
        );
        let opaque = matches!(kind, "file_include" | "column_reference");
        if n.start_byte() < done || !(literal || opaque) {
            return;
        }
        done = noncode_until(text, done, n.start_byte(), &mut spans);
        if n.start_byte() < done {
            return;
        }
        match kind {
            "cypher_text" => spans.extend(cypher_literals(text, Span::of(n))),
            "file_include" | "column_reference" => {}
            _ => spans.push(Span::of(n)),
        }
        done = n.end_byte();
    });
    noncode_until(text, done, text.len(), &mut spans);
    spans
}

/// `s` unless `n` is 1.
pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Builds snapshots from source text with a `|` cursor marker.

    use super::*;
    use crate::lsp::types::CodeAction;
    use crate::workspace::FileIndex;

    /// The document a fixture is built from.
    pub const MAIN_URI: &str = "file:///test/main.gsql";

    /// The workspace file a test's schema lives in.
    pub const SCHEMA_URI: &str = "file:///test/schema.gsql";

    pub struct Fixture {
        pub uri: String,
        pub source: SourceText,
        pub tree: Tree,
        pub analysis: Analysis,
        pub workspace: Workspace,
        pub config: Config,
    }

    impl Fixture {
        pub fn new(text: &str) -> Fixture {
            Fixture::with_files(text, &[])
        }

        /// `others` are additional workspace files given as (uri, text).
        pub fn with_files(text: &str, others: &[(&str, &str)]) -> Fixture {
            let uri = MAIN_URI.to_string();
            let mut workspace = Workspace::default();
            for (other_uri, other_text) in others {
                workspace.update(index(other_uri, other_text));
            }
            let (tree, analysis) = crate::analysis::Analysis::parse(text);
            let source = SourceText::new(text.to_string());
            workspace.update(FileIndex::of_document(
                &uri,
                &tree,
                &analysis,
                &source,
                PositionEncoding::Utf16,
                None,
            ));
            let analysis = crate::analysis::Analysis::from_tree(
                &tree,
                text,
                Some(&workspace),
            );
            Fixture {
                uri,
                source,
                tree,
                analysis,
                workspace,
                config: Config {
                    diagnostics_no_schema_notice: false,
                    diagnostics_style: false,
                    ..Config::default()
                },
            }
        }

        /// `text` with `schema` as the workspace file [`SCHEMA_URI`].
        pub fn with_schema(text: &str, schema: &str) -> Fixture {
            Fixture::with_files(text, &[(SCHEMA_URI, schema)])
        }

        pub fn snapshot(&self) -> Snapshot<'_> {
            Snapshot {
                uri: &self.uri,
                source: &self.source,
                tree: &self.tree,
                analysis: &self.analysis,
                workspace: &self.workspace,
                encoding: PositionEncoding::Utf16,
                config: &self.config,
            }
        }
    }

    /// The workspace index of the file `uri` with `text`.
    pub fn index(uri: &str, text: &str) -> FileIndex {
        let (tree, analysis) = crate::analysis::Analysis::parse(text);
        let source = SourceText::new(text.to_string());
        let encoding = PositionEncoding::Utf16;
        FileIndex::of_document(uri, &tree, &analysis, &source, encoding, None)
    }

    /// A schema whose edge `ACTED_IN` goes from `Person` to `to`.
    pub fn acted_in(to: &str) -> String {
        format!(
            "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\n\
             CREATE VERTEX {to} (PRIMARY_ID id STRING, name STRING)\n\
             CREATE DIRECTED EDGE ACTED_IN (FROM Person, TO {to})\n"
        )
    }

    /// A query with the typo `THN` and the `|` marker on `m`, the target of `ACTED_IN`.
    pub const TYPO_BEFORE_AN_EDGE: &str = "CREATE QUERY q(INT n) FOR GRAPH G {\n  \
        IF n > 0 THN\n    \
        A = SELECT m FROM Person:p -(ACTED_IN>)- :m WHERE |m.name == \"x\";\n    \
        PRINT A;\n  END;\n}\n";

    /// Splits `text` at the `|` marker, returning the text without it and the cursor offset.
    pub fn cursor(text: &str) -> (String, usize) {
        let offset = text.find('|').expect("cursor marker");
        let mut without = text.to_string();
        without.remove(offset);
        (without, offset)
    }

    /// The message of every diagnostic of `text`, with other workspace files.
    pub fn messages_with(text: &str, others: &[(&str, &str)]) -> Vec<String> {
        let fixture = Fixture::with_files(text, others);
        diagnostics::diagnostics(&fixture.snapshot())
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    /// The message of every syntax error of `text`.
    pub fn syntax_messages(text: &str) -> Vec<String> {
        let fixture = Fixture::new(text);
        diagnostics::syntax_errors(&fixture.snapshot())
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    /// (code, message) of every diagnostic of `text` that has a code.
    pub fn findings(
        text: &str,
        others: &[(&str, &str)],
    ) -> Vec<(String, String)> {
        let fixture = Fixture::with_files(text, others);
        diagnostics::diagnostics(&fixture.snapshot())
            .into_iter()
            .filter_map(|d| Some((d.code?, d.message)))
            .collect()
    }

    /// The messages of the findings with `code`.
    pub fn with_code<'a>(
        found: &'a [(String, String)],
        code: &str,
    ) -> Vec<&'a str> {
        found
            .iter()
            .filter(|(c, _)| c == code)
            .map(|(_, m)| m.as_str())
            .collect()
    }

    /// The code actions over the whole of `text`, with other workspace files.
    pub fn actions(text: &str, others: &[(&str, &str)]) -> Vec<CodeAction> {
        let fixture = Fixture::with_files(text, others);
        let snapshot = fixture.snapshot();
        let found = diagnostics::diagnostics(&snapshot);
        let whole =
            Range::new(snapshot.position(0), snapshot.position(text.len()));
        code_actions::code_actions(&snapshot, whole, &found, None, &found)
    }

    /// `text` with the code action titled `title` applied.
    pub fn apply_fix(text: &str, title: &str) -> String {
        let found = actions(text, &[]);
        let action = found
            .iter()
            .find(|a| a.title == title)
            .unwrap_or_else(|| {
                panic!(
                    "no action {title:?} for {text:?} in {:?}",
                    found
                        .iter()
                        .map(|a| &a.title)
                        .collect::<Vec<_>>()
                )
            });
        let edits = &action.edit.changes[MAIN_URI];
        SourceText::new(text.to_string())
            .apply_edits(edits, PositionEncoding::Utf16)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{Fixture, MAIN_URI, actions, syntax_messages};
    use crate::text::{PositionEncoding, SourceText};

    #[test]
    fn the_parsed_code_text_blanks_the_comments_of_the_parse() {
        let text = concat!(
            "CREATE DATA_SOURCE s1 = \"\"\"{\"u\": \"s3://b\"}\"\"\" FOR GRAPH g # c\n",
            "CREATE QUERY q() {\n  PRINT \"a\n# b\", 1; // c\n}\n",
            "CREATE QUERY r() {\n  S = SELECT s FORM /* f */ P:s;\n}\n",
            "CREATE OPENCYPHER QUERY o() FOR GRAPH g {\n",
            "  MATCH (u:P) WHERE u.name = 'O\"Reilly' # x\n  RETURN u // d /* e */\n}\n",
        );
        let fixture = Fixture::new(text);
        // Not the `//` and `# b` in strings, nor `# x` in openCypher.
        let mut expected = text.to_string();
        for comment in ["# c\n", "// c\n", "/* f */", "// d /* e */"] {
            let blank = comment.replace(|c| c != '\n', " ");
            expected = expected.replacen(comment, &blank, 1);
        }
        assert_eq!(fixture.snapshot().parsed_code_text(), expected);
    }

    #[test]
    fn the_parsed_code_text_keeps_the_rest_of_an_unclosed_string() {
        let text = "CREATE QUERY q() {\n  PRINT \"abc;\n  # note /* x\n}\n";
        let fixture = Fixture::new(text);
        assert_eq!(fixture.snapshot().parsed_code_text(), text);
    }

    #[test]
    fn the_parsed_code_text_blanks_an_unclosed_comment_to_the_end() {
        // The openCypher `/* a` is plain text.
        let cypher =
            "CREATE OPENCYPHER QUERY o() FOR GRAPH g {\n  RETURN 1 /* a\n}\n";
        let text = format!(
            "{cypher}CREATE QUERY q() {{\n  PRINT 1 /* b \"c\" # d\n}}\n"
        );
        let fixture = Fixture::new(&text);
        let at = text.find("/* b").expect("comment");
        let blank = text[at..].replace(|c| c != '\n', " ");
        assert_eq!(
            fixture.snapshot().parsed_code_text(),
            format!("{}{blank}", &text[..at])
        );
    }

    #[test]
    fn the_parsed_code_text_keeps_a_file_include_path() {
        // `/*`, `//` and `#` in a path start no comment.
        let code = "CREATE QUERY q() {\n  PRINT 1;\n}\n";
        for include in ["@/tmp/*.gsql", "@my#1.gsql", "@/data//s.gsql"] {
            let text = format!("{include}\n{code}");
            let fixture = Fixture::new(&text);
            assert_eq!(fixture.snapshot().parsed_code_text(), text);
        }
    }

    #[test]
    fn a_missing_semicolon_after_a_file_include_is_inserted_in_the_query() {
        let text =
            "@/tmp/*.gsql\nCREATE QUERY q() {\n  INT x = 1\n  PRINT x\n}\n";
        let fixed: Vec<String> = actions(text, &[])
            .iter()
            .filter(|a| a.title == "Insert `;`")
            .map(|a| {
                let edits = &a.edit.changes[MAIN_URI];
                SourceText::new(text.to_string())
                    .apply_edits(edits, PositionEncoding::Utf16)
            })
            .collect();
        assert_eq!(
            fixed,
            [
                text.replace("= 1\n", "= 1;\n"),
                text.replace("x\n}", "x;\n}")
            ]
        );
    }

    #[test]
    fn a_trailing_comma_after_a_file_include_is_found() {
        let text = "@/tmp/*.gsql\nCREATE QUERY q(INT a, ) {\n  PRINT a;\n}\n";
        assert_eq!(
            syntax_messages(text),
            ["Syntax error: remove the trailing `,`"]
        );
    }
}
