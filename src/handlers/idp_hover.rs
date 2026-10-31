// Hover for `.idp` `settings = { ... }` blocks — the one `.idp` construct
// that gets real, server-side, position-based hover. Everything else in
// `.idp` keeps using the client-side text-heuristic (`idpSchema.ts`) —
// see `backend.rs::hover`'s narrowed `is_idp` guard, which only forwards
// here for cursor positions actually inside a `settings` assignment and
// returns `None` (deferring to the client) otherwise.
//
// Strictly pre-freeze: this never calls `interpret_context`/`freezing` —
// `comline-core`'s `.idp` interpreter `panic!`s on malformed input, which
// would crash the server on a bad keystroke (see `parser.rs`'s doc
// comment on `IdpParseResult`). Position resolution works entirely
// against the raw parsed `Congregation`, the same way `.ids`'s hover.rs
// works against the raw `Document`, never touching `FrozenUnit`/IR.

use crate::parser;
use crate::util::{in_comment_or_string, position_to_offset};
use comline_core::package::config::idl::grammar::{Assignment, Key, Value};
use rust_sitter::Spanned;
use comline_core::settings::catalog;
use lsp_types::{Hover, HoverContents, MarkedString, Position};

pub fn get_idp_hover_info(source: &str, position: Position) -> Option<Hover> {
    let offset = position_to_offset(source, position)?;
    if in_comment_or_string(source, offset) {
        return None;
    }

    let congregation = parser::parse_idp(source).ok()?.document?;

    if let Some(hover) = language_hover(&congregation.assignments, offset) {
        return Some(hover);
    }

    let top = congregation.assignments.iter().find(|a| {
        matches!(&a.value.key, Key::Identifier(id) if id.value == "settings")
            && offset >= a.span.0
            && offset < a.span.1
    })?;

    let mut path = vec!["settings".to_string()];
    let mut current = top;
    while let Value::Dictionary(dict) = &current.value.value {
        match dict.assignments.iter().find(|a| offset >= a.span.0 && offset < a.span.1) {
            Some(child) => {
                path.push(key_text(&child.value.key));
                current = child;
            }
            None => break,
        }
    }

    Some(if matches!(&current.value.value, Value::Dictionary(_)) {
        group_hover(&path)
    } else {
        entry_hover(&path, &current.value.value)
    })
}

/// Cursor on an entry of `code_generation.languages` (`rust#1.85.0 = {...}`):
/// what that language's generator supports, from its own `SUPPORT` table.
/// `None` elsewhere, or for a language this build has no generator for.
fn language_hover(assignments: &[Spanned<Assignment>], offset: usize) -> Option<Hover> {
    let generation = dictionary(containing(assignments, offset, Some("code_generation"))?)?;
    let languages = dictionary(containing(generation, offset, Some("languages"))?)?;
    let entry = containing(languages, offset, None)?;
    let key = key_text(&entry.key);
    let (language, version) = key.split_once('#')?;
    let summary = crate::analysis::generation::summary(language, version)?;
    Some(Hover { contents: HoverContents::Array(vec![MarkedString::from_markdown(summary)]), range: None })
}

/// The assignment in `assignments` the cursor is in - named `key`, if given.
fn containing<'a>(assignments: &'a [Spanned<Assignment>], offset: usize, key: Option<&str>) -> Option<&'a Assignment> {
    assignments
        .iter()
        .find(|a| offset >= a.span.0 && offset < a.span.1 && key.is_none_or(|k| key_text(&a.value.key) == k))
        .map(|a| &a.value)
}

/// The entries of `assignment`'s value, when it's a `{ ... }`.
fn dictionary(assignment: &Assignment) -> Option<&[Spanned<Assignment>]> {
    match &assignment.value {
        Value::Dictionary(d) => Some(&d.assignments),
        _ => None,
    }
}

fn key_text(key: &Key) -> String {
    key.text()
}

/// A rendering of a `Value` that never panics on anything, unlike
/// `freezing::interpret_settings_value` — this runs on every keystroke,
/// including malformed/mid-edit text.
fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.value.clone(),
        Value::Number(n) => n.value.clone(),
        Value::Boolean(b) => b.as_text().to_string(),
        Value::Identifier(id) => id.value.clone(),
        Value::Namespaced(ns) => ns.value.clone(),
        Value::Variable(v) => v.value.clone(),
        Value::List(_) => "[...]".to_string(),
        Value::Dictionary(_) => "{...}".to_string(),
    }
}

