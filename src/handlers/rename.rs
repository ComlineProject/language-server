// Rename handler - renames symbols across the document

use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use std::collections::HashMap;
use lsp_types::{Position, TextEdit, Url, WorkspaceEdit};

/// Rename a symbol at a position to a new name
pub fn rename_symbol(
    source: &str,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    // Validate new name (must be valid identifier)
    if !is_valid_identifier(new_name) {
        return None;
    }
    
    // Convert position to byte offset
    let offset = position_to_offset(source, position)?;
    
    // Parse the document
    let parse_result = parser::parse(source).ok()?;
    let document = parse_result.document?;
    
    // Build symbol table
    let symbol_table = symbols::build_symbol_table(&document, uri, source);
    
    // Get the symbol at this position
    let word = get_word_at_offset(source, offset)?;
    let symbol = symbol_table.get(&word)?;
    
    // Find all locations that need to be renamed
    let mut edits = Vec::new();
    
    // Add the declaration
    edits.push(TextEdit {
        range: symbol.location.range,
        new_text: new_name.to_string(),
    });
    
    // Find all references
    use crate::handlers::references;
    let refs = references::find_references(source, uri, position, false); // exclude declaration
    
    for loc in refs {
        edits.push(TextEdit {
            range: loc.range,
            new_text: new_name.to_string(),
        });
    }
    
    // Create workspace edit
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    
    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
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

/// Get word at byte offset
fn get_word_at_offset(source: &str, offset: usize) -> Option<String> {
    if offset >= source.len() {
        return None;
    }
    
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
}
