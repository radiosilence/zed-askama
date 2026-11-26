//! Byte offsets to protocol positions and back.
//!
//! The analysis works in byte offsets, as askama and rust-analyzer's own internals do. The
//! protocol counts columns in UTF-8 or UTF-16 code units, whichever the client and server
//! agreed at initialisation.

use std::ops::Range;

use line_index::{LineCol, LineIndex, TextSize, WideEncoding, WideLineCol};
use lsp_types::{Position, PositionEncodingKind};

/// How the protocol counts columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    /// UTF-8 when the client offers it, since then no conversion is needed; otherwise UTF-16,
    /// which every client supports.
    pub fn negotiate(offered: Option<&[PositionEncodingKind]>) -> Self {
        if offered.is_some_and(|kinds| kinds.contains(&PositionEncodingKind::UTF8)) {
            Self::Utf8
        } else {
            Self::Utf16
        }
    }

    pub fn kind(self) -> PositionEncodingKind {
        match self {
            Self::Utf8 => PositionEncodingKind::UTF8,
            Self::Utf16 => PositionEncodingKind::UTF16,
        }
    }
}

/// A text's lines, for converting between offsets and positions.
pub struct Lines<'a> {
    text: &'a str,
    index: LineIndex,
    encoding: Encoding,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str, encoding: Encoding) -> Self {
        Self {
            text,
            index: LineIndex::new(text),
            encoding,
        }
    }

    /// The position of a byte offset, clamped to the text and moved back to a character
    /// boundary.
    pub fn position(&self, offset: usize) -> Position {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line_col = self.index.line_col(TextSize::new(offset_u32(offset)));
        match self.encoding {
            Encoding::Utf8 => Position::new(line_col.line, line_col.col),
            Encoding::Utf16 => {
                let wide =
                    self.index
                        .to_wide(WideEncoding::Utf16, line_col)
                        .unwrap_or(WideLineCol {
                            line: line_col.line,
                            col: line_col.col,
                        });
                Position::new(wide.line, wide.col)
            }
        }
    }

    pub fn range(&self, range: &Range<usize>) -> lsp_types::Range {
        lsp_types::Range::new(self.position(range.start), self.position(range.end))
    }

    /// The byte offset of a position, or `None` when it is not in the text.
    pub fn offset(&self, position: Position) -> Option<usize> {
        let line_col = match self.encoding {
            Encoding::Utf8 => LineCol {
                line: position.line,
                col: position.character,
            },
            Encoding::Utf16 => self.index.to_utf8(
                WideEncoding::Utf16,
                WideLineCol {
                    line: position.line,
                    col: position.character,
                },
            )?,
        };
        let offset = usize::from(self.index.offset(line_col)?);
        (offset <= self.text.len()).then_some(offset)
    }
}

/// Offsets beyond `u32` cannot occur: files are capped far below 4 GiB on read.
fn offset_u32(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Encoding, Lines};
    use lsp_types::Position;

    #[test]
    fn utf16_columns_count_a_wide_character_as_two() {
        let text = "a😀b\nc";
        let lines = Lines::new(text, Encoding::Utf16);
        let b = text.find('b').expect("fixture holds b");
        assert_eq!(
            lines.position(b),
            Position::new(0, 3),
            "😀 is two UTF-16 units"
        );
        assert_eq!(
            lines.offset(Position::new(0, 3)),
            Some(b),
            "and converts back"
        );
    }

    #[test]
    fn utf8_columns_count_bytes() {
        let text = "é{{ x }}";
        let lines = Lines::new(text, Encoding::Utf8);
        let x = text.find('x').expect("fixture holds x");
        assert_eq!(lines.position(x), Position::new(0, 5), "é is two bytes");
    }

    #[test]
    fn an_offset_inside_a_character_moves_back_to_its_start() {
        let text = "😀";
        let lines = Lines::new(text, Encoding::Utf16);
        assert_eq!(
            lines.position(2),
            Position::new(0, 0),
            "no position inside a character"
        );
    }
}