/// Cursor on a leaf `key = value` — catalog doc (if this path matches a
/// recognized shape) plus the raw value. No separate "effective" line:
/// `.idp`'s own settings dict is the base/root layer with nothing above
/// it to merge today, so "raw" and "effective" would always print
/// identically — revisit once a package-level default-policy dict (the
/// deferred decision) actually layers underneath this.
fn entry_hover(path: &[String], value: &Value) -> Hover {
    let key = path.join(".");
    let mut contents = vec![MarkedString::from_language_code(
        "comline".to_string(),
        format!("{key} = {}", render_value(value)),
    )];

    // `path`'s leading `settings` is synthetic - it names the `.idp`
    // assignment this dict hangs off, and exists for the signature line
    // above. The registry's keys are relative to the inside of a settings
    // block, and its matcher is anchored at the front, so that segment has
    // to come off before lookup. Leaving it on silently lost the doc for
    // every key here: a three-segment path like
    // `settings.validators.allowed` no longer looked like `<group>.<leaf>`
    // to the matcher, and `diagnostics."<code>".level` never had a chance
    // at four.
    let segments: Vec<&str> = path.iter().skip(1).map(String::as_str).collect();
    if let Some(m) = catalog::match_path(&segments) {
        contents.push(MarkedString::from_markdown(catalog::doc_for(&m).description.to_string()));
    }

    Hover { contents: HoverContents::Array(contents), range: None }
}

/// Cursor on `settings` itself or on any intermediate group key (e.g.
/// `validators`) — not a specific leaf. Shows the recognized-shapes
/// catalog, the same listing `.ids`'s block-level settings hover shows.
fn group_hover(path: &[String]) -> Hover {
    let signature = path.join(".");
    let catalog_lines: Vec<String> =
        catalog::block_summary().iter().map(|f| format!("- `{}` - {}", f.key_pattern, f.summary)).collect();

    let contents = vec![
        MarkedString::from_language_code("comline".to_string(), signature),
        MarkedString::from_markdown(format!("**recognized keys**\n{}", catalog_lines.join("\n"))),
    ];

    Hover { contents: HoverContents::Array(contents), range: None }
}

#[cfg(test)]
mod tests {

    #[test]
    fn hovering_a_declared_language_shows_what_its_generator_supports() {
        let source = "congregation app\nspecification_version = 1\n\ncode_generation = {\n    languages = {\n        rust#1.85.0 = {}\n    }\n}\n";
        let hover = get_idp_hover_info(source, Position::new(5, 10)).expect("a hover on `rust#1.85.0`");
        let HoverContents::Array(parts) = hover.contents else { panic!("an array") };
        let MarkedString::String(text) = &parts[0] else { panic!("markdown") };
        assert!(text.contains("**`rust#1.85.0`** supports"), "{text}");
        assert!(text.contains("- unions: "), "{text}");

        // A language this build can't generate: nothing to say.
        let other = source.replace("rust#1.85.0", "python#3.11.0");
        assert!(get_idp_hover_info(&other, Position::new(5, 10)).is_none());
    }

    use super::*;
    use crate::util::offset_to_position;

    fn hover_text(hover: Hover) -> String {
        match hover.contents {
            HoverContents::Array(parts) => parts
                .into_iter()
                .map(|p| match p {
                    MarkedString::String(s) => s,
                    MarkedString::LanguageString(ls) => ls.value,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            HoverContents::Scalar(MarkedString::String(s)) => s,
            HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value,
            HoverContents::Markup(m) => m.value,
        }
    }

    #[test]
    fn hover_on_the_settings_keyword_shows_the_group_catalog() {
        let source = "congregation acme\nsettings = {\n    validators = {\n        allowed = false\n    }\n}\n";
        let offset = source.find("settings").unwrap();
        let position = offset_to_position(source, offset);

        let hover = get_idp_hover_info(source, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("recognized keys"), "got: {text}");
    }

    #[test]
    fn hover_on_a_nested_group_key_shows_the_group_catalog() {
        let source = "congregation acme\nsettings = {\n    validators = {\n        allowed = false\n    }\n}\n";
        let offset = source.find("validators").unwrap();
        let position = offset_to_position(source, offset);

        let hover = get_idp_hover_info(source, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("settings.validators"), "got: {text}");
        assert!(text.contains("recognized keys"), "got: {text}");
    }

    #[test]
    fn hover_on_a_leaf_entry_shows_the_catalog_doc_and_raw_value() {
        let source = "congregation acme\nsettings = {\n    validators = {\n        allowed = false\n    }\n}\n";
        let offset = source.find("allowed").unwrap();
        let position = offset_to_position(source, offset);

        let hover = get_idp_hover_info(source, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("settings.validators.allowed = false"), "got: {text}");
        assert!(text.contains("permitted"), "got: {text}");
    }

    #[test]
    fn hover_on_a_non_settings_key_returns_none() {
        // Defers to the client-side idpSchema.ts for every other key.
        let source = "congregation acme\ndependencies = {\n    foo = { path = \"../foo\" }\n}\n";
        let offset = source.find("dependencies").unwrap();
        let position = offset_to_position(source, offset);

        assert!(get_idp_hover_info(source, position).is_none());
    }

    #[test]
    fn malformed_mid_edit_text_returns_none_without_panicking() {
        let source = "congregation acme\nsettings = {";
        let offset = source.find("settings").unwrap();
        let position = offset_to_position(source, offset);

        assert!(get_idp_hover_info(source, position).is_none());
    }

    #[test]
    fn a_comment_containing_the_word_settings_returns_none() {
        let source = "congregation acme\n// settings = { allowed = false }\n";
        let offset = source.find("settings").unwrap();
        let position = offset_to_position(source, offset);

        assert!(get_idp_hover_info(source, position).is_none());
    }
}
