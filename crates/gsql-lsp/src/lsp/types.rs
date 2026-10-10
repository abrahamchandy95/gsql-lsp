//! The subset of Language Server Protocol 3.17 types used by the server.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub fn new(line: u32, character: u32) -> Position {
        Position { line, character }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn new(start: Position, end: Position) -> Range {
        Range { start, end }
    }

    pub fn contains(&self, position: Position) -> bool {
        self.start <= position && position <= self.end
    }

    /// Whether `inner` lies within this range (both ends inclusive).
    pub fn contains_range(&self, inner: Range) -> bool {
        self.start <= inner.start && inner.end <= self.end
    }

    /// Whether `inner` starts and ends on lines this range covers, whatever the columns.
    pub fn contains_lines_of(&self, inner: Range) -> bool {
        self.start.line <= inner.start.line && inner.end.line <= self.end.line
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextDocumentIdentifier {
    pub uri: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentItem {
    pub uri: String,
    #[serde(default)]
    pub language_id: String,
    #[serde(default)]
    pub version: i32,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionedTextDocumentIdentifier {
    pub uri: String,
    #[serde(default)]
    pub version: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentContentChangeEvent {
    #[serde(default)]
    pub range: Option<Range>,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidOpenTextDocumentParams {
    pub text_document: TextDocumentItem,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidChangeTextDocumentParams {
    pub text_document: VersionedTextDocumentIdentifier,
    pub content_changes: Vec<TextDocumentContentChangeEvent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentParams {
    pub text_document: TextDocumentIdentifier,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentPositionParams {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceParams {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
    #[serde(default)]
    pub context: ReferenceContext,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceContext {
    #[serde(default)]
    pub include_declaration: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameParams {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
    pub new_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeParams {
    pub text_document: TextDocumentIdentifier,
    pub range: Range,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormattingOptions {
    #[serde(default = "default_tab_size")]
    pub tab_size: u32,
    #[serde(default = "default_true")]
    pub insert_spaces: bool,
    #[serde(default)]
    pub trim_trailing_whitespace: Option<bool>,
    #[serde(default)]
    pub insert_final_newline: Option<bool>,
    #[serde(default)]
    pub trim_final_newlines: Option<bool>,
}

impl Default for FormattingOptions {
    fn default() -> Self {
        FormattingOptions {
            tab_size: default_tab_size(),
            insert_spaces: true,
            trim_trailing_whitespace: None,
            insert_final_newline: None,
            trim_final_newlines: None,
        }
    }
}

/// The GSQL Style Guide indents the body of a block by 4 spaces.
fn default_tab_size() -> u32 {
    4
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFormattingParams {
    pub text_document: TextDocumentIdentifier,
    #[serde(default)]
    pub options: FormattingOptions,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRangeFormattingParams {
    pub text_document: TextDocumentIdentifier,
    pub range: Range,
    #[serde(default)]
    pub options: FormattingOptions,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRangeParams {
    pub text_document: TextDocumentIdentifier,
    pub positions: Vec<Position>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeActionParams {
    pub text_document: TextDocumentIdentifier,
    pub range: Range,
    #[serde(default)]
    pub context: CodeActionContext,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeActionContext {
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub only: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceSymbolParams {
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceFolder {
    pub uri: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEvent {
    pub uri: String,
    #[serde(rename = "type")]
    pub kind: u8,
}

pub mod file_change {
    pub const CREATED: u8 = 1;
    pub const CHANGED: u8 = 2;
    pub const DELETED: u8 = 3;
}

#[derive(Debug, Clone, Deserialize)]
pub struct DidChangeWatchedFilesParams {
    #[serde(default)]
    pub changes: Vec<FileEvent>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspaceFoldersChangeEvent {
    #[serde(default)]
    pub added: Vec<WorkspaceFolder>,
    #[serde(default)]
    pub removed: Vec<WorkspaceFolder>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DidChangeWorkspaceFoldersParams {
    #[serde(default)]
    pub event: WorkspaceFoldersChangeEvent,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    #[serde(default)]
    pub root_uri: Option<String>,
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default)]
    pub workspace_folders: Option<Vec<WorkspaceFolder>>,
    #[serde(default)]
    pub initialization_options: Option<Value>,
    #[serde(default)]
    pub capabilities: Value,
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

pub mod severity {
    pub const ERROR: u8 = 1;
    pub const WARNING: u8 = 2;
    pub const INFORMATION: u8 = 3;
    pub const HINT: u8 = 4;
}

pub mod diagnostic_tag {
    pub const UNNECESSARY: u8 = 1;
    pub const DEPRECATED: u8 = 2;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub severity: Option<u8>,
    /// LSP allows numbers too (clients send other tools' diagnostics back).
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "string_or_number"
    )]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<Value>,
    /// Other locations that belong to the diagnostic.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub related_information: Vec<RelatedInformation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedInformation {
    pub location: Location,
    pub message: String,
}

fn string_or_number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::String(text)) => Some(text),
        Some(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MarkupContent {
    pub kind: &'static str,
    pub value: String,
}

impl MarkupContent {
    pub fn markdown(value: impl Into<String>) -> MarkupContent {
        MarkupContent {
            kind: "markdown",
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hover {
    pub contents: MarkupContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

pub mod completion_kind {
    pub const TEXT: u8 = 1;
    pub const METHOD: u8 = 2;
    pub const FUNCTION: u8 = 3;
    pub const FIELD: u8 = 5;
    pub const VARIABLE: u8 = 6;
    pub const CLASS: u8 = 7;
    pub const INTERFACE: u8 = 8;
    pub const MODULE: u8 = 9;
    pub const PROPERTY: u8 = 10;
    pub const VALUE: u8 = 12;
    pub const KEYWORD: u8 = 14;
    pub const SNIPPET: u8 = 15;
    pub const FILE: u8 = 17;
    pub const CONSTANT: u8 = 21;
    pub const STRUCT: u8 = 22;
    pub const EVENT: u8 = 23;
    pub const TYPE_PARAMETER: u8 = 25;
}

pub mod insert_text_format {
    pub const PLAIN_TEXT: u8 = 1;
    pub const SNIPPET: u8 = 2;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<MarkupContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_text_format: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_edit: Option<TextEdit>,
}

impl CompletionItem {
    pub fn new(label: impl Into<String>, kind: u8) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            kind: Some(kind),
            detail: None,
            documentation: None,
            sort_text: None,
            filter_text: None,
            insert_text: None,
            insert_text_format: None,
            text_edit: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> CompletionItem {
        let detail = detail.into();
        if !detail.is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    pub fn documentation(
        mut self,
        markdown: impl Into<String>,
    ) -> CompletionItem {
        let markdown = markdown.into();
        if !markdown.is_empty() {
            self.documentation = Some(MarkupContent::markdown(markdown));
        }
        self
    }

    pub fn snippet(mut self, snippet: impl Into<String>) -> CompletionItem {
        self.insert_text = Some(snippet.into());
        self.insert_text_format = Some(insert_text_format::SNIPPET);
        self
    }

    pub fn insert(mut self, text: impl Into<String>) -> CompletionItem {
        self.insert_text = Some(text.into());
        self
    }

    pub fn sort(mut self, sort_text: impl Into<String>) -> CompletionItem {
        self.sort_text = Some(sort_text.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionList {
    pub is_incomplete: bool,
    pub items: Vec<CompletionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceEdit {
    pub changes: std::collections::BTreeMap<String, Vec<TextEdit>>,
}

pub mod symbol_kind {
    pub const FILE: u8 = 1;
    pub const MODULE: u8 = 2;
    pub const NAMESPACE: u8 = 3;
    pub const PACKAGE: u8 = 4;
    pub const CLASS: u8 = 5;
    pub const METHOD: u8 = 6;
    pub const PROPERTY: u8 = 7;
    pub const FIELD: u8 = 8;
    pub const CONSTRUCTOR: u8 = 9;
    pub const ENUM: u8 = 10;
    pub const INTERFACE: u8 = 11;
    pub const FUNCTION: u8 = 12;
    pub const VARIABLE: u8 = 13;
    pub const CONSTANT: u8 = 14;
    pub const STRING: u8 = 15;
    pub const OBJECT: u8 = 19;
    pub const KEY: u8 = 20;
    pub const EVENT: u8 = 24;
    pub const OPERATOR: u8 = 25;
    pub const STRUCT: u8 = 23;
    pub const TYPE_PARAMETER: u8 = 26;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLink {
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyItem {
    pub name: String,
    pub kind: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub uri: String,
    pub range: Range,
    pub selection_range: Range,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallHierarchyCallsParams {
    pub item: CallHierarchyItem,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyIncomingCall {
    pub from: CallHierarchyItem,
    pub from_ranges: Vec<Range>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyOutgoingCall {
    pub to: CallHierarchyItem,
    pub from_ranges: Vec<Range>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbol {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub kind: u8,
    pub range: Range,
    pub selection_range: Range,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<DocumentSymbol>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInformation {
    pub name: String,
    pub kind: u8,
    pub location: Location,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoldingRange {
    pub start_line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelectionRange {
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Box<SelectionRange>>,
}

pub mod highlight_kind {
    pub const TEXT: u8 = 1;
    pub const READ: u8 = 2;
    pub const WRITE: u8 = 3;
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocumentHighlight {
    pub range: Range,
    pub kind: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterInformation {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<MarkupContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInformation {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<MarkupContent>,
    pub parameters: Vec<ParameterInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_parameter: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureInformation>,
    pub active_signature: u32,
    pub active_parameter: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticTokens {
    pub data: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAction {
    pub title: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_preferred: Option<bool>,
    pub edit: WorkspaceEdit,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlayHint {
    pub position: Position,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<u8>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub padding_left: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub padding_right: bool,
}

pub mod inlay_hint_kind {
    pub const TYPE: u8 = 1;
    pub const PARAMETER: u8 = 2;
}

/// JSON-RPC error codes.
pub mod error_code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
    pub const REQUEST_FAILED: i64 = -32803;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numeric_diagnostic_codes() {
        let range = r#"{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}"#;
        for (code, expected) in [
            (r#""unused""#, Some("unused")),
            ("1234", Some("1234")),
            ("null", None),
        ] {
            let json =
                format!(r#"{{"range":{range},"message":"m","code":{code}}}"#);
            let diagnostic: Diagnostic = serde_json::from_str(&json).unwrap();
            assert_eq!(diagnostic.code.as_deref(), expected);
        }
        let json = format!(r#"{{"range":{range},"message":"m"}}"#);
        assert_eq!(
            serde_json::from_str::<Diagnostic>(&json)
                .unwrap()
                .code,
            None
        );
    }
}
