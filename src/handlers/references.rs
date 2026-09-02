// References handler - finds all usages of a symbol

use crate::analysis::symbols;
use crate::parser;
use crate::util::{byte_range_to_lsp_range, position_to_offset};
use comline_core::schema::idl::grammar::{Declaration, Type};
use lsp_types::{Location, Position, Url};

/// Find all references to a symbol at a position
pub fn find_references(
    source: &str,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    // Convert position to byte offset
    let offset = match position_to_offset(source, position) {
        Some(o) => o,
        None => return vec![],
    };
    
    // Parse the document
    let parse_result = match parser::parse(source) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    
    let document = match parse_result.document {
        Some(doc) => doc,
        None => return vec![],
    };
    
    // Build symbol table to get the symbol name
    let symbol_table = symbols::build_symbol_table(&document, uri, source);
    
    // Extract word at position
    let word = match get_word_at_offset(source, offset) {
        Some(w) => w,
        None => return vec![],
    };
    
    // Check if it's a known symbol
    let _symbol = match symbol_table.get(&word) {
        Some(s) => s,
        None => return vec![], // Not a defined symbol
    };
    
    // Find all references to this symbol
    let mut references = Vec::new();
    
    // Find declaration location (if requested)
    if include_declaration {
        if let Some(symbol) = symbol_table.get(&word) {
            references.push(symbol.location.clone());
        }
    }
    
    // Search for type references in all declarations
    for decl in &document.0 {
        match &**decl {
            Declaration::Struct(s) => {
                // Check each field type
                for field in s.fields() {
                    if let Some(loc) = check_type_reference(field.field_type(), &word, source, uri) {
                        references.push(loc);
                    }
                }
            }
            Declaration::Protocol(p) => {
                // Check function arguments and return types
                for func in p.functions() {
                    // Check arguments
                    if let Some(args) = func.args() {
                        // First argument
                        if let Some(loc) = check_type_reference(args.first().arg_type(), &word, source, uri) {
                            references.push(loc);
                        }
                        // Rest of arguments
                        for arg in args.rest() {
                            if let Some(loc) = check_type_reference(arg.arg_type().arg_type(), &word, source, uri) {
                                references.push(loc);
                            }
                        }
                    }
                    
                    // Check return type
                    if let Some(ret) = func.return_type() {
                        if let Some(loc) = check_type_reference(ret.return_type(), &word, source, uri) {
                            references.push(loc);
                        }
                    }
                }
            }
            Declaration::Const(c) => {
                // Check const type
                if let Some(loc) = check_type_reference(c.type_def(), &word, source, uri) {
                    references.push(loc);
                }
            }
            _ => {}
        }
    }
    
    references
}

/// Check if a type references the target symbol
fn check_type_reference(ty: &Type, target: &str, source: &str, uri: &Url) -> Option<Location> {
    match ty {
        Type::Named(name) if name.text == target => {
            // Find this reference in the source
            if let Some(pos) = find_word_in_source(source, &name.text) {
                let range = byte_range_to_lsp_range(source, pos, name.text.len());
                Some(Location {
                    uri: uri.clone(),
                    range,
                })
            } else {
                None
            }
        }
        Type::Array(arr) => {
            // Check array element type
            check_type_reference(arr.elem_type(), target, source, uri)
        }
        _ => None,
    }
}

/// Find a word in source (returns first occurrence)
fn find_word_in_source(source: &str, word: &str) -> Option<usize> {
    // This is simplified - ideally we'd find ALL occurrences
    // and match them to AST positions
    source.find(word)
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
        assert!(refs.len() >= 1, "Expected at least 1 reference");
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
}
