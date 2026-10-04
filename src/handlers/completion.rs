// Completion handler - provides auto-completion suggestions

use crate::analysis::annotations::{self, AnnotationScope};
use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use lsp_types::{CompletionItem, CompletionItemKind, Position, Url};

/// Get completion suggestions at a position
pub fn get_completions(source: &str, uri: &Url, position: Position) -> Vec<CompletionItem> {
    let offset = match position_to_offset(source, position) {
        Some(o) => o,
        None => return get_keyword_completions(),
    };

    // No code completion inside a `//` comment (docstrings included) or a
    // string literal — the text there isn't Comline.
    if in_comment_or_string(source, offset) {
        return Vec::new();
    }

    // Best-effort: a symbol table of user-declared struct/enum/type-alias
    // names, when the document happens to fully parse right now. While
    // actively typing it almost never will (the whole document is one
    // grammar rule, and a single in-progress declaration fails the entire
    // parse) — context detection below works directly on the raw text
    // either way, so a mid-edit document still gets real, context-aware
    // suggestions; this only widens the *type* list with names that
    // haven't been reparsed yet.
    let symbol_table = parser::parse(source)
        .ok()
        .and_then(|r| r.document)
        .map(|document| symbols::build_symbol_table(&document, uri, source));

    let mut completions = Vec::new();

    match determine_context(source, offset) {
        CompletionContext::TypePosition => {
            // Right after `:` / `->`, or inside an unclosed `union(...)` —
            // every known type (primitives first, then user-declared
            // struct/enum/alias names) is relevant here, regardless of how
            // much whitespace or partial typing separates the cursor from
            // that `:`/`->`/`(`.
            completions.extend(get_primitive_type_completions());
            completions.extend(get_type_completions(symbol_table.as_ref()));
        }
        CompletionContext::TopLevel => {
            // Nothing (or only a partial keyword) typed yet, no enclosing
            // block — only a new top-level declaration can start here.
            completions.extend(get_keyword_completions());
        }
        CompletionContext::StructBody => {
            // Start of a new struct/error field, before its name —
            // nothing to suggest but the one modifier keyword a field can
            // carry (the field's own name is an arbitrary new identifier,
            // and its type is handled by `TypePosition` once `:` appears).
            completions.push(optional_keyword_completion());
        }
        CompletionContext::ProtocolBody => {
            // Start of a new protocol member — today that can only be a
            // `function`.
            completions.push(function_keyword_completion());
        }
        CompletionContext::AnnotationKey(scope) => {
            completions.extend(get_annotation_completions(scope));
        }
        CompletionContext::DeclarationName => {
            // Naming something new — the identifier right after `struct`/
            // `enum`/.../`function`, or an enum's own variant name. It's
            // an arbitrary new name, not a reference to anything that
            // already exists, so there is nothing useful to suggest.
        }
        CompletionContext::Unknown => {
            // Context couldn't be narrowed down — offer the same broad
            // mix completion has always fallen back to, rather than
            // nothing.
            completions.extend(get_keyword_completions());
            completions.extend(get_primitive_type_completions());
            completions.extend(get_type_completions(symbol_table.as_ref()));
        }
    }

    completions
}

/// Is `offset` inside a `//` line comment or a `"…"` string on its line?
/// Comline has no block comments and strings don't span lines, so a scan of
/// the current line's prefix is enough.
fn in_comment_or_string(source: &str, offset: usize) -> bool {
    let offset = offset.min(source.len());
    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let bytes = source.as_bytes();
    let mut in_str = false;
    let mut i = line_start;
    while i < offset {
        match bytes[i] {
            b'\\' if in_str => {
                i += 2;
                continue;
            }
            b'"' => in_str = !in_str,
            b'/' if !in_str && bytes.get(i + 1) == Some(&b'/') => return true,
            _ => {}
        }
        i += 1;
    }
    in_str
}

