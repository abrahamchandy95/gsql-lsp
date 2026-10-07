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

use crate::analysis::Analysis;
use crate::lsp::types::{Position, Range};
use crate::text::{PositionEncoding, SourceText, Span};
use crate::workspace::Workspace;

/// How the formatter treats keyword case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeywordCase {
    #[default]
    Preserve,
    Upper,
    Lower,
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
        if let Some(case) = value.get("format").and_then(|f| f.get("keywordCase")).and_then(Value::as_str) {
            self.format_keyword_case = match case.to_ascii_lowercase().as_str() {
                "upper" => KeywordCase::Upper,
                "lower" => KeywordCase::Lower,
                _ => KeywordCase::Preserve,
            };
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

impl Snapshot<'_> {
    pub fn text(&self) -> &str {
        &self.source.text
    }

    pub fn range(&self, span: Span) -> Range {
        self.source.range(span, self.encoding)
    }

    pub fn offset(&self, position: Position) -> usize {
        self.source.offset(position, self.encoding)
    }

    pub fn position(&self, offset: usize) -> Position {
        self.source.position(offset, self.encoding)
    }

    pub fn root(&self) -> tree_sitter::Node<'_> {
        self.tree.root_node()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Builds snapshots from source text with a `|` cursor marker.

    use super::*;
    use crate::workspace::FileIndex;

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
            let uri = "file:///test/main.gsql".to_string();
            let mut workspace = Workspace::default();
            for (other_uri, other_text) in others {
                let tree = crate::syntax::parse(&mut crate::syntax::new_parser(), other_text, None);
                let analysis = crate::analysis::analyze(&tree, other_text);
                let source = SourceText::new(other_text.to_string());
                workspace.update(FileIndex::of_document(other_uri, &tree, &analysis, &source, PositionEncoding::Utf16));
            }
            let tree = crate::syntax::parse(&mut crate::syntax::new_parser(), text, None);
            let analysis = crate::analysis::analyze(&tree, text);
            let source = SourceText::new(text.to_string());
            workspace.update(FileIndex::of_document(&uri, &tree, &analysis, &source, PositionEncoding::Utf16));
            let analysis = crate::analysis::analyze_in(&tree, text, Some(&workspace));
            Fixture {
                uri,
                source,
                tree,
                analysis,
                workspace,
                config: Config { diagnostics_no_schema_notice: false, diagnostics_style: false, ..Config::default() },
            }
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

    /// Splits `text` at the `|` marker, returning the text without it and the cursor offset.
    pub fn cursor(text: &str) -> (String, usize) {
        let offset = text.find('|').expect("cursor marker");
        let mut without = text.to_string();
        without.remove(offset);
        (without, offset)
    }
}
