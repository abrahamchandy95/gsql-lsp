//! Conversions between byte offsets, tree-sitter points and LSP positions, and
//! lexical helpers over raw text.

use crate::lsp::types::{Position, Range, TextEdit};

/// How the `character` component of an LSP position is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionEncoding {
    Utf8,
    #[default]
    Utf16,
    Utf32,
}

impl PositionEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            PositionEncoding::Utf8 => "utf-8",
            PositionEncoding::Utf16 => "utf-16",
            PositionEncoding::Utf32 => "utf-32",
        }
    }

    pub fn from_name(name: &str) -> Option<PositionEncoding> {
        match name {
            "utf-8" => Some(PositionEncoding::Utf8),
            "utf-16" => Some(PositionEncoding::Utf16),
            "utf-32" => Some(PositionEncoding::Utf32),
            _ => None,
        }
    }

    fn units(self, c: char) -> usize {
        match self {
            PositionEncoding::Utf8 => c.len_utf8(),
            PositionEncoding::Utf16 => c.len_utf16(),
            PositionEncoding::Utf32 => 1,
        }
    }
}

/// A half-open byte range into a document.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    pub fn of(node: tree_sitter::Node) -> Span {
        Span {
            start: node.start_byte(),
            end: node.end_byte(),
        }
    }

    pub fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset <= self.end
    }

    pub fn contains_span(&self, other: Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Line start offsets for a text, used for position conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    /// Where each line starts. Lines end with `\n`, `\r\n` or a lone `\r`,
    /// as in LSP.
    line_starts: Vec<usize>,
    /// Per line: whether it is pure ASCII (columns are byte offsets in every encoding).
    ascii: Vec<bool>,
    /// Where each tree-sitter row starts, when they differ from the lines:
    /// tree-sitter breaks rows only at `\n`, not at a lone `\r`.
    row_starts: Option<Vec<usize>>,
    /// Checkpoints along the non-ASCII lines, every `MARK_SPACING` bytes or
    /// so: (byte offset, UTF-16 units, UTF-32 units) counted from the start
    /// of the line. Converting a position then costs a short scan, not a
    /// scan from the start of a (possibly huge) line.
    marks: Vec<(usize, usize, usize)>,
    len: usize,
}

