use tower_lsp::lsp_types::{Position, Range};

/// Convert LSP Position to byte offset in text
pub fn position_to_offset(text: &str, position: Position) -> Option<usize> {
    let mut offset = 0;
    for (line_idx, line) in text.lines().enumerate() {
        if line_idx == position.line as usize {
            let char_offset = position.character as usize;
            // Make sure we don't go past the line length
            let line_len = line.chars().count();
            if char_offset <= line_len {
                // Count bytes up to the character position
                let byte_offset = line
                    .chars()
                    .take(char_offset)
                    .map(|c| c.len_utf8())
                    .sum::<usize>();
                return Some(offset + byte_offset);
            }
            return None;
        }
        offset += line.len() + 1; // +1 for newline
    }
    None
}

/// Convert byte offset to LSP Position
pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let mut current_offset = 0;
    for (line_idx, line) in text.lines().enumerate() {
        let line_len = line.len();
        if current_offset + line_len >= offset {
            let char_offset = line[..(offset - current_offset).min(line_len)]
                .chars()
                .count();
            return Position::new(line_idx as u32, char_offset as u32);
        }
        current_offset += line_len + 1; // +1 for newline
    }
    // If we reach here, return the end of the document
    let line_count = text.lines().count();
    let last_line_len = text.lines().last().map_or(0, |l| l.chars().count());
    Position::new(line_count.saturating_sub(1) as u32, last_line_len as u32)
}

/// Convert a byte range to an LSP Range
pub fn byte_range_to_lsp_range(text: &str, start: usize, end: usize) -> Range {
    Range {
        start: offset_to_position(text, start),
        end: offset_to_position(text, end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_to_offset() {
        let text = "line1\nline2\nline3";
        assert_eq!(position_to_offset(text, Position::new(0, 0)), Some(0));
        assert_eq!(position_to_offset(text, Position::new(0, 3)), Some(3));
        assert_eq!(position_to_offset(text, Position::new(1, 0)), Some(6));
        assert_eq!(position_to_offset(text, Position::new(2, 2)), Some(14));
    }

    #[test]
    fn test_offset_to_position() {
        let text = "line1\nline2\nline3";
        assert_eq!(offset_to_position(text, 0), Position::new(0, 0));
        assert_eq!(offset_to_position(text, 3), Position::new(0, 3));
        assert_eq!(offset_to_position(text, 6), Position::new(1, 0));
        assert_eq!(offset_to_position(text, 14), Position::new(2, 2));
    }
}
