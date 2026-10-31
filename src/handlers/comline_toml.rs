//! Completion, hover and code actions for `comline.toml`, over
//! [`analysis::comline_toml`](crate::analysis::comline_toml).

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionDisabled, CodeActionKind, CodeActionOrCommand, CodeActionParams, CompletionItem,
    CompletionItemKind, Documentation, Hover, HoverContents, InsertTextFormat, MarkupContent, MarkupKind, Position,
    TextEdit, Url, WorkspaceEdit,
};
use serde_json::Value as Json;

use crate::analysis::comline_toml::{self as toml, Context, KnownEncoding};
use crate::analysis::generation;
use crate::util::{byte_range_to_lsp_range, offset_to_position, position_to_offset};

/// Hover: a key's description from the schema; for an `encodings` key, the
/// encoding's declaration.
pub fn hover(text: &str, position: Position, context: &Context) -> Option<Hover> {
    let offset = position_to_offset(text, position)?;
    let analysis = toml::analyse(text);
    let entry = analysis.entry_at(offset)?;
    let on_key = entry.key.0 <= offset && offset <= entry.key.1;
    let value = if toml::is_encoding_key(&entry.path) {
        let name = entry.path.last()?;
        match context.encoding(name) {
            Some(encoding) => toml::describe_encoding(&encoding),
            None => format!("**`{name}`** - no package in view declares this encoding"),
        }
    } else {
        let key = entry.path.last()?;
        let description = toml::description(&entry.path)?;
        let mut value = format!("**`{key}`**\n\n{description}");
        if key == "language" && !context.languages.is_empty() {
            let declared: Vec<String> =
                context.languages.iter().map(|d| format!("`{}#{}`", d.language, d.version)).collect();
            value.push_str(&format!("\n\nconfig.idp declares {}.", declared.join(", ")));
        }
        value
    };
    let span = if on_key { entry.key } else { entry.value.unwrap_or(entry.key) };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }),
        range: Some(byte_range_to_lsp_range(text, span.0, span.1)),
    })
}

/// The table the line at `line` is in, as a schema path, from the nearest
/// header above it: `[generate]` -> `generate`, `[[generate.target]]` ->
/// `generate.target`, the top level -> empty.
fn table_path(lines: &[&str], line: usize) -> Vec<String> {
    for l in lines[..line].iter().rev() {
        let t = l.trim();
        if t.starts_with('[') {
            let name = t.trim_start_matches('[').split(']').next().unwrap_or_default();
            return name.split('.').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect();
        }
    }
    vec![]
}

/// The `language = "..."` of the `[[generate.target]]` block `line` is in.
fn block_language(lines: &[&str], line: usize) -> Option<String> {
    let start = (0..=line.min(lines.len().saturating_sub(1))).rev().find(|i| lines[*i].trim().starts_with('['))?;
    lines[start].trim().starts_with("[[generate.target")
        .then(|| {
            lines[start + 1..]
                .iter()
                .take_while(|l| !l.trim().starts_with('['))
                .find_map(|l| {
                    let (k, v) = l.split_once('=')?;
                    (k.trim() == "language").then(|| v.trim().trim_matches('"').to_string())
                })
        })
        .flatten()
}

