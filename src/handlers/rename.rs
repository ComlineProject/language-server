// Rename handler - renames symbols across the project

use crate::analysis::project::{Project, Target};
use crate::util::{byte_range_to_lsp_range, position_to_offset, word_range_at};
use std::collections::HashMap;
use lsp_types::{Position, Range, TextEdit, Url, WorkspaceEdit};

/// Rename a symbol at a position to a new name, considering only this file.
pub fn rename_symbol(
    source: &str,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    rename_symbol_with_project(source, uri, position, new_name, &[])
}

/// Rename the symbol at a position across this file and `other_files`
/// (every other file in the project, as `(uri, source)` pairs): its
/// declaration, every use of it written under its own name, and its name in
/// `use` lines importing it. Uses through an `as` alias keep the alias.
/// Started *on* an alias, this returns `None` rather than silently renaming
/// the aliased declaration out from under every other file - renaming the
/// alias itself is a different operation this doesn't offer yet.
pub fn rename_symbol_with_project(
    source: &str,
    uri: &Url,
    position: Position,
    new_name: &str,
    other_files: &[(Url, String)],
) -> Option<WorkspaceEdit> {
    if !is_valid_identifier(new_name) {
        return None;
    }

    let project = Project::with_active(uri, source, other_files);
    let (target, _) = renameable_at(&project, source, uri, position)?;

    let edit = |range| TextEdit { range, new_text: new_name.to_string() };

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    let declaration = project.declaration(&target)?;
    changes.entry(declaration.uri).or_default().push(edit(declaration.range));

    for reference in project.references(&target) {
        if reference.spelled_as_target {
            changes.entry(reference.location.uri).or_default().push(edit(reference.location.range));
        }
    }

    Some(WorkspaceEdit { changes: Some(changes), document_changes: None, change_annotations: None })
}

/// The range of the word a rename at `position` would start from, or
/// `None` when there's nothing renameable there (no resolvable symbol, or
/// an `as` alias - see [`rename_symbol_with_project`]) - the answer to the
/// client's `textDocument/prepareRename`.
pub fn prepare_rename_with_project(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[(Url, String)],
) -> Option<Range> {
    let project = Project::with_active(uri, source, other_files);
    renameable_at(&project, source, uri, position).map(|(_, range)| range)
}

/// The declaration a rename at `position` targets, and the cursor word's
/// range - shared by prepare-rename and rename so they always agree.
fn renameable_at(project: &Project, source: &str, uri: &Url, position: Position) -> Option<(Target, Range)> {
    let offset = position_to_offset(source, position)?;
    let (start, end) = word_range_at(source, offset)?;
    let word = &source[start..end];

    let target = project.resolve(project.index_of(uri)?, word)?;
    (target.name == word).then(|| (target, byte_range_to_lsp_range(source, start, end)))
}

