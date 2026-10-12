// Code actions handler - provides quick fixes and refactorings

use crate::analysis::diagnostics::{
    DUPLICATE_BLOCK, DUPLICATE_DECLARATION, MISSING_IMPORT, UNRESOLVED_IMPORT,
};
use crate::analysis::import_check;
use crate::analysis::imports::add_use_edit;
use crate::analysis::project::Project;
use crate::analysis::source::ProjectSource;
use crate::analysis::symbols::{Namespace, Symbol};
use crate::handlers::rename;
use crate::util::byte_range_to_lsp_range;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, Command, Diagnostic,
    NumberOrString, Range, SymbolKind, TextEdit, Url, WorkspaceEdit,
};
use serde_json::json;
use std::collections::HashMap;

/// Get code actions for a given range, considering only this file.
pub fn get_code_actions(source: &str, uri: &Url, params: &CodeActionParams) -> Vec<CodeActionOrCommand> {
    get_code_actions_with_project::<(Url, String)>(source, uri, params, &[])
}

/// Get code actions for a given range, with `other_files` (every other
/// file in the package, plus its dependencies' and its `config.idp`) as the
/// project: one "Add `use ...`" quick fix per file that declares a type used
/// in range without being imported (the missing-import diagnostic's fix), and
/// a "Change to ..." fix for an unresolved import's "did you mean".
pub fn get_code_actions_with_project<S: ProjectSource>(
    source: &str,
    uri: &Url,
    params: &CodeActionParams,
    other_files: &[S],
) -> Vec<CodeActionOrCommand> {
    let project = Project::with_active(uri, source, other_files);
    let Some(doc) = project.index_of(uri) else {
        return vec![];
    };

    let mut actions = Vec::new();
    let check = import_check::check(&project, doc);

    for unresolved in &check.unresolved {
        let Some(suggestion) = &unresolved.suggestion else { continue };
        let range = byte_range_to_lsp_range(source, unresolved.range.0, unresolved.range.1);
        if !overlaps(range, params.range) {
            continue;
        }
        let written = &source[unresolved.range.0..unresolved.range.1];
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Change `{written}` to `{suggestion}`"),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: matching_diagnostics(params, range, UNRESOLVED_IMPORT),
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(uri.clone(), vec![TextEdit { range, new_text: suggestion.clone() }])])),
                document_changes: None,
                change_annotations: None,
            }),
            is_preferred: Some(true),
            ..Default::default()
        }));
    }

    for missing in check.missing {
        let range = byte_range_to_lsp_range(source, missing.range.0, missing.range.1);
        if !overlaps(range, params.range) {
            continue;
        }

        let diagnostics = matching_diagnostics(params, range, MISSING_IMPORT);

        let only_one = missing.candidates.len() == 1;
        for candidate in &missing.candidates {
            let path = candidate.use_path(&missing.name);
            let edit = add_use_edit(source, &path);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Add `use {path}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: diagnostics.clone(),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                    document_changes: None,
                    change_annotations: None,
                }),
                is_preferred: Some(only_one),
                ..Default::default()
            }));
        }
    }

    actions.extend(duplicate_name_actions(source, uri, params, &project, doc, other_files));

    // Several uses of the same name in range: one fix per candidate is enough.
    actions.dedup_by(|a, b| match (a, b) {
        (CodeActionOrCommand::CodeAction(a), CodeActionOrCommand::CodeAction(b)) => a.title == b.title,
        _ => false,
    });
    actions
}

/// Quick fixes for a name declared twice in one schema: on *each* of the
/// colliding declarations, a rename to a free name, a rename you type
/// yourself, and a delete.
///
/// Driven by the symbol table rather than by reading core's diagnostic
/// text - that gives the exact kind, name range and declaration range of
/// every member of the collision, none of which survives a round trip
/// through a message string. The ranges line up with the diagnostic anyway,
/// because `analysis::diagnostics` narrows core's declaration-wide spans to
/// these same name ranges.
///
/// Neither fix is marked preferred. There is no right answer to which of
/// two colliding declarations should be the one to yield.
fn duplicate_name_actions<S: ProjectSource>(
    source: &str,
    uri: &Url,
    params: &CodeActionParams,
    project: &Project,
    doc: usize,
    other_files: &[S],
) -> Vec<CodeActionOrCommand> {
    let symbols = &project.docs[doc].symbols;
    let mut actions = Vec::new();

    for group in symbols.duplicate_groups() {
        for index in group {
            let Some(symbol) = symbols.by_index(index) else { continue };
            let range = byte_range_to_lsp_range(source, symbol.name_span.0, symbol.name_span.1);
            if !overlaps(range, params.range) {
                continue;
            }

            let code = match symbol.namespace {
                Namespace::Declaration => DUPLICATE_DECLARATION,
                Namespace::SettingsPreset => DUPLICATE_BLOCK,
            };
            let diagnostics = matching_diagnostics(params, range, code);
            let what = format!("{} `{}`", keyword_of(symbol.kind), symbol.name);

            // Rename, applied: the declaration and every reference to it,
            // across the project - the same edit `textDocument/rename`
            // would produce, so the two can't drift apart.
            let free = free_name(project, symbol);
            if let Some(edit) =
                rename::rename_symbol_with_project(source, uri, range.start, &free, other_files)
            {
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Rename {what} to `{free}`"),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: diagnostics.clone(),
                    edit: Some(edit),
                    ..Default::default()
                }));
            }

            // Rename, interactive: no edit, just the client's own rename
            // box opened on this name. `editor.action.rename` is VS Code's;
            // a client that doesn't know it drops the action, and the other
            // two still work.
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Rename {what}\u{2026}"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: diagnostics.clone(),
                command: Some(Command {
                    title: "Rename".to_string(),
                    command: "editor.action.rename".to_string(),
                    arguments: Some(vec![json!([uri, range.start])]),
                }),
                ..Default::default()
            }));

            // Delete. Offered even when the declaration is referenced -
            // removing a used type is a legitimate thing to want, and the
            // unknown-type errors that follow are the editor doing its job.
            let (start, end) = delete_range(source, symbol.decl_span);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Delete {what}"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics,
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        uri.clone(),
                        vec![TextEdit {
                            range: byte_range_to_lsp_range(source, start, end),
                            new_text: String::new(),
                        }],
                    )])),
                    document_changes: None,
                    change_annotations: None,
                }),
                ..Default::default()
            }));
        }
    }

    actions
}

/// The keyword a declaration of this kind is written with. `SymbolKind` is
/// what the outline needs; several of its values stand in for something
/// Comline names differently (see `analysis::symbols`), so this maps back.
fn keyword_of(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::STRUCT => "struct",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "protocol",
        SymbolKind::CONSTANT => "const",
        SymbolKind::TYPE_PARAMETER => "type",
        SymbolKind::EVENT => "error",
        SymbolKind::FUNCTION => "validator",
        SymbolKind::OBJECT => "settings",
        _ => "declaration",
    }
}

/// A name nothing in the project has taken: the declaration's own name with
/// its kind appended (`Test` + `error` -> `TestError`), then `TestError2`,
/// `TestError3`, ... if even that is spoken for.
///
/// Checked against every namespace, not just the colliding one. A suggested
/// name that immediately collides with something else would be a poor fix,
/// even where the language would allow it.
fn free_name(project: &Project, symbol: &Symbol) -> String {
    let taken = |candidate: &str| {
        project
            .docs
            .iter()
            .any(|d| d.symbols.all_symbols().iter().any(|s| s.name == candidate))
    };

    let suffix = keyword_of(symbol.kind);
    let mut capitalized = suffix.chars();
    let base = match capitalized.next() {
        Some(first) => format!("{}{}{}", symbol.name, first.to_uppercase(), capitalized.as_str()),
        None => symbol.name.clone(),
    };

    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}{n}")).find(|c| !taken(c)).expect("the range is unbounded")
}

/// The text a "delete this declaration" fix removes: the declaration, the
/// indentation in front of it, its own line ending, and one blank line
/// after it - so deleting the middle of three declarations leaves one blank
/// line between the survivors, not three.
fn delete_range(source: &str, span: (usize, usize)) -> (usize, usize) {
    let line_start = source[..span.0].rfind('\n').map_or(0, |i| i + 1);
    let start = if source[line_start..span.0].trim().is_empty() { line_start } else { span.0 };

    let mut end = span.1;
    end += source[end..].find('\n').map_or(source.len() - end, |i| i + 1);
    // One following blank line, if there is one.
    let after = &source[end..];
    if let Some(newline) = after.find('\n') {
        if after[..newline].trim().is_empty() {
            end += newline + 1;
        }
    }
    (start, end)
}

/// The client's own copies of the diagnostic (with `code`, at `range`) a fix
/// answers to, so it can show the fix alongside it.
fn matching_diagnostics(params: &CodeActionParams, range: Range, code: &str) -> Option<Vec<Diagnostic>> {
    let found: Vec<Diagnostic> = params
        .context
        .diagnostics
        .iter()
        .filter(|d| d.range == range && d.code == Some(NumberOrString::String(code.to_string())))
        .cloned()
        .collect();
    (!found.is_empty()).then_some(found)
}

/// Whether two ranges touch (an empty cursor range at either edge counts).
fn overlaps(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{CodeActionContext, Position, TextDocumentIdentifier};

    fn params(uri: &Url, range: Range) -> CodeActionParams {
        CodeActionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range,
            context: CodeActionContext { diagnostics: vec![], only: None, trigger_kind: None },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn titles_and_edits(actions: Vec<CodeActionOrCommand>) -> Vec<(String, String, u32)> {
        actions
            .into_iter()
            .map(|a| match a {
                CodeActionOrCommand::CodeAction(a) => {
                    let edit = a.edit.unwrap().changes.unwrap().into_values().next().unwrap().remove(0);
                    (a.title, edit.new_text, edit.range.start.line)
                }
                other => panic!("expected a code action, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn nothing_to_fix_in_a_file_on_its_own() {
        let source = "struct User {}";
        let uri = Url::parse("file:///test.ids").unwrap();
        assert!(get_code_actions(source, &uri, &params(&uri, Range::default())).is_empty());
    }

    #[test]
    fn a_missing_import_gets_an_add_use_fix() {
        let chat = "use other::Thing\n\nstruct S {\n    m: Message\n}\n";
        let chat_uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let others = [(Url::parse("file:///pkg/src/types.ids").unwrap(), "struct Message {\n    t: string\n}\n".to_string())];

        let cursor = Range::new(Position::new(3, 9), Position::new(3, 9));
        let actions = get_code_actions_with_project(chat, &chat_uri, &params(&chat_uri, cursor), &others);
        assert_eq!(
            titles_and_edits(actions),
            vec![("Add `use types::Message`".to_string(), "use types::Message\n".to_string(), 1)]
        );
    }

    #[test]
    fn no_fix_away_from_the_missing_name() {
        let chat = "struct S {\n    m: Message\n    n: bool\n}\n";
        let chat_uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let others = [(Url::parse("file:///pkg/src/types.ids").unwrap(), "struct Message {\n    t: string\n}\n".to_string())];

        let elsewhere = Range::new(Position::new(2, 4), Position::new(2, 5));
        assert!(get_code_actions_with_project(chat, &chat_uri, &params(&chat_uri, elsewhere), &others).is_empty());
    }

    #[test]
    fn one_fix_per_file_declaring_the_name() {
        let chat = "struct S {\n    m: Message\n}\n";
        let chat_uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let others = [
            (Url::parse("file:///pkg/src/types.ids").unwrap(), "struct Message {\n    t: string\n}\n".to_string()),
            (Url::parse("file:///pkg/src/legacy.ids").unwrap(), "struct Message {\n    o: bool\n}\n".to_string()),
        ];

        let cursor = Range::new(Position::new(1, 8), Position::new(1, 8));
        let titles: Vec<String> =
            titles_and_edits(get_code_actions_with_project(chat, &chat_uri, &params(&chat_uri, cursor), &others))
                .into_iter()
                .map(|(t, _, _)| t)
                .collect();
        assert_eq!(titles, vec!["Add `use types::Message`", "Add `use legacy::Message`"]);
    }

    fn package_with_dependency(active_uri: &str) -> Vec<crate::analysis::source::SourceFile> {
        use crate::analysis::source::SourceFile;
        let _ = active_uri;
        vec![
            SourceFile::local(
                Url::parse("file:///pkg/config.idp").unwrap(),
                "congregation app\nspecification_version = 1\n\ndependencies = {\n    shared = {\n        path = \"../shared\"\n    }\n}\n".to_string(),
            ),
            SourceFile::local(Url::parse("file:///pkg/src/types.ids").unwrap(), "struct User {\n    id: u64\n}\n".to_string()),
            SourceFile::of_dependency(
                Url::parse("file:///shared/src/models.ids").unwrap(),
                "/// A shared thing\nstruct Thing {\n    id: u64\n}\n".to_string(),
                "shared",
                vec!["shared".to_string(), "models".to_string()],
            ),
        ]
    }

    #[test]
    fn an_unresolved_import_with_a_suggestion_gets_a_change_fix() {
        let chat = "use typse::User\n\nstruct S {\n    u: User\n}\n";
        let chat_uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let others = package_with_dependency("file:///pkg/src/chat.ids");

        let cursor = Range::new(Position::new(0, 6), Position::new(0, 6));
        let actions = get_code_actions_with_project(chat, &chat_uri, &params(&chat_uri, cursor), &others);
        assert_eq!(titles_and_edits(actions), vec![("Change `typse` to `types`".to_string(), "types".to_string(), 0)]);
    }
}

/// Quick fixes for a name declared twice. There were none at all before:
/// the only thing the editor said about a collision was the error itself.
#[cfg(test)]
mod duplicate_name_tests {
    use super::*;
    use crate::analysis::source::SourceFile;
    use lsp_types::{CodeActionContext, Position, TextDocumentIdentifier};

    const THREE_TESTS: &str = "struct Test {\n    a: str\n}\n\n\
                               error Test {\n    message = \"\"\n}\n\n\
                               /// Restricting some stuff\n\
                               settings Test {\n    validators.allowed = true\n}\n";

    fn at(line: u32, character: u32) -> CodeActionParams {
        CodeActionParams {
            text_document: TextDocumentIdentifier { uri: uri() },
            range: Range::new(Position::new(line, character), Position::new(line, character)),
            context: CodeActionContext { diagnostics: vec![], only: None, trigger_kind: None },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn uri() -> Url {
        Url::parse("file:///pkg/src/chat.ids").unwrap()
    }

    fn titles(source: &str, params: &CodeActionParams) -> Vec<String> {
        actions(source, params, &[])
            .into_iter()
            .map(|a| match a {
                CodeActionOrCommand::CodeAction(a) => a.title,
                other => panic!("expected a code action, got {other:?}"),
            })
            .collect()
    }

    fn actions(
        source: &str,
        params: &CodeActionParams,
        others: &[SourceFile],
    ) -> Vec<CodeActionOrCommand> {
        get_code_actions_with_project(source, &uri(), params, others)
    }

    fn only(source: &str, params: &CodeActionParams, title_contains: &str) -> CodeAction {
        actions(source, params, &[])
            .into_iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(a) if a.title.contains(title_contains) => Some(a),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no action matching {title_contains:?}"))
    }

    #[test]
    fn each_colliding_declaration_gets_its_own_three() {
        // On the struct's name...
        assert_eq!(
            titles(THREE_TESTS, &at(0, 8)),
            vec![
                "Rename struct `Test` to `TestStruct`",
                "Rename struct `Test`\u{2026}",
                "Delete struct `Test`",
            ]
        );
        // ...and on the error's, naming that one instead.
        assert_eq!(
            titles(THREE_TESTS, &at(4, 7)),
            vec![
                "Rename error `Test` to `TestError`",
                "Rename error `Test`\u{2026}",
                "Delete error `Test`",
            ]
        );
    }

    /// The `settings Test` is in its own namespace and collides with
    /// nothing, so it is not offered a way out of a problem it doesn't have.
    #[test]
    fn the_settings_preset_gets_nothing() {
        assert!(titles(THREE_TESTS, &at(9, 10)).is_empty());
    }

    #[test]
    fn nothing_away_from_either_name() {
        assert!(titles(THREE_TESTS, &at(1, 6)).is_empty(), "inside the struct body");
    }

    /// The applied rename is the real project-wide one, not a local edit:
    /// a sibling file's use of the name moves with it.
    #[test]
    fn the_applied_rename_follows_references_across_files() {
        let others = [
            SourceFile::local(
                Url::parse("file:///pkg/config.idp").unwrap(),
                "congregation app\nspecification_version = 1\n".to_string(),
            ),
            SourceFile::local(
                Url::parse("file:///pkg/src/other.ids").unwrap(),
                "use chat::Test\n\nstruct Holder {\n    t: Test\n}\n".to_string(),
            ),
        ];
        let action = actions(THREE_TESTS, &at(0, 8), &others)
            .into_iter()
            .find_map(|a| match a {
                CodeActionOrCommand::CodeAction(a) if a.title.contains("to `") => Some(a),
                _ => None,
            })
            .expect("an applied rename");

        let changes = action.edit.unwrap().changes.unwrap();
        let other = Url::parse("file:///pkg/src/other.ids").unwrap();
        assert_eq!(changes[&other].len(), 2, "the `use` line and the field type");
        assert!(changes[&other].iter().all(|e| e.new_text == "TestStruct"));
    }

    /// A suggested name that immediately collides with something else
    /// would be a poor fix, so the obvious one is skipped when taken.
    #[test]
    fn the_suggested_name_steps_around_what_is_taken() {
        let source = "struct Test {\n    a: str\n}\n\n\
                      error Test {\n    message = \"\"\n}\n\n\
                      struct TestError {\n    b: str\n}\n";
        assert!(titles(source, &at(4, 7)).contains(&"Rename error `Test` to `TestError2`".to_string()));
    }

    /// The interactive one carries no edit - just the client's rename box,
    /// opened on this name.
    #[test]
    fn the_interactive_rename_is_a_command_at_the_name() {
        let action = only(THREE_TESTS, &at(4, 7), "\u{2026}");
        assert!(action.edit.is_none());
        let command = action.command.expect("a client command");
        assert_eq!(command.command, "editor.action.rename");
        let arguments = command.arguments.unwrap();
        assert_eq!(arguments[0][1]["line"], 4);
        assert_eq!(arguments[0][1]["character"], 6, "the start of `Test`");
    }

    /// Delete takes the whole declaration - docstring, body, its own line
    /// ending and one blank line after it - not just the name.
    #[test]
    fn delete_removes_the_declaration_and_tidies_up() {
        let action = only(THREE_TESTS, &at(4, 7), "Delete");
        let edit = action.edit.unwrap().changes.unwrap().into_values().next().unwrap().remove(0);
        assert_eq!(edit.new_text, "");

        let start = crate::util::position_to_offset(THREE_TESTS, edit.range.start).unwrap();
        let end = crate::util::position_to_offset(THREE_TESTS, edit.range.end).unwrap();
        let mut after = THREE_TESTS.to_string();
        after.replace_range(start..end, "");
        assert_eq!(
            after,
            "struct Test {\n    a: str\n}\n\n\
             /// Restricting some stuff\n\
             settings Test {\n    validators.allowed = true\n}\n",
            "one blank line left between the survivors",
        );
    }

    /// A docstring belongs to the declaration it documents, so it goes with
    /// it rather than being orphaned above the next one.
    #[test]
    fn delete_takes_the_docstring_with_it() {
        let source = "/// The first one.\nerror Test {\n    message = \"a\"\n}\n\n\
                      error Test {\n    message = \"b\"\n}\n";
        let action = only(source, &at(1, 7), "Delete");
        let edit = action.edit.unwrap().changes.unwrap().into_values().next().unwrap().remove(0);

        let start = crate::util::position_to_offset(source, edit.range.start).unwrap();
        let end = crate::util::position_to_offset(source, edit.range.end).unwrap();
        let mut after = source.to_string();
        after.replace_range(start..end, "");
        assert_eq!(after, "error Test {\n    message = \"b\"\n}\n");
    }

    /// Two `settings` blocks of the same name *are* a collision - core
    /// reports it as `settings::duplicate-block` - so they get the fixes too.
    #[test]
    fn duplicate_settings_presets_get_fixes() {
        let source = "settings Strict {\n    validators.allowed = true\n}\n\n\
                      settings Strict {\n    validators.allowed = false\n}\n";
        assert_eq!(
            titles(source, &at(4, 10)),
            vec![
                "Rename settings `Strict` to `StrictSettings`",
                "Rename settings `Strict`\u{2026}",
                "Delete settings `Strict`",
            ]
        );
    }
}
