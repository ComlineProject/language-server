//! Semantic tokens — the single source of truth for Comline syntax
//! highlighting, consumed by the LSP (`comline-lsp`) and, via WASM, by the
//! playground's editor.
//!
//! A small line-oriented lexer: enough to colour keywords, primitive / user
//! types, strings, `//` comments, `@annotations` and numbers without a full
//! AST walk. Token positions are in characters (== UTF-16 units for ASCII
//! schemas, which is the common case).

use lsp_types::{SemanticToken, SemanticTokens, SemanticTokensResult, Url};

// Indices into the `SemanticTokensLegend` declared in `backend.rs` — keep in
// sync with `LEGEND_TYPES` there.
const KEYWORD: u32 = 0;
const TYPE: u32 = 1;
const STRING: u32 = 2;
const COMMENT: u32 = 3;
const NUMBER: u32 = 4;
const DECORATOR: u32 = 5;

/// The token-type names, in legend order. `backend.rs` turns these into
/// `SemanticTokenType`s.
pub const LEGEND_TYPES: &[&str] = &["keyword", "type", "string", "comment", "number", "decorator"];

const KEYWORDS: &[&str] = &[
    "struct", "enum", "protocol", "error", "const", "use", "import", "validator", "settings",
    "function", "optional",
];
const PRIMITIVES: &[&str] = &[
    "s8", "s16", "s32", "s64", "u8", "u16", "u32", "u64", "f32", "f64", "bool", "str", "string",
    "int", "float",
];

pub fn get_semantic_tokens(source: &str, _uri: &Url) -> Option<SemanticTokensResult> {
    let mut data: Vec<SemanticToken> = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    for (line_idx, line) in source.split('\n').enumerate() {
        let line_no = line_idx as u32;
        for (start, length, token_type) in lex_line(line) {
            let delta_line = line_no - prev_line;
            let delta_start = if delta_line == 0 { start - prev_start } else { start };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: 0,
            });
            prev_line = line_no;
            prev_start = start;
        }
    }

    Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    }))
}

/// `(start_char, length, token_type)` for each token on one line.
fn lex_line(line: &str) -> Vec<(u32, u32, u32)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];

        if c == '/' && chars.get(i + 1) == Some(&'/') {
            out.push((i as u32, (chars.len() - i) as u32, COMMENT));
            break;
        }

        if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(chars.len());
            out.push((i as u32, (end - i) as u32, STRING));
            i = end;
            continue;
        }

        if c == '@' && chars.get(i + 1).is_some_and(|c| c.is_alphabetic() || *c == '_') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            out.push((i as u32, (j - i) as u32, DECORATOR));
            i = j;
            continue;
        }

        if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_alphanumeric() || chars[j] == '.' || chars[j] == '_')
            {
                j += 1;
            }
            out.push((i as u32, (j - i) as u32, NUMBER));
            i = j;
            continue;
        }

        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let kind = if KEYWORDS.contains(&word.as_str()) {
                Some(KEYWORD)
            } else if PRIMITIVES.contains(&word.as_str())
                || word.starts_with(|c: char| c.is_uppercase())
            {
                Some(TYPE)
            } else {
                None
            };
            if let Some(k) = kind {
                out.push((i as u32, (j - i) as u32, k));
            }
            i = j;
            continue;
        }

        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types_on(src: &str) -> Vec<u32> {
        let r = get_semantic_tokens(src, &Url::parse("file:///t.ids").unwrap()).unwrap();
        let SemanticTokensResult::Tokens(t) = r else {
            panic!()
        };
        t.data.iter().map(|x| x.token_type).collect()
    }

    #[test]
    fn colours_keywords_types_strings_comments_annotations() {
        let src = "@framing = \"jsonrpc\"\nstruct Msg {\n    body: string // a note\n}\n";
        let ty = types_on(src);
        assert!(ty.contains(&DECORATOR)); // @framing
        assert!(ty.contains(&STRING)); // "jsonrpc"
        assert!(ty.contains(&KEYWORD)); // struct
        assert!(ty.contains(&TYPE)); // Msg, string
        assert!(ty.contains(&COMMENT)); // // a note
    }

    #[test]
    fn empty_source_yields_no_tokens() {
        let r = get_semantic_tokens("", &Url::parse("file:///t.ids").unwrap()).unwrap();
        let SemanticTokensResult::Tokens(t) = r else {
            panic!()
        };
        assert!(t.data.is_empty());
    }

    #[test]
    fn deltas_are_relative() {
        let r = get_semantic_tokens(
            "struct A {}\nstruct B {}\n",
            &Url::parse("file:///t.ids").unwrap(),
        )
        .unwrap();
        let SemanticTokensResult::Tokens(t) = r else {
            panic!()
        };
        let second = t.data.iter().rev().find(|x| x.token_type == KEYWORD).unwrap();
        assert_eq!(second.delta_line, 1);
        assert_eq!(second.delta_start, 0);
    }
}
