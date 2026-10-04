// Definition handler - provides go-to-definition functionality

use crate::analysis::imports::{self, ProjectFile};
use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use comline_core::schema::idl::grammar::Document;
use lsp_types::{GotoDefinitionResponse, Position, Url};

/// Find the definition of a symbol at a position, considering only this file.
pub fn find_definition(source: &str, uri: &Url, position: Position) -> Option<GotoDefinitionResponse> {
    find_definition_with_project(source, uri, position, &[])
}

/// Find the definition of a symbol at a position, also searching
/// `other_files` (every other file in the project, as `(uri, source)`
/// pairs) when it isn't declared in this file. Same lookup order as
/// `hover::get_hover_info_with_project`: the local symbol table, then a
/// `use`/`import` here that actually brings the word into scope from one
/// specific sibling (`crate::analysis::imports`), then a flat, first-match
/// scan across every other file as a fallback (the project view is only
/// whichever files happen to be open, so "no `use` resolves this" is often
/// just a sibling that isn't open, or a `use` mid-edit).
pub fn find_definition_with_project(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[(Url, String)],
) -> Option<GotoDefinitionResponse> {
    // Convert position to byte offset
    let offset = position_to_offset(source, position)?;

    // Parse the document
    let parse_result = parser::parse(source).ok()?;
    let document = parse_result.document?;

    // Build symbol table
    let symbol_table = symbols::build_symbol_table(&document, uri, source);

    // Extract word at position
    let word = get_word_at_offset(source, offset)?;

    // Look up the symbol
    if let Some(symbol) = symbol_table.get(&word) {
        return Some(GotoDefinitionResponse::Scalar(symbol.location.clone()));
    }

    let other_docs: Vec<(&Url, &String, Document)> = other_files
        .iter()
        .filter_map(|(u, s)| parser::parse(s).ok()?.document.map(|d| (u, s, d)))
        .collect();

    // A `use`/`import` here that brings `word` into scope from one sibling
    let own_imports = imports::resolved_imports(&document, &imports::namespace_of(uri));
    let siblings: Vec<ProjectFile> =
        other_docs.iter().map(|(u, s, _)| ProjectFile::new(u, s)).collect();

    if let Some(resolved) = imports::resolve_symbol(&word, &own_imports, &siblings) {
        let sibling = resolved.file;
        if let Some((_, _, other_document)) =
            other_docs.iter().find(|(u, _, _)| **u == *sibling.uri)
        {
            let other_table =
                symbols::build_symbol_table(other_document, sibling.uri, sibling.source);
            // `real_name`, not `word` - they differ for an `as` alias (see
            // `ResolvedSymbol::real_name`'s doc).
            if let Some(symbol) = other_table.get(&resolved.real_name) {
                return Some(GotoDefinitionResponse::Scalar(symbol.location.clone()));
            }
        }
    }

    // Fallback: flat, project-wide, first-match scan - not `use`-scoped
    for (other_uri, other_source, other_document) in &other_docs {
        let other_table = symbols::build_symbol_table(other_document, other_uri, other_source);
        if let Some(symbol) = other_table.get(&word) {
            return Some(GotoDefinitionResponse::Scalar(symbol.location.clone()));
        }
    }

    None
}

/// Get word at byte offset
fn get_word_at_offset(source: &str, offset: usize) -> Option<String> {
    if offset >= source.len() {
        return None;
    }
    
    // Find word boundaries (alphanumeric + underscore)
    let start = source[..offset]
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    
    let end = source[offset..]
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| offset + i)
        .unwrap_or(source.len());
    
    if start < end {
        Some(source[start..end].to_string())
    } else {
        None
    }
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

    #[test]
    fn test_word_extraction() {
        let source = "struct User { }";
        
        // Test word extraction at different positions
        assert_eq!(get_word_at_offset(source, 7), Some("User".to_string()));
        assert_eq!(get_word_at_offset(source, 8), Some("User".to_string()));
        assert_eq!(get_word_at_offset(source, 0), Some("struct".to_string()));
    }
}
