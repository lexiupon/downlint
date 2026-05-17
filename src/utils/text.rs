use lsp_types::Position;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }

    pub fn contains(self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

impl From<LspPosition> for Position {
    fn from(value: LspPosition) -> Self {
        Position::new(value.line, value.character)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PositionEncoding {
    Utf8,
    #[default]
    Utf16,
}

#[derive(Debug)]
pub enum PositionError {
    InvalidLine(u32),
    InvalidCharacter(u32),
    NotOnCharBoundary(usize),
}

impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLine(line) => write!(f, "invalid line {line}"),
            Self::InvalidCharacter(character) => write!(f, "invalid character {character}"),
            Self::NotOnCharBoundary(offset) => write!(f, "offset {offset} is not a UTF-8 boundary"),
        }
    }
}

impl std::error::Error for PositionError {}

#[derive(Clone, Debug)]
pub struct LineMap {
    line_starts: Vec<usize>,
    line_ends: Vec<usize>,
}

impl LineMap {
    pub fn new(input: &str) -> Self {
        let mut line_starts = vec![0];
        let mut line_ends = Vec::new();
        let bytes = input.as_bytes();
        let mut idx = 0usize;

        while idx < bytes.len() {
            match bytes[idx] {
                b'\n' => {
                    line_ends.push(idx);
                    idx += 1;
                    if idx <= bytes.len() {
                        line_starts.push(idx);
                    }
                }
                b'\r' => {
                    line_ends.push(idx);
                    idx += 1;
                    if bytes.get(idx) == Some(&b'\n') {
                        idx += 1;
                    }
                    if idx <= bytes.len() {
                        line_starts.push(idx);
                    }
                }
                _ => idx += 1,
            }
        }

        if *line_ends.last().unwrap_or(&usize::MAX) != bytes.len() {
            line_ends.push(bytes.len());
        }

        if *line_starts.last().unwrap_or(&0) > bytes.len() {
            line_starts.push(bytes.len());
        }

        Self {
            line_starts,
            line_ends,
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.line_starts.get(line).copied()
    }

    pub fn line_end(&self, line: usize) -> Option<usize> {
        self.line_ends.get(line).copied()
    }

    pub fn line_range(&self, line: usize) -> Option<ByteRange> {
        Some(ByteRange::new(self.line_start(line)?, self.line_end(line)?))
    }

    pub fn line_for_offset(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx,
            Err(0) => 0,
            Err(idx) => idx - 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Text {
    contents: Arc<String>,
    line_map: Arc<LineMap>,
}

impl Text {
    pub fn new(contents: impl Into<String>) -> Self {
        let contents = contents.into();
        let line_map = LineMap::new(&contents);
        Self {
            contents: Arc::new(contents),
            line_map: Arc::new(line_map),
        }
    }

    pub fn as_str(&self) -> &str {
        self.contents.as_str()
    }

    pub fn len(&self) -> usize {
        self.contents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contents.is_empty()
    }

    pub fn line_map(&self) -> &LineMap {
        self.line_map.as_ref()
    }

    pub fn slice(&self, range: ByteRange) -> &str {
        &self.contents[range.start.min(self.len())..range.end.min(self.len())]
    }

    pub fn line(&self, line: usize) -> Option<&str> {
        let range = self.line_map.line_range(line)?;
        Some(self.slice(range))
    }

    pub fn replace_range(&self, range: ByteRange, replacement: &str) -> Self {
        let mut next = String::with_capacity(self.len() - range.len() + replacement.len());
        next.push_str(&self.contents[..range.start]);
        next.push_str(replacement);
        next.push_str(&self.contents[range.end..]);
        Self::new(next)
    }

    pub fn to_lsp_position(
        &self,
        offset: usize,
        encoding: PositionEncoding,
    ) -> Result<LspPosition, PositionError> {
        if offset > self.len() {
            return Err(PositionError::NotOnCharBoundary(offset));
        }
        if !self.contents.is_char_boundary(offset) {
            return Err(PositionError::NotOnCharBoundary(offset));
        }

        let line = self.line_map.line_for_offset(offset);
        let line_start = self.line_map.line_start(line).unwrap_or(0);
        let prefix = &self.contents[line_start..offset];
        let character = match encoding {
            PositionEncoding::Utf8 => prefix.len() as u32,
            PositionEncoding::Utf16 => prefix.chars().map(char::len_utf16).sum::<usize>() as u32,
        };

        Ok(LspPosition {
            line: line as u32,
            character,
        })
    }

    pub fn byte_offset(
        &self,
        position: &Position,
        encoding: PositionEncoding,
    ) -> Result<usize, PositionError> {
        let line = position.line as usize;
        let line_range = self
            .line_map
            .line_range(line)
            .ok_or(PositionError::InvalidLine(position.line))?;
        let line_text = self.slice(line_range);
        let line_start = line_range.start;

        let offset_in_line = match encoding {
            PositionEncoding::Utf8 => position.character as usize,
            PositionEncoding::Utf16 => {
                let mut utf16_units = 0usize;
                let mut bytes = 0usize;
                for ch in line_text.chars() {
                    if utf16_units == position.character as usize {
                        break;
                    }
                    let width = ch.len_utf16();
                    if utf16_units + width > position.character as usize {
                        return Err(PositionError::InvalidCharacter(position.character));
                    }
                    utf16_units += width;
                    bytes += ch.len_utf8();
                }
                if utf16_units != position.character as usize {
                    return Err(PositionError::InvalidCharacter(position.character));
                }
                bytes
            }
        };

        let offset = line_start + offset_in_line;
        if offset > self.len() || !self.contents.is_char_boundary(offset) {
            return Err(PositionError::InvalidCharacter(position.character));
        }
        Ok(offset)
    }
}

#[derive(Clone, Debug)]
pub struct TextEditChange {
    pub range: ByteRange,
    pub replacement: String,
}

impl TextEditChange {
    pub fn apply_all(text: &Text, changes: &[Self]) -> Text {
        let mut ordered = changes.to_vec();
        ordered.sort_by_key(|change| std::cmp::Reverse(change.range.start));
        let mut current = text.clone();
        for change in ordered {
            current = current.replace_range(change.range, &change.replacement);
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_round_trip_handles_emoji() {
        let text = Text::new("a\n🚀 test");
        let offset = "a\n🚀".len();
        let position = text
            .to_lsp_position(offset, PositionEncoding::Utf16)
            .unwrap();
        assert_eq!(position.line, 1);
        assert_eq!(position.character, 2);
        assert_eq!(
            text.byte_offset(
                &Position::new(position.line, position.character),
                PositionEncoding::Utf16
            )
            .unwrap(),
            offset
        );
    }
}
