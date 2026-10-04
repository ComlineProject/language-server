// Code actions handler - provides quick fixes and refactorings

use crate::analysis::diagnostics::{MISSING_IMPORT, UNRESOLVED_IMPORT};
use crate::analysis::import_check;
use crate::analysis::imports::add_use_edit;
use crate::analysis::project::Project;
use crate::analysis::source::ProjectSource;
use crate::util::byte_range_to_lsp_range;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, Diagnostic, NumberOrString, Range,
    TextEdit, Url, WorkspaceEdit,
};
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

    // Several uses of the same name in range: one fix per candidate is enough.
    actions.dedup_by(|a, b| match (a, b) {
        (CodeActionOrCommand::CodeAction(a), CodeActionOrCommand::CodeAction(b)) => a.title == b.title,
        _ => false,
    });
    actions
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