const MARK_SPACING: usize = 128;

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0];
        let mut ascii = Vec::new();
        let mut line_ascii = true;
        let mut lone_carriage_return = false;
        let bytes = text.as_bytes();
        for (index, &byte) in bytes.iter().enumerate() {
            line_ascii &= byte.is_ascii();
            let lone = byte == b'\r' && bytes.get(index + 1) != Some(&b'\n');
            if byte == b'\n' || lone {
                lone_carriage_return |= lone;
                line_starts.push(index + 1);
                ascii.push(line_ascii);
                line_ascii = true;
            }
        }
        ascii.push(line_ascii);
        let row_starts = lone_carriage_return.then(|| {
            std::iter::once(0)
                .chain(
                    text.match_indices('\n')
                        .map(|(index, _)| index + 1),
                )
                .collect()
        });
        let mut marks = Vec::new();
        for (line, &line_ascii) in ascii.iter().enumerate() {
            if line_ascii {
                continue;
            }
            let start = line_starts[line];
            let end = line_starts
                .get(line + 1)
                .copied()
                .unwrap_or(text.len());
            let (mut utf16, mut utf32, mut last) = (0, 0, start);
            for (offset, c) in text[start..end].char_indices() {
                if start + offset >= last + MARK_SPACING {
                    marks.push((start + offset, utf16, utf32));
                    last = start + offset;
                }
                utf16 += c.len_utf16();
                utf32 += 1;
            }
        }
        LineIndex {
            line_starts,
            ascii,
            row_starts,
            marks,
            len: text.len(),
        }
    }

    /// The checkpoints inside `start..end`.
    fn marks_in(&self, start: usize, end: usize) -> &[(usize, usize, usize)] {
        let from = self.marks.partition_point(|m| m.0 < start);
        let to = self.marks.partition_point(|m| m.0 < end);
        &self.marks[from..to]
    }

    /// Whether some line ends with a lone `\r`, so that tree-sitter rows are
    /// not lines.
    pub fn has_lone_carriage_returns(&self) -> bool {
        self.row_starts.is_some()
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts
            .get(line)
            .copied()
            .unwrap_or(self.len)
    }

    /// End of the line's content, excluding the line terminator.
    pub fn line_end(&self, text: &str, line: usize) -> usize {
        let next = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.len);
        let bytes = text.as_bytes();
        let mut end = next;
        if end > self.line_start(line) && bytes.get(end - 1) == Some(&b'\n') {
            end -= 1;
        }
        if end > self.line_start(line) && bytes.get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
        end
    }

    pub fn line_of(&self, offset: usize) -> usize {
        self.line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
    }

    /// Converts an LSP position to a byte offset, clamping out-of-range values.
    pub fn offset(
        &self,
        text: &str,
        position: Position,
        encoding: PositionEncoding,
    ) -> usize {
        let line = position.line as usize;
        if line >= self.line_starts.len() {
            return self.len;
        }
        let start = self.line_start(line);
        let end = self.line_end(text, line);
        if self.ascii[line] {
            return (start + position.character as usize).min(end);
        }
        let mut remaining = position.character as usize;
        let mut offset = start;
        // Begin at the last checkpoint that does not pass the position.
        let units_at = |m: &(usize, usize, usize)| match encoding {
            PositionEncoding::Utf8 => m.0 - start,
            PositionEncoding::Utf16 => m.1,
            PositionEncoding::Utf32 => m.2,
        };
        let marks = self.marks_in(start, end);
        let reached = marks.partition_point(|m| units_at(m) <= remaining);
        if let Some(mark) = reached.checked_sub(1).map(|i| &marks[i]) {
            remaining -= units_at(mark);
            offset = mark.0;
        }
        for c in text[offset..end].chars() {
            if remaining == 0 {
                break;
            }
            let units = encoding.units(c);
            if units > remaining {
                break;
            }
            remaining -= units;
            offset += c.len_utf8();
        }
        offset
    }

    /// Converts a byte offset to an LSP position.
    pub fn position(
        &self,
        text: &str,
        offset: usize,
        encoding: PositionEncoding,
    ) -> Position {
        let mut offset = offset.min(self.len);
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.line_of(offset);
        let start = self.line_start(line);
        let character: usize = match encoding {
            PositionEncoding::Utf8 => offset - start,
            _ if self.ascii[line] => offset - start,
            _ => {
                let marks = self.marks_in(start, offset + 1);
                let (from, before) =
                    match marks.iter().rev().find(|m| m.0 <= offset) {
                        Some(mark) => (
                            mark.0,
                            if encoding == PositionEncoding::Utf16 {
                                mark.1
                            } else {
                                mark.2
                            },
                        ),
                        None => (start, 0),
                    };
                before
                    + text[from..offset]
                        .chars()
                        .map(|c| encoding.units(c))
                        .sum::<usize>()
            }
        };
        Position::new(line as u32, character as u32)
    }

    pub fn range(
        &self,
        text: &str,
        span: Span,
        encoding: PositionEncoding,
    ) -> Range {
        Range::new(
            self.position(text, span.start, encoding),
            self.position(text, span.end, encoding),
        )
    }

    pub fn span(
        &self,
        text: &str,
        range: Range,
        encoding: PositionEncoding,
    ) -> Span {
        Span::new(
            self.offset(text, range.start, encoding),
            self.offset(text, range.end, encoding),
        )
    }

    /// The tree-sitter point (row, byte column) of an offset.
    pub fn point(&self, offset: usize) -> tree_sitter::Point {
        let starts = self
            .row_starts
            .as_ref()
            .unwrap_or(&self.line_starts);
        let row = starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        tree_sitter::Point::new(row, offset - starts[row])
    }
}

