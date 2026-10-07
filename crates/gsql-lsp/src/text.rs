//! Conversions between byte offsets, tree-sitter points and LSP positions.

use crate::lsp::types::{Position, Range};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    pub fn of(node: tree_sitter::Node) -> Span {
        Span { start: node.start_byte(), end: node.end_byte() }
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
        let row_starts = lone_carriage_return
            .then(|| std::iter::once(0).chain(text.match_indices('\n').map(|(index, _)| index + 1)).collect());
        let mut marks = Vec::new();
        for (line, &line_ascii) in ascii.iter().enumerate() {
            if line_ascii {
                continue;
            }
            let start = line_starts[line];
            let end = line_starts.get(line + 1).copied().unwrap_or(text.len());
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
        LineIndex { line_starts, ascii, row_starts, marks, len: text.len() }
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
        self.line_starts.get(line).copied().unwrap_or(self.len)
    }

    /// End of the line's content, excluding the line terminator.
    pub fn line_end(&self, text: &str, line: usize) -> usize {
        let next = self.line_starts.get(line + 1).copied().unwrap_or(self.len);
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
        self.line_starts.partition_point(|&start| start <= offset).saturating_sub(1)
    }

    /// Converts an LSP position to a byte offset, clamping out-of-range values.
    pub fn offset(&self, text: &str, position: Position, encoding: PositionEncoding) -> usize {
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
    pub fn position(&self, text: &str, offset: usize, encoding: PositionEncoding) -> Position {
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
                let (from, before) = match marks.iter().rev().find(|m| m.0 <= offset) {
                    Some(mark) => (mark.0, if encoding == PositionEncoding::Utf16 { mark.1 } else { mark.2 }),
                    None => (start, 0),
                };
                before + text[from..offset].chars().map(|c| encoding.units(c)).sum::<usize>()
            }
        };
        Position::new(line as u32, character as u32)
    }

    pub fn range(&self, text: &str, span: Span, encoding: PositionEncoding) -> Range {
        Range::new(self.position(text, span.start, encoding), self.position(text, span.end, encoding))
    }

    pub fn span(&self, text: &str, range: Range, encoding: PositionEncoding) -> Span {
        Span::new(self.offset(text, range.start, encoding), self.offset(text, range.end, encoding))
    }

    /// The tree-sitter point (row, byte column) of an offset.
    pub fn point(&self, offset: usize) -> tree_sitter::Point {
        let starts = self.row_starts.as_ref().unwrap_or(&self.line_starts);
        let row = starts.partition_point(|&start| start <= offset).saturating_sub(1);
        tree_sitter::Point::new(row, offset - starts[row])
    }
}

/// A document's text together with its line index.
#[derive(Debug, Clone)]
pub struct SourceText {
    pub text: String,
    pub lines: LineIndex,
    /// Comment-blanked copy of `text`, computed on first use (see `completion::code_text`).
    pub blanked: std::sync::OnceLock<String>,
}

impl SourceText {
    pub fn new(text: String) -> SourceText {
        let lines = LineIndex::new(&text);
        SourceText::with_lines(text, lines)
    }

    pub fn with_lines(text: String, lines: LineIndex) -> SourceText {
        SourceText { text, lines, blanked: std::sync::OnceLock::new() }
    }

    pub fn range(&self, span: Span, encoding: PositionEncoding) -> Range {
        self.lines.range(&self.text, span, encoding)
    }

    pub fn offset(&self, position: Position, encoding: PositionEncoding) -> usize {
        self.lines.offset(&self.text, position, encoding)
    }

    pub fn position(&self, offset: usize, encoding: PositionEncoding) -> Position {
        self.lines.position(&self.text, offset, encoding)
    }

    pub fn slice(&self, span: Span) -> &str {
        self.text.get(span.start..span.end).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_positions_on_long_non_ascii_lines() {
        // Checkpoints must not change any answer: compare with a plain scan.
        let text = format!("{}\n{}", "a\u{e9}\u{1f600}b".repeat(400), "x".repeat(300));
        let index = LineIndex::new(&text);
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16, PositionEncoding::Utf32] {
            let mut character = 0;
            for (offset, c) in text.char_indices().take_while(|(_, c)| *c != '\n') {
                let position = index.position(&text, offset, encoding);
                assert_eq!(position, Position::new(0, character as u32), "{encoding:?} at {offset}");
                assert_eq!(index.offset(&text, position, encoding), offset, "{encoding:?} at {offset}");
                character += encoding.units(c);
            }
        }
    }

    #[test]
    fn converts_ascii_positions() {
        let text = "ab\ncde\n";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(1, 2), PositionEncoding::Utf16), 5);
        assert_eq!(index.position(text, 5, PositionEncoding::Utf16), Position::new(1, 2));
        assert_eq!(index.offset(text, Position::new(1, 99), PositionEncoding::Utf16), 6);
        assert_eq!(index.offset(text, Position::new(9, 0), PositionEncoding::Utf16), text.len());
        assert_eq!(index.position(text, text.len(), PositionEncoding::Utf16), Position::new(2, 0));
    }

    #[test]
    fn converts_multibyte_positions() {
        // "é" is 2 UTF-8 bytes / 1 UTF-16 unit, "𝄞" is 4 bytes / 2 units.
        let text = "é𝄞x";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(0, 3), PositionEncoding::Utf16), 6);
        assert_eq!(index.position(text, 6, PositionEncoding::Utf16), Position::new(0, 3));
        assert_eq!(index.position(text, 6, PositionEncoding::Utf8), Position::new(0, 6));
        assert_eq!(index.position(text, 6, PositionEncoding::Utf32), Position::new(0, 2));
        // A position inside a surrogate pair is clamped to the character start.
        assert_eq!(index.offset(text, Position::new(0, 2), PositionEncoding::Utf16), 2);
    }

    #[test]
    fn handles_crlf() {
        let text = "a\r\nb";
        let index = LineIndex::new(text);
        assert_eq!(index.line_end(text, 0), 1);
        assert_eq!(index.offset(text, Position::new(0, 5), PositionEncoding::Utf16), 1);
        assert_eq!(index.offset(text, Position::new(1, 1), PositionEncoding::Utf16), 4);
    }
}
