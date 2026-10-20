//! A dependency's `@unverified` protocols, as `config.idp` sees them.
//!
//! `comline check` refuses a dependency whose `@unverified` protocols the
//! consumer hasn't accepted, for exactly the guarantees they give up. The
//! editor says so on the dependency's entry, and offers a quick fix that
//! writes the `accept_unverified` line `comline accept` would.
//!
//! What a protocol gives up is worked out by core, so the dependency is
//! compiled as its own package (its own settings and default handshake).
//! This crate links core without its `deps` feature, so that compile never
//! fetches anything; a dependency that doesn't compile on its own (one whose
//! schemas import its own dependencies, say) gets no diagnostic here, and
//! `comline check` still has the final say.
//!
//! The fix is never marked preferred: accepting is a choice the consumer
//! writes down, so it shouldn't be applied by a "fix all".

use std::collections::{BTreeMap, HashMap};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use comline_core::package::config::dependency::DependencyConfig;
use comline_core::schema::ir::frozen::unit::Guarantee;
use comline_core::schema::ir::unverified;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, Diagnostic, DiagnosticSeverity,
    NumberOrString, TextEdit, Url, WorkspaceEdit,
};
use serde_json::json;

use crate::util::byte_range_to_lsp_range;

/// The diagnostic code on a dependency entry with an unaccepted protocol.
pub const UNACCEPTED: &str = "dependency::unaccepted_unverified";

/// One `@unverified` protocol in a dependency, as its own compile froze it.
#[derive(Debug, Clone, PartialEq)]
pub struct Unverified {
    pub protocol: String,
    pub set: Vec<Guarantee>,
    pub reason: Option<String>,
}

/// Every `@unverified` protocol in the package at `dir`. Empty when it
/// doesn't compile on its own.
pub fn unverified_in(dir: &Path) -> Vec<Unverified> {
    let compiled = panic::catch_unwind(AssertUnwindSafe(|| comline_core::package::build::compile_package(dir)));
    let Ok(Ok(context)) = compiled else { return vec![] };
    let mut found = Vec::new();
    for schema in &context.schema_contexts {
        let schema = schema.borrow();
        let frozen = schema.frozen_schema.borrow();
        let Some(units) = frozen.as_ref() else { continue };
        for (protocol, set, reason) in unverified::protocols(units) {
            found.push(Unverified {
                protocol: protocol.to_string(),
                set: set.to_vec(),
                reason: reason.map(str::to_string),
            });
        }
    }
    found
}

/// The protocols in `found` that `dependency` hasn't accepted for exactly
/// what they give up - the ones `comline check` refuses.
pub fn unaccepted(dependency: &DependencyConfig, found: Vec<Unverified>) -> Vec<Unverified> {
    found
        .into_iter()
        .filter(|u| dependency.accept_unverified.get(&u.protocol) != Some(&u.set))
        .collect()
}

/// One error per unaccepted protocol, on the dependency's entry (`range`, in
/// `manifest`'s bytes).
pub fn diagnostics(
    manifest: &str,
    range: (usize, usize),
    dependency: &DependencyConfig,
    unaccepted: &[Unverified],
) -> Vec<Diagnostic> {
    unaccepted
        .iter()
        .map(|u| {
            let given_up = unverified::describe(&u.set);
            let reason = u.reason.as_deref().map(|r| format!(": \"{r}\"")).unwrap_or_default();
            let what = match dependency.accept_unverified.get(&u.protocol) {
                None => format!("protocol `{}` is unverified, giving up {given_up}{reason}", u.protocol),
                Some(old) => format!(
                    "protocol `{}` now gives up {given_up}{reason}; it was accepted for {}",
                    u.protocol,
                    if old.is_empty() { "nothing".to_string() } else { unverified::describe(old) }
                ),
            };
            Diagnostic {
                range: byte_range_to_lsp_range(manifest, range.0, range.1),
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String(UNACCEPTED.to_string())),
                source: Some("comline".to_string()),
                message: format!("{what} — `comline check` refuses it until it's accepted"),
                data: Some(json!({ "dependency": dependency.name, "protocol": u.protocol })),
                ..Default::default()
            }
        })
        .collect()
}