/// A document's text together with its line index.
#[derive(Debug, Clone)]
pub struct SourceText {
    pub text: String,
    pub lines: LineIndex,
    /// Comment-blanked copies of `text` for `code_text` and `parsed_code_text`.
    blanked: std::sync::OnceLock<String>,
    parsed_blanked: std::sync::OnceLock<String>,
}

impl SourceText {
    pub fn new(text: String) -> SourceText {
        let lines = LineIndex::new(&text);
        SourceText::with_lines(text, lines)
    }

    pub fn with_lines(text: String, lines: LineIndex) -> SourceText {
        SourceText {
            text,
            lines,
            blanked: std::sync::OnceLock::new(),
            parsed_blanked: std::sync::OnceLock::new(),
        }
    }

    pub fn range(&self, span: Span, encoding: PositionEncoding) -> Range {
        self.lines.range(&self.text, span, encoding)
    }

    pub fn offset(
        &self,
        position: Position,
        encoding: PositionEncoding,
    ) -> usize {
        self.lines
            .offset(&self.text, position, encoding)
    }

    pub fn position(
        &self,
        offset: usize,
        encoding: PositionEncoding,
    ) -> Position {
        self.lines
            .position(&self.text, offset, encoding)
    }

    pub fn slice(&self, span: Span) -> &str {
        self.text
            .get(span.start..span.end)
            .unwrap_or("")
    }

    /// The line break for new text: `\r\n` if the first line ends with one, else `\n`.
    pub fn newline(&self) -> &'static str {
        match self.text.find(['\r', '\n']) {
            Some(at) if self.text[at..].starts_with("\r\n") => "\r\n",
            _ => "\n",
        }
    }

    /// The text with comments blanked out (same byte offsets, line breaks kept).
    pub fn code_text(&self) -> &str {
        self.blanked.get_or_init(|| {
            let spans = noncode_spans(&self.text);
            let bytes = self.text.as_bytes();
            blank(
                &self.text,
                spans
                    .into_iter()
                    .filter(|s| bytes[s.start] != b'"'),
            )
        })
    }

    /// The text with the `comments` spans of its parse tree blanked out; only the
    /// first call runs `comments`.
    pub fn parsed_code_text(
        &self,
        comments: impl FnOnce() -> Vec<Span>,
    ) -> &str {
        self.parsed_blanked
            .get_or_init(|| blank(&self.text, comments()))
    }

    /// The text with `edits` applied (LSP edits of this document, not overlapping).
    pub fn apply_edits(
        &self,
        edits: &[TextEdit],
        encoding: PositionEncoding,
    ) -> String {
        let mut spans: Vec<(usize, usize, usize, &str)> = edits
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let start = self.offset(e.range.start, encoding);
                let end = self.offset(e.range.end, encoding);
                (start, end, i, e.new_text.as_str())
            })
            .collect();
        // Right to left; at one position, later edits first to keep array order.
        spans.sort_by_key(|&(start, end, i, _)| {
            std::cmp::Reverse((start, end, i))
        });
        let mut text = self.text.clone();
        for (start, end, _, new_text) in spans {
            text.replace_range(start..end, new_text);
        }
        text
    }
}

/// `text` with `spans` blanked out (same byte offsets, line breaks kept).
fn blank(text: &str, spans: impl IntoIterator<Item = Span>) -> String {
    let mut out = text.as_bytes().to_vec();
    for span in spans {
        out[span.start..span.end]
            .iter_mut()
            .filter(|b| **b != b'\n')
            .for_each(|b| *b = b' ');
    }
    String::from_utf8(out).expect("only whole characters are blanked")
}

