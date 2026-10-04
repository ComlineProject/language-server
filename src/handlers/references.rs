// References handler - finds all usages of a symbol

use crate::analysis::project::Project;
use crate::util::{position_to_offset, word_range_at};
use lsp_types::{Location, Position, Url};

/// Find all references to a symbol at a position, considering only this file.
pub fn find_references(
    source: &str,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    find_references_with_project(source, uri, position, include_declaration, &[])
}

/// Find all references to the symbol at a position across this file and
/// `other_files` (every other file in the project, as `(uri, source)`
/// pairs): every place whose go-to-definition lands on the same
/// declaration (see [`Project::references`]). Works from the declaration
/// itself, from any use of it, or from a `use` line importing it.
pub fn find_references_with_project(
    source: &str,
    uri: &Url,
    position: Position,
    include_declaration: bool,
    other_files: &[(Url, String)],
) -> Vec<Location> {
    let Some(offset) = position_to_offset(source, position) else {
        return vec![];
    };
    let Some((start, end)) = word_range_at(source, offset) else {
        return vec![];
    };

    let project = Project::with_active(uri, source, other_files);
    let Some(target) = project.index_of(uri).and_then(|i| project.resolve(i, &source[start..end])) else {
        return vec![];
    };

    let mut locations = Vec::new();
    if include_declaration {
        locations.extend(project.declaration(&target));
    }
    locations.extend(project.references(&target).into_iter().map(|r| r.location));
    locations
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_find_references_struct() {
        let source = r#"
struct User {
    name: string
}

struct Request {
    user: User
}

struct Response {
    user: User
    success: bool
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        // Find references to "User" (click on declaration)
        let position = Position::new(1, 8);
        let refs = find_references(source, &uri, position, true);
        
        // Should find: declaration + 2 usages
        assert!(refs.len() >= 2, "Expected at least 2 references (declaration + 1 usage), got {}", refs.len());
    }
    
    #[test]
    fn test_find_references_exclude_declaration() {
        let source = r#"
struct User {
    name: string
}

struct Request {
    user: User
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        // Find references excluding declaration
        let position = Position::new(1, 8);
        let refs = find_references(source, &uri, position, false);
        
        // Should find only usages, not declaration
        assert!(!refs.is_empty(), "Expected at least 1 reference");
    }
    
    #[test]
    fn test_find_references_enum() {
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
        
        let position = Position::new(1, 6);
        let refs = find_references(source, &uri, position, true);
        
        assert!(!refs.is_empty(), "Should find references to enum");
    }
    
    #[test]
    fn test_find_references_not_defined() {
        let source = r#"
struct User {
    name: string
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        // Click on "string" (primitive - not user-defined)
        let position = Position::new(2, 11);
        let refs = find_references(source, &uri, position, true);
        
        // Should return empty for primitives
        assert!(refs.is_empty(), "Should not find references for primitive types");
    }

    #[test]
    fn test_find_references_returns_each_usage_once_at_its_own_position() {
        let source = "struct User {\n    name: string\n}\n\nstruct Request {\n    user: User\n}\n\nstruct Response {\n    user: User\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();

        let refs = find_references(source, &uri, Position::new(0, 8), true);
        let mut starts: Vec<_> = refs.iter().map(|l| (l.range.start.line, l.range.start.character)).collect();
        starts.sort();

        assert_eq!(starts, vec![(0, 7), (5, 10), (9, 10)]);
        assert!(refs.iter().all(|l| l.range.end.character == l.range.start.character + 4));
    }

    #[test]
    fn test_find_references_across_files_from_a_usage() {
        let chat_source = "use types::Message\n\nstruct S {\n    m: Message\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";
        let chat_uri = Url::parse("file:///chat.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        // From `m: Message` in chat.ids
        let refs = find_references_with_project(
            chat_source,
            &chat_uri,
            Position::new(3, 8),
            true,
            &[(types_uri.clone(), types_source.to_string())],
        );

        assert_eq!(refs.len(), 3, "declaration + `use` line + field: {refs:?}");
        assert!(refs.iter().any(|l| l.uri == types_uri && l.range.start.line == 0));
    }
}
