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

/// The candidate closest to `target` within a plausible-typo distance (half
/// the target's length, at least 1) - `None` when nothing is that close. The
/// same rule as core's "did you mean" suggestions.
pub fn closest<'a>(target: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let max_distance = target.chars().count().div_ceil(2).max(1);
    candidates
        .into_iter()
        .filter(|&candidate| candidate != target)
        .map(|candidate| (candidate, levenshtein(target, candidate)))
        .filter(|&(_, distance)| distance <= max_distance)
        .min_by_key(|&(_, distance)| distance)
        .map(|(candidate, _)| candidate.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, a_char) in a.chars().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, &b_char) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(a_char != b_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        previous = current;
    }
    previous[b.len()]
}

/// Is `offset` inside a `//` line comment, a `/* ... */` block comment (which
/// can span multiple lines), or a `"…"` string (which can't)? Needs a scan
/// from the start of the document - unlike a line comment or a string, a
/// block comment's start isn't visible from `offset`'s own line alone.
pub fn in_comment_or_string(source: &str, offset: usize) -> bool {
    let offset = offset.min(source.len());
    let bytes = source.as_bytes();

    #[derive(PartialEq)]
    enum State {
        Normal,
        Str,
        Line,
        Block,
    }

    let mut state = State::Normal;
    let mut i = 0;
    while i < offset {
        match state {
            State::Normal => match bytes[i] {
                b'"' => state = State::Str,
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    state = State::Line;
                    i += 1;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    state = State::Block;
                    i += 1;
                }
                _ => {}
            },
            State::Str => match bytes[i] {
                b'\\' => i += 1,
                b'"' | b'\n' => state = State::Normal,
                _ => {}
            },
            State::Line => {
                if bytes[i] == b'\n' {
                    state = State::Normal;
                }
            }
            State::Block => {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    state = State::Normal;
                    i += 1;
                }
            }
        }
        i += 1;
    }
    state != State::Normal
}

/// A `::`-separated path with the cursor on one of its segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAt {
    /// The path's segments up to and including the one under the cursor.
    pub segments: Vec<String>,
    /// The byte range of the segment under the cursor.
    pub range: (usize, usize),
    /// Whether `::` follows that segment directly: it names a namespace, not
    /// (only) a declaration.
    pub continues: bool,
}

/// The path (`std::validators::StringBounds`) the identifier under `offset`
/// is part of, with the cursor's segment. Segments are joined only by a
/// directly adjacent `::`; `None` off an identifier.
pub fn path_at(text: &str, offset: usize) -> Option<PathAt> {
    let (start, end) = word_range_at(text, offset)?;
    if !text[start..].starts_with(|c: char| c.is_alphabetic() || c == '_') {
        return None;
    }

    let mut segments = vec![text[start..end].to_string()];
    let mut at = start;
    while text[..at].ends_with("::") {
        let before = &text[..at - 2];
        let segment_start = before.rfind(|c: char| !is_ident_char(c)).map_or(0, |i| i + 1);
        let segment = &before[segment_start..];
        if segment.is_empty() || !segment.starts_with(|c: char| c.is_alphabetic() || c == '_') {
            break;
        }
        segments.insert(0, segment.to_string());
        at = segment_start;
    }

    Some(PathAt { segments, range: (start, end), continues: text[end..].starts_with("::") })
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

    #[test]
    fn closest_suggests_plausible_typos_only() {
        assert_eq!(closest("typse", ["types", "chat"]), Some("types".to_string()));
        assert_eq!(closest("Thign", ["Thing", "Other"]), Some("Thing".to_string()));
        assert_eq!(closest("zzzz", ["types", "chat"]), None);
        assert_eq!(closest("types", ["types"]), None, "an exact match isn't a suggestion");
    }

    #[test]
    fn in_comment_or_string_scans_one_line() {
        assert!(in_comment_or_string("/// doc", 5));
        assert!(in_comment_or_string("a: u8 // c", 9));
        assert!(in_comment_or_string("x = \"unclosed", 10));
        assert!(!in_comment_or_string("x = \"done\" ", 11));
        assert!(!in_comment_or_string("struct M {", 9));
        // a `//` inside a string is not a comment
        assert!(in_comment_or_string("x = \"a // b", 8)); // still in the string
    }

    #[test]
    fn in_comment_or_string_crosses_a_block_comment_across_lines() {
        let text = "/*\nfoo\n*/\nbar";
        // Inside the comment, on the line after its opener.
        assert!(in_comment_or_string(text, text.find("foo").unwrap() + 1));
        // Past the closing `*/`, on the following line - no longer inside.
        assert!(!in_comment_or_string(text, text.find("bar").unwrap()));
    }

    #[test]
    fn in_comment_or_string_handles_a_block_comment_on_one_line() {
        let text = "/* note */ struct M {";
        assert!(in_comment_or_string(text, 5));
        assert!(!in_comment_or_string(text, text.find("struct").unwrap() + 1));
    }

    #[test]
    fn in_comment_or_string_a_slash_inside_a_string_does_not_start_a_block_comment() {
        let text = "x = \"a /* b\" ok";
        // Past the closing quote, back to ordinary code.
        assert!(!in_comment_or_string(text, text.find("ok").unwrap()));
    }

    #[test]
    fn path_at_reads_the_whole_path_up_to_the_cursor() {
        let text = "use std::validators::StringBounds\n";
        let at = |needle: &str| path_at(text, text.find(needle).unwrap() + 1).unwrap();

        let std = at("std");
        assert_eq!((std.segments.as_slice(), std.continues), (&["std".to_string()][..], true));
        let validators = at("validators");
        assert_eq!(validators.segments, ["std", "validators"]);
        assert!(validators.continues);
        assert_eq!(&text[validators.range.0..validators.range.1], "validators");
        let last = at("StringBounds");
        assert_eq!(last.segments, ["std", "validators", "StringBounds"]);
        assert!(!last.continues, "nothing follows the last segment");
    }

    #[test]
    fn path_at_stops_at_anything_but_an_adjacent_separator() {
        let text = "use a::{B, C}\nx: parent::common::*\nuse t as T\n";
        let seg = |needle: &str| path_at(text, text.find(needle).unwrap()).unwrap().segments;
        assert_eq!(seg("B,"), ["B"], "inside a brace list there's no path before it");
        assert_eq!(seg("common"), ["parent", "common"]);
        assert_eq!(path_at("a ::b", 4).unwrap().segments, ["b"], "a space breaks the path");
        assert_eq!(path_at("9lives::x", 1), None, "not an identifier");
        assert_eq!(path_at("a::b", 4), None, "past the end");
    }
}