/// Completion at `position`: keys at each level (with their descriptions),
/// enum values, `language` from config.idp, and `encodings` keys from the
/// declarations in view.
pub fn completion(text: &str, position: Position, context: &Context) -> Vec<CompletionItem> {
    let lines: Vec<&str> = text.split('\n').collect();
    let line_index = position.line as usize;
    let Some(line) = lines.get(line_index) else { return vec![] };
    let before: String = line.chars().take(position.character as usize).collect();
    let trimmed = before.trim_start();

    if trimmed.starts_with('[') {
        return ["generate", "generate.target", "generate.encodings"]
            .iter()
            .map(|name| CompletionItem {
                label: name.to_string(),
                kind: Some(CompletionItemKind::MODULE),
                ..Default::default()
            })
            .collect();
    }

    let mut path = table_path(&lines, line_index);
    let language = block_language(&lines, line_index)
        .or_else(|| context.languages.first().map(|d| d.language.clone()));

    // Inside `encodings = { ... }` on this line: its keys.
    if let Some((key, rest)) = trimmed.split_once('=') {
        let key = key.trim();
        if key == "encodings" && rest.contains('{') {
            let tail = rest.rsplit(['{', ',']).next().unwrap_or_default();
            if !tail.contains('=') {
                return encoding_items(context, language.as_deref(), true);
            }
            return vec![];
        }
        // A value.
        path.push(key.to_string());
        if key == "language" || (path.len() == 2 && key == "only") {
            return context
                .languages
                .iter()
                .map(|d| CompletionItem {
                    label: d.language.clone(),
                    kind: Some(CompletionItemKind::ENUM_MEMBER),
                    detail: Some(format!("{}#{} - declared in config.idp", d.language, d.version)),
                    insert_text: Some(if rest.contains('"') { d.language.clone() } else { format!("\"{}\"", d.language) }),
                    ..Default::default()
                })
                .collect();
        }
        let quoted = rest.contains('"');
        return toml::values_at(&path)
            .into_iter()
            .map(|v| CompletionItem {
                insert_text: Some(if quoted { v.trim_matches('"').to_string() } else { v.clone() }),
                label: v.trim_matches('"').to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                ..Default::default()
            })
            .collect();
    }

    // A key.
    if matches!(path.last().map(String::as_str), Some("encodings")) {
        return encoding_items(context, language.as_deref(), false);
    }
    toml::keys_at(&path)
        .into_iter()
        .filter(|(k, _)| !(path.is_empty() && k != "generate"))
        .map(|(key, doc)| CompletionItem {
            label: key.clone(),
            kind: Some(CompletionItemKind::PROPERTY),
            documentation: (!doc.is_empty())
                .then(|| Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value: doc })),
            insert_text: Some(format!("{key} = ")),
            ..Default::default()
        })
        .collect()
}

/// One item per encoding in view, its value the declaring package's
/// suggested mapping for `language` as a snippet.
fn encoding_items(context: &Context, language: Option<&str>, inline: bool) -> Vec<CompletionItem> {
    context
        .all_encodings()
        .into_iter()
        .map(|encoding: KnownEncoding| {
            let suggested = language.and_then(|l| encoding.map.get(l));
            let value = match suggested {
                Some(m) => match (&m.package, &m.version) {
                    (Some(p), Some(v)) => format!("{{ type = \"${{1:{}}}\", package = \"{p}\", version = \"{v}\" }}", m.ty),
                    (Some(p), None) => format!("{{ type = \"${{1:{}}}\", package = \"{p}\" }}", m.ty),
                    _ => format!("\"${{1:{}}}\"", m.ty),
                },
                None => "\"${1:type}\"".to_string(),
            };
            let separator = if inline { " " } else { "" };
            CompletionItem {
                label: encoding.name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(format!("{:?} - {}", encoding.kind, encoding.package).to_lowercase()),
                documentation: Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: toml::describe_encoding(&encoding),
                })),
                insert_text: Some(format!("\"{}\" = {value}{separator}", encoding.name)),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            }
        })
        .collect()
}

/// Code actions for `comline.toml`: the quick fixes the diagnostics carry
/// (`data`), and "Check generator" on a `generator` entry.
///
/// `manifest` is the package's `config.idp` (its URL and text), for the fix
/// declaring a language there.
pub fn code_actions(
    text: &str,
    uri: &Url,
    params: &CodeActionParams,
    manifest: Option<(&Url, &str)>,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    for diagnostic in &params.context.diagnostics {
        let Some(data) = &diagnostic.data else { continue };
        let candidates: Vec<&str> =
            data.get("candidates").and_then(Json::as_array).into_iter().flatten().filter_map(Json::as_str).collect();
        for (i, candidate) in candidates.iter().enumerate() {
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Change to {candidate}"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(edit(uri, vec![TextEdit { range: diagnostic.range, new_text: candidate.to_string() }])),
                is_preferred: Some(i == 0),
                ..Default::default()
            }));
        }
        if data.get("fix").and_then(Json::as_str) == Some(toml::FIX_DECLARE_LANGUAGE) {
            let language = data.get("language").and_then(Json::as_str).unwrap_or_default();
            if let (Some((manifest_uri, manifest_text)), Some(version)) = (manifest, generation::floor(language)) {
                let language = language.to_lowercase();
                let new_text = declare_language(manifest_text, &language, version);
                let end = offset_to_position(manifest_text, manifest_text.len());
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Declare `{language}#{version}` in config.idp"),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diagnostic.clone()]),
                    edit: Some(edit(
                        manifest_uri,
                        vec![TextEdit { range: lsp_types::Range { start: Position::new(0, 0), end }, new_text }],
                    )),
                    ..Default::default()
                }));
            }
        }
    }

    // "Check generator": on request only, and only ever sandboxed.
    if let Some(offset) = position_to_offset(text, params.range.start) {
        let analysis = toml::analyse(text);
        if let Some(entry) = analysis.entry_at(offset).filter(|e| e.path.last().is_some_and(|k| k == "generator")) {
            let written = entry.text.clone().unwrap_or_default();
            actions.push(CodeActionOrCommand::CodeAction(check_generator(&written)));
        }
    }
    actions
}

