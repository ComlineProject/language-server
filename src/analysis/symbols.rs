// Symbol table building from AST

use comline_core::schema::idl::grammar::{Declaration, Document};
use std::collections::HashMap;
use crate::util::{byte_range_to_lsp_range, word_occurrences};
use lsp_types::{Location, Range, SymbolKind, Url};

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
    
    // Walk through all declarations (each is `Spanned<Declaration>` — deref to match)
    for declaration in &document.0 {
        match &**declaration {
            Declaration::Struct(s) => {
                let name = s.name();
                let children: Vec<String> = s.fields().iter().map(|f| f.name()).collect();
                
                let range = declaration_name_range(source, declaration.span, "struct", &name);
                
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
                
                let range = declaration_name_range(source, declaration.span, "enum", &name);
                
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
                
                let range = declaration_name_range(source, declaration.span, "protocol", &name);
                
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
                let range = declaration_name_range(source, declaration.span, "const", &name);
                
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
            Declaration::TypeAlias(t) => {
                let name = t.name();
                let range = declaration_name_range(source, declaration.span, "type", &name);

                // `TYPE_PARAMETER` is the closest LSP-standard fit for a
                // transparent type alias - LSP 3.17's `SymbolKind` has no
                // dedicated "type alias" entry, and other LSPs (e.g.
                // rust-analyzer, for Rust's own `type`) use this same kind.
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::TYPE_PARAMETER,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children: vec![],
                    },
                );
            }
            Declaration::Error(e) => {
                let name = e.name();
                let children: Vec<String> = e.fields().iter().map(|f| f.name()).collect();
                let range = declaration_name_range(source, declaration.span, "error", &name);

                // `EVENT` is the closest LSP-standard fit: a named, raised/
                // caught thing, same spirit as `TYPE_PARAMETER` standing in
                // for a type alias above - LSP 3.17 has no dedicated
                // "error"/"exception" kind.
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::EVENT,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children,
                    },
                );
            }
            Declaration::Validator(v) => {
                let name = v.name();
                let children: Vec<String> = v.properties().iter().map(|p| p.name()).collect();
                let range = declaration_name_range(source, declaration.span, "validator", &name);

                // `FUNCTION`: a validator is referenced like one is called
                // (`StringBounds(min=3, max=10)`), parameters and all.
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::FUNCTION,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children,
                    },
                );
            }
            Declaration::Settings(s) => {
                let name = s.name();
                let children: Vec<String> = s.entries().iter().map(|e| e.key()).collect();
                let range = declaration_name_range(source, declaration.span, "settings", &name);

                // `OBJECT`: a named object of key/value entries.
                table.insert(
                    name.clone(),
                    Symbol {
                        name: name.clone(),
                        kind: SymbolKind::OBJECT,
                        location: Location {
                            uri: uri.clone(),
                            range,
                        },
                        children,
                    },
                );
            }
            Declaration::Import(_) | Declaration::Use(_) => {
                // Not a declaration that can be referred to by name.
            }
        }
    }
    
    table
}

/// The range of a declaration's own name: the first whole-word `name`
/// inside the declaration's span that directly follows its `keyword`
/// (`struct User`) - not just the first `User` anywhere in the file, which
/// could be inside `UserProfile`, a `use` line, or a docstring. Rename
/// edits this range, so it has to be exact.
fn declaration_name_range(source: &str, span: (usize, usize), keyword: &str, name: &str) -> Range {
    let (start, end) = (span.0.min(source.len()), span.1.min(source.len()));
    let text = &source[start..end];

    let offset = word_occurrences(text, name)
        .into_iter()
        .find(|&i| {
            let before = text[..i].trim_end();
            word_occurrences(before, keyword).last().is_some_and(|&k| k + keyword.len() == before.len())
        })
        .map(|i| start + i)
        .unwrap_or(start);

    byte_range_to_lsp_range(source, offset, offset + name.len())
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
    fn test_symbol_table_with_type_alias() {
        let source = "type UserId = u64\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();

        let table = build_symbol_table(&result.document.unwrap(), &uri, source);

        assert_eq!(table.len(), 1);
        let alias = table.get("UserId").unwrap();
        assert_eq!(alias.kind, SymbolKind::TYPE_PARAMETER);
        assert!(alias.children.is_empty());
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

    #[test]
    fn declaration_range_is_the_declared_name_not_an_earlier_substring() {
        // `User` appears first inside `UserProfile`, then in the `use`
        // line, then in a docstring - none of them are the declaration.
        let source = "use types::User\n\nstruct UserProfile {\n    id: u64\n}\n\n/// The User record\nstruct User {\n    name: string\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();

        let table = build_symbol_table(&result.document.unwrap(), &uri, source);
        let range = table.get("User").unwrap().location.range;

        assert_eq!(range.start, lsp_types::Position::new(7, 7));
        assert_eq!(range.end, lsp_types::Position::new(7, 11));
    }
}
