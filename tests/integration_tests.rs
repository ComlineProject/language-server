// Integration tests for the LSP server

mod fixtures;

use comline_language_server::document::DocumentStore;
use lsp_types::*;

#[test]
fn test_document_store() {
    let store = DocumentStore::new();
    let (url, content) = fixtures::create_test_document();
    
    // Insert document
    store.insert(url.clone(), 1, content.clone());
    assert!(store.contains(&url));
    
    // Get document
    let doc = store.get(&url).unwrap();
    assert_eq!(doc.version, 1);
    assert_eq!(doc.text, content);
    
    // Update document
    let new_content = "struct NewUser { }".to_string();
    store.update(&url, 2, new_content.clone());
    let doc = store.get(&url).unwrap();
    assert_eq!(doc.version, 2);
    assert_eq!(doc.text, new_content);
    
    // Remove document
    store.remove(&url);
    assert!(!store.contains(&url));
}

#[test]
fn test_position_conversion() {
    use comline_language_server::util::{offset_to_position, position_to_offset};
    
    let text = "line1\nline2\nline3";
    
    // Test position_to_offset
    assert_eq!(position_to_offset(text, Position::new(0, 0)), Some(0));
    assert_eq!(position_to_offset(text, Position::new(1, 0)), Some(6));
    assert_eq!(position_to_offset(text, Position::new(2, 2)), Some(14));
    
    // Test offset_to_position
    assert_eq!(offset_to_position(text, 0), Position::new(0, 0));
    assert_eq!(offset_to_position(text, 6), Position::new(1, 0));
    assert_eq!(offset_to_position(text, 14), Position::new(2, 2));
}

#[test]
fn test_parser_basic() {
    use comline_language_server::parser;
    
    let (_, content) = fixtures::create_test_document();
    let result = parser::parse(&content);
    
    assert!(result.is_ok());
}

#[test]
fn test_symbol_table() {
    use comline_language_server::analysis::symbols::SymbolTable;
    
    let mut table = SymbolTable::new();
    
    // Symbol insertion and retrieval will be tested once implemented
    assert!(table.get("NonExistent").is_none());
}
