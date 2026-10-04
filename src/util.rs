use lsp_types::{Position, Range};

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

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier (alphanumerics + `_`) touching byte `offset`, as its byte
/// range — `None` when `offset` is past the end or not on an identifier.
pub fn word_range_at(text: &str, offset: usize) -> Option<(usize, usize)> {
    if offset >= text.len() {
        return None;
    }

    let start = text[..offset]
        .rfind(|c: char| !is_ident_char(c))
        .map(|i| i + 1)
        .unwrap_or(0);

    let end = text[offset..]
        .find(|c: char| !is_ident_char(c))
        .map(|i| offset + i)
        .unwrap_or(text.len());

    (start < end).then_some((start, end))
}

/// Every whole-identifier occurrence of `word` in `text`, as byte offsets —
/// `User` matches in `user: User`, but not inside `UserId` or `my_User`.
pub fn word_occurrences(text: &str, word: &str) -> Vec<usize> {
    if word.is_empty() {
        return vec![];
    }

    text.match_indices(word)
        .map(|(i, _)| i)
        .filter(|&i| {
            let before = text[..i].chars().next_back();
            let after = text[i + word.len()..].chars().next();
            !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
        })
        .collect()
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

    #[test]
    fn test_word_range_at() {
        let text = "user: User[]";
        assert_eq!(word_range_at(text, 8), Some((6, 10)));
        assert_eq!(word_range_at(text, 0), Some((0, 4)));
        assert_eq!(word_range_at(text, 4), Some((0, 4)), "just past a word still counts");
        assert_eq!(word_range_at(text, 5), None);
    }

    #[test]
    fn test_word_occurrences_are_whole_words_only() {
        let text = "struct UserId { user: User, u: my_User, v: User[] }";
        assert_eq!(word_occurrences(text, "User"), vec![22, 43]);
    }
}