/// Blank out `//` line comments and the *contents* of `"…"` string
/// literals — replacing each covered character with a space, preserving
/// length and every newline — so a backward token scan never gets confused
/// by punctuation that only exists inside a comment or a string (an `=`
/// inside a docstring, a stray `{` inside an error message, …). Comline has
/// no block comments and no multi-line strings, so this can process one
/// line at a time. Operates on `char`s throughout (not raw bytes), since a
/// docstring may contain arbitrary UTF-8.
fn strip_comments_and_strings(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let mut chars = line.char_indices().peekable();
        let mut in_str = false;
        while let Some((_, c)) = chars.next() {
            if in_str {
                if c == '\\' {
                    out.push(' ');
                    if let Some(&(_, next)) = chars.peek() {
                        if next != '\n' {
                            chars.next();
                            out.push(' ');
                        }
                    }
                    continue;
                }
                if c == '"' {
                    in_str = false;
                }
                out.push(' ');
                continue;
            }
            if c == '"' {
                in_str = true;
                out.push(' ');
                continue;
            }
            if c == '/' && chars.peek().map(|&(_, c2)| c2) == Some('/') {
                chars.next();
                out.push(' ');
                out.push(' ');
                for (_, c2) in chars.by_ref() {
                    out.push(if c2 == '\n' { '\n' } else { ' ' });
                }
                break;
            }
            out.push(c);
        }
    }
    out
}

/// One token in a backward scan from the cursor — just enough shape to
/// classify completion context: identifier/keyword runs, and the specific
/// punctuation marks the grammar uses around a type (`:` for a field/arg/
/// const type, `->` for a return type, parens for `union(...)` and a
/// function's argument list, braces for every block).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok<'a> {
    Word(&'a str),
    Punct(char),
    Arrow,
}

/// Tokenize `cleaned[..offset]` (forward is simplest; callers only ever
/// look at the last few entries). `cleaned` should already be the output of
/// [`strip_comments_and_strings`] — this doesn't know about comments or
/// strings itself. Each token carries the byte offset its text *ends* at,
/// so a caller can tell whether it directly abuts `offset` (still being
/// typed) or has trailing whitespace after it (already finished).
fn tokenize_prefix(cleaned: &str, offset: usize) -> Vec<(Tok<'_>, usize)> {
    let end = offset.min(cleaned.len());
    let text = &cleaned[..end];
    let mut toks = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            let mut last_end = i + c.len_utf8();
            while let Some(&(j, c2)) = chars.peek() {
                if c2.is_alphanumeric() || c2 == '_' {
                    last_end = j + c2.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            toks.push((Tok::Word(&text[start..last_end]), last_end));
            continue;
        }
        if c == '-' && chars.peek().map(|&(_, c2)| c2) == Some('>') {
            let (j, _) = chars.next().unwrap();
            toks.push((Tok::Arrow, j + 1));
            continue;
        }
        toks.push((Tok::Punct(c), i + c.len_utf8()));
    }

    toks
}

/// Declaration-introducing keywords — completion offers nothing while the
/// identifier right after one of these is being typed (see
/// [`CompletionContext::DeclarationName`]).
const DECL_KEYWORDS: &[&str] = &[
    "struct", "enum", "protocol", "const", "type", "error", "settings", "validator", "function",
];

