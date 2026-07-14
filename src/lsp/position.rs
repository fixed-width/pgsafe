//! Byte offset → LSP `Position`/`Range` mapping (UTF-16 columns).

use lsp_types::{Position, Range};

/// Maps byte offsets within a document to LSP positions (0-based line, UTF-16
/// code-unit column). Borrows the source text.
pub(crate) struct LineIndex<'a> {
    text: &'a str,
    /// Byte offset of the start of each line (line 0 starts at 0).
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { text, line_starts }
    }

    /// The LSP position for `byte`. Offsets beyond the text length clamp to its
    /// end; a byte that lands inside a multi-byte character floors to that
    /// character's start (pgsafe offsets are always on boundaries, but this keeps
    /// the slice below panic-free regardless).
    pub(crate) fn position(&self, byte: usize) -> Position {
        let mut byte = byte.min(self.text.len());
        while byte > 0 && !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        // Index of the last line whose start is <= byte.
        let line = self
            .line_starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1);
        let line_start = self.line_starts[line];
        let character = self.text[line_start..byte].encode_utf16().count();
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: u32::try_from(character).unwrap_or(u32::MAX),
        }
    }

    /// The LSP range `[start, end)`.
    pub(crate) fn range(&self, start: usize, end: usize) -> Range {
        Range {
            start: self.position(start),
            end: self.position(end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    fn pos(text: &str, byte: usize) -> (u32, u32) {
        let p = LineIndex::new(text).position(byte);
        (p.line, p.character)
    }

    #[test]
    fn ascii_first_line() {
        assert_eq!(pos("hello", 0), (0, 0));
        assert_eq!(pos("hello", 5), (0, 5));
    }

    #[test]
    fn newlines_advance_line_and_reset_column() {
        let t = "ab\ncd\nef";
        assert_eq!(pos(t, 3), (1, 0)); // 'c'
        assert_eq!(pos(t, 5), (1, 2)); // end of "cd"
        assert_eq!(pos(t, 6), (2, 0)); // 'e'
    }

    #[test]
    fn multibyte_utf8_counts_utf16_units() {
        // "café" — 'é' is 2 UTF-8 bytes but 1 UTF-16 unit.
        let t = "café x";
        let byte_of_space = t.find(' ').unwrap(); // = 5 (c,a,f = 3 bytes, é = 2 bytes)
        assert_eq!(pos(t, byte_of_space), (0, 4)); // 4 UTF-16 units before the space
    }

    #[test]
    fn astral_char_is_two_utf16_units() {
        // "😀" is 4 UTF-8 bytes and 2 UTF-16 units (surrogate pair).
        let t = "😀ab";
        let byte_of_a = "😀".len(); // 4
        assert_eq!(pos(t, byte_of_a), (0, 2));
    }

    #[test]
    fn crlf_line_endings() {
        let t = "ab\r\ncd";
        let byte_of_c = t.find('c').unwrap(); // 4
        assert_eq!(pos(t, byte_of_c), (1, 0));
    }

    #[test]
    fn byte_past_end_clamps_to_end() {
        let t = "ab";
        assert_eq!(pos(t, 999), (0, 2));
    }

    #[test]
    fn range_spans_start_to_end() {
        let t = "create table t;";
        let r = LineIndex::new(t).range(0, "create".len());
        assert_eq!((r.start.line, r.start.character), (0, 0));
        assert_eq!((r.end.line, r.end.character), (0, 6));
    }
}