/// Check if a string is a valid Comline identifier
fn is_valid_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    
    // First character must be letter or underscore
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        if !first.is_alphabetic() && first != '_' {
            return false;
        }
    }
    
    // Rest must be alphanumeric or underscore
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_rename_struct() {
        let source = r#"
struct User {
    name: string
}

struct Request {
    user: User
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 8); // On "User"
        
        let edit = rename_symbol(source, &uri, position, "Person");
        assert!(edit.is_some());
        
        let edit = edit.unwrap();
        let changes = edit.changes.unwrap();
        let edits = changes.get(&uri).unwrap();
        
        // Should have at least 2 edits (declaration + 1 reference)
        assert!(edits.len() >= 2);
        assert!(edits.iter().all(|e| e.new_text == "Person"));
    }
    
    #[test]
    fn test_rename_invalid_identifier() {
        let source = "struct User {}";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(0, 8);
        
        // Invalid names should return None
        assert!(rename_symbol(source, &uri, position, "123Invalid").is_none());
        assert!(rename_symbol(source, &uri, position, "My-Type").is_none());
        assert!(rename_symbol(source, &uri, position, "").is_none());
        
        // Valid names should work
        assert!(rename_symbol(source, &uri, position, "ValidName").is_some());
        assert!(rename_symbol(source, &uri, position, "_Private").is_some());
    }
    
    #[test]
    fn test_identifier_validation() {
        assert!(is_valid_identifier("User"));
        assert!(is_valid_identifier("_private"));
        assert!(is_valid_identifier("Type123"));
        
        assert!(!is_valid_identifier("123Type"));
        assert!(!is_valid_identifier("My-Type"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("Type Name"));
    }

    fn edits_by_file(edit: WorkspaceEdit) -> Vec<(String, u32, u32)> {
        let mut out: Vec<_> = edit
            .changes
            .unwrap()
            .into_iter()
            .flat_map(|(uri, edits)| {
                let file = uri.path().rsplit('/').next().unwrap().to_string();
                edits.into_iter().map(move |e| (file.clone(), e.range.start.line, e.range.start.character))
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn test_rename_edits_each_occurrence_exactly_once() {
        // Previously every reference resolved to the first `User` in the
        // file, so rename produced duplicate edits at the declaration and
        // never touched the uses.
        let source = "struct User {\n    name: string\n}\n\nstruct Request {\n    user: User\n    all: User[]\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();

        let edit = rename_symbol(source, &uri, Position::new(5, 12), "Person").unwrap();
        assert_eq!(
            edits_by_file(edit),
            vec![("test.ids".into(), 0, 7), ("test.ids".into(), 5, 10), ("test.ids".into(), 6, 9)]
        );
    }

    #[test]
    fn test_rename_across_files_updates_use_lines_and_keeps_aliases() {
        let types_source = "struct Message {\n    text: string\n}\n";
        let chat_source = "use types::Message\n\nstruct S {\n    m: Message\n}\n";
        let aliased_source = "use types::Message as Msg\n\nstruct T {\n    m: Msg\n}\n";
        let other_source = "use other::Message\n\nstruct U {\n    m: Message\n}\n";
        let other_decl = "struct Message {\n    unrelated: bool\n}\n";

        let types_uri = Url::parse("file:///types.ids").unwrap();
        let project = vec![
            (Url::parse("file:///chat.ids").unwrap(), chat_source.to_string()),
            (Url::parse("file:///aliased.ids").unwrap(), aliased_source.to_string()),
            (Url::parse("file:///uses_other.ids").unwrap(), other_source.to_string()),
            (Url::parse("file:///other.ids").unwrap(), other_decl.to_string()),
        ];

        let edit = rename_symbol_with_project(types_source, &types_uri, Position::new(0, 9), "Envelope", &project)
            .unwrap();
        assert_eq!(
            edits_by_file(edit),
            vec![
                ("aliased.ids".into(), 0, 11),
                ("chat.ids".into(), 0, 11),
                ("chat.ids".into(), 3, 7),
                ("types.ids".into(), 0, 7),
            ],
            "`m: Msg` keeps its alias; `uses_other.ids` imports a different `Message`"
        );
    }

    #[test]
    fn test_rename_started_on_an_alias_is_refused() {
        let types_source = "struct Message {\n    text: string\n}\n";
        let aliased_source = "use types::Message as Msg\n\nstruct T {\n    m: Msg\n}\n";
        let aliased_uri = Url::parse("file:///aliased.ids").unwrap();

        let edit = rename_symbol_with_project(
            aliased_source,
            &aliased_uri,
            Position::new(3, 8),
            "Other",
            &[(Url::parse("file:///types.ids").unwrap(), types_source.to_string())],
        );
        assert!(edit.is_none());
    }

    #[test]
    fn test_prepare_rename_returns_the_word_range_or_nothing() {
        let types_source = "struct Message {\n    text: string\n}\n";
        let aliased_source = "use types::Message as Msg\n\nstruct T {\n    m: Msg\n    s: string\n}\n";
        let aliased_uri = Url::parse("file:///aliased.ids").unwrap();
        let project = [(Url::parse("file:///types.ids").unwrap(), types_source.to_string())];

        let range = prepare_rename_with_project(aliased_source, &aliased_uri, Position::new(0, 14), &project)
            .expect("`Message` in the `use` line is renameable");
        assert_eq!((range.start.character, range.end.character), (11, 18));

        assert!(prepare_rename_with_project(aliased_source, &aliased_uri, Position::new(3, 8), &project).is_none());
        assert!(prepare_rename_with_project(aliased_source, &aliased_uri, Position::new(4, 8), &project).is_none());
    }
}