/// Where the string or comment at `index` ends, and whether it is an unclosed block
/// comment; `None` for code. A string ends at its closing quote or its line end.
pub fn noncode_end(
    bytes: &[u8],
    index: usize,
    end: usize,
) -> Option<(usize, bool)> {
    let line_end = || {
        bytes[index..end]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(end, |n| index + n)
    };
    let next = if index + 1 < end { bytes[index + 1] } else { 0 };
    match bytes[index] {
        b'"' => {
            let mut at = index + 1;
            while at < end {
                match bytes[at] {
                    b'\\' => at += 2,
                    b'"' => return Some((at + 1, false)),
                    b'\n' => return Some((at, false)),
                    _ => at += 1,
                }
            }
            Some((end, false))
        }
        b'#' => Some((line_end(), false)),
        b'/' if next == b'/' => Some((line_end(), false)),
        b'/' if next == b'*' => {
            let mut at = index + 2;
            let mut previous = b' ';
            while at < end {
                let c = bytes[at];
                at += 1;
                if previous == b'*' && c == b'/' {
                    return Some((at, false));
                }
                previous = c;
            }
            Some((end, true))
        }
        _ => None,
    }
}

/// Byte ranges of the strings and comments of `text`, in order.
pub fn noncode_spans(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    noncode_until(text, 0, text.len(), &mut spans);
    spans
}

/// Adds the strings and comments from `index` to `end` to `spans` and returns
/// where reading stopped: past `end` for a `/*` not closed by then.
pub fn noncode_until(
    text: &str,
    mut index: usize,
    end: usize,
    spans: &mut Vec<Span>,
) -> usize {
    let bytes = text.as_bytes();
    while index < end {
        match noncode_end(bytes, index, end) {
            Some((mut stop, unclosed)) => {
                if unclosed {
                    stop = noncode_end(bytes, index, bytes.len())
                        .map_or(stop, |e| e.0);
                }
                spans.push(Span::new(index, stop));
                index = stop;
            }
            None => index += 1,
        }
    }
    index
}

/// Byte ranges of the strings, backtick names and comments of the openCypher text
/// at `span`: a `"` or `'` not closed on its line is plain, and `#` starts no comment.
pub fn cypher_literals(text: &str, span: Span) -> Vec<Span> {
    let bytes = &text.as_bytes()[..span.end];
    let line_end = |from: usize| {
        bytes[from..]
            .iter()
            .position(|&b| matches!(b, b'\r' | b'\n'))
            .map_or(bytes.len(), |n| from + n)
    };
    let after = |from: usize, close: &[u8]| {
        bytes[from..]
            .windows(close.len())
            .position(|w| w == close)
            .map(|n| from + n + close.len())
    };
    let (mut spans, mut index) = (Vec::new(), span.start);
    while index < bytes.len() {
        let end = match (bytes[index], bytes.get(index + 1)) {
            (quote @ (b'"' | b'\''), _) => {
                // A `\` escapes any character but a line break.
                let line = &bytes[index + 1..line_end(index + 1)];
                let mut at = 0;
                while at < line.len() && line[at] != quote {
                    at += if line[at] == b'\\' { 2 } else { 1 };
                }
                (at < line.len()).then_some(index + at + 2)
            }
            (b'`', _) => after(index + 1, b"`"),
            (b'/', Some(b'/')) => Some(line_end(index)),
            (b'/', Some(b'*')) => after(index + 2, b"*/"),
            _ => None,
        };
        match end {
            Some(end) => {
                spans.push(Span::new(index, end));
                index = end;
            }
            None => index += 1,
        }
    }
    spans
}

/// A character of a GSQL name: ASCII letter, digit or `_`.
pub fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A whole GSQL name (`[A-Za-z_][A-Za-z0-9_]*`), reserved words included.
pub fn is_identifier(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(is_word_char)
}

/// The byte offsets where `word` occurs in `text` as a whole word, case-sensitively.
pub fn word_offsets<'a>(
    text: &'a str,
    word: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(word)
        .map(|(i, _)| i)
        .filter(move |&i| {
            !text[..i].ends_with(is_word_char)
                && !text[i + word.len()..].starts_with(is_word_char)
        })
}

