// Definition handler - provides go-to-definition functionality

use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use tower_lsp::lsp_types::{GotoDefinitionResponse, Position, Url};

/// Find the definition of a symbol at a position
pub fn find_definition(source: &str, uri: &Url, position: Position) -> Option<GotoDefinitionResponse> {
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
    
    // TODO: Handle field references and cross-file references
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
    
    #[test]
    fn test_word_extraction() {
        let source = "struct User { }";
        
        // Test word extraction at different positions
        assert_eq!(get_word_at_offset(source, 7), Some("User".to_string()));
        assert_eq!(get_word_at_offset(source, 8), Some("User".to_string()));
        assert_eq!(get_word_at_offset(source, 0), Some("struct".to_string()));
    }
}