/// Quick fixes for the [`UNACCEPTED`] diagnostics in `params`: accept the
/// one protocol, and, when the dependency has more than one unaccepted,
/// accept them all.
pub fn code_actions(
    package_dir: &Path,
    uri: &Url,
    manifest: &str,
    params: &CodeActionParams,
) -> Vec<CodeActionOrCommand> {
    let Some(dependencies) = crate::parser::parse_idp(manifest)
        .ok()
        .and_then(|r| r.document)
        .and_then(|congregation| DependencyConfig::parse_dependencies(&congregation.assignments).ok())
    else {
        return vec![];
    };

    let mut actions = Vec::new();
    let mut offered_all = Vec::new();
    for diagnostic in &params.context.diagnostics {
        if diagnostic.code != Some(NumberOrString::String(UNACCEPTED.to_string())) {
            continue;
        }
        let Some(data) = &diagnostic.data else { continue };
        let (Some(name), Some(protocol)) = (data["dependency"].as_str(), data["protocol"].as_str()) else {
            continue;
        };
        let Some(dependency) = dependencies.get(name) else { continue };
        let Some(dir) = dependency.package_dir(package_dir) else { continue };
        let unaccepted = unaccepted(dependency, unverified_in(&dir));

        if let Some(one) = unaccepted.iter().find(|u| u.protocol == protocol) {
            let title = format!("Accept that `{protocol}` gives up {}", unverified::describe(&one.set));
            actions.extend(action(title, uri, manifest, dependency, &[one], diagnostic));
        }
        if unaccepted.len() > 1 && !offered_all.contains(&name) {
            offered_all.push(name);
            let all: Vec<&Unverified> = unaccepted.iter().collect();
            let title = format!("Accept every unverified protocol in `{name}`");
            actions.extend(action(title, uri, manifest, dependency, &all, diagnostic));
        }
    }
    actions
}

fn action(
    title: String,
    uri: &Url,
    manifest: &str,
    dependency: &DependencyConfig,
    accepting: &[&Unverified],
    diagnostic: &Diagnostic,
) -> Option<CodeActionOrCommand> {
    let mut accepted = dependency.accept_unverified.clone();
    for u in accepting {
        accepted.insert(u.protocol.clone(), u.set.clone());
    }
    let updated = with_acceptance(manifest, &dependency.name, &accepted)?;
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(uri.clone(), vec![edit_between(manifest, &updated)])])),
            document_changes: None,
            change_annotations: None,
        }),
        is_preferred: Some(false),
        ..Default::default()
    }))
}

/// One edit turning `before` into `after`: the span between their common
/// prefix and suffix.
fn edit_between(before: &str, after: &str) -> TextEdit {
    let prefix = before
        .char_indices()
        .zip(after.chars())
        .find(|((_, a), b)| a != b)
        .map_or(before.len().min(after.len()), |((i, _), _)| i);
    let suffix = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let range = byte_range_to_lsp_range(before, prefix, before.len() - suffix);
    TextEdit { range, new_text: after[prefix..after.len() - suffix].to_string() }
}

/// `{ A = ["schema_agreement"] B = [...] }`, as `config.idp` reads it.
fn render(accepted: &BTreeMap<String, Vec<Guarantee>>) -> String {
    let entries: Vec<String> = accepted
        .iter()
        .map(|(protocol, set)| format!("{protocol} = {}", unverified::written(set)))
        .collect();
    format!("{{ {} }}", entries.join(" "))
}

/// `manifest` with `name`'s entry carrying `accept_unverified = accepted`,
/// replacing one already there - the edit `comline accept` makes. Every
/// other byte is left as written.
fn with_acceptance(manifest: &str, name: &str, accepted: &BTreeMap<String, Vec<Guarantee>>) -> Option<String> {
    let tokens = tokens(manifest);
    let (block_open, block_close) = block(&tokens, 0, manifest.len(), "dependencies")?;
    let (entry_open, entry_close) = block(&tokens, block_open, block_close, name)?;
    let line = format!("accept_unverified = {}", render(accepted));

    if let Some((start, end)) = block(&tokens, entry_open, entry_close, "accept_unverified") {
        let key = tokens.iter().rev().find(|&&(t, at)| at < start && t == Token::Word("accept_unverified"))?.1;
        return Some(format!("{}{line}{}", &manifest[..key], &manifest[end + 1..]));
    }

    let line_start = manifest[..entry_close].rfind('\n').map_or(0, |i| i + 1);
    if manifest[line_start..entry_close].trim().is_empty() {
        // `}` on its own line: a new line before it, indented like the fields.
        let close_indent = indent_of(manifest, entry_close);
        let step = if close_indent.starts_with('\t') { "\t" } else { "    " };
        Some(format!("{}{close_indent}{step}{line}\n{}", &manifest[..line_start], &manifest[line_start..]))
    } else {
        // A one-line entry (`a = { path = "../a" }`): keep it on one line.
        Some(format!("{} {line} {}", manifest[..entry_close].trim_end(), &manifest[entry_close..]))
    }
}

