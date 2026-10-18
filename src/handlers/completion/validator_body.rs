//! Completion inside a validator's body, `validator NAME for TYPE { ... }`.
//!
//! The body holds properties (`name: Type [= default]`) and at most one
//! `validate { ... }` block of `assert(condition, "message")` calls. So:
//! - where a member starts: `validate`, unless the body has one already (a
//!   property's name is a new identifier, nothing to suggest);
//! - after a property's `:`: the types, as everywhere;
//! - in `validate { }`, where a statement starts: `assert(...)`;
//! - in an assert's condition: the roots `value` and `params`, then `value`'s
//!   accessors after `value.`, this validator's properties after `params.`,
//!   and `and` / `or` once a comparison is complete.
//!
//! Like the rest of completion this reads tokens, not a parse: a body being
//! typed rarely parses.

use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat};

use comline_core::schema::idl::vocabulary;

use super::{tokenize_prefix, use_path, Tok};

/// Where in a validator's body the cursor is.
#[derive(Debug, PartialEq)]
pub(super) enum Body {
    /// A property's type, after its `:`.
    Type,
    /// Where a body member starts.
    Member { has_validate: bool },
    /// Where a statement of `validate { }` starts.
    Statement,
    /// An operand of a comparison.
    Operand,
    /// After `value.`.
    ValueAccessor { has_length: bool },
    /// After `params.`: the validator's properties, as `(name, type)`.
    Param(Vec<(String, String)>),
    /// After a complete comparison.
    Connective,
    /// Somewhere nothing fits: a property's name, its default, an assert's
    /// message.
    Nothing,
}

const COMPARISON: [char; 4] = ['=', '<', '>', '!'];

/// The validator body context at `offset`, or `None` outside one. `toks` is
/// `cleaned[..offset]` tokenized, the word being typed already dropped.
pub(super) fn context(cleaned: &str, offset: usize, toks: &[(Tok, usize)]) -> Option<Body> {
    let inner = innermost_open_brace(toks, toks.len())?;
    let in_validate = inner > 0 && matches!(toks[inner - 1].0, Tok::Word("validate"));
    let body = if in_validate { innermost_open_brace(toks, inner - 1)? } else { inner };
    let header = validator_header(toks, body)?;

    let at_line_start = {
        let line = &cleaned[cleaned[..offset].rfind('\n').map_or(0, |i| i + 1)..offset];
        // The word being typed doesn't count.
        line.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_').trim().is_empty()
    };
    let last = toks.last().map(|(t, _)| *t);

    if !in_validate {
        return Some(match last {
            Some(Tok::Punct(':')) => Body::Type,
            _ if at_line_start || last == Some(Tok::Punct('{')) => {
                Body::Member { has_validate: body_has_validate(cleaned, toks[body].1, offset) }
            }
            _ => Body::Nothing,
        });
    }

    // In an assert's parentheses: its condition, then after the `,` its
    // message.
    if let Some(open) = unclosed_assert_paren(toks, inner) {
        let after = &toks[open + 1..];
        if after.iter().any(|(t, _)| matches!(t, Tok::Punct(','))) {
            return Some(Body::Nothing);
        }
        let start = after
            .iter()
            .rposition(|(t, _)| matches!(t, Tok::Word("and") | Tok::Word("or")))
            .map_or(0, |i| i + 1);
        return Some(condition(&after[start..], || {
            (properties(cleaned, toks[body].1), targets(cleaned, toks, header, body))
        }));
    }
    Some(match last {
        Some(Tok::Punct('{')) | Some(Tok::Punct(')')) => Body::Statement,
        _ if at_line_start => Body::Statement,
        _ => Body::Nothing,
    })
}

/// One comparison being written, from its first token.
fn condition(
    comparison: &[(Tok, usize)],
    parts: impl FnOnce() -> (Vec<(String, String)>, Vec<String>),
) -> Body {
    let n = comparison.len();
    let at = |back: usize| (back <= n && back > 0).then(|| comparison[n - back].0);
    match (at(2), at(1)) {
        (Some(Tok::Word("value")), Some(Tok::Punct('.'))) => {
            let (_, targets) = parts();
            Body::ValueAccessor { has_length: targets.iter().any(|t| may_have_length(t)) }
        }
        (Some(Tok::Word("params")), Some(Tok::Punct('.'))) => Body::Param(parts().0),
        (_, None) => Body::Operand,
        (_, Some(Tok::Punct(c))) if COMPARISON.contains(&c) => Body::Operand,
        (_, Some(Tok::Word(_))) | (_, Some(Tok::Punct('"')))
            if comparison.iter().any(|(t, _)| matches!(t, Tok::Punct(c) if COMPARISON.contains(c))) =>
        {
            Body::Connective
        }
        _ => Body::Nothing,
    }
}

/// `value.length` exists for a string and an array. A named target may be
/// an alias of either, so it keeps `length` on offer.
fn may_have_length(target: &str) -> bool {
    let target = target.trim();
    target == "string"
        || target.ends_with(']')
        || vocabulary::PRIMITIVES.iter().all(|p| p.name != target)
}

