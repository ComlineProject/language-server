// Definition handler - provides go-to-definition functionality

use crate::analysis::modules;
use crate::analysis::project::Project;
use crate::analysis::source::ProjectSource;
use crate::util::{byte_range_to_lsp_range, position_to_offset, word_range_at};
use lsp_types::{GotoDefinitionResponse, Location, Position, Url};

/// Find the definition of a symbol at a position, considering only this file.
pub fn find_definition(source: &str, uri: &Url, position: Position) -> Option<GotoDefinitionResponse> {
    find_definition_with_project::<(Url, String)>(source, uri, position, &[])
}

/// Find the definition of a symbol at a position, also searching
/// `other_files` (every other file in the project, as `(uri, source)`
/// pairs) when it isn't declared in this file. See [`Project::resolve`]
/// for the lookup order (local, then `use`-scoped, then a flat fallback) -
/// shared with find-references and rename, so the three always agree.
///
/// A path segment that names a module rather than a declaration (`std` or
/// `validators` in `use std::validators::StringBounds`) is checked first,
/// jumping to that module's own schema file instead - the same detection
/// hover uses, via [`modules::path_module_at`], so the two always agree on
/// which positions are "a module" versus "a declaration". A dependency's
/// file (std included) comes back with its own URI unchanged
/// (`comline-std:/validators.ids`); the client opens it read-only because
/// nothing it asks the server for can be saved back anywhere, not because
/// of any flag set here.
pub fn find_definition_with_project<S: ProjectSource>(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[S],
) -> Option<GotoDefinitionResponse> {
    let offset = position_to_offset(source, position)?;

    if let Some(location) = module_definition(source, uri, offset, other_files) {
        return Some(GotoDefinitionResponse::Scalar(location));
    }

    let (start, end) = word_range_at(source, offset)?;

    let project = Project::with_active(uri, source, other_files);
    let active = project.index_of(uri)?;
    let target = project.resolve(active, &source[start..end])?;

    project.declaration(&target).map(GotoDefinitionResponse::Scalar)
}

/// The definition of a module path segment at `offset`: the start of its
/// own schema file, if it has one. `None` for a pure grouping namespace (a
/// directory with no schema of its own, or a dependency's package root) -
/// there's no single file to open for one of those, only a listing (what
/// hover already shows).
fn module_definition<S: ProjectSource>(
    source: &str,
    uri: &Url,
    offset: usize,
    other_files: &[S],
) -> Option<Location> {
    let project = Project::with_active(uri, source, other_files);
    let (namespace, _) = modules::path_module_at(source, uri, offset, &project)?;
    let doc = project.docs.iter().find(|d| d.namespace == namespace)?;
    Some(Location { uri: doc.uri.clone(), range: byte_range_to_lsp_range(doc.source, 0, 0) })
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

    // ---- go-to-definition on a module path segment ----

    fn local(path: &str, text: &str) -> crate::analysis::source::SourceFile {
        crate::analysis::source::SourceFile::local(Url::parse(&format!("file:///pkg/{path}")).unwrap(), text.to_string())
    }

    /// The files of a multi-schema package, plus std.
    fn documented_package() -> Vec<crate::analysis::source::SourceFile> {
        let mut files = vec![
            local("src/types.ids", "struct User {\n    id: u64\n}\n"),
            local("src/api.ids", "struct Root {\n    id: u64\n}\n"),
            local("src/api/common.ids", "struct Error {\n    code: u32\n}\n"),
        ];
        files.extend(crate::analysis::stdlib::files(&crate::analysis::stdlib::root()));
        files
    }

    /// Go-to-definition at the first `needle` in `source`, written in
    /// `pkg/src/<file>`.
    fn definition_in(file: &str, source: &str, needle: &str) -> Option<Location> {
        let uri = Url::parse(&format!("file:///pkg/src/{file}")).unwrap();
        let offset = source.find(needle).unwrap();
        let position = crate::util::offset_to_position(source, offset);
        match find_definition_with_project(source, &uri, position, &documented_package())? {
            GotoDefinitionResponse::Scalar(location) => Some(location),
            other => panic!("expected a scalar location, got {other:?}"),
        }
    }

    #[test]
    fn a_module_segment_jumps_to_its_own_schema_file() {
        let location = definition_in("chat.ids", "use types::User\n", "types").expect("module definition");
        assert_eq!(location.uri.as_str(), "file:///pkg/src/types.ids");
        assert_eq!(location.range.start, Position::new(0, 0));
    }

    #[test]
    fn a_std_module_segment_jumps_to_its_virtual_file() {
        let source = "use std::validators::StringBounds\n";
        let location = definition_in("chat.ids", source, "validators").expect("std module definition");
        assert_eq!(location.uri.as_str(), "comline-std:/validators.ids");
    }

    #[test]
    fn the_std_package_root_has_no_single_file_to_jump_to() {
        // `std` itself is a pure grouping namespace - only its modules
        // (`std::http`, `std::validators`, ...) have their own schema.
        let source = "use std::validators::StringBounds\n";
        assert!(definition_in("chat.ids", source, "std").is_none());
    }

    #[test]
    fn a_directory_with_its_own_schema_jumps_to_it() {
        // `api` has both a schema (`api.ids`) and a deeper module
        // (`api::common`) - the schema is what a jump lands on.
        let location = definition_in("chat.ids", "use api::common::Error\n", "api").expect("api.ids exists");
        assert_eq!(location.uri.as_str(), "file:///pkg/src/api.ids");
    }

    #[test]
    fn a_pure_directory_grouping_has_no_single_file_to_jump_to() {
        // `api::v1` here has no schema of its own, only a deeper one
        // (`api::v1::user`) - same shape as `std` itself above, just
        // within the package being edited rather than a dependency.
        let files = vec![local("src/api/v1/user.ids", "struct Profile {\n    id: u64\n}\n")];
        let uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let source = "use api::v1::user::Profile\n";
        let offset = source.find("v1").unwrap();
        let position = crate::util::offset_to_position(source, offset);
        assert!(find_definition_with_project(source, &uri, position, &files).is_none());
    }

    #[test]
    fn a_last_segment_that_names_a_declaration_still_goes_to_the_declaration() {
        // `Request` here could be mistaken for a module segment (it's the
        // last segment of a `::` path) - it must still resolve as the
        // struct declaration, not as a (nonexistent) module.
        let location =
            definition_in("chat.ids", "use std::http::Request\n", "Request").expect("declaration, not module");
        assert_eq!(location.uri.as_str(), "comline-std:/http.ids");
        assert_ne!(location.range.start, Position::new(0, 0), "lands on Request's own declaration, not the file start");
    }

    #[test]
    fn a_whole_namespace_import_names_a_module_at_the_end() {
        let location = definition_in("chat.ids", "use types\n", "types").expect("lone word on a use line");
        assert_eq!(location.uri.as_str(), "file:///pkg/src/types.ids");
    }
}