/// Determine completion context based on position — a backward scan over
/// tokens, not a full grammar parse, so it degrades gracefully on exactly
/// the syntactically-incomplete text that's the norm while actively
/// typing.
fn determine_context(source: &str, offset: usize) -> CompletionContext {
    if offset == 0 {
        return CompletionContext::TopLevel;
    }
    let offset = offset.min(source.len());
    let cleaned = strip_comments_and_strings(source);
    let mut toks = tokenize_prefix(&cleaned, offset);

    // Drop a trailing word that's still being typed (abuts the cursor with
    // no whitespace in between) — the client already filters completions
    // by it; it isn't part of the surrounding context being classified.
    if let Some(&(Tok::Word(_), end)) = toks.last() {
        if end == offset {
            toks.pop();
        }
    }

    // Naming something new: the identifier right after a
    // declaration-introducing keyword.
    if let Some(&(Tok::Word(w), _)) = toks.last() {
        if DECL_KEYWORDS.contains(&w) {
            return CompletionContext::DeclarationName;
        }
    }

    // Right after a bare `@` — the start of an annotation key. Which keys
    // make sense depends on what `@key=value` is about to attach to, which
    // the grammar ties to the *enclosing* block, not anything about `@`
    // itself: a struct/error body (a field annotation, e.g. `@validators`),
    // a protocol body (a function annotation, e.g. `@timeout_ms`), or top
    // level (a struct's or protocol's own leading annotation, e.g.
    // `@framing` before `protocol`). `enclosing_block` already answers
    // exactly that question for `StructBody` / `ProtocolBody` — reused
    // as-is; it ignores the trailing `@` token like it does any other
    // non-brace token.
    if matches!(toks.last(), Some((Tok::Punct('@'), _))) {
        match enclosing_block(&toks) {
            Some(EnclosingBlock::Struct) => {
                return CompletionContext::AnnotationKey(AnnotationScope::Field)
            }
            Some(EnclosingBlock::Protocol) => {
                return CompletionContext::AnnotationKey(AnnotationScope::Function)
            }
            None => return CompletionContext::AnnotationKey(AnnotationScope::Leading),
            // An enum variant or some other block don't take annotations —
            // fall through to the same handling `enclosing_block` gives
            // any other token there (nothing for an enum, the broad
            // fallback otherwise), rather than inventing a separate rule
            // just for a stray `@`.
            Some(EnclosingBlock::Enum) | Some(EnclosingBlock::Other) => {}
        }
    }

    // Right after `:` (a field/argument/const/validator-property type) or
    // `->` (a return type).
    match toks.last() {
        Some((Tok::Punct(':'), _)) | Some((Tok::Arrow, _)) => {
            return CompletionContext::TypePosition;
        }
        _ => {}
    }

    // Inside any unclosed `(...)` — a `union(...)`'s member list or a
    // function's argument list (where a bare type, with no `name:`
    // prefix, is also valid grammar) both expect a type here.
    if in_unclosed_paren(&toks) {
        return CompletionContext::TypePosition;
    }

    match enclosing_block(&toks) {
        Some(EnclosingBlock::Struct) => CompletionContext::StructBody,
        Some(EnclosingBlock::Protocol) => CompletionContext::ProtocolBody,
        // An enum variant, like a declaration's own name, is an arbitrary
        // new identifier — nothing to suggest.
        Some(EnclosingBlock::Enum) => CompletionContext::DeclarationName,
        Some(EnclosingBlock::Other) => CompletionContext::Unknown,
        None => CompletionContext::TopLevel,
    }
}