/// The `{` and matching `}` of `key = { ... }` directly inside the brackets
/// at `open` and `close` (`0` and the length for the top level).
fn block(tokens: &[(Token<'_>, usize)], open: usize, close: usize, key: &str) -> Option<(usize, usize)> {
    let inside: Vec<_> = tokens
        .iter()
        .copied()
        .filter(|&(_, at)| (open == 0 || at > open) && at < close)
        .collect();
    let mut depth = 0usize;
    for (i, &(token, _)) in inside.iter().enumerate() {
        match token {
            Token::Open => depth += 1,
            Token::Close => depth = depth.saturating_sub(1),
            Token::Word(word) if depth == 0 && word == key => {
                if let [(Token::Equals, _), (Token::Open, block_open), ..] = inside[i + 1..] {
                    return matching(&inside[i + 2..]).map(|block_close| (block_open, block_close));
                }
            }
            _ => {}
        }
    }
    None
}

/// Given tokens starting at an opening bracket, the offset of its match.
fn matching(tokens: &[(Token<'_>, usize)]) -> Option<usize> {
    let mut depth = 0usize;
    for &(token, at) in tokens {
        match token {
            Token::Open => depth += 1,
            Token::Close if depth == 1 => return Some(at),
            Token::Close => depth -= 1,
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    Equals,
    /// `{` or `[`.
    Open,
    /// `}` or `]`.
    Close,
    Other,
}

/// `config.idp`'s tokens with their byte offsets, minus whitespace, comments
/// (`//`, `/* */`) and string contents.
fn tokens(text: &str) -> Vec<(Token<'_>, usize)> {
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'#' | b'@' | b'.');
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + 2 + n + 2);
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                tokens.push((Token::Other, i));
                i += 1;
            }
            b'{' | b'[' => {
                tokens.push((Token::Open, i));
                i += 1;
            }
            b'}' | b']' => {
                tokens.push((Token::Close, i));
                i += 1;
            }
            b'=' => {
                tokens.push((Token::Equals, i));
                i += 1;
            }
            _ if is_word(b) => {
                let start = i;
                while i < bytes.len() && is_word(bytes[i]) {
                    i += 1;
                }
                tokens.push((Token::Word(&text[start..i]), start));
            }
            _ if b.is_ascii_whitespace() => i += 1,
            _ => {
                tokens.push((Token::Other, i));
                i += 1;
            }
        }
    }
    tokens
}

/// The leading whitespace of the line `at` is on.
fn indent_of(text: &str, at: usize) -> &str {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[start..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;

    const HEAD: &str = "congregation app\nspecification_version = 1\n";

    fn both() -> BTreeMap<String, Vec<Guarantee>> {
        BTreeMap::from([(
            "Billing".to_string(),
            vec![Guarantee::SchemaAgreement, Guarantee::EncodingAgreement],
        )])
    }

    fn apply(manifest: &str, edit: &TextEdit) -> String {
        let offset = |p: Position| {
            let line_start: usize = manifest.split_inclusive('\n').take(p.line as usize).map(str::len).sum();
            line_start + p.character as usize
        };
        format!("{}{}{}", &manifest[..offset(edit.range.start)], edit.new_text, &manifest[offset(edit.range.end)..])
    }

    #[test]
    fn a_line_goes_before_the_entrys_closing_brace() {
        let manifest = format!("{HEAD}dependencies = {{\n    billing = {{\n        path = \"../billing\"\n    }}\n}}\n");
        let updated = with_acceptance(&manifest, "billing", &both()).unwrap();
        assert_eq!(
            updated,
            format!(
                "{HEAD}dependencies = {{\n    billing = {{\n        path = \"../billing\"\n        \
                 accept_unverified = {{ Billing = [\"schema_agreement\", \"encoding_agreement\"] }}\n    }}\n}}\n"
            )
        );
        assert_eq!(apply(&manifest, &edit_between(&manifest, &updated)), updated);
    }

    #[test]
    fn a_one_line_entry_stays_on_one_line() {
        let manifest = format!("{HEAD}dependencies = {{\n    billing = {{ path = \"../billing\" }}\n}}\n");
        let updated = with_acceptance(&manifest, "billing", &both()).unwrap();
        assert_eq!(
            updated,
            format!(
                "{HEAD}dependencies = {{\n    billing = {{ path = \"../billing\" accept_unverified = \
                 {{ Billing = [\"schema_agreement\", \"encoding_agreement\"] }} }}\n}}\n"
            )
        );
    }

    #[test]
    fn an_existing_acceptance_is_replaced_in_place() {
        let manifest = format!(
            "{HEAD}dependencies = {{\n    billing = {{\n        accept_unverified = {{ Billing = [\"schema_agreement\"] }}\n        \
             path = \"../billing\"\n    }}\n}}\n"
        );
        let updated = with_acceptance(&manifest, "billing", &both()).unwrap();
        assert_eq!(
            updated,
            format!(
                "{HEAD}dependencies = {{\n    billing = {{\n        accept_unverified = \
                 {{ Billing = [\"schema_agreement\", \"encoding_agreement\"] }}\n        path = \"../billing\"\n    }}\n}}\n"
            )
        );
    }

    #[test]
    fn a_missing_entry_gives_no_edit() {
        let manifest = format!("{HEAD}dependencies = {{\n    other = {{ path = \"../other\" }}\n}}\n");
        assert_eq!(with_acceptance(&manifest, "billing", &both()), None);
        // A `billing` key that isn't directly in the block doesn't count.
        let nested = format!("{HEAD}dependencies = {{\n    other = {{ billing = {{ }} }}\n}}\n");
        assert_eq!(with_acceptance(&nested, "billing", &both()), None);
    }

    #[test]
    fn edit_between_spans_only_what_changed() {
        let edit = edit_between("a = { x }\n", "a = { x y }\n");
        assert_eq!(edit.new_text, "y ");
        assert_eq!(edit.range.start, edit.range.end);
        assert_eq!(edit.range.start, Position { line: 0, character: 8 });
    }
}
