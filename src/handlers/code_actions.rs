// Code actions handler - provides quick fixes and refactorings

use crate::analysis::diagnostics::MISSING_IMPORT;
use crate::analysis::import_check;
use crate::analysis::imports::add_use_edit;
use crate::analysis::project::Project;
use crate::util::byte_range_to_lsp_range;
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, NumberOrString, Range, Url,
    WorkspaceEdit,
};
use std::collections::HashMap;

/// Get code actions for a given range, considering only this file.
pub fn get_code_actions(source: &str, uri: &Url, params: &CodeActionParams) -> Vec<CodeActionOrCommand> {
    get_code_actions_with_project(source, uri, params, &[])
}

/// Get code actions for a given range, with `other_files` (every other
/// open file, as `(uri, source)` pairs) as the project. Today: one "Add
/// `use ...`" quick fix per open file that declares a type used in range
/// without being imported (the missing-import diagnostic's fix).
pub fn get_code_actions_with_project(
    source: &str,
    uri: &Url,
    params: &CodeActionParams,
    other_files: &[(Url, String)],
) -> Vec<CodeActionOrCommand> {
    let project = Project::with_active(uri, source, other_files);
    let Some(doc) = project.index_of(uri) else {
        return vec![];
    };

    let mut actions = Vec::new();
    for missing in import_check::check(&project, doc).missing {
        let range = byte_range_to_lsp_range(source, missing.range.0, missing.range.1);
        if !overlaps(range, params.range) {
            continue;
        }

        // The client's own copy of the diagnostic this fixes, so it can
        // show the fix alongside it.
        let diagnostics: Vec<_> = params
            .context
            .diagnostics
            .iter()
            .filter(|d| d.range == range && d.code == Some(NumberOrString::String(MISSING_IMPORT.to_string())))
            .cloned()
            .collect();

        let only_one = missing.candidates.len() == 1;
        for candidate in &missing.candidates {
            let path = candidate.use_path(&missing.name);
            let edit = add_use_edit(source, &path);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Add `use {path}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: (!diagnostics.is_empty()).then(|| diagnostics.clone()),
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
}