/// The innermost `{` among `toks[..end]` not closed before `end`.
fn innermost_open_brace(toks: &[(Tok, usize)], end: usize) -> Option<usize> {
    let mut depth = 0usize;
    for i in (0..end).rev() {
        match toks[i].0 {
            Tok::Punct('}') => depth += 1,
            Tok::Punct('{') if depth == 0 => return Some(i),
            Tok::Punct('{') => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The index of `validator` when the `{` at `brace` opens a validator's
/// body: `validator NAME for ... {`.
fn validator_header(toks: &[(Tok, usize)], brace: usize) -> Option<usize> {
    let start = toks[..brace].iter().rposition(|(t, _)| {
        matches!(t, Tok::Word("validator") | Tok::Punct('{') | Tok::Punct('}') | Tok::Punct(';'))
    })?;
    matches!(toks[start].0, Tok::Word("validator")).then_some(start)
}

/// The `(` of the `assert(` the cursor is in, after the `validate {` at
/// `block`.
fn unclosed_assert_paren(toks: &[(Tok, usize)], block: usize) -> Option<usize> {
    let mut depth = 0usize;
    for i in (block + 1..toks.len()).rev() {
        match toks[i].0 {
            Tok::Punct(')') => depth += 1,
            Tok::Punct('(') if depth == 0 => {
                return (i > 0 && matches!(toks[i - 1].0, Tok::Word("assert"))).then_some(i);
            }
            Tok::Punct('(') => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The tokens of the validator body opening at byte `open` (just past its
/// `{`), up to its closing `}` or the end of the text, with the depth each
/// is at (1 for the body itself).
fn body_tokens(cleaned: &str, open: usize) -> Vec<(Tok<'_>, usize, usize)> {
    let mut depth = 1usize;
    let mut out = Vec::new();
    for (tok, end) in tokenize_prefix(&cleaned[open..], cleaned.len() - open) {
        match tok {
            Tok::Punct('{') => depth += 1,
            Tok::Punct('}') if depth == 1 => break,
            Tok::Punct('}') => depth -= 1,
            _ => {}
        }
        out.push((tok, end + open, depth));
    }
    out
}

/// Whether the body already has a `validate` block, other than a `validate`
/// being typed at `offset`.
fn body_has_validate(cleaned: &str, open: usize, offset: usize) -> bool {
    body_tokens(cleaned, open)
        .iter()
        .any(|(t, end, depth)| *depth == 1 && matches!(t, Tok::Word("validate")) && *end != offset)
}

/// The body's properties, as `(name, type)`.
fn properties(cleaned: &str, open: usize) -> Vec<(String, String)> {
    let toks = body_tokens(cleaned, open);
    let mut properties = Vec::new();
    for pair in toks.windows(2) {
        if let ((Tok::Word(name), _, 1), (Tok::Punct(':'), colon, 1)) = (pair[0], pair[1]) {
            // The type runs to the end of the line, or to `= default`.
            let line = cleaned[colon..].lines().next().unwrap_or("");
            let ty = line.split('=').next().unwrap_or("").trim().to_string();
            properties.push((name.to_string(), ty));
        }
    }
    properties
}

/// The header's targets, `validator NAME for A, B {`, as written.
fn targets(cleaned: &str, toks: &[(Tok, usize)], header: usize, brace: usize) -> Vec<String> {
    let Some(&(Tok::Word("for"), after_for)) = toks.get(header + 2) else {
        return Vec::new();
    };
    // Up to just before the `{`.
    let text = &cleaned[after_for..toks[brace].1 - 1];
    text.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
}

/// The items for `body`; `None` for a type position, which the caller
/// completes as everywhere else.
pub(super) fn completions(body: Body) -> Option<Vec<CompletionItem>> {
    Some(match body {
        Body::Type => return None,
        Body::Member { has_validate: false } => vec![snippet(
            "validate",
            "validate {\n\tassert($1, \"$2\")\n}",
        )],
        Body::Member { has_validate: true } | Body::Nothing => Vec::new(),
        Body::Statement => vec![snippet("assert", "assert($1, \"$2\")$0")],
        Body::Operand => vec![
            CompletionItem {
                label: "value".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("The value being checked".to_string()),
                ..Default::default()
            },
            CompletionItem {
                label: "params".to_string(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some("This validator's properties".to_string()),
                insert_text: Some("params.".to_string()),
                command: Some(use_path::trigger_suggest()),
                ..Default::default()
            },
        ],
        Body::ValueAccessor { has_length } => {
            let mut items = Vec::new();
            if has_length {
                items.push(accessor("length", "Its length: a string's characters, an array's elements"));
            }
            items.push(accessor("name", "The checked field's name, for a message"));
            items
        }
        Body::Param(properties) => properties
            .into_iter()
            .map(|(name, ty)| CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::FIELD),
                detail: (!ty.is_empty()).then_some(ty),
                ..Default::default()
            })
            .collect(),
        Body::Connective => ["and", "or"].into_iter().map(keyword).collect(),
    })
}

fn snippet(label: &str, text: &str) -> CompletionItem {
    CompletionItem {
        insert_text: Some(text.to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..keyword(label)
    }
}

fn keyword(label: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: vocabulary::keyword(label).map(|k| k.description.to_string()),
        ..Default::default()
    }
}

fn accessor(label: &str, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(CompletionItemKind::PROPERTY),
        detail: Some(detail.to_string()),
        ..Default::default()
    }
}
