// Symbol provider for LSP document symbols

use crate::analysis::symbols;
use crate::parser;
use tower_lsp::lsp_types::{DocumentSymbol, Range, SymbolInformation, SymbolKind, Url};

/// Get document symbols for outline view
#[allow(deprecated)] // DocumentSymbol uses deprecated fields
pub fn get_document_symbols(source: &str, uri: &Url) -> Vec<DocumentSymbol> {
    // Parse the document
    let parse_result = match parser::parse(source) {
        Ok(result) => result,
        Err(_) => return vec![],
    };
    
    let document = match parse_result.document {
        Some(doc) => doc,
        None => return vec![],
    };
    
    // Build symbol table
    let symbol_table = symbols::build_symbol_table(&document, uri, source);
    
    // Convert to LSP DocumentSymbols
    symbol_table
        .all_symbols()
        .iter()
        .map(|symbol| {
            let children: Vec<DocumentSymbol> = symbol
                .children
                .iter()
                .map(|child_name| {
                    // Create a child symbol (field or function)
                    let kind = match symbol.kind {
                        SymbolKind::STRUCT => SymbolKind::FIELD,
                        SymbolKind::INTERFACE => SymbolKind::METHOD,
                        _ => SymbolKind::PROPERTY,
                    };
                    
                    // Try to find the child's position in source
                    let child_range = find_child_range(source, child_name, &symbol.name);
                    
                    DocumentSymbol {
                        name: child_name.clone(),
                        detail: None,
                        kind,
                        tags: None,
                        deprecated: None,
                        range: child_range,
                        selection_range: child_range,
                        children: None,
                    }
                })
                .collect();
            
            DocumentSymbol {
                name: symbol.name.clone(),
                detail: Some(format_symbol_detail(symbol.kind, &symbol.children)),
                kind: symbol.kind,
                tags: None,
                deprecated: None,
                range: symbol.location.range,
                selection_range: symbol.location.range,
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            }
        })
        .collect()
}

/// Format symbol detail string (e.g., "3 fields", "2 methods")
fn format_symbol_detail(kind: SymbolKind, children: &[String]) -> String {
    if children.is_empty() {
        return String::new();
    }
    
    let count = children.len();
    let item_type = match kind {
        SymbolKind::STRUCT => if count == 1 { "field" } else { "fields" },
        SymbolKind::INTERFACE => if count == 1 { "function" } else { "functions" },
        SymbolKind::ENUM => if count == 1 { "variant" } else { "variants" },
        _ => "items",
    };
    
    format!("{} {}", count, item_type)
}

/// Find a child's range within its parent declaration
fn find_child_range(source: &str, child_name: &str, parent_name: &str) -> Range {
    // Find parent, then search for child within it
    if let Some(parent_pos) = source.find(parent_name) {
        if let Some(relative_pos) = source[parent_pos..].find(child_name) {
            let pos = parent_pos + relative_pos;
            return byte_offset_to_range(source, pos, child_name.len());
        }
    }
    
    // Fallback
    Range::default()
}

/// Convert byte offset to LSP Range
fn byte_offset_to_range(source: &str, offset: usize, length: usize) -> Range {
    use tower_lsp::lsp_types::Position;
    
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    
    let line = line_starts
        .iter()
        .position(|&start| start > offset)
        .unwrap_or(line_starts.len())
        - 1;
    let line_start = line_starts[line];
    let column = source[line_start..offset].chars().count();
    
    let start = Position::new(line as u32, column as u32);
    let end = Position::new(line as u32, (column + length) as u32);
    
    Range { start, end }
}

/// Get workspace symbols (basic implementation - searches all symbols by name)
pub fn get_workspace_symbols(_query: &str) -> Vec<SymbolInformation> {
    // TODO: Implement workspace-wide symbol search
    // This would require maintaining a workspace-level symbol index
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_get_document_symbols() {
        let source = r#"
struct User {
    name: string
    age: i32
}

enum Role {
    Admin
    User
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        let symbols = get_document_symbols(source, &uri);
        
        assert_eq!(symbols.len(), 2);
        
        // Check struct
        assert_eq!(symbols[0].name, "User");
        assert_eq!(symbols[0].kind, SymbolKind::STRUCT);
        assert_eq!(symbols[0].detail, Some("2 fields".to_string()));
        assert!(symbols[0].children.is_some());
        assert_eq!(symbols[0].children.as_ref().unwrap().len(), 2);
        
        // Check enum
        assert_eq!(symbols[1].name, "Role");
        assert_eq!(symbols[1].kind, SymbolKind::ENUM);
    }
    
    #[test]
    fn test_document_symbols_with_protocol() {
        let source = r#"
protocol UserService {
    function getUser(i64) -> string;
    function deleteUser(i64) -> bool;
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        let symbols = get_document_symbols(source, &uri);
        
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "UserService");
        assert_eq!(symbols[0].kind, SymbolKind::INTERFACE);
        assert_eq!(symbols[0].detail, Some("2 functions".to_string()));
        
        let children = symbols[0].children.as_ref().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].kind, SymbolKind::METHOD);
    }
}