/// Whether `s` starts with `prefix`, ignoring ASCII case (byte-wise, so never panics).
pub fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Whether `s` ends with `suffix`, ignoring ASCII case (byte-wise, so never panics).
pub fn ends_with_ignore_ascii_case(s: &str, suffix: &str) -> bool {
    s.len()
        .checked_sub(suffix.len())
        .is_some_and(|start| {
            s.as_bytes()[start..].eq_ignore_ascii_case(suffix.as_bytes())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_positions_on_long_non_ascii_lines() {
        // Checkpoints must not change any answer: compare with a plain scan.
        let text = format!(
            "{}\n{}",
            "a\u{e9}\u{1f600}b".repeat(400),
            "x".repeat(300)
        );
        let index = LineIndex::new(&text);
        for encoding in [
            PositionEncoding::Utf8,
            PositionEncoding::Utf16,
            PositionEncoding::Utf32,
        ] {
            let mut character = 0;
            for (offset, c) in text
                .char_indices()
                .take_while(|(_, c)| *c != '\n')
            {
                let position = index.position(&text, offset, encoding);
                assert_eq!(
                    position,
                    Position::new(0, character as u32),
                    "{encoding:?} at {offset}"
                );
                assert_eq!(
                    index.offset(&text, position, encoding),
                    offset,
                    "{encoding:?} at {offset}"
                );
                character += encoding.units(c);
            }
        }
    }

    #[test]
    fn converts_ascii_positions() {
        let text = "ab\ncde\n";
        let index = LineIndex::new(text);
        assert_eq!(
            index.offset(text, Position::new(1, 2), PositionEncoding::Utf16),
            5
        );
        assert_eq!(
            index.position(text, 5, PositionEncoding::Utf16),
            Position::new(1, 2)
        );
        assert_eq!(
            index.offset(text, Position::new(1, 99), PositionEncoding::Utf16),
            6
        );
        assert_eq!(
            index.offset(text, Position::new(9, 0), PositionEncoding::Utf16),
            text.len()
        );
        assert_eq!(
            index.position(text, text.len(), PositionEncoding::Utf16),
            Position::new(2, 0)
        );
    }

    #[test]
    fn converts_multibyte_positions() {
        // "é" is 2 UTF-8 bytes / 1 UTF-16 unit, "𝄞" is 4 bytes / 2 units.
        let text = "é𝄞x";
        let index = LineIndex::new(text);
        assert_eq!(
            index.offset(text, Position::new(0, 3), PositionEncoding::Utf16),
            6
        );
        assert_eq!(
            index.position(text, 6, PositionEncoding::Utf16),
            Position::new(0, 3)
        );
        assert_eq!(
            index.position(text, 6, PositionEncoding::Utf8),
            Position::new(0, 6)
        );
        assert_eq!(
            index.position(text, 6, PositionEncoding::Utf32),
            Position::new(0, 2)
        );
        // A position inside a surrogate pair is clamped to the character start.
        assert_eq!(
            index.offset(text, Position::new(0, 2), PositionEncoding::Utf16),
            2
        );
    }

    #[test]
    fn handles_crlf() {
        let text = "a\r\nb";
        let index = LineIndex::new(text);
        assert_eq!(index.line_end(text, 0), 1);
        assert_eq!(
            index.offset(text, Position::new(0, 5), PositionEncoding::Utf16),
            1
        );
        assert_eq!(
            index.offset(text, Position::new(1, 1), PositionEncoding::Utf16),
            4
        );
    }

    #[test]
    fn takes_the_newline_from_the_first_line_break() {
        let newline =
            |text: &str| SourceText::new(text.to_string()).newline();
        assert_eq!(newline("a\r\nb\nc"), "\r\n");
        assert_eq!(newline("a\nb\r\nc"), "\n");
        assert_eq!(newline("a\rb\r\nc"), "\n");
        assert_eq!(newline("a"), "\n");
        assert_eq!(newline(""), "\n");
    }

    fn spans(pairs: &[(usize, usize)]) -> Vec<Span> {
        pairs
            .iter()
            .map(|&(start, end)| Span::new(start, end))
            .collect()
    }

    #[test]
    fn blanks_comments_but_keeps_strings() {
        let source =
            SourceText::new("a # x\nb // y\n\"#s\" /* z\n*/ c".to_string());
        assert_eq!(source.code_text(), "a    \nb     \n\"#s\"     \n   c");
        assert_eq!(
            noncode_spans(&source.text),
            spans(&[(2, 5), (8, 12), (13, 17), (18, 25)])
        );
    }

    #[test]
    fn reads_a_string_to_its_line() {
        // A string broken by a `#` line, then an unclosed string before a `//` line.
        let text = "\"a\n# b\" c # d\n\"open\n// e";
        let source = SourceText::new(text.to_string());
        assert_eq!(source.code_text(), "\"a\n          \n\"open\n    ");
        assert_eq!(
            noncode_spans(text),
            spans(&[(0, 2), (3, 13), (14, 19), (20, 24)])
        );
        // A `"""` string ends at an inner quote, so its `//` starts a comment.
        let source = SourceText::new(
            "\"\"\"{\"u\": \"s3://b\"}\"\"\" # c\nx".to_string(),
        );
        assert_eq!(source.code_text(), "\"\"\"{\"u\": \"s3:            \nx");
        let source = SourceText::new("\"\"\"a # b\n# c".to_string());
        assert_eq!(source.code_text(), "\"\"\"a # b\n   ");
    }

    #[test]
    fn a_block_comment_not_closed_by_the_end_runs_on() {
        for (text, stop) in
            [("a /* b \"c\" # d\ne", 16), ("a /* b \"c\" */ e", 13)]
        {
            let mut found = Vec::new();
            assert_eq!(noncode_until(text, 0, 7, &mut found), stop);
            assert_eq!(found, spans(&[(2, stop)]));
        }
    }

    #[test]
    fn reads_opencypher_literals_as_the_grammar_does() {
        // A `"` in `'...'`, a `"` not closed on its line, a name over lines, and `#`.
        let text = "{ WHERE u.n = 'O\"Reilly' AND u.m = \"a\nb\" // c\n\
                    RETURN `x\ny`, \"\\\"\" /* d */ # e }";
        let found: Vec<&str> =
            cypher_literals(text, Span::new(1, text.len() - 1))
                .into_iter()
                .map(|s| &text[s.start..s.end])
                .collect();
        assert_eq!(
            found,
            ["'O\"Reilly'", "// c", "`x\ny`", "\"\\\"\"", "/* d */"]
        );
    }

    #[test]
    fn applies_edits_in_array_order_at_one_position() {
        let source = SourceText::new("ab".to_string());
        let insert = |text: &str| TextEdit {
            range: Range::new(Position::new(0, 1), Position::new(0, 1)),
            new_text: text.to_string(),
        };
        let edits = [insert("x"), insert("y")];
        assert_eq!(
            source.apply_edits(&edits, PositionEncoding::Utf16),
            "axyb"
        );
        // Inserts before a replacement that starts at the same position.
        let replace = TextEdit {
            range: Range::new(Position::new(0, 1), Position::new(0, 2)),
            new_text: "Z".to_string(),
        };
        let edits = [insert("x"), insert("y"), replace];
        assert_eq!(
            source.apply_edits(&edits, PositionEncoding::Utf16),
            "axyZ"
        );
    }

    #[test]
    fn finds_whole_words() {
        assert_eq!(
            word_offsets("create created_by create", "create")
                .collect::<Vec<_>>(),
            [0, 18]
        );
        assert!(
            is_identifier("_a1")
                && !is_identifier("1a")
                && !is_identifier("")
        );
    }

    #[test]
    fn ignores_ascii_case_without_slicing_characters() {
        assert!(starts_with_ignore_ascii_case("CREATE query", "create"));
        assert!(ends_with_ignore_ascii_case("x.Accum", "accum"));
        assert!(!ends_with_ignore_ascii_case(
            "\"日本語日本語x\"",
            "USER_DEFINED_HEADER"
        ));
        assert!(!starts_with_ignore_ascii_case("日本", "ab"));
    }
}
