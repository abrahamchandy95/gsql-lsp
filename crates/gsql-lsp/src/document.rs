//! Open documents: text, syntax tree and incremental updates.

use tree_sitter::{InputEdit, Parser, Tree};

use crate::lsp::types::TextDocumentContentChangeEvent;
use crate::syntax;
use crate::text::{PositionEncoding, SourceText};

/// Largest text (bytes) that is parsed from scratch to get a fresh parse's error recovery;
/// a full parse takes about 50 ms at this size (about 1 s for 5 MB).
const MAX_FRESH_REPARSE_BYTES: usize = 256 * 1024;

pub struct Document {
    pub uri: String,
    pub version: i32,
    pub source: SourceText,
    pub tree: Tree,
}

impl Document {
    pub fn new(uri: String, version: i32, text: String, parser: &mut Parser) -> Document {
        let tree = syntax::parse(parser, &text, None);
        Document { uri, version, source: SourceText::new(text), tree }
    }

    pub fn text(&self) -> &str {
        &self.source.text
    }

    /// Applies LSP content changes in order and reparses incrementally.
    pub fn apply_changes(
        &mut self,
        changes: &[TextDocumentContentChangeEvent],
        version: Option<i32>,
        encoding: PositionEncoding,
        parser: &mut Parser,
    ) {
        self.apply_changes_bounded(changes, version, encoding, parser, MAX_FRESH_REPARSE_BYTES);
    }