/// "Check generator" on `generator = "<written>"`: would run its `Describe`,
/// sandboxed as `comline generate` runs a `.wasm`. The language server
/// doesn't embed that sandbox (wasmtime), and never runs a native
/// generator, so the action is offered disabled, saying why.
pub fn check_generator(written: &str) -> CodeAction {
    let reason = if written.ends_with(".wasm") {
        "not available in the editor yet: the language server doesn't embed the wasm sandbox. Run \
         `comline generate`, which describes it sandboxed"
            .to_string()
    } else {
        "only a `.wasm` generator can be checked, sandboxed - the editor never runs a native generator".to_string()
    };
    CodeAction {
        title: "Check generator".to_string(),
        kind: Some(CodeActionKind::EMPTY),
        disabled: Some(CodeActionDisabled { reason }),
        ..Default::default()
    }
}

fn edit(uri: &Url, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit { changes: Some(HashMap::from([(uri.clone(), edits)])), ..Default::default() }
}

/// The matching `}` of the `{` at `open`.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    for (i, c) in text[open..].char_indices() {
        match c {
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `{` opening `key = {` at or after `from`, within `until`.
fn block_of(text: &str, key: &str, from: usize, until: usize) -> Option<(usize, usize)> {
    let mut at = from;
    while let Some(found) = text[at..until].find(key) {
        let start = at + found;
        let rest = &text[start + key.len()..until];
        let before_ok = start == 0 || !text[..start].ends_with(|c: char| c.is_alphanumeric() || c == '_');
        let trimmed = rest.trim_start();
        if before_ok && trimmed.starts_with('=') && trimmed[1..].trim_start().starts_with('{') {
            let open = start + key.len() + (rest.len() - trimmed.len()) + 1 + (trimmed[1..].len() - trimmed[1..].trim_start().len());
            return Some((open, matching_brace(text, open)?));
        }
        at = start + key.len();
    }
    None
}

/// `manifest` with `language#version = {}` added to
/// `code_generation.languages`, creating either block when missing.
pub fn declare_language(manifest: &str, language: &str, version: &str) -> String {
    let entry = format!("{language}#{version} = {{}}");
    let Some((cg_open, cg_close)) = block_of(manifest, "code_generation", 0, manifest.len()) else {
        let separator = if manifest.ends_with('\n') || manifest.is_empty() { "" } else { "\n" };
        return format!("{manifest}{separator}\ncode_generation = {{\n    languages = {{\n        {entry}\n    }}\n}}\n");
    };
    match block_of(manifest, "languages", cg_open, cg_close) {
        Some((_, close)) => {
            let line_start = manifest[..close].rfind('\n').map_or(0, |n| n + 1);
            let indent: String = manifest[line_start..close].chars().take_while(|c| c.is_whitespace()).collect();
            if manifest[line_start..close].trim().is_empty() {
                format!("{}{indent}    {entry}\n{}", &manifest[..line_start], &manifest[line_start..])
            } else {
                format!("{}\n{indent}    {entry}\n{indent}{}", manifest[..close].trim_end(), &manifest[close..])
            }
        }
        None => format!(
            "{}\n    languages = {{\n        {entry}\n    }}{}",
            &manifest[..=cg_open],
            &manifest[cg_open + 1..]
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::comline_toml::{diagnostics, KnownEncoding};
    use crate::analysis::source::DeclaredLanguage;
    use comline_core::encodings::{EncodingKind, EncodingMapping as Suggested};
    use lsp_types::{CodeActionContext, PartialResultParams, Range, TextDocumentIdentifier, WorkDoneProgressParams};
    use std::collections::BTreeMap;

    fn context() -> Context {
        Context {
            has_manifest: true,
            languages: vec![DeclaredLanguage { language: "rust".into(), version: "1.85.0".into() }],
            encodings: vec![KnownEncoding {
                name: "com.acme.lines@1".into(),
                kind: EncodingKind::Framing,
                package: "acme".into(),
                map: BTreeMap::from([(
                    "rust".to_string(),
                    Suggested { ty: "acme::Lines".into(), package: Some("acme".into()), version: Some("^1".into()) },
                )]),
            }],
            used: Some(Default::default()),
            base_dir: None,
        }
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn keys_complete_at_each_level_with_their_descriptions() {
        let items = completion("[generate]\n", Position::new(1, 0), &context());
        assert!(labels(&items).contains(&"mode") && labels(&items).contains(&"encodings"), "{:?}", labels(&items));
        assert!(!labels(&items).contains(&"language"));
        let mode = items.iter().find(|i| i.label == "mode").unwrap();
        assert!(matches!(&mode.documentation, Some(Documentation::MarkupContent(m)) if m.value.contains("Emit form")));
        let items = completion("[[generate.target]]\n", Position::new(1, 0), &context());
        assert!(labels(&items).contains(&"language") && labels(&items).contains(&"flatten"));
        let items = completion("", Position::new(0, 0), &context());
        assert_eq!(labels(&items), ["generate"]);
    }

    #[test]
    fn enum_values_complete() {
        let items = completion("[generate]\nmode = ", Position::new(1, 7), &context());
        assert_eq!(labels(&items), ["code", "lib"]);
        assert_eq!(items[0].insert_text.as_deref(), Some("\"code\""));
        let items = completion("[[generate.target]]\nvalidate_on_decode = \"", Position::new(1, 22), &context());
        assert_eq!(labels(&items), ["off", "collect", "first"]);
        assert_eq!(items[0].insert_text.as_deref(), Some("off"));
    }

    #[test]
    fn language_completes_from_config_idp() {
        let items = completion("[[generate.target]]\nlanguage = ", Position::new(1, 11), &context());
        assert_eq!(labels(&items), ["rust"]);
        assert!(items[0].detail.as_deref().unwrap().contains("rust#1.85.0"));
    }

    #[test]
    fn encodings_keys_complete_with_the_suggested_mapping() {
        let text = "[[generate.target]]\nlanguage = \"rust\"\nencodings = { ";
        let items = completion(text, Position::new(2, 14), &context());
        let lines = items.iter().find(|i| i.label == "com.acme.lines@1").expect("declared encoding offered");
        assert_eq!(
            lines.insert_text.as_deref(),
            Some("\"com.acme.lines@1\" = { type = \"${1:acme::Lines}\", package = \"acme\", version = \"^1\" } ")
        );
        assert!(labels(&items).contains(&"packed@1"), "the official ones too: {:?}", labels(&items));
        let items = completion("[generate.encodings]\n", Position::new(1, 0), &context());
        assert!(labels(&items).contains(&"com.acme.lines@1"));
    }

    #[test]
    fn hover_shows_a_key_description() {
        let hover = hover("[generate]\nmode = \"lib\"\n", Position::new(1, 1), &context()).unwrap();
        let HoverContents::Markup(m) = hover.contents else { panic!() };
        assert!(m.value.starts_with("**`mode`**") && m.value.contains("Emit form"), "{}", m.value);
        let hover = hover_text("[[generate.target]]\nlanguage = \"rust\"\n", Position::new(1, 2));
        assert!(hover.contains("config.idp declares `rust#1.85.0`"), "{hover}");
    }

    fn hover_text(text: &str, position: Position) -> String {
        match hover(text, position, &context()).unwrap().contents {
            HoverContents::Markup(m) => m.value,
            _ => panic!(),
        }
    }

    #[test]
    fn hover_on_an_encodings_key_shows_its_declaration() {
        let text = "[generate]\nencodings = { \"com.acme.lines@1\" = \"x\" }\n";
        let value = hover_text(text, Position::new(1, 18));
        assert!(value.contains("a framing, declared by acme"), "{value}");
        assert!(value.contains("rust: `acme::Lines` from `acme` `^1`"), "{value}");
    }

    fn params(range: Range, diagnostics: Vec<lsp_types::Diagnostic>) -> CodeActionParams {
        CodeActionParams {
            text_document: TextDocumentIdentifier { uri: Url::parse("file:///p/comline.toml").unwrap() },
            range,
            context: CodeActionContext { diagnostics, only: None, trigger_kind: None },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        }
    }

    fn titles(actions: &[CodeActionOrCommand]) -> Vec<String> {
        actions
            .iter()
            .map(|a| match a {
                CodeActionOrCommand::CodeAction(a) => a.title.clone(),
                CodeActionOrCommand::Command(c) => c.title.clone(),
            })
            .collect()
    }

    #[test]
    fn a_typo_is_fixed_to_the_closest_key_or_value() {
        let uri = Url::parse("file:///p/comline.toml").unwrap();
        let text = "[generate]\nmdoe = \"lbi\"\n";
        let d = diagnostics(text, &context());
        let actions = code_actions(text, &uri, &params(d[0].range, d.clone()), None);
        assert_eq!(titles(&actions), ["Change to mode"]);
        let CodeActionOrCommand::CodeAction(a) = &actions[0] else { panic!() };
        let edits = &a.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri];
        assert_eq!(edits[0].range, d[0].range);
        let text = "[generate]\nmode = \"lbi\"\n";
        let d = diagnostics(text, &context());
        assert_eq!(titles(&code_actions(text, &uri, &params(d[0].range, d.clone()), None)), ["Change to \"lib\""]);
        let text = "[[generate.target]]\nlanguage = \"rsut\"\n";
        let d = diagnostics(text, &context());
        let manifest_uri = Url::parse("file:///p/config.idp").unwrap();
        let actions = code_actions(text, &uri, &params(d[0].range, d.clone()), Some((&manifest_uri, "congregation p\n")));
        assert_eq!(titles(&actions)[0], "Change to \"rust\"");
    }

    #[test]
    fn an_undeclared_language_is_declared_in_config_idp() {
        let uri = Url::parse("file:///p/comline.toml").unwrap();
        let manifest_uri = Url::parse("file:///p/config.idp").unwrap();
        let manifest = "congregation p\n\ncode_generation = {\n    languages = {\n        rust#1.85.0 = {}\n    }\n}\n";
        let text = "[[generate.target]]\nlanguage = \"typescript\"\n";
        let d = diagnostics(text, &context());
        let actions = code_actions(text, &uri, &params(d[0].range, d.clone()), Some((&manifest_uri, manifest)));
        let floor = generation::floor("typescript").unwrap();
        assert_eq!(titles(&actions), [format!("Declare `typescript#{floor}` in config.idp")]);
        let CodeActionOrCommand::CodeAction(a) = &actions[0] else { panic!() };
        let new_text = &a.edit.as_ref().unwrap().changes.as_ref().unwrap()[&manifest_uri][0].new_text;
        assert_eq!(
            new_text,
            &format!(
                "congregation p\n\ncode_generation = {{\n    languages = {{\n        rust#1.85.0 = {{}}\n        typescript#{floor} = {{}}\n    }}\n}}\n"
            )
        );
    }

    #[test]
    fn declaring_a_language_creates_the_blocks_it_needs() {
        assert_eq!(
            declare_language("congregation p\n", "rust", "1.85.0"),
            "congregation p\n\ncode_generation = {\n    languages = {\n        rust#1.85.0 = {}\n    }\n}\n"
        );
        assert_eq!(
            declare_language("congregation p\ncode_generation = {\n}\n", "rust", "1.85.0"),
            "congregation p\ncode_generation = {\n    languages = {\n        rust#1.85.0 = {}\n    }\n}\n"
        );
        assert_eq!(
            declare_language("congregation p\ncode_generation = { languages = { rust#1.85.0 = {} } }\n", "typescript", "5.0"),
            "congregation p\ncode_generation = { languages = { rust#1.85.0 = {}\n    typescript#5.0 = {}\n} }\n"
        );
    }

    #[test]
    fn check_generator_is_offered_disabled_and_never_runs_anything() {
        let uri = Url::parse("file:///p/comline.toml").unwrap();
        let text = "[[generate.target]]\nlanguage = \"rust\"\ngenerator = \"gen.wasm\"\n";
        let at = Range::new(Position::new(2, 14), Position::new(2, 14));
        let actions = code_actions(text, &uri, &params(at, vec![]), None);
        let [CodeActionOrCommand::CodeAction(a)] = actions.as_slice() else { panic!("{actions:?}") };
        assert_eq!(a.title, "Check generator");
        assert!(a.edit.is_none() && a.command.is_none());
        assert!(a.disabled.as_ref().unwrap().reason.contains("wasm sandbox"));
        let native = check_generator("./gen");
        assert!(native.disabled.unwrap().reason.contains("never runs a native generator"));
        let at = Range::new(Position::new(1, 2), Position::new(1, 2));
        assert!(code_actions(text, &uri, &params(at, vec![]), None).is_empty());
    }
}
