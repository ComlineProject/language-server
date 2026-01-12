// Symbol table building - to be implemented

use std::collections::HashMap;
use tower_lsp::lsp_types::{Location, SymbolKind};

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub location: Location,
}

pub struct SymbolTable {
    symbols: HashMap<String, Vec<Symbol>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
        }
    }

    pub fn insert(&mut self, name: String, symbol: Symbol) {
        self.symbols.entry(name).or_insert_with(Vec::new).push(symbol);
    }

    pub fn get(&self, name: &str) -> Option<&Vec<Symbol>> {
        self.symbols.get(name)
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Build symbol table from AST
pub fn build_symbol_table(_source: &str) -> SymbolTable {
    // TODO: Implement symbol table building
    SymbolTable::new()
}