/// Whether the cursor sits inside a `(` that hasn't been closed yet, per a
/// backward-matching scan over `toks`.
fn in_unclosed_paren(toks: &[(Tok, usize)]) -> bool {
    let mut depth = 0i32;
    for (tok, _) in toks.iter().rev() {
        match tok {
            Tok::Punct(')') => depth += 1,
            Tok::Punct('(') => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

enum EnclosingBlock {
    Struct,
    Protocol,
    Enum,
    Other,
}

/// What kind of block the innermost unclosed `{` (if any) belongs to,
/// determined by the declaration keyword immediately before that block's
/// own name. A function's `(...)...;` has no braces of its own, so this
/// never needs to look past one — the protocol's own `{` is always the
/// innermost unclosed brace while positioned between two function
/// declarations.
fn enclosing_block(toks: &[(Tok, usize)]) -> Option<EnclosingBlock> {
    let mut depth = 0i32;
    for i in (0..toks.len()).rev() {
        match toks[i].0 {
            Tok::Punct('}') => depth += 1,
            Tok::Punct('{') => {
                if depth > 0 {
                    depth -= 1;
                    continue;
                }
                // Found the innermost unclosed `{`. Walk back past the
                // block's own name (one Word, if present) to the
                // declaration keyword that introduced it.
                let mut j = i;
                if j > 0 {
                    if let Tok::Word(_) = toks[j - 1].0 {
                        j -= 1;
                    }
                }
                let kw = (j > 0).then(|| toks[j - 1].0).and_then(|t| match t {
                    Tok::Word(w) => Some(w),
                    _ => None,
                });
                return Some(match kw {
                    Some("struct") | Some("error") => EnclosingBlock::Struct,
                    Some("protocol") => EnclosingBlock::Protocol,
                    Some("enum") => EnclosingBlock::Enum,
                    _ => EnclosingBlock::Other,
                });
            }
            _ => {}
        }
    }
    None
}

fn optional_keyword_completion() -> CompletionItem {
    CompletionItem {
        label: "optional".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Optional field modifier".to_string()),
        ..Default::default()
    }
}

fn function_keyword_completion() -> CompletionItem {
    CompletionItem {
        label: "function".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Define a function".to_string()),
        insert_text: Some("function $1($2)$0;".to_string()),
        insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
        ..Default::default()
    }
}

/// Get keyword completions (struct, enum, protocol, etc.)
fn get_keyword_completions() -> Vec<CompletionItem> {
    vec![
        CompletionItem {
            label: "struct".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a structure".to_string()),
            insert_text: Some("struct $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "enum".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define an enumeration".to_string()),
            insert_text: Some("enum $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "protocol".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a protocol".to_string()),
            insert_text: Some("protocol $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "const".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a constant".to_string()),
            insert_text: Some("const $1: $2 = $0".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "type".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a transparent type alias".to_string()),
            insert_text: Some("type $1 = $0".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "error".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define an error".to_string()),
            insert_text: Some("error $1 {\n\tmessage = \"$2\"\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "settings".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define schema-wide settings".to_string()),
            insert_text: Some("settings $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "validator".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a named, parameterised field check".to_string()),
            insert_text: Some("validator $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "use".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Import statement".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "import".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Legacy import statement".to_string()),
            ..Default::default()
        },
    ]
}

/// Get primitive type completions
fn get_primitive_type_completions() -> Vec<CompletionItem> {
    vec![
        // Integer types
        CompletionItem {
            label: "i8".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("8-bit signed integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "i16".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("16-bit signed integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "i32".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("32-bit signed integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "i64".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("64-bit signed integer".to_string()),
            ..Default::default()
        },
        // Unsigned integers
        CompletionItem {
            label: "u8".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("8-bit unsigned integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "u16".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("16-bit unsigned integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "u32".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("32-bit unsigned integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "u64".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("64-bit unsigned integer".to_string()),
            ..Default::default()
        },
        // Floats
        CompletionItem {
            label: "f32".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("32-bit floating point".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "f64".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("64-bit floating point".to_string()),
            ..Default::default()
        },
        // Strings and bool
        CompletionItem {
            label: "string".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("String type".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "str".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("String slice type".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "bool".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("Boolean type".to_string()),
            ..Default::default()
        },
    ]
}

/// User-defined type completions from the symbol table — struct, enum, and
/// `type` alias names only (not const/protocol names, which aren't valid
/// in a type position). `None` when the document doesn't currently parse —
/// no custom names are available yet, but callers still have the
/// primitive list.
fn get_type_completions(symbol_table: Option<&symbols::SymbolTable>) -> Vec<CompletionItem> {
    let Some(symbol_table) = symbol_table else {
        return Vec::new();
    };
    symbol_table
        .all_symbols()
        .iter()
        .filter_map(|symbol| {
            let kind = match symbol.kind {
                lsp_types::SymbolKind::STRUCT => CompletionItemKind::STRUCT,
                lsp_types::SymbolKind::ENUM => CompletionItemKind::ENUM,
                lsp_types::SymbolKind::TYPE_PARAMETER => CompletionItemKind::CLASS,
                _ => return None,
            };

            let detail = if !symbol.children.is_empty() {
                Some(format!("{} - {} items", symbol.name, symbol.children.len()))
            } else {
                None
            };

            Some(CompletionItem {
                label: symbol.name.clone(),
                kind: Some(kind),
                detail,
                ..Default::default()
            })
        })
        .collect()
}

/// The snippet a known annotation key inserts — completion-specific
/// (editor insertion mechanics), so it lives here rather than in the
/// shared `analysis::annotations` table, which only describes *meaning*.
/// `None` falls back to inserting just the bare key name.
fn annotation_insert_text(key: &str) -> Option<&'static str> {
    match key {
        "validators" => Some("validators = [$0]"),
        "timeout_ms" => Some("timeout_ms = $0"),
        "framing" => Some("framing = \"${1|jsonrpc,datagram|}\"$0"),
        // Conceptually a bare marker, but the grammar requires `=value`
        // unconditionally — see `annotations::KNOWN_ANNOTATIONS`'s own
        // `idempotent` entry for why `= true` and not a bool literal.
        "idempotent" => Some("idempotent = ${1:true}$0"),
        _ => None,
    }
}

/// Annotation-key completions for `scope`, sourced from
/// [`annotations::KNOWN_ANNOTATIONS`] — the same table [`hover`] reads for
/// an annotation key's own tooltip, so the two can't describe one key two
/// different ways.
fn get_annotation_completions(scope: AnnotationScope) -> Vec<CompletionItem> {
    annotations::for_scope(scope)
        .map(|info| {
            let snippet = annotation_insert_text(info.key);
            CompletionItem {
                label: info.key.to_string(),
                kind: Some(CompletionItemKind::PROPERTY),
                detail: Some(info.description.to_string()),
                insert_text: snippet.map(str::to_string),
                insert_text_format: snippet.map(|_| lsp_types::InsertTextFormat::SNIPPET),
                ..Default::default()
            }
        })
        .collect()
}

/// Completion context
#[derive(Debug, PartialEq)]
enum CompletionContext {
    /// After `:` / `->`, or inside an unclosed `union(...)` / function
    /// argument list.
    TypePosition,
    /// No enclosing block, and not naming something — a new top-level
    /// declaration can start here.
    TopLevel,
    /// Inside a `struct`/`error` body, before a field's name.
    StructBody,
    /// Inside a `protocol` body, before a member.
    ProtocolBody,
    /// Right after a bare `@` — naming an annotation key, for the given
    /// scope.
    AnnotationKey(AnnotationScope),
    /// Typing an arbitrary new name: right after a declaration keyword, or
    /// an enum variant. Nothing to suggest.
    DeclarationName,
    /// Context couldn't be narrowed down.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyword_completions() {
        let completions = get_keyword_completions();
        assert!(completions.len() >= 5);
        assert!(completions.iter().any(|c| c.label == "struct"));
        assert!(completions.iter().any(|c| c.label == "enum"));
        assert!(completions.iter().any(|c| c.label == "protocol"));
        assert!(completions.iter().any(|c| c.label == "type"));
        assert!(completions.iter().any(|c| c.label == "error"));
        assert!(completions.iter().any(|c| c.label == "settings"));
        assert!(completions.iter().any(|c| c.label == "validator"));
    }

    #[test]
    fn test_primitive_type_completions() {
        let completions = get_primitive_type_completions();
        assert!(completions.len() >= 10);
        assert!(completions.iter().any(|c| c.label == "i32"));
        assert!(completions.iter().any(|c| c.label == "string"));
        assert!(completions.iter().any(|c| c.label == "bool"));
    }

    #[test]
    fn test_completion_after_colon() {
        let source = "struct User {\n    name: ";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 10); // After ":"

        let completions = get_completions(source, &uri, position);
        // Should include types
        assert!(!completions.is_empty());
        assert!(completions.iter().any(|c| c.label == "string"));
    }

    #[test]
    fn test_completion_top_level() {
        let source = "\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(0, 0);

        let completions = get_completions(source, &uri, position);
        // Should include keywords
        assert!(completions.iter().any(|c| c.label == "struct"));
        assert!(completions.iter().any(|c| c.label == "enum"));
    }

    #[test]
    fn top_level_after_closed_declarations_still_offers_keywords() {
        // A full document that ends with a trailing partial identifier at
        // top level (the common "actively typing a new declaration"
        // shape) fails to parse as a whole — context detection must not
        // depend on the document parsing successfully.
        let source = "enum Language {\n    English\n}\n\nstruct Greeting {\n    message: string\n}\n\nprotocol Thing {\n    function foo();\n}\n\nty";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(11, 2);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "type"));
        assert!(completions.iter().any(|c| c.label == "struct"));
        // No primitive types at top level - "u32" etc. can never start a
        // top-level declaration.
        assert!(!completions.iter().any(|c| c.label == "u32"));
    }

    #[test]
    fn protocol_body_offers_function_not_types_or_top_level_keywords() {
        let source = "protocol Thing {\n    function foo();\n    f";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(2, 5);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "function"));
        assert!(!completions.iter().any(|c| c.label == "struct"));
        assert!(!completions.iter().any(|c| c.label == "u32"));
    }

    #[test]
    fn typing_a_function_name_offers_nothing() {
        let source = "protocol Thing {\n    function f";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 13);

        assert!(get_completions(source, &uri, position).is_empty());
    }

    #[test]
    fn typing_any_declaration_name_offers_nothing() {
        let uri = Url::parse("file:///test.ids").unwrap();
        for source in ["struct Us", "enum Sta", "const MA", "type Use", "error No"] {
            let position = Position::new(0, source.chars().count() as u32);
            assert!(
                get_completions(source, &uri, position).is_empty(),
                "expected no completions while typing the name in {source:?}"
            );
        }
    }

    #[test]
    fn enum_body_offers_nothing() {
        let source = "enum Status {\n    Active\n    P";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(2, 5);

        assert!(get_completions(source, &uri, position).is_empty());
    }

    #[test]
    fn type_position_survives_interior_whitespace() {
        let source = "struct X {\n    test:  "; // two trailing spaces
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "u32"));
    }

    #[test]
    fn type_position_survives_a_partial_type_name() {
        let source = "struct X {\n    test: u";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "u64"));
    }

    #[test]
    fn union_member_position_offers_types() {
        let source = "struct X {\n    v: union(";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "u32"));
    }

    #[test]
    fn return_type_position_offers_types() {
        let source = "protocol P {\n    function f() -> ";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "string"));
    }

    #[test]
    fn function_arg_list_offers_types_not_declaration_keywords() {
        // No `name:` yet - a bare type is also valid grammar here.
        let source = "protocol P {\n    function f(";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "u32"));
        assert!(!completions.iter().any(|c| c.label == "struct"));
    }

    #[test]
    fn annotation_on_a_field_offers_validators() {
        let source = "struct X {\n    @";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 5);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "validators"));
        // Function/leading-only keys shouldn't leak into field scope.
        assert!(!completions.iter().any(|c| c.label == "timeout_ms"));
        assert!(!completions.iter().any(|c| c.label == "framing"));
    }

    #[test]
    fn annotation_on_a_function_offers_timeout_ms_and_idempotent() {
        let source = "protocol P {\n    @";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 5);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "timeout_ms"));
        assert!(completions.iter().any(|c| c.label == "idempotent"));
        assert!(!completions.iter().any(|c| c.label == "validators"));
    }

    #[test]
    fn annotation_at_top_level_offers_framing() {
        let source = "@";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(0, 1);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "framing"));
        assert!(!completions.iter().any(|c| c.label == "validators"));
        assert!(!completions.iter().any(|c| c.label == "timeout_ms"));
    }

    #[test]
    fn annotation_key_still_resolves_while_partially_typed() {
        // Trailing partial word after `@` is dropped the same way any
        // other in-progress word is, same as `type_position_survives_a_
        // partial_type_name` for `:`.
        let source = "protocol P {\n    @time";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 9);

        let completions = get_completions(source, &uri, position);
        assert!(completions.iter().any(|c| c.label == "timeout_ms"));
    }

    #[test]
    fn annotation_in_an_enum_body_offers_nothing() {
        let source = "enum Status {\n    @";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 5);

        assert!(get_completions(source, &uri, position).is_empty());
    }

    #[test]
    fn no_completions_inside_comments_or_strings() {
        let uri = Url::parse("file:///test.ids").unwrap();

        // inside a `///` docstring
        let src = "/// Message that can be se\nstruct M {}\n";
        assert!(get_completions(src, &uri, Position::new(0, 25)).is_empty());

        // inside a `//` comment after code
        let src = "struct M { a: u8 } // note he\n";
        assert!(get_completions(src, &uri, Position::new(0, 27)).is_empty());

        // inside a string literal
        let src = "error E {\n    message = \"oops re\n}\n";
        assert!(get_completions(src, &uri, Position::new(1, 20)).is_empty());

        // code before a trailing comment on the same line still completes
        let src = "struct M {\n    name:  // a field\n}\n";
        assert!(!get_completions(src, &uri, Position::new(1, 10)).is_empty());
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
    fn strip_comments_and_strings_preserves_length_and_newlines() {
        let source = "struct M {\n    a: str // note\n    b: str\n}\n";
        let cleaned = strip_comments_and_strings(source);
        assert_eq!(cleaned.chars().count(), source.chars().count());
        assert_eq!(
            cleaned.chars().filter(|&c| c == '\n').count(),
            source.chars().filter(|&c| c == '\n').count()
        );
        assert!(!cleaned.contains("note"));
        assert!(cleaned.contains("struct M {"));
    }

    #[test]
    fn test_context_detection() {
        assert_eq!(determine_context("name: ", 6), CompletionContext::TypePosition);
        assert_eq!(determine_context("", 0), CompletionContext::TopLevel);
        assert_eq!(determine_context("struct User {\n    ", 18), CompletionContext::StructBody);
        assert_eq!(
            determine_context("protocol Thing {\n    ", 21),
            CompletionContext::ProtocolBody
        );
        assert_eq!(determine_context("struct ", 7), CompletionContext::DeclarationName);
    }
}