    /// `apply_changes` with the size bound of fresh reparses as a parameter.
    fn apply_changes_bounded(
        &mut self,
        changes: &[TextDocumentContentChangeEvent],
        version: Option<i32>,
        encoding: PositionEncoding,
        parser: &mut Parser,
        max_fresh_bytes: usize,
    ) {
        let mut text = std::mem::take(&mut self.source.text);
        let mut lines = self.source.lines.clone();
        let mut incremental = true;
        for change in changes {
            match change.range {
                Some(range) => {
                    let start = lines.offset(&text, range.start, encoding);
                    let old_end = lines.offset(&text, range.end, encoding).max(start);
                    let start_position = lines.point(start);
                    let old_end_position = lines.point(old_end);
                    text.replace_range(start..old_end, &change.text);
                    lines = crate::text::LineIndex::new(&text);
                    let new_end = start + change.text.len();
                    self.tree.edit(&InputEdit {
                        start_byte: start,
                        old_end_byte: old_end,
                        new_end_byte: new_end,
                        start_position,
                        old_end_position,
                        new_end_position: lines.point(new_end),
                    });
                }
                None => {
                    text = change.text.clone();
                    lines = crate::text::LineIndex::new(&text);
                    incremental = false;
                }
            }
        }
        // Incremental reparsing of erroneous input keeps a different (worse) error
        // recovery than a fresh parse, so up to a size bound only clean trees are reused:
        // an old tree with errors is dropped, and an incremental result with errors is
        // parsed again. Clean files of any size stay incremental.
        let bounded = text.len() <= max_fresh_bytes;
        let reuse = incremental && !(bounded && self.tree.root_node().has_error());
        self.tree = if reuse {
            let tree = syntax::parse(parser, &text, Some(&self.tree));
            if bounded && tree.root_node().has_error() { syntax::parse(parser, &text, None) } else { tree }
        } else {
            syntax::parse(parser, &text, None)
        };
        self.source = SourceText::with_lines(text, lines);
        if let Some(version) = version {
            self.version = version;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::types::{Position, Range};
    use crate::text::Span;

    fn change(range: Option<((u32, u32), (u32, u32))>, text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: range.map(|((sl, sc), (el, ec))| Range::new(Position::new(sl, sc), Position::new(el, ec))),
            text: text.to_string(),
        }
    }

    #[test]
    fn incremental_edits_match_a_fresh_parse() {
        let mut parser = syntax::new_parser();
        let mut document =
            Document::new("file:///a.gsql".into(), 1, "CREATE QUERY q() {\n  PRINT 1;\n}\n".into(), &mut parser);
        document.apply_changes(
            &[change(Some(((1, 8), (1, 9))), "x + 2"), change(Some(((1, 2), (1, 2))), "INT x = 1;\n  ")],
            Some(2),
            PositionEncoding::Utf16,
            &mut parser,
        );
        assert_eq!(document.text(), "CREATE QUERY q() {\n  INT x = 1;\n  PRINT x + 2;\n}\n");
        let fresh = syntax::parse(&mut parser, document.text(), None);
        assert_eq!(document.tree.root_node().to_sexp(), fresh.root_node().to_sexp());
        assert!(!document.tree.root_node().has_error());
        assert_eq!(document.version, 2);
    }

    #[test]
    fn lone_carriage_returns_end_lines() {
        let mut parser = syntax::new_parser();
        let mut document =
            Document::new("file:///a.gsql".into(), 1, "CREATE QUERY q() {\r  PRINT 1;\r}\r".into(), &mut parser);
        assert_eq!(document.source.range(Span::new(27, 28), PositionEncoding::Utf16).start, Position::new(1, 8));
        document.apply_changes(&[change(Some(((1, 8), (1, 9))), "2")], Some(2), PositionEncoding::Utf16, &mut parser);
        assert_eq!(document.text(), "CREATE QUERY q() {\r  PRINT 2;\r}\r");
        let fresh = syntax::parse(&mut parser, document.text(), None);
        assert_eq!(document.tree.root_node().to_sexp(), fresh.root_node().to_sexp());
    }

    #[test]
    fn full_replacement() {
        let mut parser = syntax::new_parser();
        let mut document = Document::new("file:///a.gsql".into(), 1, "LS".into(), &mut parser);
        document.apply_changes(&[change(None, "USE GRAPH g")], Some(5), PositionEncoding::Utf16, &mut parser);
        assert_eq!(document.text(), "USE GRAPH g");
        assert_eq!(document.tree.root_node().child(0).unwrap().kind(), "use_statement");
    }

    #[test]
    fn multibyte_edits() {
        let mut parser = syntax::new_parser();
        let mut document = Document::new("file:///a.gsql".into(), 1, "// héllo\nUSE GRAPH g\n".into(), &mut parser);
        // Replace "g" (line 1, UTF-16 column 10) with "social".
        document.apply_changes(
            &[change(Some(((1, 10), (1, 11))), "social")],
            None,
            PositionEncoding::Utf16,
            &mut parser,
        );
        assert_eq!(document.text(), "// héllo\nUSE GRAPH social\n");
        let fresh = syntax::parse(&mut parser, document.text(), None);
        assert_eq!(document.tree.root_node().to_sexp(), fresh.root_node().to_sexp());
    }

    /// Diagnostics of the document's own tree, to compare against a fresh parse.
    fn diagnostics_of(document: &Document) -> Vec<crate::lsp::types::Diagnostic> {
        let mut fixture = crate::features::test_support::Fixture::new(document.text());
        fixture.tree = document.tree.clone();
        fixture.analysis = crate::analysis::analyze(&document.tree, document.text());
        crate::features::diagnostics::diagnostics(&fixture.snapshot())
    }

    fn assert_matches_fresh(document: &Document, parser: &mut Parser, context: &str) {
        let fresh = Document::new("file:///a.gsql".into(), 1, document.text().to_string(), parser);
        assert_eq!(document.tree.root_node().to_sexp(), fresh.tree.root_node().to_sexp(), "{context}");
        assert_eq!(diagnostics_of(document), diagnostics_of(&fresh), "{context}");
    }

    /// LSP position (UTF-16) of a char offset.
    fn position_of(text: &str, char_offset: usize) -> (u32, u32) {
        let (mut line, mut column) = (0, 0);
        for c in text.chars().take(char_offset) {
            if c == '\n' {
                line += 1;
                column = 0;
            } else {
                column += c.len_utf16() as u32;
            }
        }
        (line, column)
    }

    fn char_edit(document: &mut Document, parser: &mut Parser, start: usize, end: usize, new: &str) {
        let len = document.text().chars().count();
        let start = start.min(len);
        let end = end.min(len).max(start);
        let range = (position_of(document.text(), start), position_of(document.text(), end));
        let version = document.version + 1;
        document.apply_changes(&[change(Some(range), new)], Some(version), PositionEncoding::Utf16, parser);
    }

    #[test]
    fn stale_tree_after_edits_creating_errors() {
        let base = include_str!("testdata/stale_tree.gsql");
        let edits: [(usize, usize, &str); 4] =
            [(801, 858, ""), (419, 491, "a\u{1F600}b"), (1163, 1191, "END;"), (326, 383, "}")];
        for crlf in [false, true] {
            let text = if crlf { base.replace('\n', "\r\n") } else { base.to_string() };
            let mut parser = syntax::new_parser();
            let mut document = Document::new("file:///a.gsql".into(), 1, text, &mut parser);
            for (n, (start, end, new)) in edits.iter().enumerate() {
                char_edit(&mut document, &mut parser, *start, *end, new);
                assert_matches_fresh(&document, &mut parser, &format!("crlf={crlf} edit {n}"));
            }
        }
    }

    #[test]
    fn randomized_edits_match_a_fresh_parse() {
        let bases = [
            include_str!("testdata/stale_tree.gsql"),
            "CREATE QUERY q(INT a) FOR GRAPH g {\n  INT x = a;\r\n  // h\u{e9}llo \u{1F600}\n  PRINT x;\n}\n",
        ];
        let snippets = [
            "",
            "}",
            "{",
            ";",
            "END;",
            "a\u{1F600}b",
            "\r\n",
            "\n",
            " SELECT v FROM",
            "INT y = ",
            "(",
            ")",
            "\"",
            "// \u{e9}",
            "WHERE",
            "@@c +=",
            ".",
            "PRINT",
            "ACCUM",
            ",",
        ];
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound.max(1) as u64) as usize
        };
        let mut parser = syntax::new_parser();
        for base in bases {
            for session in 0..12 {
                let mut document = Document::new("file:///a.gsql".into(), 1, base.to_string(), &mut parser);
                for step in 0..8 {
                    let len = document.text().chars().count();
                    let start = next(len + 1);
                    let end = (start + next(40)).min(len);
                    let new = snippets[next(snippets.len())];
                    char_edit(&mut document, &mut parser, start, end, new);
                    assert_matches_fresh(&document, &mut parser, &format!("session {session} step {step}"));
                }
            }
        }
    }

    fn copy_of(document: &Document) -> Document {
        Document {
            uri: document.uri.clone(),
            version: document.version,
            source: document.source.clone(),
            tree: document.tree.clone(),
        }
    }

    /// `char_edit` through the bounded inner function.
    fn bounded_edit(document: &mut Document, parser: &mut Parser, edit: (usize, usize, &str), bound: usize) {
        let len = document.text().chars().count();
        let (start, end) = (edit.0.min(len), edit.1.min(len).max(edit.0.min(len)));
        let range = (position_of(document.text(), start), position_of(document.text(), end));
        document.apply_changes_bounded(&[change(Some(range), edit.2)], None, PositionEncoding::Utf16, parser, bound);
    }

    #[test]
    fn texts_over_the_size_bound_keep_the_incremental_tree() {
        let base = include_str!("testdata/stale_tree.gsql");
        let edits: [(usize, usize, &str); 4] =
            [(801, 858, ""), (419, 491, "a\u{1F600}b"), (1163, 1191, "END;"), (326, 383, "}")];
        let mut parser = syntax::new_parser();
        let mut document = Document::new("file:///a.gsql".into(), 1, base.to_string(), &mut parser);
        let mut over_the_bound_differed = false;
        for edit in edits {
            let mut within = copy_of(&document);
            let mut over = copy_of(&document);
            let mut exact = copy_of(&document);
            bounded_edit(&mut within, &mut parser, edit, usize::MAX);
            let len = within.text().len();
            // The bound is inclusive: a text of exactly `len` bytes is still parsed afresh.
            bounded_edit(&mut over, &mut parser, edit, len - 1);
            bounded_edit(&mut exact, &mut parser, edit, len);
            assert_eq!(over.text(), within.text());
            assert_matches_fresh(&within, &mut parser, "within the bound");
            assert_matches_fresh(&exact, &mut parser, "exactly at the bound");
            let fresh = syntax::parse(&mut parser, over.text(), None);
            over_the_bound_differed |= over.tree.root_node().to_sexp() != fresh.root_node().to_sexp();
            document = within;
        }
        // Over the bound the error recovery of the incremental parse is kept (it differs
        // from a fresh parse for these edits); only the text and positions stay exact.
        assert!(over_the_bound_differed);
    }
}
