// Symbol table building from AST

use comline_core::schema::idl::grammar::{Declaration, Document};
use std::collections::HashMap;
use tower_lsp::lsp_types::{Location, Position, Range, SymbolKind, Url};

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub location: Location,
    /// For structs/protocols: list of field/function names
    pub children: Vec<String>,
}

pub struct SymbolTable {
    symbols: HashMap<String, Symbol>,
    /// Track all symbols in order of appearance
    ordered_symbols: Vec<String>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
            ordered_symbols: Vec::new(),
        }
    }

    pub fn insert(&mut self, name: String, symbol: Symbol) {
        if !self.symbols.contains_key(&name) {
            self.ordered_symbols.push(name.clone());
        }
        self.symbols.insert(name, symbol);
    }

    pub fn get(&self, name: &str) -> Option<&Symbol> {
        self.symbols.get(name)
    }
    
    pub fn all_symbols(&self) -> Vec<&Symbol> {
        self.ordered_symbols
            .iter()
            .filter_map(|name| self.symbols.get(name))
            .collect()
    }
    
    pub fn len(&self) -> usize {
        self.symbols.len()
    }
    
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Build symbol table from parsed AST
pub fn build_symbol_table(document: &Document, uri: &Url, source: &str) -> SymbolTable {
    let mut table = SymbolTable::new();
    
    // Track line starts for position calculation
    let _line_starts = build_line_starts(source);
    
    // Walk through all declarations (each is `Spanned<Declaration>` — deref to match)
    for declaration in &document.0 {
        match &**declaration {
            Declaration::Struct(s) => {
                let name = s.name();
                let children: Vec<String> = s.fields().iter().map(|f| f.name()).collect();
                
                // For now, use approximate position (we'd need byte offsets from rust-sitter)
                // This is a simplified version - ideally we'd get exact positions from the AST
                let range = find_declaration_range(source, &name);
                
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::STRUCT,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children,
                    },
                );
            }
            Declaration::Enum(e) => {
                let name = e.name();
                let children: Vec<String> = e.variants().iter().map(|v| v.identifier().text.clone()).collect();
                
                let range = find_declaration_range(source, &name);
                
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::ENUM,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children,
                    },
                );
            }
            Declaration::Protocol(p) => {
                let name = p.name();
                let children: Vec<String> = p.functions().iter().map(|f| f.name()).collect();
                
                let range = find_declaration_range(source, &name);
                
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::INTERFACE,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children,
                    },
                );
            }
            Declaration::Const(c) => {
                let name = c.name();
                let range = find_declaration_range(source, &name);
                
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::CONSTANT,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children: vec![],
                    },
                );
            }
            Declaration::Import(_)
            | Declaration::Use(_)
            | Declaration::Error(_)
            | Declaration::Settings(_)
            | Declaration::Validator(_) => {
                // Not surfaced in the outline (for now)
            }
        }
    }
    
    table
}

/// Build line starts index for position calculation
fn build_line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// Find the range of a declaration by searching for its name
/// This is a simplified heuristic - ideally we'd get exact byte offsets from the parser
fn find_declaration_range(source: &str, name: &str) -> Range {
    // Search for the name in the source
    if let Some(pos) = source.find(name) {
        // Convert byte offset to line/column
        let line_starts = build_line_starts(source);
        let line = line_starts.iter().position(|&start| start > pos).unwrap_or(line_starts.len()) - 1;
        let line_start = line_starts[line];
        let column = source[line_start..pos].chars().count();
        
        let start = Position::new(line as u32, column as u32);
        let end = Position::new(line as u32, (column + name.len()) as u32);
        
        Range { start, end }
    } else {
        // Fallback to 0,0
        Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    #[test]
    fn test_build_symbol_table() {
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
        let result = parser::parse(source).unwrap();
        assert!(result.is_ok());
        
        let table = build_symbol_table(&result.document.unwrap(), &uri, source);
        
        assert_eq!(table.len(), 2);
        assert!(table.get("User").is_some());
        assert!(table.get("Role").is_some());
        
        let user_symbol = table.get("User").unwrap();
        assert_eq!(user_symbol.kind, SymbolKind::STRUCT);
        assert_eq!(user_symbol.children.len(), 2);
        assert!(user_symbol.children.contains(&"name".to_string()));
        assert!(user_symbol.children.contains(&"age".to_string()));
        
        let role_symbol = table.get("Role").unwrap();
        assert_eq!(role_symbol.kind, SymbolKind::ENUM);
        assert_eq!(role_symbol.children.len(), 2);
    }
    
    #[test]
    fn test_symbol_table_with_protocol() {
        let source = r#"
protocol UserService {
    function getUser(i64) -> string;
    function createUser(string) -> i64;
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();
        
        let table = build_symbol_table(&result.document.unwrap(), &uri, source);
        
        assert_eq!(table.len(), 1);
        let protocol = table.get("UserService").unwrap();
        assert_eq!(protocol.kind, SymbolKind::INTERFACE);
        assert_eq!(protocol.children.len(), 2);
    }
}
