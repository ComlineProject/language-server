// Definition handler - provides go-to-definition functionality

use crate::analysis::project::Project;
use crate::analysis::source::ProjectSource;
use crate::util::{position_to_offset, word_range_at};
use lsp_types::{GotoDefinitionResponse, Position, Url};

/// Find the definition of a symbol at a position, considering only this file.
pub fn find_definition(source: &str, uri: &Url, position: Position) -> Option<GotoDefinitionResponse> {
    find_definition_with_project::<(Url, String)>(source, uri, position, &[])
}

/// Find the definition of a symbol at a position, also searching
/// `other_files` (every other file in the project, as `(uri, source)`
/// pairs) when it isn't declared in this file. See [`Project::resolve`]
/// for the lookup order (local, then `use`-scoped, then a flat fallback) -
/// shared with find-references and rename, so the three always agree.
pub fn find_definition_with_project<S: ProjectSource>(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[S],
) -> Option<GotoDefinitionResponse> {
    let offset = position_to_offset(source, position)?;
    let (start, end) = word_range_at(source, offset)?;

    let project = Project::with_active(uri, source, other_files);
    let active = project.index_of(uri)?;
    let target = project.resolve(active, &source[start..end])?;

    project.declaration(&target).map(GotoDefinitionResponse::Scalar)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_goto_definition_struct() {
        let source = r#"
struct User {
    name: string
}

struct Request {
    user: User
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        // Click on "User" in "user: User" (line 6, around column 11)
        let position = Position::new(6, 11);
        
        let result = find_definition(source, &uri, position);
        assert!(result.is_some());
        
        if let Some(GotoDefinitionResponse::Scalar(location)) = result {
            // Should point to the User struct definition on line 1
            assert_eq!(location.range.start.line, 1);
        }
    }
    
    #[test]
    fn test_goto_definition_enum() {
        let source = r#"
enum Status {
    Active
    Inactive
}

struct Task {
    status: Status
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        // Click on "Status" in "status: Status"
        let position = Position::new(7, 13);
        
        let result = find_definition(source, &uri, position);
        assert!(result.is_some());
    }
    
    #[test]
    fn test_goto_definition_on_declaration() {
        let source = r#"
struct User {
    name: string
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        // Click on "User" in the declaration itself
        let position = Position::new(1, 8);
        
        let result = find_definition(source, &uri, position);
        assert!(result.is_some(), "Should return definition even when clicking on declaration itself");
    }
    
    #[test]
    fn test_goto_definition_not_found() {
        let source = r#"
struct User {
    name: string
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        // Click on "string" (primitive type - no definition)
        let position = Position::new(2, 11);
        
        let result = find_definition(source, &uri, position);
        // Should return None for primitive types
        assert!(result.is_none());
    }
    
    fn scalar(result: Option<GotoDefinitionResponse>) -> lsp_types::Location {
        match result.expect("should resolve a definition") {
            GotoDefinitionResponse::Scalar(location) => location,
            other => panic!("expected a scalar location, got {other:?}"),
        }
    }

    #[test]
    fn test_goto_definition_in_another_file_via_use() {
        let chat_source = "use types::Message\n\nprotocol Chat {\n    function send() -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let chat_uri = Url::parse("file:///chat.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        // "Message" in the return-type position
        let location = scalar(find_definition_with_project(
            chat_source,
            &chat_uri,
            Position::new(3, 24),
            &[(types_uri.clone(), types_source.to_string())],
        ));

        assert_eq!(location.uri, types_uri);
        assert_eq!(location.range.start.line, 0);
    }

    #[test]
    fn test_goto_definition_two_siblings_same_name_picks_the_imported_one() {
        let active_source = "use b::Message\n\nprotocol P {\n    function f() -> Message;\n}\n";
        let a_source = "struct Message {\n    from_a: bool\n}\n";
        let b_source = "struct Message {\n    from_b: bool\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let a_uri = Url::parse("file:///a.ids").unwrap();
        let b_uri = Url::parse("file:///b.ids").unwrap();

        // `a.ids` comes first - a flat scan would pick it
        let location = scalar(find_definition_with_project(
            active_source,
            &active_uri,
            Position::new(3, 20),
            &[(a_uri, a_source.to_string()), (b_uri.clone(), b_source.to_string())],
        ));

        assert_eq!(location.uri, b_uri);
    }

    #[test]
    fn test_goto_definition_through_an_as_alias() {
        let active_source = "use types::Message as Msg\n\nprotocol P {\n    function f() -> Msg;\n}\n";
        let types_source = "struct Other {\n    x: bool\n}\n\nstruct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        let location = scalar(find_definition_with_project(
            active_source,
            &active_uri,
            Position::new(3, 21),
            &[(types_uri.clone(), types_source.to_string())],
        ));

        assert_eq!(location.uri, types_uri);
        assert_eq!(location.range.start.line, 4, "should land on `Message`, not `Other`");
    }

    #[test]
    fn test_goto_definition_from_the_use_line_itself() {
        let active_source = "use types::Message\n\nstruct S {\n    m: Message\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        // "Message" in `use types::Message`
        let location = scalar(find_definition_with_project(
            active_source,
            &active_uri,
            Position::new(0, 13),
            &[(types_uri.clone(), types_source.to_string())],
        ));

        assert_eq!(location.uri, types_uri);
    }

    #[test]
    fn test_goto_definition_falls_back_to_an_unimported_sibling() {
        // No `use` at all - the sibling may simply not be imported yet, or
        // the `use` is mid-edit; still better to jump than to do nothing.
        let active_source = "struct S {\n    m: Message\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        let location = scalar(find_definition_with_project(
            active_source,
            &active_uri,
            Position::new(1, 8),
            &[(types_uri.clone(), types_source.to_string())],
        ));

        assert_eq!(location.uri, types_uri);
    }

    #[test]
    fn test_goto_definition_prefers_a_local_declaration() {
        let active_source = "use types::Message\n\nstruct Message {\n    local: bool\n}\n\nstruct S {\n    m: Message\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        let location = scalar(find_definition_with_project(
            active_source,
            &active_uri,
            Position::new(7, 8),
            &[(types_uri, types_source.to_string())],
        ));

        assert_eq!(location.uri, active_uri);
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
    fn test_goto_definition_into_a_dependency() {
        let chat = "use shared::models::Thing\n\nstruct S {\n    t: Thing\n}\n";
        let chat_uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let others = package_with_dependency("file:///pkg/src/chat.ids");

        let location = scalar(find_definition_with_project(chat, &chat_uri, Position::new(3, 8), &others));
        assert_eq!(location.uri.as_str(), "file:///shared/src/models.ids");
        assert_eq!(location.range.start.line, 1);
    }
}
